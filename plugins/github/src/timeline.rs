//! Paged timelines and API-established owner actions; GitHub text stays here.
use crate::{
    config::Guard,
    delivery::Heads,
    discover::Changed,
    gh::{Api, Error},
    state::{Kind, PendingEvent},
};
use borgchart::bus::time::{format_utc, parse_utc};
use serde_json::Value;
use std::{ops::RangeInclusive, time::SystemTime};

const QUERY: &str = r#"query($owner: String!, $name: String!, $number: Int!, $since: DateTime!, $after: String) {
 repository(owner: $owner, name: $name) { issueOrPullRequest(number: $number) {
  __typename
  ... on Issue { id author { ...actor } body createdAt lastEditedAt editor { ...actor }
   assignees(first: 20) { nodes { databaseId } }
   closedByPullRequestsReferences(first: 10) { nodes { headRefName headRefOid headRepository { nameWithOwner } } }
   timelineItems(since: $since, first: 100, after: $after, itemTypes: [ISSUE_COMMENT, ASSIGNED_EVENT]) {
    nodes { __typename
     ... on IssueComment { ...comment }
     ... on AssignedEvent { ...assignment }
    } pageInfo { hasNextPage endCursor }
   }
  }
  ... on PullRequest { id author { ...actor } body createdAt lastEditedAt editor { ...actor }
   assignees(first: 20) { nodes { databaseId } } headRefName headRefOid headRepository { nameWithOwner }
   reviews(last: 100) { nodes { __typename ...review } }
   timelineItems(since: $since, first: 100, after: $after, itemTypes: [ISSUE_COMMENT, ASSIGNED_EVENT, MERGED_EVENT]) {
    nodes { __typename
     ... on IssueComment { ...comment }
     ... on AssignedEvent { ...assignment }
     ... on MergedEvent { id actor { ...actor } commit { oid } createdAt }
    } pageInfo { hasNextPage endCursor }
   }
  }
 } }
}
fragment actor on Actor { ... on User { databaseId } ... on Bot { databaseId } }
fragment comment on IssueComment { id author { ...actor } body createdAt lastEditedAt editor { ...actor } }
fragment assignment on AssignedEvent { id actor { ...actor } assignee { ... on User { databaseId } } createdAt }
fragment review on PullRequestReview { id author { ...actor } state submittedAt commit { oid } body lastEditedAt editor { ...actor }
 comments(first: 50) { nodes { id author { ...actor } body lastEditedAt editor { ...actor } } } }
"#;
fn malformed() -> Error {
    Error::Other(
        "GitHub returned an invalid timeline; check gh and retry without advancing the cursor"
            .into(),
    )
}
pub(crate) fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, Error> {
    value.get(key).and_then(Value::as_str).ok_or_else(malformed)
}
fn nodes(value: &Value) -> Result<&Vec<Value>, Error> {
    value["nodes"].as_array().ok_or_else(malformed)
}
/// Text that goes into a notice: bounded, from a fixed character set.
pub(crate) fn checked(
    text: &str,
    len: RangeInclusive<usize>,
    allowed: fn(u8) -> bool,
) -> Result<&str, Error> {
    (len.contains(&text.len()) && text.bytes().all(allowed))
        .then_some(text)
        .ok_or_else(malformed)
}
/// A node ID as notices name it and `read` accepts it.
pub fn valid_id(id: &str) -> bool {
    let allowed = |b: u8| b.is_ascii_alphanumeric() || b"_-".contains(&b);
    checked(id, 1..=128, allowed).is_ok()
}
fn node_id(value: &Value) -> Result<&str, Error> {
    let id = string(value, "id")?;
    valid_id(id).then_some(id).ok_or_else(malformed)
}
pub(crate) fn oid(value: &Value) -> Result<Option<String>, Error> {
    if value.get("commit") == Some(&Value::Null) {
        return Ok(None);
    }
    let oid = string(&value["commit"], "oid")?;
    Ok(Some(
        checked(oid, 7..=64, |b| b.is_ascii_hexdigit())?.into(),
    ))
}
pub(crate) fn account(value: &Value) -> Option<u64> {
    value["databaseId"].as_u64()
}
pub(crate) fn review_state(value: &Value) -> Result<&str, Error> {
    checked(string(value, "state")?, 1..=32, |b| {
        b.is_ascii_uppercase() || b == b'_'
    })
}
/// Owner-authored, never edited, and mentioning the machine.
fn mentions(value: &Value, guard: &Guard) -> Result<bool, Error> {
    let body = string(value, "body")?;
    let unedited = value.get("lastEditedAt") == Some(&Value::Null);
    Ok(account(&value["author"]) == Some(guard.owner_id)
        && unedited
        && mention(body, &guard.machine_login))
}
/// Drop fenced blocks, quoted lines and closed code spans, then match `@login` at
/// login-character boundaries, case-insensitively.
fn mention(body: &str, login: &str) -> bool {
    let mut fence: Option<(char, usize)> = None;
    let mut prose = String::new();
    for line in body.lines() {
        let t = line.trim_start();
        let first = t.chars().next().unwrap_or(' ');
        let run = t.len() - t.trim_start_matches(first).len();
        if let Some((ch, len)) = fence {
            if first == ch && run >= len && t[run..].trim().is_empty() {
                fence = None;
            }
        } else if run >= 3 && (first == '`' || first == '~') {
            fence = Some((first, run));
        } else if first != '>' {
            prose.push_str(&line.to_ascii_lowercase());
            prose.push('\n');
        }
    }
    let b = prose.as_bytes();
    let ticks = |i: usize| b[i..].iter().take_while(|&&c| c == b'`').count();
    let mut text = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let run = ticks(i);
        let close = (run > 0)
            .then(|| (i + run..b.len()).find(|&j| b[j - 1] != b'`' && ticks(j) == run))
            .flatten();
        if let Some(j) = close {
            text.push(b' ');
            i = j + run;
        } else {
            text.extend_from_slice(&b[i..i + run.max(1)]);
            i += run.max(1);
        }
    }
    let needle = format!("@{}", login.to_ascii_lowercase());
    let login_char = |c: Option<&u8>| c.is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'-');
    let text = String::from_utf8_lossy(&text);
    text.match_indices(&needle).any(|(i, _)| {
        !login_char(i.checked_sub(1).and_then(|p| text.as_bytes().get(p)))
            && !login_char(text.as_bytes().get(i + needle.len()))
    })
}
pub fn fetch(
    api: &impl Api,
    item: &Changed,
    floor: SystemTime,
    since: SystemTime,
    guard: &Guard,
) -> Result<(Vec<PendingEvent>, Heads), Error> {
    let (owner, name) = item.repository.split_once('/').ok_or_else(malformed)?;
    let since_text = format_utc(since);
    let mut after = String::new();
    let mut events = Vec::new();
    let mut heads = Vec::new();
    loop {
        let mut strings = vec![
            ("owner", owner),
            ("name", name),
            ("since", since_text.as_str()),
        ];
        if !after.is_empty() {
            strings.push(("after", after.as_str()));
        }
        let answer = api.graphql(QUERY, &strings, &[("number", item.number)])?;
        let raw = &answer["data"]["repository"]["issueOrPullRequest"];
        let pr = match string(raw, "__typename")? {
            "Issue" => false,
            "PullRequest" => true,
            _ => return Err(malformed()),
        };
        if node_id(raw)? != item.node_id {
            return Err(malformed());
        }
        if after.is_empty() {
            let linked: &[Value] = if pr {
                std::slice::from_ref(raw)
            } else {
                nodes(&raw["closedByPullRequestsReferences"])?
            };
            for head in linked {
                heads.push((
                    string(head, "headRefName")?.into(),
                    string(head, "headRefOid")?.into(),
                    head["headRepository"]["nameWithOwner"]
                        .as_str()
                        .is_some_and(|repo| repo.eq_ignore_ascii_case(&item.repository)),
                ));
            }
        }
        let assigned = nodes(&raw["assignees"])?
            .iter()
            .any(|a| account(a) == Some(guard.machine_id));
        let machine_authored = pr && account(&raw["author"]) == Some(guard.machine_id);
        let timeline = &raw["timelineItems"];
        let body = std::iter::once(raw).filter(|_| after.is_empty());
        let reviews: &[Value] = if pr && after.is_empty() {
            nodes(&raw["reviews"])?
        } else {
            &[]
        };
        for value in body.chain(nodes(timeline)?).chain(reviews) {
            let name = string(value, "__typename")?;
            let by = account(&value["actor"]).or(account(&value["author"]));
            if by != Some(guard.owner_id) {
                continue;
            }
            let review = name == "PullRequestReview";
            let time = &value[if review { "submittedAt" } else { "createdAt" }];
            if review && time.is_null() {
                continue; // an unsubmitted draft
            }
            let at = time.as_str().and_then(parse_utc).ok_or_else(malformed)?;
            if at < floor || at < since {
                continue;
            }
            let kind = match name {
                "Issue" | "PullRequest" | "IssueComment" => {
                    mentions(value, guard)?.then_some(Kind::Mention)
                }
                "AssignedEvent" => (!pr && account(&value["assignee"]) == Some(guard.machine_id))
                    .then_some(Kind::Assigned),
                "MergedEvent" if assigned || machine_authored => Some(Kind::Merged {
                    commit: oid(value)?,
                }),
                "PullRequestReview" => {
                    let mut admitted = assigned || mentions(value, guard)?;
                    if !assigned {
                        for comment in nodes(&value["comments"])? {
                            admitted |= mentions(comment, guard)?;
                        }
                    }
                    if admitted {
                        Some(Kind::Review {
                            state: review_state(value)?.into(),
                            commit: oid(value)?,
                        })
                    } else {
                        None
                    }
                }
                "MergedEvent" => None,
                _ => return Err(malformed()),
            };
            if let Some(kind) = kind {
                let mut reaction = None;
                if review
                    && string(value, "body")?.trim().is_empty()
                    && let Some(reply) = nodes(&value["comments"])?.first()
                {
                    reaction = Some(node_id(reply)?.into()); // no review box; react on its first comment
                }
                events.push(PendingEvent {
                    id: node_id(value)?.into(),
                    reactable: matches!(name, "IssueComment" | "PullRequestReview"),
                    reaction,
                    repository: item.repository.clone(),
                    number: item.number,
                    is_pull_request: pr,
                    kind,
                    recipient: String::new(),
                    reason: String::new(),
                });
            }
        }
        if !timeline["pageInfo"]["hasNextPage"]
            .as_bool()
            .ok_or_else(malformed)?
        {
            return Ok((events, heads));
        }
        let next = string(&timeline["pageInfo"], "endCursor")?;
        if next.is_empty() || next == after {
            return Err(malformed());
        }
        after = next.into();
    }
}

#[cfg(test)]
mod tests {
    use super::mention;
    #[test]
    fn mentions_need_login_boundaries_outside_code_and_quotes() {
        for (body, expected) in [
            ("@Example-Bot, please look", true),
            ("plain text", false),
            ("@example-bot2", false),
            ("me@example-bot", false),
            ("```rust\n@example-bot\n```", false),
            ("~~~\n@example-bot\n~~~", false),
            ("`@example-bot`", false),
            ("``@example-bot``", false),
            ("  > @example-bot", false),
            ("an unmatched ` before @example-bot", true),
            ("`span\n@example-bot`", false),
            ("``a ` and @example-bot``", false),
            ("```\n@example-bot\n```\n@Example-Bot,", true),
            ("~~~\n@example-bot\n~~~~\n@example-bot", true),
        ] {
            assert_eq!(mention(body, "example-bot"), expected, "{body}");
        }
    }
}
