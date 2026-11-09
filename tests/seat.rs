#![cfg(feature = "test-support")]
//! Full CLI with owned homes and fake vendor binaries; no real account access.
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::symlink;
use std::process::{Command, Output};
struct Fixture(tempfile::TempDir);
impl Fixture {
    fn new() -> Self {
        let f = Self(tempfile::tempdir().unwrap());
        for dir in [
            "bin",
            "home/fake-usage",
            "home/.codex",
            "home/.claude",
            "cache",
            "config",
        ] {
            fs::create_dir_all(f.0.path().join(dir)).unwrap();
        }
        for vendor in ["claude", "codex"] {
            symlink(
                env!("CARGO_BIN_EXE_fake-agent"),
                f.0.path().join("bin").join(vendor),
            )
            .unwrap();
        }
        fs::write(f.0.path().join("config/config.yaml"), "schema: 1\n").unwrap();
        f
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_brg"))
            .env_clear()
            .env("HOME", self.0.path().join("home"))
            .env("PATH", self.0.path().join("bin"))
            .env("XDG_CACHE_HOME", self.0.path().join("cache"))
            .env("BORGCHART_CONFIG", self.0.path().join("config/config.yaml"))
            .env("BORGCHART_MEMBER", "must-not-leak")
            .env("ANTHROPIC_API_KEY", "DO_NOT_LEAK_FAKE_KEY")
            .env("OPENAI_BASE_URL", "https://invalid.example")
            .env("HERDR_TEST", "must-not-leak")
            .args(args)
            .output()
            .unwrap()
    }
    fn control(&self, vendor: &str, data: Value) {
        fs::write(
            self.0.path().join(format!("home/fake-usage/{vendor}.json")),
            serde_json::to_vec(&data).unwrap(),
        )
        .unwrap();
    }
    fn events(&self, vendor: &str) -> Vec<Value> {
        fs::read_to_string(
            self.0
                .path()
                .join(format!("home/fake-usage/{vendor}.jsonl")),
        )
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
    }
}
#[test]
fn route_protocol_cache_environment_and_cli_exit_contract() {
    for (vendor, model) in [("codex", "gpt-6-astra"), ("claude", "opus")] {
        let f = Fixture::new();
        let out = f.run(&["seat", "route", vendor, model]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{} {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.stdout.starts_with(b"go:"));
        let events = f.events(vendor);
        let env = events[0]["env"].as_object().unwrap();
        assert!(
            env.keys()
                .all(|k| !k.starts_with("BORGCHART_") && !k.starts_with("HERDR_"))
        );
        assert!(!env.contains_key("ANTHROPIC_API_KEY"));
        assert!(!env.contains_key("OPENAI_BASE_URL"));
        assert_eq!(
            f.run(&["seat", "route", vendor, model]).status.code(),
            Some(0)
        );
        assert_eq!(f.events(vendor).len(), events.len());
        f.control(vendor, json!({"mode":"malformed"}));
        let failed = f.run(&["seat", "route", vendor, model, "--refresh"]);
        assert_eq!(failed.status.code(), Some(3));
        assert!(failed.stdout.starts_with(b"wait:"));
        assert!(!String::from_utf8_lossy(&failed.stdout).contains("DO_NOT_LOG"));
        assert!(!String::from_utf8_lossy(&failed.stderr).contains("DO_NOT_LOG"));
        let calls = f.events(vendor).len();
        f.control(vendor, json!({}));
        assert_eq!(
            f.run(&["seat", "route", vendor, model, "--refresh"])
                .status
                .code(),
            Some(3)
        );
        assert_eq!(f.events(vendor).len(), calls);
    }
    let f = Fixture::new();
    for args in [
        vec!["seat"],
        vec!["seat", "route", "codex"],
        vec!["seat", "route", "unknown", "astra"],
        vec!["seat", "route", "codex", "bad model"],
        vec!["seat", "route", "codex", "astra", "--json"],
    ] {
        assert_eq!(f.run(&args).status.code(), Some(2));
    }
    assert_eq!(f.run(&["seat", "--help"]).status.code(), Some(0));
    assert_eq!(
        f.run(&["seat", "route", "codex", "unknown-model"])
            .status
            .code(),
        Some(3)
    );
}
#[test]
fn api_auth_and_bad_configuration_refuse_without_vendor_launch() {
    for config in [
        "schema: 1\nauth: api_key\n",
        "schema: 1\nseat: {reader: DO_NOT_LOG_CONFIG}\n",
    ] {
        let f = Fixture::new();
        fs::write(f.0.path().join("config/config.yaml"), config).unwrap();
        let out = f.run(&["seat", "route", "codex", "astra"]);
        assert_eq!(out.status.code(), Some(3));
        assert!(!String::from_utf8_lossy(&out.stdout).contains("DO_NOT_LOG"));
        assert!(
            fs::read_dir(f.0.path().join("home/fake-usage"))
                .unwrap()
                .next()
                .is_none()
        );
    }
}
#[test]
fn configured_cache_path_is_relative_to_brgs_config_file() {
    let f = Fixture::new();
    fs::write(
        f.0.path().join("config/config.yaml"),
        "schema: 1\nseat: {cache_dir: private-cache}\n",
    )
    .unwrap();
    assert_eq!(
        f.run(&["seat", "route", "claude", "sonnet"]).status.code(),
        Some(0)
    );
    assert!(
        fs::read_dir(f.0.path().join("config/private-cache"))
            .unwrap()
            .next()
            .is_some()
    );
}

#[test]
fn credential_change_during_probe_waits_then_next_identity_can_probe() {
    for (vendor, model) in [("codex", "astra"), ("claude", "opus")] {
        let f = Fixture::new();
        assert_eq!(
            f.run(&["seat", "route", vendor, model]).status.code(),
            Some(0)
        );
        f.control(vendor, json!({"mode":"rotate"}));
        let changed = f.run(&["seat", "route", vendor, model, "--refresh"]);
        assert_eq!(changed.status.code(), Some(3));
        assert!(!String::from_utf8_lossy(&changed.stdout).contains("DO_NOT_READ"));
        f.control(vendor, json!({}));
        assert_eq!(
            f.run(&["seat", "route", vendor, model]).status.code(),
            Some(0)
        );
        assert_eq!(
            f.events(vendor)
                .iter()
                .filter(|e| e["event"] == "start")
                .count(),
            3
        );
    }
}
