//! Loader and `brg validate` tests against teams built in temporary directories.

mod team_fixture;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use borgchart::config::Config;
use borgchart::diag::Report;
use borgchart::team::{Grant, Posture, Vendor};
use borgchart::validate::{Validated, validate_with};

use team_fixture::Fixture;

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
    let triager = &v.team.roles["triager"];
    assert_eq!(triager.vendor, Vendor::Claude);
    assert_eq!(triager.model.as_ref().unwrap().name, "haiku");
    assert_eq!(triager.posture, Posture::Locked);
    assert!(triager.instruction.contains("Be brief."));
    assert!(!triager.instruction.contains("{{includes"));
    assert_eq!(
        triager.grants["slackbot"],
        Grant::Actions(vec!["read".into(), "escalate".into()])
    );
    let lead = &v.team.roles["lead"];
    assert_eq!(lead.model.as_ref().unwrap().effort.as_deref(), Some("high"));
    assert_eq!(
        v.team.plugins["slackbot"].actions["escalate"].prefix,
        vec!["slackbot", "escalate"]
    );
}

#[test]
fn copy_limits_refuse_zero_and_require_member_as_the_last_folder() {
    for (max, cwd, accepted) in [
        (-1, Some("copies/{member_id}"), false),
        (0, Some("copies/{member_id}"), false),
        (1, None, true),
        (1, Some("shared"), true),
        (1, Some("copies/{member_id}/src"), false),
        (2, None, true),
        (2, Some("copies/{member_id}"), true),
        (2, Some("copies/{member_id}/src"), false),
        (2, Some("copies/{member_id}/{member_id}"), false),
        (2, Some("shared"), true),
        (2, Some("copies/prefix-{member_id}"), false),
        (2, Some("copies/{member_id}-suffix"), false),
        (2, Some("copies/{member_id}/../shared"), false),
    ] {
        let f = Fixture::new();
        // Use a new leaf so neither parent nor plugin delivery is ambiguous.
        let path = f.path("team.yaml");
        let text = fs::read_to_string(&path).unwrap();
        let cwd_line = cwd.map(|c| format!("    cwd: '{c}'\n")).unwrap_or_default();
        fs::write(
            path,
            format!("{text}\n  helper:\n    prompt: lead.md\n    max: {max}\n{cwd_line}"),
        )
        .unwrap();
        let (report, validated) = f.run();
        assert_eq!(
            validated.is_some(),
            accepted,
            "max={max}, cwd={cwd:?}: {}",
            errors(&report)
        );
    }
}

#[test]
fn parents_and_plugin_delivery_cannot_choose_between_copies() {
    for target in ["lead", "triager"] {
        let f = Fixture::new();
        f.edit(
            "team.yaml",
            &format!("    prompt: {target}.md\n"),
            &format!("    prompt: {target}.md\n    max: 2\n    cwd: copies/{{member_id}}\n"),
        );
        let (report, validated) = f.run();
        assert!(
            validated.is_none(),
            "ambiguous target {target} accepted: {}",
            errors(&report)
        );
    }
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
    f.edit("lead.md", "models:", "mood: calm\nmodels:");
    expect_error(&f, "mood");
}

#[test]
fn empty_frontmatter_uses_vendor_defaults() {
    let f = Fixture::new();
    for frontmatter in ["", " \n\t\n"] {
        f.write("lead.md", &format!("---\n{frontmatter}---\nBody\n"));
        let (report, validated) = f.run();
        assert!(!report.has_errors(), "{}", errors(&report));
        assert!(validated.is_some());
    }
}

#[test]
fn frontmatter_must_close() {
    let f = Fixture::new();
    f.write("lead.md", "---\nTriage.\n");
    expect_error(&f, "not closed");
}

#[test]
fn frontmatter_bom_and_crlf_are_accepted() {
    let f = Fixture::new();
    f.write(
        "lead.md",
        "\u{feff}---\r\nmodels: {claude: opus}\r\n---\r\nTriage.\r\n---\r\nStill body.\r\n",
    );
    let (report, validated) = f.run();
    assert!(!report.has_errors(), "{}", errors(&report));
    let lead = &validated.unwrap().team.roles["lead"];
    assert!(lead.instruction.contains("Still body."));
}

#[test]
fn prompt_symlink_outside_team_is_refused() {
    let f = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("lead.md");
    fs::write(&target, "---\n---\nx\n").unwrap();
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
        "---\n---\n{{includes/shared}}\n{{includes/shared}}\n",
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
    expect_error(&f, "parent `boss` is not a role");
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
fn plugin_env_names_are_listed_once() {
    let f = Fixture::new();
    f.edit(
        "team.yaml",
        "{SLACK_CHANNEL_ID: C0000EXAMPLE}",
        "{SLACK_CHANNEL_ID: C0000EXAMPLE}, SLACK_CHANNEL_ID",
    );
    expect_error(&f, "env `SLACK_CHANNEL_ID` is listed twice");
}

#[test]
fn plugin_env_takes_plain_values_and_secrets() {
    let f = Fixture::new();
    let (report, validated) = f.run();
    let v = validated.unwrap_or_else(|| panic!("{}", errors(&report)));
    let p = &v.team.plugins["slackbot"];
    assert_eq!(p.env, ["SLACK_BOT_TOKEN", "SLACK_CHANNEL_ID"]);
    assert_eq!(p.values["SLACK_CHANNEL_ID"], "C0000EXAMPLE");
    assert_eq!(p.secrets().collect::<Vec<_>>(), ["SLACK_BOT_TOKEN"]);
    // The map form: a name without a value is a secret.
    f.edit(
        "team.yaml",
        "env: [SLACK_BOT_TOKEN, {SLACK_CHANNEL_ID: C0000EXAMPLE}]",
        "env:\n      SLACK_BOT_TOKEN:\n      SLACK_CHANNEL_ID: C0000EXAMPLE",
    );
    let (report, validated) = f.run();
    let v = validated.unwrap_or_else(|| panic!("{}", errors(&report)));
    let p = &v.team.plugins["slackbot"];
    assert_eq!(p.secrets().collect::<Vec<_>>(), ["SLACK_BOT_TOKEN"]);
    assert_eq!(p.values["SLACK_CHANNEL_ID"], "C0000EXAMPLE");
}

#[test]
fn missing_secret_names_warn() {
    let f = Fixture::new();
    let (report, validated) = f.run();
    assert!(validated.is_some(), "{}", errors(&report));
    let w = warnings(&report);
    assert!(w.contains("SLACK_BOT_TOKEN"), "{w}");
    assert!(!w.contains("SLACK_CHANNEL_ID"), "{w}");
}

#[test]
fn delivers_to_must_be_a_role() {
    let f = Fixture::new();
    f.edit("team.yaml", "delivers_to: triager", "delivers_to: nobody");
    expect_error(&f, "`delivers_to: nobody` is not a role");
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
    f.edit(
        "team.yaml",
        "    prompt: lead.md\n    posture: locked\n",
        "    prompt: lead.md\n    posture: vendor\n",
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
    assert!(w.contains("`triager`"), "{w}");
    assert!(w.contains("`lead`"), "{w}");
}

#[test]
fn api_key_with_claude_connectors_is_an_error() {
    let f = Fixture::new();
    f.edit(
        "team.yaml",
        "    plugins: {slackbot: [read, reply]}",
        "    plugins: {slackbot: [read, reply]}\n    connectors: {claude.ai Example: [read_thing]}",
    );
    f.edit("config.yaml", "schema: 1\n", "schema: 1\nauth: api_key\n");
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
        "config.yaml",
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
fn role_and_plugin_names_must_differ() {
    let f = Fixture::new();
    f.edit("team.yaml", "  lead:\n", "  slackbot:\n");
    f.edit("team.yaml", "parent: lead", "parent: slackbot");
    expect_error(&f, "`slackbot`");
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
fn auto_trust_warns_about_locked_codex_members() {
    // Catches: a `locked` Codex member silently left at its trust screen.
    let f = Fixture::new();
    f.edit(
        "team.yaml",
        "    prompt: lead.md\n    posture: locked\n",
        "    prompt: lead.md\n    vendor: codex\n    posture: locked\n",
    );
    f.edit(
        "config.yaml",
        "schema: 1\n",
        "schema: 1\nauto_trust: true\n",
    );
    let (report, validated) = f.run();
    assert!(validated.is_some(), "{}", errors(&report));
    assert!(
        warnings(&report).contains("doesn't trust"),
        "{}",
        warnings(&report)
    );
    f.edit("config.yaml", "auto_trust: true", "auto_trust: false");
    let (report, _) = f.run();
    assert!(!warnings(&report).contains("doesn't trust"));
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
        "config.yaml",
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
        "config.yaml",
        "schema: 1\n",
        &format!(
            "schema: 1\ntmux: {{user_config: '{}'}}\n",
            f.path("user[1].conf").display()
        ),
    );
    expect_error(&f, "can't load a path");

    let f = Fixture::new();
    f.edit(
        "config.yaml",
        "schema: 1\n",
        &format!(
            "schema: 1\ntmux: {{user_config: {}}}\n",
            f.path("missing.conf").display()
        ),
    );
    expect_error(&f, "is not a file");
}

#[test]
fn the_configuration_is_strict() {
    let f = Fixture::new();
    f.edit("config.yaml", "schema: 1\n", "schema: 1\nmembers: {}\n");
    expect_error(&f, "unknown field `members`");
}

#[test]
fn wrong_schema_is_an_error() {
    let f = Fixture::new();
    f.edit("config.yaml", "schema: 1", "schema: 2");
    expect_error(&f, "unsupported `schema: 2`");
}

#[test]
fn removed_team_fields_are_errors() {
    let f = Fixture::new();
    f.edit(
        "team.yaml",
        "default: claude\n",
        "default: claude\nname: other\n",
    );
    expect_error(&f, "unknown field `name`");
}

#[test]
fn a_missing_configuration_means_the_defaults() {
    let f = Fixture::new();
    let (report, validated) = f.run_team();
    assert!(validated.is_some(), "{}", errors(&report));
}

/// A vendor-posture pair with an alert plugin and a bus.
fn bus_fixture() -> Fixture {
    let f = Fixture {
        dir: tempfile::tempdir().unwrap(),
    };
    f.write(
        "team/team.yaml",
        r#"
bus:
  alert: {plugin: notify, action: alert}
plugins:
  notify:
    path: plugins/notify
    actions:
      alert: notify alert <id> <member> <reason> <pending>
roles:
  lead:
    prompt: lead.md
  helper:
    prompt: helper.md
    parent: lead
"#,
    );
    f.write("team/lead.md", "---\n---\nLead.\n");
    f.write("team/helper.md", "---\n---\nHelp.\n");
    f.exec("team/plugins/notify/notify");
    f
}

fn run_bus(f: &Fixture) -> (Report, Option<Validated>) {
    validate_with(&f.path("team"), Config::default())
}

#[test]
fn bus_config_loads() {
    let f = bus_fixture();
    let (report, validated) = run_bus(&f);
    let v = validated.unwrap_or_else(|| panic!("{}", errors(&report)));
    let alert = v.team.bus.unwrap().alert.unwrap();
    assert_eq!(
        (alert.plugin.as_str(), alert.action.as_str()),
        ("notify", "alert")
    );
    // `bus: {}` turns the bus on without an alert.
    f.edit(
        "team/team.yaml",
        "  alert: {plugin: notify, action: alert}\n",
        "",
    );
    f.edit("team/team.yaml", "bus:\n", "bus: {}\n");
    let (report, validated) = run_bus(&f);
    let v = validated.unwrap_or_else(|| panic!("{}", errors(&report)));
    assert_eq!(v.team.bus.unwrap().alert, None);
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
        (
            "  alert:",
            "  root: /somewhere\n  alert:",
            "unknown field `root`",
        ),
    ];
    for (from, to, needle) in cases {
        let f = bus_fixture();
        f.edit("team/team.yaml", from, to);
        let (report, validated) = run_bus(&f);
        let errs = errors(&report);
        assert!(validated.is_none(), "{to}: expected failure");
        assert!(errs.contains(needle), "{to}: wanted {needle:?} in:\n{errs}");
    }
}

#[test]
fn bus_refuses_locked_members() {
    let f = bus_fixture();
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
    let f = bus_fixture();
    let snap = |path: PathBuf, config: Config| {
        let (report, v) = validate_with(&path, config);
        v.unwrap_or_else(|| panic!("{}", errors(&report)))
            .config_snapshot()
    };
    let a = snap(f.path("team"), Config::default());
    assert_eq!(a.version, 1);
    assert!(a.inputs.contains_key("config/auth"));
    assert!(a.inputs.contains_key("config/auto_trust"));
    assert!(a.inputs.contains_key("members/helper/instruction"));

    let alias = f.path("alias");
    std::os::unix::fs::symlink(f.path("team"), &alias).unwrap();
    assert_eq!(snap(alias, Config::default()), a, "same bytes, other path");

    // The global settings that reach members are frozen too.
    let api = Config {
        auth: borgchart::config::Auth::ApiKey,
        ..Config::default()
    };
    let c = snap(f.path("team"), api);
    assert_ne!(a.inputs["config/auth"], c.inputs["config/auth"]);
    let trust = Config {
        auto_trust: Some(true),
        ..Config::default()
    };
    assert_ne!(snap(f.path("team"), trust).digest, a.digest);

    f.edit("team/helper.md", "Help.", "Help more.");
    let b = snap(f.path("team"), Config::default());
    assert_ne!(a.digest, b.digest);
    assert_ne!(
        a.inputs["members/helper/prompt"],
        b.inputs["members/helper/prompt"]
    );
    assert_eq!(a.inputs["team.yaml"], b.inputs["team.yaml"]);
}

#[test]
fn snapshot_describes_what_was_validated() {
    let f = bus_fixture();
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
fn a_role_cwd_must_stay_inside_the_folder() {
    for (cwd, ok) in [
        ("copies/{member}", false),
        ("sub", true),
        ("copies/{member_id}", true),
        ("../x", false),
        ("/abs", false),
        ("~/x", false),
        ("{instance}", false),
    ] {
        let f = Fixture::new();
        f.edit(
            "team.yaml",
            "    prompt: lead.md\n",
            &format!("    prompt: lead.md\n    cwd: '{cwd}'\n    args: [--x]\n"),
        );
        let (report, validated) = f.run();
        assert_eq!(validated.is_some(), ok, "{cwd}: {}", errors(&report));
        if let Some(v) = validated {
            assert_eq!(v.team.roles["lead"].args, ["--x"]);
            assert_eq!(v.team.roles["lead"].cwd.as_deref(), Some(cwd));
        }
    }
}
