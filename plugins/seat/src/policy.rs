//! Subscription quota policy. Only normalized, allowlisted fields leave here.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const TTL: u64 = 120;
pub const BACKOFF: u64 = 30;
pub const UNAVAILABLE: &str =
    "usage unavailable; check subscription login and CLI compatibility, then retry after backoff";
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Claude,
    Codex,
}
impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
    pub fn source(self) -> &'static str {
        match self {
            Self::Claude => "get_usage (experimental)",
            Self::Codex => "app-server",
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Window {
    pub key: String,
    pub scope: String,
    pub used_percent: Option<f64>,
    pub resets_at: Option<String>,
    pub window_seconds: Option<u64>,
    pub blocked: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub schema_version: u32,
    pub provider: Provider,
    pub source: String,
    pub status: String,
    pub observed_at: String,
    pub expires_at: String,
    pub included_allowed: Option<bool>,
    pub windows: Vec<Window>,
    pub known_scopes: Vec<String>,
    pub held_keys: Vec<String>,
    pub denied_scopes: Vec<String>,
    pub timed_denial: bool,
    pub unrecognized_constraint: bool,
    pub last_success_at: Option<String>,
}
#[derive(Debug)]
pub struct Assessment {
    pub available: bool,
    pub reason: String,
    pub warnings: Vec<String>,
    pub retry_at: Option<SystemTime>,
}
pub fn stamp(t: SystemTime) -> String {
    borgchart::bus::time::format_utc(t)
}
pub fn time(v: &Value) -> Option<SystemTime> {
    if let Some(n) = v.as_f64() {
        if !n.is_finite() || !(0.0..=253402300799.0).contains(&n) {
            return None;
        }
        return UNIX_EPOCH.checked_add(Duration::try_from_secs_f64(n).ok()?);
    }
    let s = v.as_str()?;
    if s.ends_with('Z') {
        return borgchart::bus::time::parse_utc(s);
    }
    let split = s.len().checked_sub(6)?;
    let zone = s.get(split..)?;
    let b = zone.as_bytes();
    if b.len() != 6 || !matches!(b[0], b'+' | b'-') || b[3] != b':' {
        return None;
    }
    let hours = zone.get(1..3)?.parse::<u64>().ok()?;
    let mins = zone.get(4..6)?.parse::<u64>().ok()?;
    if hours > 23 || mins > 59 || !b[1..3].iter().chain(&b[4..6]).all(u8::is_ascii_digit) {
        return None;
    }
    let local = borgchart::bus::time::parse_utc(&format!("{}Z", s.get(..split)?))?;
    let offset = Duration::from_secs(hours * 3600 + mins * 60);
    let utc = if b[0] == b'+' {
        local.checked_sub(offset)?
    } else {
        local.checked_add(offset)?
    };
    borgchart::bus::time::parse_utc(&stamp(utc))?;
    Some(utc)
}
pub fn unknown(provider: Provider, now: SystemTime, held_keys: Vec<String>) -> Snapshot {
    Snapshot {
        schema_version: 2,
        provider,
        source: provider.source().into(),
        status: "unknown".into(),
        observed_at: stamp(now),
        expires_at: stamp(now + Duration::from_secs(BACKOFF)),
        included_allowed: None,
        windows: Vec::new(),
        known_scopes: Vec::new(),
        held_keys,
        denied_scopes: Vec::new(),
        timed_denial: false,
        unrecognized_constraint: false,
        last_success_at: None,
    }
}
fn scope_ok(s: &str) -> bool {
    matches!(
        s,
        "all" | "opus" | "sonnet" | "fable" | "astra" | "sol" | "terra" | "luna"
    )
}
fn key_ok(s: &str) -> bool {
    regex::Regex::new(r"^(?:(?:codex|astra|sol|terra|luna)\.(?:primary|secondary)|five_hour|seven_day(?:_opus|_sonnet)?|model\.(?:fable|opus|sonnet))$").unwrap().is_match(s)
}
fn class(model: &str) -> Option<&str> {
    let captures = regex::Regex::new(r"^(?:gpt-.+-)?(astra|sol|terra|luna)$")
        .unwrap()
        .captures(model)?;
    captures.get(1).map(|s| s.as_str())
}
fn percentage(v: &Value) -> Option<f64> {
    v.as_f64().filter(|n| n.is_finite() && *n >= 0.)
}
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
        Value::Number(v) => v.as_f64() != Some(0.),
    }
}
impl Snapshot {
    pub fn valid(&self, provider: Provider, now: SystemTime) -> bool {
        let Some(observed) = time(&Value::String(self.observed_at.clone())) else {
            return false;
        };
        let Some(expires) = time(&Value::String(self.expires_at.clone())) else {
            return false;
        };
        if self.schema_version != 2
            || self.provider != provider
            || self.source != provider.source()
            || !matches!(self.status.as_str(), "ok" | "unknown")
            || observed > now
            || expires <= observed
            || expires
                > observed + Duration::from_secs(if self.status == "ok" { TTL } else { BACKOFF })
        {
            return false;
        }
        if self
            .last_success_at
            .as_ref()
            .is_some_and(|s| time(&Value::String(s.clone())).is_none_or(|t| t > observed))
        {
            return false;
        }
        if !self.held_keys.iter().all(|k| key_ok(k))
            || !self
                .known_scopes
                .iter()
                .chain(&self.denied_scopes)
                .all(|s| scope_ok(s))
        {
            return false;
        }
        if self.windows.iter().any(|w| {
            !key_ok(&w.key)
                || !scope_ok(&w.scope)
                || w.used_percent.is_some_and(|n| !n.is_finite() || n < 0.)
                || w.window_seconds == Some(0)
                || w.resets_at
                    .as_ref()
                    .is_some_and(|s| time(&Value::String(s.clone())).is_none())
        }) {
            return false;
        }
        self.status != "ok"
            || (self.included_allowed.is_some()
                && !self.windows.is_empty()
                && !self.known_scopes.is_empty())
    }
    fn window(
        &mut self,
        key: String,
        scope: &str,
        row: &Value,
        fields: (&str, &str),
        duration: Option<u64>,
        now: SystemTime,
    ) -> Option<()> {
        row.as_object()?;
        let (used, reset) = fields;
        let resets = time(&row[reset]);
        if let Some(reset) = resets
            && reset > now
        {
            let expiry = time(&Value::String(self.expires_at.clone()))?;
            self.expires_at = stamp(expiry.min(reset));
        }
        self.windows.push(Window {
            key,
            scope: scope.into(),
            used_percent: percentage(&row[used]),
            resets_at: resets.map(stamp),
            window_seconds: duration,
            blocked: truthy(&row["locked_reason"])
                || matches!(row["severity"].as_str(), Some("blocked" | "exceeded")),
        });
        Some(())
    }
}
pub fn normalize(
    provider: Provider,
    data: &Value,
    now: SystemTime,
    previous: Option<&Snapshot>,
) -> Snapshot {
    normalize_inner(provider, data, now, previous)
        .filter(|s| s.included_allowed.is_some() && !s.windows.is_empty())
        .unwrap_or_else(|| {
            unknown(
                provider,
                now,
                previous.map(|s| s.held_keys.clone()).unwrap_or_default(),
            )
        })
}
fn normalize_inner(
    provider: Provider,
    data: &Value,
    now: SystemTime,
    previous: Option<&Snapshot>,
) -> Option<Snapshot> {
    data.as_object()?;
    let mut s = unknown(provider, now, vec![]);
    s.status = "ok".into();
    s.expires_at = stamp(now + Duration::from_secs(TTL));
    match provider {
        Provider::Codex => {
            s.included_allowed = data["ordinaryUsageAllowed"].as_bool();
            let empty = serde_json::Map::new();
            let buckets = match data.get("rateLimitsByLimitId") {
                None | Some(Value::Null) => &empty,
                Some(v) => v.as_object()?,
            };
            let general = buckets
                .get("codex")
                .filter(|v| truthy(v))
                .unwrap_or(&data["rateLimits"]);
            if general["limitId"] != "codex" {
                return None;
            }
            s.known_scopes.push("all".into());
            s.timed_denial = general["rateLimitReachedType"] == "rate_limit_reached";
            for (name, bucket) in std::iter::once(("codex", general)).chain(
                buckets
                    .iter()
                    .filter(|(k, v)| k.as_str() != "codex" && v.is_object())
                    .map(|(k, v)| (k.as_str(), v)),
            ) {
                let scope = if name == "codex" {
                    "all"
                } else {
                    let Some(c) = class(bucket["normalModelSlug"].as_str().unwrap_or_default())
                    else {
                        continue;
                    };
                    c
                };
                if bucket["spendControlReached"] == true
                    || (!bucket["rateLimitReachedType"].is_null()
                        && bucket["rateLimitReachedType"] != "rate_limit_reached")
                {
                    s.denied_scopes.push(scope.into());
                }
                for label in ["primary", "secondary"] {
                    let row = &bucket[label];
                    if row.is_null() {
                        continue;
                    }
                    let duration = row["windowDurationMins"]
                        .as_u64()
                        .filter(|n| *n > 0)
                        .and_then(|n| n.checked_mul(60));
                    s.window(
                        format!("{}.{label}", if name == "codex" { "codex" } else { scope }),
                        scope,
                        row,
                        ("usedPercent", "resetsAt"),
                        duration,
                        now,
                    )?;
                }
            }
        }
        Provider::Claude => {
            if data["rate_limits_available"] != true {
                return None;
            }
            let limits = data.get("rate_limits")?;
            limits.as_object()?;
            limits["seven_day"].as_object()?;
            if !limits["five_hour"].is_object() {
                let rows = limits["limits"].as_array()?;
                if !rows.iter().any(|r| r["kind"] == "weekly_all")
                    || rows.iter().any(|r| r["kind"] == "session")
                {
                    return None;
                }
            }
            s.included_allowed = Some(matches!(
                data["subscription_type"].as_str(),
                Some("pro" | "max" | "team" | "enterprise")
            ));
            s.known_scopes.push("all".into());
            for (key, scope, duration) in [
                ("five_hour", "all", 18000),
                ("seven_day", "all", 604800),
                ("seven_day_opus", "opus", 604800),
                ("seven_day_sonnet", "sonnet", 604800),
            ] {
                if limits.get(key).is_some() && !s.known_scopes.iter().any(|v| v == scope) {
                    s.known_scopes.push(scope.into());
                }
                let row = &limits[key];
                if !row.is_null() {
                    s.window(
                        key.into(),
                        scope,
                        row,
                        ("utilization", "resets_at"),
                        Some(duration),
                        now,
                    )?;
                }
            }
            let mut seen = std::collections::BTreeSet::new();
            if let Some(rows) = limits["limits"].as_array() {
                for row in rows.iter().filter(|r| r.is_object()) {
                    if !row["scope"].is_null() && !row["scope"].is_object() {
                        return None;
                    }
                    let model = row
                        .pointer("/scope/model/display_name")
                        .and_then(Value::as_str)
                        .map(str::to_lowercase);
                    if row["kind"] == "weekly_scoped" && model.is_some() {
                        let model = model.as_deref().unwrap();
                        if matches!(model, "fable" | "opus" | "sonnet")
                            && !truthy(&row["scope"]["surface"])
                        {
                            s.window(
                                format!("model.{model}"),
                                model,
                                row,
                                ("percent", "resets_at"),
                                Some(604800),
                                now,
                            )?;
                            seen.insert(model.to_string());
                        } else if row["is_active"] == true {
                            s.unrecognized_constraint = true;
                        }
                    } else if row["is_active"] == true
                        && !matches!(row["kind"].as_str(), Some("session" | "weekly_all"))
                    {
                        s.unrecognized_constraint = true;
                    }
                }
            }
            if !limits["model_scoped"].is_null() {
                for row in limits["model_scoped"]
                    .as_array()?
                    .iter()
                    .filter(|r| r.is_object())
                {
                    let model = row["display_name"]
                        .as_str()
                        .unwrap_or_default()
                        .to_lowercase();
                    if model.starts_with("haiku") {
                        s.unrecognized_constraint = true;
                    } else if matches!(model.as_str(), "fable" | "opus" | "sonnet")
                        && !seen.contains(&model)
                    {
                        s.window(
                            format!("model.{model}"),
                            &model,
                            row,
                            ("utilization", "resets_at"),
                            Some(604800),
                            now,
                        )?;
                        seen.insert(model);
                    }
                }
            }
            s.known_scopes.extend(seen);
            s.known_scopes.sort();
            s.known_scopes.dedup();
        }
    }
    let old = previous.map(|s| s.held_keys.as_slice()).unwrap_or_default();
    for w in &s.windows {
        if w.used_percent
            .is_some_and(|u| u >= 90. || (old.contains(&w.key) && u >= 80.))
        {
            s.held_keys.push(w.key.clone());
        }
    }
    s.held_keys.extend(
        old.iter()
            .filter(|key| {
                !s.windows
                    .iter()
                    .any(|w| &w.key == *key && w.used_percent.is_some())
            })
            .cloned(),
    );
    s.held_keys.sort();
    s.held_keys.dedup();
    Some(s)
}
pub fn assess(provider: Provider, model: &str, s: &Snapshot, now: SystemTime) -> Assessment {
    let requested_model = model;
    let mut a = Assessment {
        available: false,
        reason: UNAVAILABLE.into(),
        warnings: vec![],
        retry_at: None,
    };
    if !s.valid(provider, now)
        || s.status != "ok"
        || time(&Value::String(s.expires_at.clone())).is_none_or(|t| t <= now)
    {
        return a;
    }
    let model = match provider {
        Provider::Claude if model == "haiku" => "all",
        Provider::Claude if matches!(model, "opus" | "sonnet" | "fable") => model,
        Provider::Codex => match class(model) {
            Some(c) => c,
            None => {
                a.reason = "quota mapping is not known for this model".into();
                return a;
            }
        },
        _ => {
            a.reason = "quota mapping is not known for this model".into();
            return a;
        }
    };
    if s.unrecognized_constraint {
        return a;
    }
    let scope = if provider == Provider::Claude {
        model
    } else {
        "all"
    };
    if !s.known_scopes.iter().any(|v| v == scope) {
        a.reason = format!("{scope} quota scope is unknown");
        return a;
    }
    if s.denied_scopes.iter().any(|v| v == "all" || v == model) {
        a.reason = "backend reports a reached or spend-control limit".into();
        return a;
    }
    let windows: Vec<_> = s
        .windows
        .iter()
        .filter(|w| w.scope == "all" || w.scope == model)
        .collect();
    if !windows.iter().any(|w| w.scope == "all") || windows.iter().any(|w| w.used_percent.is_none())
    {
        a.reason = "applicable quota window is missing utilization".into();
        return a;
    }
    let constrained: Vec<_> = windows
        .iter()
        .copied()
        .filter(|w| {
            w.blocked
                || w.used_percent.unwrap() >= 90.
                || (s.held_keys.contains(&w.key) && w.used_percent.unwrap() >= 80.)
        })
        .collect();
    let denied = s.included_allowed == Some(false);
    let blocked = denied
        || constrained
            .iter()
            .any(|w| w.blocked || w.used_percent.unwrap() >= 100.);
    a.available = !blocked;
    a.reason = if denied {
        "backend denies included usage"
    } else if blocked {
        "quota blocked"
    } else if constrained.is_empty() {
        "included quota available"
    } else {
        "applicable quota is constrained"
    }
    .into();
    if !blocked {
        for w in &constrained {
            let reset = w.resets_at.as_deref().unwrap_or("unknown");
            let left = w
                .resets_at
                .as_ref()
                .and_then(|v| time(&Value::String(v.clone())))
                .map(|t| {
                    t.duration_since(now)
                        .unwrap_or_default()
                        .as_secs()
                        .to_string()
                })
                .unwrap_or_else(|| "unknown".into());
            a.warnings.push(format!("{} {requested_model}: {}% quota used; scope {}/{}; observed {}; reset {reset}; {left} seconds remaining{}; advisory warning",provider.name(),w.used_percent.unwrap(),w.scope,w.key,s.observed_at,if w.used_percent.unwrap()<90. {"; hysteresis warning held below 90%"} else {""}));
        }
    } else if !constrained.iter().any(|w| w.blocked) && (!denied || s.timed_denial) {
        let hard: Vec<_> = constrained
            .iter()
            .filter(|w| w.used_percent.unwrap() >= 100.)
            .collect();
        let resets: Option<Vec<_>> = hard
            .iter()
            .map(|w| {
                w.resets_at
                    .as_ref()
                    .and_then(|v| time(&Value::String(v.clone())))
                    .filter(|t| *t > now)
            })
            .collect();
        if let Some(resets) = resets {
            a.retry_at = resets.into_iter().max();
        }
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn now() -> SystemTime {
        time(&json!("2026-11-09T22:00:00Z")).unwrap()
    }
    fn claude(used: f64) -> Value {
        json!({"subscription_type":"max","rate_limits_available":true,"rate_limits":{
            "five_hour":{"utilization":used,"resets_at":"2026-11-10T00:00:00+00:00"},
            "seven_day":{"utilization":20,"resets_at":"2026-11-15T00:00:00Z"},
            "seven_day_opus":null,"seven_day_sonnet":null,
            "extra_usage":{"is_enabled":true}
        }})
    }
    fn codex(used: f64) -> Value {
        json!({"ordinaryUsageAllowed":true,"rateLimits":{"limitId":"codex","primary":{
            "usedPercent":used,"resetsAt":1794268800,"windowDurationMins":300
        }}})
    }
    #[test]
    fn threshold_and_explicit_scope_matrix() {
        for p in [Provider::Claude, Provider::Codex] {
            for (used, allowed, warned) in [
                (0., true, false),
                (89.9, true, false),
                (90., true, true),
                (99.9, true, true),
                (100., false, false),
                (150., false, false),
            ] {
                let data = if p == Provider::Claude {
                    claude(used)
                } else {
                    codex(used)
                };
                let s = normalize(p, &data, now(), None);
                let a = assess(
                    p,
                    if p == Provider::Claude {
                        "opus"
                    } else {
                        "gpt-6-astra"
                    },
                    &s,
                    now(),
                );
                assert_eq!(a.available, allowed, "{p:?} {used}");
                assert_eq!(!a.warnings.is_empty(), warned, "{p:?} {used}");
            }
        }
        let s = normalize(Provider::Claude, &claude(1.), now(), None);
        assert!(!assess(Provider::Claude, "fable", &s, now()).available);
        assert!(!assess(Provider::Claude, "unknown", &s, now()).available);
        assert!(!assess(Provider::Codex, "astra", &s, now()).available);
    }
    #[test]
    fn haiku_does_not_wait_below_shared_warning_threshold() {
        for used in [0., 89.9] {
            let mut data = claude(used);
            data["rate_limits"]["seven_day"]["utilization"] = json!(used);
            let s = normalize(Provider::Claude, &data, now(), None);
            let a = assess(Provider::Claude, "haiku", &s, now());
            assert!(a.available, "shared usage {used}");
            assert!(a.warnings.is_empty(), "shared usage {used}");
        }
    }
    #[test]
    fn haiku_waits_when_either_shared_window_is_exhausted() {
        for key in ["five_hour", "seven_day"] {
            let mut data = claude(1.);
            data["rate_limits"][key]["utilization"] = json!(100.);
            let s = normalize(Provider::Claude, &data, now(), None);
            assert!(
                !assess(Provider::Claude, "haiku", &s, now()).available,
                "{key}"
            );
        }
    }
    #[test]
    fn haiku_is_not_blocked_by_other_model_windows() {
        let mut data = claude(1.);
        for key in ["seven_day_opus", "seven_day_sonnet"] {
            data["rate_limits"][key] = json!({"utilization":100.});
        }
        data["rate_limits"]["limits"] = json!([{
            "kind":"weekly_scoped","scope":{"model":{"display_name":"Fable"}},
            "percent":100.,"is_active":true
        }]);
        let s = normalize(Provider::Claude, &data, now(), None);
        let a = assess(Provider::Claude, "haiku", &s, now());
        assert!(a.available);
        assert!(a.warnings.is_empty());
    }
    #[test]
    fn haiku_waits_when_model_scoped_contains_a_haiku_constraint() {
        for (name, available) in [("Fable 5", true), ("Haiku", false), ("HAIKU 5", false)] {
            let mut data = claude(1.);
            data["rate_limits"]["model_scoped"] = json!([{
                "display_name":name,"utilization":100.
            }]);
            let s = normalize(Provider::Claude, &data, now(), None);
            assert_eq!(
                assess(Provider::Claude, "haiku", &s, now()).available,
                available,
                "{name}"
            );
        }
    }
    #[test]
    fn haiku_shared_warnings_are_held_until_usage_falls_below_eighty() {
        for key in ["five_hour", "seven_day"] {
            let mut data = claude(1.);
            let mut previous = None;
            for (used, warned) in [(90., true), (85., true), (79., false)] {
                data["rate_limits"][key]["utilization"] = json!(used);
                let s = normalize(Provider::Claude, &data, now(), previous.as_ref());
                let a = assess(Provider::Claude, "haiku", &s, now());
                assert!(a.available, "{key} {used}");
                assert_eq!(!a.warnings.is_empty(), warned, "{key} {used}");
                previous = Some(s);
            }
        }
    }
    #[test]
    fn haiku_waits_for_a_shared_window_without_utilization() {
        for key in ["five_hour", "seven_day"] {
            let mut data = claude(1.);
            data["rate_limits"][key]["utilization"] = Value::Null;
            let s = normalize(Provider::Claude, &data, now(), None);
            assert!(
                !assess(Provider::Claude, "haiku", &s, now()).available,
                "{key}"
            );
        }
    }
    #[test]
    fn reset_caps_freshness_and_naive_or_future_observations_refuse() {
        let mut d = claude(1.);
        d["rate_limits"]["five_hour"]["resets_at"] = json!("2026-11-09T22:00:10Z");
        let s = normalize(Provider::Claude, &d, now(), None);
        assert_eq!(
            time(&json!(s.expires_at)),
            Some(now() + Duration::from_secs(10))
        );
        assert!(
            !assess(
                Provider::Claude,
                "opus",
                &s,
                now() + Duration::from_secs(10)
            )
            .available
        );
        assert!(!assess(Provider::Claude, "opus", &s, now() - Duration::from_secs(1)).available);
        assert_eq!(time(&json!("2026-11-09T22:00:00")), None);
        assert_eq!(time(&json!("2026-11-09T23:00:00+01:00")), Some(now()));
        assert_eq!(time(&json!("2026-11-09T21:00:00-01:00")), Some(now()));
        assert_eq!(
            time(&json!("2026-11-09T22:00:00.125Z")),
            Some(now() + Duration::from_millis(125))
        );
        assert_eq!(time(&json!(true)), None);
        assert_eq!(time(&json!(-1)), None);
        assert_eq!(time(&json!("2026-02-31T00:00:00Z")), None);
    }
    #[test]
    fn held_warning_releases_below_eighty_and_survives_unknown() {
        let high = normalize(Provider::Claude, &claude(95.), now(), None);
        let failed = unknown(Provider::Claude, now(), high.held_keys.clone());
        let held = normalize(Provider::Claude, &claude(85.), now(), Some(&failed));
        assert!(
            !assess(Provider::Claude, "opus", &held, now())
                .warnings
                .is_empty()
        );
        let low = normalize(Provider::Claude, &claude(79.), now(), Some(&held));
        assert!(
            assess(Provider::Claude, "opus", &low, now())
                .warnings
                .is_empty()
        );
    }
    #[test]
    fn denial_missing_utilization_and_active_unknown_constraints_fail_closed() {
        let mut data = codex(10.);
        data["ordinaryUsageAllowed"] = json!(false);
        assert!(
            !assess(
                Provider::Codex,
                "sol",
                &normalize(Provider::Codex, &data, now(), None),
                now()
            )
            .available
        );
        for bad in [Value::Null, json!(true), json!("10"), json!(-1)] {
            let mut data = claude(1.);
            data["rate_limits"]["five_hour"]["utilization"] = bad;
            assert!(
                !assess(
                    Provider::Claude,
                    "opus",
                    &normalize(Provider::Claude, &data, now(), None),
                    now()
                )
                .available
            );
        }
        let mut data = claude(1.);
        data["rate_limits"]["limits"] = json!([{"kind":"unknown","is_active":true}]);
        assert!(
            !assess(
                Provider::Claude,
                "opus",
                &normalize(Provider::Claude, &data, now(), None),
                now()
            )
            .available
        );
    }
    #[test]
    fn fable_requires_explicit_scope_and_other_model_limits_do_not_block() {
        let mut data = claude(1.);
        data["rate_limits"]["limits"] = json!([{"kind":"weekly_scoped","scope":{"model":{"display_name":"Fable"}},"percent":10,"resets_at":"2026-11-10T00:00:00Z"}]);
        let s = normalize(Provider::Claude, &data, now(), None);
        assert!(assess(Provider::Claude, "fable", &s, now()).available);
        let mut data = codex(1.);
        data["rateLimitsByLimitId"] =
            json!({"other":{"normalModelSlug":"gpt-6-sol","spendControlReached":true}});
        let s = normalize(Provider::Codex, &data, now(), None);
        assert!(assess(Provider::Codex, "astra", &s, now()).available);
        assert!(!assess(Provider::Codex, "sol", &s, now()).available);
    }
    #[test]
    fn normalized_storage_excludes_raw_fields_and_rejects_tampering() {
        let mut d = claude(1.);
        d["DO_NOT_LOG"] = json!("private sentinel");
        let s = normalize(Provider::Claude, &d, now(), None);
        let text = serde_json::to_string(&s).unwrap();
        assert!(!text.contains("DO_NOT_LOG"));
        assert!(!text.contains("private sentinel"));
        assert!(s.valid(Provider::Claude, now()));
        let mut v = serde_json::to_value(&s).unwrap();
        v["unexpected"] = json!(true);
        assert!(serde_json::from_value::<Snapshot>(v).is_err());
        let mut bad = s.clone();
        bad.held_keys.push("private sentinel".into());
        assert!(!bad.valid(Provider::Claude, now()));
        let mut bad = s.clone();
        bad.schema_version = 1;
        assert!(!bad.valid(Provider::Claude, now()));
        let mut bad = s;
        bad.expires_at = stamp(now() + Duration::from_secs(121));
        assert!(!bad.valid(Provider::Claude, now()));
    }
    #[test]
    fn sessionless_and_invalid_windows_preserve_scope_refusals() {
        let mut d = claude(20.);
        d["rate_limits"]["five_hour"] = Value::Null;
        d["rate_limits"]["limits"] = json!([{"kind":"weekly_all"}]);
        let s = normalize(Provider::Claude, &d, now(), None);
        assert!(assess(Provider::Claude, "opus", &s, now()).available);
        d["rate_limits"]["limits"] = json!([{"kind":"session"},{"kind":"weekly_all"}]);
        assert!(
            !assess(
                Provider::Claude,
                "opus",
                &normalize(Provider::Claude, &d, now(), None),
                now()
            )
            .available
        );
        for malformed in [json!({}), json!("garbage"), json!(true)] {
            let mut d = codex(20.);
            d["rateLimits"]["secondary"] = malformed;
            assert!(
                !assess(
                    Provider::Codex,
                    "astra",
                    &normalize(Provider::Codex, &d, now(), None),
                    now()
                )
                .available
            );
        }
    }
    #[test]
    fn recovery_requires_known_future_resets_for_every_hard_limit() {
        let mut d = claude(100.);
        d["rate_limits"]["seven_day"]["utilization"] = json!(100.);
        let s = normalize(Provider::Claude, &d, now(), None);
        assert_eq!(
            assess(Provider::Claude, "opus", &s, now()).retry_at,
            time(&json!("2026-11-15T00:00:00Z"))
        );
        for reset in [Value::Null, json!("bad"), json!("2026-11-08T22:00:00Z")] {
            d["rate_limits"]["seven_day"]["resets_at"] = reset;
            let a = assess(
                Provider::Claude,
                "opus",
                &normalize(Provider::Claude, &d, now(), None),
                now(),
            );
            assert!(!a.available);
            assert!(a.retry_at.is_none());
        }
        let mut d = codex(100.);
        d["ordinaryUsageAllowed"] = json!(false);
        assert!(
            assess(
                Provider::Codex,
                "astra",
                &normalize(Provider::Codex, &d, now(), None),
                now()
            )
            .retry_at
            .is_none()
        );
        d["rateLimits"]["rateLimitReachedType"] = json!("rate_limit_reached");
        assert!(
            assess(
                Provider::Codex,
                "astra",
                &normalize(Provider::Codex, &d, now(), None),
                now()
            )
            .retry_at
            .is_some()
        );
    }
    #[test]
    fn same_class_hysteresis_does_not_warn_for_a_low_sibling() {
        let mut d = codex(10.);
        d["rateLimitsByLimitId"] = json!({
            "first":{"normalModelSlug":"gpt-6-astra","primary":{"usedPercent":95,"resetsAt":1794268800}},
            "second":{"normalModelSlug":"astra","primary":{"usedPercent":50,"resetsAt":1794268800}}
        });
        let high = normalize(Provider::Codex, &d, now(), None);
        let a = assess(Provider::Codex, "gpt-6-astra", &high, now());
        assert_eq!(a.warnings.len(), 1);
        assert!(a.warnings[0].contains("gpt-6-astra"));
        d["rateLimitsByLimitId"]["first"]["primary"]["usedPercent"] = json!(85);
        let held = normalize(Provider::Codex, &d, now(), Some(&high));
        assert_eq!(
            assess(Provider::Codex, "astra", &held, now())
                .warnings
                .len(),
            1
        );
        d["rateLimitsByLimitId"]["first"]["primary"]["usedPercent"] = json!(79);
        let low = normalize(Provider::Codex, &d, now(), Some(&held));
        assert!(
            assess(Provider::Codex, "astra", &low, now())
                .warnings
                .is_empty()
        );
    }
}
