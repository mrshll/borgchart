//! Plain-message round trips, the one wait and unread status counts.
#![cfg(feature = "test-support")]

mod common;
mod instance_env;

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use instance_env::*;
use serde_json::{Value, json};

fn publish(env: &Env, instance: &str, draft: &Value) -> Output {
    let mut command = env.cmd(&[
        "msg",
        "publish",
        "--instance",
        instance,
        "--external",
        "ops",
        "--json",
    ]);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(draft.to_string().as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn send(env: &Env, instance: &str, to: &str, body: &str) -> Value {
    let out = publish(env, instance, &json!({"to": to, "body": body}));
    assert!(out.status.success(), "{}", both(&out));
    serde_json::from_slice(&out.stdout).unwrap()
}

fn member(env: &Env, instance: &str, id: &str, args: &[&str]) -> Command {
    let start = env.starts(id).last().unwrap().clone();
    let mut cmd = env.cmd(args);
    cmd.env("BORGCHART_INSTANCE", instance)
        .env("BORGCHART_MEMBER", id)
        .env(
            "BORGCHART_INCARNATION",
            start["env"]["BORGCHART_INCARNATION"].as_str().unwrap(),
        );
    cmd
}

fn waiter(env: &Env, instance: &str, id: &str) -> Command {
    let mut cmd = member(
        env,
        instance,
        id,
        &["inbox", "wait", "--timeout", "60", "--json", "--full"],
    );
    cmd.env("BORGCHART_WAIT_MIN_MS", "700")
        .env("BORGCHART_WAIT_TIMEOUT_MS", "1400")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

fn finish(mut child: Child) -> Output {
    let deadline = Instant::now() + Duration::from_secs(4);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let out = child.wait_with_output().unwrap();
            panic!("wait did not finish: {}", both(&out));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn plain_message_round_trip_and_missing_reference() {
    let env = Env::new();
    let instance = env.create("oncall", "work/a");
    let lead = env.member(&instance, "lead");
    let out = send(&env, &instance, &lead, "hello");
    assert_eq!(out["id"], "m1");
    let inbox: Value = serde_json::from_slice(
        &env.ok(&[
            "inbox",
            "--instance",
            &instance,
            "--member",
            &lead,
            "--full",
            "--json",
        ])
        .stdout,
    )
    .unwrap();
    assert_eq!(inbox["records"][0]["from"], "external:ops");
    assert_eq!(inbox["records"][0]["body"], "hello");
    assert!(inbox.get("next").is_none());
    let read: Value = serde_json::from_slice(
        &env.ok(&["msg", "read", "m1", "--instance", &instance, "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(read["record"]["from"], "external:ops");
    let missing = publish(
        &env,
        &instance,
        &json!({"to": lead, "body": "reply", "reply_to": ["m9"]}),
    );
    assert_eq!(missing.status.code(), Some(5), "{}", both(&missing));
    let ack = member(&env, &instance, &lead, &["msg", "ack", "m1", "--json"])
        .output()
        .unwrap();
    assert!(ack.status.success(), "{}", both(&ack));
    let inbox: Value = serde_json::from_slice(
        &env.ok(&[
            "inbox",
            "--instance",
            &instance,
            "--member",
            &lead,
            "--json",
        ])
        .stdout,
    )
    .unwrap();
    assert_eq!(inbox["records"], json!([]));
    env.ok(&["stop", &instance]);
}

#[test]
fn a_draft_with_a_key_is_refused_naming_the_field() {
    let env = Env::new();
    let instance = env.create("oncall", "work/a");
    let lead = env.member(&instance, "lead");
    let out = publish(
        &env,
        &instance,
        &json!({"to": lead, "body": "hello", "key": "old"}),
    );
    assert_eq!(out.status.code(), Some(5), "{}", both(&out));
    assert!(both(&out).contains("unknown field `key`"), "{}", both(&out));
    env.ok(&["stop", &instance]);
}

#[test]
fn identical_publishes_are_two_messages_and_inbox_has_no_pages() {
    let env = Env::new();
    let instance = env.create("oncall", "work/a");
    let lead = env.member(&instance, "lead");
    for n in 1..=22 {
        assert_eq!(
            send(&env, &instance, &lead, "same draft")["id"],
            format!("m{n}")
        );
    }
    let inbox: Value = serde_json::from_slice(
        &env.ok(&[
            "inbox",
            "--member",
            &lead,
            "--instance",
            &instance,
            "--json",
        ])
        .stdout,
    )
    .unwrap();
    assert_eq!(inbox["records"].as_array().unwrap().len(), 22);
    assert_eq!(inbox["records"][0]["id"], "m1");
    assert_eq!(inbox["records"][21]["id"], "m22");
    assert!(inbox.get("next").is_none());
    env.ok(&["stop", &instance]);
}

#[test]
fn a_new_message_returns_before_the_minimum_with_the_whole_inbox() {
    let env = Env::new();
    let instance = env.create("oncall", "work/a");
    let lead = env.member(&instance, "lead");
    send(&env, &instance, &lead, "old");
    let start = Instant::now();
    let child = waiter(&env, &instance, &lead).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    send(&env, &instance, &lead, "new");
    let out = finish(child);
    assert!(out.status.success(), "{}", both(&out));
    assert!(start.elapsed() < Duration::from_millis(700));
    let inbox: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(inbox["records"].as_array().unwrap().len(), 2);
    assert_eq!(inbox["records"][1]["body"], "new");
    env.ok(&["stop", &instance]);
}

#[test]
fn existing_messages_wait_for_the_minimum_and_match_inbox_output() {
    let env = Env::new();
    let instance = env.create("oncall", "work/a");
    let lead = env.member(&instance, "lead");
    send(&env, &instance, &lead, "old");
    let start = Instant::now();
    let out = finish(waiter(&env, &instance, &lead).spawn().unwrap());
    assert!(out.status.success(), "{}", both(&out));
    assert!(start.elapsed() >= Duration::from_millis(700));
    let expected = env.ok(&[
        "inbox",
        "--instance",
        &instance,
        "--member",
        &lead,
        "--json",
        "--full",
    ]);
    assert_eq!(out.stdout, expected.stdout);
    // Acknowledging everything during the wait must not cause an empty wake
    // at the minimum. A later message still wakes it.
    let start = Instant::now();
    let mut child = waiter(&env, &instance, &lead)
        .env("BORGCHART_WAIT_TIMEOUT_MS", "3000")
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let ack = member(&env, &instance, &lead, &["msg", "ack", "m1"])
        .output()
        .unwrap();
    assert!(ack.status.success(), "{}", both(&ack));
    std::thread::sleep(Duration::from_millis(550));
    assert!(start.elapsed() >= Duration::from_millis(700));
    assert!(child.try_wait().unwrap().is_none(), "empty inbox woke wait");
    send(&env, &instance, &lead, "new after ack");
    let out = finish(child);
    assert!(out.status.success(), "{}", both(&out));
    assert!(start.elapsed() >= Duration::from_millis(700));
    let inbox: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(inbox["records"].as_array().unwrap().len(), 1);
    assert_eq!(inbox["records"][0]["body"], "new after ack");
    env.ok(&["stop", &instance]);
}

#[test]
fn wait_errors_and_usage_errors_obey_the_minimum() {
    let env = Env::new();
    for args in [
        vec!["inbox", "wait", "--timeout", "60"],
        vec!["inbox", "wait", "--timeout", "bad"],
    ] {
        let start = Instant::now();
        let out = env
            .cmd(&args)
            .env("BORGCHART_WAIT_MIN_MS", "700")
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(
            start.elapsed() >= Duration::from_millis(700),
            "{}",
            both(&out)
        );
    }
}

#[test]
fn an_empty_wait_times_out_and_a_replaced_wait_is_fenced() {
    let env = Env::new();
    let instance = env.create("oncall", "work/a");
    let lead = env.member(&instance, "lead");
    let out = finish(waiter(&env, &instance, &lead).spawn().unwrap());
    assert_eq!(out.status.code(), Some(9), "{}", both(&out));
    let start = Instant::now();
    let child = waiter(&env, &instance, &lead).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    env.ok(&["stop", &format!("{instance}/{lead}")]);
    env.ok(&["start", &format!("{instance}/{lead}")]);
    let out = finish(child);
    assert_eq!(out.status.code(), Some(8), "{}", both(&out));
    assert!(start.elapsed() >= Duration::from_millis(700));
    env.ok(&["stop", &instance]);
}

#[test]
fn polling_does_not_reread_an_old_record() {
    let env = Env::new();
    let instance = env.create("oncall", "work/a");
    let lead = env.member(&instance, "lead");
    send(&env, &instance, &lead, "old");
    let mut cmd = waiter(&env, &instance, &lead);
    cmd.env("BORGCHART_WAIT_MIN_MS", "2000")
        .env("BORGCHART_WAIT_TIMEOUT_MS", "3000");
    let child = cmd.spawn().unwrap();
    std::thread::sleep(Duration::from_millis(350));
    let records = env.path(&format!("state/{instance}/messages/records"));
    let old = records.join("0000000001.json");
    fs::set_permissions(&old, fs::Permissions::from_mode(0o000)).unwrap();
    // Simulate a whole committed next record: a real sender still refuses
    // damaged old data. This tests the wait's incremental reader only.
    let record = json!({"schema": 2, "id": "m2", "seq": 2, "at": "2026-11-08T00:00:00Z", "from": "external:ops", "to": lead, "body": "new"});
    borgchart::bus::fsx::write_new(
        &records.join("0000000002.json"),
        record.to_string().as_bytes(),
    )
    .unwrap();
    let out = finish(child);
    fs::set_permissions(old, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(out.status.success(), "{}", both(&out));
    let inbox: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(inbox["records"][1]["body"], "new");
    env.ok(&["stop", &instance]);
}

#[test]
fn status_shows_unacked_counts_and_survives_store_damage() {
    let env = Env::new();
    let instance = env.create("oncall", "work/a");
    env.ok(&["create", &format!("{instance}/helper")]);
    let lead = env.member(&instance, "lead");
    let helper = env.member(&instance, "helper");
    send(&env, &instance, &lead, "one");
    send(&env, &instance, &lead, "two");
    send(&env, &instance, &helper, "three");
    let rows = env.rows(&instance);
    assert_eq!(
        rows.iter().find(|r| r["member"] == lead).unwrap()["unread"],
        2
    );
    assert_eq!(
        rows.iter().find(|r| r["member"] == helper).unwrap()["unread"],
        1
    );
    let text = stdout(&env.ok(&["status", &instance]));
    assert!(
        text.contains("2 unread") && text.contains("1 unread"),
        "{text}"
    );
    let out = member(&env, &instance, &lead, &["msg", "ack", "m1", "m2"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    assert_eq!(env.row(&instance, "lead")["unread"], 0);
    fs::write(
        env.path(&format!("state/{instance}/messages/store.json")),
        "bad",
    )
    .unwrap();
    let text = stdout(&env.ok(&["status", &instance]));
    assert!(text.contains("messages unavailable"), "{text}");
    assert!(env.row(&instance, "lead").get("unread").is_none());
    env.ok(&["stop", &instance]);
}

#[test]
fn a_damaged_store_and_an_unreadable_next_record_wait_before_erroring() {
    for startup in [true, false] {
        let env = Env::new();
        let instance = env.create("oncall", "work/a");
        let lead = env.member(&instance, "lead");
        if startup {
            fs::write(
                env.path(&format!("state/{instance}/messages/store.json")),
                "bad json",
            )
            .unwrap();
        }
        let start = Instant::now();
        let child = waiter(&env, &instance, &lead).spawn().unwrap();
        if !startup {
            std::thread::sleep(Duration::from_millis(300));
            let path = env.path(&format!(
                "state/{instance}/messages/records/0000000001.json"
            ));
            borgchart::bus::fsx::write_new(&path, b"bad json").unwrap();
        }
        let out = finish(child);
        assert_eq!(out.status.code(), Some(7), "{}", both(&out));
        assert!(start.elapsed() >= Duration::from_millis(700));
        env.ok(&["stop", &instance]);
    }
}

#[test]
fn status_without_a_bus_has_no_unread_counts_or_bus_warning() {
    let env = Env::new();
    env.write("teams/oncall/team.yaml", &TEAM.replace("bus: {}\n", ""));
    let instance = env.create("oncall", "work/a");
    assert!(env.row(&instance, "lead").get("unread").is_none());
    let text = stdout(&env.ok(&["status", &instance]));
    assert!(
        !text.contains("unread") && !text.contains("messages unavailable"),
        "{text}"
    );
    env.ok(&["stop", &instance]);
}
