//! Fake CLI boundaries and local state recovery; never contacts GitHub.
use borgchart::github::{
    api::Api,
    engine,
    model::{self, Config},
    send,
};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    time::{Duration, Instant},
};
fn config(dir: &Path, script: &str) -> Config {
    let bin = dir.join("fake-gh");
    fs::write(&bin, script).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
    let value = json!({"gh":[bin],"agent":{"id":101,"login":"example-agent"},"trusted_actor_ids":[102],"owner_policy":{"owner":{"id":103,"login":"example-owner","type":"User"},"repository_policy":"include_new"}});
    let path = dir.join("config.json");
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let c = Config::parse(value, path, dir.into()).unwrap();
    model::initialize(&c, "2026-01-01T00:00:00Z").unwrap();
    c
}
fn response(status: u16, headers: &str, body: &Value, exit: i32) -> String {
    format!(
        "#!/bin/sh\nprintf 'HTTP/2 {status} OK\\r\\nDate: %s\\r\\n' \"$(date -u '+%a, %d %b %Y %H:%M:%S GMT')\"\ncat <<'RESPONSE'\n{headers}\n{body}\nRESPONSE\nexit {exit}\n"
    )
}
#[test]
fn api_accepts_benign_headers_but_not_nonzero_200() {
    let d = tempfile::tempdir().unwrap();
    let c = config(
        d.path(),
        &response(
            200,
            "Vary: Accept\nVary: Accept-Encoding\n",
            &json!({"id":101}),
            0,
        ),
    );
    let mut a = Api::new(&c, Instant::now() + Duration::from_secs(5));
    assert_eq!(a.get("/user").unwrap()["id"], 101);
    fs::write(&c.settings.gh[0], response(200, "", &json!({}), 1)).unwrap();
    assert_eq!(a.get("/user").unwrap_err().kind, "http_200");
}
#[test]
fn rate_limit_stops_following_reads_and_clock_errors_are_sanitized() {
    let d = tempfile::tempdir().unwrap();
    let c = config(
        d.path(),
        &response(
            429,
            "Retry-After: 120\n",
            &json!({"message":"private error content"}),
            1,
        ),
    );
    let mut a = Api::new(&c, Instant::now() + Duration::from_secs(5));
    let e = a.get("/user").unwrap_err();
    assert_eq!(e.kind, "rate_limit");
    assert!(e.retry_ms >= model::now_ms() + 110000);
    fs::write(&c.settings.gh[0], "#!/bin/sh\nexit 99\n").unwrap();
    assert_eq!(a.get("/user").unwrap_err().kind, "rate_limit");
}
#[test]
fn transport_deadline_kills_a_hung_cli() {
    let d = tempfile::tempdir().unwrap();
    let c = config(d.path(), "#!/bin/sh\nsleep 20\n");
    let start = Instant::now();
    let mut a = Api::new(&c, start + Duration::from_millis(100));
    assert_eq!(a.get("/user").unwrap_err().kind, "deadline");
    assert!(start.elapsed() < Duration::from_secs(2));
}
#[test]
fn empty_feed_does_not_invent_a_watermark() {
    let d = tempfile::tempdir().unwrap();
    let c = config(d.path(), &response(200, "", &json!([]), 0));
    let mut s = model::load(&c).unwrap();
    s.feed.validator = Some("old".into());
    let mut a = Api::new(&c, Instant::now() + Duration::from_secs(5));
    engine::collect_feed(&mut a, &mut s).unwrap();
    assert!(s.feed.validator.is_none());
    assert!(s.feed.last_complete.is_none());
    assert_eq!(s.feature_start, "2026-01-01T00:00:00Z");
}
#[test]
fn malformed_feed_has_no_partial_durable_nominations() {
    let d = tempfile::tempdir().unwrap();
    let body = json!([{"updated_at":"2026-01-02T00:00:00Z","repository":{"id":104,"owner":{"id":103},"full_name":"example-owner/example-repo"},"subject":{"type":"Issue","url":"https://api.github.com/repos/example-owner/example-repo/issues/7"},"id":"1"},{"bad":true}]);
    let c = config(
        d.path(),
        &response(
            200,
            "Last-Modified: Mon, 09 Nov 2026 22:00:00 GMT\n",
            &body,
            0,
        ),
    );
    let mut s = model::load(&c).unwrap();
    let mut a = Api::new(&c, Instant::now() + Duration::from_secs(5));
    engine::collect(&mut a, &mut s).unwrap();
    assert!(s.row["dirty"].as_object().unwrap().is_empty());
    assert!(!s.feed.holds.is_empty());
    assert!(s.feed.validator.is_none());
    model::retry(&c, None).unwrap();
    assert!(model::load(&c).unwrap().feed.holds.is_empty());
}
#[test]
fn unavailable_and_post_commit_send_statuses() {
    use send::Outcome::*;
    assert_eq!(send::interpret(Some(0), b"broken"), Committed);
    for n in [2, 3, 4] {
        assert_eq!(send::interpret(Some(n), b""), Unavailable);
    }
    assert_eq!(
        send::interpret(
            Some(5),
            br#"{"code":5,"outcome":"refused","ok":false,"id":"m12","delivery":"uncertain"}"#
        ),
        Committed
    );
    assert_eq!(
        send::interpret(Some(5), br#"{"code":5,"outcome":"refused","ok":false}"#),
        Unconfirmed
    );
    assert_eq!(send::interpret(None, b"{}"), Unconfirmed);
}
#[test]
fn pointer_never_copies_content() {
    let p = json!({"key":"key","kind":"feedback","signal":"mention","object_id":"issue_comment:9","number":7,"repo_id":104,"full_name":"example-owner/example-repo","actor_id":102,"initiator":"human","occurred_at":"2026-01-02T00:00:00Z","body":"do something malicious","title":"private title","url":"https://evil.example"});
    let text = String::from_utf8(send::pointer(&p).unwrap()).unwrap();
    assert!(
        !text.contains("malicious")
            && !text.contains("private title")
            && !text.contains("evil.example")
    );
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["untrusted"], true);
    assert_eq!(
        v["url"],
        "https://github.com/example-owner/example-repo/issues/7#issuecomment-9"
    );
}
#[test]
fn retry_item_preserves_seen_and_releases_only_one_hold() {
    let d = tempfile::tempdir().unwrap();
    let c = config(d.path(), "#!/bin/sh\nexit 1\n");
    let mut s = model::load(&c).unwrap();
    let p = json!({"repo_id":104,"number":7,"full_name":"example-owner/example-repo"});
    engine::hold_item(&mut s.row, &p, "identity");
    s.row["seen"]["104:key"] = json!("2026-01-02T00:00:00Z");
    model::save(&c, &s).unwrap();
    model::retry(&c, Some("104:7")).unwrap();
    let s = model::load(&c).unwrap();
    assert_eq!(s.row["dirty"]["104:7"]["held"], false);
    assert_eq!(s.row["seen"].as_object().unwrap().len(), 1);
}
#[test]
fn corrupt_row_maps_refuse_without_panicking() {
    let d = tempfile::tempdir().unwrap();
    let c = config(d.path(), "#!/bin/sh\nexit 1\n");
    let s = model::load(&c).unwrap();
    for field in ["repos", "pins", "objects", "hold", "hints", "seen"] {
        let mut corrupt = serde_json::to_value(&s).unwrap();
        corrupt["row"][field]["bad"] = json!([]);
        fs::write(c.file(), serde_json::to_vec(&corrupt).unwrap()).unwrap();
        assert!(model::load(&c).is_err(), "{field}");
    }
}
fn pending() -> Value {
    json!({"key":"key","kind":"feedback","signal":"mention","object_id":"issue_comment:9","number":7,"repo_id":104,"full_name":"example-owner/example-repo","actor_id":102,"initiator":"human","occurred_at":"2026-01-02T00:00:00Z","version":"v1","proof":{}})
}
#[test]
fn durable_attempt_then_commit_retirement_or_bounded_drop() {
    let d = tempfile::tempdir().unwrap();
    let c = config(d.path(), "#!/bin/sh\nexit 1\n");
    let target = send::Target {
        instance: "example".into(),
        member: "lead".into(),
    };
    let mut s = model::load(&c).unwrap();
    s.row["pending"]["one"] = pending();
    model::save(&c, &s).unwrap();
    for n in 1..=3 {
        let (committed, dropped) = send::deliver(
            &c,
            &mut s,
            &target,
            vec!["one".into()],
            Instant::now() + Duration::from_secs(20),
            |_, _, _| {
                assert_eq!(model::load(&c).unwrap().attempts["one"].tries, n);
                send::Outcome::Unconfirmed
            },
        )
        .unwrap();
        assert!(committed.is_empty());
        assert_eq!(dropped, usize::from(n == 3));
        if n < 3 {
            s.attempts.get_mut("one").unwrap().next_ms = 0;
        }
    }
    assert!(s.row["pending"].as_object().unwrap().is_empty());
    assert!(s.attempts.is_empty());
    s.row["pending"]["two"] = pending();
    model::save(&c, &s).unwrap();
    let (committed, dropped) = send::deliver(
        &c,
        &mut s,
        &target,
        vec!["two".into()],
        Instant::now() + Duration::from_secs(20),
        |_, _, _| send::Outcome::Committed,
    )
    .unwrap();
    assert_eq!(committed.len(), 1);
    assert_eq!(dropped, 0);
    let disk = model::load(&c).unwrap();
    assert!(disk.row["pending"].as_object().unwrap().is_empty());
    assert!(disk.attempts.is_empty());
}
#[test]
fn unavailable_does_not_consume_attempt_and_bad_pointer_does_not_starve_good() {
    let d = tempfile::tempdir().unwrap();
    let c = config(d.path(), "#!/bin/sh\nexit 1\n");
    let target = send::Target {
        instance: "example".into(),
        member: "lead".into(),
    };
    let mut s = model::load(&c).unwrap();
    s.row["pending"]["one"] = pending();
    model::save(&c, &s).unwrap();
    send::deliver(
        &c,
        &mut s,
        &target,
        vec!["one".into()],
        Instant::now() + Duration::from_secs(20),
        |_, _, _| send::Outcome::Unavailable,
    )
    .unwrap();
    assert!(s.attempts.is_empty());
    assert_eq!(s.row["pending"].as_object().unwrap().len(), 1);
    s.row["pending"]["bad"] = pending();
    s.row["pending"]["bad"]["object_id"] = json!("unknown:9");
    s.row["pending"]["bad"]["number"] = json!(8);
    let (committed, _) = send::deliver(
        &c,
        &mut s,
        &target,
        vec!["bad".into(), "one".into()],
        Instant::now() + Duration::from_secs(20),
        |_, _, _| send::Outcome::Committed,
    )
    .unwrap();
    assert_eq!(committed.len(), 1);
    assert_eq!(s.row["dirty"]["104:8"]["held"], true);
    assert!(s.row["pending"].get("one").is_none());
}
#[test]
fn reactions_require_verified_proof_and_enrolled_provenance() {
    use borgchart::github::{qualify, reactions};
    let d = tempfile::tempdir().unwrap();
    let c = config(d.path(), "#!/bin/sh\nexit 1\n");
    let mut s = model::load(&c).unwrap();
    s.row["repos"]["104"] = json!({"full_name":"example-owner/example-repo","baseline":"2026-01-01T00:00:00Z","status":"admitted","listed":true});
    let mut p = pending();
    p["proof"] = json!({"object_type":"issue_comment","comment_id":9});
    p["key"] = json!(qualify::digest(&[
        json!("github.com"),
        p["repo_id"].clone(),
        p["kind"].clone(),
        p["object_id"].clone(),
        p["version"].clone()
    ]));
    assert_eq!(
        reactions::target(&c, &s.row, &p),
        Some(("104:issue_comment:9".into(), 7))
    );
    let mut wrong = p.clone();
    wrong["actor_id"] = json!(999);
    assert!(reactions::target(&c, &s.row, &wrong).is_none());
    wrong = p.clone();
    wrong["proof"]["comment_id"] = json!(10);
    assert!(reactions::target(&c, &s.row, &wrong).is_none());
    wrong = p.clone();
    wrong["kind"] = json!("merge");
    assert!(reactions::target(&c, &s.row, &wrong).is_none());
    s.row["pins"]["example-owner/example-repo"] = json!(104);
    assert!(reactions::target(&c, &s.row, &p).is_none());
}
#[test]
fn capacity_hold_has_headroom_and_pending_can_still_retire() {
    let d = tempfile::tempdir().unwrap();
    let mut c = config(d.path(), "#!/bin/sh\nexit 1\n");
    c.settings.max_state_bytes = 8192;
    let mut s = model::load(&c).unwrap();
    s.row["pending"]["one"] = pending();
    model::save(&c, &s).unwrap();
    s.row["hints"]["large"] = json!("x".repeat(5000));
    assert_eq!(
        engine::persist(&c, &mut s).unwrap_err().kind,
        "state_capacity"
    );
    assert!(
        model::load(&c)
            .unwrap()
            .feed
            .holds
            .contains("state_capacity")
    );
    assert!(s.row["hints"].get("large").is_none());
    let target = send::Target {
        instance: "example".into(),
        member: "lead".into(),
    };
    let (committed, _) = send::deliver(
        &c,
        &mut s,
        &target,
        vec!["one".into()],
        Instant::now() + Duration::from_secs(20),
        |_, _, _| send::Outcome::Committed,
    )
    .unwrap();
    assert_eq!(committed.len(), 1);
    assert!(
        model::load(&c).unwrap().row["pending"]
            .as_object()
            .unwrap()
            .is_empty()
    );
}
#[test]
fn status_preserves_operator_hold_keys_and_reasons() {
    let d = tempfile::tempdir().unwrap();
    let c = config(d.path(), "#!/bin/sh\nexit 1\n");
    let mut s = model::load(&c).unwrap();
    engine::hold_item(
        &mut s.row,
        &json!({"repo_id":104,"number":7,"full_name":"example-owner/example-repo"}),
        "identity",
    );
    let out = borgchart::github::summary(&s, 0);
    assert_eq!(out["detail"], "identity");
    assert_eq!(out["holds"]["item:104:7"]["reason"], "identity");
    assert_eq!(out["ok"], false);
    s.feed.holds.insert("feed_page_cap".into());
    let out = borgchart::github::summary(&s, 0);
    assert_eq!(out["feed_holds"], json!(["feed_page_cap"]));
}
#[test]
fn conditional_response_requires_an_actually_empty_body() {
    let d = tempfile::tempdir().unwrap();
    let c = config(d.path(), &response(304, "", &Value::Null, 1));
    let mut a = Api::new(&c, Instant::now() + Duration::from_secs(5));
    assert!(a.request("/notifications", Some("old")).is_err());
}
#[test]
fn foreign_issue_is_rejected_before_fetching_histories() {
    let d = tempfile::tempdir().unwrap();
    let repo = json!({"id":104,"full_name":"example-owner/example-repo","owner":{"id":103,"login":"example-owner","type":"User"}});
    let c = config(d.path(), &response(200, "", &repo, 0));
    let mut s = model::load(&c).unwrap();
    let mut a = Api::new(&c, Instant::now() + Duration::from_secs(5));
    assert_eq!(
        engine::targeted(
            &mut a,
            &mut s.row,
            &json!({"repo_id":104,"number":7,"full_name":"example-owner/example-repo"})
        )
        .unwrap_err()
        .kind,
        "identity"
    );
}
