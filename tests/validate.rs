//! Loader and `brg validate` tests against teams built in temporary directories.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use borgchart::diag::Report;
use borgchart::team::{Grant, Posture, Vendor};
use borgchart::validate::{Input, Validated, validate};

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    /// A small valid team: a locked triager and a lead, with a script plugin.
    fn new() -> Self {
        let f = Fixture {
            dir: tempfile::tempdir().unwrap(),
        };
        f.write(
            "team.yaml",
            r#"
default: claude
plugins:
  slackbot:
    path: plugins/slackbot
    process: [./watch.py]
    delivers_to: triager
    env: [SLACK_BOT_TOKEN, SLACK_CHANNEL_ID]
    actions:
      read: slackbot read <ts>
      reply: slackbot reply <thread_ts> <text>
      escalate: slackbot escalate <ts> <severity> <reason>
members:
  triager:
    prompt: triager.md
    parent: lead
    posture: locked
    untrusted_input: true
    plugins: {slackbot: [read, escalate]}
  lead:
    prompt: lead.md
    posture: locked
    untrusted_input: true
    plugins: {slackbot: [read, reply]}
"#,
        );
        f.write(
            "triager.md",
            "---\nrole: triager\nmodels: {claude: haiku}\n---\nDecide whether to escalate.\n{{includes/shared}}\n",
        );
        f.write(
            "lead.md",
            "---\nrole: lead\nmodels: {claude: opus high, codex: example-model medium}\n---\nTriage escalations.\n",
        );
        f.write("includes/shared.md", "Be brief.\n");
        f.exec("plugins/slackbot/slackbot");
        f.exec("plugins/slackbot/watch.py");
        f.write(
            "borgchart.yaml",
            "schema: 1\nplugins:\n  slackbot:\n    env: {SLACK_CHANNEL_ID: C0000EXAMPLE, SLACK_BOT_TOKEN: not-a-real-token}\n",
        );
        f
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn exec(&self, rel: &str) {
        self.write(rel, "#!/bin/sh\necho ok\n");
        fs::set_permissions(self.path(rel), fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn edit(&self, rel: &str, from: &str, to: &str) {
        let text = fs::read_to_string(self.path(rel)).unwrap();
        assert!(text.contains(from), "fixture {rel} lacks {from:?}");
        self.write(rel, &text.replacen(from, to, 1));
    }

    fn run(&self) -> (Report, Option<Validated>) {
        validate(&Input::Deployment(self.path("borgchart.yaml")))
    }

    fn run_team(&self) -> (Report, Option<Validated>) {
        validate(&Input::Team(self.dir.path().to_path_buf()))
    }
}

fn errors(report: &Report) -> String {
    report
        .errors()
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn warnings(report: &Report) -> String {
    report
        .warnings()
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn expect_error(f: &Fixture, needle: &str) {
    let (report, validated) = f.run();
    let errs = errors(&report);
    assert!(validated.is_none(), "expected failure, got ok");
    assert!(
        errs.contains(needle),
        "expected error containing {needle:?}, got:\n{errs}"
    );
}

#[test]
fn valid_team_loads() {
    let f = Fixture::new();
    let (report, validated) = f.run();
    assert!(!report.has_errors(), "{}", errors(&report));
    let v = validated.unwrap();
    let triager = &v.team.members["triager"];
    assert_eq!(triager.role, "triager");
    assert_eq!(triager.vendor, Vendor::Claude);
    assert_eq!(triager.model.as_ref().unwrap().name, "haiku");
    assert_eq!(triager.posture, Posture::Locked);
    assert!(triager.instruction.contains("Be brief."));
    assert!(!triager.instruction.contains("{{includes"));
    assert_eq!(
        triager.grants["slackbot"],
        Grant::Actions(vec!["read".into(), "escalate".into()])
    );
    let lead = &v.team.members["lead"];
    assert_eq!(lead.model.as_ref().unwrap().effort.as_deref(), Some("high"));
    assert_eq!(
        v.team.plugins["slackbot"].actions["escalate"].prefix,
        vec!["slackbot", "escalate"]
    );
}

#[test]
fn duplicate_keys_are_errors() {
    let f = Fixture::new();
    f.edit(
        "team.yaml",
        "default: claude\n",
        "default: claude\ndefault: codex\n",
    );
    expect_error(&f, "duplicate");
}

#[test]
fn yes_is_not_a_boolean() {
    let f = Fixture::new();
    f.edit(
        "team.yaml",
        "    prompt: lead.md\n",
        "    prompt: lead.md\n    start: no\n",
    );
    expect_error(&f, "boolean");
}

#[test]
fn merge_keys_are_refused() {
    let f = Fixture::new();
    f.edit(
        "team.yaml",
        "  lead:\n    prompt: lead.md\n",
        "  lead:\n    <<: {prompt: lead.md}\n",
    );
    expect_error(&f, "merge");
}

#[test]
fn unknown_fields_are_errors() {
    let f = Fixture::new();
    f.edit(
        "team.yaml",
        "    prompt: lead.md\n",
        "    prompt: lead.md\n    colour: blue\n",
    );
    expect_error(&f, "colour");
}

#[test]
fn unknown_frontmatter_fields_are_errors() {
    let f = Fixture::new();
    f.edit("lead.md", "role: lead\n", "role: lead\nmood: calm\n");
    expect_error(&f, "mood");
}

#[test]
fn frontmatter_must_close() {
    let f = Fixture::new();
    f.write("lead.md", "---\nrole: lead\nTriage.\n");
    expect_error(&f, "not closed");
}

#[test]
fn frontmatter_bom_and_crlf_are_accepted() {
    let f = Fixture::new();
    f.write(
        "lead.md",
        "\u{feff}---\r\nrole: lead\r\nmodels: {claude: opus}\r\n---\r\nTriage.\r\n---\r\nStill body.\r\n",
    );
    let (report, validated) = f.run();
    assert!(!report.has_errors(), "{}", errors(&report));
    let lead = &validated.unwrap().team.members["lead"];
    assert!(lead.instruction.contains("Still body."));
}

#[test]
fn legacy_team_file_points_to_docs() {
    let f = Fixture::new();
    fs::rename(f.path("team.yaml"), f.path("topology.yaml")).unwrap();
    expect_error(&f, "docs/format.md");
}

#[test]
fn legacy_frontmatter_points_to_docs() {
    let f = Fixture::new();
    f.write(
        "lead.md",
        "---\nname: lead\nclaude_model: opus\n---\nTriage.\n",
    );
    expect_error(&f, "older frontmatter");
}

#[test]
fn prompt_symlink_outside_team_is_refused() {
    let f = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("lead.md");
    fs::write(&target, "---\nrole: lead\n---\nx\n").unwrap();
    fs::remove_file(f.path("lead.md")).unwrap();
    std::os::unix::fs::symlink(&target, f.path("lead.md")).unwrap();
    expect_error(&f, "outside");
}

#[test]
fn dotdot_paths_are_refused() {
    let f = Fixture::new();
    f.edit("team.yaml", "prompt: lead.md", "prompt: ../lead.md");
    expect_error(&f, "`..`");
}

#[test]
fn missing_include_is_an_error() {
    let f = Fixture::new();
    fs::remove_file(f.path("includes/shared.md")).unwrap();
    expect_error(&f, "include");
}

#[test]
fn nested_include_is_an_error() {
    let f = Fixture::new();
    f.write("includes/shared.md", "Be brief.\n{{includes/other}}\n");
    f.write("includes/other.md", "x\n");
    expect_error(&f, "one level deep");
}

#[test]
fn include_amplification_is_capped() {
    let f = Fixture::new();
    f.write("includes/shared.md", &"x".repeat(600 * 1024));
    f.write(
        "triager.md",
        "---\nrole: triager\n---\n{{includes/shared}}\n{{includes/shared}}\n",
    );
    expect_error(&f, "once includes are expanded");
}

#[test]
fn parent_cycles_are_errors() {
    let f = Fixture::new();
    f.edit(
        "team.yaml",
        "    prompt: lead.md\n",
        "    prompt: lead.md\n    parent: triager\n",
    );
    expect_error(&f, "parent cycle");
}

#[test]
fn unknown_parent_is_an_error() {
    let f = Fixture::new();
    f.edit("team.yaml", "parent: lead", "parent: boss");
    expect_error(&f, "parent `boss` is not a member");
}

#[test]
fn bad_effort_is_an_error() {
    let f = Fixture::new();
    f.edit("lead.md", "opus high", "opus extreme");
    expect_error(&f, "unknown effort");
}

#[test]
fn missing_plugin_dir_is_an_error() {
    let f = Fixture::new();
    f.edit("team.yaml", "path: plugins/slackbot", "path: plugins/nope");
    expect_error(&f, "does not exist");
}

#[test]
fn non_executable_command_is_an_error() {
    let f = Fixture::new();
    fs::set_permissions(
        f.path("plugins/slackbot/slackbot"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    expect_error(&f, "not executable");
}

#[test]
fn grant_of_undeclared_plugin_is_an_error() {
    let f = Fixture::new();
    f.edit(
        "team.yaml",
        "plugins: {slackbot: [read, reply]}",
        "plugins: {pager: [page]}",
    );
    expect_error(&f, "undeclared plugin `pager`");
}

#[test]
fn grant_of_undeclared_action_is_an_error() {
    let f = Fixture::new();
    f.edit("team.yaml", "[read, escalate]", "[read, delete]");
    expect_error(&f, "undeclared action `delete`");
}

#[test]
fn undeclared_env_in_deployment_is_an_error() {
    let f = Fixture::new();
    f.edit(
        "borgchart.yaml",
        "SLACK_CHANNEL_ID: C0000EXAMPLE",
        "SLACK_CHANNEL_ID: C0000EXAMPLE, EXTRA: x",
    );
    expect_error(&f, "env `EXTRA` is not declared");
}

#[test]
fn missing_secret_names_warn() {
    let f = Fixture::new();
    f.write("borgchart.yaml", "schema: 1\n");
    let (report, validated) = f.run();
    assert!(validated.is_some(), "{}", errors(&report));
    let w = warnings(&report);
    assert!(w.contains("SLACK_BOT_TOKEN"), "{w}");
}

#[test]
fn delivers_to_must_be_a_member() {
    let f = Fixture::new();
    f.edit("team.yaml", "delivers_to: triager", "delivers_to: nobody");
    expect_error(&f, "`delivers_to: nobody` is not a member");
}

#[test]
fn unknown_and_reserved_postures_are_errors() {
    let f = Fixture::new();
    f.edit(
        "team.yaml",
        "    posture: locked\n    untrusted_input: true\n    plugins: {slackbot: [read, reply]}",
        "    posture: open\n    plugins: {slackbot: [read, reply]}",
    );
    expect_error(&f, "unknown posture `open`");

    let f = Fixture::new();
    f.edit(
        "team.yaml",
        "    posture: locked\n    untrusted_input: true\n    plugins: {slackbot: [read, reply]}",
        "    posture: sandboxed\n    plugins: {slackbot: [read, reply]}",
    );
    expect_error(&f, "reserved for hosting");
}

#[test]
fn untrusted_input_needs_locked_or_accept_risk() {
    let f = Fixture::new();
    f.write(
        "borgchart.yaml",
        "schema: 1\nmembers:\n  lead: {posture: vendor}\nplugins:\n  slackbot:\n    env: {SLACK_CHANNEL_ID: C0000EXAMPLE, SLACK_BOT_TOKEN: x}\n",
    );
    expect_error(&f, "must be `locked`");

    let f = Fixture::new();
    f.edit(
        "team.yaml",
        "    prompt: lead.md\n    posture: locked\n",
        "    prompt: lead.md\n    posture: vendor\n    accept_risk: true\n",
    );
    let (report, validated) = f.run();
    assert!(validated.is_some(), "{}", errors(&report));
    let w = warnings(&report);
    assert!(w.contains("background process"), "{w}");
}

#[test]
fn vendor_warnings_cover_targets_and_parents() {
    let f = Fixture::new();
    f.edit(
        "team.yaml",
        "    posture: locked\n    untrusted_input: true\n    plugins: {slackbot: [read, escalate]}",
        "    plugins: {slackbot: [read, escalate]}",
    );
    f.edit(
        "team.yaml",
        "    prompt: lead.md\n    posture: locked\n    untrusted_input: true\n    plugins: {slackbot: [read, reply]}",
        "    prompt: lead.md",
    );
    let (report, validated) = f.run();
    assert!(validated.is_some(), "{}", errors(&report));
    let w = warnings(&report);
    assert!(
        w.contains("member `triager`: receives work from a plugin process"),
        "{w}"
    );
    assert!(
        w.contains("member `lead`: is the parent of a member"),
        "{w}"
    );
}

#[test]
fn api_key_with_claude_connectors_is_an_error() {
    let f = Fixture::new();
    f.edit(
        "team.yaml",
        "    plugins: {slackbot: [read, reply]}",
        "    plugins: {slackbot: [read, reply]}\n    connectors: {claude.ai Example: [read_thing]}",
    );
    f.edit(
        "borgchart.yaml",
        "schema: 1\n",
        "schema: 1\nauth: api_key\n",
    );
    expect_error(&f, "auth: api_key");
}

#[test]
fn locked_connectors_need_tool_lists() {
    let f = Fixture::new();
    f.edit(
        "team.yaml",
        "    plugins: {slackbot: [read, reply]}",
        "    plugins: {slackbot: [read, reply]}\n    connectors: [example]",
    );
    expect_error(&f, "must list connector tools");
}

#[test]
fn shared_plugin_needs_plugins_dir() {
    let f = Fixture::new();
    f.edit(
        "team.yaml",
        "path: plugins/slackbot",
        "path: shared:slackbot",
    );
    expect_error(&f, "needs `plugins_dir`");

    let shared = tempfile::tempdir().unwrap();
    let dir = shared.path().join("slackbot");
    fs::create_dir_all(&dir).unwrap();
    for name in ["slackbot", "watch.py"] {
        fs::write(dir.join(name), "#!/bin/sh\n").unwrap();
        fs::set_permissions(dir.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    f.edit(
        "borgchart.yaml",
        "schema: 1\n",
        &format!("schema: 1\nplugins_dir: {}\n", shared.path().display()),
    );
    let (report, validated) = f.run();
    assert!(validated.is_some(), "{}", errors(&report));
}

#[test]
fn bad_connector_shape_has_a_clear_message() {
    let f = Fixture::new();
    f.edit(
        "team.yaml",
        "    plugins: {slackbot: [read, reply]}",
        "    plugins: {slackbot: [read, reply]}\n    connectors: {srv: read}",
    );
    expect_error(&f, "list of server names, or a map");
}

#[test]
fn member_and_plugin_names_must_differ() {
    let f = Fixture::new();
    f.edit("team.yaml", "  lead:\n", "  slackbot:\n");
    f.edit("team.yaml", "parent: lead", "parent: slackbot");
    expect_error(&f, "both a member and a plugin");
}

#[test]
fn parse_errors_name_the_file_once() {
    let f = Fixture::new();
    f.edit(
        "team.yaml",
        "default: claude\n",
        "default: claude\ndefault: codex\n",
    );
    let (report, _) = f.run();
    let msg = errors(&report);
    assert!(
        msg.starts_with("error: ") && !msg.contains("error: error"),
        "{msg}"
    );
    assert!(!msg.contains("<input>"), "{msg}");
}

#[test]
fn auto_trust_warns_about_project_settings_for_vendor_members() {
    let f = Fixture::new();
    f.edit(
        "team.yaml",
        "    prompt: lead.md\n    posture: locked\n    untrusted_input: true\n",
        "    prompt: lead.md\n    accept_risk: true\n    untrusted_input: true\n",
    );
    f.write("work/.mcp.json", "{}");
    f.edit(
        "borgchart.yaml",
        "schema: 1\n",
        &format!(
            "schema: 1\nauto_trust: true\nmembers:\n  lead: {{cwd: {}}}\n",
            f.path("work").display()
        ),
    );
    let (report, validated) = f.run();
    assert!(validated.is_some(), "{}", errors(&report));
    assert!(
        warnings(&report).contains("has project settings"),
        "{}",
        warnings(&report)
    );
}

#[test]
fn auto_trust_warns_about_codex_project_settings_and_locked_codex_members() {
    // Catches: a Codex folder whose `.codex/` would load without a warning, a
    // `locked` Codex member silently left at its trust screen, and a warning
    // that brg will trust a folder when the member's own `projects` args win.
    for (posture, args, want, warned) in [
        (
            "    accept_risk: true\n",
            "",
            "has project settings (`.codex/`)",
            true,
        ),
        ("    posture: locked\n", "", "doesn't trust", true),
        (
            "    accept_risk: true\n",
            ", args: [-c, 'projects={}']",
            "has project settings (`.codex/`)",
            false,
        ),
    ] {
        let f = Fixture::new();
        f.edit(
            "team.yaml",
            "    prompt: lead.md\n    posture: locked\n",
            &format!("    prompt: lead.md\n    vendor: codex\n{posture}"),
        );
        f.write("work/.codex/config.toml", "");
        f.edit(
            "borgchart.yaml",
            "schema: 1\n",
            &format!(
                "schema: 1\nauto_trust: true\nmembers:\n  lead: {{cwd: {}{args}}}\n",
                f.path("work").display()
            ),
        );
        let (report, validated) = f.run();
        assert!(validated.is_some(), "{}", errors(&report));
        assert_eq!(
            warnings(&report).contains(want),
            warned,
            "{args}: {}",
            warnings(&report)
        );
    }
}

#[test]
fn user_tmux_config_risks_are_warned_with_their_lines() {
    // Catches: a user tmux config whose scripts, hooks, session-restore
    // plugins or load-time sessions could undo brg's options or outlive
    // `brg down` passing validation silently, including in a file it sources;
    // a relative include (which tmux finds from wherever `brg up` runs)
    // passing as checked; warnings for key bindings, which act only on a key
    // press; and a missing or glob-like config path passing as fine.
    let f = Fixture::new();
    f.write(
        "tmux/main.conf",
        &format!(
            "set -g mouse on\nrun-shell 'echo hi'\nset-hook -g after-new-window 'rename-window x'\nset -g @plugin 'tmux-plugins/tmux-resurrect'\nsource-file -q {}\n# run 'commented out'\nbind-key R run-shell 'echo only on a key press'\nif-shell 'true' 'set -g status on'\nnew-session -A -s main\nsource-file extra.conf\n",
            f.path("tmux/extra.conf").display()
        ),
    );
    f.write(
        "tmux/extra.conf",
        "bind r source-file ~/.tmux.conf\nrun -b 'sleep 1'\n",
    );
    f.edit(
        "borgchart.yaml",
        "schema: 1\n",
        &format!(
            "schema: 1\ntmux: {{user_config: {}}}\n",
            f.path("tmux/main.conf").display()
        ),
    );
    let (report, validated) = f.run();
    assert!(validated.is_some(), "{}", errors(&report));
    let w = warnings(&report);
    // (file, what it's about, where). The key binding on line 7 runs only on
    // a key press, so nothing names it.
    for (file, about, at) in [
        ("main.conf", "run-shell", "lines 2, 8;"),
        ("main.conf", "set-hook", "line 3;"),
        ("main.conf", "save/restore", "line 4;"),
        ("main.conf", "sessions", "line 9;"),
        ("main.conf", "relative path", "line 10;"),
        ("extra.conf", "run-shell", "line 2;"),
    ] {
        assert!(
            w.lines()
                .any(|l| l.contains(file) && l.contains(about) && l.contains(at)),
            "{file} {about} {at}\n{w}"
        );
    }

    let f = Fixture::new();
    f.write("user[1].conf", "");
    f.edit(
        "borgchart.yaml",
        "schema: 1\n",
        &format!(
            "schema: 1\ntmux: {{user_config: '{}'}}\n",
            f.path("user[1].conf").display()
        ),
    );
    expect_error(&f, "can't load a path");

    let f = Fixture::new();
    f.edit(
        "borgchart.yaml",
        "schema: 1\n",
        &format!(
            "schema: 1\ntmux: {{user_config: {}}}\n",
            f.path("missing.conf").display()
        ),
    );
    expect_error(&f, "is not a file");
}

#[test]
fn deployment_must_match_the_team() {
    let f = Fixture::new();
    f.edit(
        "borgchart.yaml",
        "schema: 1\n",
        "schema: 1\nmembers:\n  ghost: {task: x}\n",
    );
    expect_error(&f, "member `ghost` is not in the team");
}

#[test]
fn wrong_schema_is_an_error() {
    let f = Fixture::new();
    f.edit("borgchart.yaml", "schema: 1", "schema: 2");
    expect_error(&f, "unsupported `schema: 2`");
}

#[test]
fn team_alone_validates_without_deployment() {
    let f = Fixture::new();
    let (report, validated) = f.run_team();
    assert!(validated.is_some(), "{}", errors(&report));
}

#[test]
fn cli_reports_json_and_exit_codes() {
    let f = Fixture::new();
    let ok = common::command(env!("CARGO_BIN_EXE_brg"))
        .args(["validate", "--json", "--file"])
        .arg(f.path("borgchart.yaml"))
        .output()
        .unwrap();
    assert!(ok.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&ok.stdout).unwrap();
    assert_eq!(parsed["ok"], true);

    f.edit("team.yaml", "parent: lead", "parent: boss");
    let bad = common::command(env!("CARGO_BIN_EXE_borgchart"))
        .args(["validate", "--file"])
        .arg(f.path("borgchart.yaml"))
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("parent `boss`"));

    let empty = tempfile::tempdir().unwrap();
    let usage = common::command(env!("CARGO_BIN_EXE_brg"))
        .arg("validate")
        .current_dir(empty.path())
        .output()
        .unwrap();
    assert_eq!(usage.status.code(), Some(2));
}

/// A vendor-posture pair with an alert plugin, deployed with a bus whose
/// run root sits beside (not inside) the team.
fn bus_fixture() -> (Fixture, PathBuf) {
    let f = Fixture {
        dir: tempfile::tempdir().unwrap(),
    };
    f.write(
        "team/team.yaml",
        r#"
plugins:
  notify:
    path: plugins/notify
    actions:
      alert: notify alert <id> <member> <reason> <pending>
members:
  lead:
    prompt: lead.md
  helper:
    prompt: helper.md
    parent: lead
"#,
    );
    f.write("team/lead.md", "---\nrole: lead\n---\nLead.\n");
    f.write("team/helper.md", "---\nrole: helper\n---\nHelp.\n");
    f.exec("team/plugins/notify/notify");
    let runs = f.path("runs");
    f.write(
        "team/borgchart.yaml",
        &format!(
            "schema: 1\nbus:\n  root: {}\n  alert: {{plugin: notify, action: alert}}\n",
            runs.display()
        ),
    );
    (f, runs)
}

fn run_bus(f: &Fixture) -> (Report, Option<Validated>) {
    validate(&Input::Deployment(f.path("team/borgchart.yaml")))
}

#[test]
fn bus_config_loads() {
    let (f, runs) = bus_fixture();
    let (report, validated) = run_bus(&f);
    let v = validated.unwrap_or_else(|| panic!("{}", errors(&report)));
    let bus = v.deployment.unwrap().bus.unwrap();
    assert_eq!(bus.root, runs);
    assert_eq!(bus.warn_age_days, 7);
    assert_eq!(bus.max_age_days, 14);
    let alert = bus.alert.unwrap();
    assert_eq!(
        (alert.plugin.as_str(), alert.action.as_str()),
        ("notify", "alert")
    );
}

#[test]
fn bus_config_is_checked() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "  alert:",
            "  run: example-run\n  alert:",
            "unknown field `run`",
        ),
        ("action: alert", "action: page", "has no action `page`"),
        (
            "plugin: notify",
            "plugin: pager",
            "alert plugin `pager` is not in the team",
        ),
        ("  alert:", "  warn_age_days: 14\n  alert:", "warn_age_days"),
        ("  alert:", "  extra: 1\n  alert:", "unknown field"),
    ];
    for (from, to, needle) in cases {
        let (f, _) = bus_fixture();
        f.edit("team/borgchart.yaml", from, to);
        let (report, validated) = run_bus(&f);
        let errs = errors(&report);
        assert!(validated.is_none(), "{to}: expected failure");
        assert!(errs.contains(needle), "{to}: wanted {needle:?} in:\n{errs}");
    }
}

#[test]
fn bus_root_must_be_absolute_and_outside_the_team() {
    let (f, runs) = bus_fixture();
    f.edit("team/borgchart.yaml", &runs.display().to_string(), "runs");
    let (report, _) = run_bus(&f);
    assert!(errors(&report).contains("must be an absolute path"));

    let (f, runs) = bus_fixture();
    let inside = f.path("team/runs");
    f.edit(
        "team/borgchart.yaml",
        &runs.display().to_string(),
        &inside.display().to_string(),
    );
    let (report, validated) = run_bus(&f);
    assert!(validated.is_none());
    assert!(
        errors(&report).contains("keep team messages outside"),
        "{}",
        errors(&report)
    );
}

#[test]
fn bus_refuses_locked_members() {
    let (f, _) = bus_fixture();
    f.edit(
        "team/team.yaml",
        "    prompt: helper.md\n",
        "    prompt: helper.md\n    posture: locked\n",
    );
    let (report, validated) = run_bus(&f);
    assert!(validated.is_none());
    assert!(
        errors(&report).contains("doesn't support `locked` members"),
        "{}",
        errors(&report)
    );
}

#[test]
fn config_snapshot_follows_file_bytes_not_paths() {
    let (f, _) = bus_fixture();
    let snap = |path: PathBuf| {
        let (report, v) = validate(&Input::Deployment(path));
        v.unwrap_or_else(|| panic!("{}", errors(&report)))
            .config_snapshot()
    };
    let a = snap(f.path("team/borgchart.yaml"));
    assert_eq!(a.version, 1);
    assert!(a.inputs.contains_key("deployment"));
    assert!(a.inputs.contains_key("members/helper/instruction"));

    let alias = f.path("alias");
    std::os::unix::fs::symlink(f.path("team"), &alias).unwrap();
    assert_eq!(
        snap(alias.join("borgchart.yaml")),
        a,
        "same bytes, other path"
    );

    f.edit("team/helper.md", "Help.", "Help more.");
    let b = snap(f.path("team/borgchart.yaml"));
    assert_ne!(a.digest, b.digest);
    assert_ne!(
        a.inputs["members/helper/prompt"],
        b.inputs["members/helper/prompt"]
    );
    assert_eq!(a.inputs["team.yaml"], b.inputs["team.yaml"]);
}

#[test]
fn snapshot_describes_what_was_validated() {
    let (f, _) = bus_fixture();
    let (report, old) = run_bus(&f);
    let old = old.unwrap_or_else(|| panic!("{}", errors(&report)));
    let before = old.config_snapshot();
    f.edit("team/team.yaml", "    parent: lead\n", "");
    let (report, new) = run_bus(&f);
    let new = new.unwrap_or_else(|| panic!("{}", errors(&report)));
    assert_eq!(old.config_snapshot(), before, "nothing is re-read");
    assert_ne!(new.config_snapshot().digest, before.digest);
    fs::remove_file(f.path("team/team.yaml")).unwrap();
    assert_eq!(old.config_snapshot(), before);
}

#[test]
fn missing_bus_root_through_a_symlink_is_placed_correctly() {
    let (f, runs) = bus_fixture();
    std::os::unix::fs::symlink(f.path("team"), f.path("alias")).unwrap();
    let sneaky = f.path("alias/not-created/yet");
    f.edit(
        "team/borgchart.yaml",
        &runs.display().to_string(),
        &sneaky.display().to_string(),
    );
    let (report, validated) = run_bus(&f);
    assert!(validated.is_none());
    assert!(
        errors(&report).contains("keep team messages outside"),
        "{}",
        errors(&report)
    );
}
