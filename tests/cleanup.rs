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
    let [empty, with_file, with_repository, kept] = std::array::from_fn(|_| helper(&env, &inst));
    let leaf = |id: &str| env.path(&format!("work/a/copies/{id}"));
    fs::create_dir_all(leaf(&empty).join("nested/empty")).unwrap();
    fs::create_dir_all(leaf(&with_file).join("nested")).unwrap();
    fs::write(leaf(&with_file).join("nested/notes.txt"), "keep me").unwrap();
    fs::create_dir(leaf(&with_repository).join(".git")).unwrap();
    for id in [&empty, &with_file, &with_repository] {
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
    assert!(leaf(&with_repository).join(".git").is_dir());
    assert!(
        out.contains(&format!(
            "left {}: holds a git repository",
            leaf(&with_repository).display()
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

/// Run git in `dir`, away from the caller's repository and settings.
fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = common::command("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(args)
        .env("HOME", dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `work/a/lead` as the root's checkout: a clone of a local remote with
/// one pushed commit.
fn root_checkout(env: &Env) -> std::path::PathBuf {
    let origin = env.path("origin.git");
    git(
        env.dir.path(),
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            &origin.display().to_string(),
        ],
    );
    let lead = env.path("work/a/lead");
    git(
        env.dir.path(),
        &[
            "clone",
            "-q",
            &origin.display().to_string(),
            &lead.display().to_string(),
        ],
    );
    fs::write(lead.join("README"), "hello\n").unwrap();
    git(&lead, &["add", "README"]);
    git(&lead, &["commit", "-q", "-m", "first"]);
    git(&lead, &["push", "-q", "origin", "main"]);
    lead
}

#[test]
fn cleanup_removes_clean_worktrees_and_keeps_any_with_work() {
    let env = leaves_env();
    let lead = root_checkout(&env);
    let inst = env.create("oncall", "work/a");
    let [clean, untracked, modified, unpushed] = std::array::from_fn(|_| helper(&env, &inst));
    let leaf = |id: &str| env.path(&format!("work/a/copies/{id}"));
    for id in [&clean, &untracked, &modified, &unpushed] {
        git(
            &lead,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                &leaf(id).display().to_string(),
                "main",
            ],
        );
    }
    fs::write(leaf(&untracked).join("new.txt"), "work").unwrap();
    fs::write(leaf(&modified).join("README"), "changed\n").unwrap();
    fs::write(leaf(&unpushed).join("README"), "committed\n").unwrap();
    git(&leaf(&unpushed), &["commit", "-q", "-am", "local only"]);
    for id in [&clean, &untracked, &modified, &unpushed] {
        delete_member(&env, &inst, id);
    }

    let out = stdout(&env.ok(&["cleanup"]));

    assert!(!leaf(&clean).exists(), "{out}");
    assert!(!git(&lead, &["worktree", "list"]).contains(&clean), "{out}");
    for (id, why) in [
        (&untracked, "uncommitted or untracked changes"),
        (&modified, "uncommitted or untracked changes"),
        (&unpushed, "1 commit(s) on no remote"),
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
    // The root's own checkout is never touched.
    assert!(lead.join(".git").is_dir());
    assert_eq!(fs::read_to_string(lead.join("README")).unwrap(), "hello\n");
    env.ok(&["stop", &inst]);
}

#[test]
fn cleanup_fails_and_keeps_a_worktree_git_cannot_check() {
    let env = leaves_env();
    let inst = env.create("oncall", "work/a");
    let broken = helper(&env, &inst);
    let leaf = env.path(&format!("work/a/copies/{broken}"));
    fs::write(leaf.join(".git"), "gitdir: nowhere\n").unwrap();
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
