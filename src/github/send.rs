//! Bounded reuse of brg's authoritative bus send command.
use super::{
    api::{Api, Result},
    engine,
    model::{self, Attempt, State, now_ms},
};
use crate::bounded_process::{self, Limits};
use serde_json::{Value, json};
use std::{
    process::Command,
    time::{Duration, Instant},
};
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub instance: String,
    pub member: String,
}
impl Target {
    pub fn environment() -> std::result::Result<Self, String> {
        let instance = std::env::var("BORGCHART_INSTANCE").unwrap_or_default();
        let member = std::env::var("BORGCHART_DELIVER_TO").unwrap_or_default();
        let valid = regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9_-]{0,150}$").unwrap();
        if !valid.is_match(&instance)
            || !valid.is_match(&member)
            || ["BORGCHART_MEMBER", "BORGCHART_INCARNATION"]
                .iter()
                .any(|k| std::env::var(k).is_ok_and(|v| !v.is_empty()))
        {
            return Err("brg_environment".into());
        }
        Ok(Self { instance, member })
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Committed,
    Unavailable,
    Unconfirmed,
}
pub fn interpret(code: Option<i32>, stdout: &[u8]) -> Outcome {
    match code {
        Some(0) => return Outcome::Committed,
        Some(2..=4) => return Outcome::Unavailable,
        _ => {}
    }
    let Ok(v) = serde_json::from_slice::<Value>(stdout) else {
        return Outcome::Unconfirmed;
    };
    if code == Some(5)
        && v["code"] == 5
        && v["outcome"] == "refused"
        && v["ok"] == false
        && v["id"]
            .as_str()
            .is_some_and(|s| regex::Regex::new(r"^m[1-9][0-9]*$").unwrap().is_match(s))
        && matches!(
            v["delivery"].as_str(),
            Some("typed" | "stored" | "uncertain" | "recorded-not-typed")
        )
    {
        Outcome::Committed
    } else {
        Outcome::Unconfirmed
    }
}
pub fn pointer(p: &Value) -> std::result::Result<Vec<u8>, String> {
    let name = p["full_name"]
        .as_str()
        .filter(|s| model::full_name(s))
        .ok_or("pointer")?;
    let number = p["number"].as_u64().filter(|n| *n > 0).ok_or("pointer")?;
    let object = p["object_id"].as_str().ok_or("pointer")?;
    let (kind, id) = object.split_once(':').ok_or("pointer")?;
    if id.parse::<u64>().ok().is_none_or(|n| n == 0) {
        return Err("pointer".into());
    }
    let (path, fragment) = match kind {
        "issue_comment" => ("issues", format!("#issuecomment-{id}")),
        "review_comment" => ("pull", format!("#discussion_r{id}")),
        "review" => ("pull", format!("#pullrequestreview-{id}")),
        "event" => (
            if p["kind"] == "merge" {
                "pull"
            } else {
                "issues"
            },
            String::new(),
        ),
        _ => return Err("pointer".into()),
    };
    serde_json::to_vec(&json!({"untrusted":true,"key":p["key"],"kind":p["kind"],"signal":p["signal"],"repository_id":p["repo_id"],"repository":name,"number":number,"actor_id":p["actor_id"],"initiator":p["initiator"],"occurred_at":p["occurred_at"],"url":format!("https://github.com/{name}/{path}/{number}{fragment}")})).map_err(|_|"pointer".into())
}
pub fn send(target: &Target, body: &[u8], deadline: Instant) -> Outcome {
    if Target::environment().as_ref() != Ok(target) {
        return Outcome::Unavailable;
    }
    let Ok(binary) = std::env::current_exe() else {
        return Outcome::Unavailable;
    };
    let mut command = Command::new(binary);
    command.args([
        "send",
        "--external",
        "github",
        "--json",
        &format!("{}/{}", target.instance, target.member),
        "-",
    ]);
    match bounded_process::run(
        &mut command,
        Some(body),
        Limits {
            deadline,
            stdout_bytes: 65536,
            stderr_bytes: 65536,
        },
    ) {
        Ok(o) => interpret(o.status.code(), &o.stdout),
        Err(bounded_process::Error::Io(
            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied,
        )) => Outcome::Unavailable,
        Err(_) => Outcome::Unconfirmed,
    }
}
pub fn produce(api: &mut Api<'_>, s: &mut State, target: &Target) -> Result<(Vec<Value>, usize)> {
    s.row["hold"]
        .as_object_mut()
        .unwrap()
        .remove("brg_unavailable");
    let keys = engine::verified_pending(api, s)?;
    deliver(api.cfg, s, target, keys, api.deadline, send)
}
/// Apply already-revalidated keys, persisting attempts before invoking transport.
/// The injected callable keeps transport outcomes testable without changing the
/// production executable or accepting a config-controlled bus command.
pub fn deliver(
    cfg: &model::Config,
    s: &mut State,
    target: &Target,
    keys: Vec<String>,
    deadline: Instant,
    mut transport: impl FnMut(&Target, &[u8], Instant) -> Outcome,
) -> Result<(Vec<Value>, usize)> {
    let mut committed = Vec::new();
    let mut dropped = 0;
    if cfg.settings.mode != "wake" {
        return Ok((committed, dropped));
    }
    for key in keys {
        if deadline.saturating_duration_since(Instant::now()) < Duration::from_secs(15) {
            break;
        }
        if s.attempts.get(&key).is_some_and(|a| now_ms() < a.next_ms) {
            continue;
        }
        let Some(item) = s.row["pending"].get(&key).cloned() else {
            continue;
        };
        let body = match pointer(&item) {
            Ok(body) => body,
            Err(_) => {
                engine::hold_item(&mut s.row, &item, "pointer");
                engine::persist(cfg, s)?;
                continue;
            }
        };
        let before = s.attempts.get(&key).cloned();
        let tries = before.as_ref().map_or(0, |a| a.tries);
        if tries < 3 {
            s.attempts.insert(
                key.clone(),
                Attempt {
                    tries: tries + 1,
                    next_ms: now_ms() + cfg.settings.poll_seconds * 1000,
                },
            );
            engine::persist(cfg, s)?;
            match transport(target, &body, Instant::now() + Duration::from_secs(15)) {
                Outcome::Committed => {
                    s.row["pending"].as_object_mut().unwrap().remove(&key);
                    s.attempts.remove(&key);
                    engine::persist(cfg, s)?;
                    committed.push(item);
                    continue;
                }
                Outcome::Unavailable => {
                    if let Some(a) = before {
                        s.attempts.insert(key.clone(), a);
                    } else {
                        s.attempts.remove(&key);
                    }
                    model::hold(&mut s.row, "brg_unavailable", "brg_not_ready");
                    engine::persist(cfg, s)?;
                    break;
                }
                Outcome::Unconfirmed => {}
            }
        }
        if s.attempts.get(&key).is_some_and(|a| a.tries >= 3) {
            s.row["pending"].as_object_mut().unwrap().remove(&key);
            s.attempts.remove(&key);
            dropped += 1;
            eprintln!(
                "github-intake: abandoned event {} after 3 attempts",
                item["key"].as_str().unwrap_or("unknown")
            );
        }
        engine::persist(cfg, s)?;
    }
    Ok((committed, dropped))
}
