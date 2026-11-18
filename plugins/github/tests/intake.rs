//! Process-boundary tests of the gh profile guard, CLI and strict journal.
mod support;
use brg_github::gh::{Api, Gh};
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt, process::Command};
use support::Fixture;

#[test]
fn missing_or_empty_gh_config_refuses_before_spawning_gh() {
    for value in [None, Some("")] {
        let f = Fixture::new();
        let mut command = f.command();
        Fixture::set(&mut command, "GH_CONFIG_DIR", value);
        command.output().unwrap();
        assert_eq!(f.health()["ok"], false);
        assert!(f.detail().contains("set GH_CONFIG_DIR"));
        assert!(f.calls().is_empty());
    }
}
#[test]
fn inherited_tokens_and_host_overrides_cannot_outrank_the_machine_profile() {
    let f = Fixture::new();
    let mut command = f.command();
    for name in [
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GH_ENTERPRISE_TOKEN",
        "GITHUB_ENTERPRISE_TOKEN",
        "GH_HOST",
        "GH_REPO",
        "GH_DEBUG",
    ] {
        command.env(name, "fictional-override");
    }
    let output = command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(f.health()["ok"], true);
    assert_eq!(f.read("environment"), "unset\n".repeat(7));
    assert_eq!(
        f.read("config-dir"),
        f.path("machine-profile").to_str().unwrap()
    );
}
#[test]
fn owner_identity_refuses_discovery_and_reports_the_machine_profile_fix() {
    let f = Fixture::new();
    f.response("machine", 200, "{\"id\":202,\"login\":\"different-name\"}");
    f.run();
    assert_eq!(f.health()["ok"], false);
    assert!(
        f.detail()
            .contains("start brg with GH_CONFIG_DIR naming the machine account's login")
    );
    assert!(!f.calls().contains("repos/"));
}
#[test]
fn unset_invalid_or_unknown_owner_names_github_owner_without_discovery() {
    for value in [None, Some("bad/login"), Some("example-missing")] {
        let f = Fixture::new();
        f.response("owner", 404, "{\"message\":\"private text\"}");
        let mut command = f.command();
        Fixture::set(&mut command, "GITHUB_OWNER", value);
        command.output().unwrap();
        assert_eq!(f.health()["ok"], false);
        let detail = f.detail();
        assert!(detail.contains("GITHUB_OWNER"), "{detail}");
        assert!(!detail.contains("private text"));
        assert!(!f.calls().contains("repos/"));
        if value != Some("example-missing") {
            assert!(f.calls().is_empty());
        }
    }
}
#[test]
fn damaged_or_unreadable_intake_refuses_and_leaves_the_journal_untouched() {
    for text in ["{", "null", "{}"] {
        let f = Fixture::new();
        f.write("state/intake.json", text);
        let output = f.run();
        assert!(!output.status.success());
        assert_eq!(f.read("state/intake.json"), text);
        assert!(f.calls().is_empty());
        assert_eq!(f.health()["ok"], false);
        let detail = f.detail();
        assert!(
            detail.contains("intake.json") && detail.contains("move it aside"),
            "{detail}"
        );
    }
    let f = Fixture::new();
    fs::create_dir(f.path("state/intake.json")).unwrap();
    assert!(!f.run().status.success());
    assert!(f.path("state/intake.json").is_dir());
    assert!(f.calls().is_empty());
}
#[test]
fn start_and_restart_skip_old_activity_keep_admitted_events_and_say_so() {
    let f = Fixture::new();
    let old = json!({"secs_since_epoch": 1_500_000_000, "nanos_since_epoch": 0});
    let cursor = json!({"at": old, "drained": old, "floor": old, "seen": {}});
    let secs = std::time::UNIX_EPOCH.elapsed().unwrap().as_secs();
    let recent = json!({"secs_since_epoch": secs, "nanos_since_epoch": 0});
    f.write(
        "state/intake.json",
        &json!({"cursors": {"example-org/widgets": cursor}, "seen": {"IC_kept": recent}, "pending": []})
            .to_string(),
    );
    f.response(
        "issues",
        200,
        &json!([{"node_id":"I_old","number":1,"updated_at":"2020-01-01T00:00:00Z"}]).to_string(),
    );
    let output = f.run();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(f.health()["ok"], true);
    let detail = f.detail();
    assert!(
        detail.contains("0 changed item(s) processed;") && detail.contains("is not delivered"),
        "{detail}"
    );
    let state = f.json("state/intake.json");
    assert!(state["seen"]["IC_kept"].is_object(), "{state}");
    assert_ne!(state["cursors"]["example-org/widgets"]["floor"], old);
    assert_eq!(
        fs::metadata(f.path("state/intake.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}
#[test]
fn graphql_passes_values_literally_without_gh_substitutions() {
    // Run the library call in an isolated child, keeping test environment changes
    // out of the multithreaded parent and preserving the real member's environment.
    if std::env::var_os("FIXTURE_GRAPHQL_CHILD").is_some() {
        let result = Gh
            .graphql(
                "query($name: String!, $number: Int!) { viewer { databaseId } }",
                &[("name", "@credentials {owner}; touch injected")],
                &[("number", 7)],
            )
            .unwrap();
        assert_eq!(result["data"]["viewer"]["databaseId"], 101);
        return;
    }
    let f = Fixture::new();
    let mut command = Command::new(std::env::current_exe().unwrap());
    f.environment(&mut command);
    let output = command
        .env("FIXTURE_GRAPHQL_CHILD", "1")
        .args([
            "--exact",
            "graphql_passes_values_literally_without_gh_substitutions",
            "--nocapture",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        f.calls()
            .contains("\n-f\nname=@credentials {owner}; touch injected\n-F\nnumber=7\n--end--"),
        "{}",
        f.calls()
    );
    assert!(!f.path("injected").exists());
}
