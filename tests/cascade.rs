//! Persisted cascades fence new work and restore only the copies that ran.
#![cfg(feature = "test-support")]

mod common;
mod instance_env;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use instance_env::*;

fn department() -> Env {
    let env = Env::new();
    env.write(
        "teams/oncall/team.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md, cwd: lead}\n  dev: {team: teams/dev, parent: lead, start: false, max: 4, cwd: 'dev/{member_id}'}\n",
    );
    env.team("teams/oncall/teams/dev");
    env.write(
        "teams/oncall/teams/dev/team.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md, cwd: repo}\n  helper: {prompt: helper.md, parent: lead, start: false, cwd: repo}\n  scout: {prompt: scout.md, start: false, cwd: repo}\n  design: {team: teams/design, parent: lead, start: false, max: 2, cwd: 'design/{member_id}'}\n",
    );
    env.team("teams/oncall/teams/dev/teams/design");
    env.write(
        "teams/oncall/teams/dev/teams/design/team.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md, cwd: work}\n  helper: {prompt: helper.md, parent: lead, start: false, cwd: work}\n",
    );
    fs::create_dir(env.path("work/a/lead")).unwrap();
    env
}

fn prepared_copy(env: &Env, parent: &str, role: &str, cwd: &str) -> (String, PathBuf) {
    let out = env.ok(&["create", &format!("{parent}/{role}")]);
    let id = stdout(&out).split_whitespace().next().unwrap().to_string();
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(env.path(&format!("state/{id}/instance.json"))).unwrap())
            .unwrap();
    let folder = PathBuf::from(record["folder"].as_str().unwrap());
    fs::create_dir(folder.join(cwd)).unwrap();
    (id, folder)
}

fn start(env: &Env, instance: &str) {
    env.ok(&["start", instance]);
    env.wait_for("lead idle", || env.state_of(instance, "lead") == "idle");
}

fn own_row(env: &Env, instance: &str, role: &str) -> serde_json::Value {
    env.rows(instance)
        .into_iter()
        .find(|row| row["instance"] == instance && row["role"] == role && row["kind"] != "copy")
        .unwrap()
}

fn child_lead(env: &Env, instance: &str, args: &[&str]) -> Command {
    let lead = env.member(instance, "lead");
    env.wait_for("child launch recorded", || !env.starts(&lead).is_empty());
    let starts = env.starts(&lead);
    let vars = &starts.last().unwrap()["env"];
    let mut cmd = env.cmd(args);
    for name in [
        "BORGCHART_INSTANCE",
        "BORGCHART_MEMBER",
        "BORGCHART_INCARNATION",
    ] {
        cmd.env(name, vars[name].as_str().unwrap());
    }
    cmd
}

#[test]
fn declaring_team_roles_fences_first_copy_creation_during_stop() {
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
fn stopping_department_refuses_child_create_and_start() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (dev, _) = prepared_copy(&env, &dept, "dev", "repo");
    start(&env, &dev);
    env.ok(&["create", &format!("{dev}/helper")]);
    let helper = format!("{dev}/{}", env.member(&dev, "helper"));
    env.ok(&["stop", &helper]);
    let (panel, _) = prepared_copy(&env, &dev, "design", "work");
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
    assert!(env.rows(&dept).iter().any(|row| {
        row["instance"] == dev && row["kind"] == "copy" && row["state"] == "stopping"
    }));
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
    let (dev, folder) = prepared_copy(&env, &dept, "dev", "repo");
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
    fs::create_dir(folder.join("repo/lead")).unwrap();
    let refused = env.fails(&[
        "create",
        "oncall",
        &folder.join("repo").display().to_string(),
    ]);
    assert!(refused.contains(&dept), "{refused}");
    env.ok(&["stop", &dept]);
    start(&env, &dept);
    env.wait_for("copy restored after interrupted stop", || {
        env.state_of(&dev, "lead") == "idle"
    });
    env.ok(&["stop", &dept]);
    env.tmux(&["kill-session", "-t", "=sentinel"]);
}

#[test]
fn unknown_liveness_keeps_cascade_stopping_until_stop_can_confirm_it() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (dev, folder) = prepared_copy(&env, &dept, "dev", "repo");
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
    fs::create_dir(folder.join("repo/lead")).unwrap();
    let refused = env.fails(&[
        "create",
        "oncall",
        &folder.join("repo").display().to_string(),
    ]);
    assert!(refused.contains(&dept), "{refused}");
    env.fails(&["delete", &dev, "--force"]);
    fs::write(incarnation, original).unwrap();
    env.ok(&["stop", &dept]);
    start(&env, &dept);
    env.wait_for("copy restored after liveness recovered", || {
        env.state_of(&dev, "lead") == "idle"
    });
    env.ok(&["stop", &dept]);
    env.tmux(&["kill-session", "-t", "=sentinel"]);
}

#[test]
fn unreachable_server_with_exited_processes_clears_stop_marks() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (dev, _) = prepared_copy(&env, &dept, "dev", "repo");
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
    env.wait_for("ordinary instance marked stopping", || {
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
        let (dev, _) = prepared_copy(&env, &dept, "dev", "repo");
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
fn full_start_does_not_revive_independently_stopped_copy_or_descendants() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (dev, _) = prepared_copy(&env, &dept, "dev", "repo");
    start(&env, &dev);
    let (panel, _) = prepared_copy(&env, &dev, "design", "work");
    start(&env, &panel);
    let (peer, _) = prepared_copy(&env, &dept, "dev", "repo");
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
fn full_start_skips_running_copy_validation_but_checks_stopped_restores() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (dev, folder) = prepared_copy(&env, &dept, "dev", "repo");
    start(&env, &dev);
    env.ok(&["stop", &dept]);
    start(&env, &dept);
    env.wait_for("copy restored", || env.state_of(&dev, "lead") == "idle");
    let lead = env.member(&dev, "lead");
    let before = env.starts(&lead).len();
    let path = env.path("teams/oncall/teams/dev/lead.md");
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
    // copies still go through their normal folder and configuration checks.
    env.ok(&["stop", &dept]);
    fs::remove_dir(folder.join("repo")).unwrap();
    let refused = env.fails(&["start", &dept]);
    assert!(refused.contains(&dev), "{refused}");
    assert_eq!(env.state_of(&dept, "lead"), "idle");
    env.ok(&["stop", &dept]);
}

#[test]
fn unreadable_copy_does_not_hide_status_or_block_stopping_readable_instances() {
    for damage in ["record", "missing-record", "state"] {
        let env = department();
        let dept = env.create("oncall", "work/a");
        let (dev, _) = prepared_copy(&env, &dept, "dev", "repo");
        start(&env, &dev);
        let (peer, _) = prepared_copy(&env, &dept, "dev", "repo");
        start(&env, &peer);
        let (panel, _) = prepared_copy(&env, &dev, "design", "work");
        let path = match damage {
            "record" | "missing-record" => env.path(&format!("state/{panel}/instance.json")),
            "state" => env.path(&format!("state/{panel}")),
            _ => unreachable!(),
        };
        let moved = env.path("moved-copy-data");
        fs::rename(&path, &moved).unwrap();
        if damage == "record" {
            fs::write(&path, "unreadable instance record").unwrap();
        }
        for args in [vec!["status", &dept, "--json"], vec!["status", "--json"]] {
            let out = env.ok(&args);
            let rows: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
            assert!(
                rows.iter()
                    .any(|r| r["instance"] == dept && r["kind"] == "member")
            );
            assert!(
                rows.iter()
                    .any(|r| r["instance"] == dev && r["kind"] == "member")
            );
            let row = rows.iter().find(|r| r["instance"] == panel).unwrap();
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
fn failed_copy_delete_preserves_registered_state() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (dev, _) = prepared_copy(&env, &dept, "dev", "repo");
    env.ok(&["stop", &dept]);
    let parent = env.path(&format!("state/{dept}/instance.json"));
    let original = fs::read(&parent).unwrap();
    fs::write(&parent, "unreadable parent record").unwrap();
    env.fails(&["delete", &dev, "--force"]);
    assert!(!env.registry()["instances"][&dev].is_null());
    assert!(env.path(&format!("state/{dev}/instance.json")).is_file());
    fs::write(parent, original).unwrap();
    env.ok(&["delete", &dev, "--force"]);
    assert!(env.registry()["instances"][&dev].is_null());
    assert!(!env.path(&format!("state/{dev}")).exists());
}

#[test]
fn full_start_restores_only_previously_running_copies_at_depth_three() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (dev, _) = prepared_copy(&env, &dept, "dev", "repo");
    let (stopped, _) = prepared_copy(&env, &dept, "dev", "repo");
    let (never_started, _) = prepared_copy(&env, &dept, "dev", "repo");
    for instance in [&dev, &stopped] {
        start(&env, instance);
    }
    env.ok(&["stop", &stopped]);
    env.ok(&["create", &format!("{dev}/helper")]);
    env.ok(&["create", &format!("{dev}/scout")]);
    let scout = format!("{dev}/{}", env.member(&dev, "scout"));
    env.ok(&["stop", &scout]);
    let (panel, _) = prepared_copy(&env, &dev, "design", "work");
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
fn running_panel_is_restored_without_starting_its_stopped_enclosing_copy() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (dev, _) = prepared_copy(&env, &dept, "dev", "repo");
    start(&env, &dev);
    let (panel, _) = prepared_copy(&env, &dev, "design", "work");
    env.ok(&["stop", &dev]);
    start(&env, &panel);
    env.ok(&["stop", &dept]);
    start(&env, &dept);
    env.wait_for("panel restored through stopped enclosing copy", || {
        env.state_of(&panel, "lead") == "idle"
    });
    assert_eq!(own_row(&env, &dev, "lead")["state"], "stopped");
    env.ok(&["stop", &dept]);
}

#[test]
fn copy_start_failure_does_not_hide_successful_siblings() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (broken, broken_folder) = prepared_copy(&env, &dept, "dev", "repo");
    let (healthy, _) = prepared_copy(&env, &dept, "dev", "repo");
    for instance in [&broken, &healthy] {
        start(&env, instance);
    }
    env.ok(&["stop", &dept]);
    fs::remove_dir(broken_folder.join("repo")).unwrap();
    let out = env.brg(&["start", &dept]);
    assert!(!out.status.success(), "{}", both(&out));
    assert!(both(&out).contains(&broken), "{}", both(&out));
    env.wait_for("healthy copy restored", || {
        env.state_of(&healthy, "lead") == "idle"
    });
    assert_eq!(env.state_of(&dept, "lead"), "idle");
    assert_eq!(env.state_of(&broken, "lead"), "stopped");
    env.ok(&["stop", &dept]);
}

#[test]
fn member_delete_preserves_kept_copies_even_with_force() {
    let env = department();
    let dept = env.create("oncall", "work/a");
    let (dev, folder) = prepared_copy(&env, &dept, "dev", "repo");
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
            assert!(env.path(&format!("state/{dev}/instance.json")).is_file());
            env.ok(&["status", &dev]);
        }
    }
    env.ok(&["delete", &dev]);
    env.ok(&["delete", &lead]);
    env.ok(&["delete", &dept]);
    assert!(folder.is_dir());
}
