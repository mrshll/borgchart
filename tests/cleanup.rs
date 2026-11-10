//! `brg cleanup`: what it removes for deleted members and copies, what it
//! leaves, and where it stops.
//!
//! Run with `cargo test --features test-support`.
#![cfg(feature = "test-support")]

mod common;

mod instance_env;

use std::fs;

use instance_env::*;

/// The test team with `helper` and `dev`, a team role whose copies run
/// the `dev` team, each in its own worktree.
const TEAM_WITH_WORKTREES: &str = r#"
bus: {}
roles:
  lead:
    prompt: lead.md
  helper:
    prompt: helper.md
    vendor: codex
    parent: lead
    start: false
    max: 8
    worktree: true
  scout:
    prompt: scout.md
    start: false
  dev:
    team: dev
    parent: lead
    start: false
    worktree: true
"#;

fn worktrees_env() -> Env {
    let env = Env::new();
    env.write("teams/oncall/team.yaml", TEAM_WITH_WORKTREES);
    env.team("teams/dev");
    env.write(
        "teams/dev/team.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md}\n",
    );
    for folder in ["work/a", "work/b"] {
        fs::remove_dir(env.path(folder)).unwrap();
        env.checkout(folder);
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

/// Delete a member with `--force`, which leaves a worktree with work in it.
fn delete_member(env: &Env, inst: &str, id: &str) {
    env.ok(&["stop", &format!("{inst}/{id}")]);
    env.ok(&["delete", &format!("{inst}/{id}"), "--force"]);
}

/// A member deleted while its worktree held a stray file, which is then
/// removed: the clean worktree `delete --force` leaves for cleanup.
fn left_clean(env: &Env, inst: &str) -> (String, std::path::PathBuf) {
    let id = helper(env, inst);
    let leaf = env.worktree(&format!("{inst}/{id}"));
    fs::write(leaf.join("stray.txt"), "x").unwrap();
    delete_member(env, inst, &id);
    fs::remove_file(leaf.join("stray.txt")).unwrap();
    (id, leaf)
}

/// A stopped dev copy of `inst`, deleted again: its ID.
fn deleted_copy(env: &Env, inst: &str) -> String {
    let out = env.ok(&["create", &format!("{inst}/dev")]);
    let copy = stdout(&out).split_whitespace().next().unwrap().to_string();
    assert!(env.worktree(&copy).is_dir());
    env.ok(&["delete", &copy]);
    assert!(!env.worktree(&copy).exists());
    copy
}

#[test]
fn cleanup_removes_clean_worktrees_and_keeps_any_with_work() {
    let env = worktrees_env();
    let lead = env.path("work/a");
    let inst = env.create("oncall", "work/a");
    let (clean, clean_leaf) = left_clean(&env, &inst);
    let kept = helper(&env, &inst);
    let [untracked, modified, unpushed, moved, skipped, nested] =
        std::array::from_fn(|_| helper(&env, &inst));
    let leaf = |id: &str| env.worktree(&format!("{inst}/{id}"));
    fs::write(leaf(&untracked).join("new.txt"), "work").unwrap();
    fs::write(leaf(&modified).join("README"), "changed\n").unwrap();
    fs::write(leaf(&unpushed).join("README"), "committed\n").unwrap();
    git(&leaf(&unpushed), &["commit", "-q", "-am", "local only"]);
    // A commit only the worktree's HEAD reflog still reaches.
    fs::write(leaf(&moved).join("README"), "moved away\n").unwrap();
    git(&leaf(&moved), &["commit", "-q", "-am", "moved away from"]);
    git(
        &leaf(&moved),
        &["checkout", "-q", "--detach", "origin/main"],
    );
    // An edit git is told not to look at.
    git(
        &leaf(&skipped),
        &["update-index", "--skip-worktree", "README"],
    );
    fs::write(leaf(&skipped).join("README"), "hidden edit\n").unwrap();
    // A clone with local work in an ignored folder of a clean worktree.
    fs::write(lead.join(".git/info/exclude"), "scratch/\n").unwrap();
    let inner = leaf(&nested).join("scratch/clone");
    fs::create_dir_all(&inner).unwrap();
    git(&inner, &["init", "-q"]);
    fs::write(inner.join("work.txt"), "work").unwrap();
    git(&inner, &["add", "work.txt"]);
    git(&inner, &["commit", "-q", "-m", "inner"]);
    for id in [&untracked, &modified, &unpushed, &moved, &skipped, &nested] {
        delete_member(&env, &inst, id);
    }

    // The caller's git settings, pointing at the root's checkout, are ignored.
    let out = env
        .cmd(&["cleanup"])
        .env("GIT_DIR", lead.join(".git"))
        .env("GIT_WORK_TREE", &lead)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    let out = stdout(&out);

    assert!(!clean_leaf.exists(), "{out}");
    assert!(!git(&lead, &["worktree", "list"]).contains(&clean), "{out}");
    for (id, why) in [
        (&untracked, "uncommitted or untracked changes"),
        (&modified, "uncommitted or untracked changes"),
        (&unpushed, "1 commit(s) on no remote"),
        (&moved, "1 commit(s) on no remote"),
        (&skipped, "files git is told to skip or assume unchanged"),
        (&nested, "another repository or worktree inside it"),
    ] {
        assert!(leaf(id).join(".git").is_file(), "{id} went: {out}");
        assert!(
            out.contains(&format!(
                "left {}: git worktree with {why}",
                leaf(id).display()
            )),
            "{out}"
        );
    }
    assert_eq!(
        fs::read_to_string(leaf(&untracked).join("new.txt")).unwrap(),
        "work"
    );
    assert!(inner.join("work.txt").is_file());
    assert!(
        leaf(&kept).join(".git").is_file(),
        "a kept member's worktree went: {out}"
    );
    // The root's own checkout is never touched.
    assert!(lead.join(".git").is_dir());
    assert_eq!(fs::read_to_string(lead.join("README")).unwrap(), "first\n");
    env.ok(&["stop", &inst]);
}

#[test]
fn cleanup_removes_leftover_state_and_nothing_else_in_the_state_folder() {
    let env = worktrees_env();
    let inst = env.create("oncall", "work/a");
    let member = helper(&env, &inst);
    env.ok(&["stop", &format!("{inst}/{member}")]);
    env.ok(&["delete", &format!("{inst}/{member}")]);
    let copy = deleted_copy(&env, &inst);
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
    let env = worktrees_env();
    let a = env.create("oncall", "work/a");
    let b = env.create("oncall", "work/b");
    let (_, in_a) = left_clean(&env, &a);
    let (_, in_b) = left_clean(&env, &b);

    env.ok(&["cleanup", &a]);

    assert!(!in_a.exists());
    assert!(in_b.join(".git").is_file());
    env.ok(&["stop", &a]);
    env.ok(&["stop", &b]);
}

#[test]
fn cleanup_keeps_a_worktree_another_instance_works_in() {
    let env = worktrees_env();
    let a = env.create("oncall", "work/a");
    let (_, leaf) = left_clean(&env, &a);
    let b = env
        .ok(&["create", "oncall", &leaf.display().to_string()])
        .stdout;
    let b = String::from_utf8_lossy(&b)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();

    let out = stdout(&env.ok(&["cleanup"]));

    assert!(leaf.join(".git").is_file(), "{out}");
    assert!(out.contains("works there"), "{out}");
    for inst in [&b, &a] {
        env.ok(&["stop", inst]);
    }
}

#[test]
fn cleanup_fails_and_keeps_a_worktree_git_cannot_check() {
    let env = worktrees_env();
    let inst = env.create("oncall", "work/a");
    let broken = helper(&env, &inst);
    let leaf = env.worktree(&format!("{inst}/{broken}"));
    fs::write(leaf.join(".git"), "gitdir: nowhere\n").unwrap();
    env.ok(&["stop", &format!("{inst}/{broken}")]);
    let err = env.fails(&["delete", &format!("{inst}/{broken}")]);
    assert!(err.contains("git can't check"), "{err}");
    delete_member(&env, &inst, &broken);

    let out = env.brg(&["cleanup"]);

    assert!(!out.status.success(), "{}", both(&out));
    assert!(
        both(&out).contains("can't check git worktree"),
        "{}",
        both(&out)
    );
    assert!(leaf.join(".git").is_file());
    env.ok(&["stop", &inst]);
}
