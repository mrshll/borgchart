#[path = "../src/github/model.rs"]
#[allow(dead_code)]
mod model;
mod bus {
    pub use borgchart::bus::*;
}
mod state {
    pub use borgchart::state::*;
}
use model::*;
use serde_json::json;

fn config(dir: &std::path::Path) -> Config {
    Config::parse(json!({"gh":["/example/gh"],"agent":{"id":101,"login":"example-agent"},"trusted_actor_ids":[102],"owner_policy":{"owner":{"id":103,"login":"example-owner","type":"User"},"repository_policy":"include_new"}}),dir.join("config.json"),dir.to_path_buf()).unwrap()
}
#[test]
fn explicit_init_and_scope_are_fail_closed() {
    let d = tempfile::tempdir().unwrap();
    let cfg = config(d.path());
    assert!(load(&cfg).is_err());
    initialize(&cfg, "2026-01-01T00:00:00Z").unwrap();
    assert_eq!(load(&cfg).unwrap().feature_start, "2026-01-01T00:00:00Z");
    assert!(initialize(&cfg, "2026-01-01T00:00:00Z").is_err());
    let mut other = cfg.clone();
    other.settings.trusted_actor_ids = vec![999];
    assert_eq!(load(&other).unwrap_err(), "scope_changed");
}
#[test]
fn private_atomic_state_and_nonblocking_lock() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let cfg = config(d.path());
    initialize(&cfg, "2026-01-01T00:00:00Z").unwrap();
    let lock = lock(&cfg).unwrap();
    assert!(model::lock(&cfg).is_err());
    drop(lock);
    assert_eq!(
        std::fs::metadata(cfg.file()).unwrap().permissions().mode() & 0o077,
        0
    );
    let mut s = load(&cfg).unwrap();
    s.feed.due_ms = 123;
    save(&cfg, &s).unwrap();
    assert_eq!(load(&cfg).unwrap().feed.due_ms, 123);
    std::fs::write(cfg.file(), "{}").unwrap();
    assert!(load(&cfg).is_err());
}
#[test]
fn scope_ignores_runtime_limits_and_moving_feed() {
    let d = tempfile::tempdir().unwrap();
    let mut c = config(d.path());
    let before = c.scope();
    c.settings.poll_seconds = 120;
    c.settings.reactions = true;
    c.settings.gh = vec!["/other/gh".into()];
    c.settings.owner_policy.exclude_repository_ids = vec![104];
    assert_eq!(before, c.scope());
}
#[test]
fn invalid_config_and_class_overlap_are_rejected() {
    let d = tempfile::tempdir().unwrap();
    let c = config(d.path());
    let mut v = c.value();
    v["poll_seconds"] = json!(0);
    assert!(Config::parse(v, d.path().join("c"), d.path().into()).is_err());
    let mut v = c.value();
    v["trusted_reviewer_ids"] = json!([102]);
    assert!(Config::parse(v, d.path().join("c"), d.path().into()).is_err());
    let mut v = c.value();
    v["unknown"] = json!(true);
    assert!(Config::parse(v, d.path().join("c"), d.path().into()).is_err());
}
