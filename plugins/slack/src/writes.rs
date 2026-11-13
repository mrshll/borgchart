//! Guarded replies, parent promotions and explicitly allowlisted pages.
use crate::{
    Error, Result,
    api::{self, Api},
    mentions,
};
use borgchart::plugin;
use regex::Regex;
use serde_json::json;
use std::sync::LazyLock;

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
    let app_token = api::var("SLACK_APP_TOKEN");
    if (!token.is_empty() && text.contains(token))
        || (!app_token.is_empty() && text.contains(&app_token))
    {
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
pub fn reply(api: &Api, ts: &str, text: &str) -> Result<()> {
    let request = mentions::resolve(api, ts)?;
    check_text(text, 1500, &api::var("SLACK_BOT_TOKEN"))?;
    api.call(
        "chat.postMessage",
        json!({"channel":request.channel,"thread_ts":request.thread,"text":text}),
        true,
    )?;
    println!("reply posted");
    Ok(())
}
pub fn page(api: &Api, user: &str, text: &str) -> Result<()> {
    if !USER.is_match(user) || !api::csv(&api::var("SLACK_PAGE_IDS")).contains(user) {
        return Err(Error::usage(
            "paging requires an allowed user; set SLACK_PAGE_IDS and choose a listed ID",
        ));
    }
    check_text(text, 500, &api::var("SLACK_BOT_TOKEN"))?;
    let channel = api.channel()?;
    api.call(
        "chat.postMessage",
        json!({"channel":channel,"text":format!("<@{user}> {text}")}),
        true,
    )
    .map_err(|_| Error::failure("page may have been posted; check the channel before retrying"))?;
    println!("page posted");
    Ok(())
}
pub fn promote(ts: &str, reason: &str) -> Result<()> {
    let target = api::var("BORGCHART_PARENT");
    let sender = api::var("BORGCHART_MEMBER");
    let notice = api::promotion_notice(&target, &sender, ts, reason)?;
    let output = plugin::send("slack", &target, &notice, Some(&sender)).map_err(Error::failure)?;
    println!("{}", api::one_line(&output));
    Ok(())
}
pub fn react(api: &Api, ts: &str, reaction: &str) -> Result<()> {
    let request = mentions::resolve(api, ts)?;
    let name = match reaction {
        "eyes" => "eyes",
        "done" => "white_check_mark",
        _ => return Err(Error::usage("choose eyes or done for the reaction")),
    };
    match api.call(
        "reactions.add",
        json!({"channel":request.channel,"timestamp":request.ts,"name":name}),
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
        for limit in [500, 1500] {
            assert!(check_text(&"é".repeat(limit), limit, "").is_ok());
            assert!(check_text(&"é".repeat(limit + 1), limit, "").is_err());
        }
    }
}
