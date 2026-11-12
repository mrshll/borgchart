//! `brg destroy <root>`: stop a whole tree and forget it, so `brg create`
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

/// The test team with `helper` and `dev`, a team role whose child teams run the
/// `dev` team, each in its own worktree.
const TREE: &str = r#"
bus: {}
roles:
  lead:
    prompt: lead.md
  helper:
    prompt: helper.md
    vendor: codex
    parent: lead
    start: false
    max: 2
    worktree: true
  dev:
    template: dev
    parent: lead
    start: false
    max: 2
    worktree: true
"#;

fn tree_env() -> Env {
    let env = Env::new();
    env.write("templates/oncall/template.yaml", TREE);
    env.team("templates/dev");
    env.write(
        "templates/dev/template.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md}\n",
    );
    fs::remove_dir(env.path("work/a")).unwrap();
    env.checkout("work/a");
    env
}

/// A started dev team of `root`: its ID and folder.
fn started_copy(env: &Env, root: &str) -> (String, std::path::PathBuf) {
    let out = env.ok(&["create", &format!("{root}/dev")]);
    let copy = stdout(&out).split_whitespace().next().unwrap().to_string();
    let folder = env.worktree(&copy);
    env.ok(&["start", &copy]);
    env.wait_for("child team lead", || !env.member(&copy, "lead").is_empty());
    (copy, folder)
}

fn registered(env: &Env) -> Vec<String> {
    let registry: Value =
        serde_json::from_slice(&fs::read(env.path("state/registry.json")).unwrap()).unwrap();
    registry["teams"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect()
}

#[test]
fn destroy_stops_and_forgets_a_tree_so_create_starts_it_fresh() {
    let env = tree_env();
    let root = env.create("oncall", "work/a");
    let lead = env.member(&root, "lead");
    env.ok(&["create", &format!("{root}/helper")]);
    let helper = env.member(&root, "helper");
    let (copy, copy_folder) = started_copy(&env, &root);
    // Open work that `delete` would refuse without --force.
    let sent = env.publish(&root, &json!({"to": lead, "body": "unread"}));
    assert!(sent.status.success(), "{}", both(&sent));
    fs::write(env.path("work/a/kept.txt"), "kept").unwrap();
    fs::write(copy_folder.join("kept.txt"), "kept").unwrap();
    // Another tree working in a worktree brg made for this tree.
    let out = env.ok(&["create", &format!("{root}/dev")]);
    let spare = stdout(&out).split_whitespace().next().unwrap().to_string();
    let spare_folder = env.worktree(&spare);
    let other = stdout(&env.ok(&["create", "oncall", &spare_folder.display().to_string()]))
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();

    let out = env.ok(&["destroy", &root]);
    let text = stdout(&out);

    assert!(text.starts_with(&format!("destroying {root}:")), "{text}");
    assert_eq!(registered(&env), std::slice::from_ref(&other), "{text}");
    assert!(!env.path(&format!("state/{root}")).exists(), "{text}");
    let sessions = env.tmux(&["list-sessions", "-F", "#{session_name}"]);
    assert!(!sessions.contains(&root), "still running: {sessions}");
    assert_eq!(
        fs::read_to_string(env.path("work/a/kept.txt")).unwrap(),
        "kept"
    );
    assert_eq!(
        fs::read_to_string(copy_folder.join("kept.txt")).unwrap(),
        "kept"
    );
    assert!(
        !env.worktree(&format!("{root}/{helper}")).exists(),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "left {}: git worktree with uncommitted",
            copy_folder.display()
        )),
        "{text}"
    );
    assert!(spare_folder.join(".git").is_file(), "{text}");
    assert!(
        text.contains(&format!("left {}: a team", spare_folder.display())),
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
fn destroy_refuses_members_copies_and_trees_it_cannot_confirm_stopped() {
    let env = tree_env();
    let root = env.create("oncall", "work/a");
    let lead = env.member(&root, "lead");
    let (copy, _) = started_copy(&env, &root);
    let before = registered(&env);

    // Inside a member: destroy would stop its own caller.
    env.wait_for("lead start record", || !env.starts(&lead).is_empty());
    let vars: BTreeMap<String, String> =
        serde_json::from_value(env.starts(&lead)[0]["env"].clone()).unwrap();
    let inside = common::command(env!("CARGO_BIN_EXE_brg"))
        .env_clear()
        .envs(vars)
        .args(["destroy", &root])
        .output()
        .unwrap();
    assert!(!inside.status.success(), "{}", both(&inside));
    assert!(
        both(&inside).contains("your own shell"),
        "{}",
        both(&inside)
    );

    // A child team is not a tree's root.
    assert!(env.fails(&["destroy", &copy]).contains("root"));

    // A member whose stop brg can't confirm.
    let copy_lead = env.member(&copy, "lead");
    let incarnation = env.path(&format!(
        "state/{copy}/members/{copy_lead}/incarnation.json"
    ));
    let original = fs::read(&incarnation).unwrap();
    fs::write(&incarnation, "unreadable start record").unwrap();
    env.tmux(&["new-session", "-d", "-s", "sentinel", "sleep", "60"]);
    // Destroy's stop is what refuses here, naming the member it can't confirm.
    let refused = env.fails(&["destroy", &root]);
    assert!(refused.contains(&copy_lead), "{refused}");

    assert_eq!(registered(&env), before);
    assert!(env.path(&format!("state/{root}/team.json")).is_file());
    fs::write(incarnation, original).unwrap();
    env.ok(&["stop", &root]);
    env.tmux(&["kill-session", "-t", "=sentinel"]);
}

#[test]
fn destroy_removes_clean_worktrees_in_the_tree_and_keeps_the_root_checkout() {
    let env = tree_env();
    let lead = env.path("work/a");
    let root = env.create("oncall", "work/a");
    // A child team whose worktree is clean.
    let (_, copy_folder) = started_copy(&env, &root);
    // A helper whose worktree holds untracked work.
    env.ok(&["create", &format!("{root}/helper")]);
    let helper = env.member(&root, "helper");
    let busy = env.worktree(&format!("{root}/{helper}"));
    fs::write(busy.join("draft.txt"), "work").unwrap();

    let text = stdout(&env.ok(&["destroy", &root]));

    assert!(!copy_folder.exists(), "{text}");
    assert!(busy.join("draft.txt").is_file(), "{text}");
    assert!(
        text.contains(&format!(
            "left {}: git worktree with uncommitted",
            busy.display()
        )),
        "{text}"
    );
    let worktrees = git(&lead, &["worktree", "list"]);
    assert!(
        !worktrees.contains(&copy_folder.display().to_string()),
        "{worktrees}"
    );
    assert!(lead.join(".git").is_dir());
    assert_eq!(fs::read_to_string(lead.join("README")).unwrap(), "first\n");
}
