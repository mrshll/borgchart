//! Persisted cascades fence new work and restore only the child teams that ran.
#![cfg(feature = "test-support")]

mod common;
mod team_env;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use team_env::*;

fn department() -> Env {
    let env = Env::new();
    env.write(
        "templates/oncall/template.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md}\n  dev: {template: dev, parent: lead, start: false, max: 4, worktree: true}\n",
    );
    env.team("templates/dev");
    env.write(
        "templates/dev/template.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md}\n  helper: {prompt: helper.md, parent: lead, start: false}\n  scout: {prompt: scout.md, start: false}\n  design: {template: design, parent: lead, start: false, max: 2}\n",
    );
    env.team("templates/design");
    env.write(
        "templates/design/template.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md}\n  helper: {prompt: helper.md, parent: lead, start: false}\n",
    );
    fs::remove_dir(env.path("work/a")).unwrap();
    env.checkout("work/a");
    env
}

fn prepared_copy(env: &Env, parent: &str, role: &str) -> (String, PathBuf) {
    let out = env.ok(&["create", &format!("{parent}/{role}")]);
    let id = stdout(&out).split_whitespace().next().unwrap().to_string();
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(env.path(&format!("state/{id}/team.json"))).unwrap())
            .unwrap();
    let folder = PathBuf::from(record["folder"].as_str().unwrap());
    (id, folder)
}

fn start(env: &Env, instance: &str) {
    env.ok(&["start", instance]);
    env.wait_for("lead idle", || env.state_of(instance, "lead") == "idle");
}

fn own_row(env: &Env, instance: &str, role: &str) -> serde_json::Value {
    env.rows(instance)
        .into_iter()
        .find(|row| row["team"] == instance && row["role"] == role && row["kind"] != "team")
        .unwrap()
}

fn child_lead(env: &Env, instance: &str, args: &[&str]) -> Command {
    let lead = env.member(instance, "lead");
    env.wait_for("child launch recorded", || !env.starts(&lead).is_empty());
    let starts = env.starts(&lead);
    let vars = &starts.last().unwrap()["env"];
    let mut cmd = env.cmd(args);
    for name in [
        "BORGCHART_TEAM",
        "BORGCHART_MEMBER",
        "BORGCHART_INCARNATION",
    ] {
        cmd.env(name, vars[name].as_str().unwrap());
    }
    cmd
}

#[test]
fn declaring_team_roles_fences_first_child_team_creation_during_stop() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let gate = env.path("stop-gate");
    let stop = env
        .cmd(&["stop", &dept])
        .env("BORGCHART_TEST_STOP_PAUSE", &gate)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    env.wait_for("empty department marked stopping", || {
        gate.with_extension("waiting").exists()
    });
    let out = child_lead(&env, &dept, &["create", &format!("{dept}/dev")])
        .output()
        .unwrap();
    assert!(!out.status.success(), "{}", both(&out));
    assert!(!env.path("work/a/dev").exists());
    fs::write(gate, "").unwrap();
    let out = stop.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", both(&out));
}

#[test]
fn stopping_parent_team_refuses_child_create_and_start() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (dev, _) = prepared_copy(&env, &dept, "dev");
    start(&env, &dev);
    env.ok(&["create", &format!("{dev}/helper")]);
    let helper = format!("{dev}/{}", env.member(&dev, "helper"));
    env.ok(&["stop", &helper]);
    let (panel, _) = prepared_copy(&env, &dev, "design");
    let gate = env.path("stop-gate");
    let stop = env
        .cmd(&["stop", &dept])
        .env("BORGCHART_TEST_STOP_PAUSE", &gate)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    env.wait_for("cascade marks persisted", || {
        gate.with_extension("waiting").exists()
    });
    assert!(
        env.rows(&dept).iter().any(|row| {
            row["team"] == dev && row["kind"] == "team" && row["state"] == "stopping"
        })
    );
    assert!(stdout(&env.ok(&["status", &dept])).contains("stopping"));
    // An already-finished descendant still inherits its enclosing stop.
    env.ok(&["stop", &panel]);
    env.fails(&["create", &format!("{panel}/helper")]);
    for args in [
        vec!["create", &format!("{dev}/scout")],
        vec!["create", &format!("{dev}/design")],
        vec!["start", &helper],
        vec!["start", &panel],
    ] {
        let out = child_lead(&env, &dev, &args).output().unwrap();
        assert!(!out.status.success(), "{args:?}: {}", both(&out));
    }
    assert_eq!(env.row(&dev, "lead")["liveness"], "alive");
    assert!(env.member(&dev, "scout").is_empty());
    assert_eq!(own_row(&env, &dev, "helper")["liveness"], "gone");
    assert!(env.member(&panel, "lead").is_empty());
    fs::write(gate, "").unwrap();
    let out = stop.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", both(&out));
    assert_eq!(env.state_of(&dept, "lead"), "stopped");
    assert_eq!(env.state_of(&dev, "lead"), "stopped");
}

#[test]
fn interrupted_cascade_holds_folders_and_resumes_before_start() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (dev, folder) = prepared_copy(&env, &dept, "dev");
    start(&env, &dev);
    let gate = env.path("stop-gate");
    let mut stop = env
        .cmd(&["stop", &dept])
        .env("BORGCHART_TEST_STOP_PAUSE", &gate)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    env.wait_for("cascade marks persisted", || {
        gate.with_extension("waiting").exists()
    });
    stop.kill().unwrap();
    stop.wait().unwrap();
    // Simulate panes disappearing after the stop process died, keeping the
    // server reachable so liveness can confirm the members are gone.
    env.tmux(&["new-session", "-d", "-s", "sentinel", "sleep", "60"]);
    for instance in [&dev, &dept] {
        env.tmux(&["kill-session", "-t", &format!("=brg-{instance}")]);
    }
    for instance in [&dept, &dev] {
        let refused = env.fails(&["start", instance]);
        assert!(refused.contains(&format!("brg stop {dept}")), "{refused}");
    }
    fs::create_dir(folder.join("sub")).unwrap();
    let refused = env.fails(&[
        "create",
        "oncall",
        &folder.join("sub").display().to_string(),
    ]);
    assert!(refused.contains(&dept), "{refused}");
    env.ok(&["stop", &dept]);
    start(&env, &dept);
    env.wait_for("child team restored after interrupted stop", || {
        env.state_of(&dev, "lead") == "idle"
    });
    env.ok(&["stop", &dept]);
    env.tmux(&["kill-session", "-t", "=sentinel"]);
}

#[test]
fn unknown_liveness_keeps_cascade_stopping_until_stop_can_confirm_it() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (dev, folder) = prepared_copy(&env, &dept, "dev");
    start(&env, &dev);
    let lead = env.member(&dev, "lead");
    let incarnation = env.path(&format!("state/{dev}/members/{lead}/incarnation.json"));
    let original = fs::read(&incarnation).unwrap();
    fs::write(&incarnation, "unreadable start record").unwrap();
    env.tmux(&["new-session", "-d", "-s", "sentinel", "sleep", "60"]);
    env.fails(&["stop", &dept]);
    for instance in [&dev, &dept] {
        let refused = env.fails(&["start", instance]);
        assert!(refused.contains(&format!("brg stop {dept}")), "{refused}");
    }
    fs::create_dir(folder.join("sub")).unwrap();
    let refused = env.fails(&[
        "create",
        "oncall",
        &folder.join("sub").display().to_string(),
    ]);
    assert!(refused.contains(&dept), "{refused}");
    env.fails(&["delete", &dev, "--force"]);
    fs::write(incarnation, original).unwrap();
    env.ok(&["stop", &dept]);
    start(&env, &dept);
    env.wait_for("child team restored after liveness recovered", || {
        env.state_of(&dev, "lead") == "idle"
    });
    env.ok(&["stop", &dept]);
    env.tmux(&["kill-session", "-t", "=sentinel"]);
}

#[test]
fn unreachable_server_with_exited_processes_clears_stop_marks() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (dev, _) = prepared_copy(&env, &dept, "dev");
    start(&env, &dev);
    env.tmux(&["kill-server"]);
    env.ok(&["stop", &dept]);
    start(&env, &dept);
    start(&env, &dev);
    env.ok(&["stop", &dept]);
}

#[test]
fn ordinary_full_stop_fences_start_until_retried() {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    let gate = env.path("stop-gate");
    let mut stop = env
        .cmd(&["stop", &inst])
        .env("BORGCHART_TEST_STOP_PAUSE", &gate)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    env.wait_for("ordinary team marked stopping", || {
        gate.with_extension("waiting").exists()
    });
    stop.kill().unwrap();
    stop.wait().unwrap();
    env.fails(&["start", &inst]);
    env.ok(&["stop", &inst]);
    start(&env, &inst);
    env.ok(&["stop", &inst]);
}

#[test]
fn killed_server_without_recorded_process_does_not_wedge_ordinary_stop() {
    for missing_pane in [false, true] {
        killed_ordinary_server_can_stop_and_restart(missing_pane);
    }
}

fn killed_ordinary_server_can_stop_and_restart(missing_pane: bool) {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    let lead = env.member(&inst, "lead");
    let path = env.path(&format!("state/{inst}/members/{lead}/incarnation.json"));
    let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    if missing_pane {
        record["pane"] = serde_json::Value::Null;
        fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
    }
    env.tmux(&["kill-server"]);
    if missing_pane {
        // Exercise an absent socket as well as tmux's refused stale one.
        fs::remove_file(recorded_socket(&env, &inst)).unwrap();
    }
    env.ok(&["stop", &inst]);
    start(&env, &inst);
    env.ok(&["stop", &inst]);
}

#[test]
fn unreachable_server_with_surviving_process_refuses_until_it_exits() {
    for nested in [false, true] {
        surviving_process_blocks_stop(nested);
    }
}

fn recorded_socket(env: &Env, inst: &str) -> PathBuf {
    let meta: serde_json::Value =
        serde_json::from_slice(&fs::read(env.path(&format!("state/{inst}/start.json"))).unwrap())
            .unwrap();
    PathBuf::from(meta["socket_path"].as_str().unwrap())
}

fn surviving_process_blocks_stop(nested: bool) {
    let env = if nested { department() } else { Env::new() };
    let dept = env.create("oncall", "work/a");
    let dev = if nested {
        let (dev, _) = prepared_copy(&env, &dept, "dev");
        start(&env, &dev);
        dev
    } else {
        dept.clone()
    };
    let lead = env.member(&dev, "lead");
    let path = env.path(&format!("state/{dev}/members/{lead}/incarnation.json"));
    let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let mut survivor = Command::new("sleep").env_clear().arg("60").spawn().unwrap();
    let pid = survivor.id();
    record["pane"]["pane_pid"] = pid.into();
    fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    env.tmux(&["kill-server"]);
    let out = env.brg(&["stop", &dept]);
    survivor.kill().unwrap();
    survivor.wait().unwrap();
    assert!(!out.status.success(), "{}", both(&out));
    let refused = both(&out);
    assert!(refused.contains(&format!("{dev}/{lead}")), "{refused}");
    assert!(refused.contains(&pid.to_string()), "{refused}");
    for instance in [&dept, &dev] {
        env.fails(&["start", instance]);
        env.fails(&["delete", instance, "--force"]);
    }
    env.ok(&["stop", &dept]);
    start(&env, &dept);
    env.ok(&["stop", &dept]);
}

#[test]
fn full_start_does_not_revive_independently_stopped_child_team_or_descendants() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (dev, _) = prepared_copy(&env, &dept, "dev");
    start(&env, &dev);
    let (panel, _) = prepared_copy(&env, &dev, "design");
    start(&env, &panel);
    let (peer, _) = prepared_copy(&env, &dept, "dev");
    start(&env, &peer);
    env.ok(&["stop", &dept]);
    start(&env, &dept);
    env.wait_for("panel restored", || env.state_of(&panel, "lead") == "idle");
    env.ok(&["stop", &panel]);
    start(&env, &dev);
    start(&env, &dept);
    assert_eq!(own_row(&env, &panel, "lead")["state"], "stopped");
    start(&env, &panel);
    env.ok(&["stop", &dev]);
    start(&env, &dept);
    assert_eq!(own_row(&env, &dev, "lead")["state"], "stopped");
    assert_eq!(own_row(&env, &panel, "lead")["state"], "stopped");
    assert_eq!(own_row(&env, &peer, "lead")["state"], "idle");
    env.ok(&["stop", &dept]);
}

#[test]
fn full_start_skips_running_child_team_validation_but_checks_stopped_restores() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (dev, folder) = prepared_copy(&env, &dept, "dev");
    start(&env, &dev);
    env.ok(&["stop", &dept]);
    start(&env, &dept);
    env.wait_for("child team restored", || {
        env.state_of(&dev, "lead") == "idle"
    });
    let lead = env.member(&dev, "lead");
    let before = env.starts(&lead).len();
    let path = env.path("templates/dev/lead.md");
    let prompt = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        format!("{prompt}\nUpdated fictional instructions.\n"),
    )
    .unwrap();
    env.ok(&["start", &dept]);
    assert_eq!(env.starts(&lead).len(), before);
    assert_eq!(env.state_of(&dev, "lead"), "idle");
    // The restore list stays useful for later full starts, and stopped
    // child teams still go through their normal folder and configuration checks.
    env.ok(&["stop", &dept]);
    fs::remove_dir_all(&folder).unwrap();
    let refused = env.fails(&["start", &dept]);
    assert!(refused.contains(&dev), "{refused}");
    assert_eq!(env.state_of(&dept, "lead"), "idle");
    env.ok(&["stop", &dept]);
}

#[test]
fn unreadable_child_team_does_not_hide_status_or_block_stopping_readable_teams() {
    for damage in ["record", "missing-record", "state"] {
        let env = department();
        let dept = env.create("oncall", "work/a");
        let (dev, _) = prepared_copy(&env, &dept, "dev");
        start(&env, &dev);
        let (peer, _) = prepared_copy(&env, &dept, "dev");
        start(&env, &peer);
        let (panel, _) = prepared_copy(&env, &dev, "design");
        let path = match damage {
            "record" | "missing-record" => env.path(&format!("state/{panel}/team.json")),
            "state" => env.path(&format!("state/{panel}")),
            _ => unreachable!(),
        };
        let moved = env.path("moved-child-team-data");
        fs::rename(&path, &moved).unwrap();
        if damage == "record" {
            fs::write(&path, "unreadable team record").unwrap();
        }
        for args in [vec!["status", &dept, "--json"], vec!["status", "--json"]] {
            let out = env.ok(&args);
            let rows: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
            assert!(
                rows.iter()
                    .any(|r| r["team"] == dept && r["kind"] == "member")
            );
            assert!(
                rows.iter()
                    .any(|r| r["team"] == dev && r["kind"] == "member")
            );
            let row = rows.iter().find(|r| r["team"] == panel).unwrap();
            assert_eq!(row["state"], "unreadable", "{damage}: {rows:?}");
            assert!(!row["detail"].as_str().unwrap().is_empty());
        }
        for args in [vec!["status", &dept], vec!["status"]] {
            let text = stdout(&env.ok(&args));
            assert!(text.contains("unreadable"), "{text}");
            assert!(text.contains(&env.member(&dept, "lead")), "{text}");
        }
        let refused = env.fails(&["stop", &dept]);
        assert!(refused.contains(&panel), "{refused}");
        for instance in [&dept, &dev, &peer] {
            assert_eq!(own_row(&env, instance, "lead")["liveness"], "gone");
        }
        let refused = env.fails(&["delete", &dept, "--force"]);
        assert!(
            refused.contains(&panel) && refused.contains(&peer),
            "{refused}"
        );
        if damage == "record" {
            fs::remove_file(&path).unwrap();
        }
        fs::rename(moved, path).unwrap();
        env.ok(&["stop", &dept]);
    }
}

#[test]
fn failed_child_team_delete_preserves_registered_state() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (dev, _) = prepared_copy(&env, &dept, "dev");
    env.ok(&["stop", &dept]);
    let parent = env.path(&format!("state/{dept}/team.json"));
    let original = fs::read(&parent).unwrap();
    fs::write(&parent, "unreadable parent record").unwrap();
    env.fails(&["delete", &dev, "--force"]);
    assert!(!env.registry()["teams"][&dev].is_null());
    assert!(env.path(&format!("state/{dev}/team.json")).is_file());
    fs::write(parent, original).unwrap();
    env.ok(&["delete", &dev, "--force"]);
    assert!(env.registry()["teams"][&dev].is_null());
    assert!(!env.path(&format!("state/{dev}")).exists());
}

#[test]
fn full_start_restores_only_previously_running_child_teams_at_depth_three() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (dev, _) = prepared_copy(&env, &dept, "dev");
    let (stopped, _) = prepared_copy(&env, &dept, "dev");
    let (never_started, _) = prepared_copy(&env, &dept, "dev");
    for instance in [&dev, &stopped] {
        start(&env, instance);
    }
    env.ok(&["stop", &stopped]);
    env.ok(&["create", &format!("{dev}/helper")]);
    env.ok(&["create", &format!("{dev}/scout")]);
    let scout = format!("{dev}/{}", env.member(&dev, "scout"));
    env.ok(&["stop", &scout]);
    let (panel, _) = prepared_copy(&env, &dev, "design");
    start(&env, &panel);
    env.ok(&["create", &format!("{panel}/helper")]);
    env.ok(&["stop", &dept]);
    env.ok(&["stop", &dept]);
    start(&env, &dept);
    for (instance, role) in [
        (&dev, "lead"),
        (&dev, "helper"),
        (&panel, "lead"),
        (&panel, "helper"),
    ] {
        env.wait_for("previous member restored", || {
            own_row(&env, instance, role)["state"] == "idle"
        });
    }
    assert_eq!(env.state_of(&stopped, "lead"), "stopped");
    assert!(env.member(&never_started, "lead").is_empty());
    assert_eq!(env.state_of(&dev, "scout"), "stopped");
    env.ok(&["stop", &dept]);
}

#[test]
fn running_panel_is_restored_without_starting_its_stopped_enclosing_child_team() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (dev, _) = prepared_copy(&env, &dept, "dev");
    start(&env, &dev);
    let (panel, _) = prepared_copy(&env, &dev, "design");
    env.ok(&["stop", &dev]);
    start(&env, &panel);
    env.ok(&["stop", &dept]);
    start(&env, &dept);
    env.wait_for(
        "panel restored through stopped enclosing child team",
        || env.state_of(&panel, "lead") == "idle",
    );
    assert_eq!(own_row(&env, &dev, "lead")["state"], "stopped");
    env.ok(&["stop", &dept]);
}

#[test]
fn child_team_start_failure_does_not_hide_successful_siblings() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (broken, broken_folder) = prepared_copy(&env, &dept, "dev");
    let (healthy, _) = prepared_copy(&env, &dept, "dev");
    for instance in [&broken, &healthy] {
        start(&env, instance);
    }
    env.ok(&["stop", &dept]);
    fs::remove_dir_all(&broken_folder).unwrap();
    let out = env.brg(&["start", &dept]);
    assert!(!out.status.success(), "{}", both(&out));
    assert!(both(&out).contains(&broken), "{}", both(&out));
    env.wait_for("healthy child team restored", || {
        env.state_of(&healthy, "lead") == "idle"
    });
    assert_eq!(env.state_of(&dept, "lead"), "idle");
    assert_eq!(env.state_of(&broken, "lead"), "stopped");
    env.ok(&["stop", &dept]);
}

#[test]
fn member_delete_preserves_kept_child_teams_even_with_force() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (dev, folder) = prepared_copy(&env, &dept, "dev");
    env.ok(&["stop", &dept]);
    let lead = format!("{dept}/{}", env.member(&dept, "lead"));
    for target in [&dept, &lead] {
        for force in [false, true] {
            let mut args = vec!["delete", target];
            if force {
                args.push("--force");
            }
            let refused = env.fails(&args);
            assert!(refused.contains(&dev), "{refused}");
            assert!(env.path(&format!("state/{dev}/team.json")).is_file());
            env.ok(&["status", &dev]);
        }
    }
    env.ok(&["delete", &dev]);
    env.ok(&["delete", &lead]);
    env.ok(&["delete", &dept]);
    assert!(
        !folder.exists(),
        "delete leaves the child team's clean worktree"
    );
}

#[test]
fn recursive_member_stop_reuses_nested_child_team_cascades() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (dev, _) = prepared_copy(&env, &dept, "dev");
    start(&env, &dev);
    let (panel, _) = prepared_copy(&env, &dev, "design");
    start(&env, &panel);
    let lead = env.member(&dept, "lead");
    let mut stop = env
        .cmd(&["stop", "-r", &format!("{dept}/{lead}")])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while stop.try_wait().unwrap().is_none() {
        if std::time::Instant::now() >= deadline {
            stop.kill().unwrap();
            stop.wait().unwrap();
            panic!("nested recursive stop did not finish; possible registry relock");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let out = stop.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", both(&out));
    let text = stdout(&out);
    assert!(
        text.find(&format!("{panel}: stopped")).unwrap()
            < text.find(&format!("{dev}: stopped")).unwrap(),
        "{text}"
    );
    assert!(
        text.find(&format!("{dev}: stopped")).unwrap()
            < text.find(&format!("{lead}: stopped")).unwrap(),
        "{text}"
    );
    for inst in [&dev, &panel] {
        let record: serde_json::Value = serde_json::from_slice(
            &fs::read(env.path(&format!("state/{inst}/team.json"))).unwrap(),
        )
        .unwrap();
        assert_ne!(record["stopping"], true);
        assert_eq!(env.state_of(inst, "lead"), "stopped");
    }
    env.ok(&["stop", "-r", &format!("{dept}/{lead}")]);
    env.ok(&["stop", "-r", &dept]);
}

#[test]
fn recursive_stop_checks_child_team_sockets_before_stopping_any_member() {
    let env = department();
    let team = env.path("templates/oncall/template.yaml");
    let mut text = fs::read_to_string(&team).unwrap();
    text.push_str("  helper: {prompt: helper.md, parent: lead}\n");
    fs::write(team, text).unwrap();
    let dept = env.create("oncall", "work/a");
    let (dev, _) = prepared_copy(&env, &dept, "dev");
    start(&env, &dev);
    let path = env.path(&format!("state/{dev}/start.json"));
    let original = fs::read(&path).unwrap();
    let mut meta: serde_json::Value = serde_json::from_slice(&original).unwrap();
    meta["socket"] = "another-socket".into();
    fs::write(&path, serde_json::to_vec(&meta).unwrap()).unwrap();
    let error = env.fails(&[
        "stop",
        "-r",
        &format!("{dept}/{}", env.member(&dept, "lead")),
    ]);
    assert!(error.contains("Nothing was changed"), "{error}");
    assert_eq!(env.row(&dept, "lead")["liveness"], "alive");
    assert_eq!(own_row(&env, &dept, "helper")["liveness"], "alive");
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(env.path(&format!("state/{dev}/team.json"))).unwrap())
            .unwrap();
    assert_ne!(record["stopping"], true);
    fs::write(path, original).unwrap();
    env.ok(&["stop", &dept]);
}

#[test]
fn a_failed_member_stop_does_not_fence_a_child_team_the_walk_has_not_reached() {
    let env = department();
    let team = env.path("templates/oncall/template.yaml");
    let mut text = fs::read_to_string(&team).unwrap();
    text.push_str("  helper: {prompt: helper.md, parent: lead}\n");
    fs::write(team, text).unwrap();
    let dept = env.create("oncall", "work/a");
    let (dev, _) = prepared_copy(&env, &dept, "dev");
    start(&env, &dev);
    let helper = own_row(&env, &dept, "helper")["member"]
        .as_str()
        .unwrap()
        .to_string();
    let path = env.path(&format!("state/{dept}/members/{helper}/incarnation.json"));
    let original = fs::read(&path).unwrap();
    fs::write(&path, "broken").unwrap();
    let target = format!("{dept}/{}", env.member(&dept, "lead"));
    env.fails(&["stop", "-r", &target]);
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(env.path(&format!("state/{dev}/team.json"))).unwrap())
            .unwrap();
    assert_ne!(record["stopping"], true, "unreached child team was fenced");
    assert_eq!(env.row(&dev, "lead")["liveness"], "alive");
    fs::write(path, original).unwrap();
    env.ok(&["stop", "-r", &target]);
    env.ok(&["stop", &dept]);
}

#[test]
fn a_full_resume_resumes_restored_child_teams() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (dev, _) = prepared_copy(&env, &dept, "dev");
    start(&env, &dev);
    let lead = env.member(&dev, "lead");
    let session = env.path(&format!("state/{dev}/members/{lead}/session"));
    env.wait_for("child team lead's session recorded", || session.exists());
    let id = fs::read_to_string(&session).unwrap().trim().to_string();
    env.ok(&["stop", &dept]);
    env.ok(&["start", &dept, "--resume"]);
    env.wait_for("child team lead restored", || {
        env.state_of(&dev, "lead") == "idle"
    });
    let starts = env.starts(&lead);
    let argv: Vec<&str> = starts.last().unwrap()["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert!(
        argv.windows(2).any(|w| w == ["--resume", id.as_str()]),
        "{argv:?}"
    );
    env.ok(&["stop", &dept]);
}
