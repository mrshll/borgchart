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
            "bus: {}\nroles:\n  lead: {prompt: lead.md, cwd: lead}\n  dev: {team: teams/dev, parent: lead, start: false, max: 2, cwd: 'dev/{member_id}'}\n",
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

fn assert_peer_note(out: &Value, instance: &str) {
    let notes = out["notes"].as_array().expect("peer trouble needs a note");
    assert!(
        notes
            .iter()
            .any(|note| note.as_str().unwrap().contains(instance)),
        "{out}"
    );
}

#[test]
fn peer_failures_preserve_local_inbox_and_wait() {
    for failure in [
        "copy", "metadata", "record", "records", "missing", "lock", "busy",
    ] {
        let tree = Tree::new();
        value(tree.env.publish(
            &tree.parent,
            &json!({"to": tree.parent_member, "body": "Local work"}),
        ));
        tree.publish(true, "Remote work", &[]);
        let root = tree.env.path(&format!("state/{}", tree.child));
        let path = root.join(match failure {
            "copy" => "instance.json",
            "metadata" => "messages/store.json",
            "record" => "messages/records/0000000001.json",
            "records" => "messages/records",
            "lock" => "messages/lock",
            _ => "messages",
        });
        let lock = if failure == "busy" {
            let file = fs::File::open(root.join("messages/lock")).unwrap();
            file.lock().unwrap();
            Some(file)
        } else {
            None
        };
        if failure == "missing" {
            fs::rename(&path, root.join("hidden-messages")).unwrap();
        } else if failure == "lock" {
            fs::remove_file(&path).unwrap();
        } else if failure != "busy" {
            // Corrupt metadata is unreadable regardless of the test user's UID.
            if failure == "records" {
                fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
            } else {
                fs::write(&path, "unreadable").unwrap();
            }
        }
        for args in [
            vec!["inbox", "--full", "--json"],
            vec!["inbox", "wait", "--timeout", "60", "--full", "--json"],
        ] {
            let out = value(finish(
                tree.member_cmd(false, &args)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap(),
            ));
            assert_eq!(out["records"][0]["body"], "Local work", "{failure}: {out}");
            assert_peer_note(&out, &tree.child);
        }
        drop(lock);
        if failure == "records" {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
}

#[test]
fn peer_failures_do_not_replace_wait_timeout_or_fencing() {
    let tree = Tree::new();
    let lock = fs::File::open(
        tree.env
            .path(&format!("state/{}/messages/lock", tree.child)),
    )
    .unwrap();
    lock.lock().unwrap();
    let started = Instant::now();
    let out = finish(
        tree.member_cmd(false, &["inbox", "wait", "--timeout", "60", "--json"])
            .env("BORGCHART_WAIT_TIMEOUT_MS", "700")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    assert_eq!(out.status.code(), Some(9), "{}", both(&out));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_peer_note(&serde_json::from_slice(&out.stdout).unwrap(), &tree.child);
    let arrival_ready = tree.env.path("wait-ready-local-arrival");
    let waiting = tree
        .member_cmd(
            false,
            &["inbox", "wait", "--timeout", "60", "--full", "--json"],
        )
        .env("BORGCHART_TEST_WAIT_READY", &arrival_ready)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    tree.env
        .wait_for("wait before a local arrival", || arrival_ready.exists());
    value(tree.env.publish(
        &tree.parent,
        &json!({"to": tree.parent_member, "body": "Local arrival"}),
    ));
    let inbox = value(finish(waiting));
    assert_eq!(inbox["records"][0]["body"], "Local arrival");
    assert_peer_note(&inbox, &tree.child);
    value(
        tree.member_cmd(
            false,
            &[
                "msg",
                "ack",
                inbox["records"][0]["id"].as_str().unwrap(),
                "--json",
            ],
        )
        .output()
        .unwrap(),
    );
    let ready = tree.env.path("wait-ready-busy-peer");
    let waiting = tree
        .member_cmd(false, &["inbox", "wait", "--timeout", "60", "--json"])
        .env("BORGCHART_TEST_WAIT_READY", &ready)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    tree.env
        .wait_for("wait with a busy peer", || ready.exists());
    tree.env
        .ok(&["stop", &format!("{}/{}", tree.parent, tree.parent_member)]);
    let out = finish(waiting);
    assert_eq!(out.status.code(), Some(8), "{}", both(&out));
    assert_peer_note(&serde_json::from_slice(&out.stdout).unwrap(), &tree.child);
}

#[test]
fn wait_discovers_a_copy_created_or_first_started_after_it_begins() {
    for create_during_wait in [false, true] {
        let mut tree = Tree::new();
        let previous = tree.child.clone();
        if !create_during_wait {
            tree.env.ok(&["create", &format!("{}/dev", tree.parent)]);
        }
        let ready = tree.env.path("wait-ready-new-edge");
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
        tree.env
            .wait_for("wait before a new edge", || ready.exists());
        if create_during_wait {
            tree.env.ok(&["create", &format!("{}/dev", tree.parent)]);
        }
        tree.child = tree.env.registry()["instances"]
            .as_object()
            .unwrap()
            .keys()
            .find(|id| id.starts_with(&format!("{}/", tree.parent)) && **id != previous)
            .unwrap()
            .clone();
        tree.env.ok(&["start", &tree.child]);
        tree.child_member = tree.env.member(&tree.child, "lead");
        tree.publish(true, "New child report", &[]);
        let out = value(finish(waiting));
        assert_eq!(out["records"][0]["body"], "New child report");
    }
}

#[test]
fn a_busy_local_import_lock_respects_inbox_and_wait_deadlines() {
    let tree = Tree::new();
    value(tree.env.publish(
        &tree.parent,
        &json!({"to": tree.parent_member, "body": "Readable local work"}),
    ));
    tree.publish(true, "Pending import", &[]);
    for wait in [false, true] {
        if wait {
            value(
                tree.member_cmd(false, &["msg", "ack", "m1", "--json"])
                    .output()
                    .unwrap(),
            );
        }
        let gate = tree.env.path(if wait {
            "local-wait-gate"
        } else {
            "local-inbox-gate"
        });
        fs::create_dir(&gate).unwrap();
        let args = if wait {
            vec!["inbox", "wait", "--timeout", "60", "--json"]
        } else {
            vec!["inbox", "--full", "--json"]
        };
        let child = tree
            .member_cmd(false, &args)
            .env("BORGCHART_TEST_IMPORT_GATE", &gate)
            .env(
                "BORGCHART_TEST_WAIT_READY",
                tree.env.path("wait-ready-busy-local"),
            )
            .env("BORGCHART_WAIT_TIMEOUT_MS", "5000")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        tree.env.wait_for("import reached the local lock", || {
            fs::read_dir(&gate).unwrap().count() == 1
        });
        let lock = fs::File::open(
            tree.env
                .path(&format!("state/{}/messages/lock", tree.parent)),
        )
        .unwrap();
        lock.lock_shared().unwrap();
        let started = Instant::now();
        fs::write(gate.join("proceed"), "").unwrap();
        if wait {
            let ready = tree.env.path("wait-ready-busy-local");
            tree.env
                .wait_for("wait while local import is busy", || ready.exists());
            // A real stop needs this lock to end the incarnation. Simulate
            // its ended start so this tests fencing independently of stop.
            let path = tree.env.path(&format!(
                "state/{}/members/{}/incarnation.json",
                tree.parent, tree.parent_member
            ));
            let mut incarnation: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            incarnation["ended_at"] = json!("2026-01-01T00:00:00Z");
            let replacement = path.with_extension("replacement");
            fs::write(&replacement, serde_json::to_vec(&incarnation).unwrap()).unwrap();
            fs::rename(replacement, path).unwrap();
            let out = finish(child);
            drop(lock);
            assert_eq!(out.status.code(), Some(8), "{}", both(&out));
            assert!(started.elapsed() < Duration::from_secs(3));
            let out: Value = serde_json::from_slice(&out.stdout).unwrap();
            assert!(!out["notes"].as_array().unwrap().is_empty(), "{out}");
            continue;
        }
        let out = finish(child);
        let out = value(out);
        assert_eq!(out["records"][0]["body"], "Readable local work");
        assert!(!out["notes"].as_array().unwrap().is_empty(), "{out}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}

#[test]
fn busy_import_retries_cached_peer_records_after_the_local_lock_is_released() {
    let tree = Tree::new();
    tree.publish(true, "First pending report", &[]);
    let gate = tree.env.path("retry-import-gate");
    fs::create_dir(&gate).unwrap();
    let ready = tree.env.path("wait-ready-retry");
    let reads = tree.env.path("retry-peer-reads");
    let waiting = tree
        .member_cmd(
            false,
            &["inbox", "wait", "--timeout", "60", "--full", "--json"],
        )
        .env("BORGCHART_TEST_IMPORT_GATE", &gate)
        .env("BORGCHART_TEST_WAIT_READY", &ready)
        .env("BORGCHART_TEST_PEER_READ_LOG", &reads)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    tree.env.wait_for("import before a busy local lock", || {
        fs::read_dir(&gate).unwrap().count() == 1
    });
    let lock = fs::File::open(
        tree.env
            .path(&format!("state/{}/messages/lock", tree.parent)),
    )
    .unwrap();
    lock.lock_shared().unwrap();
    fs::write(gate.join("proceed"), "").unwrap();
    tree.env
        .wait_for("wait after deferred import", || ready.exists());
    tree.publish(true, "Second pending report", &[]);
    tree.env
        .wait_for("both reports retained during contention", || {
            fs::read_to_string(&reads)
                .unwrap_or_default()
                .lines()
                .any(|line| line == "next:3")
        });
    drop(lock);
    let inbox = value(finish(waiting));
    let bodies: Vec<_> = inbox["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| record["body"].as_str().unwrap())
        .collect();
    assert_eq!(bodies, ["First pending report", "Second pending report"]);
}

#[test]
fn already_imported_origins_do_not_take_the_publication_lock() {
    let tree = Tree::new();
    tree.publish(true, "Already delivered", &[]);
    tree.inbox(false);
    let gate = tree.env.path("unneeded-import");
    fs::create_dir(&gate).unwrap();
    fs::write(gate.join("proceed"), "").unwrap();
    let lock = fs::File::open(
        tree.env
            .path(&format!("state/{}/messages/lock", tree.parent)),
    )
    .unwrap();
    lock.lock_shared().unwrap();
    let out = value(
        tree.member_cmd(false, &["inbox", "--full", "--json"])
            .env("BORGCHART_TEST_IMPORT_GATE", &gate)
            .output()
            .unwrap(),
    );
    assert_eq!(out["records"][0]["body"], "Already delivered");
    assert_eq!(fs::read_dir(&gate).unwrap().count(), 1);
    assert!(out["notes"].as_array().unwrap().is_empty(), "{out}");
}

#[test]
fn wait_reads_peer_history_once_then_only_new_sequences() {
    let tree = Tree::new();
    tree.publish(true, "Previous report", &[]);
    let inbox = tree.inbox(false);
    value(
        tree.member_cmd(
            false,
            &[
                "msg",
                "ack",
                inbox["records"][0]["id"].as_str().unwrap(),
                "--json",
            ],
        )
        .output()
        .unwrap(),
    );
    let log = tree.env.path("peer-reads");
    let out = finish(
        tree.member_cmd(false, &["inbox", "wait", "--timeout", "60", "--json"])
            .env("BORGCHART_TEST_PEER_READ_LOG", &log)
            .env("BORGCHART_WAIT_TIMEOUT_MS", "1100")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    assert_eq!(out.status.code(), Some(9), "{}", both(&out));
    let reads = fs::read_to_string(log).unwrap();
    assert_eq!(
        reads.lines().filter(|line| *line == "full").count(),
        1,
        "{reads}"
    );
    assert!(
        reads
            .lines()
            .filter(|line| line.starts_with("next:"))
            .count()
            >= 2,
        "{reads}"
    );
}

#[test]
fn a_damaged_local_import_still_lists_readable_inbox_records() {
    let tree = Tree::new();
    for body in ["Readable local work", "Damaged work"] {
        value(tree.env.publish(
            &tree.parent,
            &json!({"to": tree.parent_member, "body": body}),
        ));
    }
    tree.publish(true, "Pending import", &[]);
    fs::write(
        tree.env.path(&format!(
            "state/{}/messages/records/0000000002.json",
            tree.parent
        )),
        "unreadable",
    )
    .unwrap();
    let inbox = tree.inbox(false);
    assert_eq!(inbox["store"], "incomplete");
    assert_eq!(inbox["records"][0]["body"], "Readable local work");
    assert!(!inbox["notes"].as_array().unwrap().is_empty(), "{inbox}");
}

#[test]
fn status_distinguishes_a_busy_local_store_from_damage() {
    let tree = Tree::new();
    let lock = fs::File::open(
        tree.env
            .path(&format!("state/{}/messages/lock", tree.parent)),
    )
    .unwrap();
    lock.lock().unwrap();
    let row = tree.env.row(&tree.parent, "lead");
    assert!(row["unread"].is_null(), "{row}");
    // The retry action, rather than a repair command, is the status contract.
    let detail = row["detail"].as_str().unwrap();
    assert!(detail.contains("try again"), "{row}");
    assert!(!detail.contains("bus check"), "{row}");
}

#[test]
fn cross_edge_publish_reserves_space_for_the_import_envelope() {
    let tree = Tree::new();
    let probe = tree.publish(true, "", &[]);
    let record = value(
        tree.member_cmd(
            true,
            &["msg", "read", probe["id"].as_str().unwrap(), "--json"],
        )
        .output()
        .unwrap(),
    )["record"]
        .clone();
    let mut record = record;
    record.as_object_mut().unwrap().remove("acked");
    let room = 128 * 1024 - serde_json::to_vec_pretty(&record).unwrap().len();
    let body = "\"".repeat((room - 30) / 2);
    let draft = tree.env.path("oversize-draft.json");
    fs::write(
        &draft,
        json!({"to": format!("{}/{}", tree.parent, tree.parent_member), "body": body}).to_string(),
    )
    .unwrap();
    let out = tree
        .member_cmd(
            true,
            &[
                "msg",
                "publish",
                "--record",
                draft.to_str().unwrap(),
                "--json",
            ],
        )
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(5),
        "publication must refuse before import gets stuck: {}",
        both(&out)
    );
    let accepted = tree.publish(true, &"\"".repeat((room - 2048) / 2), &[]);
    let inbox = tree.inbox(false);
    assert!(
        inbox["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["origin"]["id"] == accepted["id"])
    );
}

#[test]
fn status_counts_pending_edge_records_once_and_reports_unknown_peers() {
    let tree = Tree::new();
    tree.publish(true, "Pending in child", &[]);
    let before = tree
        .env
        .ok(&["bus", "check", "--instance", &tree.parent, "--json"]);
    assert_eq!(
        tree.env.row(&tree.parent, "lead")["unread"],
        1,
        "status must include messages before import"
    );
    let after = tree
        .env
        .ok(&["bus", "check", "--instance", &tree.parent, "--json"]);
    assert_eq!(
        before.stdout, after.stdout,
        "status must not import into a store"
    );
    let inbox = tree.inbox(false);
    assert_eq!(
        tree.env.row(&tree.parent, "lead")["unread"],
        1,
        "import must not double-count"
    );
    value(
        tree.member_cmd(
            false,
            &[
                "msg",
                "ack",
                inbox["records"][0]["id"].as_str().unwrap(),
                "--json",
            ],
        )
        .output()
        .unwrap(),
    );
    assert_eq!(
        tree.env.row(&tree.parent, "lead")["unread"],
        0,
        "peer originals must not resurrect acked imports"
    );
    tree.publish(false, "Pending in parent", &[]);
    assert_eq!(
        tree.env.row(&tree.child, "lead")["unread"],
        1,
        "a child's count includes its parent store"
    );
    let lock = fs::File::open(
        tree.env
            .path(&format!("state/{}/messages/lock", tree.child)),
    )
    .unwrap();
    lock.lock().unwrap();
    let row = tree.env.row(&tree.parent, "lead");
    assert!(
        row["unread"].is_null(),
        "an unreadable peer cannot mean zero: {row}"
    );
    assert!(
        row["detail"].as_str().unwrap().contains(&tree.child),
        "{row}"
    );
}

#[test]
fn sender_delete_waits_for_import_but_not_the_receivers_ack() {
    for member_only in [true, false] {
        let tree = Tree::new();
        // A receiver's unrelated m1 must not count as a delivery receipt.
        value(tree.env.publish(
            &tree.parent,
            &json!({"to": tree.parent_member, "body": "Unrelated local task"}),
        ));
        tree.publish(true, "Report awaiting delivery", &[]);
        let sender = if member_only {
            format!("{}/{}", tree.child, tree.child_member)
        } else {
            tree.child.clone()
        };
        tree.env.ok(&["stop", &sender]);
        tree.env.fails(&["delete", &sender]);
        assert_eq!(tree.inbox(false)["records"].as_array().unwrap().len(), 2);
        let receiver = tree.env.path(&format!("state/{}/messages", tree.parent));
        fs::set_permissions(&receiver, fs::Permissions::from_mode(0o500)).unwrap();
        let deleted = tree.env.brg(&["delete", &sender]);
        fs::set_permissions(&receiver, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(deleted.status.success(), "{}", both(&deleted));
        assert_eq!(
            tree.inbox(false)["records"].as_array().unwrap().len(),
            2,
            "delivery must release the sender without acking the receiver's inbox"
        );
    }
}

#[test]
fn sender_delete_lists_messages_to_a_deleted_copy() {
    for member_only in [true, false] {
        let tree = Tree::new();
        tree.publish(false, "Task never imported", &[]);
        tree.env.ok(&["stop", &tree.child]);
        tree.env.ok(&["delete", &tree.child]);
        let sender = if member_only {
            format!("{}/{}", tree.parent, tree.parent_member)
        } else {
            tree.parent.clone()
        };
        tree.env.ok(&["stop", &sender]);
        let pending = tree.env.fails(&["delete", &sender]);
        assert!(pending.contains("to a deleted copy"), "{pending}");
        tree.env.ok(&["delete", &sender, "--force"]);
    }
}

#[test]
fn sender_delete_releases_messages_to_a_member_deleted_before_import() {
    for member_only in [true, false] {
        let tree = Tree::new();
        tree.publish(true, "Report never imported", &[]);
        tree.env.ok(&["stop", &tree.parent]);
        // Kept-copy rules currently prevent deleting the parent member through
        // the CLI. Model its tombstone to guard receipt accounting if that
        // state becomes reachable; the receiver's store still holds no import.
        let path = tree
            .env
            .path(&format!("state/{}/instance.json", tree.parent));
        let mut record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        record["members"][&tree.parent_member]["deleted"] = json!(true);
        fs::write(path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
        let sender = if member_only {
            format!("{}/{}", tree.child, tree.child_member)
        } else {
            tree.child.clone()
        };
        tree.env.ok(&["stop", &sender]);
        tree.env.ok(&["delete", &sender]);
    }
}

#[test]
fn sender_delete_does_not_hide_unreadable_receipts() {
    for member_only in [true, false] {
        let tree = Tree::new();
        tree.publish(true, "Delivered but not inspectable", &[]);
        tree.inbox(false);
        let sender = if member_only {
            format!("{}/{}", tree.child, tree.child_member)
        } else {
            tree.child.clone()
        };
        tree.env.ok(&["stop", &sender]);
        let receiver = tree.env.path(&format!("state/{}/messages", tree.parent));
        fs::set_permissions(&receiver, fs::Permissions::from_mode(0o000)).unwrap();
        let refused = tree.env.brg(&["delete", &sender]);
        let forced = tree.env.brg(&["delete", &sender, "--force"]);
        fs::set_permissions(&receiver, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!refused.status.success(), "{}", both(&refused));
        assert!(forced.status.success(), "{}", both(&forced));
    }
}

#[test]
fn receiver_member_delete_requires_ack_for_imported_messages() {
    let tree = Tree::new();
    tree.publish(false, "Imported unread task", &[]);
    let inbox = tree.inbox(true);
    let id = inbox["records"][0]["id"].as_str().unwrap();
    let receiver = format!("{}/{}", tree.child, tree.child_member);
    tree.env.ok(&["stop", &receiver]);
    tree.env.fails(&["delete", &receiver]);
    tree.env.ok(&["start", &receiver]);
    value(
        tree.member_cmd(true, &["msg", "ack", id, "--json"])
            .output()
            .unwrap(),
    );
    tree.env.ok(&["stop", &receiver]);
    tree.env.ok(&["delete", &receiver]);
}

#[test]
fn receiver_member_delete_refuses_unimported_messages_without_importing() {
    let tree = Tree::new();
    tree.publish(false, "Pending incoming task", &[]);
    let receiver = format!("{}/{}", tree.child, tree.child_member);
    tree.env.ok(&["stop", &receiver]);
    let before = tree
        .env
        .ok(&["bus", "check", "--instance", &tree.child, "--json"]);
    tree.env.fails(&["delete", &receiver]);
    let after = tree
        .env
        .ok(&["bus", "check", "--instance", &tree.child, "--json"]);
    assert_eq!(before.stdout, after.stdout, "delete must not import");
    tree.env.ok(&["delete", &receiver, "--force"]);
}

#[test]
fn receiver_member_delete_requires_force_when_its_edge_is_unreadable() {
    let tree = Tree::new();
    let receiver = format!("{}/{}", tree.child, tree.child_member);
    tree.env.ok(&["stop", &receiver]);
    let lock = fs::File::open(
        tree.env
            .path(&format!("state/{}/messages/lock", tree.parent)),
    )
    .unwrap();
    lock.lock().unwrap();
    tree.env.fails(&["delete", &receiver]);
    tree.env.ok(&["delete", &receiver, "--force"]);
}

fn publish_to(tree: &Tree, child: bool, to: &str) -> Output {
    tree.env.write(
        "route-draft.json",
        &json!({"to": to, "body": "Routing check"}).to_string(),
    );
    tree.member_cmd(
        child,
        &[
            "msg",
            "publish",
            "--record",
            tree.env.path("route-draft.json").to_str().unwrap(),
            "--json",
        ],
    )
    .output()
    .unwrap()
}

#[test]
fn sibling_and_unlinked_member_routes_are_refused() {
    let tree = Tree::new();
    tree.env.ok(&["create", &format!("{}/dev", tree.parent)]);
    let sibling = tree.env.registry()["instances"]
        .as_object()
        .unwrap()
        .keys()
        .find(|id| id.starts_with(&format!("{}/", tree.parent)) && **id != tree.child)
        .unwrap()
        .clone();
    tree.env.ok(&["start", &sibling]);
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
}

// Coverage only: publish-time routing already enforces the root boundary.
#[test]
fn a_copy_non_root_cannot_bypass_its_root_in_either_direction() {
    let tree = Tree::new();
    tree.env.ok(&["create", &format!("{}/helper", tree.child)]);
    let helper = tree.env.member(&tree.child, "helper");
    let out = publish_to(&tree, false, &format!("{}/{helper}", tree.child));
    assert_eq!(out.status.code(), Some(5), "{}", both(&out));
    tree.env.write(
        "route-draft.json",
        &json!({"to": format!("{}/{}", tree.parent, tree.parent_member), "body": "Escalation"})
            .to_string(),
    );
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
                "msg",
                "publish",
                "--record",
                tree.env.path("route-draft.json").to_str().unwrap(),
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
fn mutual_imports_release_peer_locks_before_appending() {
    let tree = Tree::new();
    tree.publish(false, "Parent task", &[]);
    tree.publish(true, "Child report", &[]);
    let gate = tree.env.path("mutual-import-gate");
    fs::create_dir(&gate).unwrap();
    let spawn = |child| {
        tree.member_cmd(child, &["inbox", "--full", "--json"])
            .env("BORGCHART_TEST_IMPORT_GATE", &gate)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let parent = spawn(false);
    let child = spawn(true);
    tree.env.wait_for("both stores have read their peer", || {
        fs::read_dir(&gate).unwrap().count() == 2
    });
    fs::write(gate.join("proceed"), "").unwrap();
    for (process, body) in [(parent, "Child report"), (child, "Parent task")] {
        let inbox = value(finish(process));
        assert_eq!(inbox["records"].as_array().unwrap().len(), 1);
        assert_eq!(inbox["records"][0]["body"], body);
    }
}
