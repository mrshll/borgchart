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
                env!("CARGO_BIN_EXE_fake-usage"),
                f.0.path().join("bin").join(vendor),
            )
            .unwrap();
        }
        fs::write(f.0.path().join("config/config.yaml"), "schema: 1\n").unwrap();
        f
    }
    fn run(&self, args: &[&str]) -> Output {
        self.run_with_cache(args, None)
    }
    fn run_with_cache(&self, args: &[&str], cache: Option<&std::path::Path>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_brg-seat"));
        command
            .env_clear()
            .env("HOME", self.0.path().join("home"))
            .env("PATH", self.0.path().join("bin"))
            .env("XDG_CACHE_HOME", self.0.path().join("cache"))
            .env("BORGCHART_CONFIG", self.0.path().join("config/config.yaml"))
            .env("BORGCHART_MEMBER", "must-not-leak")
            .env("ANTHROPIC_API_KEY", "DO_NOT_LEAK_FAKE_KEY")
            .env("OPENAI_BASE_URL", "https://invalid.example")
            .env("HERDR_TEST", "must-not-leak")
            .env("BRG_SEAT_TEST", "must-not-leak")
            .args(args);
        if let Some(cache) = cache {
            command.env("XDG_CACHE_HOME", cache);
        }
        command.output().unwrap()
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
        let out = f.run(&["route", vendor, model]);
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
        assert!(env.keys().all(|k| !k.starts_with("BORGCHART_")
            && !k.starts_with("HERDR_")
            && !k.starts_with("BRG_SEAT_")));
        assert!(!env.contains_key("ANTHROPIC_API_KEY"));
        assert!(!env.contains_key("OPENAI_BASE_URL"));
        assert_eq!(f.run(&["route", vendor, model]).status.code(), Some(0));
        assert_eq!(f.events(vendor).len(), events.len());
        f.control(vendor, json!({"mode":"malformed"}));
        let failed = f.run(&["route", vendor, model, "--refresh"]);
        assert_eq!(failed.status.code(), Some(3));
        assert!(failed.stdout.starts_with(b"wait:"));
        assert!(!String::from_utf8_lossy(&failed.stdout).contains("DO_NOT_LOG"));
        assert!(!String::from_utf8_lossy(&failed.stderr).contains("DO_NOT_LOG"));
        let calls = f.events(vendor).len();
        f.control(vendor, json!({}));
        assert_eq!(
            f.run(&["route", vendor, model, "--refresh"]).status.code(),
            Some(3)
        );
        assert_eq!(f.events(vendor).len(), calls);
    }
    let f = Fixture::new();
    for args in [
        vec![],
        vec!["route", "codex"],
        vec!["route", "unknown", "astra"],
        vec!["route", "codex", "bad model"],
        vec!["route", "codex", "astra", "--json"],
    ] {
        assert_eq!(f.run(&args).status.code(), Some(2));
    }
    assert_eq!(f.run(&["--help"]).status.code(), Some(0));
    assert_eq!(
        f.run(&["route", "codex", "unknown-model"]).status.code(),
        Some(3)
    );
}
#[test]
fn plugin_does_not_read_core_configuration() {
    let f = Fixture::new();
    fs::write(
        f.0.path().join("config/config.yaml"),
        "not a brg configuration",
    )
    .unwrap();
    assert_eq!(f.run(&["route", "codex", "astra"]).status.code(), Some(0));
}
#[test]
fn cache_respects_the_absolute_xdg_location() {
    let f = Fixture::new();
    let cache = f.0.path().join("private-cache");
    assert_eq!(
        f.run_with_cache(&["route", "claude", "sonnet"], Some(&cache))
            .status
            .code(),
        Some(0)
    );
    assert!(
        fs::read_dir(cache.join("borgchart/seat-v2"))
            .unwrap()
            .next()
            .is_some()
    );
}

#[test]
fn credential_change_during_probe_waits_then_next_identity_can_probe() {
    for (vendor, model) in [("codex", "astra"), ("claude", "opus")] {
        let f = Fixture::new();
        assert_eq!(f.run(&["route", vendor, model]).status.code(), Some(0));
        f.control(vendor, json!({"mode":"rotate"}));
        let changed = f.run(&["route", vendor, model, "--refresh"]);
        assert_eq!(changed.status.code(), Some(3));
        assert!(!String::from_utf8_lossy(&changed.stdout).contains("DO_NOT_READ"));
        f.control(vendor, json!({}));
        assert_eq!(f.run(&["route", vendor, model]).status.code(), Some(0));
        assert_eq!(
            f.events(vendor)
                .iter()
                .filter(|e| e["event"] == "start")
                .count(),
            3
        );
    }
}

#[test]
fn invalid_cache_locations_refuse_without_a_probe() {
    let f = Fixture::new();
    let parent = f.0.path().join("nested/../cache");
    for configured in [
        std::path::Path::new("relative-cache"),
        std::path::Path::new(""),
        &parent,
    ] {
        let out = f.run_with_cache(&["route", "codex", "astra"], Some(configured));
        assert_eq!(out.status.code(), Some(3));
        assert!(String::from_utf8_lossy(&out.stdout).contains("invalid cache location"));
        assert!(
            fs::read_dir(f.0.path().join("home/fake-usage"))
                .unwrap()
                .next()
                .is_none()
        );
    }
}
