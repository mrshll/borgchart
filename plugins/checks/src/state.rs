//! Atomic evidence snapshots, health and pure signal transitions.
use crate::config;
use borgchart::plugin;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Record {
    pub command: Vec<String>,
    pub last_run: String,
    pub status: String,
    pub exit_code: Value,
    pub stderr: String,
    pub output: String,
    pub digest: String,
    pub last_success: String,
    pub good_output: String,
    pub previous_output: String,
}
#[derive(Default, Serialize)]
pub struct Snapshot {
    pub checks: BTreeMap<String, Record>,
    pub pending: Vec<String>,
    pub uncertain_sends: u64,
}
impl Snapshot {
    pub fn read(store: &plugin::State) -> Self {
        let raw: Value = store.read_json("checks.json");
        Self {
            checks: raw["checks"]
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(name, v)| config::valid_name(name) && v.is_object())
                .filter_map(|(name, v)| {
                    serde_json::from_value(v.clone())
                        .ok()
                        .map(|v| (name.clone(), v))
                })
                .collect(),
            pending: raw["pending"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
            uncertain_sends: raw["uncertain_sends"].as_u64().unwrap_or(0),
        }
    }
    pub fn prune(&mut self, names: &BTreeSet<String>) {
        self.checks.retain(|name, _| names.contains(name));
        self.pending.retain(|s| valid_signal(s, names));
    }
    pub fn save(&self, store: &plugin::State) -> std::io::Result<()> {
        store.write_json("checks.json", self)
    }
    pub fn health(&self, store: &plugin::State, detail: &str) -> std::io::Result<()> {
        let failing: Vec<_> = self
            .checks
            .iter()
            .filter(|(_, r)| r.status == "failing")
            .map(|(n, _)| n.as_str())
            .collect();
        let message = if !detail.is_empty() {
            detail.to_owned()
        } else if failing.is_empty() {
            "watching".into()
        } else {
            format!("failing: {}", failing.join(", "))
        };
        let checks: BTreeMap<_, _> = self
            .checks
            .iter()
            .map(|(n, r)| (n, json!({"status":r.status,"last_run":r.last_run})))
            .collect();
        store.health(detail.is_empty() && failing.is_empty(), &message,
            json!({"uncertain_sends":self.uncertain_sends,"pending_sends":self.pending.len(),"checks":checks}))
    }
}
pub fn valid_signal(line: &str, names: &BTreeSet<String>) -> bool {
    line.split_once(' ').is_some_and(|(n, s)| {
        names.contains(n)
            && config::valid_name(n)
            && ["changed", "failing", "recovered"].contains(&s)
    })
}
pub fn capped(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut end = text.len().min(65536);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].into()
}
pub fn update(
    record: &mut Record,
    check: &config::Check,
    output: &[u8],
    stderr: &[u8],
    code: Value,
) -> Vec<String> {
    let was_failing = record.status == "failing";
    record.command = check.run.clone();
    record.output = capped(output);
    let errors = String::from_utf8_lossy(stderr);
    record.stderr = errors
        .lines()
        .last()
        .unwrap_or("")
        .chars()
        .rev()
        .take(500)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    record.exit_code = code.clone();
    let mut signals = Vec::new();
    if code != json!(0) {
        record.status = "failing".into();
        if !was_failing {
            signals.push("failing");
        }
    } else {
        let filtered = check
            .ignore
            .as_ref()
            .map(|re| re.replace_all(output, &b""[..]));
        let digest = format!(
            "{:x}",
            Sha256::digest(filtered.as_deref().unwrap_or(output))
        );
        if was_failing {
            signals.push("recovered");
        }
        if !record.digest.is_empty() && digest != record.digest {
            signals.push("changed");
            record.previous_output = record.good_output.clone();
        }
        record.status = "ok".into();
        record.digest = digest;
        record.good_output = record.output.clone();
    }
    signals
        .into_iter()
        .map(|s| format!("{} {s}", check.name))
        .collect()
}
pub fn one_line(text: &str) -> String {
    plugin::escape(text)
        .replace('\n', "\\x0a")
        .replace('\t', "\\x09")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn baseline_failure_recovery_and_changed_signals_use_the_last_good_digest() {
        let check = config::parse("- {name: probe, run: [printf], every_minutes: 1}")
            .unwrap()
            .remove(0);
        let mut record = Record::default();
        assert!(update(&mut record, &check, b"first", b"", json!(0)).is_empty());
        assert_eq!(
            update(&mut record, &check, b"partial", b"", json!(7)),
            ["probe failing"]
        );
        assert!(update(&mut record, &check, b"partial", b"", json!(7)).is_empty());
        assert_eq!(
            update(&mut record, &check, b"second", b"", json!(0)),
            ["probe recovered", "probe changed"]
        );
        assert_eq!(record.previous_output, "first");
        assert!(update(&mut record, &check, b"second", b"", json!(0)).is_empty());
    }
    #[test]
    fn ignored_text_cannot_hide_distinct_invalid_utf8_bytes() {
        let check = config::parse(
            "- {name: probe, run: [printf], every_minutes: 1, ignore: 'volatile:[0-9]+'}",
        )
        .unwrap()
        .remove(0);
        let mut record = Record::default();
        update(&mut record, &check, b"\xff volatile:1", b"", json!(0));
        assert!(update(&mut record, &check, b"\xff volatile:2", b"", json!(0)).is_empty());
        assert_eq!(
            update(&mut record, &check, b"\xfe volatile:2", b"", json!(0)),
            ["probe changed"]
        );
    }
    #[test]
    fn pending_filter_rejects_forged_statuses_instructions_and_removed_names() {
        let names = BTreeSet::from(["probe".into()]);
        assert!(valid_signal("probe changed", &names));
        for line in [
            "probe changed; injected",
            "probe changed\nother failing",
            "removed changed",
            "probe rebooted",
            "probe  changed",
        ] {
            assert!(!valid_signal(line, &names));
        }
    }
}
