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
/bin/cat "$FIXTURE_DIR/plugins/github/intake.json" >> "$FIXTURE_DIR/journal-at-send"
for arg do recipient=$text; text=$arg; done
printf '%s\n%s\n---\n' "$recipient" "$text" >> "$FIXTURE_DIR/deliveries"
if [ -f "$FIXTURE_DIR/crash" ]; then
  /bin/rm "$FIXTURE_DIR/crash"
  kill -KILL "$PPID"
fi
if [ -f "$FIXTURE_DIR/fail-once" ]; then /bin/rm "$FIXTURE_DIR/fail-once"; exit 1; fi
if [ -f "$FIXTURE_DIR/fail" ] || { [ -f "$FIXTURE_DIR/fail-recipient" ] && [ "$recipient" = "$(/bin/cat "$FIXTURE_DIR/fail-recipient")" ]; }; then
  printf '{"message":"blocked\\nretry"}'
  exit 1
fi
printf '{"delivery":"stored"}'
"#,
    );
    fs::set_permissions(f.path("brg"), fs::Permissions::from_mode(0o755)).unwrap();
    f.write("status", "[]");
    f.response("issues-gadgets", 200, "[]");
    std::os::unix::fs::symlink("/usr/bin/git", f.path("git")).unwrap();
    f
}
fn drive(f: &Fixture, times: &str) -> bool {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    f.environment(&mut cmd);
    let output = cmd
        .env("ADMISSION_CHILD", times)
        .env("BORGCHART_BIN", f.path("brg"))
        .env("BORGCHART_DELIVER_TO", "root")
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
    let dir = std::path::PathBuf::from(std::env::var_os("FIXTURE_DIR").unwrap());
    let epoch = parse_utc(&fs::read_to_string(dir.join("epoch")).unwrap_or(AT.into())).unwrap();
    let mut session = Session::new(Gh::default(), &store, epoch).unwrap();
    for offset in times.split(',') {
        session
            .tick(&store, epoch + Duration::from_secs(offset.parse().unwrap()))
            .unwrap();
        fs::write(
            dir.join(format!("calls-at-{offset}")),
            fs::read(dir.join("calls")).unwrap_or_default(),
        )
        .unwrap();
        if dir.join("next-graphql-1").exists() || dir.join("next-config.yaml").exists() {
            fs::copy(
                dir.join("plugins/github/health.json"),
                dir.join("health-before-repair"),
            )
            .unwrap();
        }
        for name in [
            "issues",
            "graphql-1",
            "config.yaml",
            "template.yaml",
            "reaction",
        ] {
            let _ = fs::rename(dir.join(format!("next-{name}")), dir.join(name));
        }
        let _ = fs::rename(dir.join("next-reaction-code"), dir.join("reaction-code"));
    }
}
#[test]
fn template_edits_replace_the_discovery_scope_at_the_next_poll() {
    let f = fixture();
    f.repositories("template.yaml", "example-org/widgets");
    f.repositories(
        "next-template.yaml",
        "example-org/gadgets\nexample-org/gadgets",
    );
    drive(&f, "0,60");
    let after = f.read("calls-at-60");
    let next = after.strip_prefix(&f.read("calls-at-0")).unwrap();
    assert_eq!(next.matches("repos/example-org/gadgets/").count(), 1);
    assert!(!next.contains("repos/example-org/widgets/"));
}
#[test]
fn absent_github_section_turns_on_at_the_next_poll_without_restart() {
    let f = fixture();
    fs::rename(f.path("config.yaml"), f.path("next-config.yaml")).unwrap();
    f.write("config.yaml", "schema: 1\n");
    drive(&f, "0,60");
    let off = f.json("health-before-repair");
    assert_eq!(off["ok"], true);
    assert!(off["detail"].as_str().unwrap().starts_with("off: no "));
    assert!(f.read("calls-at-0").is_empty());
    let calls = f.read("calls-at-60");
    assert!(calls.contains("\nuser\n") && calls.contains("\nusers/example-owner\n"));
    assert!(calls.contains("repos/example-org/widgets/issues?"));
    assert_eq!(f.health()["ok"], true);
}
#[test]
fn invalid_global_configuration_stops_discovery_at_the_next_poll() {
    let f = fixture();
    let invalid = f.read("config.yaml").replace("schema: 1", "unknown: true");
    f.write("next-config.yaml", &invalid);
    drive(&f, "0,60");
    assert_eq!(f.health()["ok"], false);
    assert!(f.detail().contains("config.yaml"));
    assert!(!f.read("calls-at-0").is_empty());
    assert_eq!(f.read("calls-at-60"), f.read("calls-at-0"));
}
#[test]
fn listing_the_first_repository_later_skips_activity_before_the_listing() {
    let f = fixture();
    let yaml = "schema: 1\ngithub:\n  owner: example-owner\n  gh_config_dir: ~/machine-profile\n";
    f.write("config.yaml", &format!("{yaml}  repositories: []\n"));
    f.write(
        "next-config.yaml",
        &format!("{yaml}  repositories: [example-org/widgets]\n"),
    );
    // A day after process start, the last idle poll precedes the listing by one minute.
    drive(&f, "86340,86400");
    assert!(f.read("calls-at-86340").is_empty());
    let calls = f.read("calls-at-86400");
    let since = calls.split("since=").nth(1).unwrap();
    let since = parse_utc(&since[..since.find(['&', '\n']).unwrap()]).unwrap();
    let idle = parse_utc(AT).unwrap() + Duration::from_secs(86340);
    assert!(since >= idle - brg_github::state::OVERLAP, "{calls}");
}
#[test]
fn pending_comments_deliver_without_reactions_while_off_or_unlisted() {
    for off in [true, false] {
        let f = fixture();
        let team = "example-team/widgets";
        f.write(
            "status",
            &json!([checkout(&f, team, "team", "first"), lead(team, "lead")]).to_string(),
        );
        changed(
            &f,
            &[
                (
                    false,
                    vec![event("IssueComment", "IC_idle", 202, "@example-bot")],
                ),
                (
                    true,
                    vec![event("IssueComment", "IC_blocked", 202, "@example-bot")],
                ),
            ],
        );
        f.write("fail-once", "");
        f.write("fail-recipient", "example-team/widgets/lead");
        let unlisted = f.read("config.yaml").replace("[example-org/widgets]", "[]");
        f.write(
            "next-config.yaml",
            if off { "schema: 1\n" } else { &unlisted },
        );
        // The blocked comment keeps a delivery pending, so the tick reaches the reaction gate.
        drive(&f, "120,180");
        assert_eq!(pending(&f), 1, "{off}");
        assert_eq!(reads(&text(&f), "IC_idle"), 2, "{off}");
        assert_eq!(f.read("calls-at-180"), f.read("calls-at-120"), "{off}");
    }
}
/// Every delivery: its recipient line, then the notice.
fn text(f: &Fixture) -> String {
    f.read("deliveries")
}
fn content(author: u64, body: &str) -> Value {
    json!({"id":"PRRC_reply","author":{"databaseId":author},"body":body,"lastEditedAt":null,"editor":null})
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
    let (reviews, nodes): (Vec<_>, Vec<_>) = nodes
        .into_iter()
        .partition(|node| pr && node["__typename"] == "PullRequestReview");
    let mut i = content(202, "BODY_MARKER");
    i["__typename"] = json!(if pr { "PullRequest" } else { "Issue" });
    i["id"] = json!("I_1");
    i["createdAt"] = json!(AT);
    i["assignees"] = json!({"nodes":[{"databaseId":101}]});
    i["headRepository"] = json!({"nameWithOwner":"example-org/widgets"});
    i["headRefName"] = json!("example-branch");
    i["headRefOid"] = json!("abcdef0123456789012345678901234567890123");
    i["closedByPullRequestsReferences"] = json!({"nodes":[]});
    if pr {
        i["reviews"] = json!({"nodes":reviews});
    }
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
            json!({"node_id":format!("I_{n}"),"number":n,"updated_at":AT})
        })
        .collect();
    f.response("issues", 200, &json!(list).to_string());
}
fn reads(sent: &str, id: &str) -> usize {
    sent.matches(&format!("brg-github read {id}\n")).count()
}
fn pending(f: &Fixture) -> usize {
    f.json("plugins/github/intake.json")["pending"]
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
    let mut unknown_merge = merge.clone();
    unknown_merge["id"] = json!("ME_unknown");
    unknown_merge["commit"] = Value::Null;
    changed(
        &f,
        &[
            (false, issue),
            (true, vec![review, merge, unknown_merge, commented, draft]),
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
        ("ME_unknown", "merged on unknown commit", 2),
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
        deliveries(&f)
            .iter()
            .all(|d| d.starts_with("example-team/root\n"))
    );
    assert_eq!(f.health()["ok"], true);
}
#[test]
fn owner_thread_replies_outside_the_timeline_wake_by_assignment_or_unedited_mention() {
    for (assigned, edited, expected) in [(true, false, 1), (false, false, 1), (false, true, 0)] {
        let f = fixture();
        let mut reply = content(202, "@example-bot please check this reply");
        if edited {
            reply["lastEditedAt"] = json!(AT);
            reply["editor"] = json!({"databaseId":202});
        }
        let mut review = event("PullRequestReview", "PRR_reply", 202, "");
        review["state"] = json!("COMMENTED");
        review["comments"] = json!({"nodes":[reply]});
        changed(&f, &[(true, vec![review.clone()])]);
        let mut raw = item(true, vec![review]);
        if !assigned {
            raw["assignees"] = json!({"nodes":[]});
        }
        answer(&f, 1, &raw);
        drive(&f, "0,60");
        assert_eq!(
            reads(&text(&f), "PRR_reply"),
            expected,
            "{assigned}/{edited}"
        );
        assert_eq!(
            eyes(&f.calls()),
            vec!["PRRC_reply"; expected],
            "{assigned}/{edited}"
        );
        assert_eq!(f.health()["ok"], true);
    }
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
    let mut nodes = vec![
        planted,
        selfedit,
        event("IssueComment", "IC_plain", 202, "plain"),
        event("AssignedEvent", "AE_pr", 202, ""),
        event("PullRequestReview", "PRR_unassigned", 202, ""),
        event("MergedEvent", "ME_unassigned", 202, ""),
    ];
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
    for id in ["I_1", "PRR_inline202"] {
        assert_eq!(reads(&sent, id), 1, "{id}: {sent}");
    }
    assert_eq!(reads(&sent, "IC_selfedit"), 0);
    assert_eq!(sent.matches("brg-github read").count(), 2, "{sent}");
    assert_eq!(f.health()["ok"], true);
}
#[test]
fn pre_epoch_and_pre_drain_actions_are_skipped_but_newly_submitted_drafts_arrive() {
    let f = fixture();
    let mut nodes = Vec::new();
    for (id, created) in [("IC_before", "2026-01-02T11:59:59Z"), ("IC_stale", AT)] {
        let mut e = event("IssueComment", id, 202, "@example-bot");
        e["createdAt"] = json!(created);
        nodes.push(e);
    }
    let mut review = event("PullRequestReview", "PRR_draft", 202, "");
    review["createdAt"] = json!("2020-01-01T00:00:00Z");
    review["submittedAt"] = json!("2026-01-02T12:10:00Z");
    nodes.push(review);
    changed(&f, &[(true, nodes.clone())]);
    fs::rename(f.path("graphql-1"), f.path("next-graphql-1")).unwrap();
    // Before the cursor moves, only the start time rejects the pre-start comment.
    answer(&f, 1, &item(true, nodes[..1].to_vec()));
    let relisted = json!([{"node_id":"I_1","number":1,"updated_at":"2026-01-02T12:10:00Z"}]);
    f.response("next-issues", 200, &relisted.to_string());
    drive(&f, "600,660");
    let sent = text(&f);
    assert!(
        sent.contains("PRR_draft") && !sent.contains("IC_"),
        "{sent}"
    );
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
    assert!(f.detail().contains("blocked\\x0aretry"), "{}", f.detail());
    // The retry is killed mid-send, after its admission was saved.
    fs::remove_file(f.path("fail")).unwrap();
    f.write("crash", "");
    f.write("expect-crash", "");
    assert!(!drive(&f, "180"));
    let journal = f.read("journal-at-send");
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
        let second =
            json!({"data":{"repository":{"issueOrPullRequest":item(false,nodes[4..].to_vec())}}});
        f.response("graphql-1-next", 200, &second.to_string());
        drive(&f, "240");
        let sent = text(&f);
        let notices = deliveries(&f);
        assert!(notices.len() > 1);
        for notice in notices {
            let notice = notice.split_once('\n').unwrap().1;
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
            {"node_id":"I_1","number":1,"updated_at":AT},
            {"node_id":"I_2","number":2,"updated_at":"2026-01-02T12:10:00Z"}
        ]);
        f.response("issues", 200, &listed.to_string());
        f.write("issues-split", "2026-01-02T12:00:00.000Z");
        f.response("issues-after", 200, &json!([listed[1]]).to_string());
        fs::rename(f.path("graphql-1"), f.path("broken")).unwrap();
        answer(
            &f,
            1,
            &item(true, vec![event("PullRequestReview", "PRR_safe", 202, "")]),
        );
        fs::rename(f.path("graphql-1"), f.path("next-graphql-1")).unwrap();
        fs::rename(f.path("broken"), f.path("graphql-1")).unwrap();
        drive(&f, "720,780,840");
        assert_eq!(f.json("health-before-repair")["ok"], false);
        assert_eq!(reads(&text(&f), "PRR_safe"), 1);
        assert_eq!(f.health()["ok"], true);
    }
    let f = fixture();
    changed(&f, &[(true, vec![])]);
    f.response("graphql-1", 429, "{}");
    drive(&f, "0,60,120");
    assert_eq!(f.read("calls-at-0"), f.read("calls-at-120"));
    assert_eq!(f.health()["ok"], false);
}
fn git(path: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("/usr/bin/git")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
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
    git(&path, &["commit", "--allow-empty", "-m", "Example head"]);
    json!({"kind":kind,"team":team,"worktree":path})
}
fn lead(team: &str, member: &str) -> Value {
    json!({"kind":"member","team":team,"member":member,"role":"lead","liveness":"alive"})
}
fn deliveries(f: &Fixture) -> Vec<String> {
    text(f)
        .split("\n---\n")
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
#[test]
fn checkout_heads_route_prs_and_linked_issues_and_copy_only_approvals() {
    for mode in [
        "shared", "member", "detached", "issue", "root", "mention", "unknown", "null",
    ] {
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
        let mut rows = rows;
        if mode == "unknown" {
            rows[2]["liveness"] = json!("unknown");
        }
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
        if mode == "null" {
            raw["reviews"]["nodes"][0]["commit"] = Value::Null;
        }
        if mode == "mention" {
            raw["assignees"] = json!({"nodes":[]});
            raw["reviews"]["nodes"][0]["body"] = json!("@example-bot");
        }
        if mode == "detached" {
            let oid = git(&f.path("first"), &["rev-parse", "HEAD"]);
            git(&f.path("first"), &["checkout", "--detach"]);
            git(&f.path("second"), &["checkout", "--detach"]);
            raw["headRefName"] = json!("UNTRUSTED_BRANCH_MARKER");
            raw["headRefOid"] = json!(oid);
            raw["headRepository"] = json!({"nameWithOwner":"example-fork/widgets"});
        }
        if mode == "issue" {
            raw["closedByPullRequestsReferences"] = json!({"nodes":[
                {"headRefName":"unheld","headRefOid":"0000000","headRepository":{"nameWithOwner":"example-org/widgets"}},
                {"headRefName":"example-branch","headRefOid":"1111111","headRepository":{"nameWithOwner":"EXAMPLE-ORG/Widgets"}}
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
        assert!(
            !text(&f).contains("example-branch") && !text(&f).contains("UNTRUSTED_BRANCH_MARKER")
        );
        if mode != "issue" && mode != "root" {
            let copy = sent
                .iter()
                .find(|s| s.starts_with("example-team/root\n"))
                .unwrap();
            assert!(copy.contains("approval copy; owning lead: example-team/widgets/lead"));
            assert!(copy.contains(if mode == "null" {
                "on unknown commit"
            } else {
                "3f2a91c"
            }));
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
        "twelve",
        "fork",
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
            "twelve" => {
                for n in 0..11 {
                    let team = format!("example-team/development-project-{n:02}");
                    rows.push(checkout(&f, &team, "team", &format!("dir{n}")));
                }
                "and 9 more"
            }
            "fork" => "no team holds the branch",
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
        if mode == "fork" {
            let mut raw = item(
                true,
                vec![event("IssueComment", "IC_routed", 202, "@example-bot")],
            );
            raw["headRepository"] = json!({"nameWithOwner":"example-fork/widgets"});
            answer(&f, 1, &raw);
        }
        drive(&f, "120");
        if mode == "retry" {
            fs::remove_file(f.path("fail")).unwrap();
            f.write("status", "[]");
            drive(&f, "180");
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

#[test]
fn an_owner_edit_cannot_create_a_mention_on_the_next_poll() {
    let f = fixture();
    let plain = event("IssueComment", "IC_edited", 202, "plain");
    changed(&f, &[(false, vec![plain.clone()])]);
    let mut edited = plain.clone();
    edited["body"] = json!("@example-bot");
    edited["lastEditedAt"] = json!("2026-01-02T12:01:00Z");
    edited["editor"] = json!({"databaseId":202});
    answer(&f, 1, &item(false, vec![edited]));
    fs::rename(f.path("graphql-1"), f.path("next-graphql-1")).unwrap();
    answer(&f, 1, &item(false, vec![plain]));
    f.response(
        "next-issues",
        200,
        &json!([{"node_id":"I_1","number":1,"updated_at":"2026-01-02T12:01:00Z"}]).to_string(),
    );
    drive(&f, "0,60");
    assert!(text(&f).is_empty());
    assert_eq!(f.health()["ok"], true);
}
#[test]
fn a_superseded_item_keeps_its_owner_action_while_discovery_drains_a_backlog() {
    let f = fixture();
    let mut items = vec![(false, vec![]); 20];
    let mut mention = event("IssueComment", "IC_backlog", 202, "@example-bot");
    mention["createdAt"] = json!("2026-01-02T12:01:00Z");
    items.push((false, vec![mention]));
    changed(&f, &items);
    let list: Vec<_> = (1..=21)
        .map(|n| {
            json!({"node_id":format!("I_{n}"),"number":n,
        "updated_at":if n == 21 {"2026-01-02T12:20:00Z"} else {"2026-01-02T12:10:00Z"}})
        })
        .collect();
    f.response("issues", 200, &json!(list).to_string());
    drive(&f, "1800,1860,1920");
    assert_eq!(reads(&text(&f), "IC_backlog"), 1);
    assert_eq!(f.health()["ok"], true);
}
#[test]
fn a_failed_recipient_keeps_its_events_without_blocking_other_groups() {
    let f = fixture();
    let team = "example-team/widgets";
    f.write(
        "status",
        &json!([checkout(&f, team, "team", "first"), lead(team, "lead")]).to_string(),
    );
    changed(
        &f,
        &[(
            true,
            vec![event("PullRequestReview", "PRR_isolated", 202, "")],
        )],
    );
    f.write("fail-recipient", "example-team/root");
    f.write("fail", "");
    drive(&f, "120");
    assert!(
        f.detail().contains("example-team/root:")
            && f.detail().contains("example-team/widgets/lead:")
    );
    assert_eq!(pending(&f), 2);
    fs::remove_file(f.path("fail")).unwrap();
    drive(&f, "180");
    assert_eq!(pending(&f), 1);
    assert_eq!(f.health()["ok"], false);
    fs::remove_file(f.path("fail-recipient")).unwrap();
    drive(&f, "240");
    assert_eq!(pending(&f), 0);
    assert_eq!(reads(&text(&f), "PRR_isolated"), 5);
    assert_eq!(f.health()["ok"], true);
}

#[test]
fn maximum_identifiers_and_a_long_routing_reason_still_fit_one_notice() {
    let f = fixture();
    let repo = format!("{}/{}", "a".repeat(39), "b".repeat(100));
    let team = format!("example-team/{}", "c".repeat(210));
    let config = f.read("config.yaml").replace("example-org/widgets", &repo);
    f.write("config.yaml", &config);
    f.write(
        "status",
        &json!([checkout(&f, &team, "team", "first"), lead(&team, "lead")]).to_string(),
    );
    let mut review = event("PullRequestReview", &"R".repeat(128), 202, "");
    review["state"] = json!("A".repeat(32));
    review["commit"] = Value::Null;
    let mut raw = item(true, vec![review]);
    raw["headRepository"] = json!({"nameWithOwner":repo});
    answer(&f, u64::MAX, &raw);
    f.response(
        "issues",
        200,
        &json!([{"node_id":"I_1","number":u64::MAX,"updated_at":AT}]).to_string(),
    );
    drive(&f, "120");
    let sent = deliveries(&f);
    assert_eq!(sent.len(), 1);
    assert!(sent[0].split_once('\n').unwrap().1.len() <= 900);
    assert!(sent[0].contains('…'));
    assert_eq!(pending(&f), 0);
}

#[test]
fn short_downtime_replays_once_and_new_repositories_start_at_restart() {
    let f = fixture();
    let kept = event("AssignedEvent", "AE_kept", 202, "");
    changed(&f, &[(false, vec![kept.clone()])]);
    drive(&f, "0");
    f.write("epoch", "2026-01-02T12:02:00Z");
    let mut action = event("IssueComment", "IC_downtime", 202, "@example-bot");
    action["createdAt"] = json!("2026-01-02T12:01:00Z");
    changed(&f, &[(false, vec![kept, action])]);
    let mut new = item(false, vec![event("AssignedEvent", "AE_new", 202, "")]);
    new["id"] = json!("I_2");
    answer(&f, 2, &new);
    let listed = json!([{"node_id":"I_2","number":2,"updated_at":AT}]);
    f.response("issues-gadgets", 200, &listed.to_string());
    add_gadgets(&f);
    drive(&f, "0,60");
    assert_eq!(reads(&text(&f), "AE_kept"), 1);
    assert_eq!(reads(&text(&f), "IC_downtime"), 1);
    assert!(!text(&f).contains("example-org/gadgets#"));
    assert!(!text(&f).contains("intake was not running"));
    assert!(f.detail().contains("2026-01-02T12:00:00.000Z"));
}
fn add_gadgets(f: &Fixture) {
    let config = f.read("config.yaml");
    f.write(
        "config.yaml",
        &config.replace("]", ", example-org/gadgets]"),
    );
}
#[test]
fn long_downtime_bounds_replay_and_journals_one_root_notice_on_restart_or_turn_on() {
    for turn_on in [false, true] {
        let f = fixture();
        let old = parse_utc("2025-12-20T12:00:00Z").unwrap();
        let cursor = brg_github::state::Cursor::new(old);
        let mut backlog = brg_github::state::Cursor::new(old - Duration::from_secs(86400));
        backlog.at = parse_utc(AT).unwrap();
        let admitted = parse_utc("2025-12-30T12:00:00Z").unwrap();
        f.write("plugins/github/intake.json", &json!({"cursors":{"example-org/widgets":cursor,"example-org/gadgets":backlog},"seen":{"IC_kept":admitted},"pending":[]}).to_string());
        add_gadgets(&f);
        let mut nodes = Vec::new();
        for (id, at) in [
            ("IC_old", "2025-12-30T11:59:59Z"),
            ("IC_window", "2025-12-30T12:00:00Z"),
            ("IC_kept", "2025-12-30T12:00:00Z"),
        ] {
            let mut e = event("IssueComment", id, 202, "PRIVATE_MARKER @example-bot");
            e["createdAt"] = json!(at);
            nodes.push(e);
        }
        changed(&f, &[(false, nodes)]);
        // Keep discovery undrained across polls, so replay relists an admitted action.
        let listed = json!({"node_id":"I_1","number":1,"updated_at":AT});
        f.response("issues", 200, &json!(vec![listed; 20]).to_string());
        if turn_on {
            // Off until the next poll, which then runs at AT like the restart arm.
            f.write("epoch", "2026-01-02T11:59:00Z");
            fs::rename(f.path("config.yaml"), f.path("next-config.yaml")).unwrap();
        }
        drive(&f, "0,60,120");
        let sent = text(&f);
        assert_eq!(reads(&sent, "IC_window"), 1);
        assert_eq!(reads(&sent, "IC_old") + reads(&sent, "IC_kept"), 0);
        assert_eq!(sent.matches("intake was not running").count(), 1, "{sent}");
        let notice =
            "example-team/root\nGitHub: intake was not running between 2025-12-19T12:00:00.000Z";
        assert!(sent.contains(notice), "{sent}");
        assert!(!sent.contains("PRIVATE_MARKER"));
        assert!(f.read("journal-at-send").contains("intake was not running"));
        assert!(
            f.detail()
                .contains("before 2025-12-30T12:00:00.000Z is not")
        );
    }
}
const EYES: &str = "query=mutation($id: ID!) { addReaction(input: {subjectId: $id, content: EYES}) { reaction { content } } }\n";
/// The subject of each eyes reaction, in call order.
fn eyes(calls: &str) -> Vec<&str> {
    let ids = calls.split("--end--\n").filter(|call| call.contains(EYES));
    ids.map(|call| call.lines().find_map(|s| s.strip_prefix("id=")).unwrap())
        .collect()
}
#[test]
fn received_comments_and_reviews_get_one_eyes_each_without_bursts() {
    let f = fixture();
    let mut inline = event("PullRequestReview", "PRR_inline", 202, "Review summary");
    inline["comments"] = json!({"nodes":[content(202, "@example-bot")]});
    let comment = event("IssueComment", "IC_received", 202, "@example-bot");
    let assigned = event("AssignedEvent", "AE_received", 202, "");
    changed(
        &f,
        &[
            (false, vec![comment, assigned]),
            (true, vec![inline.clone()]),
            (true, vec![event("MergedEvent", "ME_received", 202, "")]),
            (false, vec![]),
        ],
    );
    let mut raw = item(true, vec![inline]);
    raw["id"] = json!("I_2");
    raw["assignees"] = json!({"nodes":[]});
    answer(&f, 2, &raw);
    let mut body = item(false, vec![]);
    body["id"] = json!("I_4");
    body["body"] = json!("@example-bot");
    answer(&f, 4, &body);
    drive(&f, "0");
    assert_eq!(eyes(&f.read("calls-at-0")).len(), 1);
    fs::rename(f.path("config.yaml"), f.path("disabled")).unwrap();
    drive(&f, "60");
    fs::rename(f.path("disabled"), f.path("config.yaml")).unwrap();
    f.response("machine", 200, r#"{"id":202,"login":"example-owner"}"#);
    drive(&f, "120");
    assert_eq!(eyes(&f.calls()).len(), 1);
    f.response("machine", 200, r#"{"id":101,"login":"example-bot"}"#);
    drive(&f, "180,240"); // Restart retains the second queued reaction.
    assert_eq!(eyes(&f.calls()), ["IC_received", "PRR_inline"]);
    drive(&f, "300"); // A wrongly queued merge, assignment or body would react here.
    assert_eq!(eyes(&f.calls()), ["IC_received", "PRR_inline"]);
    for id in ["AE_received", "ME_received", "I_4"] {
        assert_eq!(reads(&text(&f), id), 1, "{id}");
    }
}
#[test]
fn no_eyes_until_the_owning_leads_notice_commits_and_its_approval_copy_never_reacts() {
    let f = fixture();
    let team = "example-team/widgets";
    f.write(
        "status",
        &json!([checkout(&f, team, "team", "first"), lead(team, "lead")]).to_string(),
    );
    let review = event("PullRequestReview", "PRR_delayed", 202, "");
    changed(&f, &[(true, vec![review])]);
    f.write("fail-recipient", "example-team/widgets/lead");
    drive(&f, "0,60");
    assert!(eyes(&f.calls()).is_empty());
    fs::remove_file(f.path("fail-recipient")).unwrap();
    drive(&f, "120,180");
    assert_eq!(eyes(&f.calls()), ["PRR_delayed"]);
    assert_eq!(pending(&f), 0);
}
#[test]
fn a_failed_eyes_is_visible_without_retrying_or_resending_the_notice() {
    let f = fixture();
    let comment = event("IssueComment", "IC_failed", 202, "@example-bot");
    changed(&f, &[(false, vec![comment])]);
    f.response("reaction", 403, "{}");
    drive(&f, "0");
    assert_eq!(eyes(&f.calls()), ["IC_failed"]);
    assert_eq!(f.health()["ok"], false);
    assert!(f.detail().contains("IC_failed"));
    drive(&f, "60,120");
    assert_eq!(eyes(&f.calls()), ["IC_failed"]);
    assert_eq!(reads(&text(&f), "IC_failed"), 1);
}
#[test]
fn rate_limited_eyes_pauses_api_calls_then_retries_without_resending_the_notice() {
    let f = fixture();
    let comment = event("IssueComment", "IC_limited", 202, "@example-bot");
    changed(&f, &[(false, vec![comment])]);
    f.response("reaction", 429, "{}");
    f.response("next-reaction", 200, "{}");
    let resume = parse_utc(AT).unwrap().elapsed().unwrap().as_secs() + 660;
    drive(&f, &format!("0,60,{resume}"));
    assert_eq!(f.read("calls-at-0"), f.read("calls-at-60"));
    assert_eq!(eyes(&f.calls()), ["IC_limited", "IC_limited"]);
    assert_eq!(reads(&text(&f), "IC_limited"), 1);
}
