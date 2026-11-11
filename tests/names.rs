//! `create <instance>/<role> --name`: member and copy IDs that say what
//! each one is for.
//!
//! Run with `cargo test --features test-support`.
#![cfg(feature = "test-support")]

mod common;
mod instance_env;

use std::fs;

use instance_env::*;

/// The test team with `helper`, an agent role, and `dev`, a team role,
/// each in its own worktree.
const TEAM: &str = r#"
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
    team: dev
    parent: lead
    start: false
    max: 2
    worktree: true
"#;

fn names_env() -> (Env, String) {
    let env = Env::new();
    env.write("teams/oncall/team.yaml", TEAM);
    env.team("teams/dev");
    env.write(
        "teams/dev/team.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md}\n",
    );
    fs::remove_dir(env.path("work/a")).unwrap();
    env.checkout("work/a");
    let inst = env.create("oncall", "work/a");
    (env, inst)
}

#[test]
fn a_name_is_the_id_of_the_member_or_copy_and_is_never_reissued() {
    let (env, inst) = names_env();
    let member = format!("{inst}/helper-rounding-fix");
    let out = stdout(&env.ok(&[
        "create",
        &format!("{inst}/helper"),
        "--name",
        "rounding-fix",
    ]));
    let line = format!("{member}  folder {}", env.worktree(&member).display());
    assert!(out.lines().any(|l| l == line), "{out}");
    assert!(env.worktree(&member).join(".git").is_file());
    env.wait_for("named member started", || {
        !env.starts("helper-rounding-fix").is_empty()
    });

    let copy = format!("{inst}/dev-make-install");
    let out = stdout(&env.ok(&["create", &format!("{inst}/dev"), "--name", "make-install"]));
    assert!(out.starts_with(&copy), "{out}");
    assert!(env.worktree(&copy).join(".git").is_file());
    let tree = stdout(&env.ok(&["status", &inst]));
    assert!(
        tree.contains("helper-rounding-fix") && tree.contains("dev-make-install"),
        "{tree}"
    );

    // Deleted, the IDs stay issued, and cleanup still knows the worktree
    // `delete --force` leaves as brg's own.
    env.ok(&["stop", &member]);
    fs::write(env.worktree(&member).join("stray.txt"), "x").unwrap();
    env.ok(&["delete", &member, "--force"]);
    fs::remove_file(env.worktree(&member).join("stray.txt")).unwrap();
    env.ok(&["delete", &copy]);
    for (role, name) in [("helper", "rounding-fix"), ("dev", "make-install")] {
        let err = env.fails(&["create", &format!("{inst}/{role}"), "--name", name]);
        assert!(err.contains("issued before"), "{err}");
    }
    let out = stdout(&env.ok(&["cleanup", &inst]));
    assert!(!out.contains("skipped"), "{out}");
    assert!(!env.worktree(&member).exists(), "{out}");
    env.ok(&["stop", &inst]);
}

#[test]
fn names_that_do_not_fit_are_refused_before_anything_is_issued() {
    let (env, inst) = names_env();
    let before = env.registry();
    for name in ["Rounding", "rounding_fix", "fix-", &"a".repeat(25)] {
        let err = env.fails(&["create", &format!("{inst}/helper"), "--name", name]);
        assert!(err.contains("at most 24 characters"), "{name}: {err}");
    }
    let err = env.fails(&["create", "oncall", "work/b", "--name", "widgets"]);
    assert!(err.contains("<instance>/<role> --name"), "{err}");
    assert_eq!(env.registry(), before);
    assert!(
        env.rows(&inst)
            .iter()
            .all(|r| r["role"] != "helper" || r["member"].is_null())
    );
    env.ok(&["stop", &inst]);
}
