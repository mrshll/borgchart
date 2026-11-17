//! Process-boundary tests of the gh profile guard, CLI and strict journal.
mod support;
use brg_github::gh::Gh;
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt, process::Command};
use support::Fixture;

#[test]
fn missing_or_empty_gh_config_refuses_before_spawning_gh() {
    for value in [None, Some("")] {
        let f = Fixture::new();
        let mut command = f.command();
        match value {
            None => {
                command.env_remove("GH_CONFIG_DIR");
            }
            Some(value) => {
                command.env("GH_CONFIG_DIR", value);
            }
        }
        command.output().unwrap();
        let health = f.health();
        assert_eq!(health["ok"], false);
        assert!(
            health["detail"]
                .as_str()
                .unwrap()
                .contains("set GH_CONFIG_DIR")
        );
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
    assert_eq!(
        fs::read_to_string(f.path("environment")).unwrap(),
        "unset\n".repeat(7)
    );
    assert_eq!(
        fs::read_to_string(f.path("config-dir")).unwrap(),
        f.path("machine-profile").to_str().unwrap()
    );
    assert!(!f.calls().contains("auth"));
    assert!(f.calls().contains("--include\n--method\nGET\nuser\n"));
}
#[test]
fn owner_identity_refuses_discovery_and_reports_the_machine_profile_fix() {
    let f = Fixture::new();
    f.response("machine", 200, "{\"id\":202,\"login\":\"different-name\"}");
    f.run();
    assert_eq!(f.health()["ok"], false);
    assert!(
        f.health()["detail"]
            .as_str()
            .unwrap()
            .contains("start brg with GH_CONFIG_DIR naming the machine account's login")
    );
    assert!(!f.calls().contains("repos/"));
    assert!(!f.path("state/intake.json").exists());
}
#[test]
fn unset_invalid_or_unknown_owner_names_github_owner_without_discovery() {
    for value in [None, Some("bad/login"), Some("example-missing")] {
        let f = Fixture::new();
        f.response("owner", 404, "{\"message\":\"private text\"}");
        let mut command = f.command();
        match value {
            None => {
                command.env_remove("GITHUB_OWNER");
            }
            Some(value) => {
                command.env("GITHUB_OWNER", value);
            }
        }
        command.output().unwrap();
        assert_eq!(f.health()["ok"], false);
        let health = f.health();
        let detail = health["detail"].as_str().unwrap();
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
    for text in ["{", "null", "[]", "{}", "{\"epoch\":\"yesterday\"}"] {
        let f = Fixture::new();
        f.write("state/intake.json", text);
        let output = f.run();
        assert!(!output.status.success());
        assert_eq!(
            fs::read_to_string(f.path("state/intake.json")).unwrap(),
            text
        );
        assert!(f.calls().is_empty());
        assert_eq!(f.health()["ok"], false);
        let health = f.health();
        let detail = health["detail"].as_str().unwrap();
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
    let f = Fixture::new();
    std::os::unix::fs::symlink("missing-journal", f.path("state/intake.json")).unwrap();
    assert!(!f.run().status.success());
    assert!(
        fs::symlink_metadata(f.path("state/intake.json"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(f.calls().is_empty());
}
#[test]
fn first_start_saves_private_state_and_reports_the_no_replay_epoch() {
    let f = Fixture::new();
    fs::remove_dir(f.path("state")).unwrap();
    f.response(
        "issues",
        200,
        &json!([{"node_id":"I_old","number":1,"updated_at":"2020-01-01T00:00:00Z"}]).to_string(),
    );
    let output = f.run();
    assert!(output.status.success(), "{output:?}");
    let health = f.health();
    assert_eq!(health["ok"], true);
    assert!(
        health["detail"]
            .as_str()
            .unwrap()
            .contains("0 changed item(s) processed; activity before")
    );
    assert!(
        health["detail"]
            .as_str()
            .unwrap()
            .contains("is not delivered")
    );
    let state = f.json("state/intake.json");
    assert_eq!(state["machine_id"], 101);
    assert_eq!(state["owner_id"], 202);
    assert_eq!(state["pending"], json!([]));
    assert_eq!(
        state["cursors"]["example-org/widgets"]["floor"],
        state["epoch"]
    );
    assert_eq!(
        fs::metadata(f.path("state/intake.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(f.path("state")).unwrap().permissions().mode() & 0o777,
        0o700
    );
}
#[test]
fn rest_rate_limit_health_names_the_reset_without_response_bodies() {
    let f = Fixture::new();
    f.write("issues", "HTTP/2.0 403 Forbidden\r\nX-RateLimit-Remaining: 0\r\nX-RateLimit-Reset: 4102444800\r\n\r\n{\"message\":\"private response\"}");
    f.write("issues-code", "1");
    f.run();
    let health = f.health();
    let detail = health["detail"].as_str().unwrap();
    assert_eq!(health["ok"], false);
    assert!(
        detail.contains("rate limited until 2100-01-01T00:00:00.000Z"),
        "{detail}"
    );
    assert!(!detail.contains("private response"));
}
#[test]
fn graphql_uses_literal_argument_vectors_and_parses_json() {
    // Run the library call in an isolated child, keeping test environment changes
    // out of the multithreaded parent and preserving the real member's environment.
    if std::env::var_os("FIXTURE_GRAPHQL_CHILD").is_some() {
        for value in ["@credentials", "@-", "{owner}", "{repo}", "{branch}"] {
            assert!(
                Gh.graphql(
                    "query($name: String!) { viewer { databaseId } }",
                    &[("name", value)]
                )
                .is_err()
            );
        }
        let result = Gh
            .graphql(
                "query($name: String!) { viewer { databaseId } } # $(touch injected)",
                &[("name", "example-owner; touch injected")],
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
            "graphql_uses_literal_argument_vectors_and_parses_json",
            "--nocapture",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(f.calls().contains("graphql\n-f\nquery=query($name: String!) { viewer { databaseId } } # $(touch injected)\n-F\nname=example-owner; touch injected\n"));
    assert!(!f.path("injected").exists());
    assert_eq!(f.calls().matches("--end--").count(), 1);
}
