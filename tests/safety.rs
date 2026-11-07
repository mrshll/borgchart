//! Safety around instances: checkouts held across instances, old sessions,
//! brg's own state folder, changed files and storage that can't be read.
//!
//! Run with `cargo test --features test-support`.
#![cfg(feature = "test-support")]

mod common;
mod instance_env;

use std::fs;

use instance_env::*;
use serde_json::json;

fn first_word(text: &str) -> String {
    text.split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}

impl Env {
    fn folder(&self, rel: &str) -> String {
        self.path(rel).display().to_string()
    }
}

// --- 6. Checkouts ---

#[test]
fn a_folder_held_by_another_instance_is_refused() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    // While A runs.
    let err = env.fails(&["create", "oncall", &env.folder("work/a")]);
    assert!(err.contains(&a) && err.contains("work/a"), "{err}");
    // While A is stopped with an open task naming the checkout.
    let lead = env.member(&a, "lead");
    env.assign(&a, &lead);
    env.ok(&["stop", &a]);
    let err = env.fails(&["create", "oncall", &env.folder("work/a")]);
    assert!(err.contains(&a) && err.contains("t1"), "{err}");
    // Once it's settled, by deleting A, the folder is free.
    env.ok(&["delete", &a, "--force"]);
    let b = env.create("oncall", "work/a");
    env.ok(&["stop", &b]);
}

#[test]
fn two_concurrent_creates_on_one_folder_one_wins() {
    let env = Env::new();
    let folder = env.folder("work/a");
    let children: Vec<_> = (0..2)
        .map(|_| {
            env.cmd(&["create", "oncall", &folder])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let outs: Vec<_> = children
        .into_iter()
        .map(|c| c.wait_with_output().unwrap())
        .collect();
    let won: Vec<_> = outs.iter().filter(|o| o.status.success()).collect();
    assert_eq!(
        won.len(),
        1,
        "{:?}",
        outs.iter().map(both).collect::<Vec<_>>()
    );
    let id = first_word(&stdout(won[0]));
    env.ok(&["stop", &id]);
}

// --- 11. Bypasses ---

#[test]
fn a_stopped_instance_cant_start_into_a_folder_another_holds() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    env.ok(&["stop", &a]);
    let b = env.create("oncall", "work/a");
    let err = env.fails(&["start", &a]);
    assert!(err.contains(&b), "{err}");
    env.ok(&["stop", &b]);
}

#[test]
fn symlinks_and_ancestors_alias_a_held_folder() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    std::os::unix::fs::symlink(env.path("work/a"), env.path("alias")).unwrap();
    let err = env.fails(&["create", "oncall", &env.folder("alias")]);
    assert!(err.contains(&a), "{err}");
    let err = env.fails(&["create", "oncall", &env.folder("work")]);
    assert!(err.contains(&a), "{err}");
    fs::create_dir_all(env.path("work/a/inner")).unwrap();
    let err = env.fails(&["create", "oncall", &env.folder("work/a/inner")]);
    assert!(err.contains(&a), "{err}");
    env.ok(&["stop", &a]);
}

#[test]
fn start_team_cant_give_a_role_a_cwd_inside_another_instances_folder() {
    let env = Env::new();
    fs::create_dir_all(env.path("work/a")).unwrap();
    fs::create_dir_all(env.path("work/b/a/sub")).unwrap();
    env.write(
        "teams/oncall/team.yaml",
        &TEAM.replace(
            "  lead:\n    prompt: lead.md\n",
            "  lead:\n    prompt: lead.md\n    cwd: a\n",
        ),
    );
    let a = env.create("oncall", "work");
    let b = env.create("oncall", "work/b");
    env.ok(&["stop", &a]);
    // The team moves, and its copy points the lead into B's folder.
    fs::rename(env.path("teams/oncall"), env.path("moved")).unwrap();
    fs::create_dir_all(env.path("elsewhere")).unwrap();
    fs::rename(env.path("moved"), env.path("elsewhere/oncall")).unwrap();
    env.write(
        "elsewhere/oncall/team.yaml",
        &TEAM.replace(
            "  lead:\n    prompt: lead.md\n",
            "  lead:\n    prompt: lead.md\n    cwd: b/a/sub\n",
        ),
    );
    let err = env.fails(&["start", &a, "--team", &env.folder("elsewhere/oncall")]);
    assert!(err.contains(&b), "{err}");
    env.ok(&["stop", &b]);
}

#[test]
fn unknown_liveness_keeps_the_reservation_and_blocks_delete() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    // brg can no longer reach the server that ran A.
    env.tmux(&["kill-server"]);
    let err = env.fails(&["create", "oncall", &env.folder("work/a")]);
    assert!(err.contains(&a), "{err}");
    let err = env.fails(&["delete", &a, "--force"]);
    assert!(err.contains("even with --force"), "{err}");
    assert!(env.path(&format!("state/{a}")).is_dir());
}

#[test]
fn a_live_old_session_refuses_every_start() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    env.ok(&["stop", &a]);
    env.tmux(&[
        "new-session",
        "-d",
        "-s",
        "brg-oldteam-0a1b2c3d",
        "sleep",
        "60",
    ]);
    let err = env.fails(&["start", &a]);
    assert!(err.contains("brg-oldteam-0a1b2c3d"), "{err}");
    let err = env.fails(&["create", "oncall", &env.folder("work/b")]);
    assert!(err.contains("brg-oldteam-0a1b2c3d"), "{err}");
    env.tmux(&["kill-session", "-t", "=brg-oldteam-0a1b2c3d"]);
    env.ok(&["start", &a]);
    env.ok(&["stop", &a]);
}

// --- 7. Moved folders ---

#[test]
fn a_moved_team_folder_is_re_pointed_after_a_full_stop() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    let lead = env.member(&a, "lead");
    let out = env.publish(
        &a,
        "note-1",
        &json!({"to": lead, "kind": "note", "body": "keep me"}),
    );
    assert!(out.status.success(), "{}", both(&out));
    fs::create_dir_all(env.path("elsewhere")).unwrap();
    fs::rename(env.path("teams/oncall"), env.path("elsewhere/oncall")).unwrap();
    let moved = env.folder("elsewhere/oncall");
    let err = env.fails(&["start", &a, "--team", &moved]);
    assert!(err.contains(&format!("brg stop {a}")), "{err}");
    env.ok(&["stop", &a]);
    let err = env.fails(&["start", &a]);
    assert!(err.contains("--team"), "{err}");
    env.ok(&["start", &a, "--team", &moved]);
    env.wait_for("lead idle", || env.state_of(&a, "lead") == "idle");
    assert_eq!(env.member(&a, "lead"), lead, "the same instance and member");
    let inbox = stdout(&env.ok(&["inbox", "--member", &lead, "--instance", &a, "--full"]));
    assert!(inbox.contains("keep me"), "{inbox}");
    env.ok(&["stop", &a]);
}

#[test]
fn a_removed_working_folder_refuses_start() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    env.ok(&["stop", &a]);
    fs::remove_dir_all(env.path("work/a")).unwrap();
    let err = env.fails(&["start", &a]);
    assert!(
        err.contains("working folder") && err.contains("gone"),
        "{err}"
    );
}

// --- 13. Storage safety ---

#[test]
fn a_start_that_orphans_open_work_is_refused() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    env.ok(&["create", &format!("{a}/scout")]);
    let scout = env.member(&a, "scout");
    env.assign(&a, &scout);
    env.ok(&["stop", &a]);
    env.write(
        "teams/oncall/team.yaml",
        &TEAM.replace("  scout:\n    prompt: scout.md\n    start: false\n", ""),
    );
    let err = env.fails(&["start", &a]);
    assert!(err.contains("t1") && err.contains(&scout), "{err}");
}

#[test]
fn a_rollover_with_open_work_is_refused() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    let lead = env.member(&a, "lead");
    env.assign(&a, &lead);
    env.ok(&["stop", &a]);
    env.write(
        "teams/oncall/lead.md",
        "---\nrole: lead\nmodels: {claude: opus high}\n---\nYou are the lead, changed.\n",
    );
    let err = env.fails(&["start", &a]);
    assert!(err.contains("open work") && err.contains("t1"), "{err}");
}

#[test]
fn unread_messages_are_open_work_for_delete() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    let lead = env.member(&a, "lead");
    let out = env.publish(
        &a,
        "note-1",
        &json!({"to": lead, "kind": "note", "body": "hi"}),
    );
    assert!(out.status.success(), "{}", both(&out));
    env.ok(&["stop", &a]);
    let err = env.fails(&["delete", &a]);
    assert!(err.contains("unread"), "{err}");
    env.ok(&["delete", &a, "--force"]);
}

#[test]
fn an_unreadable_store_still_stops_and_delete_needs_force() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    fs::write(
        env.path(&format!("state/{a}/messages/current")),
        "not a run\n",
    )
    .unwrap();
    env.ok(&["stop", &a]);
    let err = env.fails(&["start", &a]);
    assert!(err.contains("can't be read"), "{err}");
    let err = env.fails(&["delete", &a]);
    assert!(err.contains("can't be inspected"), "{err}");
    env.ok(&["delete", &a, "--force"]);
}

// --- 14. brg's state folder and the team folder ---

#[test]
fn a_working_folder_overlapping_brgs_state_is_refused() {
    let env = Env::new();
    // Around the state home, inside it, and through a symlink.
    fs::create_dir_all(env.path("state")).unwrap();
    let err = env.fails(&["create", "oncall", &env.folder("")]);
    assert!(err.contains("state"), "{err}");
    fs::create_dir_all(env.path("state/inside")).unwrap();
    let err = env.fails(&["create", "oncall", &env.folder("state/inside")]);
    assert!(err.contains("state"), "{err}");
    std::os::unix::fs::symlink(env.path("state"), env.path("work/link")).unwrap();
    let err = env.fails(&["create", "oncall", &env.folder("work/link")]);
    assert!(err.contains("state"), "{err}");
}

#[test]
fn locked_members_cant_work_around_the_team_folder_and_vendor_ones_get_a_note() {
    let env = Env::new();
    // `vendor` posture: a note.
    let out = env.ok(&["create", "oncall", &env.folder("teams")]);
    assert!(stdout(&out).contains("team folder"), "{}", both(&out));
    let a = first_word(&stdout(&out));
    env.ok(&["stop", &a]);
    env.ok(&["delete", &a]);
    // `locked`: refused (a team without a bus may have `locked` members).
    env.write(
        "teams/oncall/team.yaml",
        &TEAM.replace("bus: {}\n", "").replace(
            "  lead:\n    prompt: lead.md\n",
            "  lead:\n    prompt: lead.md\n    posture: locked\n",
        ),
    );
    let err = env.fails(&["create", "oncall", &env.folder("teams")]);
    assert!(
        err.contains("locked") && err.contains("team folder"),
        "{err}"
    );
}

// --- A start that never reached tmux ---

#[test]
fn a_start_that_never_reached_tmux_leaves_nothing_held() {
    let env = Env::new();
    // A socket directory whose socket path is too long for tmux.
    let long = env.path(&format!("t{}", "x".repeat(120)));
    fs::create_dir_all(&long).unwrap();
    let unusable = |args: &[&str]| {
        env.cmd(args)
            .env("BORGCHART_TMUX_TMPDIR", &long)
            .output()
            .unwrap()
    };
    // A first start: the new instance is taken back.
    let out = unusable(&["create", "oncall", &env.folder("work/a")]);
    assert!(!out.status.success(), "{}", both(&out));
    assert_eq!(env.registry()["instances"], json!({}));
    // A later start of an existing instance: stop and delete still work.
    let a = env.create("oncall", "work/a");
    env.ok(&["stop", &a]);
    let out = unusable(&["start", &a]);
    assert!(!out.status.success(), "{}", both(&out));
    let out = env.ok(&["stop", &a]);
    assert!(stdout(&out).contains("not running"), "{}", both(&out));
    env.ok(&["delete", &a]);
    // The tmux guard refused the unusable directory, as tmux would.
    assert!(!env.tmux.refused().is_empty());
    env.tmux.clear_refused();
}
