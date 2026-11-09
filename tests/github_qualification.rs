use borgchart::github::{mentions, qualify};
use qualify::*;
use serde_json::{Value, json};
const T0: &str = "2026-01-01T00:00:00+00:00";
const T1: &str = "2026-01-02T00:00:00+00:00";
const T2: &str = "2026-01-03T00:00:00+00:00";
const ROOT: &str = "https://api.github.com/repos/example-owner/example-repo";
fn user(id: u64) -> Value {
    json!({"id":id,"type":"User"})
}
fn config() -> Value {
    json!({"agent":{"id":101,"login":"example-agent"},"trusted_actor_ids":[102],"app_initiators":[],"trusted_reviewer_ids":[],"issue_mentions_after":T0,"merge_wakes_after":T0})
}
fn owner() -> Value {
    json!({"owner":{"id":103,"login":"example-owner","type":"User"}})
}
fn facts(pr: bool) -> Facts {
    let mut issue = json!({"id":105,"number":7,"repository_url":ROOT,"url":format!("{ROOT}/issues/7"),"state":"open","assignees":[user(101)],"user":user(101)});
    if pr {
        issue["pull_request"] = json!({"url":format!("{ROOT}/pulls/7")});
    }
    Facts {
        repo: json!({"id":104,"full_name":"example-owner/example-repo","owner":owner()["owner"]}),
        issue,
        ..Default::default()
    }
}
fn assignment() -> Value {
    json!({"id":106,"url":format!("{ROOT}/issues/events/106"),"event":"assigned","created_at":T1,"actor":user(101),"assigner":user(102),"assignee":user(101)})
}
fn comment() -> Value {
    json!({"id":107,"url":format!("{ROOT}/issues/comments/107"),"issue_url":format!("{ROOT}/issues/7"),"user":user(102),"body":"@example-agent please inspect","created_at":T1,"updated_at":T1})
}
fn verify(c: &Value, f: &Facts) -> Result<Value, String> {
    verify_item(c, &owner(), 104, "example-owner/example-repo", 7, f)
}
fn row() -> Value {
    json!({"repos":{"104":{"status":"admitted","baseline":T0}},"trusted_since":{"102":T0},"mention_since":T0,"seen":{},"objects":{},"pending":{}})
}
#[test]
fn timestamp_precision_and_validation() {
    let base = stamp(T0).unwrap();
    assert_eq!(stamp("2026-01-01T00:00:00.000001999Z").unwrap(), base + 1);
    assert_eq!(
        stamp("2026-01-01T00:00:00.123456+00:00").unwrap(),
        base + 123456
    );
    for bad in [
        "2026-02-30T00:00:00Z",
        "2026-01-01T00:00:00+01:00",
        "2026-01-01T00:00:00.1234567890Z",
        "",
    ] {
        assert!(stamp(bad).is_err());
    }
}
#[test]
fn assignment_trust_and_identity() {
    for who in [101, 102, 999] {
        let mut f = facts(false);
        let mut e = assignment();
        e["assigner"] = user(who);
        f.events.push(e);
        assert_eq!(
            verify(&config(), &f).unwrap()["signals"]
                .as_array()
                .unwrap()
                .len(),
            usize::from(who == 102)
        );
    }
    for mutation in [
        "bot",
        "app",
        "conflict",
        "unassigned",
        "closed",
        "wrong_owner",
        "wrong_parent",
    ] {
        let mut f = facts(false);
        f.events.push(assignment());
        match mutation {
            "bot" => f.events[0]["assigner"]["type"] = json!("Bot"),
            "app" => f.events[0]["performed_via_github_app"] = json!({"id":999}),
            "conflict" => f.events[0]["actor"] = user(999),
            "unassigned" => f.issue["assignees"] = json!([]),
            "closed" => f.issue["state"] = json!("closed"),
            "wrong_owner" => f.repo["owner"]["id"] = json!(999),
            _ => {
                f.issue["repository_url"] = json!("https://api.github.com/repos/example-other/repo")
            }
        };
        let result = verify(&config(), &f);
        if mutation.starts_with("wrong") {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap()["signals"], json!([]), "{mutation}");
        }
    }
}
#[test]
fn mentions_edits_cutoffs_and_first_sight() {
    for mutation in ["none", "edited", "old", "bot", "untrusted", "quote"] {
        let mut f = facts(false);
        let mut c = comment();
        match mutation {
            "edited" => c["updated_at"] = json!(T2),
            "old" => {
                c["created_at"] = json!(T0);
                c["updated_at"] = json!(T0);
            }
            "bot" => c["user"]["type"] = json!("Bot"),
            "untrusted" => c["user"] = user(999),
            "quote" => c["body"] = json!("> @example-agent"),
            _ => {}
        };
        f.comments.push(c);
        assert_eq!(
            verify(&config(), &f).unwrap()["signals"]
                .as_array()
                .unwrap()
                .len(),
            usize::from(mutation == "none")
        );
    }
    let mut f = facts(false);
    f.comments.push(comment());
    let s = verify(&config(), &f).unwrap();
    let mut r = row();
    assert_eq!(
        merge_snapshot(&mut r, &s, T2, &members(&config())).unwrap(),
        1
    );
    assert_eq!(
        merge_snapshot(&mut r, &s, T2, &members(&config())).unwrap(),
        0
    );
    f.comments[0]["body"] = json!("@example-agent changed");
    assert_eq!(
        merge_snapshot(
            &mut r,
            &verify(&config(), &f).unwrap(),
            T2,
            &members(&config())
        )
        .unwrap(),
        0
    );
}
#[test]
fn merge_is_atomic_and_cancels() {
    let mut f = facts(false);
    f.events.push(assignment());
    let s = verify(&config(), &f).unwrap();
    let mut r = row();
    assert_eq!(
        merge_snapshot(&mut r, &s, T1, &members(&config())).unwrap(),
        1
    );
    let saved = r.clone();
    let mut bad = s.clone();
    bad["observations"][0]["occurred_at"] = json!("bad");
    assert!(merge_snapshot(&mut r, &bad, T2, &members(&config())).is_err());
    assert_eq!(r, saved);
    f.issue["state"] = json!("closed");
    merge_snapshot(
        &mut r,
        &verify(&config(), &f).unwrap(),
        T2,
        &members(&config()),
    )
    .unwrap();
    assert_eq!(r["pending"], json!({}));
    assert_eq!(r["seen"].as_object().unwrap().len(), 1);
}
#[test]
fn conservative_mentions() {
    for (body, expected) in [
        ("@example-agent: please inspect", true),
        ("  @EXAMPLE-AGENT please inspect", true),
        ("@example-agent-other no", false),
        ("hello @example-agent", false),
        ("> @example-agent", false),
        ("```\n@example-agent\n```", false),
        ("    @example-agent", false),
        ("<pre>\n@example-agent\n</pre>", false),
        ("`open\n@example-agent", false),
        ("@example-agent\n---", false),
        ("- @example-agent", false),
        ("text\n\n@example-agent", true),
        ("<div>\n```\n</div>\n\n@example-agent", true),
        (" <div>\n```\n\n@example-agent", false),
        ("[open\n@example-agent", false),
        ("$$\n\n@example-agent", false),
        ("@example-agent\n| --- |", false),
    ] {
        assert_eq!(
            mentions::explicit_mention(body, "example-agent"),
            expected,
            "{body:?}"
        );
    }
    assert!(!mentions::explicit_mention("@x", ""));
}
#[test]
fn python_reference_parity() {
    // Golden fake inputs and outputs from Python reference 237d974. The optional
    // local corpus lets development compare larger generated samples too.
    let extra = std::env::var_os("BORG_GITHUB_PARITY").map(|p| std::fs::read_to_string(p).unwrap());
    let corpus: Value = serde_json::from_str(
        extra
            .as_deref()
            .unwrap_or(include_str!("fixtures/github/qualification.json")),
    )
    .unwrap();
    for case in corpus["snapshots"].as_array().unwrap() {
        let f: Facts = serde_json::from_value(case["facts"].clone()).unwrap();
        let got = verify_item(
            &case["config"],
            &case["owner"],
            104,
            "example-owner/example-repo",
            7,
            &f,
        );
        if let Some(expected) = case["expected"].get("ok") {
            assert_eq!(got.as_ref().unwrap(), expected, "{}", case["name"]);
        } else {
            assert_eq!(
                got.unwrap_err(),
                case["expected"]["error"].as_str().unwrap(),
                "{}",
                case["name"]
            );
        }
    }
    for case in corpus["mentions"].as_array().unwrap() {
        let body = case["body"].as_str().unwrap();
        assert_eq!(
            mentions::explicit_mention(body, "example-agent"),
            case["expected"].as_bool().unwrap(),
            "{body:?}"
        );
    }
    for case in corpus["digests"].as_array().unwrap() {
        assert_eq!(
            digest(case["input"].as_array().unwrap()),
            case["expected"].as_str().unwrap(),
            "{}",
            case["input"]
        );
    }
}
#[test]
fn trust_class_validation() {
    let mut c = config();
    c["app_initiators"] = json!([{"app_id":112,"bot_user_id":113,"repository_ids":[105,104]}]);
    c["trusted_reviewer_ids"] = json!([109, 108]);
    assert_eq!(
        trust_classes(&c).unwrap()["app_initiators"][0]["repository_ids"],
        json!([104, 105])
    );
    assert_eq!(members(&c)["reviewer:108"], json!({"reviewer":108}));
    for invalid in [
        json!([102]),
        json!([101]),
        json!([113]),
        json!([108, 108]),
        json!([true]),
        json!([0]),
    ] {
        c["trusted_reviewer_ids"] = invalid;
        assert!(trust_classes(&c).is_err());
    }
    c["trusted_reviewer_ids"] = json!([]);
    c["app_initiators"][0]["repository_ids"] = json!([104, 104]);
    assert!(trust_classes(&c).is_err());
}
#[test]
fn merge_boundaries_and_atomic_validation() {
    let mut f = facts(false);
    f.comments.push(comment());
    let s = verify(&config(), &f).unwrap();
    let mut r = row();
    r["mention_since"] = json!(T1);
    assert_eq!(
        merge_snapshot(&mut r, &s, T2, &members(&config())).unwrap(),
        0
    );
    r["mention_since"] = json!(T0);
    assert_eq!(
        merge_snapshot(&mut r, &s, T2, &members(&config())).unwrap(),
        0
    );
    for field in ["actor_id", "initiator", "occurred_at", "proof", "repo_id"] {
        let mut bad = s.clone();
        bad["observations"][0][field] = json!("invalid");
        let mut r = row();
        let before = r.clone();
        assert!(
            merge_snapshot(&mut r, &bad, T2, &members(&config())).is_err(),
            "{field}"
        );
        assert_eq!(r, before);
    }
    let mut f = facts(false);
    f.events.push(assignment());
    f.events[0]["created_at"] = json!("2026-01-01T00:00:00.000002Z");
    let s = verify(&config(), &f).unwrap();
    let mut r = row();
    r["trusted_since"]["102"] = json!("2026-01-01T00:00:00.000001Z");
    assert_eq!(
        merge_snapshot(&mut r, &s, T1, &members(&config())).unwrap(),
        1
    );
}
#[test]
fn python_merge_parity() {
    let corpus: Value =
        serde_json::from_str(include_str!("fixtures/github/qualification.json")).unwrap();
    for case in corpus["merges"].as_array().unwrap() {
        let mut row = case["row"].clone();
        let policy = members(&case["config"]);
        let queued = merge_snapshot(
            &mut row,
            &case["snapshot"],
            case["now"].as_str().unwrap(),
            &policy,
        )
        .unwrap();
        assert_eq!(
            queued,
            case["queued"].as_u64().unwrap() as usize,
            "{}",
            case["name"]
        );
        let again = merge_snapshot(
            &mut row,
            &case["snapshot"],
            case["now"].as_str().unwrap(),
            &policy,
        )
        .unwrap();
        assert_eq!(again, 0);
        assert_eq!(row, case["expected"], "{}", case["name"]);
    }
}
#[test]
fn edit_freshness_and_late_errors_are_atomic() {
    let mut f = facts(false);
    f.events.push(assignment());
    let first = verify(&config(), &f).unwrap();
    let mut r = row();
    assert_eq!(
        merge_snapshot(&mut r, &first, T1, &members(&config())).unwrap(),
        1
    );
    // An observed object can gain a new version even when its source date is old.
    let mut edited = first.clone();
    for field in ["observations", "signals"] {
        edited[field][0]["key"] = json!("example-new-version");
        edited[field][0]["version"] = json!("example-version");
        edited[field][0]["occurred_at"] = json!(T0);
    }
    assert_eq!(
        merge_snapshot(&mut r, &edited, T2, &members(&config())).unwrap(),
        1
    );
    let before = r.clone();
    let mut late = first.clone();
    for field in ["observations", "signals"] {
        late[field][0]["key"] = json!("example-late-source");
    }
    r["repos"]["104"]["baseline"] = json!("invalid");
    let invalid_before = r.clone();
    assert!(merge_snapshot(&mut r, &late, T2, &members(&config())).is_err());
    assert_eq!(r, invalid_before);
    r = before;
    let mut closed = first.clone();
    closed["open"] = json!(false);
    closed["signals"] = json!([]);
    closed["observations"]
        .as_array_mut()
        .unwrap()
        .push(json!({}));
    let before = r.clone();
    assert!(merge_snapshot(&mut r, &closed, T2, &members(&config())).is_err());
    assert_eq!(r, before);
}
#[test]
fn enrollment_and_merge_cancellation_rules() {
    let mut c = config();
    c["app_initiators"] = json!([{"app_id":112,"bot_user_id":113,"repository_ids":[104]}]);
    let mut f = facts(false);
    let bot = json!({"id":113,"type":"Bot"});
    f.issue["user"] = bot.clone();
    f.issue["performed_via_github_app"] = json!({"id":112});
    let mut e = assignment();
    e["assigner"] = bot;
    f.events.push(e);
    let s = verify(&c, &f).unwrap();
    let mut r = row();
    r["trusted_since"]["app:112"] = json!(T1);
    r["objects"]["104:event:106"] = json!({"kind":"assignment","seen_at":T2});
    assert_eq!(merge_snapshot(&mut r, &s, T2, &members(&c)).unwrap(), 0);
    let mut f = facts(true);
    let mut e = assignment();
    e["event"] = json!("merged");
    e["actor"] = user(102);
    f.events.push(e);
    f.issue["state"] = json!("closed");
    let s = verify(&config(), &f).unwrap();
    let mut r = row();
    assert_eq!(
        merge_snapshot(&mut r, &s, T2, &members(&config())).unwrap(),
        1
    );
    f.events.clear();
    merge_snapshot(
        &mut r,
        &verify(&config(), &f).unwrap(),
        T2,
        &members(&config()),
    )
    .unwrap();
    assert_eq!(r["pending"].as_object().unwrap().len(), 1);
}
#[test]
fn malformed_facts_and_rows_do_not_panic() {
    for value in [
        Value::Null,
        json!(false),
        json!(1),
        json!("example"),
        json!([]),
        json!({}),
    ] {
        let mut f = facts(false);
        f.repo = value.clone();
        assert!(verify(&config(), &f).is_err());
        let mut f = facts(false);
        f.issue = value.clone();
        assert!(verify(&config(), &f).is_err());
        let mut f = facts(false);
        f.events.push(value.clone());
        assert!(verify(&config(), &f).is_err());
        let mut r = value.clone();
        let before = r.clone();
        assert!(merge_snapshot(&mut r, &json!({}), T2, &json!({})).is_err());
        assert_eq!(r, before);
    }
}
#[test]
fn microsecond_mention_and_handoff_ordering() {
    let mut f = facts(false);
    f.comments.push(comment());
    f.comments[0]["created_at"] = json!("2026-01-02T00:00:00.000001Z");
    f.comments[0]["updated_at"] = json!("2026-01-02T00:00:00.000002Z");
    assert_eq!(verify(&config(), &f).unwrap()["signals"], json!([]));
    let mut f = facts(true);
    let mut request = assignment();
    request["event"] = json!("review_requested");
    request["review_requester"] = user(101);
    request["requested_reviewer"] = user(102);
    request["created_at"] = json!("2026-01-02T00:00:00.000001Z");
    f.events.push(request);
    f.reviews.push(json!({"id":110,"pull_request_url":format!("{ROOT}/pulls/7"),"state":"APPROVED","user":user(102),"submitted_at":"2026-01-02T00:00:00.000001Z","body":"example feedback","commit_id":"abc"}));
    assert_eq!(verify(&config(), &f).unwrap()["signals"], json!([]));
    f.reviews[0]["submitted_at"] = json!("2026-01-02T00:00:00.000002Z");
    assert_eq!(
        verify(&config(), &f).unwrap()["signals"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
