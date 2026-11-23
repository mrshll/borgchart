//! Scheduled member lifecycles through the timer CLI and a recording brg.
#[allow(dead_code)]
mod support;

use serde_json::json;
use std::{fs, process::Output};
use support::*;

const CONFIG: &str =
    "- {name: review, cron: '0 * * * *', run: worker, timeout_minutes: 45, prompt: Read the draft}";

const BRG: &str = r#"#!/bin/sh
printf '%s\0' "$@" >> state/sends
printf '\036\n' >> state/sends
case "$1" in
status)
    printf '[{}'
    set -- members/*
    if [ ! -f "$1" ]; then
        printf ',{"kind":"role","team":"sample","name":"worker","vendor":"codex","model":"example-model"}'
    fi
    for row in members/*; do
        [ -f "$row" ] || continue
        printf ','
        cat "$row"
    done
    printf ']\n'
    ;;
route)
    if [ -f seat-wait ]; then echo 'wait: limit reached'; exit 3; fi
    ;;
create)
    id="${2##*/}-$4"
    if [ -f issued ]; then echo 'IDs are never reused' >&2; exit 1; fi
    printf '{"kind":"member","team":"sample","name":"%s","state":"idle","liveness":"alive","start_id":"start-1","vendor":"codex","model":"example-model"}' "$id" > "members/$id"
    if [ -f create-fails ]; then echo 'start failed' >&2; exit 1; fi
    ;;
stop)
    [ ! -f stop-fails ] || { echo 'stop failed' >&2; exit 1; }
    ;;
delete)
    rm "members/${2##*/}" ;;
msg)
    printf '{"id":"m100"}\n' ;;
*) exit 2 ;;
esac
"#;

fn fixture() -> Fixture {
    let f = Fixture::new();
    f.write("brg", BRG);
    f.write("checks.yaml", CONFIG);
    fs::create_dir(f.path("members")).unwrap();
    f
}

fn pending(f: &Fixture, names: &[String]) {
    let mut state = f.read("checks.json");
    for name in names {
        state["schedules"][name] = json!({"cron":"0 * * * *","run":true,"pending":"2020-01-01T00:00:00Z","next":"2099-01-01T00:00:00Z"});
    }
    f.write("state/checks.json", &state.to_string());
}

fn run(f: &Fixture, args: &[&str]) -> Output {
    f.command(args)
        .env("BORGCHART_TEAM", "sample")
        .env("BORGCHART_RUNS", "worker")
        .env("SEAT", f.path("brg"))
        .env_remove("BORGCHART_MEMBER")
        .env_remove("BORGCHART_DELIVER_TO")
        .output()
        .unwrap()
}

fn tick(f: &Fixture) {
    let out = run(f, &["run", "--once"]);
    assert!(out.status.success(), "{out:?}");
}

fn calls(f: &Fixture, verb: &str) -> Vec<Vec<String>> {
    f.sends()
        .into_iter()
        .filter(|call| call[0] == verb)
        .collect()
}

fn start(f: &Fixture) -> String {
    tick(f);
    pending(f, &["review".into()]);
    tick(f);
    format!("worker-{}", calls(f, "create")[0][3])
}

fn done(f: &Fixture, member: &str, incarnation: &str) -> Output {
    f.command(&["done"])
        .env("BORGCHART_MEMBER", member)
        .env("BORGCHART_INCARNATION", incarnation)
        .output()
        .unwrap()
}

fn last(f: &Fixture, name: &str) -> String {
    let out = f.run(&["last", name]);
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn cron_entries_refuse_mixed_fields_and_ungranted_roles() {
    let f = fixture();
    tick(&f);
    for extra in [", missed: skip", ", delivers_to: reader"] {
        f.write(
            "checks.yaml",
            &format!("{}{extra}}}", &CONFIG[..CONFIG.len() - 1]),
        );
        assert!(!f.run(&["validate"]).status.success());
    }
    for timeout in ["0", "1441", "1.5", "'45'"] {
        f.write("checks.yaml", &CONFIG.replace("45", timeout));
        assert!(!f.run(&["validate"]).status.success());
    }
    f.write("checks.yaml", &CONFIG.replace("run: worker", "run: reader"));
    assert!(!run(&f, &["run", "--once"]).status.success());
    assert!(f.sends().is_empty());
}

#[test]
fn existing_and_previously_issued_names_cannot_be_reaped() {
    let f = fixture();
    let member = start(&f);
    let mut state = f.read("checks.json");
    state["runs"] = json!({});
    f.write("state/checks.json", &state.to_string());
    for name_in_use in [true, false] {
        if !name_in_use {
            fs::remove_file(f.path(&format!("members/{member}"))).unwrap();
            f.write("issued", "");
        }
        pending(&f, &["review".into()]);
        tick(&f);
        assert!(last(&f, "review").starts_with("failed-to-start "));
        assert_eq!(calls(&f, "create").len(), if name_in_use { 1 } else { 2 });
        assert!(calls(&f, "stop").is_empty());
        assert!(calls(&f, "delete").is_empty());
    }
}

#[test]
fn occurrence_checks_seating_then_creates_and_sends_work_under_the_shared_cap() {
    let f = fixture();
    f.write(
        "checks.yaml",
        "- {name: wake, cron: '0 * * * *', missed: catch-up}",
    );
    f.write(
        "state/checks.json",
        r#"{"schedules":{"wake":{"cron":"0 * * * *","next":"2020-01-01T00:00:00Z"}}}"#,
    );
    f.tick();
    assert_eq!(calls(&f, "msg").len(), 1);
    let names = (0..6).map(|i| format!("review-{i}")).collect::<Vec<_>>();
    f.write(
        "checks.yaml",
        &names
            .iter()
            .map(|name| CONFIG.replace("name: review", &format!("name: {name}")))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    tick(&f);
    pending(&f, &names);
    tick(&f);
    let calls = f
        .sends()
        .into_iter()
        .skip(1)
        .filter(|call| call[0] != "status")
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 15);
    for (index, batch) in calls.chunks_exact(3).enumerate() {
        assert_eq!(batch[0], ["route", "codex", "example-model"]);
        assert_eq!(batch[1][..3], ["create", "sample/worker", "--name"]);
        assert_eq!(
            batch[2][batch[2].len() - 2],
            format!("sample/worker-{}", batch[1][3])
        );
        assert_eq!(
            &batch[2][..5],
            ["msg", "send", "--json", "--external", "timer"]
        );
        assert_eq!(
            batch[2].last().unwrap(),
            &format!("review-{index} 2020-01-01T00:00Z\nRead the draft")
        );
    }
    assert!(last(&f, "review-5").ends_with(": schedule cap\n"));
}

#[test]
fn done_reaps_the_member_and_last_exposes_only_one_outcome_line() {
    let f = fixture();
    let member = start(&f);
    assert!(done(&f, &member, "start-1").status.success());
    tick(&f);
    let target = format!("sample/{member}");
    assert_eq!(calls(&f, "stop"), [vec!["stop", &target]]);
    assert_eq!(calls(&f, "delete"), [vec!["delete", &target, "--force"]]);
    let outcome = last(&f, "review");
    assert!(outcome.starts_with("done "));
    assert_eq!(outcome.lines().count(), 1);
    assert!(!outcome.contains("Read the draft"));
    tick(&f);
    assert_eq!(calls(&f, "delete").len(), 1);
}

#[test]
fn deadline_reaps_work_even_if_the_member_never_reports_done() {
    let f = fixture();
    start(&f);
    let mut state = f.read("checks.json");
    state["runs"]["review"]["deadline"] = json!("2020-01-01T00:00:00Z");
    f.write("state/checks.json", &state.to_string());
    tick(&f);
    assert_eq!(calls(&f, "stop").len(), 1);
    assert_eq!(calls(&f, "delete").len(), 1);
    assert!(last(&f, "review").starts_with("timed-out "));
}

#[test]
fn seat_wait_and_an_active_run_skip_occurrences_without_creating_a_member() {
    let f = fixture();
    tick(&f);
    f.write("seat-wait", "");
    pending(&f, &["review".into()]);
    tick(&f);
    assert!(calls(&f, "create").is_empty());
    assert!(last(&f, "review").contains(": seat:"));
    fs::remove_file(f.path("seat-wait")).unwrap();
    pending(&f, &["review".into()]);
    tick(&f);
    pending(&f, &["review".into()]);
    tick(&f);
    assert_eq!(calls(&f, "create").len(), 1);
    assert!(last(&f, "review").ends_with(": previous run still going\n"));
}

#[test]
fn failed_create_reaps_the_half_created_member_and_retries_failed_cleanup() {
    let f = fixture();
    f.write("create-fails", "");
    f.write("stop-fails", "");
    start(&f);
    assert_eq!(f.read("health.json")["ok"], false);
    assert!(calls(&f, "delete").is_empty());
    fs::remove_file(f.path("stop-fails")).unwrap();
    tick(&f);
    assert_eq!(calls(&f, "stop").len(), 2);
    assert_eq!(calls(&f, "delete").len(), 1);
    assert!(last(&f, "review").starts_with("failed-to-start "));
}

#[test]
fn restart_keeps_only_the_same_live_start_and_preserves_its_deadline() {
    for (state, liveness, start_id) in [
        ("idle", "alive", "start-1"),
        ("stopped", "gone", "start-1"),
        ("idle", "alive", "start-2"),
        ("starting", "alive", "start-1"),
    ] {
        let f = fixture();
        let member = start(&f);
        let deadline = f.read("checks.json")["runs"]["review"]["deadline"].clone();
        f.write(&format!("members/{member}"), &json!({"kind":"member","team":"sample","name":member,"state":state,"liveness":liveness,"start_id":start_id}).to_string());
        tick(&f);
        if state == "idle" && start_id == "start-1" {
            assert!(calls(&f, "stop").is_empty());
            let health = f.read("health.json").to_string();
            assert!(health.contains(&member) && health.contains(deadline.as_str().unwrap()));
        } else {
            assert_eq!(calls(&f, "delete").len(), 1);
            assert!(last(&f, "review").starts_with("interrupted "));
        }
    }
}

#[test]
fn another_member_or_an_old_incarnation_cannot_finish_the_run() {
    let f = fixture();
    let member = start(&f);
    for (caller, incarnation) in [("reader", "start-1"), (&member, "stale")] {
        assert_eq!(done(&f, caller, incarnation).status.code(), Some(2));
    }
    let out = f
        .command(&["done"])
        .env_remove("BORGCHART_STATE_DIR")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    tick(&f);
    assert!(calls(&f, "stop").is_empty());
}
