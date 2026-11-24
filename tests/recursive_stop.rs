//! Recursive stops follow recorded parent edges, and full starts restore eligible members.
#![cfg(feature = "test-support")]

mod common;
mod team_env;

use team_env::*;

fn tree() -> (Env, String) {
    let env = Env::new();
    env.write("templates/oncall/template.yaml", "bus: {}\nroles:\n  lead: {prompt: lead.md}\n  helper: {prompt: helper.md, parent: lead}\n  leaf: {prompt: helper.md, parent: helper}\n  scout: {prompt: scout.md, parent: lead}\n");
    let inst = env.create("oncall", "work/a");
    (env, inst)
}

#[test]
fn recursive_stop_stops_children_first_and_keeps_siblings_running() {
    let (env, inst) = tree();
    let helper = env.member(&inst, "helper");
    let leaf = env.member(&inst, "leaf");
    let target = format!("{inst}/{helper}");
    let out = env.ok(&["stop", "-r", &target]);
    let text = stdout(&out);
    assert!(
        text.find(&leaf).unwrap() < text.find(&helper).unwrap(),
        "{text}"
    );
    assert_eq!(env.state_of(&inst, "helper"), "stopped");
    assert_eq!(env.state_of(&inst, "leaf"), "stopped");
    assert_eq!(env.row(&inst, "lead")["liveness"], "alive");
    assert_eq!(env.row(&inst, "scout")["liveness"], "alive");
    env.ok(&["stop", "-r", &target]);
    env.ok(&["stop", &inst]);
}

#[test]
fn ordinary_member_stop_still_leaves_children_running() {
    let (env, inst) = tree();
    env.ok(&["stop", &format!("{inst}/{}", env.member(&inst, "helper"))]);
    assert_eq!(env.row(&inst, "leaf")["liveness"], "alive");
    env.ok(&["stop", &inst]);
}

fn as_member(env: &Env, inst: &str, role: &str, args: &[&str]) -> std::process::Command {
    let id = env.member(inst, role);
    env.wait_for("member started", || !env.starts(&id).is_empty());
    let starts = env.starts(&id);
    let vars = &starts.last().unwrap()["env"];
    let mut cmd = env.cmd(args);
    for key in [
        "BORGCHART_TEAM",
        "BORGCHART_MEMBER",
        "BORGCHART_INCARNATION",
    ] {
        cmd.env(key, vars[key].as_str().unwrap());
    }
    cmd
}

const CLOCK: &str = "
plugins:
  clock:
    process: [sleep, '60']
    delivers_to: lead
    actions: {done: clockctl done, last: clock-history last}
  observer:
    process: [sleep, '60']
    delivers_to: lead
    actions: {done: observerctl done}
  manual:
    actions: {done: manualctl done}
";

fn write_run_team(env: &Env, plugins: &str, roles: &str) {
    let team = TEAM
        .replace("  lead:\n", "  lead:\n    vendor: claude\n")
        .replace(
            "    parent: lead\n",
            "    parent: lead\n    plugins: {clock: [\"*\"]}\n",
        );
    env.write(
        "templates/oncall/template.yaml",
        &format!("default: codex\n{team}{roles}{plugins}"),
    );
}

fn run_team() -> (Env, String) {
    let env = Env::new();
    write_run_team(&env, CLOCK, "");
    let inst = env.create("oncall", "work/a");
    (env, inst)
}

const RUN_CHOICES: &str = "vendor=claude,model=example-review,effort=high";

fn as_plugin(env: &Env, inst: &str, args: &[&str]) -> std::process::Command {
    let mut cmd = env.cmd(args);
    cmd.env("BORGCHART_TEAM", inst)
        .env("BORGCHART_RUN_ROLE", "clock-run");
    cmd
}

fn create_run(env: &Env, inst: &str) -> String {
    let out = as_plugin(
        env,
        inst,
        &[
            "create",
            &format!("{inst}/clock-run"),
            "--name",
            "sample",
            "--run",
            "review",
            "--set",
            RUN_CHOICES,
        ],
    )
    .output()
    .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    env.wait_for("scheduled member started", || {
        !env.starts("clock-run-sample").is_empty()
    });
    "clock-run-sample".into()
}

#[test]
fn built_in_run_launches_under_its_recipient_with_only_its_done_grant() {
    let (env, inst) = run_team();
    let validate = env.ok(&["validate", "oncall"]);
    let text = stdout(&validate);
    assert!(
        text.lines()
            .any(|line| line.contains("clock-run") && line.contains("built in")),
        "{text}"
    );
    let member = create_run(&env, &inst);
    let row = env.row(&inst, "clock-run");
    assert_eq!(row["parent"], env.member(&inst, "lead"));
    assert_eq!(row["vendor"], "claude");
    assert_eq!(row["model"], "example-review");
    assert_eq!(row["effort"], "high");
    let status = stdout(&env.ok(&["status", &inst]));
    assert!(
        status
            .lines()
            .any(|line| line.contains(&member) && line.contains("run: review")),
        "{status}"
    );
    let start = &env.starts(&member)[0];
    let argv = start["argv"].as_array().unwrap();
    assert!(argv.iter().any(|a| a == "--strict-mcp-config"), "{argv:?}");
    let prompt = argv.last().unwrap().as_str().unwrap();
    assert!(prompt.contains("short-lived member") && prompt.contains("external:timer"));
    let path = start["env"]["PATH"].as_str().unwrap();
    let shims = std::env::split_paths(path).next().unwrap();
    assert!(shims.join("clockctl").is_file() && prompt.contains("/clockctl done"));
    for command in ["clock-history", "observerctl"] {
        assert!(!shims.join(command).exists() && !prompt.contains(command));
    }
    env.ok(&["stop", &inst]);
}

#[test]
fn built_in_runs_refuse_creation_outside_the_schedule_and_disallowed_choices() {
    let (env, inst) = run_team();
    let target = format!("{inst}/clock-run");
    assert!(env.fails(&["create", &target]).contains("schedule file"));
    for (grant, choices, reason) in [
        ("observer-run", RUN_CHOICES, "cannot schedule `clock-run`"),
        ("clock-run", "vendor=claude", "both `--set vendor="),
        (
            "clock-run",
            "vendor=claude,model=example-review,remote_control=true",
            "remote_control",
        ),
    ] {
        let out = as_plugin(
            &env,
            &inst,
            &["create", &target, "--run", "review", "--set", choices],
        )
        .env("BORGCHART_RUN_ROLE", grant)
        .output()
        .unwrap();
        assert!(
            !out.status.success() && both(&out).contains(reason),
            "{}",
            both(&out)
        );
    }
    env.ok(&["stop", &inst]);
}

#[test]
fn stale_run_role_env_cannot_create_a_declared_role_with_the_same_name() {
    let (env, inst) = run_team();
    write_run_team(
        &env,
        &CLOCK.replace("done: clockctl done, ", ""),
        "  clock-run: {prompt: lead.md, parent: lead, start: false}\n",
    );
    let out = as_plugin(
        &env,
        &inst,
        &[
            "create",
            &format!("{inst}/clock-run"),
            "--run",
            "review",
            "--set",
            RUN_CHOICES,
        ],
    )
    .output()
    .unwrap();
    assert!(
        !out.status.success() && both(&out).contains("no longer a built-in run role"),
        "{}",
        both(&out)
    );
    env.ok(&["stop", &inst]);
}

#[test]
fn a_plugin_can_stop_and_delete_a_run_after_its_base_role_is_removed() {
    let (env, inst) = run_team();
    let member = create_run(&env, &inst);
    let target = format!("{inst}/{member}");
    write_run_team(&env, &CLOCK.replace("done: clockctl done, ", ""), "");
    for args in [vec!["stop", &target], vec!["delete", &target, "--force"]] {
        let out = as_plugin(&env, &inst, &args).output().unwrap();
        assert!(out.status.success(), "{}", both(&out));
    }
    assert!(env.fails(&["start", &target]).contains("deleted"));
    env.ok(&["stop", &inst]);
}

#[test]
fn full_start_skips_run_members_and_status_distinguishes_them() {
    let (env, inst) = run_team();
    let out = env
        .cmd(&["create", &format!("{inst}/helper"), "--run", "review"])
        .env("BORGCHART_TEAM", &inst)
        .env("BORGCHART_RUNS", "helper")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    let helper = env.member(&inst, "helper");
    env.wait_for("run member started", || !env.starts(&helper).is_empty());
    let lead = env.row(&inst, "lead");
    let lead_id = lead["member"].as_str().unwrap();
    let status = stdout(&env.ok(&["status", &inst]));
    let line = |id: &str| status.lines().find(|line| line.contains(id)).unwrap();
    assert_eq!(
        (
            env.row(&inst, "helper")["run"].as_str(),
            line(&helper).contains("run: review"),
        ),
        (Some("review"), true),
        "{status}"
    );
    assert!(lead.get("run").is_none());
    assert!(!line(lead_id).contains("run:"), "{status}");
    let out = env.ok(&["start", &inst]);
    let text = stdout(&out);
    assert!(
        text.lines()
            .any(|line| line.contains(&helper) && line.ends_with("already running")),
        "{text}"
    );
    assert_eq!(env.starts(&helper).len(), 1);
    env.ok(&["create", &format!("{inst}/scout")]);
    env.ok(&["stop", &inst]);
    env.ok(&["start", &inst]);
    assert_eq!(env.state_of(&inst, "helper"), "stopped");
    assert_eq!(env.row(&inst, "scout")["liveness"], "alive");
    env.ok(&["start", &format!("{inst}/{helper}")]);
    assert_eq!(env.row(&inst, "helper")["liveness"], "alive");
    env.ok(&["stop", &inst]);
}

#[test]
fn run_marks_require_a_plugin_with_a_current_role_grant() {
    let (env, inst) = run_team();
    write_run_team(
        &env,
        &CLOCK.replacen("delivers_to: lead", "delivers_to: recipient", 1),
        "  reader: {prompt: lead.md, start: false, plugins: {manual: [done]}}\n  recipient: {prompt: lead.md, start: false, plugins: {clock: [done]}}\n",
    );
    let other = env.create("oncall", "work/b");
    let args = ["create", &format!("{inst}/helper"), "--run", "review"];
    let mut member = as_member(&env, &inst, "lead", &args);
    member.env("BORGCHART_RUNS", "helper");
    let plugin = |target: &str, role: &str, grant: &str| {
        let mut cmd = env.cmd(&["create", &format!("{target}/{role}"), "--run", "review"]);
        cmd.env("BORGCHART_TEAM", &inst)
            .env("BORGCHART_RUNS", grant);
        cmd
    };
    for (caller, mut cmd) in [
        ("owner", env.cmd(&args)),
        ("member", member),
        ("different launched grant", plugin(&inst, "helper", "scout")),
        ("different team", plugin(&other, "helper", "helper")),
    ] {
        assert!(
            !cmd.output().unwrap().status.success(),
            "{caller} marked a run"
        );
    }
    for (role, condition) in [
        ("scout", "`done`"),
        ("reader", "`process`"),
        ("recipient", "`clock-run` (built in for plugin `clock`)"),
    ] {
        let out = plugin(&inst, role, role).output().unwrap();
        assert!(
            !out.status.success() && both(&out).contains(condition),
            "{}",
            both(&out)
        );
    }
    env.ok(&["stop", &other]);
    env.ok(&["stop", &inst]);
}

#[test]
fn recursive_authority_accepts_descendants_and_refuses_self_siblings_and_ancestors() {
    let (env, inst) = tree();
    for role in ["helper", "lead", "scout"] {
        let out = as_member(
            &env,
            &inst,
            "helper",
            &["stop", "-r", &format!("{inst}/{}", env.member(&inst, role))],
        )
        .output()
        .unwrap();
        assert!(!out.status.success(), "{}", both(&out));
        assert!(both(&out).contains("outside"), "{}", both(&out));
    }
    let out = as_member(
        &env,
        &inst,
        "lead",
        &[
            "stop",
            "-r",
            &format!("{inst}/{}", env.member(&inst, "helper")),
        ],
    )
    .output()
    .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    assert_eq!(env.state_of(&inst, "leaf"), "stopped");
    assert_eq!(env.row(&inst, "scout")["liveness"], "alive");
    env.ok(&["stop", &inst]);
}

#[test]
fn failure_leaves_ancestors_running_and_a_retry_finishes() {
    let (env, inst) = tree();
    let leaf = env.member(&inst, "leaf");
    let path = env.path(&format!("state/{inst}/members/{leaf}/incarnation.json"));
    let original = std::fs::read(&path).unwrap();
    std::fs::write(&path, "broken").unwrap();
    let target = format!("{inst}/{}", env.member(&inst, "helper"));
    let error = env.fails(&["stop", "-r", &target]);
    assert!(error.contains(&format!("brg stop -r {target}")), "{error}");
    assert_eq!(env.row(&inst, "helper")["liveness"], "alive");
    assert_eq!(env.row(&inst, "scout")["liveness"], "alive");
    std::fs::write(path, original).unwrap();
    env.ok(&["stop", "-r", &target]);
    assert_eq!(env.state_of(&inst, "helper"), "stopped");
    env.ok(&["stop", &inst]);
}

#[test]
fn recursive_stop_in_the_same_boot_without_tmux_passes_members_with_no_live_start() {
    let (env, inst) = tree();
    let helper = env.member(&inst, "helper");
    let leaf = env.member(&inst, "leaf");
    let target = format!("{inst}/{helper}");
    env.ok(&["stop", "-r", &target]);
    // The helper's start has ended; the leaf never started, as when its
    // first start failed before recording it.
    std::fs::remove_file(env.path(&format!("state/{inst}/members/{leaf}/incarnation.json")))
        .unwrap();
    let meta_file = env.path(&format!("state/{inst}/start.json"));
    let mut meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&meta_file).unwrap()).unwrap();
    meta["members"][&leaf]["started"] = false.into();
    std::fs::write(&meta_file, serde_json::to_vec(&meta).unwrap()).unwrap();
    env.kill_server();
    // The walk requires each stopped member to read gone.
    env.ok(&["stop", "-r", &target]);
}

#[test]
fn recursive_stop_removes_every_target_from_the_restore_snapshot() {
    let (env, inst) = tree();
    env.ok(&["stop", &inst]);
    let helper = env.member(&inst, "helper");
    let leaf = env.member(&inst, "leaf");
    env.ok(&["stop", "-r", &format!("{inst}/{helper}")]);
    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(env.path(&format!("state/{inst}/team.json"))).unwrap(),
    )
    .unwrap();
    let restore = record["running_at_stop"].as_array().unwrap();
    assert!(!restore.contains(&serde_json::json!(helper)));
    assert!(!restore.contains(&serde_json::json!(leaf)));
    assert!(restore.contains(&serde_json::json!(env.member(&inst, "scout"))));
}

#[test]
fn recursive_walk_uses_recorded_parents_after_team_file_changes() {
    let (env, inst) = tree();
    let team = env.path("templates/oncall/template.yaml");
    let changed = std::fs::read_to_string(&team)
        .unwrap()
        .replace("parent: helper", "parent: lead");
    std::fs::write(team, changed).unwrap();
    env.ok(&[
        "stop",
        "-r",
        &format!("{inst}/{}", env.member(&inst, "helper")),
    ]);
    assert_eq!(env.state_of(&inst, "leaf"), "stopped");
    env.ok(&["stop", &inst]);
}

#[test]
fn a_member_create_waits_for_the_walk_and_is_then_fenced() {
    use std::process::Stdio;
    let (env, inst) = tree();
    let gate = env.path("stop-gate");
    let target = format!("{inst}/{}", env.member(&inst, "helper"));
    let mut create = as_member(&env, &inst, "helper", &["create", &format!("{inst}/leaf")]);
    let stop = env
        .cmd(&["stop", "-r", &target])
        .env("BORGCHART_TEST_STOP_PAUSE", &gate)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    env.wait_for("recursive walk holds registry lock", || {
        gate.with_extension("waiting").exists()
    });
    let mut create = create
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(create.try_wait().unwrap().is_none());
    std::fs::write(gate, "").unwrap();
    let out = stop.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", both(&out));
    let out = create.wait_with_output().unwrap();
    assert!(!out.status.success(), "{}", both(&out));
    assert!(both(&out).contains("stopped"), "{}", both(&out));
    env.ok(&["stop", &inst]);
}

#[test]
fn an_unconfirmed_child_stop_keeps_its_ancestors_running() {
    let (env, inst) = tree();
    let leaf = env.member(&inst, "leaf");
    let helper = env.member(&inst, "helper");
    let rename = |from: &str, to: &str| {
        let out = env
            .tmux
            .tmux(&env.socket)
            .args(["rename-window", "-t", &format!("=brg-{inst}:={from}"), to])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", both(&out));
    };
    rename(&leaf, "moved-leaf");
    let target = format!("{inst}/{helper}");
    let out = env.brg(&["stop", "-r", &target]);
    assert!(!out.status.success(), "{}", both(&out));
    assert!(both(&out).contains("unconfirmed"), "{}", both(&out));
    assert_eq!(env.row(&inst, "helper")["liveness"], "alive");
    rename("moved-leaf", &leaf);
    env.ok(&["stop", "-r", &target]);
    env.ok(&["stop", &inst]);
}

#[test]
fn recursive_failure_advice_does_not_expand_the_stop_to_the_team() {
    let (env, inst) = tree();
    let target = format!("{inst}/{}", env.member(&inst, "helper"));
    let out = env
        .cmd(&["stop", "-r", &target])
        .env("PATH", env.path("bin"))
        .output()
        .unwrap();
    assert!(!out.status.success(), "{}", both(&out));
    let error = both(&out);
    assert!(error.contains(&format!("brg stop -r {target}")), "{error}");
    assert!(
        !error.contains(&format!("then retry: brg stop {inst}")),
        "{error}"
    );
    env.ok(&["stop", &inst]);
}
