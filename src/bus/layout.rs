//! Where everything for one run lives: `<bus.root>/<run>/`.
//!
//! Ownership of each area is fixed so pieces never write each other's files:
//!
//! ```text
//! <bus.root>/<run>/                     0700
//!   run.json                            run manifest (immutable once written)
//!   runtime/                            lifecycle: lock, generations, incarnations
//!   bus/                                records, operation keys, acks, publication lock
//!   attention/                          wake/alert worker state and alerts
//! ```

use std::path::{Path, PathBuf};

use crate::deploy::BusConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPaths {
    pub root: PathBuf,
}

/// Create the run directory if needed and check it (and the bus root) is
/// private: see [`super::fsx::ensure_private_tree`]. Every writer calls this
/// before its first write into a run.
pub fn ensure_run_dir(config: &BusConfig) -> std::io::Result<RunPaths> {
    let paths = RunPaths::new(config);
    super::fsx::ensure_private_tree(&config.root, &paths.root)?;
    Ok(paths)
}

impl RunPaths {
    pub fn new(config: &BusConfig) -> Self {
        RunPaths {
            root: config.root.join(&config.run),
        }
    }

    pub fn from_root(root: &Path) -> Self {
        RunPaths {
            root: root.to_path_buf(),
        }
    }

    /// The run manifest: identity, frozen topology and configuration digest.
    pub fn manifest(&self) -> PathBuf {
        self.root.join("run.json")
    }

    // --- runtime/: lifecycle and runtime binding ---

    pub fn runtime_dir(&self) -> PathBuf {
        self.root.join("runtime")
    }
    /// Serializes `up`, `down`, member starts/stops and rebinds for this run.
    pub fn lifecycle_lock(&self) -> PathBuf {
        self.runtime_dir().join("lifecycle.lock")
    }
    /// One immutable file per runtime generation: `generations/<n:06>.json`.
    pub fn generations_dir(&self) -> PathBuf {
        self.runtime_dir().join("generations")
    }
    pub fn generation(&self, n: u64) -> PathBuf {
        self.generations_dir().join(format!("{n:06}.json"))
    }
    /// A member's current incarnation (replaced at each start).
    pub fn current_incarnation(&self, member: &str) -> PathBuf {
        self.runtime_dir()
            .join("incarnations")
            .join(format!("{member}.json"))
    }
    /// Every incarnation a member has had, immutable.
    pub fn incarnation_history(&self, member: &str, incarnation: &str) -> PathBuf {
        self.runtime_dir()
            .join("incarnations")
            .join(member)
            .join(format!("{incarnation}.json"))
    }

    // --- bus/: records ---

    pub fn bus_dir(&self) -> PathBuf {
        self.root.join("bus")
    }
    /// Held (flock, exclusive) while assigning a sequence number and publishing.
    pub fn bus_lock(&self) -> PathBuf {
        self.bus_dir().join("lock")
    }
    pub fn records_dir(&self) -> PathBuf {
        self.bus_dir().join("records")
    }
    /// Records are named by sequence number: `records/<seq:010>.json`.
    pub fn record(&self, seq: u64) -> PathBuf {
        self.records_dir().join(format!("{seq:010}.json"))
    }
    /// Operation keys for one actor slot: `keys/<slot>/<sha256(key)>.json`.
    pub fn keys_dir(&self, slot: &str) -> PathBuf {
        self.bus_dir().join("keys").join(slot)
    }
    pub fn acks_dir(&self) -> PathBuf {
        self.bus_dir().join("acks")
    }
    /// The acknowledgement of one record by its recipient.
    pub fn ack(&self, record_id: &str) -> PathBuf {
        self.acks_dir().join(format!("{record_id}.json"))
    }

    // --- attention/: wake and alert worker ---

    pub fn attention_dir(&self) -> PathBuf {
        self.root.join("attention")
    }
    /// Held by the one running attention worker.
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
    /// known. Rebinding requires this directory to be empty.
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
