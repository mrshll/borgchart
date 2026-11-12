//! Recursive member stops follow recorded parent edges.
#![cfg(feature = "test-support")]

mod common;
mod instance_env;

use instance_env::*;

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
        "BORGCHART_INSTANCE",
        "BORGCHART_MEMBER",
        "BORGCHART_INCARNATION",
    ] {
        cmd.env(key, vars[key].as_str().unwrap());
    }
    cmd
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
    env.tmux(&["kill-server"]);
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
        &std::fs::read(env.path(&format!("state/{inst}/instance.json"))).unwrap(),
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
fn recursive_failure_advice_does_not_expand_the_stop_to_the_instance() {
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
