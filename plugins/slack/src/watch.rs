//! Poll one channel window and deliver bounded ID notices.
use crate::{
    Error, Result,
    api::{self, Api, Item},
};
use borgchart::plugin::{self, State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct Cursor {
    initialized: bool,
    messages: BTreeMap<String, Entry>,
    pending: Vec<Item>,
}
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
struct Entry {
    newest_reply: String,
    latest_reply: Option<String>,
}
fn timestamp(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| api::valid_ts(s))
}
fn number(ts: &str) -> f64 {
    ts.parse().unwrap_or(0.0)
}
fn messages(data: &Value) -> Vec<&Value> {
    let mut values: Vec<_> = data["messages"].as_array().into_iter().flatten().collect();
    values.sort_by_key(|m| m["ts"].as_str().unwrap_or(""));
    values
}
fn collect(
    api: &Api,
    state: &mut Cursor,
    cutoff: f64,
    own: &BTreeSet<String>,
) -> Result<Vec<String>> {
    let mut errors = Vec::new();
    let first = !state.initialized;
    let allow = api::csv(&api::var("SLACK_ALLOW_IDS"));
    state.messages.retain(|ts, entry| {
        api::valid_ts(ts) && number(ts) >= cutoff && api::valid_ts(&entry.newest_reply)
    });
    state.pending.retain(|item| {
        item.line().is_ok() && number(item.thread.as_deref().unwrap_or(&item.ts)) >= cutoff
    });
    let history = api.call(
        "conversations.history",
        json!({"channel":api.channel()?,"oldest":format!("{cutoff:.6}"),"limit":200}),
        false,
    )?;
    for message in messages(&history) {
        let Some(ts) = timestamp(&message["ts"]) else {
            continue;
        };
        if number(ts) < cutoff || message["thread_ts"].as_str().unwrap_or(ts) != ts {
            continue;
        }
        let old = state.messages.get(ts);
        let mut entry = old.cloned().unwrap_or_else(|| Entry {
            newest_reply: ts.into(),
            latest_reply: None,
        });
        if !first && old.is_none() && api::allowed(message, own, &allow) {
            state.pending.push(Item {
                kind: "post".into(),
                ts: ts.into(),
                thread: None,
            });
        }
        let latest = timestamp(&message["latest_reply"]);
        if first {
            // Baseline the thread even when Slack's replies endpoint lags.
            entry.newest_reply = latest.unwrap_or(ts).into();
            entry.latest_reply = latest.map(str::to_owned);
        } else if let Some(latest) = latest
            && Some(latest) != entry.latest_reply.as_deref()
        {
            match api.call(
                "conversations.replies",
                json!({"channel":api.channel()?,"ts":ts,"oldest":entry.newest_reply,"limit":200}),
                false,
            ) {
                Err(error) => {
                    errors.push(error.to_string());
                    state.messages.insert(ts.into(), entry);
                    continue;
                }
                Ok(data) => {
                    for reply in messages(&data) {
                        let Some(reply_ts) = timestamp(&reply["ts"]) else {
                            continue;
                        };
                        if reply_ts <= entry.newest_reply.as_str() {
                            continue;
                        }
                        entry.newest_reply = reply_ts.into();
                        if api::allowed(reply, own, &allow) {
                            state.pending.push(Item {
                                kind: "reply".into(),
                                ts: reply_ts.into(),
                                thread: Some(ts.into()),
                            });
                        }
                    }
                    if entry.newest_reply.as_str() >= latest {
                        entry.latest_reply = Some(latest.into());
                    }
                }
            }
        }
        state.messages.insert(ts.into(), entry);
    }
    state.initialized = true;
    Ok(errors)
}
fn poll_once(
    api: &Api,
    files: &State,
    own: &BTreeSet<String>,
    window_hours: f64,
) -> Result<Vec<String>> {
    let mut state: Cursor = files.read_json("cursor.json");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let errors = collect(api, &mut state, now - window_hours * 3600.0, own)?;
    files.write_json("cursor.json", &state)?;
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
    if count == 0 {
        return Ok(errors);
    }
    let target = api::var("BORGCHART_DELIVER_TO");
    if !api::valid_address(&target) {
        return Err(Error::usage(
            "set BORGCHART_DELIVER_TO to the intake member's ID",
        ));
    }
    plugin::send(
        "slack",
        &target,
        &api::message_notice(&state.pending[..count])?,
        None,
    )
    .map_err(|message| {
        Error::failure(format!(
            "delivery failed: {}; batch kept for the next poll",
            api::safe(&message)
        ))
    })?;
    state.pending.drain(..count);
    files.write_json("cursor.json", &state)?;
    Ok(errors)
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
    let poll_seconds = positive_env("SLACK_POLL_SECONDS", "15")?;
    let window_hours = positive_env("SLACK_WINDOW_HOURS", "24")?;
    let mut own = BTreeSet::new();
    loop {
        let (state, lock) =
            match State::from_env().and_then(|state| state.lock().map(|lock| (state, lock))) {
                Ok(pair) => pair,
                Err(error) => {
                    eprintln!("{}", api::one_line(&Error::from(error).to_string()));
                    if once {
                        return Ok(());
                    }
                    std::thread::sleep(Duration::from_secs_f64(poll_seconds));
                    continue;
                }
            };
        let poll = (|| {
            if own.is_empty() {
                let auth = api.call("auth.test", json!({}), false)?;
                own = ["user_id", "bot_id"]
                    .iter()
                    .filter_map(|k| auth[k].as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect();
                if own.is_empty() {
                    return Err(Error::failure(
                        "auth.test returned no bot identity; check Slack access",
                    ));
                }
            }
            poll_once(api, &state, &own, window_hours)
        })();
        let (ok, detail) = match poll {
            Ok(errors) if errors.is_empty() => (true, "watching".into()),
            Ok(errors) => (false, errors.join("; ")),
            Err(error) => (false, error.to_string()),
        };
        if state.health(ok, &api::safe(&detail)).is_err() {
            eprintln!("cannot write health; check plugin state permissions");
        }
        drop(lock);
        if once {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs_f64(poll_seconds));
    }
}
