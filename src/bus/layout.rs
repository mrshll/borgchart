//! Where a team's message storage lives.
//!
//! Each deployment has a directory under `bus.root`, named by its
//! deployment ID. Inside it, `current` names the storage in use (a *run*,
//! internally; users never see or name runs). brg starts a new one when a
//! team's configuration or message format changes, or when the storage
//! reaches its age limit, and carries unfinished work over.
//!
//! ```text
//! <bus.root>/<deployment-id>/           0700
//!   current                             the run in use (e.g. `r2`)
//!   r1/, r2/, …                         one run each:
//!     run.json                          run manifest (immutable once written)
//!     runtime/                          lifecycle: lock, incarnations
//!     bus/                              records, operation keys, acks, publication lock
//!     attention/                        watcher state and alerts
//! ```
//!
//! Ownership of each area is fixed so pieces never write each other's files.

use std::path::{Path, PathBuf};

use crate::deploy::BusConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPaths {
    pub root: PathBuf,
}

/// One deployment's message storage: `<bus.root>/<deployment-id>/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamStore {
    pub base: PathBuf,
    pub dir: PathBuf,
}

impl TeamStore {
    pub fn new(config: &BusConfig, deployment_id: &str) -> Self {
        TeamStore {
            base: config.root.clone(),
            dir: config.root.join(deployment_id),
        }
    }

    /// The file naming the run in use.
    pub fn current_file(&self) -> PathBuf {
        self.dir.join("current")
    }

    /// The run in use, if any. Never creates anything.
    pub fn current(&self) -> std::io::Result<Option<String>> {
        match std::fs::read_to_string(self.current_file()) {
            Ok(text) => {
                let id = text.trim().to_string();
                if super::is_valid_run_id(&id) {
                    Ok(Some(id))
                } else {
                    Err(std::io::Error::other(format!(
                        "{} is unreadable",
                        self.current_file().display()
                    )))
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Record `id` as the run in use (atomically).
    pub fn set_current(&self, id: &str) -> std::io::Result<()> {
        super::fsx::replace(&self.current_file(), format!("{id}\n").as_bytes())
    }

    pub fn run(&self, id: &str) -> RunPaths {
        RunPaths::from_root(&self.dir.join(id))
    }

    /// The ID for the run after `current` (`r1`, `r2`, …).
    pub fn next_id(current: Option<&str>) -> String {
        let n = current
            .and_then(|c| c.strip_prefix('r'))
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(0);
        format!("r{}", n + 1)
    }

    /// Create `id`'s directory if needed and check that the root, the
    /// team's directory and the run directory are private: see
    /// [`super::fsx::ensure_private_tree`]. Every writer calls this before
    /// its first write into a run.
    pub fn ensure_run(&self, id: &str) -> std::io::Result<RunPaths> {
        let paths = self.run(id);
        super::fsx::ensure_private_tree(&self.base, &paths.root)?;
        Ok(paths)
    }
}

impl RunPaths {
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
    /// Serializes `up`, `down` and member starts/stops for this run.
    pub fn lifecycle_lock(&self) -> PathBuf {
        self.runtime_dir().join("lifecycle.lock")
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
    /// Held by the team's one running watcher.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_store_tracks_the_current_run() {
        let dir = tempfile::tempdir().unwrap();
        let config = BusConfig {
            root: dir.path().join("teams"),
            alert: None,
            warn_age_days: 7,
            max_age_days: 14,
        };
        let store = TeamStore::new(&config, "oncall-0a1b2c3d");
        assert_eq!(store.current().unwrap(), None);
        assert!(!store.dir.exists(), "reading creates nothing");
        assert_eq!(TeamStore::next_id(None), "r1");
        let r1 = store.ensure_run("r1").unwrap();
        assert!(r1.root.ends_with("oncall-0a1b2c3d/r1"));
        store.set_current("r1").unwrap();
        assert_eq!(store.current().unwrap().as_deref(), Some("r1"));
        assert_eq!(TeamStore::next_id(Some("r1")), "r2");
        assert_eq!(TeamStore::next_id(Some("r41")), "r42");
        std::fs::write(store.current_file(), "../elsewhere\n").unwrap();
        assert!(store.current().is_err());
    }
}
