//! Instances and the four verbs: `create`, `start`, `stop`, `delete`, with
//! `ls`, `status` and addressing. Teams are found by name in the search
//! paths of a test configuration (`BORGCHART_CONFIG`) or by path. Real tmux
//! on a private per-test socket, with `fake-agent` standing in for `claude`
//! and `codex`.
//!
//! Run with `cargo test --features test-support`.
#![cfg(feature = "test-support")]

mod common;

mod instance_env;

use std::fs;
use std::process::Command;
use std::time::Duration;

use instance_env::*;
use serde_json::{Value, json};

#[test]
fn renamed_teams_keep_instance_identity_messages_and_use_the_new_source() {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    let lead = env.member(&inst, "lead");
    let sent = env.ok(&[
        "send",
        &format!("{inst}/{lead}"),
        "Keep the draft",
        "--external",
        "ops",
        "--json",
    ]);
    let message: Value = serde_json::from_slice(&sent.stdout).unwrap();
    env.ok(&["stop", &inst]);
    let issued = env.registry()["issued"].clone();
    fs::rename(env.path("teams/oncall"), env.path("teams/pager")).unwrap();
    env.write(
        "teams/pager/lead.md",
        "---\nmodels: {claude: opus high}\n---\nUse the violet checklist.\n",
    );
    let out = env.ok(&[
        "start",
        &inst,
        "--team",
        &env.path("teams/pager").display().to_string(),
    ]);
    println!("rename start: {}", both(&out));
    env.wait_for("restarted lead", || env.starts(&lead).len() == 2);
    assert!(
        env.starts(&lead)[1]["argv"]
            .to_string()
            .contains("violet checklist")
    );
    assert_eq!(env.member(&inst, "lead"), lead);
    assert!(
        env.rows(&inst)
            .iter()
            .all(|r| r["instance"] == inst && r["team"] == "pager")
    );
    let status = stdout(&env.ok(&["status", &inst]));
    assert!(
        status.contains(&inst) && status.contains("pager"),
        "{status}"
    );
    assert_eq!(env.registry()["issued"], issued);
    let hint = env.fails(&["status", "pager"]);
    assert!(hint.contains("is a team") && hint.contains(&inst), "{hint}");
    env.ok(&[
        "msg",
        "read",
        message["id"].as_str().unwrap(),
        "--instance",
        &inst,
    ]);
    env.ok(&["stop", &inst]);
}

#[test]
fn repoint_refuses_every_kept_agent_whose_role_is_missing() {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    for role in ["helper", "scout"] {
        env.ok(&["create", &format!("{inst}/{role}")]);
    }
    let helper = env.member(&inst, "helper");
    let scout = env.member(&inst, "scout");
    env.ok(&["stop", &inst]);
    env.team("moved/oncall");
    env.write(
        "moved/oncall/team.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md}\n",
    );
    let moved = env.path("moved/oncall").display().to_string();
    let refusal = env.fails(&["start", &inst, "--team", &moved]);
    println!("missing agents: {refusal}");
    for member in [&helper, &scout] {
        assert!(refusal.contains(member), "{refusal}");
    }
    // Refusal must leave the original source usable.
    env.ok(&["start", &inst]);
    env.wait_for("helper restarted", || env.starts(&helper).len() == 2);
    env.ok(&["stop", &inst]);
    for member in [&helper, &scout] {
        env.ok(&["delete", &format!("{inst}/{member}")]);
    }
    // Deleted roles impose no compatibility constraint.
    env.ok(&["start", &inst, "--team", &moved]);
    env.ok(&["stop", &inst]);
}

#[test]
fn invalid_repoint_leaves_the_original_team_usable() {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    env.ok(&["stop", &inst]);
    env.team("moved/oncall");
    env.write("moved/oncall/team.yaml", "roles: [");
    env.fails(&[
        "start",
        &inst,
        "--team",
        &env.path("moved/oncall").display().to_string(),
    ]);
    env.ok(&["start", &inst]);
    env.ok(&["stop", &inst]);
}

#[test]
fn repoint_refuses_a_dropped_role_name_parent_before_changing_source() {
    let env = Env::new();
    env.write(
        "teams/oncall/team.yaml",
        &TEAM
            .replace("prompt: lead.md", "prompt: lead.md\n    start: false")
            .replace("    start: false\n  scout:", "    start: true\n  scout:"),
    );
    let out = env.ok(&[
        "create",
        "oncall",
        &env.path("work/a").display().to_string(),
    ]);
    let inst = stdout(&out).split_whitespace().next().unwrap().to_string();
    env.wait_for("child idle", || env.state_of(&inst, "helper") == "idle");
    let helper = env.member(&inst, "helper");
    env.ok(&["stop", &inst]);
    let path = env.path(&format!("state/{inst}/instance.json"));
    let original = fs::read(&path).unwrap();
    env.team("moved/oncall");
    env.write(
        "moved/oncall/team.yaml",
        "bus: {}\nroles:\n  helper: {prompt: helper.md, vendor: codex}\n  scout: {prompt: scout.md, start: false}\n",
    );
    let refusal = env.fails(&[
        "start",
        &inst,
        "--team",
        &env.path("moved/oncall").display().to_string(),
    ]);
    assert!(
        refusal.contains(&helper) && refusal.contains(&format!("{inst}/lead")),
        "{refusal}"
    );
    assert_eq!(fs::read(path).unwrap(), original);
    env.ok(&["start", &inst]);
    env.ok(&["stop", &inst]);
}

#[test]
fn starting_repairs_an_unresolved_parent_so_recursive_stop_reaches_the_child() {
    let env = Env::new();
    env.write(
        "teams/oncall/team.yaml",
        &TEAM
            .replace("prompt: lead.md", "prompt: lead.md\n    start: false")
            .replace("    start: false\n  scout:", "    start: true\n  scout:"),
    );
    let out = env.ok(&[
        "create",
        "oncall",
        &env.path("work/a").display().to_string(),
    ]);
    let inst = stdout(&out).split_whitespace().next().unwrap().to_string();
    env.wait_for("child idle", || env.state_of(&inst, "helper") == "idle");
    env.ok(&["create", &format!("{inst}/lead")]);
    let parent = env.member(&inst, "lead");
    env.ok(&["start", &inst]);
    env.ok(&["stop", "-r", &format!("{inst}/{parent}")]);
    assert_eq!(env.state_of(&inst, "helper"), "stopped");
    env.ok(&["stop", &inst]);
}

#[test]
fn stopped_parents_cannot_be_deleted_before_their_kept_children() {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    let parent = env.member(&inst, "lead");
    env.ok(&["create", &format!("{inst}/helper")]);
    let child = env.member(&inst, "helper");
    let parent_address = format!("{inst}/{parent}");
    let child_address = format!("{inst}/{child}");
    env.ok(&["stop", &inst]);

    for force in [false, true] {
        let mut args = vec!["delete", &parent_address];
        if force {
            args.push("--force");
        }
        let refusal = env.fails(&args);
        assert!(refusal.contains(&child_address), "{refusal}");
        assert_eq!(env.member(&inst, "lead"), parent);
        assert_eq!(env.member(&inst, "helper"), child);
    }

    env.write("teams/oncall/team.yaml", "roles: [");
    for force in [false, true] {
        let mut args = vec!["delete", &parent_address];
        if force {
            args.push("--force");
        }
        let refusal = env.fails(&args);
        assert!(refusal.contains(&child_address), "{refusal}");
    }
    env.write("teams/oncall/team.yaml", TEAM);
    assert_eq!(env.member(&inst, "lead"), parent);
    assert_eq!(env.member(&inst, "helper"), child);

    env.ok(&["delete", &child_address]);
    env.ok(&["delete", &parent_address]);
    assert!(env.rows(&inst).iter().all(|r| r["kind"] != "member"));
    env.ok(&["delete", &inst]);
}

#[test]
fn deleted_members_cannot_restart_or_receive_work_and_ids_are_never_reused() {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    let lead = env.member(&inst, "lead");
    let address = format!("{inst}/{lead}");
    env.fails(&["delete", &address, "--force"]);
    assert_eq!(env.member(&inst, "lead"), lead);
    let published = env.publish(&inst, &json!({"to": lead, "body": "Review the sample."}));
    assert!(published.status.success(), "{}", both(&published));
    let message: Value = serde_json::from_slice(&published.stdout).unwrap();
    env.ok(&["stop", &inst]);
    let refused = env.fails(&["delete", &address]);
    assert_eq!(env.member(&inst, "lead"), lead);
    assert_eq!(env.row(&inst, "lead")["unread"], 1);
    assert!(
        refused.contains(message["id"].as_str().unwrap()),
        "{refused}"
    );
    env.ok(&["delete", &address, "--force"]);
    assert!(env.rows(&inst).iter().all(|r| r["member"] != lead));
    assert_eq!(env.state_of(&inst, "lead"), "not created (start: true)");
    assert!(!stdout(&env.ok(&["status", &inst])).contains(&lead));
    env.fails(&["start", &address]);
    assert!(env.rows(&inst).iter().all(|r| r["member"] != lead));
    let out = env.publish(&inst, &json!({"to": lead, "body": "Do not deliver."}));
    assert!(!out.status.success(), "{}", both(&out));
    env.ok(&["start", &inst]);
    let replacement = env.member(&inst, "lead");
    assert_ne!(replacement, lead);
    assert_eq!(env.row(&inst, "lead")["unread"], 0);
    // A child can't be created while its parent role has no member (#73), so
    // it comes after the replacement.
    env.ok(&["create", &format!("{inst}/helper")]);
    let child = env.member(&inst, "helper");
    assert_eq!(env.row(&inst, "helper")["parent"], replacement);
    env.wait_for("replacement idle", || env.state_of(&inst, "lead") == "idle");
    env.ok(&[
        "send",
        "--external",
        "ops",
        &format!("{inst}/{replacement}"),
        "hello",
        "--from",
        &child,
    ]);
    env.ok(&["stop", &format!("{inst}/{child}")]);
    env.ok(&["delete", &format!("{inst}/{child}")]);

    // Replaying the same suffix sequence must skip every deleted ID.
    let seeded_create = || {
        let out = env
            .cmd(&["create", &format!("{inst}/helper")])
            .env("BORGCHART_SUFFIX_SEED", "7")
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", both(&out));
        env.member(&inst, "helper")
    };
    let helper = seeded_create();
    env.ok(&["stop", &format!("{inst}/{helper}")]);
    env.ok(&["delete", &format!("{inst}/{helper}")]);
    let next = seeded_create();
    assert_ne!(next, helper);
    env.ok(&["stop", &inst]);
    env.ok(&["delete", &inst, "--force"]);
}

#[test]
fn members_with_unloadable_teams_and_incomplete_parent_records_cannot_be_deleted() {
    for missing_record in [false, true] {
        let env = Env::new();
        let inst = env.create("oncall", "work/a");
        let parent = env.member(&inst, "lead");
        env.ok(&["create", &format!("{inst}/helper")]);
        let child = env.member(&inst, "helper");
        env.ok(&["stop", &inst]);
        let start_file = env.path(&format!("state/{inst}/start.json"));
        let start = fs::read(&start_file).unwrap();
        if missing_record {
            fs::remove_file(&start_file).unwrap();
        } else {
            let mut meta: Value = serde_json::from_slice(&start).unwrap();
            meta["members"].as_object_mut().unwrap().remove(&child);
            fs::write(&start_file, serde_json::to_vec(&meta).unwrap()).unwrap();
        }
        env.write("teams/oncall/team.yaml", "roles: [");
        env.fails(&["delete", &format!("{inst}/{parent}"), "--force"]);
        fs::write(&start_file, start).unwrap();
        env.write("teams/oncall/team.yaml", TEAM);
        assert_eq!(env.member(&inst, "lead"), parent);
        assert_eq!(env.member(&inst, "helper"), child);
        env.ok(&["delete", &format!("{inst}/{child}")]);
        env.ok(&["delete", &format!("{inst}/{parent}")]);
        env.ok(&["delete", &inst]);
    }
}

#[test]
fn members_with_unknown_liveness_cannot_be_deleted() {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    let lead = env.member(&inst, "lead");
    env.ok(&["stop", &inst]);
    let start_file = env.path(&format!("state/{inst}/members/{lead}/incarnation.json"));
    let start = fs::read(&start_file).unwrap();
    fs::write(&start_file, "unreadable start record").unwrap();
    env.fails(&["delete", &format!("{inst}/{lead}"), "--force"]);
    assert_eq!(env.member(&inst, "lead"), lead);
    fs::write(&start_file, start).unwrap();
    env.ok(&["delete", &format!("{inst}/{lead}")]);
    env.ok(&["delete", &inst]);
}

#[test]
fn members_can_be_deleted_without_a_message_bus() {
    let env = Env::new();
    env.write("teams/oncall/team.yaml", &TEAM.replace("bus: {}\n", ""));
    let inst = env.create("oncall", "work/a");
    let lead = env.member(&inst, "lead");
    env.ok(&["stop", &inst]);
    env.ok(&["delete", &format!("{inst}/{lead}")]);
    env.ok(&["start", &inst]);
    assert_ne!(env.member(&inst, "lead"), lead);
    env.ok(&["stop", &inst]);
    env.ok(&["delete", &inst]);
}

#[test]
fn publish_refusals_only_suggest_declared_role_addresses() {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    for to in ["helpr", "helper-zzzz", "helper"] {
        let out = env.publish(&inst, &json!({"to": to, "body": "Review the sample."}));
        assert!(!out.status.success(), "{}", both(&out));
        let refusal: Value = serde_json::from_slice(&out.stdout).unwrap();
        let message = refusal["message"].as_str().unwrap();
        if to == "helper" {
            assert!(
                message.contains(&format!("brg create {inst}/helper")),
                "{message}"
            );
        } else {
            assert!(
                !message.contains(&format!("brg create {inst}/{to}")),
                "{message}"
            );
        }
    }
    env.ok(&["stop", &inst]);
}

#[test]
fn role_addresses_never_choose_a_member_and_status_tracks_each_start() {
    let env = Env::new();
    env.write(
        "teams/oncall/team.yaml",
        &TEAM.replace("prompt: lead.md", "prompt: lead.md\n    start: false"),
    );
    let out = env.ok(&[
        "create",
        "oncall",
        &env.path("work/a").display().to_string(),
    ]);
    let empty = stdout(&out).split_whitespace().next().unwrap().to_string();
    assert!(env.rows(&empty).iter().all(|r| r["kind"] == "role"));
    let tree = stdout(&env.ok(&["status", &empty]));
    for role in ["lead", "helper", "scout"] {
        assert!(
            tree.lines()
                .any(|line| line.contains(role) && line.contains("not created")),
            "{tree}"
        );
    }
    env.ok(&["delete", &empty]);
    env.write("teams/oncall/team.yaml", TEAM);
    let inst = env.create("oncall", "work/a");
    let lead = env.member(&inst, "lead");
    for command in ["start", "stop", "attach", "logs", "delete", "send"] {
        let address = format!("{inst}/helper");
        let args = if command == "send" {
            vec![command, address.as_str(), "hello", "--external", "ops"]
        } else {
            vec![command, address.as_str()]
        };
        let error = env.fails(&args);
        assert!(
            error.contains(&format!("brg create {inst}/helper")),
            "{command}: {error}"
        );
    }
    let tree = stdout(&env.ok(&["status", &inst]));
    assert!(tree.contains("not created (start: false)"), "{tree}");
    env.ok(&["create", &format!("{inst}/helper")]);
    let helper = env.member(&inst, "helper");
    let address = format!("{inst}/{helper}");
    env.wait_for("helper idle", || env.state_of(&inst, "helper") == "idle");
    let row = env.row(&inst, "helper");
    assert_eq!(row["parent"], lead);
    assert_eq!(
        row["start_id"],
        env.starts(&helper)[0]["env"]["BORGCHART_INCARNATION"]
    );
    let start_id = row["start_id"].as_str().expect("a start ID").to_string();
    let out = env.publish(&inst, &json!({"to": "helper", "body": "Do not choose."}));
    assert!(!out.status.success());
    assert!(both(&out).contains(&helper), "{}", both(&out));
    let out = env.publish(&inst, &json!({"to": helper, "body": "Review the sample."}));
    assert!(out.status.success(), "{}", both(&out));
    env.ok(&["send", &address, "hello", "--external", "ops"]);
    for stopped in [false, true] {
        if stopped {
            env.ok(&["stop", &address]);
        }
        for command in ["stop", "send"] {
            let role = format!("{inst}/helper");
            let args = if command == "send" {
                vec![command, role.as_str(), "hello", "--external", "ops"]
            } else {
                vec![command, role.as_str()]
            };
            let error = env.fails(&args);
            assert!(error.contains(&helper), "{error}");
        }
        let tree = stdout(&env.ok(&["status", &inst]));
        assert!(tree.contains(&helper), "{tree}");
        let row = env.row(&inst, "helper");
        assert_eq!(row["start_id"], start_id);
        assert_eq!(row["member"], helper);
        assert_eq!(row["unread"], 2);
    }
    env.ok(&["start", &address]);
    assert_eq!(env.member(&inst, "helper"), helper);
    assert_ne!(env.row(&inst, "helper")["start_id"], start_id);
    env.ok(&["stop", &inst]);
}

// --- 1. IDs ---

#[test]
fn create_twice_makes_two_instances_with_suffixed_members() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    let b = env.create("oncall", "work/b");
    assert_ne!(a, b);
    let lead_a = env.member(&a, "lead");
    let lead_b = env.member(&b, "lead");
    assert!(well_formed(&lead_a, "lead"), "{lead_a}");
    assert!(well_formed(&lead_b, "lead"), "{lead_b}");
    // The member's own ID reaches it, with its instance and role.
    let start = &env.starts(&lead_a)[0];
    assert_eq!(start["env"]["BORGCHART_MEMBER"], lead_a);
    assert_eq!(start["env"]["BORGCHART_INSTANCE"], a);
    assert_eq!(start["env"]["BORGCHART_ROLE"], "lead");
    assert!(
        start["env"]["BORGCHART_NOTES"]
            .as_str()
            .unwrap()
            .ends_with(&format!("{a}/notes"))
    );
    assert_eq!(
        start["cwd"].as_str().unwrap(),
        env.path("work/a")
            .canonicalize()
            .unwrap()
            .display()
            .to_string()
    );
    // A role's name where a member is expected is refused, listing it.
    let err = env.fails(&["stop", &format!("{a}/lead")]);
    assert!(err.contains("is a role") && err.contains(&lead_a), "{err}");
    let err = env.fails(&["logs", &format!("{a}/lead")]);
    assert!(err.contains(&lead_a), "{err}");
    assert_eq!(env.state_of(&a, "lead"), "idle", "nothing was stopped");
    // The tmux session is `brg-<instance>`.
    assert!(
        env.tmux(&["list-sessions", "-F", "#{session_name}"])
            .contains(&format!("brg-{a}"))
    );
    env.ok(&["stop", &a]);
    env.ok(&["stop", &b]);
}

#[test]
fn a_deleted_instances_suffix_is_never_issued_again() {
    let env = Env::new();
    let seeded = |args: &[&str]| {
        let out = env
            .cmd(args)
            .env("BORGCHART_SUFFIX_SEED", "7")
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", both(&out));
        out
    };
    let folder = env.path("work/a").display().to_string();
    let first = stdout(&seeded(&["create", "oncall", &folder]))
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    env.ok(&["stop", &first]);
    env.ok(&["delete", &first]);
    let second = stdout(&seeded(&["create", "oncall", &folder]))
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    assert!(well_formed(&first, "oncall") && well_formed(&second, "oncall"));
    assert_ne!(first, second, "the same seed draws the same suffix first");
    let issued = env.registry()["issued"]["oncall"].clone();
    assert_eq!(issued.as_array().unwrap().len(), 2, "{issued}");
    env.ok(&["stop", &second]);
}

// --- 2. Finding teams ---

#[test]
fn teams_are_found_by_name_or_by_path() {
    let env = Env::new();
    // By path, from the current folder and absolute.
    let out = env.ok(&["validate", "./teams/oncall"]);
    assert!(stdout(&out).starts_with("ok:"), "{}", both(&out));
    env.ok(&["validate", &env.path("teams/oncall").display().to_string()]);
    env.ok(&["validate", "oncall"]);
    // A path without `./` is refused with the hint.
    let err = env.fails(&["create", "teams/oncall", "work/a"]);
    assert!(err.contains("./teams/oncall"), "{err}");
    let err = env.fails(&["validate", "teams/oncall"]);
    assert!(err.contains("./teams/oncall"), "{err}");
    // A name in two search paths is refused, listing both.
    env.team("more/oncall");
    env.write(
        "config.yaml",
        &format!(
            "schema: 1\nteams: [{}, {}]\n",
            env.path("teams").display(),
            env.path("more").display()
        ),
    );
    let err = env.fails(&["create", "oncall", "work/a"]);
    assert!(
        err.contains(
            &env.path("teams/oncall")
                .canonicalize()
                .unwrap()
                .display()
                .to_string()
        ) && err.contains(
            &env.path("more/oncall")
                .canonicalize()
                .unwrap()
                .display()
                .to_string()
        ),
        "{err}"
    );
    // Two folders with the same name both run, by path.
    let a = stdout(&env.ok(&["create", "./teams/oncall", "work/a"]))
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    let b = stdout(&env.ok(&["create", "./more/oncall", "work/b"]))
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    assert!(well_formed(&a, "oncall") && well_formed(&b, "oncall") && a != b);
    let ls: Value = serde_json::from_slice(&env.ok(&["ls", "--json"]).stdout).unwrap();
    let teams: Vec<&Value> = ls.as_array().unwrap().iter().collect();
    assert_eq!(teams.len(), 2, "{ls}");
    for t in &teams {
        assert_eq!(t["instances"].as_array().unwrap().len(), 1, "{ls}");
    }
    env.ok(&["stop", &a]);
    env.ok(&["stop", &b]);
}

#[test]
fn create_needs_an_existing_folder() {
    let env = Env::new();
    let err = env.fails(&["create", "oncall"]);
    assert!(err.contains("folder"), "{err}");
    let err = env.fails(&["create", "oncall", "work/missing"]);
    assert!(err.contains("does not exist"), "{err}");
    assert!(!env.path("state/registry.json").exists() || env.registry()["instances"] == json!({}));
}

// --- 3, 4, 10. start, stop and create of a member ---

#[test]
fn start_brings_back_what_should_run_and_leaves_running_members_alone() {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    let lead = env.member(&inst, "lead");
    env.ok(&["create", &format!("{inst}/helper")]);
    let helper = env.member(&inst, "helper");
    assert!(well_formed(&helper, "helper"), "{helper}");
    env.wait_for("helper idle", || env.state_of(&inst, "helper") == "idle");
    // Running: only a crashed member is started again.
    env.tmux(&["kill-window", "-t", &format!("=brg-{inst}:={lead}")]);
    let out = env.ok(&["start", &inst]);
    assert!(stdout(&out).contains("already running"), "{}", both(&out));
    env.wait_for("lead idle", || env.state_of(&inst, "lead") == "idle");
    assert_eq!(env.starts(&lead).len(), 2);
    assert_eq!(env.starts(&helper).len(), 1);
    // Stopped: the members running at the stop come back. A second stop
    // keeps the first's record.
    env.ok(&["stop", &inst]);
    let out = env.ok(&["stop", &inst]);
    assert!(stdout(&out).contains("not running"), "{}", both(&out));
    env.ok(&["start", &inst]);
    env.wait_for("helper idle", || env.state_of(&inst, "helper") == "idle");
    assert_eq!(env.starts(&helper).len(), 2);
    assert_eq!(env.starts(&lead).len(), 3);
    env.ok(&["stop", &inst]);
}

#[test]
fn full_start_leaves_a_stopped_member_with_unacked_messages_stopped() {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    env.ok(&["create", &format!("{inst}/scout")]);
    let scout = env.member(&inst, "scout");
    env.wait_for("scout idle", || env.state_of(&inst, "scout") == "idle");
    env.ok(&["stop", &format!("{inst}/{scout}")]);
    let out = env.publish(
        &inst,
        &json!({"to": scout, "body": "Read this when started."}),
    );
    assert!(out.status.success(), "{}", both(&out));
    env.ok(&["stop", &inst]);
    env.ok(&["start", &inst]);
    env.wait_for("lead idle", || env.state_of(&inst, "lead") == "idle");
    assert_eq!(env.state_of(&inst, "scout"), "stopped");
    assert_eq!(env.starts(&scout).len(), 1);
    let inbox = env.ok(&["inbox", "--member", &scout, "--instance", &inst, "--json"]);
    let inbox: Value = serde_json::from_slice(&inbox.stdout).unwrap();
    assert_eq!(inbox["records"][0]["id"], "m1");
    env.ok(&["stop", &inst]);
}

#[test]
fn stop_on_a_stopped_instance_or_member_is_a_no_op() {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    let lead = env.member(&inst, "lead");
    env.ok(&["stop", &format!("{inst}/{lead}")]);
    let out = env.ok(&["stop", &format!("{inst}/{lead}")]);
    assert!(stdout(&out).contains("not running"), "{}", both(&out));
    env.ok(&["stop", &inst]);
    env.ok(&["stop", &inst]);
}

#[test]
fn create_of_an_existing_member_is_refused_and_start_resumes_it() {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    env.ok(&["create", &format!("{inst}/helper")]);
    let helper = env.member(&inst, "helper");
    let err = env.fails(&["create", &format!("{inst}/helper")]);
    assert!(err.contains(&format!("brg start {inst}/{helper}")), "{err}");
    env.ok(&["stop", &format!("{inst}/{helper}")]);
    let err = env.fails(&["create", &format!("{inst}/helper")]);
    assert!(err.contains(&format!("brg start {inst}/{helper}")), "{err}");
    let out = env.ok(&["start", &format!("{inst}/{helper}")]);
    assert!(stdout(&out).contains("started"), "{}", both(&out));
    env.wait_for("helper idle", || env.state_of(&inst, "helper") == "idle");
    assert_eq!(env.member(&inst, "helper"), helper, "the same member");
    assert_eq!(env.starts(&helper).len(), 2);
    let out = env.ok(&["start", &format!("{inst}/{helper}")]);
    assert!(stdout(&out).contains("already running"), "{}", both(&out));
    env.ok(&["stop", &inst]);
}

// --- 5. delete ---

#[test]
fn delete_needs_a_stop_and_force_for_unacked_messages_and_keeps_the_folder() {
    let env = Env::new();
    fs::write(env.path("work/a/keep.txt"), "mine\n").unwrap();
    let inst = env.create("oncall", "work/a");
    let lead = env.member(&inst, "lead");
    let err = env.fails(&["delete", &inst]);
    assert!(
        err.contains("running") && err.contains(&format!("brg stop {inst}")),
        "{err}"
    );
    let out = env.publish(&inst, &json!({"to": lead, "body": "hi"}));
    assert!(out.status.success(), "{}", both(&out));
    env.ok(&["stop", &inst]);
    let err = env.fails(&["delete", &inst]);
    assert!(
        err.contains("open work") && err.contains("unread m1"),
        "{err}"
    );
    assert!(env.path(&format!("state/{inst}")).is_dir());
    env.ok(&["delete", &inst, "--force"]);
    assert!(!env.path(&format!("state/{inst}")).exists());
    assert!(env.registry()["instances"].get(&inst).is_none());
    assert_eq!(
        fs::read_to_string(env.path("work/a/keep.txt")).unwrap(),
        "mine\n"
    );
    let err = env.fails(&["status", &inst]);
    assert!(err.contains("no instance"), "{err}");
}

// --- 8. Addressing ---

#[test]
fn addresses_name_the_instance_in_full_outside_a_member() {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    let lead = env.member(&inst, "lead");
    // Outside a member, a member needs its instance, even with one running.
    let err = env.fails(&["logs", &lead]);
    assert!(err.contains(&format!("<instance>/{lead}")), "{err}");
    // A team's name where an instance is expected.
    let err = env.fails(&["status", "oncall"]);
    assert!(err.contains("is a team") && err.contains(&inst), "{err}");
    // Inside a member, the instance comes from BORGCHART_INSTANCE.
    let out = env
        .cmd(&["logs", &lead])
        .env("BORGCHART_INSTANCE", &inst)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    // Bus commands take `--instance`, required outside a member.
    let err = env.fails(&["inbox", "--external", "ops"]);
    assert!(err.contains("--instance") && err.contains(&inst), "{err}");
    env.ok(&["inbox", "--external", "ops", "--instance", &inst]);
    env.ok(&["bus", "check", "--instance", &inst]);
    // An instance isn't a recipient; nor is a role.
    for to in [inst.as_str(), "lead"] {
        let out = env.publish(&inst, &json!({"to": to, "body": "hi"}));
        assert!(!out.status.success(), "{}", both(&out));
        assert!(both(&out).contains(&lead), "{}", both(&out));
    }
    // A role with no member shows as on demand.
    assert_eq!(env.state_of(&inst, "helper"), "not created (start: false)");
    let text = stdout(&env.ok(&["status", &inst]));
    assert!(text.lines().next().unwrap().starts_with(&inst), "{text}");
    assert!(
        text.lines()
            .any(|l| l.contains("helper") && l.contains("not created (start: false)")),
        "{text}"
    );
    // Bare `status` shows every instance.
    let b = env.create("oncall", "work/b");
    let all = stdout(&env.ok(&["status"]));
    assert!(all.contains(&inst) && all.contains(&b), "{all}");
    env.ok(&["stop", &inst]);
    env.ok(&["stop", &b]);
}

/// `brg status [<instance>] -w --interval 1`, its output collected as it
/// comes: one screen per refresh.
struct Watch {
    child: std::process::Child,
    out: std::sync::Arc<std::sync::Mutex<String>>,
}

impl Watch {
    fn start(env: &Env, instance: Option<&str>) -> Watch {
        let mut args = vec!["status"];
        args.extend(instance);
        args.extend(["-w", "--interval", "1"]);
        let mut child = env
            .cmd(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let out = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sink = out.clone();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = [0u8; 4096];
            while let Ok(n) = stdout.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });
        Watch { child, out }
    }

    /// Each complete screen so far (one ends where the next begins).
    fn screens(&self) -> Vec<String> {
        let out = self.out.lock().unwrap().clone();
        let mut parts: Vec<String> = out.split("\x1b[2J").skip(1).map(String::from).collect();
        parts.pop();
        parts
    }

    fn running(&mut self) -> bool {
        self.child.try_wait().unwrap().is_none()
    }

    fn dump(&self) -> String {
        self.out.lock().unwrap().clone()
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn status_w_with_no_instance_follows_every_instance() {
    let env = Env::new();
    let mut watch = Watch::start(&env, None);
    env.wait_for("a screen saying there are none", || {
        watch
            .screens()
            .iter()
            .any(|s| s.contains("there are no instances"))
    });
    let seen = watch.screens().len();
    env.wait_for("another refresh", || watch.screens().len() > seen);
    assert!(watch.running(), "{}", watch.dump());
    let a = env.create("oncall", "work/a");
    let b = env.create("oncall", "work/b");
    env.wait_for("a refresh showing both instances", || {
        watch
            .screens()
            .iter()
            .any(|s| s.contains(&a) && s.contains(&b) && s.contains("lead"))
    });
    assert!(watch.running(), "{}", watch.dump());
    drop(watch);
    env.ok(&["stop", &a]);
    env.ok(&["stop", &b]);
}

#[test]
fn status_w_with_an_instance_shows_only_that_instance() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    let b = env.create("oncall", "work/b");
    let watch = Watch::start(&env, Some(&a));
    env.wait_for("two screens", || watch.screens().len() >= 2);
    for screen in watch.screens() {
        assert!(screen.contains(&a) && !screen.contains(&b), "{screen}");
    }
    drop(watch);
    env.ok(&["stop", &a]);
    env.ok(&["stop", &b]);
}

// --- 14. Configuration ---

#[test]
fn auth_comes_from_the_configuration_and_a_missing_file_means_login() {
    let env = Env::new();
    let sentinel = "sk-not-a-real-key-2b7d";
    // No configuration file: the defaults, so no key even though the shell has one.
    let out = env
        .cmd(&[
            "create",
            "./teams/oncall",
            &env.path("work/a").display().to_string(),
        ])
        .env("BORGCHART_CONFIG", env.path("missing.yaml"))
        .env("ANTHROPIC_API_KEY", sentinel)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    let a = stdout(&out).split_whitespace().next().unwrap().to_string();
    env.wait_for("lead idle", || env.state_of(&a, "lead") == "idle");
    let lead = env.member(&a, "lead");
    assert!(
        env.starts(&lead)[0]["env"]
            .get("ANTHROPIC_API_KEY")
            .is_none()
    );
    // `auth: api_key` from the configuration passes it.
    env.config("auth: api_key\n");
    let out = env
        .cmd(&[
            "create",
            "oncall",
            &env.path("work/b").display().to_string(),
        ])
        .env("ANTHROPIC_API_KEY", sentinel)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    assert!(!both(&out).contains(sentinel));
    let b = stdout(&out).split_whitespace().next().unwrap().to_string();
    env.wait_for("lead idle", || env.state_of(&b, "lead") == "idle");
    let lead_b = env.member(&b, "lead");
    assert_eq!(env.starts(&lead_b)[0]["env"]["ANTHROPIC_API_KEY"], sentinel);
    // A changed auth setting applies to a new member while its siblings run.
    env.config("");
    env.ok(&["create", &format!("{b}/scout")]);
    let scout = env.member(&b, "scout");
    env.wait_for("scout started", || !env.starts(&scout).is_empty());
    assert!(
        env.starts(&scout)[0]["env"]
            .get("ANTHROPIC_API_KEY")
            .is_none()
    );
    assert_eq!(env.starts(&lead_b).len(), 1);
    env.ok(&["stop", &b]);
    env.ok(&["start", &b]);
    env.wait_for("lead idle", || env.state_of(&b, "lead") == "idle");
    assert!(
        env.starts(&lead_b)[1]["env"]
            .get("ANTHROPIC_API_KEY")
            .is_none()
    );
    env.ok(&["stop", &a]);
    env.ok(&["stop", &b]);
}

#[test]
fn auto_trust_comes_from_the_configuration() {
    let env = Env::new();
    fs::create_dir_all(env.path("home/fake-agent")).unwrap();
    fs::write(env.path("home/fake-agent/trust"), "").unwrap();
    let folder = env.path("work/a").display().to_string();
    // The default leaves a `vendor` member's trust dialog for a person.
    let out = env.ok(&["create", "oncall", &folder]);
    assert!(
        !stdout(&out).contains("accepted folder trust"),
        "{}",
        both(&out)
    );
    let a = stdout(&out).split_whitespace().next().unwrap().to_string();
    let lead = env.member(&a, "lead");
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !fs::read_to_string(env.path(&format!("home/fake-agent/{lead}.jsonl")))
            .unwrap_or_default()
            .contains("trust-accepted")
    );
    env.ok(&["stop", &a]);
    env.config("auto_trust: true\n");
    let out = env.ok(&[
        "create",
        "oncall",
        &env.path("work/b").display().to_string(),
    ]);
    assert!(
        stdout(&out).contains("accepted folder trust for"),
        "{}",
        both(&out)
    );
    let b = stdout(&out).split_whitespace().next().unwrap().to_string();
    env.ok(&["stop", &b]);
}

#[test]
fn tmux_user_config_comes_from_the_configuration_and_a_change_is_reported() {
    let env = Env::new();
    env.write("my-tmux.conf", "set -g @my-option loaded\n");
    env.config(&format!(
        "tmux: {{user_config: {}}}\n",
        env.path("my-tmux.conf").display()
    ));
    let a = env.create("oncall", "work/a");
    assert_eq!(
        env.tmux(&["show-options", "-gqv", "@my-option"]).trim(),
        "loaded"
    );
    env.config("");
    let out = env.ok(&[
        "create",
        "oncall",
        &env.path("work/b").display().to_string(),
    ]);
    assert!(
        stdout(&out).contains("takes effect only once"),
        "{}",
        both(&out)
    );
    let b = stdout(&out).split_whitespace().next().unwrap().to_string();
    env.ok(&["stop", &a]);
    env.ok(&["stop", &b]);
}

#[test]
fn plain_plugin_values_come_from_the_team() {
    let env = Env::new();
    env.write(
        "teams/oncall/team.yaml",
        &TEAM.replace(
            "roles:\n  lead:\n    prompt: lead.md\n",
            "plugins:\n  pager:\n    path: plugins/pager\n    env:\n      PAGER_ROOM: R0000EXAMPLE\n      PAGER_TOKEN:\n    actions:\n      read: pager read <id>\nroles:\n  lead:\n    prompt: lead.md\n    plugins: {pager: [read]}\n",
        ),
    );
    fs::create_dir_all(env.path("teams/oncall/plugins/pager")).unwrap();
    script(
        &env.path("teams/oncall/plugins/pager/pager"),
        "#!/bin/sh\necho \"room=$PAGER_ROOM token_len=${#PAGER_TOKEN} instance=$BORGCHART_INSTANCE\" >> \"$BORGCHART_STATE_DIR/calls.log\"\n",
    );
    // The secret must come from the environment.
    let err = env.fails(&[
        "create",
        "oncall",
        &env.path("work/a").display().to_string(),
    ]);
    assert!(
        err.contains("PAGER_TOKEN") && !err.contains("PAGER_ROOM"),
        "{err}"
    );
    let out = env
        .cmd(&[
            "create",
            "oncall",
            &env.path("work/a").display().to_string(),
        ])
        .env("PAGER_TOKEN", "tok-not-real")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    let inst = stdout(&out).split_whitespace().next().unwrap().to_string();
    env.wait_for("lead idle", || env.state_of(&inst, "lead") == "idle");
    let lead = env.member(&inst, "lead");
    let shim = env.path(&format!("state/{inst}/members/{lead}/bin/pager"));
    let run = Command::new(&shim)
        .args(["read", "1"])
        .env_clear()
        .envs(
            serde_json::from_value::<std::collections::BTreeMap<String, String>>(
                env.starts(&lead)[0]["env"].clone(),
            )
            .unwrap(),
        )
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", both(&run));
    let log =
        fs::read_to_string(env.path(&format!("state/{inst}/plugins/pager/calls.log"))).unwrap();
    assert_eq!(
        log.trim(),
        format!("room=R0000EXAMPLE token_len=12 instance={inst}")
    );
    env.ok(&["stop", &inst]);
}

#[test]
fn a_roles_args_come_from_the_team_and_members_work_in_its_folder() {
    let env = Env::new();
    env.write(
        "teams/oncall/team.yaml",
        &TEAM.replace(
            "  lead:\n    prompt: lead.md\n",
            "  lead:\n    prompt: lead.md\n    args: [--example-flag]\n",
        ),
    );
    let inst = env.create("oncall", "work/a");
    let lead = env.member(&inst, "lead");
    let start = &env.starts(&lead)[0];
    assert_eq!(
        start["cwd"].as_str().unwrap(),
        env.path("work/a")
            .canonicalize()
            .unwrap()
            .display()
            .to_string()
    );
    assert!(
        start["argv"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a == "--example-flag"),
        "{start}"
    );
    env.ok(&["stop", &inst]);
    // `cwd` is gone: members work in the instance's folder or a worktree.
    env.write(
        "teams/oncall/team.yaml",
        &TEAM.replace(
            "  lead:\n    prompt: lead.md\n",
            "  lead:\n    prompt: lead.md\n    cwd: sub\n",
        ),
    );
    let err = env.fails(&["validate", "oncall"]);
    assert!(err.contains("unknown field `cwd`"), "{err}");
}
