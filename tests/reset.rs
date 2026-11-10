//! `brg reset <root>`: stop a whole tree and forget it, so `brg create`
//! starts it fresh; and what it refuses.
//!
//! Run with `cargo test --features test-support`.
#![cfg(feature = "test-support")]

mod common;

mod instance_env;

use std::collections::BTreeMap;
use std::fs;

use instance_env::*;
use serde_json::{Value, json};

/// The test team with `helper` working in `copies/{member_id}` and `dev`,
/// a team role whose copies run the `dev` team in `dev/{member_id}`.
const TREE: &str = r#"
bus: {}
roles:
  lead:
    prompt: lead.md
    cwd: lead
  helper:
    prompt: helper.md
    vendor: codex
    parent: lead
    start: false
    max: 2
    cwd: "copies/{member_id}"
  dev:
    team: dev
    parent: lead
    start: false
    max: 2
    cwd: "dev/{member_id}"
"#;

fn tree_env() -> Env {
    let env = Env::new();
    env.write("teams/oncall/team.yaml", TREE);
    env.team("teams/dev");
    env.write(
        "teams/dev/team.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md, cwd: repo}\n",
    );
    fs::create_dir_all(env.path("work/a/lead")).unwrap();
    fs::create_dir_all(env.path("work/a/copies")).unwrap();
    env
}

/// A started dev copy of `root`: its ID and folder.
fn started_copy(env: &Env, root: &str) -> (String, std::path::PathBuf) {
    let out = env.ok(&["create", &format!("{root}/dev")]);
    let copy = stdout(&out).split_whitespace().next().unwrap().to_string();
    let folder = env.path(&format!("work/a/dev/{}", copy.rsplit('/').next().unwrap()));
    fs::create_dir(folder.join("repo")).unwrap();
    env.ok(&["start", &copy]);
    env.wait_for("copy lead", || !env.member(&copy, "lead").is_empty());
    (copy, folder)
}

fn registered(env: &Env) -> Vec<String> {
    let registry: Value =
        serde_json::from_slice(&fs::read(env.path("state/registry.json")).unwrap()).unwrap();
    registry["instances"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect()
}

#[test]
fn reset_stops_and_forgets_a_tree_so_create_starts_it_fresh() {
    let env = tree_env();
    let root = env.create("oncall", "work/a");
    let lead = env.member(&root, "lead");
    env.ok(&["create", &format!("{root}/helper")]);
    let helper = env.member(&root, "helper");
    let (copy, copy_folder) = started_copy(&env, &root);
    // Open work that `delete` would refuse without --force.
    let sent = env.publish(&root, &json!({"to": lead, "body": "unread"}));
    assert!(sent.status.success(), "{}", both(&sent));
    fs::write(env.path("work/a/lead/kept.txt"), "kept").unwrap();
    fs::write(copy_folder.join("repo/kept.txt"), "kept").unwrap();
    // Another tree working in an empty folder brg made for this tree.
    let out = env.ok(&["create", &format!("{root}/dev")]);
    let spare = stdout(&out).split_whitespace().next().unwrap().to_string();
    let spare_folder = env.path(&format!("work/a/dev/{}", spare.rsplit('/').next().unwrap()));
    fs::create_dir(spare_folder.join("lead")).unwrap();
    let other = env.create(
        "oncall",
        &format!("work/a/dev/{}", spare.rsplit('/').next().unwrap()),
    );

    let out = env.ok(&["reset", &root]);
    let text = stdout(&out);

    assert!(text.starts_with(&format!("resetting {root}:")), "{text}");
    assert_eq!(registered(&env), std::slice::from_ref(&other), "{text}");
    assert!(!env.path(&format!("state/{root}")).exists(), "{text}");
    let sessions = env.tmux(&["list-sessions", "-F", "#{session_name}"]);
    assert!(!sessions.contains(&root), "still running: {sessions}");
    assert_eq!(
        fs::read_to_string(env.path("work/a/lead/kept.txt")).unwrap(),
        "kept"
    );
    assert_eq!(
        fs::read_to_string(copy_folder.join("repo/kept.txt")).unwrap(),
        "kept"
    );
    assert!(
        !env.path(&format!("work/a/copies/{helper}")).exists(),
        "{text}"
    );
    assert!(
        text.contains(&format!("left {}: holds files", copy_folder.display())),
        "{text}"
    );
    assert!(spare_folder.join("lead").is_dir(), "{text}");
    assert!(
        text.contains(&format!("left {}: an instance", spare_folder.display())),
        "{text}"
    );

    // The printed command starts the tree again, under a new ID.
    env.ok(&["stop", &other]);
    let create = text
        .lines()
        .last()
        .and_then(|line| line.strip_prefix("start it fresh: brg "))
        .unwrap_or_else(|| panic!("no create line: {text}"));
    let created = env.ok(&create.split_whitespace().collect::<Vec<_>>());
    let fresh = stdout(&created)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    assert!(well_formed(&fresh, "oncall") && fresh != root, "{fresh}");
    assert!(!registered(&env).contains(&copy));
    env.ok(&["stop", &fresh]);
}

#[test]
fn reset_refuses_members_copies_and_trees_it_cannot_confirm_stopped() {
    let env = tree_env();
    let root = env.create("oncall", "work/a");
    let lead = env.member(&root, "lead");
    let (copy, _) = started_copy(&env, &root);
    let before = registered(&env);

    // Inside a member: the reset would stop its own caller.
    env.wait_for("lead start record", || !env.starts(&lead).is_empty());
    let vars: BTreeMap<String, String> =
        serde_json::from_value(env.starts(&lead)[0]["env"].clone()).unwrap();
    let inside = common::command(env!("CARGO_BIN_EXE_brg"))
        .env_clear()
        .envs(vars)
        .args(["reset", &root])
        .output()
        .unwrap();
    assert!(!inside.status.success(), "{}", both(&inside));
    assert!(
        both(&inside).contains("your own shell"),
        "{}",
        both(&inside)
    );

    // A copy is not a tree's root.
    assert!(env.fails(&["reset", &copy]).contains("root"));

    // A member whose stop brg can't confirm.
    let copy_lead = env.member(&copy, "lead");
    let incarnation = env.path(&format!(
        "state/{copy}/members/{copy_lead}/incarnation.json"
    ));
    let original = fs::read(&incarnation).unwrap();
    fs::write(&incarnation, "unreadable start record").unwrap();
    env.tmux(&["new-session", "-d", "-s", "sentinel", "sleep", "60"]);
    // Reset's stop is what refuses here, naming the member it can't confirm.
    let refused = env.fails(&["reset", &root]);
    assert!(refused.contains(&copy_lead), "{refused}");

    assert_eq!(registered(&env), before);
    assert!(env.path(&format!("state/{root}/instance.json")).is_file());
    fs::write(incarnation, original).unwrap();
    env.ok(&["stop", &root]);
    env.tmux(&["kill-session", "-t", "=sentinel"]);
}

#[test]
fn reset_says_and_fails_when_it_cannot_place_folders_brg_made() {
    let env = tree_env();
    let root = env.create("oncall", "work/a");
    env.ok(&["create", &format!("{root}/helper")]);
    let helper = env.member(&root, "helper");
    env.ok(&["stop", &format!("{root}/{helper}")]);
    env.ok(&["delete", &format!("{root}/{helper}")]);
    // A kept member's folder is found where its start recorded it.
    env.ok(&["create", &format!("{root}/helper")]);
    let started = env.member(&root, "helper");
    fs::rename(env.path("teams/oncall"), env.path("teams-moved")).unwrap();

    let out = env.brg(&["reset", &root]);
    let text = both(&out);

    assert!(!out.status.success(), "{text}");
    assert!(text.contains("remove any left empty by hand"), "{text}");
    assert!(
        !text.contains("brg start"),
        "advice that can't work: {text}"
    );
    assert!(registered(&env).is_empty(), "{text}");
    assert!(env.path(&format!("work/a/copies/{helper}")).is_dir());
    assert!(
        !env.path(&format!("work/a/copies/{started}")).exists(),
        "{text}"
    );
}
