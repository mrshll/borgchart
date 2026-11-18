//! Process-boundary tests of the gh profile guard, CLI and strict journal.
mod support;
use brg_github::gh::{Api, Gh};
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt, process::Command};
use support::Fixture;

#[test]
fn relative_profile_follows_configuration_folder_and_template_scope_takes_precedence() {
    for template in [None, Some(""), Some("example-org/gadgets")] {
        let f = Fixture::new();
        let private = f.path("personal/config.yaml");
        fs::create_dir(f.path("personal")).unwrap();
        fs::write(
            &private,
            f.read("config.yaml")
                .replace("~/machine-profile", "machine-profile"),
        )
        .unwrap();
        if let Some(text) = template {
            f.repositories("template.yaml", text);
        }
        f.command()
            .env("BORGCHART_CONFIG", &private)
            .output()
            .unwrap();
        let overridden = template.is_some_and(|t| !t.is_empty());
        assert_eq!(f.calls().contains("repos/example-org/gadgets/"), overridden);
        assert_eq!(
            f.calls().contains("repos/example-org/widgets/"),
            !overridden
        );
        assert_eq!(f.detail().contains("is ignored"), overridden);
        assert_eq!(
            f.read("config-dir"),
            f.path("personal/machine-profile").to_str().unwrap()
        );
    }
}
#[test]
fn unlisted_repositories_skip_even_the_guard_and_missing_private_config_stays_off() {
    let f = Fixture::new();
    f.write(
        "config.yaml",
        "schema: 1\ngithub:\n  owner: example-owner\n  gh_config_dir: machine-profile\n",
    );
    f.run();
    assert_eq!(f.health()["ok"], true);
    assert!(f.detail().contains("no repositories listed"));
    assert!(f.calls().is_empty());
    f.repositories("template.yaml", "example-org/gadgets");
    f.run();
    assert!(f.calls().contains("repos/example-org/gadgets/"));
    assert!(!f.detail().contains("is ignored"));
    fs::remove_file(f.path("calls")).unwrap();
    fs::remove_file(f.path("config.yaml")).unwrap();
    f.run();
    assert!(f.detail().starts_with("off: no"));
    assert!(f.calls().is_empty());
}
#[test]
fn another_plugins_repository_list_cannot_override_the_private_list() {
    let f = Fixture::new();
    f.response("issues-gadgets", 200, "[]");
    f.repositories("template.yaml", "example-org/gadgets");
    f.write(
        "template.yaml",
        &f.read("template.yaml").replace("state:", "other:"),
    );
    f.run();
    assert_eq!(f.health()["ok"], true);
    assert!(f.calls().contains("repos/example-org/widgets/"));
    assert!(!f.calls().contains("repos/example-org/gadgets/"));
}
#[test]
fn absent_or_empty_template_scope_uses_only_the_private_list() {
    for template in [
        None,
        Some("roles: {}"),
        Some("plugins: {state: {}}"),
        Some("plugins: {state: {env: {}}}"),
        Some("plugins: {state: {env: {GITHUB_REPOSITORIES: '  '}}}"),
    ] {
        for listed in [true, false] {
            let f = Fixture::new();
            if !listed {
                f.write(
                    "config.yaml",
                    &f.read("config.yaml").replace("[example-org/widgets]", "[]"),
                );
            }
            if let Some(text) = template {
                f.write("template.yaml", text);
            }
            f.run();
            assert_eq!(f.health()["ok"], true);
            assert_eq!(f.calls().contains("repos/example-org/widgets/"), listed);
            if !listed {
                assert!(f.detail().contains("no repositories listed"));
                assert!(f.calls().is_empty());
            }
        }
    }
}
#[test]
fn invalid_or_unreadable_template_scope_stops_discovery_instead_of_using_the_private_list() {
    for value in [
        "[",
        "42",
        "true",
        "null",
        "[example-org/widgets]",
        "example-org/bad?query",
    ] {
        let f = Fixture::new();
        f.write(
            "template.yaml",
            &format!("plugins: {{state: {{env: {{GITHUB_REPOSITORIES: {value}}}}}}}"),
        );
        f.run();
        assert_eq!(f.health()["ok"], false, "{value}");
        assert!(f.detail().contains("template.yaml"));
        assert!(f.detail().contains("GITHUB_REPOSITORIES"));
        assert!(f.calls().is_empty());
    }
    let f = Fixture::new();
    fs::create_dir(f.path("template.yaml")).unwrap();
    f.run();
    assert_eq!(f.health()["ok"], false);
    assert!(f.detail().contains("template.yaml"));
    assert!(f.calls().is_empty());
}

#[test]
fn missing_or_empty_gh_config_refuses_before_spawning_gh() {
    for field in ["", "gh_config_dir: \"\"\n  "] {
        let f = Fixture::new();
        let yaml =
            format!("schema: 1\ngithub:\n  owner: example-owner\n  {field}repositories: []\n");
        f.write("config.yaml", &yaml);
        f.run();
        assert_eq!(f.health()["ok"], false);
        assert!(f.detail().contains("gh_config_dir"));
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
    assert!(f.detail().contains(
        "set gh_config_dir in config.yaml's github section to the machine account's login"
    ));
    assert!(!f.calls().contains("repos/"));
}
#[test]
fn unset_invalid_or_unknown_owner_refuses_discovery() {
    for field in ["", "owner: bad/login\n  ", "owner: example-missing\n  "] {
        let f = Fixture::new();
        f.response("owner", 404, "{\"message\":\"private text\"}");
        let yaml = format!(
            "schema: 1\ngithub:\n  {field}gh_config_dir: machine-profile\n  repositories: [example-org/widgets]\n"
        );
        f.write("config.yaml", &yaml);
        f.run();
        assert_eq!(f.health()["ok"], false);
        let detail = f.detail();
        assert!(detail.contains("owner"), "{detail}");
        assert!(!detail.contains("private text"));
        assert!(!f.calls().contains("repos/"));
        if !field.contains("example-missing") {
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
fn a_new_entry_skips_history_while_admitted_events_and_private_journal_survive() {
    let f = Fixture::new();
    let old = json!({"secs_since_epoch": 1_500_000_000, "nanos_since_epoch": 0});
    let cursor = json!({"at": old, "drained": old, "floor": old, "seen": {}});
    let secs = std::time::UNIX_EPOCH.elapsed().unwrap().as_secs();
    let recent = json!({"secs_since_epoch": secs, "nanos_since_epoch": 0});
    f.write(
        "state/intake.json",
        &json!({"cursors": {"example-org/removed": cursor}, "seen": {"IC_kept": recent}, "pending": [{"id":"IC_pending","repository":"example-org/widgets","number":1,"is_pull_request":false,"kind":"Mention","recipient":"","reason":"routing unavailable"}]})
            .to_string(),
    );
    f.response(
        "issues",
        200,
        &json!([{"node_id":"I_old","number":1,"updated_at":"2020-01-01T00:00:00Z"}]).to_string(),
    );
    let output = f.run();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(f.health()["ok"], false);
    let detail = f.detail();
    assert!(
        detail.contains("0 changed item(s) processed;") && detail.contains("is not delivered"),
        "{detail}"
    );
    let state = f.json("state/intake.json");
    assert!(state["seen"]["IC_kept"].is_object(), "{state}");
    assert_eq!(state["pending"][0]["id"], "IC_pending");
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
        let result = Gh("machine-profile".into())
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
