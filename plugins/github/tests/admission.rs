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
    assert!(output.status.success(), "{output:?}");
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
            (
                true,
                vec![
                    event(
                        "PullRequestReview",
                        "PRR_review",
                        202,
                        "REVIEW_MARKER @example-bot",
                    ),
                    event("MergedEvent", "ME_merge", 202, ""),
                ],
            ),
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
    assert!(
        fs::read_to_string(f.path("brg-args"))
            .unwrap()
            .contains("example-team/root")
    );
    assert_eq!(f.health()["ok"], true);
}
