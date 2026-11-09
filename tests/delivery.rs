//! CLI proof that parent edges deliver between independent message stores.
#![cfg(feature = "test-support")]

mod common;
mod instance_env;

use instance_env::*;
use serde_json::{Value, json};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Tree {
    env: Env,
    parent: String,
    child: String,
    parent_member: String,
    child_member: String,
}

impl Tree {
    fn new() -> Self {
        let env = Env::new();
        env.write(
            "teams/oncall/team.yaml",
            "bus: {}\nroles:\n  lead: {prompt: lead.md, cwd: lead}\n  dev: {team: teams/dev, parent: lead, start: false, cwd: 'dev/{member_id}'}\n",
        );
        env.team("teams/oncall/teams/dev");
        fs::create_dir(env.path("work/a/lead")).unwrap();
        let parent = env.create("oncall", "work/a");
        env.ok(&["create", &format!("{parent}/dev")]);
        let parent_member = env.member(&parent, "lead");
        env.ok(&[
            "inbox",
            "--instance",
            &parent,
            "--member",
            &parent_member,
            "--json",
        ]);
        let registry = env.registry();
        let child = registry["instances"]
            .as_object()
            .unwrap()
            .keys()
            .find(|id| id.starts_with(&format!("{parent}/")))
            .unwrap()
            .clone();
        env.ok(&["start", &child]);
        let child_member = env.member(&child, "lead");
        Self {
            env,
            parent,
            child,
            parent_member,
            child_member,
        }
    }

    fn member_cmd(&self, child: bool, args: &[&str]) -> Command {
        let (instance, member) = if child {
            (&self.child, &self.child_member)
        } else {
            (&self.parent, &self.parent_member)
        };
        let start: Value = serde_json::from_slice(
            &fs::read(self.env.path(&format!(
                "state/{instance}/members/{member}/incarnation.json"
            )))
            .unwrap(),
        )
        .unwrap();
        let mut cmd = self.env.cmd(args);
        cmd.env("BORGCHART_INSTANCE", instance)
            .env("BORGCHART_MEMBER", member)
            .env(
                "BORGCHART_INCARNATION",
                start["incarnation"].as_str().unwrap(),
            )
            .env("BORGCHART_WAIT_MIN_MS", "0")
            .env("BORGCHART_WAIT_TIMEOUT_MS", "8000");
        cmd
    }

    fn publish(&self, child: bool, body: &str, reply_to: &[&str]) -> Value {
        let to = if child {
            format!("{}/{}", self.parent, self.parent_member)
        } else {
            format!("{}/{}", self.child, self.child_member)
        };
        let mut process = self
            .member_cmd(child, &["msg", "publish", "--json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        process
            .stdin
            .take()
            .unwrap()
            .write_all(
                json!({"to": to, "body": body, "reply_to": reply_to})
                    .to_string()
                    .as_bytes(),
            )
            .unwrap();
        value(process.wait_with_output().unwrap())
    }

    fn inbox(&self, child: bool) -> Value {
        value(
            self.member_cmd(child, &["inbox", "--full", "--json"])
                .output()
                .unwrap(),
        )
    }
}

fn value(out: Output) -> Value {
    assert!(out.status.success(), "{}", both(&out));
    serde_json::from_slice(&out.stdout).unwrap()
}

fn finish(mut child: Child) -> Output {
    let deadline = Instant::now() + Duration::from_secs(12);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!(
                "delivery command did not finish: {}",
                both(&child.wait_with_output().unwrap())
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn parent_edge_round_trip_maps_replies_to_the_original_local_record() {
    let tree = Tree::new();
    // Give the receiver its own m1, so the imported ID cannot accidentally
    // masquerade as the sender's original m1.
    value(tree.env.publish(
        &tree.parent,
        &json!({"to": tree.parent_member, "body": "Local task"}),
    ));
    let parent_before = tree
        .env
        .ok(&["bus", "check", "--instance", &tree.parent, "--json"]);
    let sent = tree.publish(true, "Child report", &[]);
    let parent_after = tree
        .env
        .ok(&["bus", "check", "--instance", &tree.parent, "--json"]);
    assert_eq!(
        parent_before.stdout, parent_after.stdout,
        "publish must write only its own store"
    );
    // The importer must work with read-only access to the sending store,
    // including opening its shared lock without write permission.
    let source = tree.env.path(&format!("state/{}/messages", tree.child));
    let paths = [
        (source.clone(), 0o500),
        (source.join("records"), 0o500),
        (source.join("lock"), 0o400),
    ];
    for (path, mode) in &paths {
        fs::set_permissions(path, fs::Permissions::from_mode(*mode)).unwrap();
    }
    let output = tree
        .member_cmd(false, &["inbox", "--full", "--json"])
        .output()
        .unwrap();
    for (path, mode) in &paths {
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(if *mode == 0o400 { 0o600 } else { 0o700 }),
        )
        .unwrap();
    }
    let inbox = value(output);
    let imported = inbox["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["body"] == "Child report")
        .unwrap();
    assert_ne!(imported["id"], sent["id"]);
    assert_eq!(
        imported["origin"],
        json!({"instance": tree.child, "id": sent["id"]})
    );
    assert_eq!(
        imported["from"],
        format!("{}/{}", tree.child, tree.child_member)
    );
    assert_eq!(
        imported["to"],
        format!("{}/{}", tree.parent, tree.parent_member)
    );
    tree.publish(false, "Parent reply", &[imported["id"].as_str().unwrap()]);
    let reply = tree.inbox(true);
    assert_eq!(
        reply["records"].as_array().unwrap().len(),
        1,
        "imported reports must never be sent on again"
    );
    assert_eq!(reply["records"][0]["body"], "Parent reply");
    assert_eq!(reply["records"][0]["reply_to"], json!([sent["id"]]));
    let id = reply["records"][0]["id"].as_str().unwrap();
    value(
        tree.member_cmd(true, &["msg", "ack", id, "--json"])
            .output()
            .unwrap(),
    );
    assert!(tree.inbox(true)["records"].as_array().unwrap().is_empty());
}

#[test]
fn a_child_publish_wakes_a_parent_already_waiting() {
    let tree = Tree::new();
    let ready = tree.env.path("wait-ready");
    let child = tree
        .member_cmd(
            false,
            &["inbox", "wait", "--timeout", "60", "--full", "--json"],
        )
        .env("BORGCHART_TEST_WAIT_READY", &ready)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    tree.env.wait_for("parent wait loop", || ready.exists());
    tree.publish(true, "Wake the parent", &[]);
    let out = value(finish(child));
    assert_eq!(out["records"][0]["body"], "Wake the parent");
}

#[test]
fn concurrent_importers_append_each_origin_once() {
    let tree = Tree::new();
    tree.publish(true, "One delivery", &[]);
    let gate = tree.env.path("import-gate");
    fs::create_dir(&gate).unwrap();
    let spawn = || {
        tree.member_cmd(false, &["inbox", "--full", "--json"])
            .env("BORGCHART_TEST_IMPORT_GATE", &gate)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let first = spawn();
    let second = spawn();
    tree.env.wait_for("both importers have read the peer", || {
        fs::read_dir(&gate).unwrap().count() == 2
    });
    fs::write(gate.join("proceed"), "").unwrap();
    for child in [first, second] {
        let out = value(finish(child));
        assert_eq!(out["records"].as_array().unwrap().len(), 1);
    }
    let out = tree.inbox(false);
    assert_eq!(out["records"].as_array().unwrap().len(), 1);
    assert_eq!(out["total"], 1);
}

#[test]
fn import_crashes_before_and_after_append_retry_without_loss_or_duplicates() {
    let tree = Tree::new();
    for (index, point) in ["before-append", "after-append"].into_iter().enumerate() {
        let body = format!("Delivery {index}");
        tree.publish(true, &body, &[]);
        let out = tree
            .member_cmd(false, &["inbox", "--full", "--json"])
            .env("BORGCHART_TEST_IMPORT_CRASH", point)
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(86),
            "fault point was not reached: {}",
            both(&out)
        );
        for _ in 0..2 {
            let inbox = tree.inbox(false);
            assert_eq!(inbox["total"], index + 1);
            assert_eq!(
                inbox["records"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|r| r["body"] == body)
                    .count(),
                1
            );
        }
    }
}

#[test]
fn a_sender_restart_before_import_does_not_lose_its_committed_message() {
    let tree = Tree::new();
    let sent = tree.publish(true, "Committed before restart", &[]);
    let address = format!("{}/{}", tree.child, tree.child_member);
    tree.env.ok(&["stop", &address]);
    tree.env.ok(&["start", &address]);
    let inbox = tree.inbox(false);
    assert_eq!(inbox["records"].as_array().unwrap().len(), 1);
    assert_eq!(inbox["records"][0]["body"], "Committed before restart");
    assert_eq!(
        inbox["records"][0]["origin"],
        json!({"instance": tree.child, "id": sent["id"]})
    );
}
