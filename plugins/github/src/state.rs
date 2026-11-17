//! Strict, atomic intake snapshots; a damaged journal is never reset.
use crate::{config::Entry, discover::Changed};
use borgchart::plugin;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Later intake stages add typed events here before delivery.
#[derive(Debug, Serialize, Deserialize)]
pub enum PendingEvent {}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    pub at: SystemTime,
    pub floor: SystemTime,
    /// Update versions processed in the current pass; cleared once it drains.
    pub processed: BTreeMap<String, SystemTime>,
}
impl Cursor {
    pub fn new(now: SystemTime) -> Self {
        Self {
            at: now,
            floor: now,
            processed: BTreeMap::new(),
        }
    }
    pub fn mark(&mut self, item: &Changed) {
        self.at = self.at.max(item.updated_at);
        self.processed.insert(item.node_id.clone(), item.updated_at);
    }
    pub fn prune(&mut self) {
        let cutoff = self
            .at
            .checked_sub(Duration::from_secs(86400))
            .unwrap_or(UNIX_EPOCH);
        self.processed.retain(|_, at| *at >= cutoff);
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub epoch: SystemTime,
    pub machine_id: u64,
    pub owner_id: u64,
    pub cursors: BTreeMap<String, Cursor>,
    pub seen: BTreeMap<String, SystemTime>,
    pub pending: Vec<PendingEvent>,
}
impl Snapshot {
    pub fn read(store: &plugin::State) -> Result<Option<Self>, String> {
        store.read_json_strict("intake.json")
    }
    pub fn start(
        previous: Option<Self>,
        epoch: SystemTime,
        machine_id: u64,
        owner_id: u64,
    ) -> Self {
        Self {
            epoch,
            machine_id,
            owner_id,
            cursors: BTreeMap::new(),
            seen: previous
                .as_ref()
                .map(|s| s.seen.clone())
                .unwrap_or_default(),
            pending: previous.map(|s| s.pending).unwrap_or_default(),
        }
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
            cursor.prune();
        }
    }
    pub fn save(&self, store: &plugin::State) -> Result<(), String> {
        store.write_json("intake.json", self).map_err(|_| {
            "cannot save intake.json; check state directory permissions and retry".into()
        })
    }
}
