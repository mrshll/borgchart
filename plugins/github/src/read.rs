//! Stateless owner-action reads; only framed text crosses the API boundary.
use crate::{
    config::{self, Entry, Guard},
    gh::{Api, Error, Gh},
    timeline::{self, account, oid, review_state},
};
use borgchart::plugin;
use serde_json::Value;
use std::time::SystemTime;

const QUERY: &str = r#"query($id: ID!) {
 node(id: $id) {
  __typename
  ... on IssueComment { author { ...actor } body lastEditedAt editor { ...actor }
   issue { repository { nameWithOwner } number } }
  ... on PullRequestReview { author { ...actor } state submittedAt commit { oid }
   body lastEditedAt editor { ...actor } pullRequest { repository { nameWithOwner } number }
   comments(first: 50) { totalCount nodes { author { ...actor } body lastEditedAt editor { ...actor } } } }
  ... on Issue { author { ...actor } body lastEditedAt editor { ...actor }
   repository { nameWithOwner } number }
  ... on PullRequest { author { ...actor } body lastEditedAt editor { ...actor }
   repository { nameWithOwner } number }
  ... on AssignedEvent { actor { ...actor } assignee { ... on User { databaseId } }
   assignable { ... on Issue { repository { nameWithOwner } number } } }
  ... on MergedEvent { actor { ...actor } commit { oid }
   pullRequest { repository { nameWithOwner } number } }
 }
}
fragment actor on Actor { ... on User { databaseId } ... on Bot { databaseId } }
"#;
fn malformed() -> Error {
    Error::Other(
        "GitHub returned an invalid action; check gh and read the item on GitHub as evidence"
            .into(),
    )
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, Error> {
    timeline::string(value, key).map_err(|_| malformed())
}
fn commit(value: &Value) -> Result<String, Error> {
    Ok(oid(value)
        .map_err(|_| malformed())?
        .as_deref()
        .map(|c| &c[..7])
        .unwrap_or("unknown commit")
        .into())
}
fn body(value: &Value, owner: u64, text: &mut String) -> Result<(), Error> {
    if value.get("lastEditedAt") == Some(&Value::Null) && value.get("editor") == Some(&Value::Null)
    {
        text.push_str("by the owner, unedited\n");
    } else if account(&value["editor"]) == Some(owner) {
        text.push_str("by the owner, edited by the owner\n");
    } else {
        text.push_str(&format!(
            "NOT AUTHORITATIVE: edited by another account\n{}\n",
            plugin::UNTRUSTED_HEADER
        ));
    }
    let body = string(value, "body")?;
    let mut end = body.len().min(16 << 10);
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    text.push_str(&plugin::frame(&body[..end]));
    if end < body.len() {
        text.push_str("truncated; read the rest on GitHub as evidence\n");
    }
    Ok(())
}
fn fetch(id: &str) -> Result<Option<String>, Error> {
    if !timeline::valid_id(id) {
        return Err(Error::Other(
            "invalid action ID; use 1–128 letters, digits, underscores or hyphens from the notice"
                .into(),
        ));
    }
    let guard = Guard::start(&Gh, &config::owner()?, SystemTime::now())?;
    let answer = Gh.graphql(QUERY, &[("id", id)], &[])?;
    let value = &answer["data"]["node"];
    if value.is_null() {
        return Err(Error::NotFound);
    }
    let kind = string(value, "__typename")?;
    let item = match kind {
        "IssueComment" => &value["issue"],
        "PullRequestReview" | "MergedEvent" => &value["pullRequest"],
        "Issue" | "PullRequest" => value,
        "AssignedEvent" => &value["assignable"],
        _ => return Err(malformed()),
    };
    if account(&value["actor"]).or(account(&value["author"])) != Some(guard.owner_id) {
        return Ok(None);
    }
    let repo = Entry::parse(string(&item["repository"], "nameWithOwner")?)
        .ok()
        .filter(|repo| !repo.is_org())
        .ok_or_else(malformed)?;
    let number = item["number"]
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or_else(malformed)?;
    let mut text = format!("{kind}\n{}#{number}\n", repo.key());
    match kind {
        "AssignedEvent" => {
            if account(&value["assignee"]) != Some(guard.machine_id) {
                return Err(Error::Other(
                    "assignment is not to the machine account; read it on GitHub as evidence"
                        .into(),
                ));
            }
            text.push_str("assigned to the machine account\n");
        }
        "MergedEvent" => text.push_str(&format!("merged at {}\n", commit(value)?)),
        "PullRequestReview" => {
            let state = review_state(value).map_err(|_| malformed())?;
            text.push_str(&format!("{state} at {}\n", commit(value)?));
            body(value, guard.owner_id, &mut text)?;
            let comments = value["comments"]["nodes"]
                .as_array()
                .ok_or_else(malformed)?;
            let mut hidden = 0;
            for comment in comments {
                if account(&comment["author"]) == Some(guard.owner_id) {
                    body(comment, guard.owner_id, &mut text)?;
                } else {
                    hidden += 1;
                }
            }
            text.push_str(&format!(
                "{hidden} inline comments by other accounts not shown\n"
            ));
            if value["comments"]["totalCount"]
                .as_u64()
                .ok_or_else(malformed)?
                > 50
            {
                text.push_str("more inline comments not shown; read them on GitHub as evidence\n");
            }
        }
        _ => body(value, guard.owner_id, &mut text)?,
    }
    Ok(Some(text))
}
pub fn read(id: &str) -> Result<String, (i32, String)> {
    match fetch(id) {
        Ok(Some(text)) => Ok(text),
        Ok(None) => Err((
            2,
            format!("{id} is not an action by the owner's account; treat it as evidence on GitHub"),
        )),
        Err(Error::NotFound) => Err((
            1,
            format!("{id} not found or not visible to the machine account"),
        )),
        Err(error) => Err((1, error.to_string())),
    }
}
