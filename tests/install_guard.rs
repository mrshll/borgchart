//! Reproduce catalog upgrades blocking joins, and check the full-restart escape.
#![cfg(feature = "test-support")]

mod common;
mod instance_env;

use std::fs;
use std::os::unix::fs::symlink;

use instance_env::*;

fn catalog(env: &Env, root: &str) {
    env.team(&format!("{root}/oncall"));
    env.team(&format!("{root}/dev"));
    env.write(
        &format!("{root}/oncall/team.yaml"),
        &format!("{TEAM}  dev: {{team: dev, parent: lead, max: 2}}\n"),
    );
}

fn create_copy(env: &Env, inst: &str) -> String {
    stdout(&env.ok(&["create", &format!("{inst}/dev")]))
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

fn resumed(env: &Env, inst: &str, member: &str, session: &str) {
    env.wait_for("resumed fake agent", || env.starts(member).len() == 2);
    let starts = env.starts(member);
    let args: Vec<&str> = starts[1]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        args.windows(2).any(|a| a == ["--resume", session]),
        "{inst}/{member}: {args:?}"
    );
}

fn catalog_change_blocks_joins_until_restart(flip_symlink: bool) {
    let env = Env::new();
    for root in ["revision-a", "revision-b"] {
        catalog(&env, root);
    }
    if flip_symlink {
        symlink(env.path("revision-a"), env.path("current")).unwrap();
    }
    env.write(
        "config.yaml",
        if flip_symlink {
            "schema: 1\nteams: [current]\n"
        } else {
            "schema: 1\nteams: [revision-a]\n"
        },
    );
    let inst = env.create("oncall", "work/a");
    let copy = create_copy(&env, &inst);
    env.ok(&["start", &copy]);
    env.wait_for("copy lead idle", || env.state_of(&copy, "lead") == "idle");
    let root_lead = env.member(&inst, "lead");
    let copy_lead = env.member(&copy, "lead");
    let session = |instance: &str, member: &str| {
        let path = env.path(&format!("state/{instance}/members/{member}/session"));
        env.wait_for("vendor session recorded", || path.exists());
        fs::read_to_string(path).unwrap().trim().to_string()
    };
    let root_session = session(&inst, &root_lead);
    let copy_session = session(&copy, &copy_lead);
    env.ok(&["create", &format!("{inst}/scout")]);
    let scout = env
        .rows(&inst)
        .into_iter()
        .find(|row| row["instance"] == inst && row["role"] == "scout")
        .unwrap()["member"]
        .as_str()
        .unwrap()
        .to_string();
    env.ok(&["stop", &format!("{inst}/{scout}")]);
    let note = format!("state/{inst}/notes/draft.txt");
    env.write(&note, "unfinished draft\n");
    let sent = env.ok(&[
        "send",
        &format!("{inst}/{root_lead}"),
        "Keep the draft",
        "--external",
        "ops",
        "--json",
    ]);
    let message: serde_json::Value = serde_json::from_slice(&sent.stdout).unwrap();
    let message_id = message["id"].as_str().unwrap();

    if flip_symlink {
        fs::remove_file(env.path("current")).unwrap();
        symlink(env.path("revision-b"), env.path("current")).unwrap();
    } else {
        env.write("config.yaml", "schema: 1\nteams: [revision-b]\n");
    }
    // These are characterizations of the failure, not the desired future policy.
    for args in [
        vec!["create".into(), format!("{inst}/dev")],
        vec!["create".into(), format!("{inst}/helper")],
        vec!["start".into(), format!("{inst}/{scout}")],
        vec!["start".into(), inst.clone()],
        vec!["start".into(), inst.clone(), "--resume".into()],
    ] {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = env.brg(&args);
        println!("{args:?}: exit {:?}\n{}", out.status.code(), both(&out));
        assert!(!out.status.success(), "{}", both(&out));
    }
    // A copy with no team roles still uses its own unchanged source files.
    env.ok(&["start", &copy]);
    env.ok(&["create", &format!("{copy}/helper")]);
    env.ok(&["stop", &inst]);
    let out = env.ok(&["start", &inst, "--resume"]);
    println!("full restart: exit {:?}\n{}", out.status.code(), both(&out));
    resumed(&env, &inst, &root_lead, &root_session);
    resumed(&env, &copy, &copy_lead, &copy_session);
    assert_eq!(
        fs::read_to_string(env.path(&note)).unwrap(),
        "unfinished draft\n"
    );
    env.ok(&["msg", "read", message_id, "--instance", &inst]);
    let next = create_copy(&env, &inst);
    env.ok(&["start", &next]);
    env.wait_for("new copy lead started", || {
        env.state_of(&next, "lead") == "idle"
    });
    // The public status header identifies the source used by each kept copy.
    for (instance, revision) in [
        (&inst, "revision-a/oncall"),
        (&copy, "revision-a/dev"),
        (&next, "revision-b/dev"),
    ] {
        let status = stdout(&env.ok(&["status", instance]));
        assert!(
            status.contains(&env.path(revision).display().to_string()),
            "{status}"
        );
    }
    env.ok(&["stop", &inst]);
}

#[test]
fn identical_catalog_repoint_blocks_running_parent_joins() {
    catalog_change_blocks_joins_until_restart(false);
}

#[test]
fn current_symlink_flip_blocks_running_parent_joins() {
    catalog_change_blocks_joins_until_restart(true);
}
