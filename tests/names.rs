//! Named team, member and child team IDs and their lifetime.
//!
//! Run with `cargo test --features test-support`.
#![cfg(feature = "test-support")]

mod common;
mod team_env;

use std::fs;

use team_env::*;

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
    template: dev
    parent: lead
    start: false
    max: 2
    worktree: true
"#;

fn names_env() -> (Env, String) {
    let env = Env::new();
    env.write("templates/oncall/template.yaml", TEAM);
    env.team("templates/dev");
    env.write(
        "templates/dev/template.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md}\n",
    );
    fs::remove_dir(env.path("work/a")).unwrap();
    env.checkout("work/a");
    let inst = env.create("oncall", "work/a");
    (env, inst)
}

#[test]
fn a_name_is_the_id_of_the_member_or_child_team_and_is_never_reissued() {
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
    let err = env.fails(&["create", &format!("{inst}/helper"), "--name", "Rounding"]);
    assert!(err.contains("lowercase"), "{err}");
    assert_eq!(env.registry(), before);
    assert!(
        env.rows(&inst)
            .iter()
            .all(|r| r["role"] != "helper" || r["member"].is_null())
    );
    env.ok(&["stop", &inst]);
}

#[test]
fn a_named_child_team_whose_worktree_fails_keeps_its_name_free() {
    let (env, inst) = names_env();
    git(&env.path("work/a"), &["branch", "fix-rounding"]);
    let dev = format!("{inst}/dev");
    let err = env.fails(&[
        "create",
        &dev,
        "--name",
        "rounding-fix",
        "--branch",
        "fix-rounding",
    ]);
    assert!(err.contains("needs its own worktree"), "{err}");
    env.ok(&[
        "create",
        &dev,
        "--name",
        "rounding-fix",
        "--branch",
        "fix-rounding-2",
    ]);
    assert!(
        env.worktree(&format!("{dev}-rounding-fix"))
            .join(".git")
            .is_file()
    );
    env.ok(&["stop", &inst]);
}

#[test]
fn named_team_survives_restart_and_cleanup_recognizes_only_its_issued_id() {
    let env = Env::new();
    let id = "oncall-north-west";
    let out = stdout(&env.ok(&["create", "oncall", "work/a", "--name", "north-west"]));
    assert_eq!(out.split_whitespace().next(), Some(id));
    env.ok(&["stop", id]);
    env.ok(&["start", id]);
    env.wait_for("restarted lead", || env.state_of(id, "lead") == "idle");
    env.ok(&["stop", id]);
    env.ok(&["delete", id]);
    let leftover = env.path(&format!("state/{id}"));
    let unknown = env.path("state/oncall-south-ridge");
    fs::create_dir(&leftover).unwrap();
    fs::create_dir(&unknown).unwrap();
    env.fails(&["create", "oncall", "work/a", "--name", "south-ridge"]);
    env.ok(&["cleanup"]);
    assert!(!leftover.exists());
    assert!(unknown.is_dir());
}

#[test]
fn team_names_cannot_reissue_or_disguise_an_issued_team_id() {
    let env = Env::new();
    env.team("templates/oncall-north");
    env.team("templates/a-long-template-for-a-team");
    let id = "oncall-north-ridge";
    env.ok(&["create", "oncall", "work/a", "--name", "north-ridge"]);
    for deleted in [false, true] {
        if deleted {
            env.ok(&["stop", id]);
            env.ok(&["delete", id]);
        }
        let before = env.registry();
        for (template, name) in [
            ("oncall", "north-ridge"),
            ("oncall-north", "ridge"),
            ("oncall-north", "north-ridge"),
            ("a-long-template-for-a-team", "a-long-team-name"),
            ("oncall", "a-name-longer-than-the-limit"),
            ("oncall", "North"),
        ] {
            env.fails(&["create", template, "work/b", "--name", name]);
        }
        assert_eq!(env.registry(), before);
    }
    let random = env.create("oncall", "work/b");
    env.ok(&["stop", &random]);
}

#[test]
fn a_random_draw_skips_a_deleted_team_with_a_chosen_four_character_name() {
    let env = Env::new();
    let seeded_create = |env: &Env| {
        let out = env
            .cmd(&["create", "oncall", "work/a"])
            .env("BORGCHART_SUFFIX_SEED", "7")
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", both(&out));
        stdout(&out).split_whitespace().next().unwrap().to_owned()
    };
    // Learn the first draw in an independent registry, so it is still
    // available to choose by name in the registry under test.
    let control = seeded_create(&env);
    let name = control.strip_prefix("oncall-").unwrap();
    env.ok(&["stop", &control]);
    let named_env = Env::new();
    let id = format!("oncall-{name}");
    let out = stdout(&named_env.ok(&["create", "oncall", "work/a", "--name", name]));
    assert_eq!(out.split_whitespace().next(), Some(id.as_str()));
    named_env.ok(&["stop", &id]);
    named_env.ok(&["delete", &id]);
    let next = seeded_create(&named_env);
    assert!(well_formed(&next, "oncall"));
    assert_ne!(next, id);
    let leftover = named_env.path(&format!("state/{id}"));
    fs::create_dir(&leftover).unwrap();
    named_env.ok(&["cleanup"]);
    assert!(!leftover.exists());
    named_env.ok(&["stop", &next]);
}
