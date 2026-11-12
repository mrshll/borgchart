//! Safety around teams: checkouts held across teams, brg's own
//! state folder, changed files and storage that can't be read.
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
fn only_running_members_hold_their_folder() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    let err = env.fails(&["create", "oncall", &env.folder("work/a")]);
    assert!(err.contains(&a) && err.contains("running there"), "{err}");
    env.ok(&["stop", &a]);
    let b = env.create("oncall", "work/a");
    assert!(env.registry()["teams"].get(&a).is_some());
    env.ok(&["stop", &b]);
}

#[test]
fn stopped_members_with_unreadable_messages_hold_no_folder() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    env.ok(&["stop", &a]);
    fs::write(
        env.path(&format!("state/{a}/messages/store.json")),
        "not json\n",
    )
    .unwrap();
    let b = env.create("oncall", "work/a");
    assert!(env.registry()["teams"].get(&a).is_some());
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
fn killed_server_holds_checkout_until_full_stop_confirms_processes_gone() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    // brg can no longer reach the server that ran A.
    env.tmux(&["kill-server"]);
    let err = env.fails(&["create", "oncall", &env.folder("work/a")]);
    assert!(err.contains(&a), "{err}");
    let err = env.fails(&["delete", &a, "--force"]);
    assert!(err.contains("even with --force"), "{err}");
    assert!(env.path(&format!("state/{a}")).is_dir());
    env.ok(&["stop", &a]);
    env.ok(&["delete", &a, "--force"]);
    let b = env.create("oncall", "work/a");
    env.ok(&["stop", &b]);
}

/// A team `A` running in `work/a`, whose start.json is then
/// damaged: brg can no longer read which tmux server runs it.
fn damaged_start_record() -> (Env, String) {
    let env = Env::new();
    env.write("templates/oncall/template.yaml", TEAM);
    let a = env.create("oncall", "work/a");
    fs::write(
        env.path(&format!("state/{a}/start.json")),
        "{\"team_dir\": ",
    )
    .unwrap();
    (env, a)
}

fn lead_window_up(env: &Env, a: &str) -> bool {
    env.tmux(&["list-windows", "-a", "-F", "#{window_name} #{pane_dead}"])
        .lines()
        .any(|l| l == format!("{} 0", env.member(a, "lead")))
}

#[test]
fn a_damaged_start_record_keeps_the_reservation() {
    let (env, a) = damaged_start_record();
    let err = env.fails(&["create", "oncall", &env.folder("work/a")]);
    assert!(err.contains("start.json"), "{err}");
    assert_eq!(env.registry()["teams"].as_object().unwrap().len(), 1);
    assert!(lead_window_up(&env, &a));
}

#[test]
fn a_damaged_start_record_blocks_delete_even_with_force() {
    let (env, a) = damaged_start_record();
    let other = env.tmux.socket("brg-other");
    for socket in [&env.socket, &other] {
        for args in [&["delete", &a][..], &["delete", &a, "--force"][..]] {
            let out = env
                .cmd(args)
                .env("BORGCHART_TMUX_SOCKET", socket)
                .output()
                .unwrap();
            assert!(
                !out.status.success(),
                "{args:?} on {socket}: {}",
                both(&out)
            );
            assert!(both(&out).contains("start.json"), "{}", both(&out));
        }
    }
    assert!(env.path(&format!("state/{a}")).is_dir());
    assert!(lead_window_up(&env, &a));
}

#[test]
fn a_damaged_start_record_blocks_a_repoint() {
    let (env, a) = damaged_start_record();
    env.team("moved/oncall");
    let record = env.path(&format!("state/{a}/team.json"));
    let before = (fs::read_to_string(&record).unwrap(), env.registry());
    let other = env.tmux.socket("brg-other");
    let out = env
        .cmd(&["start", &a, "--template", &env.folder("moved/oncall")])
        .env("BORGCHART_TMUX_SOCKET", &other)
        .output()
        .unwrap();
    assert!(!out.status.success(), "{}", both(&out));
    assert!(both(&out).contains("start.json"), "{}", both(&out));
    assert_eq!(
        (fs::read_to_string(&record).unwrap(), env.registry()),
        before
    );
    assert!(lead_window_up(&env, &a));
}

// --- 7. Moved folders ---

#[test]
fn repointing_at_a_retired_template_names_the_replacement() {
    let env = Env::new();
    let team = env.create("oncall", "work/a");
    env.ok(&["stop", &team]);
    env.team("old/oncall");
    fs::rename(
        env.path("old/oncall/template.yaml"),
        env.path("old/oncall/team.yaml"),
    )
    .unwrap();
    let before = env.registry();
    let error = env.fails(&["start", &team, "--template", &env.folder("old/oncall")]);
    assert!(
        error.contains("team.yaml") && error.contains("template.yaml"),
        "{error}"
    );
    assert_eq!(env.registry(), before);
}

#[test]
fn a_moved_team_folder_is_re_pointed_after_a_full_stop() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    let lead = env.member(&a, "lead");
    let out = env.publish(&a, &json!({"to": lead, "body": "keep me"}));
    assert!(out.status.success(), "{}", both(&out));
    fs::create_dir_all(env.path("elsewhere")).unwrap();
    fs::rename(env.path("templates/oncall"), env.path("elsewhere/oncall")).unwrap();
    let moved = env.folder("elsewhere/oncall");
    let err = env.fails(&["start", &a, "--template", &moved]);
    assert!(err.contains(&format!("brg stop {a}")), "{err}");
    env.ok(&["stop", &a]);
    let err = env.fails(&["start", &a]);
    assert!(err.contains("--template"), "{err}");
    env.ok(&["start", &a, "--template", &moved]);
    env.wait_for("lead idle", || env.state_of(&a, "lead") == "idle");
    assert_eq!(env.member(&a, "lead"), lead, "the same team and member");
    let inbox = stdout(&env.ok(&["msg", "list", "--member", &lead, "--team", &a, "--full"]));
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
fn unread_messages_are_open_work_for_delete() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    let lead = env.member(&a, "lead");
    let out = env.publish(&a, &json!({"to": lead, "body": "hi"}));
    assert!(out.status.success(), "{}", both(&out));
    env.ok(&["stop", &a]);
    let err = env.fails(&["delete", &a]);
    assert!(
        err.contains("unread m1") && err.contains(&lead) && err.contains("--force"),
        "{err}"
    );
    assert!(env.path(&format!("state/{a}")).is_dir());
    assert!(env.registry()["teams"].get(&a).is_some());
    env.ok(&["delete", &a, "--force"]);
    assert!(!env.path(&format!("state/{a}")).exists());
    assert!(env.registry()["teams"].get(&a).is_none());
}

#[test]
fn an_unreadable_store_still_stops_and_delete_needs_force() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    fs::write(
        env.path(&format!("state/{a}/messages/store.json")),
        "not json\n",
    )
    .unwrap();
    env.ok(&["stop", &a]);
    let err = env.fails(&["start", &a]);
    assert!(
        err.contains("damaged") && err.contains("store.json"),
        "{err}"
    );
    let err = env.fails(&["delete", &a]);
    assert!(err.contains("can't be inspected"), "{err}");
    env.ok(&["delete", &a, "--force"]);
}

// --- 14. brg's state folder and the template folder ---

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
    let out = env.ok(&["create", "oncall", &env.folder("templates")]);
    assert!(stdout(&out).contains("template folder"), "{}", both(&out));
    let a = first_word(&stdout(&out));
    env.ok(&["stop", &a]);
    env.ok(&["delete", &a]);
    // `locked`: refused because the working folder includes the template files.
    env.write(
        "templates/oncall/template.yaml",
        &TEAM.replace(
            "  lead:\n    prompt: lead.md\n",
            "  lead:\n    prompt: lead.md\n    posture: locked\n",
        ),
    );
    let err = env.fails(&["create", "oncall", &env.folder("templates")]);
    assert!(
        err.contains("locked") && err.contains("template folder"),
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
    // A first start: the new team is taken back.
    let out = unusable(&["create", "oncall", &env.folder("work/a")]);
    assert!(!out.status.success(), "{}", both(&out));
    assert_eq!(env.registry()["teams"], json!({}));
    // A later start of an existing team: stop and delete still work.
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

// --- Review fixes: damaged storage, partial contexts, config paths ---

#[test]
fn a_store_without_its_identity_needs_force_for_delete() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    let lead = env.member(&a, "lead");
    let out = env.publish(&a, &json!({"to": lead, "body": "hi"}));
    assert!(out.status.success(), "{}", both(&out));
    env.ok(&["stop", &a]);
    fs::remove_file(env.path(&format!("state/{a}/messages/store.json"))).unwrap();
    let b = env.create("oncall", "work/a");
    env.ok(&["stop", &b]);
    let err = env.fails(&["delete", &a]);
    assert!(err.contains("can't be inspected"), "{err}");
    env.ok(&["delete", &a, "--force"]);
}

#[test]
fn a_start_refuses_unreadable_records_full_or_member() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    let lead = env.member(&a, "lead");
    let out = env.publish(&a, &json!({"to": lead, "body": "hi"}));
    assert!(out.status.success(), "{}", both(&out));
    env.ok(&["stop", &a]);
    fs::write(
        env.path(&format!("state/{a}/messages/records/0000000001.json")),
        "bad json",
    )
    .unwrap();
    for target in [a.clone(), format!("{a}/{lead}")] {
        let err = env.fails(&["start", &target]);
        assert!(
            err.contains("can't be read") && err.contains("brg create"),
            "{target}: {err}"
        );
    }
    assert_eq!(env.tmux(&["list-sessions"]), "");
}

#[test]
fn a_partial_member_context_is_refused_for_reads_too() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    let b = env.create("oncall", "work/b");
    let lead = env.member(&a, "lead");
    for (var, value) in [
        ("BORGCHART_MEMBER", lead.as_str()),
        ("BORGCHART_INCARNATION", "x.1.abcd"),
    ] {
        for args in [
            vec!["msg", "integrity-check", "--team", b.as_str()],
            vec!["msg", "read", "m1", "--team", b.as_str()],
            vec![
                "msg",
                "list",
                "--member",
                lead.as_str(),
                "--team",
                a.as_str(),
            ],
        ] {
            let out = env.cmd(&args).env(var, value).output().unwrap();
            assert!(!out.status.success(), "{var} {args:?}: {}", both(&out));
            assert!(
                both(&out).contains("incomplete member context"),
                "{var} {args:?}: {}",
                both(&out)
            );
        }
    }
    // A plugin process has only its team: that still works.
    let out = env
        .cmd(&["msg", "integrity-check"])
        .env("BORGCHART_TEAM", &a)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    env.ok(&["stop", &a]);
    env.ok(&["stop", &b]);
}

#[test]
fn a_relative_config_path_reaches_members_resolved() {
    let env = Env::new();
    let out = env
        .cmd(&["create", "oncall", &env.folder("work/a")])
        .env("BORGCHART_CONFIG", "config.yaml")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    let a = first_word(&stdout(&out));
    env.wait_for("lead idle", || env.state_of(&a, "lead") == "idle");
    let lead = env.member(&a, "lead");
    let forwarded = env.starts(&lead)[0]["env"]["BORGCHART_CONFIG"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(forwarded, env.folder("config.yaml"));
    // A nested create from the member's folder, with what it was given.
    let out = env
        .cmd(&["create", "oncall", &env.folder("work/b")])
        .current_dir(env.path("work/a"))
        .env("BORGCHART_CONFIG", &forwarded)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    let b = first_word(&stdout(&out));
    env.ok(&["stop", &a]);
    env.ok(&["stop", &b]);
}

#[test]
fn a_failed_create_never_takes_back_an_instance_started_meanwhile() {
    let env = Env::new();
    let long = env.path(&format!("t{}", "x".repeat(120)));
    fs::create_dir_all(&long).unwrap();
    let pause = env.path("rollback-pause");
    // A create that can't reach tmux, paused before it takes its team back.
    let failing = env
        .cmd(&["create", "oncall", &env.folder("work/a")])
        .env("BORGCHART_TMUX_TMPDIR", &long)
        .env("BORGCHART_TEST_ROLLBACK_PAUSE", &pause)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let waiting = pause.with_extension("waiting");
    env.wait_for("the rollback to pause", || waiting.exists());
    // Meanwhile someone starts the registered team.
    let reg: serde_json::Value =
        serde_json::from_slice(&fs::read(env.path("state/registry.json")).unwrap()).unwrap();
    let a = reg["teams"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    env.ok(&["start", &a]);
    fs::write(&pause, "").unwrap();
    let out = failing.wait_with_output().unwrap();
    assert!(!out.status.success(), "{}", both(&out));
    // The started team is still there, and still runs.
    assert!(env.registry()["teams"].get(&a).is_some(), "taken back");
    assert!(env.path(&format!("state/{a}/team.json")).exists());
    env.wait_for("lead idle", || env.state_of(&a, "lead") == "idle");
    env.ok(&["stop", &a]);
    env.tmux.clear_refused();
}
