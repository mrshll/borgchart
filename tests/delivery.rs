//! CLI proof that parent edges share one tree message store.
#![cfg(feature = "test-support")]

mod common;
mod instance_env;

use instance_env::*;
use serde_json::{Value, json};
use std::fs;
use std::io::Write;
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
            "bus: {}\nroles:\n  lead: {prompt: lead.md, cwd: lead}\n  dev: {team: dev, parent: lead, start: false, max: 2, cwd: 'dev/{member_id}'}\n",
        );
        env.team("teams/dev");
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
        value(self.send(child, &to, body, reply_to))
    }

    /// `brg send TO - --json` as the parent's or the child's root, body on stdin.
    fn send(&self, child: bool, to: &str, body: &str, reply_to: &[&str]) -> Output {
        let mut cmd = self.member_cmd(child, &["send", to, "-", "--json"]);
        for reference in reply_to {
            cmd.args(["--reply-to", reference]);
        }
        let mut process = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        process
            .stdin
            .take()
            .unwrap()
            .write_all(body.as_bytes())
            .unwrap();
        process.wait_with_output().unwrap()
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
fn parent_edge_round_trip_preserves_ids_and_plain_reply_references() {
    let tree = Tree::new();
    value(tree.env.publish(
        &tree.parent,
        &json!({"to": tree.parent_member, "body": "Local task"}),
    ));
    let sent = tree.publish(true, "Child report", &[]);
    assert_eq!(sent["id"], "m2", "IDs belong to the whole tree");
    let inbox = tree.inbox(false);
    let report = &inbox["records"][1];
    assert_eq!(report["id"], sent["id"]);
    assert_eq!(
        report["from"],
        format!("{}/{}", tree.child, tree.child_member)
    );
    assert_eq!(
        report["to"],
        format!("{}/{}", tree.parent, tree.parent_member)
    );
    let id = report["id"].as_str().unwrap();
    value(
        tree.member_cmd(false, &["msg", "ack", id, "--json"])
            .output()
            .unwrap(),
    );
    let sent_reply = tree.publish(false, "Parent reply", &[id]);
    let inbox = tree.inbox(true);
    let reply = &inbox["records"][0];
    assert_eq!(reply["id"], sent_reply["id"]);
    assert_eq!(reply["reply_to"], json!([id]));
    value(
        tree.member_cmd(
            true,
            &["msg", "ack", reply["id"].as_str().unwrap(), "--json"],
        )
        .output()
        .unwrap(),
    );
    assert!(tree.inbox(true)["records"].as_array().unwrap().is_empty());
    let selected = value(
        tree.member_cmd(
            true,
            &[
                "inbox",
                "--member",
                &format!("{}/{}", tree.parent, tree.parent_member),
                "--json",
            ],
        )
        .output()
        .unwrap(),
    );
    assert_eq!(selected["records"][0]["id"], "m1");
    let read = value(
        tree.member_cmd(true, &["msg", "read", "m1", "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(read["record"]["body"], "Local task");
    assert!(
        !tree
            .env
            .path(&format!("state/{}/messages", tree.child))
            .exists()
    );
}

#[test]
fn concurrent_publishers_in_two_instances_share_contiguous_ids() {
    let tree = Tree::new();
    let mut processes = Vec::new();
    for child in [false, true, false, true, false, true] {
        let to = if child {
            format!("{}/{}", tree.parent, tree.parent_member)
        } else {
            format!("{}/{}", tree.child, tree.child_member)
        };
        processes.push(
            tree.member_cmd(child, &["send", &to, "Concurrent task", "--json"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    let mut ids: Vec<_> = processes
        .into_iter()
        .map(|process| value(finish(process))["id"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    assert_eq!(ids, ["m1", "m2", "m3", "m4", "m5", "m6"]);
    for instance in [&tree.parent, &tree.child] {
        let checked = value(
            tree.env
                .ok(&["bus", "check", "--instance", instance, "--json"]),
        );
        assert_eq!(checked["records"], 6);
    }
}

fn publish_to(tree: &Tree, child: bool, to: &str) -> Output {
    tree.send(child, to, "Routing check", &[])
}

#[test]
fn routing_allows_parent_edges_but_refuses_siblings_cousins_and_deleted_ancestors() {
    let tree = Tree::new();
    tree.env.ok(&["stop", &tree.parent]);
    let team = tree.env.path("teams/oncall/team.yaml");
    let config = fs::read_to_string(&team).unwrap();
    fs::write(
        team,
        format!("{config}  helper: {{prompt: helper.md, parent: lead, start: false, cwd: lead}}\n"),
    )
    .unwrap();
    tree.env.write(
        "teams/dev/team.yaml",
        "roles:\n  lead: {prompt: lead.md, cwd: repo}\n  leaf: {team: leaf, parent: lead, start: false, cwd: 'leaf/{member_id}'}\n",
    );
    tree.env.team("teams/leaf");
    let child_folder = tree.env.registry()["instances"][&tree.child]["folder"]
        .as_str()
        .unwrap()
        .to_string();
    fs::create_dir(std::path::Path::new(&child_folder).join("repo")).unwrap();
    tree.env.ok(&["start", &tree.parent]);
    tree.env.ok(&["create", &format!("{}/helper", tree.parent)]);
    let helper = tree.env.member(&tree.parent, "helper");
    let parent = format!("{}/{}", tree.parent, tree.parent_member);
    let local_child = format!("{}/{helper}", tree.parent);
    let send = |from: &str, to: &str| {
        let (instance, member) = from.rsplit_once('/').unwrap();
        let start = tree.env.starts(member).last().unwrap().clone();
        tree.env
            .cmd(&["send", to, "Routing check", "--json"])
            .env("BORGCHART_INSTANCE", instance)
            .env("BORGCHART_MEMBER", member)
            .env(
                "BORGCHART_INCARNATION",
                start["env"]["BORGCHART_INCARNATION"].as_str().unwrap(),
            )
            .output()
            .unwrap()
    };
    for (from, to) in [(&parent, &local_child), (&local_child, &parent)] {
        let sent = value(send(from, to));
        let inbox = value(tree.env.ok(&[
            "inbox",
            "--instance",
            &tree.parent,
            "--member",
            to,
            "--json",
            "--full",
        ]));
        assert!(
            inbox["records"]
                .as_array()
                .unwrap()
                .iter()
                .any(|record| { record["id"] == sent["id"] && record["body"] == "Routing check" })
        );
    }
    tree.env.ok(&["create", &format!("{}/dev", tree.parent)]);
    let sibling = tree.env.registry()["instances"]
        .as_object()
        .unwrap()
        .keys()
        .find(|id| id.starts_with(&format!("{}/", tree.parent)) && **id != tree.child)
        .unwrap()
        .clone();
    let sibling_folder = tree.env.registry()["instances"][&sibling]["folder"]
        .as_str()
        .unwrap()
        .to_string();
    fs::create_dir(std::path::Path::new(&sibling_folder).join("repo")).unwrap();
    tree.env.ok(&["start", &sibling]);
    let mut leaves = Vec::new();
    for instance in [&tree.child, &sibling] {
        tree.env.ok(&["create", &format!("{instance}/leaf")]);
        let leaf = tree.env.registry()["instances"]
            .as_object()
            .unwrap()
            .keys()
            .find(|id| id.starts_with(&format!("{instance}/")))
            .unwrap()
            .clone();
        tree.env.ok(&["start", &leaf]);
        let member = tree.env.member(&leaf, "lead");
        leaves.push(format!("{leaf}/{member}"));
    }
    let cousin = send(&leaves[0], &leaves[1]);
    assert_eq!(cousin.status.code(), Some(5), "{}", both(&cousin));
    fs::create_dir(tree.env.path("work/b/lead")).unwrap();
    let unlinked = tree.env.create("oncall", "work/b");
    for receiver in [&sibling, &unlinked] {
        let member = tree.env.member(receiver, "lead");
        let out = publish_to(&tree, true, &format!("{receiver}/{member}"));
        assert_eq!(out.status.code(), Some(5), "{}", both(&out));
        let inbox = value(tree.env.ok(&[
            "inbox",
            "--instance",
            receiver,
            "--member",
            &member,
            "--json",
        ]));
        assert!(inbox["records"].as_array().unwrap().is_empty());
    }

    // Model the interval after an ancestor's tombstone is saved and before
    // its state folder is removed: the recipient and its own parent still exist.
    let record_path = tree
        .env
        .path(&format!("state/{}/instance.json", tree.parent));
    let original = fs::read(&record_path).unwrap();
    let mut record: Value = serde_json::from_slice(&original).unwrap();
    let copy = tree.child.rsplit('/').next().unwrap();
    record["copies"][copy]["deleted"] = json!(true);
    fs::write(&record_path, record.to_string()).unwrap();
    let refused = tree.env.brg(&[
        "send",
        &leaves[0],
        "Deleted ancestor",
        "--external",
        "ops",
        "--json",
    ]);
    fs::write(&record_path, original).unwrap();
    assert!(!refused.status.success(), "{}", both(&refused));
    let checked = value(
        tree.env
            .ok(&["bus", "check", "--instance", &tree.parent, "--json"]),
    );
    assert_eq!(checked["records"], 2, "refused routes must not publish");
}

// Coverage only: publish-time routing already enforces the root boundary.
#[test]
fn a_copy_non_root_cannot_bypass_its_root_in_either_direction() {
    let tree = Tree::new();
    tree.env.ok(&["create", &format!("{}/helper", tree.child)]);
    let helper = tree.env.member(&tree.child, "helper");
    let out = publish_to(&tree, false, &format!("{}/{helper}", tree.child));
    assert_eq!(out.status.code(), Some(5), "{}", both(&out));
    let start: Value = serde_json::from_slice(
        &fs::read(tree.env.path(&format!(
            "state/{}/members/{helper}/incarnation.json",
            tree.child
        )))
        .unwrap(),
    )
    .unwrap();
    let out = tree
        .member_cmd(
            true,
            &[
                "send",
                &format!("{}/{}", tree.parent, tree.parent_member),
                "Escalation",
                "--json",
            ],
        )
        .env("BORGCHART_MEMBER", &helper)
        .env(
            "BORGCHART_INCARNATION",
            start["incarnation"].as_str().unwrap(),
        )
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(5), "{}", both(&out));
    assert!(tree.inbox(false)["records"].as_array().unwrap().is_empty());
    let inbox = value(tree.env.ok(&[
        "inbox",
        "--instance",
        &tree.child,
        "--member",
        &helper,
        "--json",
    ]));
    assert!(inbox["records"].as_array().unwrap().is_empty());
}

#[test]
fn only_the_tree_root_can_publish_to_an_external_caller() {
    let tree = Tree::new();
    let refused = publish_to(&tree, true, "external:ops");
    assert_eq!(refused.status.code(), Some(5), "{}", both(&refused));
    value(publish_to(&tree, false, "external:ops"));
    let outside = value(tree.env.ok(&[
        "inbox",
        "--instance",
        &tree.parent,
        "--external",
        "ops",
        "--full",
        "--json",
    ]));
    assert_eq!(outside["records"].as_array().unwrap().len(), 1);
    assert_eq!(outside["records"][0]["body"], "Routing check");
}

#[test]
fn a_child_publish_wakes_a_parent_already_waiting() {
    let tree = Tree::new();
    let ready = tree.env.path("wait-ready");
    let waiting = tree
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
    let sent = tree.publish(true, "Wake the parent", &[]);
    let inbox = value(finish(waiting));
    assert_eq!(inbox["records"][0]["id"], sent["id"]);
    assert_eq!(inbox["records"][0]["body"], "Wake the parent");
}

#[test]
fn stopped_and_restarted_copy_sessions_cannot_send_ack_or_wait() {
    let tree = Tree::new();
    let sent = tree.publish(false, "Child task", &[]);
    let id = sent["id"].as_str().unwrap();
    for restart in [false, true] {
        let mut send = tree.member_cmd(
            true,
            &[
                "send",
                &format!("{}/{}", tree.parent, tree.parent_member),
                "Stale report",
                "--json",
            ],
        );
        let mut ack = tree.member_cmd(true, &["msg", "ack", id, "--json"]);
        let mut wait = tree.member_cmd(true, &["inbox", "wait", "--timeout", "60", "--json"]);
        tree.env.ok(&["stop", &tree.child]);
        if restart {
            tree.env.ok(&["start", &tree.child]);
        }
        for (cmd, code) in [(&mut send, 5), (&mut ack, 8), (&mut wait, 8)] {
            let out = cmd.output().unwrap();
            assert_eq!(out.status.code(), Some(code), "{}", both(&out));
        }
        if !restart {
            tree.env.ok(&["start", &tree.child]);
        }
    }
    assert_eq!(tree.inbox(true)["records"][0]["id"], id);
}

#[test]
fn a_copy_restart_fences_a_publish_waiting_for_the_tree_lock() {
    let tree = Tree::new();
    let gate = tree.env.path("publish-pause");
    let publishing = tree
        .member_cmd(
            true,
            &[
                "send",
                &format!("{}/{}", tree.parent, tree.parent_member),
                "Old session",
                "--json",
            ],
        )
        .env("BORGCHART_TEST_PUBLISH_PAUSE", &gate)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    tree.env.wait_for("publish preliminary check", || {
        gate.with_extension("waiting").exists()
    });
    tree.env.ok(&["stop", &tree.child]);
    tree.env.ok(&["start", &tree.child]);
    fs::write(&gate, b"").unwrap();
    let out = finish(publishing);
    assert_eq!(out.status.code(), Some(5), "{}", both(&out));
    assert!(tree.inbox(false)["records"].as_array().unwrap().is_empty());
}

#[test]
fn stopping_a_copy_waits_for_a_publication_already_authorized_in_the_tree() {
    let tree = Tree::new();
    let gate = tree.env.path("commit-pause");
    let publishing = tree
        .member_cmd(
            true,
            &[
                "send",
                &format!("{}/{}", tree.parent, tree.parent_member),
                "Final report",
                "--json",
            ],
        )
        .env("BORGCHART_TEST_PUBLISH_COMMIT_PAUSE", &gate)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    tree.env
        .wait_for("publication authorized under the tree lock", || {
            gate.with_extension("waiting").exists()
        });
    let mut stopping = tree
        .env
        .cmd(&["stop", &tree.child])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_millis(300);
    while stopping.try_wait().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let stopped_before_commit = stopping.try_wait().unwrap().is_some();
    fs::write(&gate, b"").unwrap();
    let sent = value(finish(publishing));
    let stopped = finish(stopping);
    assert!(stopped.status.success(), "{}", both(&stopped));
    assert!(
        !stopped_before_commit,
        "stop must not end an authorized sender before it commits"
    );
    assert_eq!(tree.inbox(false)["records"][0]["id"], sent["id"]);
}

#[test]
fn deleting_a_copy_preserves_history_without_leaving_pending_work() {
    let tree = Tree::new();
    let sent = tree.publish(false, "Copy task", &[]);
    let id = sent["id"].as_str().unwrap();
    tree.env.ok(&["stop", &tree.child]);
    let refused = tree.env.fails(&["delete", &tree.child]);
    assert!(refused.contains(id), "{refused}");
    tree.env.ok(&["delete", &tree.child, "--force"]);
    let read = value(
        tree.env
            .ok(&["msg", "read", id, "--instance", &tree.parent, "--json"]),
    );
    assert_eq!(read["record"]["body"], "Copy task");
    assert_eq!(
        read["record"]["to"],
        format!("{}/{}", tree.child, tree.child_member)
    );
    let refused = tree.send(
        false,
        &format!("{}/{}", tree.child, tree.child_member),
        "Deleted target",
        &[],
    );
    assert!(!refused.status.success(), "{}", both(&refused));
    assert_eq!(tree.env.row(&tree.parent, "lead")["unread"], 0);
    tree.env.ok(&["stop", &tree.parent]);
    tree.env.ok(&["delete", &tree.parent]);
}

#[test]
fn copy_delete_and_publish_cannot_orphan_a_message() {
    for delete_holds_lock in [false, true] {
        let tree = Tree::new();
        tree.env.ok(&["stop", &tree.child]);
        let publish_gate = tree.env.path("publish-pause");
        let mut publishing = tree
            .member_cmd(
                false,
                &[
                    "send",
                    &format!("{}/{}", tree.child, tree.child_member),
                    "Concurrent task",
                    "--json",
                ],
            )
            .env("BORGCHART_TEST_PUBLISH_PAUSE", &publish_gate)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        tree.env.wait_for("publish preliminary check", || {
            publish_gate.with_extension("waiting").exists()
        });
        if delete_holds_lock {
            let delete_gate = tree.env.path("delete-pause");
            let deleting = tree
                .env
                .cmd(&["delete", &tree.child])
                .env("BORGCHART_TEST_DELETE_PAUSE", &delete_gate)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            tree.env.wait_for("delete open-work check", || {
                delete_gate.with_extension("waiting").exists()
            });
            fs::write(&publish_gate, b"").unwrap();
            // Let the ready publisher run while delete is paused after its
            // open-work check. It must not commit into that unchecked interval.
            let deadline = Instant::now() + Duration::from_millis(300);
            while publishing.try_wait().unwrap().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            fs::write(&delete_gate, b"").unwrap();
            let deleted = finish(deleting);
            assert!(deleted.status.success(), "{}", both(&deleted));
        } else {
            tree.env.ok(&["delete", &tree.child]);
            fs::write(&publish_gate, b"").unwrap();
        }
        let out = finish(publishing);
        assert!(!out.status.success(), "{}", both(&out));
        let checked = value(
            tree.env
                .ok(&["bus", "check", "--instance", &tree.parent, "--json"]),
        );
        assert_eq!(checked["records"], 0);
    }
}

#[test]
fn a_copy_cannot_initialize_or_ignore_a_damaged_tree_store() {
    for damage in ["missing", "record"] {
        let tree = Tree::new();
        tree.publish(false, "Durable task", &[]);
        tree.env.ok(&["stop", &tree.child]);
        let store = tree.env.path(&format!("state/{}/messages", tree.parent));
        if damage == "missing" {
            fs::remove_dir_all(&store).unwrap();
        } else {
            fs::write(store.join("records/0000000001.json"), b"damaged").unwrap();
        }
        let out = tree.env.brg(&["start", &tree.child]);
        assert!(!out.status.success(), "{}", both(&out));
        assert!(
            !tree
                .env
                .path(&format!("state/{}/messages", tree.child))
                .exists()
        );
        if damage == "missing" {
            assert!(!store.exists(), "a copy must not initialize the root store");
        }
    }
}

#[test]
fn cross_edge_relays_are_refused_before_publication() {
    let tree = Tree::new();
    let out = tree
        .member_cmd(
            true,
            &[
                "send",
                &format!("{}/{}", tree.parent, tree.parent_member),
                "Relayed",
                "--from",
                &tree.child_member,
                "--json",
            ],
        )
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(5), "{}", both(&out));
    assert!(tree.inbox(false)["records"].as_array().unwrap().is_empty());
}
