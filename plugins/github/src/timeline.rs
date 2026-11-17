//! Paged timelines and API-established owner actions; GitHub text stays here.
use crate::{
    config::Guard,
    discover::Changed,
    gh::{Api, Error},
    state::{Kind, PendingEvent},
};
use borgchart::bus::time::{format_utc, parse_utc};
use serde_json::Value;
use std::time::SystemTime;

const QUERY: &str = r#"query($owner: String!, $name: String!, $number: Int!, $since: DateTime!, $after: String) {
 repository(owner: $owner, name: $name) { issueOrPullRequest(number: $number) {
  __typename
  ... on Issue { id author { ...actor } body createdAt lastEditedAt editor { ...actor }
   assignees(first: 20) { nodes { databaseId } }
   timelineItems(since: $since, first: 100, after: $after, itemTypes: [ISSUE_COMMENT, ASSIGNED_EVENT]) {
    nodes { __typename
     ... on IssueComment { ...comment }
     ... on AssignedEvent { ...assignment }
    } pageInfo { hasNextPage endCursor }
   }
  }
  ... on PullRequest { id author { ...actor } body createdAt lastEditedAt editor { ...actor }
   assignees(first: 20) { nodes { databaseId } } headRefName headRefOid
   timelineItems(since: $since, first: 100, after: $after, itemTypes: [ISSUE_COMMENT, PULL_REQUEST_REVIEW, ASSIGNED_EVENT, MERGED_EVENT]) {
    nodes { __typename
     ... on IssueComment { ...comment }
     ... on AssignedEvent { ...assignment }
     ... on PullRequestReview { id author { ...actor } state submittedAt commit { oid } body lastEditedAt editor { ...actor }
      comments(first: 50) { nodes { author { ...actor } body lastEditedAt editor { ...actor } } } }
     ... on MergedEvent { id actor { ...actor } commit { oid } createdAt }
    } pageInfo { hasNextPage endCursor }
   }
  }
 } }
}
fragment actor on Actor { ... on User { databaseId } ... on Bot { databaseId } }
fragment comment on IssueComment { id author { ...actor } body createdAt lastEditedAt editor { ...actor } }
fragment assignment on AssignedEvent { id actor { ...actor } assignee { ... on User { databaseId } } createdAt }
"#;
fn malformed() -> Error {
    Error::Other(
        "GitHub returned an invalid timeline; check gh and retry without advancing the cursor"
            .into(),
    )
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, Error> {
    value.get(key).and_then(Value::as_str).ok_or_else(malformed)
}
fn nodes(value: &Value) -> Result<&Vec<Value>, Error> {
    value["nodes"].as_array().ok_or_else(malformed)
}
fn node_id(value: &Value) -> Result<&str, Error> {
    let id = string(value, "id")?;
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err(malformed());
    }
    Ok(id)
}
fn oid(value: &Value) -> Result<String, Error> {
    let oid = string(&value["commit"], "oid")?;
    if !(7..=64).contains(&oid.len()) || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(malformed());
    }
    Ok(oid.into())
}
fn account(value: &Value) -> Option<u64> {
    value["databaseId"].as_u64()
}
fn mentions(value: &Value, guard: &Guard) -> Result<bool, Error> {
    let body = string(value, "body")?;
    let edited = value.get("lastEditedAt").ok_or_else(malformed)?;
    if !edited.is_null() && edited.as_str().and_then(parse_utc).is_none() {
        return Err(malformed());
    }
    Ok(account(&value["author"]) == Some(guard.owner_id)
        && (edited.is_null() || account(&value["editor"]) == Some(guard.owner_id))
        && mention(body, &guard.machine_login))
}
/// Ignore code spans, fenced blocks and quoted lines before matching login boundaries.
fn mention(body: &str, login: &str) -> bool {
    let needle = format!("@{}", login.to_ascii_lowercase());
    let login_char = |b: u8| b.is_ascii_alphanumeric() || b == b'-';
    let mut fence: Option<(u8, usize)> = None;
    let mut text = String::new();
    for line in body.lines() {
        let trimmed = line.trim_start();
        let bytes = trimmed.as_bytes();
        let run = bytes
            .first()
            .map_or(0, |b| bytes.iter().take_while(|c| *c == b).count());
        if let Some((ch, len)) = fence {
            if bytes.first() == Some(&ch) && run >= len && trimmed[run..].trim().is_empty() {
                fence = None;
            }
            continue;
        }
        if trimmed.starts_with('>') {
            continue;
        }
        if run >= 3 && matches!(bytes.first(), Some(b'`' | b'~')) {
            fence = Some((bytes[0], run));
            continue;
        }
        text.push_str(line);
        text.push('\n');
    }
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let count = bytes[i..].iter().take_while(|b| **b == b'`').count();
            i += count;
            let mut end = i;
            while end < bytes.len() {
                if bytes[end] != b'`' {
                    end += 1;
                    continue;
                }
                let close = bytes[end..].iter().take_while(|b| **b == b'`').count();
                end += close;
                if close == count {
                    i = end;
                    break;
                }
            }
            continue;
        }
        if bytes[i..].starts_with(needle.as_bytes())
            && (i == 0 || !login_char(bytes[i - 1]))
            && bytes.get(i + needle.len()).is_none_or(|b| !login_char(*b))
        {
            return true;
        }
        i += 1;
    }
    false
}
pub fn fetch(
    api: &impl Api,
    item: &Changed,
    floor: SystemTime,
    since: SystemTime,
    guard: &Guard,
) -> Result<Vec<PendingEvent>, Error> {
    let (owner, name) = item.repository.split_once('/').ok_or_else(malformed)?;
    let since_text = format_utc(since);
    let mut after = String::new();
    let mut events = Vec::new();
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
        if pr != item.is_pull_request || node_id(raw)? != item.node_id {
            return Err(malformed());
        }
        let assigned = nodes(&raw["assignees"])?
            .iter()
            .any(|a| account(a) == Some(guard.machine_id));
        let timeline = &raw["timelineItems"];
        for value in std::iter::once(raw).chain(nodes(timeline)?.iter()) {
            let id = node_id(value)?;
            let name = string(value, "__typename")?;
            if name == "PullRequestReview"
                && value["state"] == "PENDING"
                && value.get("submittedAt") == Some(&Value::Null)
            {
                continue;
            }
            let time = string(
                value,
                if name == "PullRequestReview" {
                    "submittedAt"
                } else {
                    "createdAt"
                },
            )?;
            let at = parse_utc(time).ok_or_else(malformed)?;
            if at < floor || at < since {
                continue;
            }
            let owner = account(&value["actor"]) == Some(guard.owner_id);
            let kind = match name {
                "Issue" | "PullRequest" | "IssueComment" => {
                    mentions(value, guard)?.then_some(Kind::Mention)
                }
                "AssignedEvent" => {
                    (!pr && owner && account(&value["assignee"]) == Some(guard.machine_id))
                        .then_some(Kind::Assigned)
                }
                "MergedEvent" => {
                    let commit = oid(value)?;
                    (pr && assigned && owner).then_some(Kind::Merged { commit })
                }
                "PullRequestReview" => {
                    let state = string(value, "state")?;
                    if ![
                        "APPROVED",
                        "CHANGES_REQUESTED",
                        "COMMENTED",
                        "DISMISSED",
                        "PENDING",
                    ]
                    .contains(&state)
                    {
                        return Err(malformed());
                    }
                    let commit = oid(value)?;
                    let mut mentioned = mentions(value, guard)?;
                    for comment in nodes(&value["comments"])? {
                        mentioned |= mentions(comment, guard)?;
                    }
                    if account(&value["author"]) != Some(guard.owner_id) {
                        None
                    } else if pr && assigned {
                        Some(Kind::Review {
                            state: state.into(),
                            commit,
                        })
                    } else {
                        mentioned.then_some(Kind::Mention)
                    }
                }
                _ => return Err(malformed()),
            };
            if let Some(kind) = kind {
                events.push(PendingEvent {
                    id: id.into(),
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
            return Ok(events);
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
    fn unmatched_backticks_do_not_hide_mentions_but_closed_spans_and_fences_do() {
        for (body, expected) in [
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
