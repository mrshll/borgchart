//! Journal leading mentions before acknowledging; deliver requests off the socket thread.
use crate::{
    Error, Result,
    api::{self, Api},
    socket::{self, Event, Health},
};
use borgchart::plugin::{self, State};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

mod deliver;
use deliver::deliver;

const RETENTION: u64 = 14 * 86400;
#[derive(Default, Deserialize, Serialize)]
pub struct Request {
    pub channel: String,
    pub ts: String,
    pub thread: String,
    user: String,
    text: String,
    at: u64,
    state: String,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn filename(channel: &str, ts: &str) -> String {
    format!("req-{channel}-{ts}.json")
}
pub fn resolve(api: &Api, target: &str) -> Result<Request> {
    if let Some((c, ts)) = target
        .split_once('-')
        .filter(|(c, ts)| api::valid_channel(c) && api::valid_ts(ts))
    {
        let request: Request = State::from_env()?.read_json(&filename(c, ts));
        if request.channel == c
            && request.ts == ts
            && api::valid_ts(&request.thread)
            && now().saturating_sub(request.at) <= RETENTION
        {
            return Ok(request);
        }
        return Err(Error::usage(
            "no such request; use the ID from the request message (requests expire after 14 days)",
        ));
    }
    api::check_ts(target)?;
    Ok(Request {
        channel: api.channel()?,
        ts: target.into(),
        thread: target.into(),
        ..Request::default()
    })
}
fn clipped(text: &str, chars: usize, bytes: usize) -> String {
    let mut cut = text.chars().take(chars).collect::<String>();
    let mut count = cut.chars().count();
    let mut framed_bytes = cut.len() + 3 * (cut.matches('\n').count() + 1);
    if cut.len() < text.len() || framed_bytes > bytes {
        while count + 12 > chars || framed_bytes + 15 > bytes {
            let Some(c) = cut.pop() else {
                break;
            };
            count -= 1;
            framed_bytes -= c.len_utf8() + if c == '\n' { 3 } else { 0 };
        }
        cut.push_str("\n[truncated]");
    }
    cut
}
fn accept(envelope: &Value, bot: &str, team: &str, users: &BTreeSet<String>) -> Option<Request> {
    let e = &envelope["payload"]["event"];
    let ts = e["ts"].as_str()?;
    let c = e["channel"].as_str()?;
    let user = e["user"].as_str()?;
    let text = e["text"].as_str()?;
    let thread = e["thread_ts"].as_str().unwrap_or(ts);
    if envelope["type"] != "events_api"
        || envelope["payload"]["team_id"] != team
        || e["type"] != "app_mention"
        || !users.contains(user)
        || user == bot
        || !e["bot_id"].is_null()
        || !e["subtype"].is_null()
        || !api::valid_channel(c)
        || !api::valid_ts(ts)
        || !api::valid_ts(thread)
        || !api::leading_mention(text, bot)
    {
        return None;
    }
    Some(Request {
        channel: c.into(),
        ts: ts.into(),
        thread: thread.into(),
        user: user.into(),
        text: clipped(&api::safe(text), 4000, usize::MAX),
        at: now(),
        state: "pending".into(),
    })
}
pub fn run(api: &Api, once: bool) -> Result<()> {
    for name in [
        "SLACK_APP_TOKEN",
        "SLACK_BOT_TOKEN",
        "SLACK_MENTION_USERS",
        "BORGCHART_DELIVER_TO",
        "BORGCHART_STATE_DIR",
    ] {
        if api::var(name).is_empty() {
            if name == "BORGCHART_DELIVER_TO" {
                return Err(Error::usage(
                    "add `delivers_to: <role>` to this plugin in template.yaml (direct runs: set BORGCHART_DELIVER_TO to a member ID)",
                ));
            }
            return Err(Error::usage(format!("set {name} before running mentions")));
        }
    }
    let allow = api::var("SLACK_MENTION_USERS");
    let user = Regex::new(r"^[UW][A-Z0-9]+$").unwrap();
    if !allow.split(',').all(|s| user.is_match(s.trim())) {
        return Err(Error::usage(
            "set SLACK_MENTION_USERS to comma-separated user IDs",
        ));
    }
    let users = api::csv(&allow);
    let target = api::var("BORGCHART_DELIVER_TO");
    if !api::valid_address(&target) {
        return Err(Error::usage(
            "add `delivers_to: <role>` to this plugin in template.yaml (direct runs: set BORGCHART_DELIVER_TO to a member ID)",
        ));
    }
    let files = State::from_env()?;
    let _lock = files.lock()?;
    let auth = socket::auth(api, &files, once)?;
    let (bot, team) = (
        auth["user_id"].as_str().unwrap_or(""),
        auth["team_id"].as_str().unwrap_or(""),
    );
    if bot.is_empty() || team.is_empty() {
        return Err(Error::failure(
            "auth.test returned no bot or workspace identity; check Slack access",
        ));
    }
    socket::run(
        api,
        &files,
        once,
        |envelope| {
            if let Some(request) = accept(envelope, bot, team, &users) {
                let name = filename(&request.channel, &request.ts);
                if fs::metadata(format!("{}/{name}", api::var("BORGCHART_STATE_DIR"))).is_err() {
                    files.write_json(&name, &request)?;
                    return Ok(Event::Journaled);
                }
                Ok(Event::Duplicate)
            } else {
                Ok(Event::Ignored)
            }
        },
        || Ok(false),
        |health| deliver(api, &files, &target, health),
    )
}
