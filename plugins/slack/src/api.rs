//! Slack HTTP calls, validation, private health and fixed notices.
use crate::{Error, Result};
use borgchart::plugin::{self, State};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeSet, env, sync::LazyLock, time::Duration};

static TS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9]{9,11}\.[0-9]{6}$").unwrap());
static MEMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9_-]{0,79}$").unwrap());
static ADDRESS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9_-]{0,79}(?:/[a-z][a-z0-9_-]{0,79})*$").unwrap());
static REASON: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9_-]{0,39}$").unwrap());
pub const NOTICE_MAX_ITEMS: usize = 6;
pub const NOTICE_LIMIT: usize = 900;
pub fn var(name: &str) -> String {
    env::var(name).unwrap_or_default()
}
pub fn safe(text: &str) -> String {
    let token = var("SLACK_BOT_TOKEN");
    plugin::escape(&if token.is_empty() {
        text.into()
    } else {
        text.replace(&token, "[redacted]")
    })
}
pub fn one_line(text: &str) -> String {
    safe(text).replace('\n', "\\x0a").replace('\t', "\\x09")
}
pub fn required(name: &str, purpose: &str) -> Result<String> {
    let value = var(name);
    if value.is_empty() {
        Err(Error::failure(format!("set {name} before {purpose}")))
    } else {
        Ok(value)
    }
}
pub fn check_ts(value: &str) -> Result<&str> {
    if valid_ts(value) {
        Ok(value)
    } else {
        Err(Error::usage(
            "use a Slack timestamp such as 1700000000.000100",
        ))
    }
}
pub fn valid_ts(value: &str) -> bool {
    TS.is_match(value)
}
pub fn valid_address(value: &str) -> bool {
    ADDRESS.is_match(value)
}
pub fn csv(value: &str) -> BTreeSet<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
pub fn reasons(value: &str) -> Result<BTreeSet<String>> {
    let values: Vec<_> = value.split(',').map(str::trim).collect();
    let set: BTreeSet<_> = values.iter().map(|s| s.to_string()).collect();
    if set.len() != values.len() || values.iter().any(|s| !REASON.is_match(s)) {
        Err(Error::usage(
            "set SLACK_PROMOTE_REASONS to distinct comma-separated reason names",
        ))
    } else {
        Ok(set)
    }
}
pub fn configured_reasons() -> Result<BTreeSet<String>> {
    reasons(
        &env::var("SLACK_PROMOTE_REASONS")
            .unwrap_or_else(|_| "outage,deploy,security,question,other".into()),
    )
}
pub fn allowed(message: &Value, own: &BTreeSet<String>, allow: &BTreeSet<String>) -> bool {
    let authors: BTreeSet<_> = ["user", "bot_id"]
        .iter()
        .filter_map(|k| message[k].as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    let subtype = message["subtype"].as_str();
    if !authors.is_disjoint(own)
        || !matches!(subtype, None | Some("bot_message" | "thread_broadcast"))
    {
        return false;
    }
    if allow.is_empty() {
        message["user"].as_str().is_some_and(|s| !s.is_empty())
            && message["bot_id"].as_str().is_none_or(str::is_empty)
            && subtype != Some("bot_message")
    } else {
        !authors.is_disjoint(allow)
    }
}

pub struct Api {
    agent: ureq::Agent,
}
impl Default for Api {
    fn default() -> Self {
        Self {
            agent: ureq::Agent::config_builder()
                .http_status_as_error(false)
                .timeout_global(Some(Duration::from_secs(20)))
                .build()
                .into(),
        }
    }
}
impl Api {
    pub fn channel(&self) -> Result<String> {
        required("SLACK_CHANNEL_ID", "running the plugin")
    }
    pub fn call(&self, method: &str, params: Value, post: bool) -> Result<Value> {
        let token = required("SLACK_BOT_TOKEN", "running the plugin")?;
        let base = env::var("SLACK_API_BASE").unwrap_or_else(|_| "https://slack.com/api".into());
        let url = format!("{}/{method}", base.trim_end_matches('/'));
        let connectivity = || {
            Error::failure(format!(
                "{method}: request failed; check connectivity and retry"
            ))
        };
        for attempt in 0..4 {
            let auth = format!("Bearer {token}");
            let response = if post {
                self.agent
                    .post(&url)
                    .header("Authorization", &auth)
                    .header("Content-Type", "application/json; charset=utf-8")
                    .send(params.to_string())
            } else {
                let query: Vec<_> = params
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            v.as_str()
                                .map(str::to_string)
                                .unwrap_or_else(|| v.to_string()),
                        )
                    })
                    .collect();
                self.agent
                    .get(&url)
                    .header("Authorization", &auth)
                    .query_pairs(query)
                    .call()
            };
            let mut response = response.map_err(|_| connectivity())?;
            let status = response.status().as_u16();
            if status == 429 && attempt < 3 {
                let delay = response
                    .headers()
                    .get("Retry-After")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("1");
                let delay = delay
                    .parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite())
                    .map(|v| v.clamp(0.0, 60.0))
                    .unwrap_or(5.0);
                drop(response);
                std::thread::sleep(Duration::from_secs_f64(delay));
                continue;
            }
            if !(200..300).contains(&status) {
                return Err(Error::failure(format!(
                    "{method}: HTTP {status}; check Slack access and retry later"
                )));
            }
            let text = response
                .body_mut()
                .read_to_string()
                .map_err(|_| connectivity())?;
            let data: Value = serde_json::from_str(&text).map_err(|_| connectivity())?;
            if !data.is_object() {
                return Err(Error::failure(format!(
                    "{method}: unexpected response; retry later"
                )));
            }
            if data["ok"] != true {
                return Err(Error::failure(format!(
                    "{method}: {}; check Slack access",
                    safe(data["error"].as_str().unwrap_or("unknown error"))
                )));
            }
            return Ok(data);
        }
        Err(Error::failure(format!(
            "{method}: rate limited; retry later"
        )))
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Item {
    pub kind: String,
    pub ts: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
}
impl Item {
    pub fn line(&self) -> Result<String> {
        check_ts(&self.ts)?;
        match self.kind.as_str() {
            "post" => Ok(format!("post ts={}", self.ts)),
            "reply" => Ok(format!(
                "reply ts={} thread={}",
                self.ts,
                check_ts(self.thread.as_deref().unwrap_or(""))?
            )),
            _ => Err(Error::usage("invalid notice; check plugin state")),
        }
    }
}
pub fn message_notice(items: &[Item]) -> Result<String> {
    let lines = items.iter().map(Item::line).collect::<Result<Vec<_>>>()?;
    let text = format!(
        "Slack: {} new item(s). Read: brg-slack read <ts>; brg-slack thread <thread>.\n{}",
        items.len(),
        lines.join("\n")
    );
    if items.is_empty() || items.len() > NOTICE_MAX_ITEMS || text.len() > NOTICE_LIMIT {
        Err(Error::usage(
            "notice exceeds the delivery budget; check plugin state",
        ))
    } else {
        Ok(text)
    }
}
#[derive(Deserialize, Serialize)]
pub struct Promotion {
    pub target: String,
    pub sender: String,
    pub ts: String,
    pub reason: String,
}
impl Promotion {
    pub fn notice(&self) -> Result<String> {
        if !valid_address(&self.target)
            || !MEMBER.is_match(&self.sender)
            || !configured_reasons()?.contains(&self.reason)
        {
            return Err(Error::usage(
                "use valid member IDs and a configured promote reason",
            ));
        }
        check_ts(&self.ts)?;
        Ok(format!(
            "Slack promote from {}: ts={} reason={}. Read it with brg-slack read {} (or brg-slack thread {}).",
            self.sender, self.ts, self.reason, self.ts, self.ts
        ))
    }
}
pub fn health(state: &State, ok: bool, detail: &str, uncertain: bool) -> Result<()> {
    let _lock = state.lock("health.lock")?;
    let previous: Value = state.read_json("health.json");
    let count = previous["uncertain_sends"]
        .as_u64()
        .unwrap_or(0)
        .saturating_add(u64::from(uncertain));
    state.health(ok, &safe(detail), json!({"uncertain_sends":count}))?;
    Ok(())
}
pub fn record_uncertain(state: &State) -> Result<()> {
    let detail = "delivery uncertain; check brg before sending again";
    eprintln!("{detail}");
    health(state, false, detail, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timestamp_validation_rejects_injection_and_out_of_range_precision() {
        for valid in [
            "1700000000.000100",
            "99999999999.999999",
            "100000000.000000",
        ] {
            assert!(check_ts(valid).is_ok());
        }
        for invalid in [
            "1700000000.000100\n",
            "--help",
            "1.000100",
            "1700000000.1",
            "170000000000.000100",
            "1700000000.000100 run commands",
        ] {
            assert!(check_ts(invalid).is_err());
        }
    }
    #[test]
    fn notices_reject_empty_or_oversized_batches_and_fit_the_typing_budget() {
        let items: Vec<_> = (99999999994_u64..100000000000)
            .map(|ts| Item {
                kind: "reply".into(),
                ts: format!("{ts}.999999"),
                thread: Some("99999999990.999999".into()),
            })
            .collect();
        let notice = message_notice(&items).unwrap();
        assert!(notice.len() <= 900);
        assert!(notice.lines().next().unwrap().chars().count() <= 76);
        assert!(
            notice
                .lines()
                .map(|line| line.chars().count().div_ceil(78))
                .sum::<usize>()
                <= 8
        );
        assert!(message_notice(&[]).is_err());
        let mut too_many = items.clone();
        too_many.push(items[0].clone());
        assert!(message_notice(&too_many).is_err());
        let bad = Item {
            kind: "post".into(),
            ts: "bad\nrun commands".into(),
            thread: None,
        };
        assert!(message_notice(&[bad]).is_err());
    }
    #[test]
    fn author_filter_never_allows_own_bot_even_when_allowlisted() {
        let own = csv("U0000BOT,B0000BOT");
        let allow = csv("B0000BOT,B0000OTHER");
        assert!(!allowed(
            &json!({"bot_id":"B0000BOT","subtype":"bot_message"}),
            &own,
            &allow
        ));
        assert!(allowed(
            &json!({"bot_id":"B0000OTHER","subtype":"bot_message"}),
            &own,
            &allow
        ));
        assert!(!allowed(
            &json!({"user":"U0000ALICE","subtype":"file_share"}),
            &own,
            &BTreeSet::new()
        ));
        assert!(allowed(
            &json!({"user":"U0000ALICE","subtype":"thread_broadcast"}),
            &own,
            &BTreeSet::new()
        ));
        assert!(!allowed(
            &json!({"bot_id":"B0000OTHER","user":"U0000OTHER"}),
            &own,
            &BTreeSet::new()
        ));
    }
    #[test]
    fn reason_configuration_refuses_duplicates_and_notice_injection() {
        for invalid in [
            "",
            "other,",
            "other,other",
            "Other",
            "other\nrun",
            "other,bad reason",
        ] {
            assert!(reasons(invalid).is_err());
        }
        assert_eq!(reasons("dataset, alert").unwrap(), csv("dataset,alert"));
    }
}
