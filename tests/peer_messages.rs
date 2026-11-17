//! Independent root leads exchange recipient-owned messages without sharing authority.
#![cfg(feature = "test-support")]

mod common;
mod team_env;

use serde_json::{Value, json};
use std::fs;
use std::process::{Command, Output};
use team_env::{Env, both};

struct Peers {
    env: Env,
    trees: [String; 2],
    leads: [String; 2],
}

impl Peers {
    fn new() -> Self {
        let env = Env::new();
        env.write(
            "templates/oncall/template.yaml",
            "roles:\n  lead: {prompt: lead.md, interactive: true}\n  helper: {prompt: helper.md, parent: lead, start: false}\n  scout: {prompt: scout.md, start: false}\n  dev: {template: dev, parent: lead, start: false}\n",
        );
        env.team("templates/dev");
        let trees = [
            env.create("oncall", "work/a"),
            env.create("oncall", "work/b"),
        ];
        let leads = trees.each_ref().map(|tree| env.member(tree, "lead"));
        for (tree, lead) in trees.iter().zip(&leads) {
            let sent = value(env.publish(tree, &json!({"to": lead, "body": "local seed"})));
            assert_eq!(sent["id"], "m1");
        }
        Self { env, trees, leads }
    }

    fn at(&self, side: usize) -> String {
        format!("{}/{}", self.trees[side], self.leads[side])
    }

    fn member(&self, tree: &str, member: &str, args: &[&str]) -> Command {
        let start: Value = serde_json::from_slice(
            &fs::read(
                self.env
                    .path(&format!("state/{tree}/members/{member}/incarnation.json")),
            )
            .unwrap(),
        )
        .unwrap();
        let mut cmd = self.env.cmd(args);
        cmd.env("BORGCHART_TEAM", tree)
            .env("BORGCHART_MEMBER", member)
            .env(
                "BORGCHART_INCARNATION",
                start["incarnation"].as_str().unwrap(),
            );
        cmd
    }

    fn cmd(&self, side: usize, args: &[&str]) -> Command {
        self.member(&self.trees[side], &self.leads[side], args)
    }

    fn send(&self, side: usize, refs: &[&str]) -> Output {
        let mut cmd = self.cmd(
            side,
            &["msg", "send", &self.at(1 - side), "peer request", "--json"],
        );
        for id in refs {
            cmd.args(["--reply-to", id]);
        }
        cmd.output().unwrap()
    }

    fn inbox(&self, side: usize) -> Value {
        value(
            self.cmd(side, &["msg", "list", "--full", "--json"])
                .output()
                .unwrap(),
        )
    }

    fn history(&self, side: usize) -> Vec<Vec<u8>> {
        let dir = self
            .env
            .path(&format!("state/{}/messages/records", self.trees[side]));
        let mut paths: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|p| p.unwrap().path())
            .collect();
        paths.sort();
        paths.iter().map(|p| fs::read(p).unwrap()).collect()
    }

    fn edit_meta(&self, side: usize, edit: impl FnOnce(&mut Value)) {
        let path = self
            .env
            .path(&format!("state/{}/start.json", self.trees[side]));
        let mut meta: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        edit(&mut meta);
        fs::write(path, serde_json::to_vec(&meta).unwrap()).unwrap();
    }

    fn refuses_without_writes(&self, cmd: &mut Command) -> Output {
        let before = [self.history(0), self.history(1)];
        let out = cmd.output().unwrap();
        assert!(!out.status.success(), "{}", both(&out));
        assert_eq!([self.history(0), self.history(1)], before);
        out
    }

    fn refuses_peer_route(&self, cmd: &mut Command) {
        let out = self.refuses_without_writes(cmd);
        assert_eq!(out.status.code(), Some(5), "{}", both(&out));
        assert!(
            both(&out).contains("cross-tree messages require"),
            "{}",
            both(&out)
        );
    }
}

fn value(out: Output) -> Value {
    assert!(out.status.success(), "{}", both(&out));
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn peer_send_belongs_only_to_the_recipient_and_grants_no_foreign_access() {
    let peers = Peers::new();
    let before = peers.history(0);
    let sent = value(peers.send(0, &[]));
    assert_eq!(sent["id"], "m2");
    assert_eq!(sent["store"], peers.trees[1]);
    assert_eq!(peers.history(0), before);
    let received = peers.inbox(1);
    let record = &received["records"][1];
    assert_eq!(record["from"], peers.at(0));
    assert_eq!(record["to"], peers.at(1));
    assert_eq!(record["body"], "peer request");
    assert_eq!(peers.env.row(&peers.trees[1], "lead")["unacked"], 2);
    let read = value(
        peers
            .cmd(1, &["msg", "read", "m2", "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(read["record"], *record);
    for args in [
        vec!["msg", "ack", "m2"],
        vec!["msg", "read", "m2", "--team", &peers.trees[1]],
        vec!["msg", "list", "--member", &peers.at(1)],
        vec!["stop", &peers.at(1)],
    ] {
        let out = peers.cmd(0, &args).output().unwrap();
        assert!(!out.status.success(), "{}", both(&out));
    }
    value(
        peers
            .cmd(1, &["msg", "ack", "m2", "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(peers.inbox(1)["records"].as_array().unwrap().len(), 1);
    assert_eq!(peers.history(0), before);
}

#[test]
fn peer_replies_qualify_the_own_store_id_and_reject_unrelated_references_without_writes() {
    let peers = Peers::new();
    value(peers.send(0, &[]));
    value(peers.send(0, &[]));
    let reply = value(peers.send(1, &["m2", "m3"]));
    assert_eq!(reply["id"], "m2");
    let received = peers.inbox(0);
    assert_eq!(
        received["records"][1]["reply_to"],
        json!([
            format!("{}:m2", peers.trees[1]),
            format!("{}:m3", peers.trees[1])
        ])
    );
    let before = [peers.history(0), peers.history(1)];
    for refs in [
        vec!["m1"],
        vec!["m9"],
        vec!["m02"],
        vec!["foreign:m2"],
        vec!["m2", "m9"],
    ] {
        let out = peers.send(1, &refs);
        assert!(!out.status.success(), "{refs:?}: {}", both(&out));
        assert_eq!([peers.history(0), peers.history(1)], before);
    }
    value(peers.send(0, &["m2"]));
    assert_eq!(
        peers.inbox(1)["records"][3]["reply_to"],
        json!([format!("{}:m2", peers.trees[0])])
    );
}

#[test]
fn peer_routes_refuse_non_roots_child_roots_locked_ends_and_identity_overrides() {
    let peers = Peers::new();
    for side in 0..=1 {
        peers
            .env
            .ok(&["create", &format!("{}/helper", peers.trees[side])]);
        let helper = peers.env.member(&peers.trees[side], "helper");
        let args = ["msg", "send", &peers.at(1 - side), "refused"];
        peers.refuses_without_writes(&mut peers.member(&peers.trees[side], &helper, &args));
        let target = format!("{}/{helper}", peers.trees[side]);
        peers
            .refuses_without_writes(&mut peers.cmd(1 - side, &["msg", "send", &target, "refused"]));

        fs::create_dir_all(peers.env.path(if side == 0 {
            "work/a/lead"
        } else {
            "work/b/lead"
        }))
        .unwrap();
        peers
            .env
            .ok(&["create", &format!("{}/dev", peers.trees[side])]);
        let child = peers.env.registry()["teams"]
            .as_object()
            .unwrap()
            .keys()
            .find(|id| id.starts_with(&format!("{}/", peers.trees[side])))
            .unwrap()
            .clone();
        peers.env.ok(&["start", &child]);
        let lead = peers.env.member(&child, "lead");
        peers.refuses_peer_route(&mut peers.member(&child, &lead, &args));
        let target = format!("{child}/{lead}");
        peers.refuses_peer_route(&mut peers.cmd(1 - side, &["msg", "send", &target, "refused"]));

        peers.edit_meta(side, |meta| {
            meta["members"][&peers.leads[side]]["posture"] = json!("locked")
        });
        peers.refuses_peer_route(&mut peers.cmd(side, &args));
        peers.refuses_peer_route(
            &mut peers.cmd(1 - side, &["msg", "send", &peers.at(side), "refused"]),
        );
        peers.edit_meta(side, |meta| {
            meta["members"][&peers.leads[side]]["posture"] = json!("vendor")
        });
    }
    for extra in [
        vec!["--from", &peers.leads[0]],
        vec!["--allow-unknown"],
        vec!["--external", "owner"],
    ] {
        let mut cmd = peers.cmd(0, &["msg", "send", &peers.at(1), "refused"]);
        peers.refuses_without_writes(cmd.args(extra));
    }
    let mut stale = peers.cmd(0, &["msg", "send", &peers.at(1), "stale"]);
    peers.refuses_without_writes(stale.env("BORGCHART_INCARNATION", "replaced-start"));
    let mut partial = peers.cmd(0, &["msg", "send", &peers.at(1), "partial"]);
    peers.refuses_without_writes(partial.env_remove("BORGCHART_INCARNATION"));
    peers.env.ok(&["stop", &peers.trees[0]]);
    peers.refuses_without_writes(&mut peers.cmd(0, &["msg", "send", &peers.at(1), "stopped"]));
}

#[test]
fn stopped_peer_keeps_mail_and_unacked_peer_work_blocks_tree_deletion() {
    let peers = Peers::new();
    value(
        peers
            .cmd(1, &["msg", "ack", "m1", "--json"])
            .output()
            .unwrap(),
    );
    peers.env.ok(&["stop", &peers.trees[1]]);
    let sent = value(peers.send(0, &[]));
    assert_eq!(sent["delivery"], "stored");
    assert_eq!(peers.env.row(&peers.trees[1], "lead")["unacked"], 1);
    assert!(!peers.env.brg(&["delete", &peers.trees[1]]).status.success());
    peers.env.ok(&["start", &peers.trees[1]]);
    assert_eq!(peers.inbox(1)["records"][0]["id"], "m2");
    peers.env.ok(&["stop", &peers.trees[1]]);
    peers.env.ok(&["delete", &peers.trees[1], "--force"]);
    let before = peers.history(0);
    assert_eq!(peers.send(0, &[]).status.code(), Some(3));
    assert!(
        !peers
            .env
            .path(&format!("state/{}", peers.trees[1]))
            .exists()
    );
    assert_eq!(peers.history(0), before);
}

#[test]
fn unknown_never_started_and_unstarted_members_refuse_before_publication() {
    let peers = Peers::new();
    for target in [
        "missing-2a3b/lead-4a5b".to_string(),
        format!("{}/scout", peers.trees[1]),
    ] {
        peers.refuses_without_writes(&mut peers.cmd(0, &["msg", "send", &target, "refused"]));
    }
    peers
        .env
        .ok(&["create", &format!("{}/scout", peers.trees[1])]);
    let scout = peers.env.member(&peers.trees[1], "scout");
    let mut cmd = peers.cmd(
        0,
        &[
            "msg",
            "send",
            &format!("{}/{scout}", peers.trees[1]),
            "unstarted",
        ],
    );
    peers.edit_meta(1, |meta| {
        meta["members"].as_object_mut().unwrap().remove(&scout);
    });
    let before = [peers.history(0), peers.history(1)];
    assert_eq!(cmd.output().unwrap().status.code(), Some(3));
    assert_eq!([peers.history(0), peers.history(1)], before);
    let meta = peers
        .env
        .path(&format!("state/{}/start.json", peers.trees[1]));
    fs::remove_file(meta).unwrap();
    fs::remove_dir_all(
        peers
            .env
            .path(&format!("state/{}/messages", peers.trees[1])),
    )
    .unwrap();
    assert_eq!(peers.send(0, &[]).status.code(), Some(3));
    assert!(
        !peers
            .env
            .path(&format!("state/{}/messages", peers.trees[1]))
            .exists()
    );
}

#[test]
fn missing_or_corrupt_peer_stores_are_named_and_never_recreated() {
    for side in 0..=1 {
        for damage in ["missing", "identity", "record", "lock"] {
            let peers = Peers::new();
            let before_other = peers.history(1 - side);
            let store = peers
                .env
                .path(&format!("state/{}/messages", peers.trees[side]));
            match damage {
                "missing" => fs::remove_dir_all(&store).unwrap(),
                "identity" => fs::write(store.join("store.json"), "broken").unwrap(),
                "record" => fs::write(store.join("records/0000000001.json"), "broken").unwrap(),
                "lock" => fs::remove_file(store.join("lock")).unwrap(),
                _ => unreachable!(),
            }
            let out = peers.send(0, &[]);
            assert!(!out.status.success(), "{side} {damage}: {}", both(&out));
            assert!(both(&out).contains(&peers.trees[side]), "{}", both(&out));
            assert_eq!(peers.history(1 - side), before_other);
            if damage == "missing" {
                assert!(!store.exists());
            }
            if damage == "lock" {
                assert!(!store.join("lock").exists());
            }
        }
    }
}

#[test]
fn peer_codex_queue_uses_the_recipient_session_and_environment_and_keeps_failed_delivery() {
    let peers = Peers::new();
    peers.edit_meta(1, |meta| {
        meta["members"][&peers.leads[1]]["vendor"] = json!("codex");
        meta["base_env"]["PEER_QUEUE_MARKER"] = json!("recipient");
    });
    let state = borgchart::state::StateDir {
        id: peers.trees[1].clone(),
        root: peers.env.path(&format!("state/{}", peers.trees[1])),
    };
    let session = "01a1d300-f48d-7c70-a58a-d5fbea1edd20";
    fs::write(
        state.member_session(&peers.leads[1]),
        format!("{session}\n"),
    )
    .unwrap();
    fs::remove_file(peers.env.path("bin/codex")).unwrap();
    team_env::script(
        &peers.env.path("bin/codex"),
        &format!(
            "#!/bin/sh\ntest \"$PEER_QUEUE_MARKER\" = recipient || exit 11\ntest -z \"${{PEER_SENDER_ONLY+x}}\" || exit 12\ntest \"$1\" = queue && test \"$2\" = --thread && test \"$3\" = {session} && test \"$4\" = --message || exit 13\nprintf '%s' \"$5\" > \"$HOME/peer-queue\"\n"
        ),
    );
    let sent = value(
        peers
            .cmd(0, &["msg", "send", &peers.at(1), "queue request", "--json"])
            .env("PEER_QUEUE_MARKER", "sender")
            .env("PEER_SENDER_ONLY", "private")
            .output()
            .unwrap(),
    );
    assert_eq!(sent["delivery"], "queued");
    let envelope = fs::read_to_string(peers.env.path("home/peer-queue")).unwrap();
    assert!(envelope.contains(&peers.at(0)));
    assert!(envelope.contains("another tree's root member, not the owner"));
    assert!(envelope.contains("queue request"));
    team_env::script(&peers.env.path("bin/codex"), "#!/bin/sh\nexit 14\n");
    let failed = value(peers.send(0, &[]));
    assert_eq!(failed["delivery"], "stored");
    assert_eq!(peers.inbox(1)["records"][2]["id"], failed["id"]);
    assert_eq!(peers.history(0).len(), 1);
}

#[test]
fn peer_claude_typing_attributes_the_other_lead_and_preserves_a_draft() {
    let peers = Peers::new();
    peers.edit_meta(1, |meta| {
        meta["members"][&peers.leads[1]]["interactive"] = json!(false)
    });
    let sent = value(peers.send(0, &[]));
    assert_eq!(sent["delivery"], "typed");
    let log = peers
        .env
        .path(&format!("home/fake-agent/{}.jsonl", peers.leads[1]));
    let submissions = || {
        fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|event| event["event"] == "submit")
            .map(|event| event["text"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    peers
        .env
        .wait_for("peer envelope submitted", || !submissions().is_empty());
    let submitted = submissions();
    assert_eq!(submitted.len(), 1);
    assert!(submitted[0].contains(&peers.at(0)));
    assert!(submitted[0].contains("another tree's root member, not the owner"));
    assert!(submitted[0].contains("peer request"));
    let pane = format!("brg-{}:{}", peers.trees[1], peers.leads[1]);
    peers.env.tmux(&["set-buffer", "-b", "draft", "draft text"]);
    peers
        .env
        .tmux(&["paste-buffer", "-p", "-b", "draft", "-t", &pane]);
    peers.env.wait_for("draft rendered", || {
        peers
            .env
            .tmux(&["capture-pane", "-p", "-t", &pane])
            .contains("draft text")
    });
    assert_eq!(value(peers.send(0, &[]))["delivery"], "blocked");
    assert!(
        peers
            .env
            .tmux(&["capture-pane", "-p", "-t", &pane])
            .contains("draft text")
    );
    assert_eq!(submissions().len(), 1);
    assert_eq!(peers.inbox(1)["records"].as_array().unwrap().len(), 3);
}

#[test]
fn peer_reply_cannot_claim_another_root_members_message() {
    let peers = Peers::new();
    peers
        .env
        .ok(&["create", &format!("{}/scout", peers.trees[0])]);
    peers
        .env
        .ok(&["create", &format!("{}/scout", peers.trees[1])]);
    let scouts = peers
        .trees
        .each_ref()
        .map(|tree| peers.env.member(tree, "scout"));
    value(
        peers
            .member(
                &peers.trees[0],
                &scouts[0],
                &["msg", "send", &peers.at(1), "other author", "--json"],
            )
            .output()
            .unwrap(),
    );
    let to_scout = format!("{}/{}", peers.trees[1], scouts[1]);
    value(
        peers
            .cmd(0, &["msg", "send", &to_scout, "other recipient", "--json"])
            .output()
            .unwrap(),
    );
    fs::create_dir_all(peers.env.path("work/c")).unwrap();
    let third = peers.env.create("oncall", "work/c");
    let lead = peers.env.member(&third, "lead");
    value(
        peers
            .member(
                &third,
                &lead,
                &["msg", "send", &peers.at(1), "third tree", "--json"],
            )
            .output()
            .unwrap(),
    );
    let before = [peers.history(0), peers.history(1)];
    for id in ["m2", "m3", "m4"] {
        assert!(!peers.send(1, &[id]).status.success());
        assert_eq!([peers.history(0), peers.history(1)], before);
    }
}

#[test]
fn simultaneous_peer_and_local_sends_allocate_only_contiguous_recipient_ids() {
    let peers = Peers::new();
    std::thread::scope(|scope| {
        let mut sends = Vec::new();
        for side in 0..=1 {
            for _ in 0..3 {
                let p = &peers;
                sends.push(scope.spawn(move || value(p.send(side, &[]))));
                sends.push(scope.spawn(move || {
                    value(p.env.publish(
                        &p.trees[side],
                        &json!({"to": p.leads[side], "body": "local"}),
                    ))
                }));
            }
        }
        for send in sends {
            send.join().unwrap();
        }
    });
    for side in 0..=1 {
        let inbox = peers.inbox(side);
        let records = inbox["records"].as_array().unwrap();
        assert_eq!(records.len(), 7);
        for (seq, record) in records.iter().enumerate() {
            assert_eq!(record["id"], format!("m{}", seq + 1));
        }
        assert_eq!(
            records
                .iter()
                .filter(|r| r["from"] == peers.at(1 - side))
                .count(),
            3
        );
    }
}

#[test]
fn stopping_a_peer_sender_during_publication_fences_it_without_foreign_writes() {
    use std::process::Stdio;
    let peers = Peers::new();
    let before = [peers.history(0), peers.history(1)];
    let pause = peers.env.path("publish-pause");
    let child = peers
        .cmd(0, &["msg", "send", &peers.at(1), "stale request", "--json"])
        .env("BORGCHART_TEST_PUBLISH_PAUSE", &pause)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    peers.env.wait_for("publication preflight", || {
        pause.with_extension("waiting").exists()
    });
    peers.env.ok(&["stop", &peers.trees[0]]);
    fs::write(&pause, "continue").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success(), "{}", both(&out));
    assert_eq!([peers.history(0), peers.history(1)], before);
}

#[test]
fn deleting_a_peer_target_during_publication_cannot_recreate_its_store() {
    use std::process::Stdio;
    let peers = Peers::new();
    let before = peers.history(0);
    let pause = peers.env.path("publish-pause");
    let child = peers
        .cmd(
            0,
            &["msg", "send", &peers.at(1), "deleted target", "--json"],
        )
        .env("BORGCHART_TEST_PUBLISH_PAUSE", &pause)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    peers.env.wait_for("publication preflight", || {
        pause.with_extension("waiting").exists()
    });
    peers.env.ok(&["stop", &peers.trees[1]]);
    peers.env.ok(&["delete", &peers.trees[1], "--force"]);
    fs::write(&pause, "continue").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success(), "{}", both(&out));
    assert!(
        !peers
            .env
            .path(&format!("state/{}", peers.trees[1]))
            .exists()
    );
    assert_eq!(peers.history(0), before);
}

#[test]
fn peer_publication_wakes_an_interactive_claude_inbox_without_typing() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let peers = Peers::new();
    value(
        peers
            .cmd(1, &["msg", "ack", "m1", "--json"])
            .output()
            .unwrap(),
    );
    let start = Instant::now();
    let ready = peers.env.path("wait-ready");
    let waiting = peers
        .cmd(1, &["msg", "wait", "--full", "--json"])
        .env("BORGCHART_WAIT_MIN_MS", "10000")
        .env("BORGCHART_WAIT_TIMEOUT_MS", "15000")
        .env("BORGCHART_TEST_WAIT_READY", &ready)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    peers
        .env
        .wait_for("interactive inbox initial snapshot", || ready.exists());
    assert_eq!(value(peers.send(0, &[]))["delivery"], "stored");
    let received = value(waiting.wait_with_output().unwrap());
    assert!(start.elapsed() < Duration::from_millis(8000));
    assert_eq!(received["records"][0]["id"], "m2");
    let log = peers
        .env
        .path(&format!("home/fake-agent/{}.jsonl", peers.leads[1]));
    assert!(
        !fs::read_to_string(log)
            .unwrap()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .any(|event| event["event"] == "submit")
    );
}

fn refuse_after_recipient_change(change: &str) {
    use std::process::Stdio;
    let peers = Peers::new();
    let before = [peers.history(0), peers.history(1)];
    let pause = peers.env.path("publish-pause");
    let publishing = peers
        .cmd(
            0,
            &["msg", "send", &peers.at(1), "changed recipient", "--json"],
        )
        .env("BORGCHART_TEST_PUBLISH_PAUSE", &pause)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    peers.env.wait_for("peer preflight", || {
        pause.with_extension("waiting").exists()
    });
    peers.edit_meta(1, |meta| {
        let member = &mut meta["members"][&peers.leads[1]];
        if change == "locked" {
            member["posture"] = json!("locked");
        } else {
            member["parent"] = json!("other-lead");
        }
    });
    fs::write(&pause, "continue").unwrap();
    let out = publishing.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(5), "{change}: {}", both(&out));
    assert!(
        both(&out).contains("cross-tree messages require"),
        "{}",
        both(&out)
    );
    assert_eq!([peers.history(0), peers.history(1)], before);
}

#[test]
fn peer_commit_rechecks_a_recipient_that_becomes_locked_after_preflight() {
    refuse_after_recipient_change("locked");
}

#[test]
fn peer_commit_rechecks_a_recipient_that_gains_a_parent_after_preflight() {
    refuse_after_recipient_change("parent");
}

#[test]
fn an_unregistered_peer_refuses_publication_before_its_state_is_removed() {
    use std::process::Stdio;
    let peers = Peers::new();
    peers.env.ok(&["stop", &peers.trees[1]]);
    let before = [peers.history(0), peers.history(1)];
    let publish_pause = peers.env.path("publish-pause");
    let publishing = peers
        .cmd(
            0,
            &["msg", "send", &peers.at(1), "deleted recipient", "--json"],
        )
        .env("BORGCHART_TEST_PUBLISH_PAUSE", &publish_pause)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    peers.env.wait_for("peer preflight", || {
        publish_pause.with_extension("waiting").exists()
    });
    let delete_pause = peers.env.path("delete-remove-pause");
    let deleting = peers
        .env
        .cmd(&["delete", &peers.trees[1], "--force"])
        .env("BORGCHART_TEST_DELETE_REMOVE_PAUSE", &delete_pause)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    peers
        .env
        .wait_for("deleted registration before state removal", || {
            delete_pause.with_extension("waiting").exists()
        });
    assert!(peers.env.registry()["teams"].get(&peers.trees[1]).is_none());
    assert!(
        peers
            .env
            .path(&format!("state/{}", peers.trees[1]))
            .exists()
    );
    fs::write(&publish_pause, "continue").unwrap();
    let out = publishing.wait_with_output().unwrap();
    let after = [peers.history(0), peers.history(1)];
    fs::write(&delete_pause, "continue").unwrap();
    let deleted = deleting.wait_with_output().unwrap();
    assert!(deleted.status.success(), "{}", both(&deleted));
    assert!(!out.status.success(), "{}", both(&out));
    assert_eq!(after, before);
    assert!(
        !peers
            .env
            .path(&format!("state/{}", peers.trees[1]))
            .exists()
    );
}

#[test]
fn peer_reference_snapshot_excludes_source_writers_without_holding_the_target_lock() {
    use std::process::Stdio;
    let peers = Peers::new();
    value(peers.send(1, &[]));
    let pause = peers.env.path("snapshot-pause");
    let publish_pause = peers.env.path("publish-pause");
    let publishing = peers
        .cmd(
            0,
            &[
                "msg",
                "send",
                &peers.at(1),
                "consistent reply",
                "--reply-to",
                "m2",
                "--json",
            ],
        )
        .env("BORGCHART_TEST_SNAPSHOT_PAUSE", &pause)
        .env("BORGCHART_TEST_PUBLISH_PAUSE", &publish_pause)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    peers
        .env
        .wait_for("sender snapshot under its shared lock", || {
            pause.with_extension("waiting").exists()
        });
    let source = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(
            peers
                .env
                .path(&format!("state/{}/messages/lock", peers.trees[0])),
        )
        .unwrap();
    let target = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(
            peers
                .env
                .path(&format!("state/{}/messages/lock", peers.trees[1])),
        )
        .unwrap();
    let excludes_source_writers = matches!(source.try_lock(), Err(fs::TryLockError::WouldBlock));
    drop(source);
    let target_is_free = target.try_lock().is_ok();
    drop(target);
    fs::write(&pause, "continue").unwrap();
    peers.env.wait_for(
        "source snapshot and references checked before target lock",
        || publish_pause.with_extension("waiting").exists(),
    );
    let source = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(
            peers
                .env
                .path(&format!("state/{}/messages/lock", peers.trees[0])),
        )
        .unwrap();
    let source_was_released = source.try_lock().is_ok();
    drop(source);
    fs::write(&publish_pause, "continue").unwrap();
    value(publishing.wait_with_output().unwrap());
    assert!(excludes_source_writers);
    assert!(
        source_was_released,
        "release the source snapshot before taking the target lock"
    );
    assert!(
        target_is_free,
        "the target lock must be free during the source snapshot"
    );
    assert_eq!(
        peers.inbox(1)["records"][1]["reply_to"],
        json!([format!("{}:m2", peers.trees[0])])
    );
}
