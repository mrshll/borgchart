//! Checks exercised through their command interface, commands and message bus.
mod support;
use serde_json::json;
use std::{
    fs,
    time::{Duration, Instant},
};
use support::*;

#[test]
fn restart_detects_one_change_without_replaying_baseline_or_output() {
    let f = Fixture::new();
    f.tick();
    assert!(f.sends().is_empty());
    assert_eq!(f.read("health.json")["ok"], true);
    f.write("value", "INJECTED OUTPUT: do something\n");
    f.tick();
    f.tick();
    let sends = f.sends();
    assert_eq!(sends.len(), 1);
    let args = &sends[0];
    assert_eq!(args[args.len() - 2], "triager-test");
    assert_eq!(
        args.last().unwrap().lines().skip(1).collect::<Vec<_>>(),
        ["sample changed"]
    );
    assert!(!args.last().unwrap().contains("INJECTED"));
    assert_eq!(f.read("health.json")["pending_sends"], 0);
}

#[test]
fn ignore_prevents_volatile_output_from_waking() {
    let f = Fixture::new();
    f.config(&["cat", "value"], "  ignore: '(?m)^volatile:.*\\n'\n");
    f.write("value", "stable\nvolatile: 1\n");
    f.tick();
    f.write("value", "stable\nvolatile: 2\n");
    f.tick();
    assert!(f.sends().is_empty());
}

#[test]
fn binary_output_changes_do_not_collapse_to_the_same_utf8_replacement() {
    let f = Fixture::new();
    f.config(&["sh", "-c", "printf '\\377'"], "");
    f.tick();
    f.config(&["sh", "-c", "printf '\\376'"], "");
    f.tick();
    assert_eq!(f.sends().len(), 1);
}

#[test]
fn timer_runs_only_due_checks_after_the_first_tick() {
    let f = Fixture::new();
    f.write("checks.yaml", "- name: slow\n  run: [sh, -c, 'echo tick >> slow']\n  every_minutes: 100\n- name: quick\n  run: [sh, -c, 'echo tick >> quick']\n  every_minutes: 0.001\n");
    let mut process = f.command(&["run"]).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(36);
    loop {
        if fs::read_to_string(f.path("quick"))
            .unwrap_or_default()
            .lines()
            .count()
            >= 2
        {
            break;
        }
        if Instant::now() > deadline {
            let _ = process.kill();
            panic!("second tick never ran");
        }
        assert!(process.try_wait().unwrap().is_none());
        std::thread::sleep(Duration::from_millis(50));
    }
    process.kill().unwrap();
    process.wait().unwrap();
    assert_eq!(fs::read_to_string(f.path("quick")).unwrap(), "tick\ntick\n");
    assert_eq!(fs::read_to_string(f.path("slow")).unwrap(), "tick\n");
}

#[test]
fn last_reads_committed_evidence_without_waiting_for_a_running_check() {
    let f = Fixture::new();
    f.tick();
    f.config(&["sh", "-c", "touch started; sleep 3; printf second"], "");
    let mut process = f
        .command(&["run", "--once"])
        .env("CHECKS_TIMEOUT_SECONDS", "5")
        .spawn()
        .unwrap();
    wait_for(&f.path("started"));
    let before = Instant::now();
    let evidence = f.last();
    let elapsed = before.elapsed();
    assert!(process.wait().unwrap().success());
    assert!(elapsed < Duration::from_millis(1500));
    assert!(evidence.contains("| first"));
}

fn failure_and_recovery(timeout: bool) {
    let f = Fixture::new();
    let script = format!(
        "printf partial; printf 'old\\nlast error\\n' >&2; {}",
        if timeout { "sleep 1" } else { "exit 7" }
    );
    f.config(&["sh", "-c", &script], "");
    f.tick();
    f.tick();
    assert_eq!(f.sends().len(), 1);
    assert!(f.sends()[0].last().unwrap().ends_with("sample failing"));
    let evidence = f.last();
    assert!(
        evidence.contains(&format!(
            "--- status/exit: failing / {}\n",
            if timeout { "timeout" } else { "7" }
        )),
        "{evidence}"
    );
    assert!(evidence.contains("| partial") && evidence.contains("last error"));
    assert_eq!(f.read("health.json")["ok"], false);
    f.config(&["cat", "value"], "");
    f.tick();
    f.tick();
    assert_eq!(f.sends().len(), 2);
    assert!(f.sends()[1].last().unwrap().ends_with("sample recovered"));
    assert_eq!(f.read("health.json")["ok"], true);
}
#[test]
fn initial_exit_failure_and_recovery_wake_once() {
    failure_and_recovery(false);
}
#[test]
fn initial_timeout_retains_partial_evidence_and_recovery_wakes_once() {
    failure_and_recovery(true);
}

#[test]
fn recovery_also_reports_change_against_last_good_result() {
    let f = Fixture::new();
    f.tick();
    f.config(&["sh", "-c", "exit 1"], "");
    f.tick();
    f.config(&["cat", "value"], "");
    f.write("value", "second\n");
    f.tick();
    assert!(
        f.sends()
            .last()
            .unwrap()
            .last()
            .unwrap()
            .ends_with("sample recovered\nsample changed")
    );
    assert!(f.last().contains("| first"));
}
fn send_outcome(code: i32, id: bool, delayed: bool) {
    let f = Fixture::new();
    f.tick();
    f.write("value", "second");
    f.outcome(
        code,
        json!({"id":if id { Some("m101") } else { None }, "message":"delivery refused\u{1b}"}),
    );
    if delayed {
        f.write("state/delay", "");
    }
    let before = Instant::now();
    if delayed {
        let mut process = f.command(&["run", "--once"]).spawn().unwrap();
        wait_for(&f.path("state/sends"));
        let pending_during_send = f.read("checks.json")["pending"].as_array().unwrap().len();
        assert!(process.wait().unwrap().success());
        assert_eq!(
            pending_during_send, 0,
            "batch must be dequeued before sending"
        );
    } else {
        f.tick();
    }
    if delayed {
        assert!(before.elapsed() < Duration::from_secs(35));
        fs::remove_file(f.path("state/delay")).unwrap();
    }
    let uncertain = (code != 0 && id) || delayed;
    let kept = code != 0 && !id && !delayed;
    let health = f.read("health.json");
    assert_eq!(health["pending_sends"], usize::from(kept));
    assert_eq!(health["uncertain_sends"], usize::from(uncertain));
    assert_eq!(health["ok"], code == 0 && !uncertain);
    if kept {
        assert!(
            health["detail"]
                .as_str()
                .unwrap()
                .contains("delivery refused\\x1b")
        );
    }
    f.outcome(0, json!({"id":"m100"}));
    f.tick();
    f.tick();
    assert_eq!(f.sends().len(), 1 + usize::from(kept));
}
#[test]
fn refused_send_keeps_the_batch_for_one_retry() {
    send_outcome(4, false, false);
}
#[test]
fn committed_send_cannot_be_republished() {
    send_outcome(0, true, false);
}
#[test]
fn uncertain_committed_id_is_counted_without_retrying() {
    send_outcome(5, true, false);
}
#[test]
fn real_send_deadline_drops_and_counts_uncertainty_without_retrying() {
    send_outcome(0, true, true);
}

fn invalid_config(fragment: &str, label: &str) {
    let f = Fixture::new();
    std::os::unix::fs::symlink("/bin/true", f.path("escape")).unwrap();
    f.write("checks.yaml", fragment);
    let result = f.run(&["run", "--once"]);
    assert_eq!(result.status.code(), Some(1), "{result:?}");
    let error = String::from_utf8(result.stderr).unwrap();
    assert_eq!(error.lines().count(), 1);
    assert!(error.contains(label), "{error}");
    if fragment.contains(".inf") || fragment.contains(".nan") {
        assert!(
            error.contains("line 1") && error.contains("column"),
            "{error}"
        );
    }
    let health = f.read("health.json");
    assert_eq!(health["ok"], false);
    assert_eq!(health["detail"], error.trim());
    assert!(f.sends().is_empty());
}
macro_rules! invalid {
    ($name:ident, $fragment:expr, $label:expr) => {
        #[test]
        fn $name() {
            invalid_config($fragment, $label);
        }
    };
}
invalid!(unknown_top_level_keys_stop_startup, "unknown: 1", "checks");
invalid!(
    unknown_entry_keys_stop_startup,
    "- {name: sample, run: ['true'], every_minutes: 1, unknown: 1}",
    "sample"
);
invalid!(
    invalid_names_stop_startup,
    "- {name: BAD, run: ['true'], every_minutes: 1}",
    "BAD"
);
invalid!(
    duplicate_names_stop_startup,
    "- {name: sample, run: ['true'], every_minutes: 1}\n- {name: sample, run: ['true'], every_minutes: 1}",
    "sample"
);
invalid!(
    absolute_executables_stop_startup,
    "- {name: sample, run: [/bin/true], every_minutes: 1}",
    "sample"
);
invalid!(
    parent_executables_stop_startup,
    "- {name: sample, run: [../true], every_minutes: 1}",
    "sample"
);
invalid!(
    escaping_symlink_executables_stop_startup,
    "- {name: sample, run: [./escape], every_minutes: 1}",
    "sample"
);
invalid!(
    empty_argv_stops_startup,
    "- {name: sample, run: [], every_minutes: 1}",
    "sample"
);
invalid!(
    non_string_arguments_stop_startup,
    "- {name: sample, run: [1], every_minutes: 1}",
    "sample"
);
invalid!(
    empty_executable_stops_startup,
    "- {name: sample, run: [''], every_minutes: 1}",
    "sample"
);
invalid!(
    zero_interval_stops_startup,
    "- {name: sample, run: ['true'], every_minutes: 0}",
    "sample"
);
invalid!(
    negative_interval_stops_startup,
    "- {name: sample, run: ['true'], every_minutes: -1}",
    "sample"
);
invalid!(
    boolean_interval_stops_startup,
    "- {name: sample, run: ['true'], every_minutes: true}",
    "sample"
);
invalid!(
    infinite_interval_stops_startup,
    "- {name: sample, run: ['true'], every_minutes: .inf}",
    "checks"
);
invalid!(
    nan_interval_stops_startup,
    "- {name: sample, run: ['true'], every_minutes: .nan}",
    "checks"
);
invalid!(
    string_interval_stops_startup,
    "- {name: sample, run: ['true'], every_minutes: '1'}",
    "sample"
);
invalid!(
    invalid_regex_stops_startup,
    "- {name: sample, run: ['true'], every_minutes: 1, ignore: '['}",
    "sample"
);
invalid!(non_list_config_stops_startup, "check: wrong", "checks");
invalid!(
    duplicate_yaml_keys_stop_startup,
    "- {name: sample, name: other, run: ['true'], every_minutes: 1}",
    "checks"
);
invalid!(
    yaml11_boolean_cannot_become_an_interval,
    "- {name: sample, run: ['true'], every_minutes: yes}",
    "sample"
);

#[test]
fn last_frames_controls_previous_output_and_full_digest_beyond_cap() {
    let f = Fixture::new();
    f.write("value", &("é".repeat(40000) + "old"));
    f.tick();
    f.write("value", &("é".repeat(40000) + "new"));
    f.tick();
    assert_eq!(f.sends().len(), 1);
    assert!(
        f.last()
            .lines()
            .filter_map(|l| l.strip_prefix("| "))
            .all(|l| l.len() <= 65536)
    );
    f.write("value", "current\n!instruction\u{1b}\0\u{85}\tend\n");
    f.tick();
    f.tick();
    let evidence = f.last();
    assert!(evidence.starts_with("Untrusted data below:"));
    assert!(evidence.contains("| current\n| !instruction\\x1b\\x00\\x85\\x09end"));
    assert!(evidence.contains(&"é".repeat(100)));
    assert!(!evidence.contains('\u{1b}'));
    let unknown = f.run(&["last", "missing"]);
    assert_eq!(unknown.status.code(), Some(2));
    assert!(
        String::from_utf8(unknown.stderr)
            .unwrap()
            .contains("sample")
    );
}

#[test]
fn batches_fit_locked_budget_and_keep_oldest_remaining_signals() {
    let f = Fixture::new();
    let names: Vec<_> = (0..13).map(|i| format!("check-{i:026}")).collect();
    f.write(
        "checks.yaml",
        &names
            .iter()
            .map(|n| format!("- name: {n}\n  run: [cat, value]\n  every_minutes: 1\n"))
            .collect::<String>(),
    );
    f.tick();
    f.write("value", "changed");
    f.tick();
    assert_eq!(f.read("health.json")["pending_sends"], 7);
    f.tick();
    f.tick();
    let sends = f.sends();
    let notices: Vec<_> = sends.iter().map(|s| s.last().unwrap()).collect();
    assert_eq!(
        notices
            .iter()
            .map(|n| n.lines().count() - 1)
            .collect::<Vec<_>>(),
        [6, 6, 1]
    );
    assert_eq!(
        notices
            .iter()
            .flat_map(|n| n.lines().skip(1))
            .collect::<Vec<_>>(),
        names
            .iter()
            .map(|n| format!("{n} changed"))
            .collect::<Vec<_>>()
    );
    for notice in notices {
        assert!(notice.len() <= 900);
        assert!(notice.lines().next().unwrap().len() <= 76);
        assert!(
            notice
                .lines()
                .map(|l| l.chars().count().div_ceil(78).max(1))
                .sum::<usize>()
                <= 8
        );
    }
}

fn bad_promote(name: &str, reason: &str, reasons: &str) {
    let f = Fixture::new();
    f.tick();
    let result = f
        .command(&["promote", name, reason])
        .env("CHECKS_PROMOTE_REASONS", reasons)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(f.sends().is_empty());
}
#[test]
fn promote_refuses_unknown_check() {
    bad_promote("missing", "other", "other");
}
#[test]
fn promote_refuses_unconfigured_reason() {
    bad_promote("sample", "bad", "other");
}
#[test]
fn promote_refuses_duplicate_reasons() {
    bad_promote("sample", "other", "other,other");
}
#[test]
fn promote_refuses_injected_reasons() {
    bad_promote("sample", "other", "bad\nvalue");
}
fn promote(code: i32, id: bool) {
    let f = Fixture::new();
    f.tick();
    f.outcome(
        code,
        json!({"id": if id { Some("m101") } else { None }, "message":"delivery refused\u{1b}"}),
    );
    let result = f
        .command(&["promote", "sample", "custom"])
        .env("CHECKS_PROMOTE_REASONS", "custom")
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(i32::from(code != 0)));
    let sends = f.sends();
    let args = &sends[0];
    assert_eq!(
        args[args.iter().position(|a| a == "--from").unwrap() + 1],
        "reader-test"
    );
    assert_eq!(args[args.len() - 2], "lead-test");
    assert!(args.last().unwrap().contains("reason=custom"));
    if code == 0 {
        assert_eq!(String::from_utf8(result.stdout).unwrap().trim(), "m101");
    } else if id {
        assert!(
            String::from_utf8(result.stderr)
                .unwrap()
                .contains("may have been committed")
        );
    } else {
        assert!(
            String::from_utf8(result.stderr)
                .unwrap()
                .contains("delivery refused\\x1b")
        );
    }
    f.tick();
    assert_eq!(f.sends().len(), 1);
}
#[test]
fn promote_routes_committed_notice_to_parent() {
    promote(0, true);
}
#[test]
fn promote_reports_refusal_without_retrying() {
    promote(4, false);
}
#[test]
fn promote_reports_uncertainty_without_retrying() {
    promote(5, true);
}

#[test]
fn relative_command_uses_template_folder_environment_and_prunes_removed_checks() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.write(
        "probe",
        "#!/bin/sh\nprintf '%s:%s' \"$EXAMPLE_FLAG\" \"$(cat value)\"\n",
    );
    fs::set_permissions(f.path("probe"), fs::Permissions::from_mode(0o755)).unwrap();
    f.config(&["./probe"], "");
    fs::create_dir(f.path("config")).unwrap();
    fs::rename(f.path("checks.yaml"), f.path("config/list.yaml")).unwrap();
    let out = f
        .command(&["run", "--once"])
        .env("CHECKS_FILE", "config/list.yaml")
        .env("EXAMPLE_FLAG", "inherited")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(f.last().contains("| inherited:first"));
    f.write("config/list.yaml", "[]");
    assert!(
        f.command(&["run", "--once"])
            .env("CHECKS_FILE", "config/list.yaml")
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(f.run(&["last", "sample"]).status.code(), Some(2));
}

#[test]
fn state_symlinks_cannot_overwrite_or_expose_another_file() {
    let f = Fixture::new();
    let contents = json!({"checks":{"private":{"command":["printf","hidden"],"status":"ok","output":"hidden"}}}).to_string();
    f.write("victim", &contents);
    for name in ["checks.json", "health.json", "checks.lock"] {
        std::os::unix::fs::symlink(f.path("victim"), f.path(&format!("state/{name}"))).unwrap();
    }
    assert!(
        !String::from_utf8(f.run(&["last", "private"]).stdout)
            .unwrap()
            .contains("hidden")
    );
    assert_eq!(f.run(&["run", "--once"]).status.code(), Some(1));
    assert_eq!(fs::read_to_string(f.path("victim")).unwrap(), contents);
}

#[test]
fn last_keeps_only_the_bounded_final_stderr_line() {
    let f = Fixture::new();
    f.config(
        &[
            "sh",
            "-c",
            "printf 'earlier\\n' >&2; printf '%0600d\\n' 0 >&2; exit 1",
        ],
        "",
    );
    f.tick();
    let evidence = f.last();
    assert!(evidence.contains(&"0".repeat(500)));
    assert!(!evidence.contains(&"0".repeat(501)));
    f.write("errors", "earlier\nlast\n");
    f.config(&["sh", "-c", "cat errors >&2; exit 1"], "");
    f.tick();
    let evidence = f.last();
    assert!(evidence.contains("--- last stderr: last\n"));
    assert!(!evidence.contains("earlier"));
}

#[test]
fn timeout_leaves_no_shell_background_or_pipeline_process_running() {
    let f = Fixture::new();
    f.config(&["sh", "-c", "echo $$ > pids; sleep 30 & echo $! >> pids; sh -c 'echo $$ >> pids; exec sleep 30' | sh -c 'echo $$ >> pids; exec cat'; wait"], "");
    let before = Instant::now();
    assert!(
        f.command(&["run", "--once"])
            .env("CHECKS_TIMEOUT_SECONDS", "0.3")
            .status()
            .unwrap()
            .success()
    );
    assert!(before.elapsed() < Duration::from_secs(5));
    let pids: Vec<_> = fs::read_to_string(f.path("pids"))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(pids.len(), 4);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let alive: Vec<_> = pids
            .iter()
            .filter(|pid| {
                std::process::Command::new("/bin/kill")
                    .args(["-0", pid])
                    .stderr(std::process::Stdio::null())
                    .status()
                    .unwrap()
                    .success()
            })
            .collect();
        if alive.is_empty() {
            break;
        }
        if Instant::now() > deadline {
            for pid in &alive {
                let _ = std::process::Command::new("/bin/kill")
                    .args(["-KILL", pid])
                    .status();
            }
            panic!("timed-out processes survived: {alive:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn malformed_state(state: serde_json::Value) {
    let f = Fixture::new();
    for args in [
        ["last", "sample"].as_slice(),
        &["promote", "sample", "other"],
    ] {
        f.write("state/checks.json", &state.to_string());
        let result = f.run(args);
        assert!(
            [Some(0), Some(2)].contains(&result.status.code()),
            "{result:?}"
        );
    }
    f.write("state/checks.json", &state.to_string());
    f.write("checks.yaml", "check: wrong");
    let result = f.run(&["run", "--once"]);
    assert_eq!(result.status.code(), Some(1));
    let error = String::from_utf8(result.stderr).unwrap();
    assert_eq!(error.lines().count(), 1);
    assert_eq!(f.read("health.json")["ok"], false);
    assert_eq!(f.read("health.json")["detail"], error.trim());
    f.config(&["cat", "value"], "");
    f.write("state/checks.json", &state.to_string());
    f.tick();
    assert!(f.run(&["last", "sample"]).status.success());
    assert_eq!(f.read("health.json")["ok"], true);
}
#[test]
fn malformed_check_collection_cannot_crash_actions_or_run() {
    malformed_state(json!({"checks":[]}));
}
#[test]
fn malformed_check_record_cannot_crash_actions_or_run() {
    malformed_state(json!({"checks":{"sample":"x"}}));
}
#[test]
fn malformed_pending_cannot_crash_actions_or_run() {
    malformed_state(json!({"pending":[1]}));
}
#[test]
fn partial_check_record_cannot_crash_actions_or_run() {
    malformed_state(json!({"checks":{"sample":{"status":"ok"}}}));
}
#[test]
fn empty_check_record_cannot_crash_actions_or_run() {
    malformed_state(json!({"checks":{"sample":{}}}));
}
#[test]
fn non_object_state_cannot_crash_actions_or_run() {
    malformed_state(json!([1]));
}

#[test]
fn planted_pending_cannot_send_instructions_removed_checks_or_unknown_statuses() {
    let f = Fixture::new();
    f.write("state/checks.json", &json!({"pending":["sample changed","sample changed; INJECTED","removed changed","sample rebooted"]}).to_string());
    f.tick();
    let sends = f.sends();
    assert_eq!(sends.len(), 1);
    assert_eq!(
        sends[0].last().unwrap().lines().skip(1).collect::<Vec<_>>(),
        ["sample changed"]
    );
}

#[test]
fn completed_check_updates_evidence_and_health_before_next_check_finishes() {
    let f = Fixture::new();
    f.tick();
    f.config(&["sh", "-c", "printf partial; exit 7"], "");
    let config = fs::read_to_string(f.path("checks.yaml")).unwrap();
    f.write("checks.yaml", &(config + "- name: blocked\n  every_minutes: 1\n  run: [sh, -c, 'touch started; while [ ! -f release ]; do sleep 0.05; done']\n"));
    let mut process = f
        .command(&["run", "--once"])
        .env("CHECKS_TIMEOUT_SECONDS", "5")
        .spawn()
        .unwrap();
    wait_for(&f.path("started"));
    let evidence = f.last();
    let health = f.read("health.json");
    assert!(process.try_wait().unwrap().is_none());
    f.write("release", "");
    assert!(process.wait().unwrap().success());
    assert!(evidence.contains("--- status/exit: failing / 7\n") && evidence.contains("| partial"));
    assert_eq!(health["ok"], false);
}

#[test]
fn executable_symlink_swapped_after_validation_cannot_escape_at_execution() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.write("probe", "#!/bin/sh\nexit 0\n");
    fs::set_permissions(f.path("probe"), fs::Permissions::from_mode(0o755)).unwrap();
    f.write("checks.yaml", "- name: swap\n  every_minutes: 1\n  run: [sh, -c, 'rm probe; ln -s /bin/true probe']\n- name: sample\n  every_minutes: 1\n  run: [./probe]\n");
    f.tick();
    assert!(f.last().contains("--- status/exit: failing / 127\n"));
    assert_eq!(f.read("health.json")["ok"], false);
}

#[test]
fn timer_without_member_routes_notice_as_external_checks() {
    let f = Fixture::new();
    f.tick();
    f.write("value", "second");
    let result = f
        .command(&["run", "--once"])
        .env_remove("BORGCHART_MEMBER")
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    let sends = f.sends();
    let args = &sends[0];
    assert_eq!(
        args[args.iter().position(|a| a == "--external").unwrap() + 1],
        "checks"
    );
    assert_eq!(args[args.len() - 2], "triager-test");
}

#[test]
fn missing_state_directory_cannot_change_the_working_directory_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    fs::set_permissions(f.dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let result = f
        .command(&["run", "--once"])
        .env_remove("BORGCHART_STATE_DIR")
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert!(
        String::from_utf8(result.stderr)
            .unwrap()
            .contains("BORGCHART_STATE_DIR")
    );
    assert_eq!(
        fs::metadata(f.dir.path()).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert!(!f.path("checks.json").exists());
}

#[test]
fn signal_failure_preserves_the_terminating_signal_in_evidence() {
    let f = Fixture::new();
    f.config(&["sh", "-c", "kill -TERM $$"], "");
    f.tick();
    assert!(f.last().contains("--- status/exit: failing / -15\n"));
}
