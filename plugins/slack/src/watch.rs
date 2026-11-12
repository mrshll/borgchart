//! Poll one channel window, deliver bounded ID notices and retry promotions.
use crate::{
    Error, Result,
    api::{self, Api, Item, Promotion},
};
use borgchart::plugin::{self, Outcome, State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Seek, SeekFrom, Write},
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
    let _lock = files.lock("cursor.lock")?;
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
    let batch: Vec<_> = state.pending.drain(..count).collect();
    // Remove before sending: crashes may lose a notice but never replay it.
    files.write_json("cursor.json", &state)?;
    match plugin::send("slack", &target, &api::message_notice(&batch)?, None) {
        Outcome::Committed { .. } => {}
        Outcome::Uncertain { .. } => api::record_uncertain(files)?,
        Outcome::Refused { message, .. } => {
            state.pending.splice(0..0, batch);
            files.write_json("cursor.json", &state)?;
            return Err(Error::failure(format!(
                "delivery failed: {}; batch kept for the next poll",
                api::safe(&message)
            )));
        }
    }
    Ok(errors)
}
fn rewrite(file: &mut std::fs::File, keep: &[Promotion], rest: &[String]) -> Result<()> {
    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    for entry in keep {
        serde_json::to_writer(&mut *file, entry)
            .map_err(|_| Error::failure("cannot write retry spool; check plugin state"))?;
        file.write_all(b"\n")?;
    }
    for line in rest {
        writeln!(file, "{line}")?;
    }
    file.flush()?;
    Ok(())
}
fn retry_undelivered(state: &State) -> Result<usize> {
    let mut file = match state.open("undelivered.jsonl", true, false) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.into()),
    };
    plugin::lock(&file)?;
    let lines = plugin::read_lines(&mut file)?;
    let mut keep = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let valid = serde_json::from_str::<Promotion>(line)
            .ok()
            .and_then(|entry| entry.notice().ok().map(|notice| (entry, notice)));
        let Some((entry, notice)) = valid else {
            eprintln!("dropping invalid promotion; check the retry spool");
            continue;
        };
        let outcome = plugin::send("slack", &entry.target, &notice, Some(&entry.sender));
        match &outcome {
            // src/runtime.rs supplies the parent-route refusal message.
            Outcome::Refused {
                code: Some(5),
                message,
            } if message.contains("may only send to its parent") => {
                eprintln!("dropping promotion; check the sender's parent route")
            }
            Outcome::Refused { .. } => keep.push(entry),
            _ => {}
        }
        // Persist this result before sending the next promotion.
        rewrite(&mut file, &keep, &lines[index + 1..])?;
        if matches!(outcome, Outcome::Uncertain { .. }) {
            api::record_uncertain(state)?;
        }
    }
    rewrite(&mut file, &keep, &[])?;
    Ok(keep.len())
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
        let state = match State::from_env() {
            Ok(state) => state,
            Err(_) => {
                eprintln!("cannot write health; check plugin state permissions");
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
        let (mut ok, mut detail) = match poll {
            Ok(errors) if errors.is_empty() => (true, "watching".into()),
            Ok(errors) => (false, errors.join("; ")),
            Err(error) => (false, error.to_string()),
        };
        match retry_undelivered(&state) {
            Ok(0) => {}
            Ok(count) => {
                let pending = format!("{count} promotions pending; check brg delivery");
                detail = if ok {
                    pending
                } else {
                    format!("{detail}; {pending}")
                };
                ok = false;
            }
            Err(_) => {
                ok = false;
                detail = "cannot access retry spool; check plugin state permissions".into();
            }
        }
        if api::health(&state, ok, &detail, false).is_err() {
            eprintln!("cannot write health; check plugin state permissions");
        }
        if once {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs_f64(poll_seconds));
    }
}
