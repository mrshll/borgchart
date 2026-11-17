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
if [ "$1" = status ]; then
  printf '%s\n' "$*" >> "$FIXTURE_DIR/status-calls"
  [ ! -f "$FIXTURE_DIR/status-fail" ] || exit 1
  /bin/cat "$FIXTURE_DIR/status"
  exit 0
fi
/bin/cat "$FIXTURE_DIR/state/intake.json" >> "$FIXTURE_DIR/journal-at-send"
for arg do recipient=$text; text=$arg; done
printf '%s\n---\n' "$text" >> "$FIXTURE_DIR/sent"
printf '%s\n%s\n---\n' "$recipient" "$text" >> "$FIXTURE_DIR/deliveries"
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
    f.write("status", "[]");
    std::os::unix::fs::symlink("/usr/bin/git", f.path("git")).unwrap();
    f
}
fn drive(f: &Fixture, times: &str) -> bool {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    f.environment(&mut cmd);
    let output = cmd
        .env("ADMISSION_CHILD", times)
        .env("BORGCHART_BIN", f.path("brg"))
        .env("BORGCHART_DELIVER_TO", "example-team/root")
        .env("BORGCHART_TEAM", "example-team")
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
    i["closedByPullRequestsReferences"] = json!({"nodes":[]});
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
fn reads(sent: &str, id: &str) -> usize {
    sent.matches(&format!("brg-github read {id}\n")).count()
}
fn pending(f: &Fixture) -> usize {
    f.json("state/intake.json")["pending"]
        .as_array()
        .unwrap()
        .len()
}
#[test]
fn four_owner_triggers_deliver_identifiers_once_without_github_text() {
    let f = fixture();
    let mut commented = event("PullRequestReview", "PRR_commented", 202, "");
    commented["state"] = json!("COMMENTED");
    let mut draft = event("PullRequestReview", "PRR_draft", 202, "");
    draft["submittedAt"] = Value::Null;
    let review = event(
        "PullRequestReview",
        "PRR_review",
        202,
        "REVIEW_MARKER @example-bot",
    );
    let mention = event(
        "IssueComment",
        "IC_mention",
        202,
        "COMMENT_MARKER @example-bot",
    );
    let issue = vec![mention, event("AssignedEvent", "AE_assign", 202, "")];
    let merge = event("MergedEvent", "ME_merge", 202, "");
    changed(
        &f,
        &[
            (false, issue),
            (true, vec![review, merge, commented, draft]),
        ],
    );
    drive(&f, "0,60,120");
    let sent = text(&f);
    for (id, kind, number) in [
        ("IC_mention", "mention", 1),
        ("AE_assign", "assigned", 1),
        ("PRR_review", "review APPROVED on 3f2a91c", 2),
        ("PRR_commented", "review COMMENTED on 3f2a91c", 2),
        ("ME_merge", "merged on 3f2a91c", 2),
    ] {
        assert_eq!(reads(&sent, id), 1, "{id}: {sent}");
        let line = format!("example-org/widgets#{number}");
        assert!(
            sent.lines()
                .any(|l| l.contains(&line) && l.contains(kind) && l.contains(id))
        );
    }
    assert!(
        !sent.contains("PRR_draft") && !sent.contains("MARKER"),
        "{sent}"
    );
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
    let kinds = [
        "IssueComment",
        "PullRequestReview",
        "AssignedEvent",
        "MergedEvent",
    ];
    let mut foreign = Vec::new();
    for actor in [101, 303, 404] {
        for kind in kinds {
            foreign.push(event(
                kind,
                &format!("Other_{actor}_{kind}"),
                actor,
                "@example-bot",
            ));
        }
    }
    let mut planted = event("IssueComment", "IC_planted", 202, "@example-bot");
    planted["lastEditedAt"] = json!(AT);
    planted["editor"] = json!({"databaseId":303});
    let mut selfedit = event("IssueComment", "IC_selfedit", 202, "@example-bot");
    selfedit["lastEditedAt"] = json!(AT);
    selfedit["editor"] = json!({"databaseId":202});
    let mut nodes = foreign.clone();
    nodes.extend([
        planted,
        selfedit,
        event("IssueComment", "IC_plain", 202, "plain"),
        event("AssignedEvent", "AE_pr", 202, ""),
        event("PullRequestReview", "PRR_unassigned", 202, ""),
        event("MergedEvent", "ME_unassigned", 202, ""),
    ]);
    for author in [202, 303] {
        let mut review = event("PullRequestReview", &format!("PRR_inline{author}"), 202, "");
        review["comments"] = json!({"nodes":[content(author,"INLINE_MARKER @example-bot")]});
        nodes.push(review);
    }
    let mut assignment = event("AssignedEvent", "AE_other", 202, "");
    assignment["assignee"]["databaseId"] = json!(303);
    changed(
        &f,
        &[
            (true, vec![]),
            (true, foreign.clone()),
            (false, foreign),
            (false, vec![assignment]),
        ],
    );
    // An unassigned pull request whose owner-authored body mentions the machine.
    let mut raw = item(true, nodes);
    raw["assignees"] = json!({"nodes":[]});
    raw["body"] = json!("BODY_MARKER @example-bot");
    answer(&f, 1, &raw);
    drive(&f, "120");
    let sent = text(&f);
    for id in ["IC_selfedit", "I_1", "PRR_inline202"] {
        assert_eq!(reads(&sent, id), 1, "{id}: {sent}");
    }
    assert_eq!(sent.matches("brg-github read").count(), 3, "{sent}");
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
    let relisted =
        json!([{"node_id":"I_1","number":1,"updated_at":"2026-01-02T12:10:00Z","pull_request":{}}]);
    f.response("next-issues", 200, &relisted.to_string());
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
    let health = f.health();
    assert_eq!(health["ok"], false);
    assert!(
        health["detail"]
            .as_str()
            .unwrap()
            .contains("blocked\\x0aretry"),
        "{health}"
    );
    // The retry is killed mid-send, after its admission was saved.
    fs::remove_file(f.path("fail")).unwrap();
    f.write("crash", "");
    f.write("expect-crash", "");
    assert!(!drive(&f, "180"));
    let journal = fs::read_to_string(f.path("journal-at-send")).unwrap();
    assert_eq!(
        journal.matches("\"id\":\"AE_retry\"").count(),
        2,
        "{journal}"
    );
    assert_eq!(pending(&f), 1);
    drive(&f, "240");
    assert_eq!(reads(&text(&f), "AE_retry"), 3);
    assert_eq!(pending(&f), 0);
    assert_eq!(f.health()["ok"], true);
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
        let second =
            json!({"data":{"repository":{"issueOrPullRequest":item(false,nodes[4..].to_vec())}}});
        f.response("graphql-1-next", 200, &second.to_string());
        drive(&f, "240");
        let sent = text(&f);
        let notices: Vec<_> = sent.split("\n---\n").filter(|n| !n.is_empty()).collect();
        assert!(notices.len() > 1);
        for notice in notices {
            assert!(notice.len() <= 900, "{} bytes", notice.len());
            assert!(notice.matches("brg-github read").count() <= 6);
        }
        for n in 0..8 {
            assert_eq!(reads(&sent, &format!("AE_{n:0width$}")), 1);
        }
        assert_eq!(f.health()["ok"], true);
    }
}
#[test]
fn malformed_identifiers_and_timeline_rate_limits_never_advance_past_a_failed_item() {
    for (field, value) in [
        ("id", json!("PRR_bad\ntext")),
        ("commit", json!({"oid":"not-hex"})),
        ("state", json!("APPROVED\ntext")),
    ] {
        let f = fixture();
        let mut e = event("PullRequestReview", "PRR_safe", 202, "");
        e[field] = value;
        changed(&f, &[(true, vec![e]), (false, vec![])]);
        let listed = json!([
            {"node_id":"I_1","number":1,"updated_at":AT,"pull_request":{}},
            {"node_id":"I_2","number":2,"updated_at":"2026-01-02T12:10:00Z"}
        ]);
        f.response("issues", 200, &listed.to_string());
        drive(&f, "720,780");
        assert!(text(&f).is_empty());
        assert_eq!(f.health()["ok"], false);
        assert_eq!(f.calls().matches("number=1").count(), 2);
        // The later item's success does not move the entry's cursor past the failure.
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
fn git(path: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("/usr/bin/git")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_AUTHOR_NAME", "Example Bot")
        .env("GIT_AUTHOR_EMAIL", "bot@example.invalid")
        .env("GIT_COMMITTER_NAME", "Example Bot")
        .env("GIT_COMMITTER_EMAIL", "bot@example.invalid")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap().trim().into()
}
fn checkout(f: &Fixture, team: &str, kind: &str, dir: &str) -> Value {
    let path = f.path(dir);
    fs::create_dir(&path).unwrap();
    git(&path, &["init", "-b", "example-branch"]);
    git(
        &path,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "--allow-empty",
            "-m",
            "Example head",
        ],
    );
    json!({"kind":kind,"team":team,"worktree":path})
}
fn lead(team: &str, member: &str) -> Value {
    json!({"kind":"member","team":team,"member":member,"role":"lead","liveness":"alive"})
}
fn deliveries(f: &Fixture) -> Vec<String> {
    fs::read_to_string(f.path("deliveries"))
        .unwrap_or_default()
        .split("\n---\n")
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
#[test]
fn checkout_heads_route_prs_and_linked_issues_and_copy_only_approvals() {
    for mode in ["shared", "member", "detached", "issue", "root", "mention"] {
        let f = fixture();
        let (team, kind, member) = match mode {
            "root" => ("example-team", "member", "root"),
            "member" => ("example-team/widgets", "member", "lead"),
            _ => ("example-team/widgets", "team", "lead"),
        };
        // Two checkouts in the same team are one owner, not an ambiguity.
        let rows = [
            checkout(&f, team, kind, "first"),
            checkout(&f, team, "member", "second"),
            lead(team, member),
        ];
        f.write("status", &json!(rows).to_string());
        let mut commented = event("PullRequestReview", "PRR_comment", 202, "");
        commented["state"] = json!("COMMENTED");
        let events = if mode == "issue" {
            vec![event("AssignedEvent", "AE_linked", 202, "")]
        } else {
            vec![
                event("PullRequestReview", "PRR_approved", 202, ""),
                commented,
            ]
        };
        changed(&f, &[(mode != "issue", events.clone())]);
        let mut raw = item(mode != "issue", events);
        if mode == "mention" {
            raw["assignees"] = json!({"nodes":[]});
            raw["timelineItems"]["nodes"][0]["body"] = json!("@example-bot");
        }
        if mode == "detached" {
            let oid = git(&f.path("first"), &["rev-parse", "HEAD"]);
            git(&f.path("first"), &["checkout", "--detach"]);
            git(&f.path("second"), &["checkout", "--detach"]);
            raw["headRefName"] = json!("UNTRUSTED_BRANCH_MARKER");
            raw["headRefOid"] = json!(oid);
        }
        if mode == "issue" {
            raw["closedByPullRequestsReferences"] = json!({"nodes":[
                {"headRefName":"unheld","headRefOid":"0000000"},
                {"headRefName":"example-branch","headRefOid":"1111111"}
            ]});
        }
        answer(&f, 1, &raw);
        drive(&f, "0,60");
        let sent = deliveries(&f);
        let recipient = if mode == "root" {
            "example-team/root"
        } else {
            "example-team/widgets/lead"
        };
        let routed: Vec<_> = sent
            .iter()
            .filter(|s| s.starts_with(&format!("{recipient}\n")))
            .collect();
        assert_eq!(routed.len(), 1, "{mode}: {sent:?}");
        assert!(routed[0].contains(&format!("team {team} holds the branch")));
        assert_eq!(
            fs::read_to_string(f.path("status-calls")).unwrap(),
            "status example-team --json\n"
        );
        assert!(
            !text(&f).contains("example-branch") && !text(&f).contains("UNTRUSTED_BRANCH_MARKER")
        );
        if mode != "issue" && mode != "root" {
            let copy = sent
                .iter()
                .find(|s| s.starts_with("example-team/root\n"))
                .unwrap();
            assert!(copy.contains("approval copy; owning lead: example-team/widgets/lead"));
            assert!(copy.contains("3f2a91c"));
            assert_eq!(reads(copy, "PRR_approved"), 1);
            assert_eq!(reads(copy, "PRR_comment"), 0);
            assert_eq!(sent.len(), 2);
        } else {
            assert_eq!(sent.len(), 1, "{mode}: {sent:?}");
        }
    }
}
#[test]
fn routing_fallbacks_deliver_and_retry_keeps_the_admitted_recipient() {
    for mode in [
        "missing",
        "unheld",
        "several",
        "stopped",
        "unavailable",
        "retry",
    ] {
        let f = fixture();
        let widgets = "example-team/widgets";
        let mut rows = vec![
            checkout(&f, widgets, "team", "first"),
            lead(widgets, "lead"),
        ];
        let reason = match mode {
            "unheld" => {
                git(&f.path("first"), &["checkout", "-b", "other-branch"]);
                "no team holds the branch"
            }
            "missing" => {
                rows[0]["worktree"] = json!(f.path("missing"));
                "no team holds the branch"
            }
            "several" => {
                let other = "example-team/other";
                rows.extend([checkout(&f, other, "member", "second"), lead(other, "lead")]);
                "several teams hold the branch: example-team/other, example-team/widgets"
            }
            "stopped" => {
                rows[1]["liveness"] = json!("gone");
                "owning lead is stopped: example-team/widgets/lead"
            }
            "unavailable" => {
                f.write("status-fail", "");
                "routing unavailable"
            }
            _ => {
                f.write("fail", "");
                "team example-team/widgets holds the branch"
            }
        };
        f.write("status", &json!(rows).to_string());
        changed(
            &f,
            &[(
                true,
                vec![event("IssueComment", "IC_routed", 202, "@example-bot")],
            )],
        );
        drive(&f, "120");
        if mode == "retry" {
            fs::remove_file(f.path("fail")).unwrap();
            f.write("status", "[]");
            drive(&f, "180");
            assert_eq!(
                fs::read_to_string(f.path("status-calls"))
                    .unwrap()
                    .lines()
                    .count(),
                1
            );
        }
        let sent = deliveries(&f);
        let recipient = if mode == "retry" {
            "example-team/widgets/lead"
        } else {
            "example-team/root"
        };
        for notice in &sent {
            assert!(
                notice.starts_with(&format!("{recipient}\n")),
                "{mode}: {sent:?}"
            );
            assert!(notice.contains(reason), "{mode}: {notice}");
            assert_eq!(reads(notice, "IC_routed"), 1);
        }
        assert_eq!(sent.len(), if mode == "retry" { 2 } else { 1 });
        assert_eq!(pending(&f), 0);
        assert_eq!(f.health()["ok"], true);
    }
}
