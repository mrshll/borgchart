//! Admission and delivery through real plugin sessions with recording executables.
#[allow(dead_code)]
mod support;
use borgchart::{bus::time::parse_utc, plugin};
use brg_github::{gh::Gh, run::Session};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, process::Command, time::Duration};
use support::Fixture;

const AT: &str = "2026-01-02T12:00:00Z";
fn fixture() -> Fixture {
    let f = Fixture::new();
    f.write(
        "brg",
        r#"#!/bin/sh
/bin/cat "$FIXTURE_DIR/state/intake.json" >> "$FIXTURE_DIR/journal-at-send"
for arg do text=$arg; done
printf '%s\n---\n' "$text" >> "$FIXTURE_DIR/sent"
printf '%s\n' "$@" >> "$FIXTURE_DIR/brg-args"
if [ -f "$FIXTURE_DIR/crash" ]; then
  /bin/rm "$FIXTURE_DIR/crash"
  kill -KILL "$PPID"
fi
if [ -f "$FIXTURE_DIR/fail" ]; then
  printf '{"message":"blocked\\nretry"}'
  exit 1
fi
printf '{"delivery":"stored"}'
"#,
    );
    fs::set_permissions(f.path("brg"), fs::Permissions::from_mode(0o755)).unwrap();
    f
}
fn drive(f: &Fixture, times: &str) -> bool {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    f.environment(&mut cmd);
    let output = cmd
        .env("ADMISSION_CHILD", times)
        .env("BORGCHART_BIN", f.path("brg"))
        .env("BORGCHART_DELIVER_TO", "example-team/root")
        .args(["--exact", "drive_fixture", "--nocapture"])
        .output()
        .unwrap();
    if !f.path("expect-crash").exists() {
        assert!(output.status.success(), "{output:?}");
    }
    output.status.success()
}
#[test]
fn drive_fixture() {
    let Ok(times) = std::env::var("ADMISSION_CHILD") else {
        return;
    };
    let store = plugin::State::from_env().unwrap();
    let epoch = parse_utc(AT).unwrap();
    let mut session = Session::new(Gh, &store, epoch).unwrap();
    for offset in times.split(',') {
        session
            .tick(&store, epoch + Duration::from_secs(offset.parse().unwrap()))
            .unwrap();
        for name in ["issues", "graphql-1"] {
            let path = std::path::PathBuf::from(std::env::var_os("FIXTURE_DIR").unwrap());
            if path.join(format!("next-{name}")).exists() {
                fs::rename(path.join(format!("next-{name}")), path.join(name)).unwrap();
            }
        }
    }
}
fn text(f: &Fixture) -> String {
    fs::read_to_string(f.path("sent")).unwrap_or_default()
}
fn content(author: u64, body: &str) -> Value {
    json!({"author":{"databaseId":author},"body":body,"lastEditedAt":null,"editor":null})
}
fn event(kind: &str, id: &str, actor: u64, body: &str) -> Value {
    let mut e = content(actor, body);
    e["__typename"] = json!(kind);
    e["id"] = json!(id);
    e["actor"] = json!({"databaseId":actor});
    e["createdAt"] = json!(AT);
    e["submittedAt"] = json!(AT);
    e["state"] = json!("APPROVED");
    e["commit"] = json!({"oid":"3f2a91c0123456789012345678901234567890123"});
    e["assignee"] = json!({"databaseId":101});
    e["comments"] = json!({"nodes":[]});
    e
}
fn item(pr: bool, nodes: Vec<Value>) -> Value {
    let mut i = content(202, "BODY_MARKER");
    i["__typename"] = json!(if pr { "PullRequest" } else { "Issue" });
    i["id"] = json!("I_1");
    i["createdAt"] = json!(AT);
    i["assignees"] = json!({"nodes":[{"databaseId":101}]});
    i["headRefName"] = json!("example-branch");
    i["headRefOid"] = json!("abcdef0123456789012345678901234567890123");
    i["timelineItems"] = json!({"nodes":nodes,"pageInfo":{"hasNextPage":false,"endCursor":null}});
    i
}
fn answer(f: &Fixture, number: u64, item: &Value) {
    f.response(
        &format!("graphql-{number}"),
        200,
        &json!({"data":{"repository":{"issueOrPullRequest":item}}}).to_string(),
    );
}
fn changed(f: &Fixture, items: &[(bool, Vec<Value>)]) {
    let list: Vec<_> = items
        .iter()
        .enumerate()
        .map(|(n, (pr, nodes))| {
            let n = n + 1;
            let mut i = item(*pr, nodes.clone());
            i["id"] = json!(format!("I_{n}"));
            answer(f, n as u64, &i);
            let mut changed = json!({"node_id":format!("I_{n}"),"number":n,"updated_at":AT});
            if *pr {
                changed["pull_request"] = json!({});
            }
            changed
        })
        .collect();
    f.response("issues", 200, &json!(list).to_string());
}
#[test]
fn four_owner_triggers_deliver_identifiers_once_without_github_text() {
    let f = fixture();
    let mut reviews = vec![
        event(
            "PullRequestReview",
            "PRR_review",
            202,
            "REVIEW_MARKER @example-bot",
        ),
        event("MergedEvent", "ME_merge", 202, ""),
    ];
    for state in ["COMMENTED", "CHANGES_REQUESTED", "DISMISSED"] {
        let mut review = event("PullRequestReview", state, 202, "");
        review["state"] = json!(state);
        reviews.push(review);
    }
    let mut pending = event("PullRequestReview", "PRR_pending", 101, "");
    pending["state"] = json!("PENDING");
    pending["submittedAt"] = Value::Null;
    reviews.push(pending);
    changed(
        &f,
        &[
            (
                false,
                vec![
                    event(
                        "IssueComment",
                        "IC_mention",
                        202,
                        "COMMENT_MARKER @example-bot",
                    ),
                    event("AssignedEvent", "AE_assign", 202, ""),
                ],
            ),
            (true, reviews),
        ],
    );
    drive(&f, "0,60,120");
    let sent = text(&f);
    for (id, kind, number) in [
        ("IC_mention", "mention", 1),
        ("AE_assign", "assigned", 1),
        ("PRR_review", "review APPROVED on 3f2a91c", 2),
        ("ME_merge", "merged", 2),
    ] {
        assert_eq!(
            sent.matches(&format!("brg-github read {id}")).count(),
            1,
            "{sent}"
        );
        assert!(
            sent.contains(kind) && sent.contains(&format!("example-org/widgets#{number}")),
            "{sent}"
        );
    }
    for marker in ["BODY_MARKER", "COMMENT_MARKER", "REVIEW_MARKER"] {
        assert!(!sent.contains(marker));
    }
    for state in ["COMMENTED", "CHANGES_REQUESTED", "DISMISSED"] {
        assert_eq!(sent.matches(&format!("brg-github read {state}")).count(), 1);
        assert!(sent.contains(&format!("review {state} on 3f2a91c")));
    }
    assert!(!sent.contains("PRR_pending"));
    assert!(
        fs::read_to_string(f.path("brg-args"))
            .unwrap()
            .contains("example-team/root")
    );
    assert_eq!(f.health()["ok"], true);
}
#[test]
fn only_owner_actions_in_scope_and_unplanted_mentions_wake_the_lead() {
    let f = fixture();
    let mut nodes = Vec::new();
    for actor in [101, 303, 404] {
        for kind in [
            "IssueComment",
            "PullRequestReview",
            "AssignedEvent",
            "MergedEvent",
        ] {
            nodes.push(event(
                kind,
                &format!("Other_{actor}_{kind}"),
                actor,
                "@example-bot",
            ));
        }
    }
    let foreign = nodes.clone();
    for (n, body) in [
        "plain",
        "@example-bot2",
        "me@example-bot",
        "```rust\n@example-bot\n```",
        "~~~\n@example-bot\n~~~",
        "`@example-bot`",
        "``@example-bot``",
        "  > @example-bot",
    ]
    .iter()
    .enumerate()
    {
        nodes.push(event("IssueComment", &format!("IC_bad{n}"), 202, body));
    }
    let mut planted = event("IssueComment", "IC_planted", 202, "@example-bot");
    planted["lastEditedAt"] = json!(AT);
    planted["editor"] = json!({"databaseId":303});
    nodes.push(planted);
    nodes.push(event("IssueComment", "IC_yes", 202, "@Example-Bot, MARKER"));
    let mut edited = event("IssueComment", "IC_selfedit", 202, "@example-bot");
    edited["lastEditedAt"] = json!(AT);
    edited["editor"] = json!({"databaseId":202});
    nodes.push(edited);
    let mut assignment = event("AssignedEvent", "AE_other", 202, "");
    assignment["assignee"]["databaseId"] = json!(303);
    changed(
        &f,
        &[
            (true, nodes.clone()),
            (true, foreign.clone()),
            (false, foreign),
            (false, vec![assignment]),
        ],
    );
    let mut raw = item(true, nodes);
    raw["assignees"] = json!({"nodes":[]});
    raw["body"] = json!("BODY_MARKER @example-bot");
    raw["timelineItems"]["nodes"]
        .as_array_mut()
        .unwrap()
        .extend([
            event("AssignedEvent", "AE_pr", 202, ""),
            event("PullRequestReview", "PRR_unassigned", 202, ""),
            event("MergedEvent", "ME_unassigned", 202, ""),
        ]);
    for author in [202, 303] {
        let mut review = event("PullRequestReview", &format!("PRR_inline{author}"), 202, "");
        review["comments"] = json!({"nodes":[content(author,"INLINE_MARKER @example-bot")]});
        raw["timelineItems"]["nodes"]
            .as_array_mut()
            .unwrap()
            .push(review);
    }
    answer(&f, 1, &raw);
    drive(&f, "120");
    let sent = text(&f);
    for id in ["IC_yes", "IC_selfedit", "I_1", "PRR_inline202"] {
        assert!(sent.contains(&format!("brg-github read {id}")), "{sent}");
    }
    for id in [
        "Other_",
        "IC_bad",
        "IC_planted",
        "AE_",
        "PRR_unassigned",
        "ME_unassigned",
        "PRR_inline303",
        "MARKER",
    ] {
        assert!(!sent.contains(id), "{sent}");
    }
    assert_eq!(f.health()["ok"], true);
}
#[test]
fn old_edits_and_pre_epoch_actions_are_skipped_but_newly_submitted_drafts_arrive() {
    let f = fixture();
    let mut nodes = Vec::new();
    for (id, created) in [("IC_before", "2026-01-02T11:59:59Z"), ("IC_stale", AT)] {
        let mut e = event("IssueComment", id, 202, "@example-bot");
        e["createdAt"] = json!(created);
        e["lastEditedAt"] = json!("2026-01-02T12:10:00Z");
        e["editor"] = json!({"databaseId":202});
        nodes.push(e);
    }
    let mut review = event("PullRequestReview", "PRR_draft", 202, "");
    review["createdAt"] = json!("2020-01-01T00:00:00Z");
    review["submittedAt"] = json!("2026-01-02T12:10:00Z");
    nodes.push(review);
    changed(&f, &[(true, nodes)]);
    fs::rename(f.path("graphql-1"), f.path("next-graphql-1")).unwrap();
    answer(&f, 1, &item(true, vec![]));
    f.response("next-issues",200,&json!([{"node_id":"I_1","number":1,"updated_at":"2026-01-02T12:10:00Z","pull_request":{}}]).to_string());
    drive(&f, "600,660");
    let sent = text(&f);
    assert!(
        sent.contains("PRR_draft") && !sent.contains("IC_"),
        "{sent}"
    );
    // The same pre-start action must be rejected even before the cursor moves.
    let f = fixture();
    let mut e = event("AssignedEvent", "AE_before", 202, "");
    e["createdAt"] = json!("2026-01-02T11:59:59Z");
    changed(&f, &[(false, vec![e])]);
    drive(&f, "0");
    assert!(text(&f).is_empty());
    assert_eq!(f.health()["ok"], true);
}
#[test]
fn failed_sends_retry_and_saved_admission_survives_a_crash() {
    let f = fixture();
    changed(
        &f,
        &[(false, vec![event("AssignedEvent", "AE_retry", 202, "")])],
    );
    f.write("fail", "");
    drive(&f, "120");
    assert_eq!(f.health()["ok"], false);
    assert!(
        f.health()["detail"]
            .as_str()
            .unwrap()
            .contains("blocked\\x0aretry")
    );
    fs::remove_file(f.path("fail")).unwrap();
    drive(&f, "180");
    assert_eq!(f.health()["ok"], true);
    assert!(
        f.json("state/intake.json")["pending"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let f = fixture();
    changed(
        &f,
        &[(false, vec![event("AssignedEvent", "AE_crash", 202, "")])],
    );
    f.write("crash", "");
    f.write("expect-crash", "");
    assert!(!drive(&f, "120"));
    assert!(
        fs::read_to_string(f.path("journal-at-send"))
            .unwrap()
            .contains("AE_crash")
    );
    assert_eq!(
        f.json("state/intake.json")["pending"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    drive(&f, "180");
    assert_eq!(text(&f).matches("brg-github read AE_crash").count(), 2);
    assert!(
        f.json("state/intake.json")["pending"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
#[test]
fn failed_later_timeline_page_retries_without_partial_admission_then_sends_bounded_batches() {
    for width in [0, 120] {
        let f = fixture();
        let nodes: Vec<_> = (0..8)
            .map(|n| event("AssignedEvent", &format!("AE_{n:0width$}"), 202, ""))
            .collect();
        changed(&f, &[(false, nodes[..4].to_vec())]);
        let mut first = item(false, nodes[..4].to_vec());
        first["timelineItems"]["pageInfo"] = json!({"hasNextPage":true,"endCursor":"next"});
        answer(&f, 1, &first);
        f.response("graphql-1-next", 200, "{\"data\":null}");
        drive(&f, "120,180");
        assert!(text(&f).is_empty());
        assert_eq!(f.health()["ok"], false);
        assert_eq!(f.calls().matches("number=1").count(), 4);
        f.response(
            "graphql-1-next",
            200,
            &json!({"data":{"repository":{"issueOrPullRequest":item(false,nodes[4..].to_vec())}}})
                .to_string(),
        );
        drive(&f, "240");
        let sent = text(&f);
        let notices: Vec<_> = sent.split("\n---\n").filter(|n| !n.is_empty()).collect();
        assert!(notices.len() > 1);
        for notice in notices {
            assert!(notice.len() <= 900, "{} bytes", notice.len());
            assert!(notice.matches("brg-github read").count() <= 6);
        }
        for n in 0..8 {
            assert_eq!(
                sent.matches(&format!("brg-github read AE_{n:0width$}"))
                    .count(),
                1
            );
        }
        assert_eq!(f.health()["ok"], true);
    }
}
#[test]
fn malformed_identifiers_and_timeline_rate_limits_never_advance_past_a_failed_item() {
    for (field, value) in [
        ("id", json!("PRR_bad\ntext")),
        ("commit", json!({"oid":"not-hex"})),
        ("state", json!("FORGED")),
    ] {
        let f = fixture();
        let mut e = event("PullRequestReview", "PRR_safe", 202, "");
        e[field] = value;
        changed(&f, &[(true, vec![e]), (false, vec![])]);
        f.response(
            "issues",
            200,
            &json!([
                {"node_id":"I_1","number":1,"updated_at":AT,"pull_request":{}},
                {"node_id":"I_2","number":2,"updated_at":"2026-01-02T12:10:00Z"}
            ])
            .to_string(),
        );
        drive(&f, "720,780");
        assert!(text(&f).is_empty());
        assert_eq!(f.health()["ok"], false);
        assert_eq!(f.calls().matches("number=1").count(), 2);
        assert_eq!(
            f.calls().matches("since=2026-01-02T11:55:00.000Z").count(),
            5
        );
    }
    let f = fixture();
    changed(&f, &[(true, vec![])]);
    f.response("graphql-1", 429, "{}");
    drive(&f, "0,60,120");
    assert_eq!(f.calls().matches("number=1").count(), 1);
    assert_eq!(f.health()["ok"], false);
}
