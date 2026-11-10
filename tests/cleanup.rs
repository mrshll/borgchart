//! `brg cleanup`: what it removes for deleted members and copies, what it
//! leaves, and where it stops.
//!
//! Run with `cargo test --features test-support`.
#![cfg(feature = "test-support")]

mod common;

mod instance_env;

use std::fs;

use instance_env::*;

/// The test team with `helper` working in `copies/{member_id}` and `dev`,
/// a team role whose copies run the `dev` team in `dev/{member_id}`.
const TEAM_WITH_LEAVES: &str = r#"
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
    max: 4
    cwd: "copies/{member_id}"
  scout:
    prompt: scout.md
    start: false
    cwd: lead
  dev:
    team: dev
    parent: lead
    start: false
    cwd: "dev/{member_id}"
"#;

fn leaves_env() -> Env {
    let env = Env::new();
    env.write("teams/oncall/team.yaml", TEAM_WITH_LEAVES);
    env.team("teams/dev");
    env.write(
        "teams/dev/team.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md, cwd: repo}\n",
    );
    for folder in ["work/a", "work/b"] {
        fs::create_dir_all(env.path(&format!("{folder}/lead"))).unwrap();
        fs::create_dir_all(env.path(&format!("{folder}/copies"))).unwrap();
    }
    env
}

/// A new helper of `inst`, by ID.
fn helper(env: &Env, inst: &str) -> String {
    let before: Vec<String> = helpers(env, inst);
    env.ok(&["create", &format!("{inst}/helper")]);
    helpers(env, inst)
        .into_iter()
        .find(|id| !before.contains(id))
        .unwrap()
}

fn helpers(env: &Env, inst: &str) -> Vec<String> {
    env.rows(inst)
        .iter()
        .filter(|r| r["role"] == "helper")
        .filter_map(|r| r["member"].as_str().map(str::to_string))
        .collect()
}

fn delete_member(env: &Env, inst: &str, id: &str) {
    env.ok(&["stop", &format!("{inst}/{id}")]);
    env.ok(&["delete", &format!("{inst}/{id}")]);
}

/// A stopped dev copy of `inst`, deleted again: its ID and folder.
fn deleted_copy(env: &Env, inst: &str, folder: &str) -> (String, std::path::PathBuf) {
    let out = env.ok(&["create", &format!("{inst}/dev")]);
    let copy = stdout(&out).split_whitespace().next().unwrap().to_string();
    let leaf = env.path(&format!(
        "{folder}/dev/{}",
        copy.rsplit('/').next().unwrap()
    ));
    assert!(leaf.is_dir());
    env.ok(&["delete", &copy]);
    (copy, leaf)
}

#[test]
fn cleanup_removes_empty_folders_of_deleted_members_and_copies_and_lists_the_rest() {
    let env = leaves_env();
    let inst = env.create("oncall", "work/a");
    let [empty, with_file, with_worktree, kept] = std::array::from_fn(|_| helper(&env, &inst));
    let leaf = |id: &str| env.path(&format!("work/a/copies/{id}"));
    fs::create_dir_all(leaf(&empty).join("nested/empty")).unwrap();
    fs::create_dir_all(leaf(&with_file).join("nested")).unwrap();
    fs::write(leaf(&with_file).join("nested/notes.txt"), "keep me").unwrap();
    fs::write(leaf(&with_worktree).join(".git"), "gitdir: elsewhere\n").unwrap();
    for id in [&empty, &with_file, &with_worktree] {
        delete_member(&env, &inst, id);
    }
    // The case that prompted cleanup: a deleted copy whose lead made an
    // empty `copies/` inside its folder.
    let (_, copy) = deleted_copy(&env, &inst, "work/a");
    fs::create_dir(copy.join("copies")).unwrap();
    fs::create_dir(env.path("work/a/copies/not-brgs")).unwrap();

    let out = stdout(&env.ok(&["cleanup"]));

    assert!(!leaf(&empty).exists(), "{out}");
    assert!(!copy.exists(), "{out}");
    assert_eq!(
        fs::read_to_string(leaf(&with_file).join("nested/notes.txt")).unwrap(),
        "keep me"
    );
    assert!(
        out.contains(&format!("left {}: holds files", leaf(&with_file).display())),
        "{out}"
    );
    assert!(leaf(&with_worktree).join(".git").is_file());
    assert!(
        out.contains(&format!(
            "left {}: holds a git worktree",
            leaf(&with_worktree).display()
        )),
        "{out}"
    );
    assert!(
        leaf(&kept).is_dir(),
        "a kept member's empty folder went: {out}"
    );
    assert!(env.path("work/a/copies/not-brgs").is_dir());
    assert!(env.path("work/a/lead").is_dir());
    env.ok(&["stop", &inst]);
}

#[test]
fn cleanup_removes_leftover_state_and_nothing_else_in_the_state_folder() {
    let env = leaves_env();
    let inst = env.create("oncall", "work/a");
    let member = helper(&env, &inst);
    delete_member(&env, &inst, &member);
    let (copy, _) = deleted_copy(&env, &inst, "work/a");
    let gone = env.create("oncall", "work/b");
    env.ok(&["stop", &gone]);
    env.ok(&["delete", &gone]);
    // What an interrupted delete leaves behind.
    let state = env.path("state");
    let leftovers = [
        state.join(&inst).join("members").join(&member),
        state.join(&copy).join("notes"),
        state.join(&gone).join("notes"),
    ];
    for dir in &leftovers {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("x"), "x").unwrap();
    }
    // Not brg's: other tools' folders, and an ID brg never issued.
    let strangers = [
        state.join("cargo-slots"),
        state.join("notes"),
        state.join("other-team-k3f9"),
    ];
    for dir in &strangers {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("x"), "x").unwrap();
    }

    // With an instance, the unregistered instance's state stays.
    env.ok(&["cleanup", &inst]);
    assert!(!leftovers[0].exists());
    assert!(!state.join(&copy).exists());
    assert!(leftovers[2].exists());

    env.ok(&["cleanup"]);
    assert!(!state.join(&gone).exists());
    for dir in &strangers {
        assert!(dir.join("x").is_file(), "{} went", dir.display());
    }
    assert!(state.join(&inst).join("instance.json").is_file());
    env.ok(&["stop", &inst]);
}

#[test]
fn cleanup_of_an_instance_touches_nothing_outside_it() {
    let env = leaves_env();
    let a = env.create("oncall", "work/a");
    let b = env.create("oncall", "work/b");
    let in_a = helper(&env, &a);
    let in_b = helper(&env, &b);
    delete_member(&env, &a, &in_a);
    delete_member(&env, &b, &in_b);

    env.ok(&["cleanup", &a]);

    assert!(!env.path(&format!("work/a/copies/{in_a}")).exists());
    assert!(env.path(&format!("work/b/copies/{in_b}")).is_dir());
    env.ok(&["stop", &a]);
    env.ok(&["stop", &b]);
}

#[test]
fn cleanup_keeps_leaves_another_instance_works_in_or_that_lead_outside() {
    let env = leaves_env();
    let a = env.create("oncall", "work/a");
    let [reused, linked] = std::array::from_fn(|_| helper(&env, &a));
    delete_member(&env, &a, &reused);
    delete_member(&env, &a, &linked);
    // Another instance works in a deleted member's empty folder.
    let reused_leaf = env.path(&format!("work/a/copies/{reused}"));
    fs::create_dir(reused_leaf.join("lead")).unwrap();
    let b = env.create("oncall", &format!("work/a/copies/{reused}"));
    // A deleted member's folder replaced by a link to empty folders elsewhere.
    let linked_leaf = env.path(&format!("work/a/copies/{linked}"));
    fs::remove_dir(&linked_leaf).unwrap();
    fs::create_dir_all(env.path("outside/linked/empty")).unwrap();
    std::os::unix::fs::symlink(env.path("outside/linked"), &linked_leaf).unwrap();
    // An instance whose `copies/` links elsewhere, so its leaves lie outside.
    let c = env.create("oncall", "work/b");
    let moved = helper(&env, &c);
    delete_member(&env, &c, &moved);
    fs::rename(env.path("work/b/copies"), env.path("outside/copies")).unwrap();
    std::os::unix::fs::symlink(env.path("outside/copies"), env.path("work/b/copies")).unwrap();

    let out = stdout(&env.ok(&["cleanup"]));

    assert!(reused_leaf.join("lead").is_dir(), "{out}");
    assert!(env.path("outside/linked/empty").is_dir(), "{out}");
    assert!(linked_leaf.symlink_metadata().unwrap().is_symlink());
    assert!(
        env.path(&format!("outside/copies/{moved}")).is_dir(),
        "{out}"
    );
    for inst in [&b, &a, &c] {
        env.ok(&["stop", inst]);
    }
}
