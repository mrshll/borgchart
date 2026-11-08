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
    /// directory, or one holding only empty Codex masks, is new; every other
    /// layout needs our exact identity.
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
            let name = entry.file_name();
            // Codex masks these names with empty directories in every writable
            // root. Its temporary mount points can vanish during inspection.
            let mask = matches!(name.to_str(), Some(".agents" | ".aws" | ".codex" | ".git"));
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(e) if mask && e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(damaged("a storage entry cannot be read")),
            };
            if mask && kind.is_dir() {
                let first = std::fs::read_dir(entry.path())
                    .and_then(|mut contents| contents.next().transpose());
                match first {
                    Ok(None) => continue,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    _ => {}
                }
            }
            any = true;
            match name.to_str() {
                Some("store.json") if kind.is_file() => identity = true,
                Some("lock") if kind.is_file() => {}
                Some("records" | "acks") if kind.is_dir() => {}
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
    /// An absent or empty directory, or one holding only empty Codex masks,
    /// may be initialised; other unrecognised layouts are refused.
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
    pub fn acks_dir(&self) -> PathBuf {
        self.root.join("acks")
    }
    /// The acknowledgement of one record by its recipient.
    pub fn ack(&self, record_id: &str) -> PathBuf {
        self.acks_dir().join(format!("{record_id}.json"))
    }
}
