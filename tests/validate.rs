//! Loader and `brg validate` tests against teams built in temporary directories.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

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
    let ok = Command::new(env!("CARGO_BIN_EXE_brg"))
        .args(["validate", "--json", "--file"])
        .arg(f.path("borgchart.yaml"))
        .output()
        .unwrap();
    assert!(ok.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&ok.stdout).unwrap();
    assert_eq!(parsed["ok"], true);

    f.edit("team.yaml", "parent: lead", "parent: boss");
    let bad = Command::new(env!("CARGO_BIN_EXE_borgchart"))
        .args(["validate", "--file"])
        .arg(f.path("borgchart.yaml"))
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("parent `boss`"));

    let empty = tempfile::tempdir().unwrap();
    let usage = Command::new(env!("CARGO_BIN_EXE_brg"))
        .arg("validate")
        .current_dir(empty.path())
        .output()
        .unwrap();
    assert_eq!(usage.status.code(), Some(2));
}
