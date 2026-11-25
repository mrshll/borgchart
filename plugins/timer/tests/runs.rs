//! Scheduled member lifecycles through the timer CLI and a recording brg.
#[allow(dead_code)]
mod support;

use serde_json::json;
use std::{
    fs,
    os::unix::fs::symlink,
    process::{Command, Output},
};
use support::live::{Running, wait_until};
use support::*;

const CONFIG: &str = "- {name: review, cron: '0 * * * *', role: worker, timeout_minutes: 45, prompt: Read the draft}";
const AD_HOC: &str = "{models: {claude: sonnet low}}";

fn file_schedule(name: &str, path: &str) -> String {
    CONFIG
        .replace("name: review", &format!("name: {name}"))
        .replace("prompt: Read the draft", &format!("prompt_file: {path}"))
}

const BRG: &str = r#"#!/bin/sh
printf '%s\0' "$@" >> state/sends
printf '\036\n' >> state/sends
case "$1" in
status)
    printf '[{}'
    set -- members/*
    if [ ! -f "$1" ] && [ -f role.json ]; then
        printf ','
        cat role.json
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
    if [ -f issued ]; then echo 'name unavailable' >&2; exit 1; fi
    parent=',"parent":"editor-2"'
    [ ! -f parentless ] || parent=''
    printf '{"kind":"member","team":"%s","name":"%s","role":"%s"%s,"state":"idle","liveness":"alive","start_id":"start-1","vendor":"codex","model":"example-model"}' "$BORGCHART_TEAM" "$id" "${2##*/}" "$parent" > "members/$id"
    if [ -f create-fails ]; then echo 'start failed' >&2; exit 1; fi
    ;;
stop)
    if [ -f already-deleted ]; then
        printf 'brg: member `%s` was deleted; create a new member with `brg create sample/worker`\n' "$2" >&2
        exit 1
    fi
    [ ! -f stop-fails ] && { [ -f "members/${2##*/}" ] || [ -f hidden-member ]; } || { echo 'stop failed' >&2; exit 1; }
    ;;
delete)
    if [ -f hidden-member ]; then rm hidden-member; exit; fi
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
    f.write(
        "role.json",
        r#"{"kind":"role","team":"sample","name":"worker","role":"worker","parent":"editor-1","vendor":"codex","model":"example-model"}"#,
    );
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

fn run_command(f: &Fixture, args: &[&str]) -> Command {
    let mut command = f.command(args);
    command
        .env("BORGCHART_TEAM", "sample")
        .env("BORGCHART_RUNS", "worker")
        .env("SEAT", f.path("brg"))
        .env_remove("BORGCHART_MEMBER")
        .env_remove("BORGCHART_DELIVER_TO");
    command
}

fn run(f: &Fixture, args: &[&str]) -> Output {
    run_command(f, args).output().unwrap()
}

fn tick(f: &Fixture) {
    let out = run(f, &["run", "checks.yaml", "--once"]);
    assert!(out.status.success(), "{out:?}");
}

fn member_status(f: &Fixture, name: &str, state: &str, liveness: Option<&str>, start_id: &str) {
    let mut row =
        json!({"kind":"member","team":"sample","name":name,"state":state,"start_id":start_id});
    if let Some(liveness) = liveness {
        row["liveness"] = json!(liveness);
    }
    f.write(&format!("members/{name}"), &row.to_string());
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
fn ad_hoc_run_uses_the_schedule_pin_and_reports_to_the_created_members_parent() {
    for (role, vendor, model, settings) in [
        (
            AD_HOC,
            "claude",
            "sonnet",
            "vendor=claude,model=sonnet,effort=low",
        ),
        (
            "{vendor: codex, models: {claude: sonnet, codex: example-model}}",
            "codex",
            "example-model",
            "vendor=codex,model=example-model",
        ),
    ] {
        let f = fixture();
        let tick = || {
            let out = run_command(&f, &["run", "checks.yaml", "--once"])
                .env("BORGCHART_RUN_ROLE", "timer-run")
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
        };
        tick();
        pending(&f, &["review".into()]);
        f.write(
            "checks.yaml",
            &CONFIG.replace("role: worker", &format!("role: {role}")),
        );
        tick();
        let create = calls(&f, "create");
        let name = &create[0][3];
        assert_eq!(
            create,
            [vec![
                "create",
                "sample/timer-run",
                "--name",
                name,
                "--run",
                "review",
                "--set",
                settings
            ]]
        );
        assert_eq!(calls(&f, "route"), [vec!["route", vendor, model]]);
        let msg = calls(&f, "msg");
        assert!(msg[0].ends_with(&[
            format!("sample/timer-run-{name}"),
            "review 2020-01-01T00:00Z | Report to sample/editor-2\nRead the draft".into(),
        ]));
        assert!(
            done(&f, &format!("timer-run-{name}"), "start-1")
                .status
                .success()
        );
        tick();
        assert!(last(&f, "review").starts_with("done "));
    }
}

#[test]
fn ad_hoc_role_without_the_plugins_run_grant_refuses_startup() {
    let f = fixture();
    f.write("checks.yaml", &CONFIG.replace("worker", AD_HOC));
    let out = run(&f, &["run", "checks.yaml", "--once"]);
    assert!(!out.status.success());
    assert_eq!(f.read("health.json")["ok"], false);
    assert!(calls(&f, "create").is_empty());
}

#[test]
fn ad_hoc_roles_refuse_ambiguous_unpinned_and_missing_vendors() {
    let f = fixture();
    for role in [
        "{models: {claude: sonnet, codex: example-model}}",
        "{vendor: codex, models: {claude: sonnet}}",
        "{}",
    ] {
        f.write("checks.yaml", &CONFIG.replace("worker", role));
        assert_eq!(
            f.run(&["validate", "checks.yaml"]).status.code(),
            Some(1),
            "{role}"
        );
    }
}

#[test]
fn ad_hoc_models_cannot_change_the_create_override_list() {
    let f = fixture();
    for model in ["sonnet,other", "sonnet=other"] {
        f.write(
            "checks.yaml",
            &CONFIG.replace("worker", &format!("{{models: {{claude: '{model}'}}}}")),
        );
        assert_eq!(
            f.run(&["validate", "checks.yaml"]).status.code(),
            Some(1),
            "{model}"
        );
    }
}

#[test]
fn ad_hoc_roles_refuse_invalid_frontmatter_and_missing_work() {
    let f = fixture();
    for role in [
        "{models: {claude: sonnet}, typo: true}",
        "{models: {claude: sonnet wrong-effort}}",
    ] {
        f.write("checks.yaml", &CONFIG.replace("worker", role));
        assert_eq!(
            f.run(&["validate", "checks.yaml"]).status.code(),
            Some(1),
            "{role}"
        );
    }
    f.write(
        "checks.yaml",
        &CONFIG
            .replace("worker", AD_HOC)
            .replace(", prompt: Read the draft", ""),
    );
    assert_eq!(f.run(&["validate", "checks.yaml"]).status.code(), Some(1));
}

#[test]
fn prompt_file_requires_a_run_and_excludes_inline_prompt() {
    let f = fixture();
    let schedule = file_schedule("review", "review.md");
    f.write("review.md", "Read the draft");
    f.write("checks.yaml", &schedule.replace("worker", AD_HOC));
    assert!(f.run(&["validate", "checks.yaml"]).status.success());
    for invalid in [
        schedule.replace("role: worker, timeout_minutes: 45, ", ""),
        schedule.replace("prompt_file:", "prompt: Read the draft, prompt_file:"),
    ] {
        f.write("checks.yaml", &invalid);
        assert_eq!(
            f.run(&["validate", "checks.yaml"]).status.code(),
            Some(1),
            "{invalid}"
        );
    }
}

#[test]
fn startup_refuses_prompt_file_paths_outside_the_checks_folder() {
    let f = fixture();
    fs::create_dir(f.path("config")).unwrap();
    f.write("outside.md", "Read the draft");
    f.write("config/review.md", "Review the notes");
    symlink("../outside.md", f.path("config/link.md")).unwrap();
    let startup = || {
        f.command(&["run", "config/checks.yaml", "--once"])
            .env("BORGCHART_RUNS", "worker")
            .output()
            .unwrap()
    };
    f.write("config/checks.yaml", &file_schedule("review", "review.md"));
    assert!(startup().status.success());
    for path in [
        f.path("config/review.md").to_str().unwrap(),
        "../config/review.md",
        "link.md",
    ] {
        f.write("config/checks.yaml", &file_schedule("review", path));
        assert_eq!(startup().status.code(), Some(1), "{path}");
    }
}

#[test]
fn prompt_files_reject_invalid_input_and_deliver_the_maximum_with_a_long_name() {
    let f = fixture();
    let name = "r".repeat(32);
    f.write("checks.yaml", &file_schedule(&name, "review.md"));
    assert_eq!(
        run(&f, &["run", "checks.yaml", "--once"]).status.code(),
        Some(1)
    );
    for bad in [
        b"\xff".to_vec(),
        b" \n\t".to_vec(),
        b"before\0after".to_vec(),
        "é".repeat(8001).into_bytes(),
    ] {
        fs::write(f.path("review.md"), bad).unwrap();
        assert_eq!(
            run(&f, &["run", "checks.yaml", "--once"]).status.code(),
            Some(1)
        );
    }
    let prompt = "é".repeat(8000);
    f.write("review.md", &format!(" \n{prompt}\n"));
    tick(&f);
    pending(&f, std::slice::from_ref(&name));
    tick(&f);
    let messages = calls(&f, "msg");
    let body = messages[0].last().unwrap();
    assert!(
        body == &format!("{name} 2020-01-01T00:00Z | Report to sample/editor-2\n{prompt}"),
        "expected the full prompt after the header, got {} bytes",
        body.len()
    );
    assert!(borgchart::runtime::check_text(body).is_ok());
}

#[test]
fn a_long_parent_address_cannot_push_a_run_message_past_the_bus_limit() {
    let f = fixture();
    f.write("checks.yaml", &file_schedule("review", "review.md"));
    f.write("review.md", &"x".repeat(16_000));
    tick(&f);
    pending(&f, &["review".into()]);
    let out = run_command(&f, &["run", "checks.yaml", "--once"])
        .env(
            "BORGCHART_TEAM",
            format!("{}sample", "department-with-a-long-name/".repeat(15)),
        )
        .env_remove("SEAT")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(calls(&f, "create").len(), 1);
    assert!(calls(&f, "msg").is_empty());
    assert!(last(&f, "review").starts_with("failed-to-start "));
}

#[test]
fn a_parentless_declared_role_receives_its_prompt_without_a_report_address() {
    let f = fixture();
    f.write("parentless", "");
    start(&f);
    assert_eq!(
        calls(&f, "msg")[0].last().unwrap(),
        "review 2020-01-01T00:00Z\nRead the draft"
    );
}

#[test]
fn changing_a_prompt_file_path_preserves_the_pending_occurrence() {
    let f = fixture();
    f.write("checks.yaml", &file_schedule("review", "initial.md"));
    f.write("initial.md", "Original notes");
    f.write("review.md", "Review the new notes.");
    tick(&f);
    assert!(calls(&f, "create").is_empty());
    pending(&f, &["review".into()]);
    f.write("checks.yaml", &file_schedule("review", "review.md"));
    tick(&f);
    assert_eq!(
        calls(&f, "msg")[0].last().unwrap(),
        "review 2020-01-01T00:00Z | Report to sample/editor-2\nReview the new notes."
    );
}

#[test]
fn occurrences_read_current_multiline_prompts_and_fail_before_seating_if_removed() {
    let f = fixture();
    let config = [("review", "review.md"), ("removed", "removed.md")]
        .map(|(name, path)| file_schedule(name, path))
        .join("\n");
    f.write("checks.yaml", &config);
    f.write("review.md", "Old draft");
    f.write("removed.md", "Original text");
    let _running = Running(run_command(&f, &["run", "checks.yaml"]).spawn().unwrap());
    wait_for(&f.path("state/health.json"));
    let first_tick = f.read("health.json")["at"].clone();
    f.write("review.md", "  New draft\n\nCheck the figures.\n");
    fs::remove_file(f.path("removed.md")).unwrap();
    pending(&f, &["review".into(), "removed".into()]);
    wait_until(|| f.read("health.json")["at"] != first_tick);
    let messages = calls(&f, "msg");
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].last().unwrap(),
        "review 2020-01-01T00:00Z | Report to sample/editor-2\nNew draft\n\nCheck the figures."
    );
    assert_eq!(calls(&f, "route").len(), 1);
    assert_eq!(calls(&f, "create").len(), 1);
    assert!(last(&f, "removed").starts_with("failed-to-start "));
}

#[test]
fn inline_run_prompt_delivers_multiline_text() {
    let f = fixture();
    f.write(
        "checks.yaml",
        "- name: review\n  cron: '0 * * * *'\n  role: worker\n  timeout_minutes: 45\n  prompt: |\n    Review the draft.\n\n    Send a summary.\n",
    );
    start(&f);
    assert_eq!(
        calls(&f, "msg")[0].last().unwrap(),
        "review 2020-01-01T00:00Z | Report to sample/editor-2\nReview the draft.\n\nSend a summary."
    );
}

#[test]
fn inline_run_prompt_refuses_oversize_and_controls_other_than_newline() {
    let f = fixture();
    for prompt in [
        "x".repeat(16001),
        "first\\tsecond".into(),
        "first\\rsecond".into(),
    ] {
        f.write(
            "checks.yaml",
            &CONFIG.replace("Read the draft", &format!("\"{prompt}\"")),
        );
        assert_eq!(f.run(&["validate", "checks.yaml"]).status.code(), Some(1));
    }
}

#[test]
fn restricted_claude_runs_fail_before_create_when_the_prompt_exceeds_the_budget() {
    let header = "review 2020-01-01T00:00Z | Report to sample/editor-2\n";
    for (prompt, allowed) in [
        ("Line one\nLine two", false),
        (&"x".repeat(701 - header.len()), false),
        (&"x".repeat(700 - header.len()), true),
    ] {
        let f = fixture();
        f.write(
            "role.json",
            r#"{"kind":"role","team":"sample","name":"worker","role":"worker","parent":"editor-2","vendor":"claude","model":"example-model","restricted_access":true}"#,
        );
        f.write("checks.yaml", &file_schedule("review", "review.md"));
        f.write("review.md", prompt);
        tick(&f);
        pending(&f, &["review".into()]);
        tick(&f);
        assert_eq!(calls(&f, "create").len(), usize::from(allowed));
        if allowed {
            let messages = calls(&f, "msg");
            let body = messages[0].last().unwrap();
            assert_eq!(body.len(), 700);
            assert_eq!(body.lines().count(), 2);
        } else {
            assert!(last(&f, "review").starts_with("failed-to-start "));
        }
    }
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
        assert!(!f.run(&["validate", "checks.yaml"]).status.success());
    }
    for timeout in ["0", "1441", "1.5", "'45'"] {
        f.write("checks.yaml", &CONFIG.replace("45", timeout));
        assert!(!f.run(&["validate", "checks.yaml"]).status.success());
    }
    f.write(
        "checks.yaml",
        &CONFIG.replace("role: worker", "role: reader"),
    );
    assert!(!run(&f, &["run", "checks.yaml", "--once"]).status.success());
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
fn forged_run_records_cannot_reap_members_outside_the_timers_run_ids() {
    let f = fixture();
    let run = start(&f);
    let mut state = f.read("checks.json");
    let ungranted = run.replacen("worker-", "reader-", 1);
    for (role, member) in [("reader", ungranted.as_str()), ("worker", "worker-manual")] {
        member_status(&f, member, "idle", Some("alive"), "another-start");
        state["runs"]["review"]["member"] = json!(member);
        state["runs"]["review"]["role"] = json!(role);
        f.write("state/checks.json", &state.to_string());
        tick(&f);
        assert!(calls(&f, "stop").is_empty(), "{member}");
        assert!(calls(&f, "delete").is_empty(), "{member}");
        let health = f.read("health.json");
        assert_eq!(health["ok"], false);
        assert!(health["detail"].as_str().unwrap().contains("review"));
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
    let seats = calls(&f, "route");
    let members = calls(&f, "create");
    let messages = calls(&f, "msg");
    for (index, name) in names.iter().take(5).enumerate() {
        assert_eq!(seats[index][1..], ["codex", "example-model"]);
        assert_eq!(
            members[index],
            [
                "create",
                "sample/worker",
                "--name",
                &members[index][3],
                "--run",
                name
            ]
        );
        let payload = [
            format!("sample/worker-{}", members[index][3]),
            format!("{name} 2020-01-01T00:00Z | Report to sample/editor-2\nRead the draft"),
        ];
        assert!(messages.iter().any(|call| call.ends_with(&payload)));
    }
    assert_eq!(fs::read_dir(f.path("members")).unwrap().count(), 5);
    assert!(last(&f, "review-5").ends_with(": schedule cap\n"));
}

#[test]
fn done_reaps_an_exited_member_and_last_exposes_only_one_outcome_line() {
    let f = fixture();
    let member = start(&f);
    assert!(done(&f, &member, "start-1").status.success());
    member_status(&f, &member, "exited", Some("gone"), "start-1");
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
fn missing_members_finalize_without_stop_after_cleanup_or_create_grace() {
    for was_ending in [true, false] {
        let f = fixture();
        if was_ending {
            f.write("create-fails", "");
            f.write("stop-fails", "");
        }
        let member = start(&f);
        fs::remove_file(f.path(&format!("members/{member}"))).unwrap();
        if !was_ending {
            let mut state = f.read("checks.json");
            state["runs"]["review"]["start_id"] = json!(null);
            state["runs"]["review"]["prompted"] = json!(false);
            f.write("state/checks.json", &state.to_string());
            tick(&f);
            assert!(calls(&f, "stop").is_empty());
            assert_eq!(f.read("health.json")["ok"], false);
            state["runs"]["review"]["started"] = json!("2020-01-01T00:00:00Z");
            f.write("state/checks.json", &state.to_string());
        }
        let stops = calls(&f, "stop").len();
        tick(&f);
        assert_eq!(calls(&f, "stop").len(), stops);
        assert!(calls(&f, "delete").is_empty());
        let outcome = if was_ending {
            "failed-to-start "
        } else {
            "interrupted "
        };
        assert!(last(&f, "review").starts_with(outcome));
        assert_eq!(f.read("health.json")["ok"], true);
    }
}

#[test]
fn a_run_with_no_member_row_is_stopped_and_deleted_after_its_role_disappears() {
    let f = fixture();
    let id = start(&f);
    fs::rename(f.path(&format!("members/{id}")), f.path("hidden-member")).unwrap();
    tick(&f);
    assert!(last(&f, "review").starts_with("interrupted "));
    assert!(calls(&f, "stop").is_empty());
    assert!(f.path("hidden-member").exists());

    let f = fixture();
    let id = start(&f);
    let address = format!("sample/{id}");
    fs::rename(f.path(&format!("members/{id}")), f.path("hidden-member")).unwrap();
    fs::remove_file(f.path("role.json")).unwrap();
    f.write("stop-fails", "");
    tick(&f);
    assert!(!f.run(&["last", "review"]).status.success());
    assert_eq!(f.read("health.json")["ok"], false);
    fs::remove_file(f.path("stop-fails")).unwrap();
    tick(&f);
    let calls = f.sends();
    assert!(calls.ends_with(&[
        vec!["stop".into(), address.clone()],
        vec!["delete".into(), address, "--force".into()],
    ]));
    assert!(!f.path("hidden-member").exists());
    assert!(last(&f, "review").starts_with("interrupted "));
}

#[test]
fn restart_reaps_a_hidden_run_after_its_role_and_grant_are_removed() {
    let f = fixture();
    let id = start(&f);
    let address = format!("sample/{id}");
    fs::rename(f.path(&format!("members/{id}")), f.path("hidden-member")).unwrap();
    fs::remove_file(f.path("role.json")).unwrap();
    f.write(
        "checks.yaml",
        &CONFIG.replace("role: worker", "role: reader"),
    );
    assert!(!f.run(&["last", "review"]).status.success());

    let out = run_command(&f, &["run", "checks.yaml", "--once"])
        .env("BORGCHART_RUNS", "reader")
        .env_remove("BORGCHART_RUN_ROLE")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(f.sends().ends_with(&[
        vec!["stop".into(), address.clone()],
        vec!["delete".into(), address, "--force".into()],
    ]));
    assert!(last(&f, "review").starts_with("interrupted "));
}

#[test]
fn a_run_core_already_deleted_is_finalized_without_retrying() {
    let f = fixture();
    let id = start(&f);
    fs::remove_file(f.path(&format!("members/{id}"))).unwrap();
    fs::remove_file(f.path("role.json")).unwrap();
    f.write("already-deleted", "");
    tick(&f);
    assert!(last(&f, "review").starts_with("interrupted "));
    tick(&f);
    assert_eq!(calls(&f, "stop"), [vec!["stop", &format!("sample/{id}")]]);
    assert!(calls(&f, "delete").is_empty());
    assert_eq!(f.read("health.json")["ok"], true);
}

#[test]
fn unknown_liveness_retries_without_ending_the_same_start() {
    let f = fixture();
    let member = start(&f);
    for liveness in [Some("unknown"), None] {
        member_status(&f, &member, "unknown", liveness, "start-1");
        tick(&f);
        assert!(calls(&f, "stop").is_empty());
        assert_eq!(f.read("health.json")["ok"], false);
    }
    member_status(&f, &member, "idle", Some("alive"), "start-1");
    tick(&f);
    assert!(calls(&f, "stop").is_empty());
    assert!(done(&f, &member, "start-1").status.success());
}

#[test]
fn restart_keeps_only_the_same_live_start_and_preserves_its_deadline() {
    for (state, liveness, start_id) in [
        ("idle", "alive", "start-1"),
        ("stopped", "gone", "start-1"),
        ("idle", "alive", "start-2"),
    ] {
        let f = fixture();
        let member = start(&f);
        let deadline = f.read("checks.json")["runs"]["review"]["deadline"].clone();
        member_status(&f, &member, state, Some(liveness), start_id);
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
