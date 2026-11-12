//! Plain-message round trips, the one wait and unacked status counts.
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
    let target = format!("{instance}/{}", draft["to"].as_str().unwrap());
    let mut command = env.cmd(&["msg", "send", &target, "-", "--external", "ops", "--json"]);
    if let Some(refs) = draft["reply_to"].as_array() {
        for reference in refs {
            command.args(["--reply-to", reference.as_str().unwrap()]);
        }
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    // brg may refuse and exit before reading the draft; its answer is in
    // the output either way.
    match child
        .stdin
        .take()
        .unwrap()
        .write_all(draft["body"].as_str().unwrap_or("").as_bytes())
    {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        Err(e) => panic!("cannot write the draft: {e}"),
    }
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
        &["msg", "wait", "--timeout", "60", "--json", "--full"],
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
            "msg",
            "list",
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
            "msg",
            "list",
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
fn removed_publish_and_record_input_are_refused() {
    let env = Env::new();
    let out = env
        .cmd(&["msg", "publish", "--record", "old.json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(both(&out).contains("unrecognized subcommand"));
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
            "msg",
            "list",
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
    // A longer minimum than the other cases, so a slow publish on a busy CI
    // runner still lands well before it: only the new message can end the
    // wait before 3 s.
    let child = waiter(&env, &instance, &lead)
        .env("BORGCHART_WAIT_MIN_MS", "3000")
        .env("BORGCHART_WAIT_TIMEOUT_MS", "6000")
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    send(&env, &instance, &lead, "new");
    let out = finish(child);
    assert!(out.status.success(), "{}", both(&out));
    assert!(start.elapsed() < Duration::from_millis(2500));
    let inbox: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(inbox["records"].as_array().unwrap().len(), 2);
    assert_eq!(inbox["records"][1]["body"], "new");
    env.ok(&["stop", &instance]);
}

#[test]
fn ack_refresh_catches_up_the_locked_tail_before_validating_acks() {
    let env = Env::new();
    let instance = env.create("oncall", "work/a");
    env.ok(&["create", &format!("{instance}/helper")]);
    let lead = env.member(&instance, "lead");
    let helper = env.member(&instance, "helper");
    send(&env, &instance, &lead, "pending at the first scan");
    let gate = env.path("ack-refresh-gate");
    fs::create_dir(&gate).unwrap();
    let child = waiter(&env, &instance, &lead)
        .env("BORGCHART_TEST_WAIT_BEFORE_ACK_REFRESH", &gate)
        .env("BORGCHART_WAIT_TIMEOUT_MS", "3000")
        .spawn()
        .unwrap();
    env.wait_for("wait between incremental read and ack refresh", || {
        gate.join("ready").exists()
    });

    // Model whole publications and acknowledgements under the actual writer
    // lock, after the wait has read its incremental tail and before refresh.
    let store = env.path(&format!("state/{instance}/messages"));
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(store.join("lock"))
        .unwrap();
    lock.lock().unwrap();
    for (seq, to) in [(2, &helper), (3, &lead)] {
        let record = json!({"schema": 4, "id": format!("m{seq}"), "seq": seq, "ack_required": true,
            "at": "2026-11-08T00:00:00Z", "from": "external:ops", "to": format!("{instance}/{to}"),
            "body": "published during ack refresh"});
        borgchart::bus::fsx::write_new(
            &store.join(format!("records/{seq:010}.json")),
            record.to_string().as_bytes(),
        )
        .unwrap();
    }
    for (id, recipient) in [("m1", &lead), ("m2", &helper)] {
        let start = env.starts(recipient).last().unwrap().clone();
        let ack = json!({"schema": 4, "id": id, "at": "2026-11-08T00:00:00Z",
            "by": {"type": "member", "member": format!("{instance}/{recipient}"),
                "incarnation": start["env"]["BORGCHART_INCARNATION"]}});
        borgchart::bus::fsx::write_new(
            &store.join(format!("acks/{id}.json")),
            ack.to_string().as_bytes(),
        )
        .unwrap();
    }
    drop(lock);
    fs::write(gate.join("proceed"), b"").unwrap();
    let out = finish(child);
    assert!(out.status.success(), "{}", both(&out));
    let inbox: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(inbox["records"].as_array().unwrap().len(), 1);
    assert_eq!(inbox["records"][0]["id"], "m3");
    env.ok(&["msg", "integrity-check", "--instance", &instance, "--json"]);
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
        "msg",
        "list",
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
    let start = Instant::now();
    let out = env
        .cmd(&["msg", "send", "x/y", "wait"])
        .env("BORGCHART_WAIT_MIN_MS", "5000")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3), "{}", both(&out));
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "failed send waited the wait minimum: {}",
        both(&out)
    );
    for args in [
        vec!["msg", "wait", "--timeout", "60"],
        vec!["msg", "wait", "--timeout", "bad"],
        vec!["msg", "wait", "--bogus"],
        vec!["msg", "--instance", "missing", "wait", "--bogus"],
        vec!["msg", "--instance=missing", "wait", "--bogus"],
        vec!["inbox", "wait"],
    ] {
        let start = Instant::now();
        let out = env
            .cmd(&args)
            .env("BORGCHART_WAIT_MIN_MS", "700")
            .output()
            .unwrap();
        assert!(!out.status.success());
        if args[0] == "inbox" {
            assert_eq!(out.status.code(), Some(2), "{}", both(&out));
        }
        assert!(
            start.elapsed() >= Duration::from_millis(700),
            "{}",
            both(&out)
        );
    }
}

#[test]
fn send_refuses_instance_instead_of_ignoring_it() {
    let env = Env::new();
    for args in [
        vec!["msg", "--instance", "other", "send", "x/y", "hello"],
        vec!["msg", "send", "--instance", "other", "x/y", "hello"],
    ] {
        let out = env.cmd(&args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", both(&out));
    }
}

#[test]
fn an_empty_wait_times_out_and_a_replaced_wait_is_fenced() {
    let env = Env::new();
    let instance = env.create("oncall", "work/a");
    let lead = env.member(&instance, "lead");
    let out = finish(waiter(&env, &instance, &lead).spawn().unwrap());
    assert_eq!(out.status.code(), Some(9), "{}", both(&out));
    let child = waiter(&env, &instance, &lead).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    env.ok(&["stop", &format!("{instance}/{lead}")]);
    env.ok(&["start", &format!("{instance}/{lead}")]);
    let out = finish(child);
    assert_eq!(out.status.code(), Some(8), "{}", both(&out));
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
    let record = json!({"schema": 4, "id": "m2", "seq": 2, "ack_required": true, "at": "2026-11-08T00:00:00Z", "from": "external:ops", "to": format!("{instance}/{lead}"), "body": "new"});
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
    assert!(
        rows.iter().all(|row| row.get("unread").is_none()),
        "{rows:?}"
    );
    assert_eq!(
        rows.iter().find(|r| r["member"] == lead).unwrap()["unacked"],
        2
    );
    assert_eq!(
        rows.iter().find(|r| r["member"] == helper).unwrap()["unacked"],
        1
    );
    let text = stdout(&env.ok(&["status", &instance]));
    assert!(
        text.contains("2 unacked") && text.contains("1 unacked"),
        "{text}"
    );
    let out = member(&env, &instance, &lead, &["msg", "ack", "m1", "m2"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    assert_eq!(env.row(&instance, "lead")["unacked"], 0);
    fs::write(
        env.path(&format!("state/{instance}/messages/store.json")),
        "bad",
    )
    .unwrap();
    let text = stdout(&env.ok(&["status", &instance]));
    assert!(text.contains("messages unavailable"), "{text}");
    assert!(env.row(&instance, "lead").get("unacked").is_none());
    env.ok(&["stop", &instance]);

    // Bus storage is present even without explicit bus settings.
    let env = Env::new();
    env.write("teams/oncall/team.yaml", &TEAM.replace("bus: {}\n", ""));
    let instance = env.create("oncall", "work/a");
    assert_eq!(env.row(&instance, "lead")["unacked"], 0);
    let text = stdout(&env.ok(&["status", &instance]));
    assert!(
        !text.contains("unacked") && !text.contains("messages unavailable"),
        "{text}"
    );
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
fn wait_help_does_not_back_off() {
    let env = Env::new();
    for (command, code) in [("msg", 0), ("inbox", 2)] {
        for flag in ["--help", "-h"] {
            let start = Instant::now();
            let out = env
                .cmd(&[command, "wait", flag])
                .env("BORGCHART_WAIT_MIN_MS", "5000")
                .output()
                .unwrap();
            assert_eq!(out.status.code(), Some(code), "{}", both(&out));
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "help waited its minimum: {command} wait {flag}"
            );
        }
    }
}
