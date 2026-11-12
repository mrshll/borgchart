//! Create-time choices must reach status and the vendor commands across restarts.
#![cfg(feature = "test-support")]

mod common;
mod team_env;

use serde_json::json;
use std::fs;
use team_env::*;

fn setup() -> (Env, String) {
    let env = Env::new();
    env.write("templates/oncall/template.yaml", "bus: {}\nroles:\n  lead: {prompt: lead.md}\n  dev: {template: child, parent: lead, max: 8, worktree: true}\n");
    env.write("templates/child/template.yaml", "bus: {}\nroles:\n  lead: {prompt: lead.md}\n  worker: {prompt: worker.md, parent: lead, max: 8, worktree: true, start: false}\n  nested: {template: leaf, parent: lead}\n");
    env.write(
        "templates/leaf/template.yaml",
        "roles:\n  lead: {prompt: lead.md}\n",
    );
    for path in ["child/lead", "child/worker", "leaf/lead"] {
        env.write(&format!("templates/{path}.md"), "---\nmodels: {claude: opus low, codex: example-model medium}\n---\nWork on the example.\n");
    }
    env.checkout("work/repo");
    let parent = env.create("oncall", "work/repo");
    (env, parent)
}

fn child(env: &Env, parent: &str, choices: &[&str]) -> String {
    let mut args = vec!["create", "", "--name", "sample"];
    let target = format!("{parent}/dev");
    args[1] = &target;
    args.extend_from_slice(choices);
    let out = env.ok(&args);
    stdout(&out).split_whitespace().next().unwrap().into()
}

fn launched(env: &Env, id: &str, count: usize, vendor: &str, model: &str, effort: &str) {
    env.wait_for("vendor launch", || env.starts(id).len() >= count);
    let starts = env.starts(id);
    let argv: Vec<&str> = starts[count - 1]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(std::path::Path::new(argv[0]).file_name().unwrap(), vendor);
    assert!(
        argv.windows(2)
            .any(|w| w == [if vendor == "codex" { "-m" } else { "--model" }, model]),
        "{argv:?}"
    );
    if vendor == "codex" {
        assert!(
            argv.contains(&format!("model_reasoning_effort=\"{effort}\"").as_str()),
            "{argv:?}"
        );
    } else {
        assert!(
            argv.windows(2).any(|w| w == ["--effort", effort]),
            "{argv:?}"
        );
    }
}

#[test]
fn child_choice_reaches_empty_role_launch_and_resume() {
    let (env, parent) = setup();
    let child = child(&env, &parent, &["--set", "lead.vendor=codex"]);
    let row = env.row(&child, "lead");
    assert_eq!(row["kind"], "role");
    assert_eq!(row["vendor"], "codex");
    assert_eq!(row["model"], "example-model");
    assert_eq!(row["effort"], "medium");
    assert_eq!(row["overrides"], json!({"vendor": "codex"}));
    println!("before start: {row}");
    assert!(stdout(&env.ok(&["status", &child])).contains("codex example-model medium"));
    env.ok(&["start", &child]);
    let id = env.member(&child, "lead");
    launched(&env, &id, 1, "codex", "example-model", "medium");
    env.wait_for("lead idle", || env.state_of(&child, "lead") == "idle");
    env.ok(&["stop", &child]);
    env.ok(&["start", &child, "--resume"]);
    launched(&env, &id, 2, "codex", "example-model", "medium");
    assert_eq!(env.starts(&id)[1]["argv"][1], "resume");
    env.ok(&["stop", &parent]);
}

#[test]
fn member_choices_do_not_leak_and_beat_team_choices() {
    let (env, parent) = setup();
    let child = child(
        &env,
        &parent,
        &["--set", "worker.vendor=codex,worker.effort=high"],
    );
    env.ok(&["start", &child]);
    let target = format!("{child}/worker");
    env.ok(&["create", &target, "--name", "first"]);
    env.ok(&[
        "create",
        &target,
        "--name",
        "second",
        "--set",
        "vendor=claude",
        "--set",
        "effort=low",
    ]);
    launched(&env, "worker-first", 1, "codex", "example-model", "high");
    launched(&env, "worker-second", 1, "claude", "opus", "low");
    let rows = env.rows(&child);
    let second = rows
        .iter()
        .find(|r| r["member"] == "worker-second")
        .unwrap();
    assert_eq!(
        second["overrides"],
        json!({"vendor":"claude", "effort":"low"})
    );
    env.ok(&["stop", &parent]);
}

#[test]
fn vendor_only_member_uses_edited_pin_at_next_start() {
    let (env, parent) = setup();
    let child = child(&env, &parent, &[]);
    env.ok(&["start", &child]);
    env.ok(&[
        "create",
        &format!("{child}/worker"),
        "--name",
        "sample",
        "--set",
        "vendor=codex",
    ]);
    launched(&env, "worker-sample", 1, "codex", "example-model", "medium");
    let address = format!("{child}/worker-sample");
    env.ok(&["stop", &address]);
    env.write(
        "templates/child/worker.md",
        "---\nmodels: {claude: opus low, codex: new-model high}\n---\nWork on the example.\n",
    );
    env.ok(&["start", &address]);
    launched(&env, "worker-sample", 2, "codex", "new-model", "high");
    env.ok(&["stop", &address]);
    let template = fs::read_to_string(env.path("templates/child/template.yaml")).unwrap();
    env.write(
        "templates/child/template.yaml",
        &template.replace(
            "prompt: worker.md,",
            "prompt: worker.md, args: [--verbose],",
        ),
    );
    env.fails(&["start", &address]);
    assert_eq!(env.starts("worker-sample").len(), 2);
    env.ok(&["stop", &parent]);
}

#[test]
fn invalid_choices_leave_records_and_worktrees_untouched() {
    let (env, parent) = setup();
    let child = child(&env, &parent, &[]);
    env.ok(&["start", &child]);
    let template = fs::read_to_string(env.path("templates/child/template.yaml")).unwrap();
    env.write(
        "templates/child/template.yaml",
        &template.replace(
            "prompt: worker.md,",
            "prompt: worker.md, args: [--verbose],",
        ),
    );
    let record = |id: &str| fs::read(env.path(&format!("state/{id}/team.json"))).unwrap();
    let before = (record(&parent), record(&child), env.registry());
    let worktrees = git(&env.path("work/repo"), &["worktree", "list", "--porcelain"]);
    for (role, choice) in [
        ("worker", "vendor=codex"),
        ("worker", "worker.vendor=codex"),
        ("dev", "vendor=codex"),
        ("dev", "missing.vendor=codex"),
        ("dev", "lead.vendor=codex,lead.vendor=claude"),
        ("dev", "nested.vendor=codex"),
        ("worker", "model=-bad"),
        ("worker", "model=two words"),
        ("worker", "unknown=value"),
        ("worker", "effort=extreme"),
    ] {
        let team = if role == "dev" { &parent } else { &child };
        let error = env.fails(&["create", &format!("{team}/{role}"), "--set", choice]);
        println!("refused {choice}: {error}");
        assert_eq!((record(&parent), record(&child), env.registry()), before);
        assert_eq!(
            git(&env.path("work/repo"), &["worktree", "list", "--porcelain"]),
            worktrees
        );
    }
    env.fails(&["create", "oncall", "--set", "vendor=codex"]);
    env.ok(&[
        "create",
        &format!("{child}/worker"),
        "--set",
        "model=custom",
    ]);
    env.ok(&["stop", &parent]);
}

#[test]
fn api_key_launch_requires_the_chosen_vendors_key() {
    let env = Env::new();
    env.write(
        "templates/oncall/scout.md",
        "---\nmodels: {claude: opus low, codex: example-model medium}\n---\nWork on the example.\n",
    );
    let parent = env.create("oncall", "work/a");
    env.ok(&["stop", &parent]);
    env.config("auth: api_key\n");
    let out = env
        .cmd(&[
            "create",
            &format!("{parent}/scout"),
            "--name",
            "sample",
            "--set",
            "vendor=codex",
        ])
        .env("OPENAI_API_KEY", "fictional-test-key")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    launched(&env, "scout-sample", 1, "codex", "example-model", "medium");
    let event = &env.starts("scout-sample")[0];
    assert_eq!(event["env"]["OPENAI_API_KEY"], "fictional-test-key");
    assert!(event["env"]["ANTHROPIC_API_KEY"].is_null());
    env.ok(&["stop", &parent]);
}
