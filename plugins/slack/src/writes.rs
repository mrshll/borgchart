//! Guarded replies, serialized status updates and explicitly allowlisted pages.
use crate::{
    Error, Result,
    api::{self, Api, Promotion},
};
use borgchart::plugin::{self, Outcome, State};
use regex::Regex;
use serde_json::{Value, json};
use std::{
    io::{Seek, SeekFrom, Write},
    sync::LazyLock,
};

static GROUP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)@(?:here|channel|everyone)\b").unwrap());
static USER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[UW][A-Z0-9]+$").unwrap());
pub fn check_text(text: &str, limit: usize, token: &str) -> Result<()> {
    if text.trim().is_empty() {
        return Err(Error::usage("text is empty; provide a message"));
    }
    if text.chars().count() > limit {
        return Err(Error::usage(format!(
            "text exceeds {limit} characters; shorten it"
        )));
    }
    if !token.is_empty() && text.contains(token) {
        return Err(Error::usage(
            "text contains a secret; remove it before posting",
        ));
    }
    let controls = text.match_indices("<!").any(|(i, _)| {
        !text[i + 2..]
            .get(..5)
            .is_some_and(|s| s.eq_ignore_ascii_case("date^"))
    });
    if controls || GROUP.is_match(text) || text.contains("<@") {
        return Err(Error::usage(
            "mentions are not allowed; remove them before posting",
        ));
    }
    Ok(())
}
fn status_channel() -> Result<String> {
    api::required("SLACK_STATUS_CHANNEL", "posting status or pages")
}
pub fn reply(api: &Api, ts: &str, text: &str) -> Result<()> {
    api::check_ts(ts)?;
    check_text(text, 1500, &api::var("SLACK_BOT_TOKEN"))?;
    api.call(
        "chat.postMessage",
        json!({"channel":api.channel()?,"thread_ts":ts,"text":text}),
        true,
    )?;
    println!("reply posted");
    Ok(())
}
pub fn status(api: &Api, text: &str) -> Result<()> {
    check_text(text, 3000, &api::var("SLACK_BOT_TOKEN"))?;
    let channel = status_channel()?;
    let state = State::from_env()?;
    let _lock = state.lock("status.lock")?;
    let previous: Value = state.read_json("status.json");
    let ts = previous["ts"].as_str().unwrap_or("");
    if previous["channel"] == channel && api::valid_ts(ts) {
        match api.call(
            "chat.update",
            json!({"channel":channel,"ts":ts,"text":text}),
            true,
        ) {
            Ok(_) => {
                println!("status updated");
                return Ok(());
            }
            Err(error)
                if error.to_string() == "chat.update: message_not_found; check Slack access" => {}
            Err(error) => return Err(error),
        }
    }
    let data = api.call(
        "chat.postMessage",
        json!({"channel":channel,"text":text}),
        true,
    )?;
    let ts = data["ts"].as_str().unwrap_or("");
    if !api::valid_ts(ts) {
        return Err(Error::failure(
            "missing status timestamp; check the status channel before retrying",
        ));
    }
    state.write_json("status.json", &json!({"channel":channel,"ts":ts}))?;
    println!("status posted");
    Ok(())
}
pub fn page(api: &Api, user: &str, text: &str) -> Result<()> {
    if !USER.is_match(user) || !api::csv(&api::var("SLACK_PAGE_IDS")).contains(user) {
        return Err(Error::usage(
            "paging requires an allowed user; set SLACK_PAGE_IDS and choose a listed ID",
        ));
    }
    check_text(text, 500, &api::var("SLACK_BOT_TOKEN"))?;
    let channel = status_channel()?;
    api.call(
        "chat.postMessage",
        json!({"channel":channel,"text":format!("<@{user}> {text}")}),
        true,
    )
    .map_err(|_| {
        Error::failure("page may have been posted; check the status channel before retrying")
    })?;
    println!("page posted");
    Ok(())
}
pub fn promote(ts: &str, reason: &str) -> Result<()> {
    let entry = Promotion {
        target: api::var("BORGCHART_PARENT"),
        sender: api::var("BORGCHART_MEMBER"),
        ts: ts.into(),
        reason: reason.into(),
    };
    let notice = entry.notice()?;
    match plugin::send("slack", &entry.target, &notice, Some(&entry.sender)) {
        Outcome::Committed { .. } => {
            println!("promoted");
            Ok(())
        }
        Outcome::Uncertain { .. } => {
            api::record_uncertain(&State::from_env()?)?;
            Err(Error::failure(
                "promotion uncertain; check brg before sending again",
            ))
        }
        Outcome::Refused { .. } => {
            let state = State::from_env()?;
            let mut spool = state.open("undelivered.jsonl", true, true)?;
            plugin::lock(&spool)?;
            spool.seek(SeekFrom::End(0))?;
            serde_json::to_writer(&mut spool, &entry)
                .map_err(|_| Error::failure("cannot write retry spool; check plugin state"))?;
            spool.write_all(b"\n")?;
            spool.flush()?;
            Err(Error::failure(
                "promotion queued for delivery; check watcher health",
            ))
        }
    }
}
pub fn react(api: &Api, ts: &str, reaction: &str) -> Result<()> {
    api::check_ts(ts)?;
    let name = match reaction {
        "eyes" => "eyes",
        "done" => "white_check_mark",
        _ => return Err(Error::usage("choose eyes or done for the reaction")),
    };
    match api.call(
        "reactions.add",
        json!({"channel":api.channel()?,"timestamp":ts,"name":name}),
        true,
    ) {
        Ok(_) => {}
        Err(error) if error.to_string() == "reactions.add: already_reacted; check Slack access" => {
        }
        Err(error) => return Err(error),
    }
    println!("reacted");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_guards_block_mentions_controls_and_secrets_but_allow_dates() {
        for text in [
            "",
            " ",
            "@HERE",
            "@Channel",
            "@everyone",
            "<!subteam^S0000>",
            "<!here>",
            "<@U0000ALICE|alias>",
            "fake-secret",
        ] {
            assert!(check_text(text, 1500, "fake-secret").is_err());
        }
        for text in [
            "<!date^1700000000^{date_short}|today>",
            "<!DATE^1700000000^{date_short}|today>",
            "@hereafter",
            "investigating",
        ] {
            assert!(check_text(text, 1500, "fake-secret").is_ok());
        }
    }
    #[test]
    fn write_limits_count_unicode_characters_instead_of_bytes() {
        for limit in [500, 1500, 3000] {
            assert!(check_text(&"é".repeat(limit), limit, "").is_ok());
            assert!(check_text(&"é".repeat(limit + 1), limit, "").is_err());
        }
    }
}
