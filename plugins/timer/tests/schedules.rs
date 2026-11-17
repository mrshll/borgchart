//! Recurring wakes exercised through the timer command and bus.
#[allow(dead_code)]
mod support;
use borgchart::bus::time;
use serde_json::json;
use support::*;

#[test]
fn due_schedule_sends_only_name_and_time_to_the_delivery_target() {
    let f = Fixture::new();
    f.write(
        "checks.yaml",
        "- {name: review, cron: '0 * * * *', missed: catch-up}",
    );
    f.write(
        "state/checks.json",
        &json!({"schedules":{"review":{"cron":"0 * * * *","next":"2020-01-01T00:00:00Z"}}})
            .to_string(),
    );
    let before = time::utc_now();
    let result = f
        .command(&["run", "--once"])
        .env_remove("BORGCHART_MEMBER")
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    let sends = f.sends();
    assert_eq!(sends.len(), 1);
    let args = &sends[0];
    assert_eq!(&args[..5], ["msg", "send", "--json", "--external", "timer"]);
    assert_eq!(args[args.len() - 2], "triager-test");
    let after = time::utc_now();
    assert!(
        [before, after]
            .iter()
            .any(|now| args.last().unwrap() == &format!("review {}:00Z", &now[..13])),
        "unexpected body: {:?}",
        args.last()
    );
}

#[test]
#[rustfmt::skip]
fn schedule_prompt_is_delivered_on_the_second_line() {
    let f = Fixture::new();
    pending_schedule(&f);
    f.write("checks.yaml", "- {name: review, cron: '0 * * * *', missed: catch-up, prompt: '  Follow notes/review.md  '}");
    f.tick();
    let sends = f.sends();
    let (first, prompt) = sends[0].last().unwrap().split_once('\n').unwrap();
    assert!(first.starts_with("review ") && first.ends_with(":00Z"));
    assert_eq!(prompt, "Follow notes/review.md");
}

#[test]
#[rustfmt::skip]
fn validate_and_startup_refuse_invalid_prompts_with_a_fix() {
    let f = Fixture::new();
    for value in ["' '", "null", "42", "true", "[text]", "\"line\\nline\"", "\"\\ttext\"", "\"text\\u007f\"", &format!("'{}'", "é".repeat(151))] {
        f.write("checks.yaml", &format!("- {{name: review, cron: '0 * * * *', prompt: {value}}}"));
        let result = f.run(&["validate"]);
        assert_eq!(result.status.code(), Some(1), "{value}");
        assert!(String::from_utf8_lossy(&result.stderr).contains("write prompt as"), "{value}");
        assert_eq!(result.stderr, f.run(&["run", "--once"]).stderr);
    }
    f.config(&["printf"], "  prompt: text");
    assert_eq!(f.run(&["validate"]).status.code(), Some(1));
    assert!(String::from_utf8_lossy(&f.run(&["validate"]).stderr).contains("use only name, run, every_minutes and ignore"));
    f.write("checks.yaml", &format!("- {{name: review, cron: '0 * * * *', prompt: ' {} '}}", "é".repeat(150)));
    assert!(f.run(&["validate"]).status.success());
}

#[test]
#[rustfmt::skip]
fn validate_reports_counts_or_the_startup_error_without_running_anything() {
    let f = Fixture::new();
    f.write("checks.yaml", "- {name: probe, run: [touch, executed], every_minutes: 1}\n- {name: review, cron: '0 0 * * *'}");
    let result = f.command(&["validate"]).env_remove("BORGCHART_STATE_DIR").output().unwrap();
    assert!(result.status.success(), "{result:?}");
    assert_eq!(String::from_utf8(result.stdout).unwrap(), "valid: 1 checks, 1 schedules\n");
    assert!(!f.path("executed").exists());
    assert!(f.sends().is_empty());
    f.write("other.yaml", "- {name: review, cron: '@daily'}");
    let validate = f.run(&["validate", "other.yaml"]);
    let startup = f.command(&["run", "--once"]).env("CHECKS_FILE", "other.yaml").output().unwrap();
    assert_eq!(validate.status.code(), Some(1));
    assert_eq!(validate.stderr, startup.stderr);
}

#[test]
fn schedule_without_delivery_target_stops_startup() {
    let f = Fixture::new();
    f.write("checks.yaml", "- {name: review, cron: '0 0 * * *'}");
    for target in [None, Some("bad target")] {
        let mut command = f.command(&["run", "--once"]);
        if let Some(target) = target {
            command.env("BORGCHART_DELIVER_TO", target);
        } else {
            command.env_remove("BORGCHART_DELIVER_TO");
        }
        assert_eq!(command.output().unwrap().status.code(), Some(1));
        assert_eq!(f.read("health.json")["ok"], false);
    }
    assert!(f.sends().is_empty());
}
fn pending_schedule(f: &Fixture) {
    f.write(
        "checks.yaml",
        "- {name: review, cron: '0 * * * *', missed: catch-up}",
    );
    f.write(
        "state/checks.json",
        &json!({"schedules":{"review":{"cron":"0 * * * *","next":"2020-01-01T00:00:00Z"}}})
            .to_string(),
    );
}
#[test]
fn failed_schedule_send_retries_after_restart_and_health_recovers() {
    let f = Fixture::new();
    pending_schedule(&f);
    f.outcome(1, json!({"message":"bus unavailable"}));
    f.tick();
    assert_eq!(f.read("health.json")["ok"], false);
    assert!(f.read("health.json")["schedules"]["review"]["pending"].is_string());
    f.write(
        "checks.yaml",
        "- {name: review, cron: '0 * * * *', missed: skip}",
    );
    f.outcome(0, json!({"delivery":"uncertain"}));
    f.tick();
    f.tick();
    assert_eq!(f.sends().len(), 2);
    assert_eq!(f.read("health.json")["ok"], true);
    assert_eq!(
        f.read("health.json")["schedules"]["review"]["pending"],
        serde_json::Value::Null
    );
}
#[test]
fn persisted_attempts_hold_pending_schedules_at_the_cap_after_restart() {
    let f = Fixture::new();
    f.write("checks.yaml", "- {name: review, cron: '0 * * * *'}");
    f.write(
        "state/checks.json",
        &json!({
            "schedules":{"review":{"cron":"0 * * * *", "next":"2099-01-01T00:00:00Z", "pending":"2020-01-01T00:00:00Z"}},
            "schedule_sends":vec![time::utc_now(); 6]
        }).to_string(),
    );
    f.tick();
    assert!(f.sends().is_empty());
    let health = f.read("health.json");
    assert_eq!(health["ok"], false);
    assert!(health["detail"].as_str().unwrap().contains("schedule cap"));
}
#[test]
fn removed_schedule_is_pruned() {
    let f = Fixture::new();
    pending_schedule(&f);
    f.outcome(1, json!({"message":"bus unavailable"}));
    f.tick();
    f.write("checks.yaml", "[]");
    f.tick();
    assert_eq!(f.sends().len(), 1);
    assert_eq!(f.read("health.json")["ok"], true);
    assert_eq!(f.read("health.json")["schedules"], json!({}));
}
#[test]
fn malformed_schedule_entries_do_not_block_valid_wakes() {
    let f = Fixture::new();
    for bad in [
        json!([]),
        json!({"broken":{}}),
        json!({"broken":"text"}),
        json!({"review":{"cron":"0 * * * *","next":"bad"}}),
        json!({"review":{"cron":"0 * * * *","next":"2020-01-01T00:00:00Z","pending":"injected"}}),
        json!({"review":{"cron":"0 * * * *","next":"2020-01-01T00:01:00Z"}}),
        json!({"review":{"cron":"0 * * * *","next":"2099-01-01T00:00:00Z","pending":"2099-01-01T00:00:00Z"}}),
    ] {
        f.write(
            "checks.yaml",
            "- {name: review, cron: '0 * * * *', missed: catch-up}",
        );
        f.write(
            "state/checks.json",
            &json!({"schedules":bad,"schedule_sends":[null,"bad",3]}).to_string(),
        );
        f.tick();
        assert!(f.sends().is_empty());
        assert_eq!(f.read("health.json")["ok"], true);
    }
    pending_schedule(&f);
    f.tick();
    assert_eq!(f.sends().len(), 1);
}

struct Running(std::process::Child);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn wait_until(mut ready: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(36);
    while !ready() {
        assert!(std::time::Instant::now() < deadline, "tick did not finish");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
#[test]
#[rustfmt::skip]
fn invalid_reload_keeps_last_good_configuration_and_health_not_ok() {
    let f = Fixture::new();
    pending_schedule(&f);
    let initial = std::fs::read_to_string(f.path("checks.yaml")).unwrap();
    f.write("checks.yaml", &(initial + "\n- {name: probe, run: [cat, value], every_minutes: 0.001}"));
    f.outcome(1, json!({"message":"bus unavailable"}));
    let _running = Running(f.command(&["run"]).spawn().unwrap());
    wait_until(|| f.path("state/health.json").exists() && f.read("health.json")["ok"] == false);
    f.write("checks.yaml", "- {name: review, cron: '0 * * * *', prompt: ''}");
    f.write("value", "second");
    std::fs::create_dir_all(f.path("state/alarms")).unwrap();
    f.write("state/alarms/reader-test.json", &json!({"team":"harbor-test","member":"reader-test","due":"2020-01-01T00:00:00Z","note":"review","set_at":"2020-01-01T00:00:00Z"}).to_string());
    f.outcome(0, json!({"delivery":"stored"}));
    wait_until(|| f.read("health.json")["detail"].as_str().unwrap().contains("last good file") && f.sends().len() == 4 && !f.path("state/alarms/reader-test.json").exists());
    assert_eq!(f.read("health.json")["ok"], false);
    assert_eq!(f.sends().len(), 4);
    assert!(!f.path("state/alarms/reader-test.json").exists());
    std::fs::remove_file(f.path("checks.yaml")).unwrap();
    wait_until(|| f.read("health.json")["detail"].as_str().unwrap().contains("cannot read YAML"));
    assert_eq!(f.read("health.json")["ok"], false);
}
#[test]
#[rustfmt::skip]
fn valid_reload_applies_new_and_removed_entries_without_restart() {
    let no_target = Fixture::new();
    no_target.write("checks.yaml", "[]");
    let _no_target = Running(no_target.command(&["run"]).env_remove("BORGCHART_DELIVER_TO").spawn().unwrap());
    wait_until(|| no_target.path("state/health.json").exists());
    no_target.write("checks.yaml", "- {name: review, cron: '0 0 * * *'}");
    let f = Fixture::new();
    f.write("checks.yaml", "- {name: old, cron: '0 * * * *'}\n- {name: sample, run: [cat, value], every_minutes: 1}");
    let _running = Running(f.command(&["run"]).spawn().unwrap());
    wait_until(|| f.path("state/health.json").exists());
    f.write("checks.yaml", "- {name: new, cron: '30 * * * *'}\n- {name: added, run: [touch, added], every_minutes: 1}");
    wait_until(|| f.path("added").exists() && f.read("health.json")["schedules"]["new"]["next"].is_string());
    wait_until(|| no_target.read("health.json")["ok"] == false);
    assert!(no_target.sends().is_empty());
    assert_eq!(f.read("health.json")["ok"], true);
    assert_eq!(f.run(&["last", "sample"]).status.code(), Some(2));
    assert!(f.read("health.json")["schedules"]["old"].is_null());
    assert!(f.sends().is_empty());
    f.write("checks.yaml", "[]");
    wait_until(|| f.read("health.json")["schedules"] == json!({}));
    assert_eq!(f.run(&["last", "added"]).status.code(), Some(2));
}
