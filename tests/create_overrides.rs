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
fn child_vendor_choices_accept_locked_and_explicitly_empty_connector_roles() {
    let (env, parent) = setup();
    env.write("templates/child/template.yaml", "bus: {}\nroles:\n  triager: {prompt: lead.md, connectors: []}\n  watcher: {prompt: worker.md, parent: triager, posture: locked}\n");
    let child = child(
        &env,
        &parent,
        &["--set", "triager.vendor=codex,watcher.vendor=codex"],
    );
    for role in ["triager", "watcher"] {
        assert_eq!(env.row(&child, role)["vendor"], "codex");
    }
    println!("{}", stdout(&env.ok(&["status", &child])));
    env.ok(&["stop", &parent]);
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
    let error = env.fails(&["start", &address]);
    assert!(
        error.contains(&format!("`{address}` was created with `vendor=codex`")),
        "{error}"
    );
    assert!(error.contains(&format!("brg delete {address}")), "{error}");
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
    for (role, choice, refusal) in [
        ("worker", "vendor=codex", "sets `args`"),
        ("worker", "worker.vendor=codex", "is one agent"),
        ("dev", "vendor=codex", "say which of its roles"),
        ("dev", "missing.vendor=codex", "unknown role `missing`"),
        ("dev", "lead.vendor=codex,lead.vendor=claude", "set twice"),
        ("dev", "nested.vendor=codex", "`nested` is a team role"),
        ("worker", "model=-bad", "leading `-`"),
        ("worker", "model=two words", "without whitespace"),
        ("worker", "unknown=value", "unknown key `unknown`"),
        ("worker", "effort=extreme", "choose an effort"),
    ] {
        let team = if role == "dev" { &parent } else { &child };
        let error = env.fails(&["create", &format!("{team}/{role}"), "--set", choice]);
        println!("refused {choice}: {error}");
        assert!(error.contains(refusal), "{choice}: {error}");
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
fn stale_member_choice_does_not_block_other_starts_or_creates() {
    let (env, parent) = setup();
    let template = fs::read_to_string(env.path("templates/child/template.yaml")).unwrap();
    env.write(
        "templates/child/template.yaml",
        &template.replace(
            "  nested:",
            "  helper: {prompt: worker.md, parent: lead, max: 8, start: false}\n  nested:",
        ),
    );
    let child = child(&env, &parent, &["--set", "worker.vendor=codex"]);
    env.ok(&["start", &child]);
    env.ok(&[
        "create",
        &format!("{child}/worker"),
        "--name",
        "stale",
        "--set",
        "vendor=codex",
    ]);
    env.ok(&[
        "create",
        &format!("{child}/worker"),
        "--name",
        "inherited",
        "--set",
        "model=custom",
    ]);
    env.ok(&[
        "create",
        &format!("{child}/worker"),
        "--name",
        "masked",
        "--set",
        "vendor=claude",
    ]);
    env.ok(&["create", &format!("{child}/helper"), "--name", "plain"]);
    for id in [
        "worker-stale",
        "worker-inherited",
        "worker-masked",
        "helper-plain",
    ] {
        env.ok(&["stop", &format!("{child}/{id}")]);
    }
    let template = fs::read_to_string(env.path("templates/child/template.yaml")).unwrap();
    env.write(
        "templates/child/template.yaml",
        &template.replace(
            "prompt: worker.md, parent:",
            "prompt: worker.md, args: [--verbose], parent:",
        ),
    );
    let before = fs::read(env.path(&format!("state/{child}/team.json"))).unwrap();
    let worktrees = git(&env.path("work/repo"), &["worktree", "list", "--porcelain"]);
    let error = env.fails(&["create", &format!("{child}/worker"), "--name", "refused"]);
    assert!(error.contains("sets `args`"), "{error}");
    assert_eq!(
        fs::read(env.path(&format!("state/{child}/team.json"))).unwrap(),
        before
    );
    assert_eq!(
        git(&env.path("work/repo"), &["worktree", "list", "--porcelain"]),
        worktrees
    );
    env.ok(&["start", &format!("{child}/helper-plain")]);
    launched(&env, "helper-plain", 2, "claude", "opus", "low");
    env.ok(&["create", &format!("{child}/helper"), "--name", "fresh"]);
    launched(&env, "helper-fresh", 1, "claude", "opus", "low");
    env.ok(&["start", &format!("{child}/worker-masked")]);
    launched(&env, "worker-masked", 2, "claude", "opus", "low");
    env.ok(&["start", &child]);
    let error = env.fails(&["start", &format!("{child}/worker-inherited")]);
    assert!(
        error.contains(&format!("`{child}` was created with `worker.vendor=codex`")),
        "{error}"
    );
    assert!(error.contains(&format!("brg delete {child}")), "{error}");
    let address = format!("{child}/worker-stale");
    let error = env.fails(&["start", &address]);
    assert!(
        error.contains(&format!("`{address}` was created with `vendor=codex`")),
        "{error}"
    );
    assert!(error.contains(&format!("brg delete {address}")), "{error}");
    assert!(error.contains("Nothing was started"), "{error}");
    assert!(!error.contains("keep its vendor"), "{error}");
    assert_eq!(env.starts("worker-stale").len(), 1);
    println!("stale member start: {error}");
    env.ok(&["delete", &format!("{child}/worker-inherited")]);
    let template = fs::read_to_string(env.path("templates/child/template.yaml")).unwrap();
    env.write(
        "templates/child/template.yaml",
        &template.replace("start: false", "start: true"),
    );
    let error = env.fails(&["start", &child]);
    assert!(error.contains(&format!("brg delete {address}")), "{error}");
    env.ok(&["stop", &parent]);
}

#[test]
fn stale_team_choice_refuses_full_start_before_issuing_or_adding_worktrees() {
    let (env, parent) = setup();
    let child = child(&env, &parent, &["--set", "worker.vendor=codex"]);
    let template = fs::read_to_string(env.path("templates/child/template.yaml")).unwrap();
    env.write(
        "templates/child/template.yaml",
        &template
            .replace(
                "lead: {prompt: lead.md}",
                "lead: {prompt: lead.md, worktree: true}",
            )
            .replace(
                "prompt: worker.md,",
                "prompt: worker.md, args: [--verbose],",
            )
            .replace("start: false", "start: true"),
    );
    let real_git = common::command("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let real_git = String::from_utf8(real_git.stdout).unwrap();
    common::write_executable(
        &env.path("bin/git"),
        &format!(
            "#!/bin/sh\nfor arg do\n  if test \"$arg\" = add; then echo add >> '{}'; fi\ndone\nexec '{}' \"$@\"\n",
            env.path("worktree-adds").display(),
            real_git.trim()
        ),
    );
    let before = fs::read(env.path(&format!("state/{child}/team.json"))).unwrap();
    let worktrees = git(&env.path("work/repo"), &["worktree", "list", "--porcelain"]);
    let error = env.fails(&["start", &child]);
    assert!(error.contains("sets `args`"), "{error}");
    assert!(
        !env.path("worktree-adds").exists(),
        "a worktree was added before refusal"
    );
    assert_eq!(
        fs::read(env.path(&format!("state/{child}/team.json"))).unwrap(),
        before
    );
    assert_eq!(
        git(&env.path("work/repo"), &["worktree", "list", "--porcelain"]),
        worktrees
    );
    assert!(
        error.contains(&format!("`{child}` was created with `worker.vendor=codex`")),
        "{error}"
    );
    assert!(error.contains(&format!("brg delete {child}")), "{error}");
    assert!(error.contains("Nothing was started"), "{error}");
    println!("stale team start: {error}");
    env.ok(&["stop", &parent]);
}

#[test]
fn stale_choices_keep_status_rows_and_recorded_running_vendor() {
    let (env, parent) = setup();
    let child = child(&env, &parent, &["--set", "worker.vendor=codex"]);
    env.ok(&["start", &child]);
    env.ok(&[
        "create",
        &format!("{child}/worker"),
        "--name",
        "stale",
        "--set",
        "vendor=codex",
    ]);
    env.wait_for("worker idle", || env.state_of(&child, "worker") == "idle");
    let template = fs::read_to_string(env.path("templates/child/template.yaml")).unwrap();
    env.write(
        "templates/child/template.yaml",
        &template.replace(
            "prompt: worker.md,",
            "prompt: worker.md, args: [--verbose],",
        ),
    );
    let row = env.row(&child, "worker");
    assert_eq!(row["member"], "worker-stale");
    assert_eq!(row["vendor"], "codex");
    assert_eq!(row["model"], serde_json::Value::Null);
    assert_eq!(row["effort"], serde_json::Value::Null);
    assert_eq!(row["overrides"], json!({"vendor": "codex"}));
    assert_eq!(row["state"], "idle");
    assert!(
        row["detail"]
            .as_str()
            .unwrap()
            .contains(&format!("brg delete {child}/worker-stale")),
        "{row}"
    );
    assert!(
        env.rows(&parent)
            .iter()
            .any(|r| r["team"] == child && r["member"] == "worker-stale" && r["state"] == "idle")
    );
    let text = stdout(&env.ok(&["status", &parent]));
    assert!(text.contains("worker-stale"), "{text}");
    assert!(
        text.contains(&format!("brg delete {child}/worker-stale")),
        "{text}"
    );
    println!("stale running member status: {row}");
    env.ok(&["stop", &format!("{child}/worker-stale")]);
    env.ok(&["delete", &format!("{child}/worker-stale")]);
    let row = env.row(&child, "worker");
    assert_eq!(row["kind"], "role");
    assert_eq!(row["vendor"], serde_json::Value::Null);
    assert_eq!(row["model"], serde_json::Value::Null);
    assert_eq!(row["effort"], serde_json::Value::Null);
    assert_eq!(row["overrides"], json!({"vendor": "codex"}));
    assert!(
        row["detail"]
            .as_str()
            .unwrap()
            .contains(&format!("brg delete {child}")),
        "{row}"
    );
    assert!(
        env.rows(&parent)
            .iter()
            .any(|r| r["team"] == child && r["kind"] == "role" && r["role"] == "worker")
    );
    env.ok(&["stop", &parent]);
}

#[test]
fn invalid_child_choices_do_not_bypass_caller_authority() {
    let (env, parent) = setup();
    let child = child(&env, &parent, &[]);
    env.ok(&["start", &child]);
    let member = env.member(&child, "lead");
    env.wait_for("child lead launch", || !env.starts(&member).is_empty());
    let starts = env.starts(&member);
    let identity = &starts[0]["env"];
    let before = env.registry();
    let out = env
        .cmd(&[
            "create",
            &format!("{parent}/dev"),
            "--set",
            "missing.vendor=codex",
        ])
        .env("BORGCHART_TEAM", &child)
        .env("BORGCHART_MEMBER", &member)
        .env(
            "BORGCHART_INCARNATION",
            identity["BORGCHART_INCARNATION"].as_str().unwrap(),
        )
        .output()
        .unwrap();
    assert!(!out.status.success(), "{}", both(&out));
    let error = both(&out);
    assert!(
        error.contains("outside `") && error.contains("'s subtree"),
        "{error}"
    );
    assert_eq!(env.registry(), before);
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
