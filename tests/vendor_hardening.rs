//! Delivery and launch diagnostics through the CLI, on private fake members.
#![cfg(feature = "test-support")]
mod common;
mod team_env;

use serde_json::Value;
use std::fs;
use team_env::{Env, stdout};

fn setup(interactive: bool) -> (Env, String, String) {
    let e = Env::new();
    if interactive {
        e.write(
            "templates/oncall/template.yaml",
            "roles:\n  lead: {prompt: lead.md, interactive: true}\n",
        );
    }
    let team = e.create("oncall", "work/a");
    let member = e.member(&team, "lead");
    (e, team, member)
}

fn command(e: &Env, team: &str, member: &str, text: &str) {
    let target = format!("brg-{team}:{member}");
    e.tmux(&["send-keys", "-t", &target, "-l", text]);
    e.tmux(&["send-keys", "-t", &target, "Enter"]);
    e.wait_for("fake command", || {
        fs::read_to_string(e.path(&format!("home/fake-agent/{member}.jsonl")))
            .unwrap_or_default()
            .lines()
            .filter_map(|s| serde_json::from_str::<Value>(s).ok())
            .any(|v| v["event"] == "submit" && v["text"] == text)
    });
}

fn send(e: &Env, team: &str, member: &str) -> Value {
    serde_json::from_slice(
        &e.ok(&[
            "msg",
            "send",
            &format!("{team}/{member}"),
            "Task",
            "--external",
            "test",
            "--json",
        ])
        .stdout,
    )
    .unwrap()
}

fn alert(e: &Env, team: &str) -> bool {
    use borgchart::state::shell_quote;
    let cmd = e.cmd(&["status", team]);
    let env = cmd
        .get_envs()
        .filter_map(|(k, v)| {
            v.map(|v| {
                format!(
                    "{}={}",
                    k.to_string_lossy(),
                    shell_quote(&v.to_string_lossy())
                )
            })
        })
        .collect::<Vec<_>>()
        .join(" ");
    let script = e.path("status.sh");
    fs::write(
        &script,
        format!(
            "env -i {env} {} status {}; echo SCENE-DONE; exec sleep 600\n",
            shell_quote(env!("CARGO_BIN_EXE_brg")),
            shell_quote(team)
        ),
    )
    .unwrap();
    e.tmux(&[
        "new-session",
        "-d",
        "-s",
        "probe",
        "-x",
        "240",
        "-y",
        "30",
        "sh",
        script.to_str().unwrap(),
    ]);
    let capture = || e.tmux(&["capture-pane", "-p", "-t", "=probe:"]);
    e.wait_for("terminal status", || capture().contains("SCENE-DONE"));
    let text = capture();
    e.tmux(&["kill-session", "-t", "=probe"]);
    let needs_attention = text.contains("1 need you");
    if needs_attention {
        let member = e.member(team, "lead");
        assert!(text.contains(&format!("! {member}")), "{text}");
    }
    needs_attention
}

#[test]
fn unreadable_delivery_alert_survives_status_and_clears_after_typed_send() {
    let (e, team, member) = setup(false);
    command(&e, &team, &member, "unreadable");
    let sent = send(&e, &team, &member);
    assert_eq!(sent["delivery"], "stored");
    let row = e.row(&team, "lead");
    assert_eq!(row["delivery"]["id"], sent["id"]);
    assert_eq!(row["delivery"]["outcome"], "stored");
    assert_eq!(row["delivery"]["reason"], sent["reason"]);
    assert!(alert(&e, &team));
    let plain = stdout(&e.ok(&["status", &team]));
    assert!(plain.contains(sent["reason"].as_str().unwrap()), "{plain}");
    let path = e.path(&format!("state/{team}/members/{member}/delivery.json"));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    command(&e, &team, &member, "work 600");
    assert_eq!(e.row(&team, "lead")["state"], "working");
    assert!(alert(&e, &team));
    command(&e, &team, &member, "readable");
    assert_eq!(send(&e, &team, &member)["delivery"], "typed");
    assert!(e.row(&team, "lead").get("delivery").is_none());
    assert!(!alert(&e, &team));
}

#[test]
fn stopped_and_inbox_only_members_do_not_get_delivery_alerts() {
    for interactive in [false, true] {
        let (e, team, member) = setup(interactive);
        if !interactive {
            e.ok(&["stop", &team]);
        }
        assert_eq!(send(&e, &team, &member)["delivery"], "stored");
        assert!(e.row(&team, "lead").get("delivery").is_none());
        assert!(
            !e.path(&format!("state/{team}/members/{member}/delivery.json"))
                .exists()
        );
    }
}

#[test]
fn stopping_after_failed_delivery_removes_the_alert() {
    let (e, team, member) = setup(false);
    command(&e, &team, &member, "unreadable");
    send(&e, &team, &member);
    assert!(alert(&e, &team));
    e.ok(&["stop", &team]);
    assert!(e.row(&team, "lead").get("delivery").is_none());
    assert!(!alert(&e, &team));
}

#[test]
fn acknowledging_failed_delivery_removes_the_alert() {
    let (e, team, member) = setup(false);
    command(&e, &team, &member, "unreadable");
    let sent = send(&e, &team, &member);
    command(&e, &team, &member, "readable");
    assert!(alert(&e, &team));
    let start = e.starts(&member).last().unwrap().clone();
    let ack = e
        .cmd(&["msg", "ack", sent["id"].as_str().unwrap()])
        .env("BORGCHART_TEAM", &team)
        .env("BORGCHART_MEMBER", &member)
        .env(
            "BORGCHART_INCARNATION",
            start["env"]["BORGCHART_INCARNATION"].as_str().unwrap(),
        )
        .output()
        .unwrap();
    assert!(ack.status.success(), "{}", team_env::both(&ack));
    assert!(e.row(&team, "lead").get("delivery").is_none());
    assert!(!alert(&e, &team));
}

#[test]
fn oversized_message_pointer_does_not_raise_a_delivery_alert() {
    let (e, team, member) = setup(false);
    let sent: Value = serde_json::from_slice(
        &e.ok(&[
            "msg",
            "send",
            &format!("{team}/{member}"),
            &"x".repeat(1200),
            "--external",
            "test",
            "--json",
        ])
        .stdout,
    )
    .unwrap();
    assert_eq!(sent["delivery"], "typed");
    assert!(
        !e.path(&format!("state/{team}/members/{member}/delivery.json"))
            .exists()
    );
    assert!(e.row(&team, "lead").get("delivery").is_none());
    assert!(!alert(&e, &team));
}

#[test]
fn delivery_status_save_failure_keeps_a_typed_send_successful() {
    use std::os::unix::fs::PermissionsExt;
    let (e, team, member) = setup(false);
    command(&e, &team, &member, "unreadable");
    send(&e, &team, &member);
    command(&e, &team, &member, "readable");
    let folder = e.path(&format!("state/{team}/members/{member}"));
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o500)).unwrap();
    let out = e.brg(&[
        "msg",
        "send",
        &format!("{team}/{member}"),
        "Task",
        "--external",
        "test",
        "--json",
    ]);
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", team_env::both(&out));
    let sent: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(sent["delivery"], "typed", "{sent}");
    assert!(
        sent["reason"]
            .as_str()
            .unwrap()
            .contains("cannot save delivery status"),
        "{sent}"
    );
    let log = fs::read_to_string(e.path(&format!("home/fake-agent/{member}.jsonl"))).unwrap();
    assert!(
        log.lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .any(|v| v["event"] == "submit"
                && v["text"]
                    .as_str()
                    .is_some_and(|text| text.contains(sent["id"].as_str().unwrap())))
    );
}

#[test]
fn locked_send_failure_after_commit_raises_a_delivery_alert() {
    let e = Env::new();
    e.write(
        "templates/oncall/template.yaml",
        "roles:\n  lead: {prompt: lead.md, restricted_access: true}\n",
    );
    let team = e.create("oncall", "work/a");
    let member = e.member(&team, "lead");
    let lock = e.path(&format!("state/{team}/locks/{member}.lock"));
    fs::remove_file(&lock).unwrap();
    fs::create_dir(&lock).unwrap();
    let out = e.brg(&[
        "msg",
        "send",
        &format!("{team}/{member}"),
        "Task",
        "--external",
        "test",
        "--json",
    ]);
    fs::remove_dir(&lock).unwrap();
    assert_eq!(out.status.code(), Some(5), "{}", team_env::both(&out));
    let sent: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(sent["delivery"], "recorded-not-delivered", "{sent}");
    assert_eq!(e.row(&team, "lead")["delivery"]["id"], sent["id"]);
    assert!(alert(&e, &team));
}

#[test]
fn broken_wake_rules_notice_explains_the_configuration_failure() {
    let (e, team, _) = setup(false);
    e.write("home/.config/borgchart/rules/wake.yaml", "wake: [");
    let row = e.row(&team, "lead");
    assert_eq!(row["verified"], false);
    let detail = row["detail"].as_str().unwrap();
    assert!(
        detail.contains("wake rules can't be read") && detail.contains("wake.yaml"),
        "{detail}"
    );
    assert!(!alert(&e, &team));
}

#[test]
fn restart_ignores_delivery_failure_from_the_previous_incarnation() {
    let (e, team, member) = setup(false);
    command(&e, &team, &member, "unreadable");
    send(&e, &team, &member);
    assert!(e.row(&team, "lead").get("delivery").is_some());
    e.ok(&["stop", &team]);
    e.ok(&["start", &team]);
    e.wait_for("restarted idle", || e.state_of(&team, "lead") == "idle");
    assert!(e.row(&team, "lead").get("delivery").is_none());
    assert!(!alert(&e, &team));
}

#[test]
fn idle_unreadable_box_alerts_but_readable_normal_mode_and_interactive_do_not() {
    for interactive in [false, true] {
        let (e, team, member) = setup(interactive);
        e.write("home/.config/borgchart/rules/wake.yaml", "wake:\n  - vendor: claude\n    version: \"fake-agent 0.0.0 (claude)\"\n    methods: [typed]\n");
        assert!(e.row(&team, "lead").get("detail").is_none());
        assert!(!alert(&e, &team));
        command(&e, &team, &member, "normal");
        assert!(!alert(&e, &team));
        command(&e, &team, &member, "unreadable");
        let row = e.row(&team, "lead");
        assert_eq!(row["state"], "idle");
        assert_eq!(row.get("detail").is_some(), !interactive, "{row}");
        assert_eq!(alert(&e, &team), !interactive);
    }
}

#[test]
fn unlisted_versions_are_notices_and_override_versions_are_verified() {
    let (e, team, _) = setup(false);
    let row = e.row(&team, "lead");
    assert_eq!(row["verified"], false);
    assert!(!row["detail"].as_str().unwrap().is_empty());
    assert!(!alert(&e, &team));
    e.write("home/.config/borgchart/rules/wake.yaml", "wake:\n  - vendor: claude\n    version: \"fake-agent 0.0.0 (claude)\"\n    methods: [typed]\n");
    let row = e.row(&team, "lead");
    assert_eq!(row["verified"], true, "{row}");
    assert!(row.get("detail").is_none());
    assert!(!alert(&e, &team));
}

#[test]
fn wake_qualification_uses_shared_list_and_keeps_input_requirements() {
    let mode = std::env::var("BRG_TEST_WAKE_QUALIFICATION");
    if mode.is_err() {
        for mode in ["bundled", "override"] {
            let dir = tempfile::tempdir().unwrap();
            if mode == "override" {
                let folder = dir.path().join("borgchart/rules");
                fs::create_dir_all(&folder).unwrap();
                fs::write(
                    folder.join("wake.yaml"),
                    "wake:\n  - vendor: claude\n    version: \"2.1.777\"\n    methods: [typed]\n",
                )
                .unwrap();
            }
            let out = common::command(std::env::current_exe().unwrap())
                .env_clear()
                .env("HOME", dir.path())
                .env("XDG_CONFIG_HOME", dir.path())
                .env("BRG_TEST_WAKE_QUALIFICATION", mode)
                .args([
                    "--exact",
                    "wake_qualification_uses_shared_list_and_keeps_input_requirements",
                    "--nocapture",
                ])
                .output()
                .unwrap();
            assert!(out.status.success(), "{mode}: {}", team_env::both(&out));
        }
        return;
    }
    let overridden = mode.unwrap() == "override";
    use borgchart::{
        team::Vendor,
        wake::{LaunchFacts, Qualification, qualification},
    };
    let mut facts = LaunchFacts {
        vendor_version: Some("2.1.325 (Claude Code)".into()),
        editor: Some("vim".into()),
        suggestions: Some(false),
        ..LaunchFacts::default()
    };
    if overridden {
        assert!(matches!(
            qualification(&facts),
            Qualification::Unqualified(_)
        ));
        facts.vendor_version = Some("2.1.777 (Claude Code)".into());
    }
    assert_eq!(qualification(&facts), Qualification::Qualified);
    let rules = borgchart::watcher::methods::load().unwrap();
    assert_eq!(
        borgchart::wake::verified(&rules, Vendor::Codex, Some("codex-cli 0.165.0")),
        !overridden
    );
    facts.suggestions = Some(true);
    assert!(matches!(
        qualification(&facts),
        Qualification::Unqualified(_)
    ));
    facts.suggestions = Some(false);
    facts.editor = Some("normal".into());
    assert!(matches!(
        qualification(&facts),
        Qualification::Unqualified(_)
    ));
}

#[test]
fn codex_missing_session_alerts_after_start_grace_and_clears_when_ready() {
    for interactive in [false, true] {
        let e = Env::new();
        e.write(
            "templates/oncall/template.yaml",
            &format!(
                "roles:\n  lead: {{prompt: lead.md, vendor: codex, interactive: {interactive}}}\n"
            ),
        );
        e.write(
            "templates/oncall/lead.md",
            "---\nmodels: {codex: example-model}\n---\nReady.\n",
        );
        let team = e.create("oncall", "work/a");
        let member = e.member(&team, "lead");
        let state = borgchart::state::StateDir::from_root(&e.path(&format!("state/{team}")));
        let mut inc = borgchart::lifecycle::load_incarnation(&state, &member)
            .unwrap()
            .unwrap();
        let _ = fs::remove_file(state.member_session(&member));
        assert!(!alert(&e, &team));
        inc.started_at = "2026-11-09T00:00:00Z".into();
        borgchart::lifecycle::begin_incarnation(&state, &inc).unwrap();
        assert!(alert(&e, &team), "missing session should need attention");
        fs::write(
            state.member_session(&member),
            "01a1d300-f48d-7c70-a58a-d5fbea1edd20\n",
        )
        .unwrap();
        assert!(
            !alert(&e, &team),
            "recorded session should restore delivery readiness"
        );
        command(&e, &team, &member, "unreadable");
        assert!(
            !alert(&e, &team),
            "Codex delivery does not depend on a readable composer"
        );
    }
}

#[test]
fn codex_failed_queue_alert_clears_after_a_successful_queue() {
    use std::os::unix::fs::PermissionsExt;
    for locked in [false, true] {
        let e = Env::new();
        e.write(
            "templates/oncall/template.yaml",
            &format!(
                "roles:\n  lead: {{prompt: lead.md, vendor: codex, restricted_access: {locked}}}\n"
            ),
        );
        e.write(
            "templates/oncall/lead.md",
            "---\nmodels: {codex: example-model}\n---\nReady.\n",
        );
        let team = e.create("oncall", "work/a");
        let member = e.member(&team, "lead");
        let cli = e.path("bin/codex");
        fs::remove_file(&cli).unwrap();
        fs::write(&cli, "#!/bin/sh\nexit 3\n").unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).unwrap();
        let failed = e.brg(&[
            "msg",
            "send",
            &format!("{team}/{member}"),
            "first task",
            "--external",
            "test",
            "--json",
        ]);
        assert_eq!(failed.status.code(), Some(if locked { 5 } else { 0 }));
        let sent: Value = serde_json::from_slice(&failed.stdout).unwrap();
        assert_eq!(e.row(&team, "lead")["delivery"]["id"], sent["id"]);
        assert!(alert(&e, &team));
        fs::write(&cli, "#!/bin/sh\nexit 0\n").unwrap();
        assert_eq!(send(&e, &team, &member)["delivery"], "queued");
        assert!(e.row(&team, "lead").get("delivery").is_none());
        assert!(!alert(&e, &team));
    }
}
