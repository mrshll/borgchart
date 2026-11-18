//! Strict, atomic intake snapshots; a damaged journal is never reset.
use crate::{config::Entry, discover::Changed};
use borgchart::plugin;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Discovery re-lists this far behind its cursor, for `updated_at` that arrives late.
pub const OVERLAP: Duration = Duration::from_secs(300);
/// A version younger than this is processed again at the next poll, for a
/// timeline that lags its item's `updated_at`.
const SETTLE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Kind {
    Cutoff,
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
    /// Nothing before this replay or entry-addition boundary is discovered.
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
}
impl Snapshot {
    /// Resume discovery, admitted IDs and pending deliveries across process starts.
    pub fn start(store: &plugin::State) -> Result<Self, String> {
        Ok(store.read_json_strict("intake.json")?.unwrap_or_default())
    }
    pub fn entries(
        &mut self,
        entries: &[Entry],
        now: SystemTime,
        resume: bool,
    ) -> Option<SystemTime> {
        self.cursors
            .retain(|key, _| entries.iter().any(|e| &e.key() == key));
        let cutoff = now
            .checked_sub(Duration::from_secs(72 * 3600))
            .unwrap_or(UNIX_EPOCH);
        let saved = self.cursors.values().map(|c| c.at.min(c.drained)).min();
        if resume {
            for cursor in self.cursors.values_mut() {
                cursor.at = cursor.at.max(cutoff);
                cursor.drained = cursor.drained.max(cutoff);
                cursor.floor = cursor.floor.max(cutoff);
            }
        }
        for entry in entries {
            self.cursors
                .entry(entry.key())
                .or_insert_with(|| Cursor::new(now));
        }
        saved.filter(|saved| resume && *saved < cutoff)
    }
    pub fn prune(&mut self, now: SystemTime) {
        let cutoff = now
            .checked_sub(Duration::from_secs(4 * 86400))
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
