//! Optional, once-only eyes reactions after durable bus commitment.
use super::{
    api::{self, Api, Result},
    engine,
    model::{self, Config, Reaction, State, now_ms, utc},
    qualify,
};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
pub fn target(cfg: &Config, row: &Value, item: &Value) -> Option<(String, u64)> {
    if item["kind"] == "merge" {
        return None;
    }
    let repo = item["repo_id"].as_u64().filter(|n| *n > 0)?;
    let number = item["number"].as_u64().filter(|n| *n > 0)?;
    let object = item["object_id"].as_str()?;
    let (kind, id) = object.split_once(':')?;
    let id = id.parse::<u64>().ok().filter(|n| *n > 0)?;
    let proof = &item["proof"];
    if item["key"]
        != qualify::digest(&[
            json!("github.com"),
            json!(repo),
            item["kind"].clone(),
            item["object_id"].clone(),
            item["version"].clone(),
        ])
    {
        return None;
    }
    let route = match (item["kind"].as_str()?, item["signal"].as_str()?, kind) {
        ("assignment", "assignment", "event") if proof["event_id"] == id => {
            format!("issue:{number}")
        }
        ("feedback", "mention", "issue_comment")
            if proof["object_type"] == kind && proof["comment_id"] == id =>
        {
            format!("issue_comment:{id}")
        }
        ("feedback", "handoff", "issue_comment" | "review_comment")
            if proof["object_type"] == kind
                && proof["request_event_id"].as_u64().is_some_and(|n| n > 0)
                && proof["reviewer_id"].as_u64().is_some_and(|n| n > 0) =>
        {
            format!("{kind}:{id}")
        }
        _ => return None,
    };
    let actor = item["actor_id"].as_u64().filter(|n| *n > 0)?;
    let initiator = item["initiator"].as_str()?;
    let config = cfg.value();
    let policy = qualify::members(&config);
    let member = &policy[initiator];
    if initiator == "human" {
        if !cfg.settings.trusted_actor_ids.contains(&actor) {
            return None;
        }
    } else if let Some(app) = initiator.strip_prefix("app:") {
        let app = app.parse::<u64>().ok()?;
        if item["kind"] != "assignment"
            || proof["app_id"] != app
            || member["bot"] != actor
            || !member["repos"].as_array()?.contains(&json!(repo))
        {
            return None;
        }
    } else {
        let reviewer = initiator.strip_prefix("reviewer:")?;
        let reviewer = reviewer.parse::<u64>().ok()?;
        if item["signal"] != "handoff"
            || actor != reviewer
            || proof["reviewer_id"] != reviewer
            || member["reviewer"] != reviewer
        {
            return None;
        }
    }
    if row["repos"][repo.to_string()]["status"] != "admitted"
        || cfg
            .settings
            .owner_policy
            .exclude_repository_ids
            .contains(&repo)
        || row["pins"].as_object()?.values().any(|v| v == repo)
    {
        return None;
    }
    Some((format!("{repo}:{route}"), number))
}
fn path(target: &str) -> Option<(u64, String)> {
    let parts: Vec<_> = target.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let repo = parts[0].parse::<u64>().ok().filter(|n| *n > 0)?;
    let id = parts[2].parse::<u64>().ok().filter(|n| *n > 0)?;
    let route = match parts[1] {
        "issue" => "issues",
        "issue_comment" => "issues/comments",
        "review_comment" => "pulls/comments",
        _ => return None,
    };
    Some((repo, format!("/repositories/{repo}/{route}/{id}")))
}
fn verify(api: &mut Api<'_>, target: &str, number: u64) -> Result<bool> {
    let (repo_id, path) = path(target).ok_or("path")?;
    let repo = api.get(&format!("/repositories/{repo_id}"))?;
    let owner = &api.cfg.settings.owner_policy.owner;
    let Some(name) = repo["full_name"].as_str().filter(|s| model::full_name(s)) else {
        return Ok(false);
    };
    if repo["id"] != repo_id
        || repo["owner"]["id"] != owner.id
        || repo["owner"]["type"] != owner.kind
        || !repo["owner"]["login"]
            .as_str()
            .unwrap_or("")
            .eq_ignore_ascii_case(&owner.login)
    {
        return Ok(false);
    }
    let root = format!("https://api.github.com/repos/{name}").to_ascii_lowercase();
    let v = api.get(&path)?;
    let same = if target.contains(":issue:") {
        v["number"] == number
            && v["repository_url"]
                .as_str()
                .unwrap_or("")
                .to_ascii_lowercase()
                == root
    } else if target.contains(":issue_comment:") {
        v["issue_url"].as_str().unwrap_or("").to_ascii_lowercase()
            == format!("{root}/issues/{number}")
    } else {
        v["pull_request_url"]
            .as_str()
            .unwrap_or("")
            .to_ascii_lowercase()
            == format!("{root}/pulls/{number}")
    };
    Ok(same)
}
fn enabled(cfg: &Config) -> bool {
    Config::read(&cfg.path).is_ok_and(|c| {
        c.scope() == cfg.scope() && c.settings.reactions && c.settings.mode == "wake"
    })
}
fn gate(api: &Api<'_>, s: &State) -> bool {
    api.deadline.saturating_duration_since(Instant::now()) >= Duration::from_secs(17)
        && enabled(api.cfg)
        && now_ms()
            >= s.reactions
                .write_retry_ms
                .max(s.backoff_ms)
                .max(api.retry_ms)
        && s.reactions.targets.len() < 10000
}
pub fn send(api: &mut Api<'_>, s: &mut State, items: &[Value]) -> Result<()> {
    if !api.cfg.settings.reactions {
        return Ok(());
    }
    let mut forbidden = 0;
    for item in items {
        let Some((key, number)) = target(api.cfg, &s.row, item) else {
            continue;
        };
        if s.reactions.targets.contains_key(&key) {
            continue;
        }
        if !gate(api, s) {
            break;
        }
        match verify(api, &key, number) {
            Ok(true) => {}
            Ok(false) => {
                s.reactions.targets.insert(
                    key,
                    Reaction {
                        outcome: "failed".into(),
                        reason: Some("moved".into()),
                        at: utc(),
                    },
                );
                engine::persist(api.cfg, s)?;
                continue;
            }
            Err(e) => {
                s.reactions.read_retry_ms =
                    s.reactions.read_retry_ms.max(api.retry_ms).max(e.retry_ms);
                if api::stop(&e.kind) || api::transient(&e.kind) {
                    break;
                }
                s.reactions.targets.insert(
                    key,
                    Reaction {
                        outcome: "failed".into(),
                        reason: Some(e.kind),
                        at: utc(),
                    },
                );
                engine::persist(api.cfg, s)?;
                continue;
            }
        }
        if api.retry_ms > now_ms() {
            s.reactions.read_retry_ms = s.reactions.read_retry_ms.max(api.retry_ms);
            break;
        }
        let pause = s
            .reactions
            .end_ms
            .saturating_add(2000)
            .saturating_sub(now_ms());
        if api.deadline.saturating_duration_since(Instant::now())
            < Duration::from_millis(pause.saturating_add(17000))
        {
            break;
        }
        if pause > 0 {
            std::thread::sleep(Duration::from_millis(pause));
        }
        if !gate(api, s) {
            break;
        }
        // A crash after this write permanently suppresses this target. Intake
        // was retired before entering this module; reactions cannot replay it.
        s.reactions.end_ms = now_ms() + 16000;
        s.reactions.targets.insert(
            key.clone(),
            Reaction {
                outcome: "uncertain".into(),
                reason: Some("in_flight".into()),
                at: utc(),
            },
        );
        engine::persist(api.cfg, s)?;
        if !enabled(api.cfg)
            || api.deadline.saturating_duration_since(Instant::now()) < Duration::from_secs(15)
        {
            s.reactions.targets.remove(&key);
            engine::persist(api.cfg, s)?;
            break;
        }
        let (_, route) = path(&key).ok_or("path")?;
        let result = api.raw(
            &format!("{route}/reactions"),
            None,
            true,
            Instant::now() + Duration::from_secs(15),
        );
        let mut stop = false;
        let mut retry = 0;
        let mut shared = false;
        let (outcome, reason) = match result {
            Err(e) => {
                stop = true;
                ("uncertain".into(), Some(e.kind))
            }
            Ok((r, _)) => {
                let primary = r
                    .headers
                    .get("x-ratelimit-remaining")
                    .is_some_and(|v| v == "0");
                if primary {
                    retry = (now_ms() + 60000).max(
                        r.headers
                            .get("x-ratelimit-reset")
                            .and_then(|v| v.parse::<u64>().ok())
                            .unwrap_or(0)
                            .saturating_mul(1000),
                    );
                    shared = true;
                    stop = true;
                }
                let limited = matches!(r.status, 403 | 429)
                    && (primary
                        || r.status == 429
                        || r.headers.contains_key("retry-after")
                        || r.body["message"]
                            .as_str()
                            .unwrap_or("")
                            .to_ascii_lowercase()
                            .contains("secondary rate limit"));
                if matches!(r.status, 200 | 201) {
                    if r.body["content"] == "eyes"
                        && r.body["user"]["id"] == api.cfg.settings.agent.id
                    {
                        s.reactions.streak = 0;
                        (
                            if r.status == 201 {
                                "created"
                            } else {
                                "existing"
                            }
                            .into(),
                            None,
                        )
                    } else {
                        stop = true;
                        ("uncertain".into(), Some("unconfirmed".into()))
                    }
                } else if limited {
                    let wait = 60u64
                        .saturating_mul(2u64.saturating_pow(s.reactions.streak.min(6)))
                        .min(3600)
                        .max(
                            r.headers
                                .get("retry-after")
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(0),
                        );
                    retry = retry.max(now_ms().saturating_add(wait.saturating_mul(1000)));
                    shared = true;
                    stop = true;
                    s.reactions.streak = s.reactions.streak.saturating_add(1).min(64);
                    ("refused".into(), Some("rate_limited".into()))
                } else if r.status == 422 {
                    retry = retry.max(now_ms() + 60000);
                    stop = true;
                    ("failed".into(), Some("http_422".into()))
                } else if r.status >= 500 {
                    stop = true;
                    ("uncertain".into(), Some(format!("http_{}", r.status)))
                } else {
                    if r.status == 401 {
                        stop = true;
                    }
                    if r.status == 403 {
                        forbidden += 1;
                        stop |= forbidden >= 3;
                    }
                    ("failed".into(), Some(format!("http_{}", r.status)))
                }
            }
        };
        s.reactions.end_ms = now_ms();
        s.reactions.write_retry_ms = s.reactions.write_retry_ms.max(retry);
        if shared {
            s.reactions.read_retry_ms = s.reactions.read_retry_ms.max(retry);
        }
        s.reactions.targets.insert(
            key,
            Reaction {
                outcome,
                reason,
                at: utc(),
            },
        );
        engine::persist(api.cfg, s)?;
        if stop {
            break;
        }
    }
    s.backoff_ms = s.backoff_ms.max(api.retry_ms);
    engine::persist(api.cfg, s)
}
