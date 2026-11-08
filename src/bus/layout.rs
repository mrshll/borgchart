//! One message store for the lifetime of an instance.

use super::fsx;
use crate::state::StateDir;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorePaths {
    pub root: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoreIdentity {
    format: u32,
    instance: String,
}

impl StorePaths {
    pub fn new(state: &StateDir) -> Self {
        Self::from_root(&state.messages_dir())
    }

    pub fn from_root(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    pub fn identity(&self) -> PathBuf {
        self.root.join("store.json")
    }

    /// Recognise the store before reading any messages. An absent or empty
    /// directory is new; every other layout needs our exact identity.
    pub fn validate(&self, instance: &str) -> Result<bool, String> {
        let damaged = |detail: &str| {
            format!(
                "instance `{instance}`'s message storage is damaged: {detail}; repair it by hand, or create a new instance (`brg create`)"
            )
        };
        match std::fs::symlink_metadata(&self.root) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Ok(m) if m.is_dir() => {}
            _ => return Err(damaged("its messages folder cannot be read as a directory")),
        }
        let entries = std::fs::read_dir(&self.root)
            .map_err(|_| damaged("its messages folder cannot be listed"))?;
        let mut any = false;
        let mut identity = false;
        for entry in entries {
            let entry = entry.map_err(|_| damaged("its messages folder cannot be listed"))?;
            any = true;
            let name = entry.file_name();
            let kind = entry
                .file_type()
                .map_err(|_| damaged("a storage entry cannot be read"))?;
            match name.to_str() {
                Some("store.json") if kind.is_file() => identity = true,
                Some("lock") if kind.is_file() => {}
                Some("records" | "acks" | "keys" | "attention") if kind.is_dir() => {}
                _ => return Err(damaged("its messages folder holds an unexpected entry")),
            }
        }
        if !any {
            return Ok(false);
        }
        if !identity {
            return Err(damaged("store.json is missing"));
        }
        let bytes =
            std::fs::read(self.identity()).map_err(|_| damaged("store.json cannot be read"))?;
        let identity: StoreIdentity =
            serde_json::from_slice(&bytes).map_err(|_| damaged("store.json cannot be read"))?;
        if identity.format != super::SCHEMA {
            return Err(damaged("store.json names another format"));
        }
        if identity.instance != instance {
            return Err(damaged("store.json names another instance"));
        }
        Ok(true)
    }

    /// Called under the instance's lifecycle lock, after every start check.
    /// Never initialise over a nonempty, unrecognised directory.
    pub fn initialize(&self, state: &StateDir) -> Result<(), String> {
        if self.validate(&state.id)? {
            return Ok(());
        }
        fsx::ensure_private_tree(&state.root, &self.root)
            .map_err(|e| format!("cannot create message storage: {e}"))?;
        let identity = StoreIdentity {
            format: super::SCHEMA,
            instance: state.id.clone(),
        };
        let bytes = serde_json::to_vec(&identity).map_err(|e| e.to_string())?;
        fsx::write_new(&self.identity(), &bytes)
            .map_err(|e| format!("cannot write store.json: {e}"))?;
        Ok(())
    }

    pub fn bus_dir(&self) -> PathBuf {
        self.root.clone()
    }

    /// Publication lock, taken after the instance's lifecycle lock when both apply.
    pub fn bus_lock(&self) -> PathBuf {
        self.root.join("lock")
    }
    pub fn records_dir(&self) -> PathBuf {
        self.root.join("records")
    }
    /// Records are named by sequence number: `records/<seq:010>.json`.
    pub fn record(&self, seq: u64) -> PathBuf {
        self.records_dir().join(format!("{seq:010}.json"))
    }
    /// Operation keys for one actor slot: `keys/<slot>/<sha256(key)>.json`.
    pub fn keys_dir(&self, slot: &str) -> PathBuf {
        self.root.join("keys").join(slot)
    }
    pub fn acks_dir(&self) -> PathBuf {
        self.root.join("acks")
    }
    /// The acknowledgement of one record by its recipient.
    pub fn ack(&self, record_id: &str) -> PathBuf {
        self.acks_dir().join(format!("{record_id}.json"))
    }

    // --- attention/: wake and alert worker ---

    pub fn attention_dir(&self) -> PathBuf {
        self.root.join("attention")
    }
    /// Held by the instance's one running watcher.
    pub fn worker_lock(&self) -> PathBuf {
        self.attention_dir().join("worker.lock")
    }
    pub fn worker_state(&self) -> PathBuf {
        self.attention_dir().join("worker.json")
    }
    /// An incarnation's observation watermark (IDs returned to it by `inbox`).
    pub fn observation(&self, member: &str, incarnation: &str) -> PathBuf {
        self.attention_dir()
            .join("observed")
            .join(member)
            .join(format!("{incarnation}.json"))
    }
    /// A wake attempt whose intent was recorded and whose outcome is not yet
    /// known.
    pub fn inflight_dir(&self) -> PathBuf {
        self.attention_dir().join("inflight")
    }
    pub fn inflight(&self, member: &str) -> PathBuf {
        self.inflight_dir().join(format!("{member}.json"))
    }
    pub fn alerts_dir(&self) -> PathBuf {
        self.attention_dir().join("alerts")
    }
    pub fn alert(&self, id: &str) -> PathBuf {
        self.alerts_dir().join(format!("{id}.json"))
    }
}
