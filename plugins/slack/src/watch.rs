//! Journal channel message events before acknowledging; deliver bounded ID notices.
use crate::{
    Error, Result,
    api::{self, Api, Item},
    socket::{self, Event, Health},
};
use borgchart::plugin::{self, State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct Cursor {
    last: String,
    seen: BTreeSet<String>,
    pending: Vec<Item>,
}
fn timestamp(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| api::valid_ts(s))
}
fn number(ts: &str) -> f64 {
    ts.parse().unwrap_or(0.0)
}
impl Cursor {
    fn prune(&mut self, cutoff: f64) {
        self.seen
            .retain(|ts| api::valid_ts(ts) && number(ts) >= cutoff);
        self.pending
            .retain(|item| item.line().is_ok() && number(&item.ts) >= cutoff);
    }
    fn journal(
        &mut self,
        message: &Value,
        own: &BTreeSet<String>,
        allow: &BTreeSet<String>,
    ) -> Event {
        let Some(ts) = timestamp(&message["ts"]) else {
            return Event::Ignored;
        };
        let thread = message["thread_ts"].as_str().unwrap_or(ts);
        if message["type"] != "message"
            || !api::valid_ts(thread)
            || !api::allowed(message, own, allow)
        {
            return Event::Ignored;
        }
        if !self.seen.insert(ts.into()) {
            return Event::Duplicate;
        }
        self.pending.push(Item {
            kind: if thread == ts { "post" } else { "reply" }.into(),
            ts: ts.into(),
            thread: (thread != ts).then(|| thread.into()),
        });
        if number(ts) > number(&self.last) {
            self.last = ts.into();
        }
        Event::Journaled
    }
}
fn deliver(
    files: &State,
    cursor: &Mutex<Cursor>,
    target: &str,
    cutoff: f64,
    health: &Mutex<Health>,
) -> Result<()> {
    let batch = {
        let mut state = cursor.lock().unwrap();
        state.prune(cutoff);
        state
            .pending
            .sort_by(|a, b| number(&a.ts).total_cmp(&number(&b.ts)));
        let mut count = 0;
        for end in 1..=state.pending.len().min(api::NOTICE_MAX_ITEMS) {
            if api::message_notice(&state.pending[..end]).is_err() {
                break;
            }
            count = end;
        }
        state.pending[..count].to_vec()
    };
    if batch.is_empty() {
        let mut status = health.lock().unwrap();
        status.delivery.clear();
        return status.write(files);
    }
    plugin::send("slack", target, &api::message_notice(&batch)?, None).map_err(|message| {
        Error::failure(format!(
            "delivery failed: {}; batch kept for retry",
            api::safe(&message)
        ))
    })?;
    {
        let mut state = cursor.lock().unwrap();
        state
            .pending
            .retain(|item| !batch.iter().any(|sent| sent.ts == item.ts));
        files.write_json("cursor.json", &*state)?;
    }
    let mut status = health.lock().unwrap();
    status.delivery.clear();
    status.write(files)
}
fn positive_env(name: &str, default: &str) -> Result<f64> {
    let value = std::env::var(name).unwrap_or_else(|_| default.into());
    value
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v > 0.0 && Duration::try_from_secs_f64(*v).is_ok())
        .ok_or_else(|| Error::usage(format!("set {name} to a positive number")))
}
pub fn run(api: &Api, once: bool) -> Result<()> {
    for name in [
        "SLACK_APP_TOKEN",
        "SLACK_BOT_TOKEN",
        "SLACK_CHANNEL_ID",
        "BORGCHART_DELIVER_TO",
        "BORGCHART_STATE_DIR",
    ] {
        if api::var(name).is_empty() {
            if name == "BORGCHART_DELIVER_TO" {
                return Err(Error::usage(
                    "add `delivers_to: <role>` to this plugin in template.yaml (direct runs: set BORGCHART_DELIVER_TO to a member ID)",
                ));
            }
            return Err(Error::usage(format!("set {name} before running watch")));
        }
    }
    let channel = api.channel()?;
    let target = api::var("BORGCHART_DELIVER_TO");
    if !api::valid_address(&target) || !api::valid_channel(&channel) {
        return Err(Error::usage(
            "add `delivers_to: <role>` to this plugin in template.yaml (direct runs: set BORGCHART_DELIVER_TO to a member ID) and set SLACK_CHANNEL_ID to a channel ID",
        ));
    }
    let window = positive_env("SLACK_WINDOW_HOURS", "24")? * 3600.0;
    let now = || {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
    };
    let files = State::from_env()?;
    let _lock = files.lock()?;
    let auth = socket::auth(api, &files, once)?;
    let own = ["user_id", "bot_id"]
        .iter()
        .filter_map(|k| auth[k].as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    let team = auth["team_id"].as_str().unwrap_or("");
    if auth["user_id"].as_str().unwrap_or("").is_empty() || team.is_empty() {
        return Err(Error::failure(
            "auth.test returned no bot or workspace identity; check Slack access",
        ));
    }
    let allow = api::csv(&api::var("SLACK_ALLOW_IDS"));
    let cursor = Mutex::new(files.read_json::<Cursor>("cursor.json"));
    socket::run(
        api,
        &files,
        once,
        |envelope| {
            let event = &envelope["payload"]["event"];
            if envelope["type"] != "events_api"
                || envelope["payload"]["team_id"] != team
                || event["channel"] != channel
            {
                return Ok(Event::Ignored);
            }
            let mut state = cursor.lock().unwrap();
            let outcome = state.journal(event, &own, &allow);
            // A duplicate may retry an earlier failed save of the in-memory journal.
            if matches!(outcome, Event::Journaled | Event::Duplicate) {
                files.write_json("cursor.json", &*state)?;
            }
            Ok(outcome)
        },
        || {
            let last = cursor.lock().unwrap().last.clone();
            let history = if last.is_empty() {
                Value::Null
            } else {
                api.call_with(
                    "conversations.history",
                    json!({"channel":channel,"oldest":format!("{:.6}", number(&last).max(now() - window)),"limit":200}),
                    false,
                    &api::var("SLACK_BOT_TOKEN"),
                    true,
                )?
            };
            let mut state = cursor.lock().unwrap();
            let mut queued = false;
            if last.is_empty() {
                state.last = format!("{:.6}", now());
            }
            for message in history["messages"].as_array().into_iter().flatten() {
                if number(message["ts"].as_str().unwrap_or("")) > number(&last) {
                    queued |= matches!(state.journal(message, &own, &allow), Event::Journaled);
                }
            }
            state.prune(now() - window);
            files.write_json("cursor.json", &*state)?;
            Ok(queued)
        },
        |health| deliver(&files, &cursor, &target, now() - window, health),
    )
}
