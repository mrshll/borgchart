//! Strict, atomic intake snapshots; a damaged journal is never reset.
use crate::{config::Entry, discover::Changed};
use borgchart::plugin;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Discovery re-lists this far behind its cursor, for `updated_at` that arrives late.
pub const OVERLAP: Duration = Duration::from_secs(300);
/// A version younger than this is processed again at the next poll, for a
/// timeline that lags its item's `updated_at`.
const SETTLE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Kind {
    Mention,
    Assigned,
    Review {
        state: String,
        commit: Option<String>,
    },
    Merged {
        commit: Option<String>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingEvent {
    pub id: String,
    #[serde(default)]
    pub reactable: bool,
    pub repository: String,
    pub number: u64,
    pub is_pull_request: bool,
    pub kind: Kind,
    pub recipient: String,
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    pub at: SystemTime,
    pub drained: SystemTime,
    /// Nothing before this is discovered: process start, turning intake on, or the entry's addition.
    pub floor: SystemTime,
    /// Settled `updated_at` versions inside the overlap, skipped when listed again.
    pub seen: BTreeMap<String, SystemTime>,
}
impl Cursor {
    pub fn new(now: SystemTime) -> Self {
        Self {
            at: now,
            drained: now,
            floor: now,
            seen: BTreeMap::new(),
        }
    }
    pub fn since(&self) -> SystemTime {
        self.at.checked_sub(OVERLAP).unwrap_or(UNIX_EPOCH)
    }
    pub fn mark(&mut self, item: &Changed, now: SystemTime) {
        self.at = self.at.max(item.updated_at);
        if now.duration_since(item.updated_at).unwrap_or_default() >= SETTLE {
            self.seen.insert(item.node_id.clone(), item.updated_at);
        }
    }
}
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub cursors: BTreeMap<String, Cursor>,
    /// Event IDs already admitted, for the timeline stage.
    pub seen: BTreeMap<String, SystemTime>,
    pub pending: Vec<PendingEvent>,
    #[serde(default)]
    pub reactions: BTreeSet<String>,
}
impl Snapshot {
    /// Cursors start again at each process start (no replay); seen and pending carry over.
    pub fn start(store: &plugin::State) -> Result<Self, String> {
        let mut snapshot: Self = store.read_json_strict("intake.json")?.unwrap_or_default();
        snapshot.cursors.clear();
        Ok(snapshot)
    }
    pub fn entries(&mut self, entries: &[Entry], now: SystemTime) {
        self.cursors
            .retain(|key, _| entries.iter().any(|e| &e.key() == key));
        for entry in entries {
            self.cursors
                .entry(entry.key())
                .or_insert_with(|| Cursor::new(now));
        }
    }
    pub fn prune(&mut self, now: SystemTime) {
        let cutoff = now
            .checked_sub(Duration::from_secs(86400))
            .unwrap_or(UNIX_EPOCH);
        self.seen.retain(|_, at| *at >= cutoff);
        for cursor in self.cursors.values_mut() {
            let since = cursor.since();
            cursor.seen.retain(|_, at| *at >= since);
        }
    }
    pub fn save(&self, store: &plugin::State) -> Result<(), String> {
        store.write_json("intake.json", self).map_err(|_| {
            "cannot save intake.json; check state directory permissions and retry".into()
        })
    }
}
