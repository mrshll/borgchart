//! Where an instance's message storage lives.
//!
//! Each instance's state folder holds `messages/`. Inside it, `current`
//! names the storage in use (a *run*, internally; users never see or name
//! runs). brg starts a new one when the instance's configuration or message
//! format changes, or when the storage reaches its age limit.
//!
//! ```text
//! <state home>/<instance>/messages/     0700
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

use crate::state::StateDir;

/// Age (days) at which an instance's message storage is reported as aging.
pub const WARN_AGE_DAYS: u32 = 7;
/// Age (days) at which brg rolls an instance's message storage over.
pub const MAX_AGE_DAYS: u32 = 14;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPaths {
    pub root: PathBuf,
}

/// One instance's message storage: `<state folder>/messages/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamStore {
    /// The instance's state folder.
    pub base: PathBuf,
    pub dir: PathBuf,
}

impl TeamStore {
    pub fn new(state: &StateDir) -> Self {
        TeamStore {
            base: state.root.clone(),
            dir: state.messages_dir(),
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
                if run_number(&id).is_some() {
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

    /// The runs that exist (`r1`, `r2`, …), oldest first. Never creates anything.
    pub fn runs(&self) -> std::io::Result<Vec<String>> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut runs: Vec<(u64, String)> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let n = run_number(&name)?;
                e.file_type().ok()?.is_dir().then_some((n, name))
            })
            .collect();
        runs.sort();
        Ok(runs.into_iter().map(|(_, name)| name).collect())
    }

    /// The ID for a new run: one past the highest that exists, whatever
    /// `current` says, so a lost `current` file can never reuse an old run.
    pub fn next_id(&self) -> std::io::Result<String> {
        let n = self
            .runs()?
            .iter()
            .filter_map(|r| run_number(r))
            .max()
            .unwrap_or(0);
        let next = n
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("no more storage numbers left"))?;
        Ok(format!("r{next}"))
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

fn run_number(name: &str) -> Option<u64> {
    let digits = name.strip_prefix('r')?;
    if digits.is_empty() || digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
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

    /// Written (immutably) when brg rolls the team's storage over to a new
    /// run. Rollover writes it while holding this run's `bus/lock`, before
    /// carrying and switching `current`; every writer re-checks it after
    /// taking `bus/lock` and, if present, re-resolves `current` instead of
    /// writing here. So nothing published during a rollover is stranded.
    pub fn retired(&self) -> PathBuf {
        self.root.join("retired")
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
        let state = StateDir::from_root(&dir.path().join("oncall-2a3b"));
        let store = TeamStore::new(&state);
        assert_eq!(store.current().unwrap(), None);
        assert!(!store.dir.exists(), "reading creates nothing");
        assert_eq!(store.next_id().unwrap(), "r1");
        let r1 = store.ensure_run("r1").unwrap();
        assert!(r1.root.ends_with("oncall-2a3b/messages/r1"));
        store.set_current("r1").unwrap();
        assert_eq!(store.current().unwrap().as_deref(), Some("r1"));
        assert_eq!(store.next_id().unwrap(), "r2");
        store.ensure_run("r2").unwrap();
        store.ensure_run("r10").unwrap();
        std::fs::create_dir(store.dir.join("r07")).unwrap();
        std::fs::create_dir(store.dir.join("notes")).unwrap();
        assert_eq!(store.runs().unwrap(), ["r1", "r2", "r10"]);
        // A lost `current` never makes brg reuse an existing run.
        std::fs::remove_file(store.current_file()).unwrap();
        assert_eq!(store.next_id().unwrap(), "r11");
        for bad in [
            "../elsewhere",
            "other",
            "r0",
            "r007",
            "r18446744073709551616",
        ] {
            std::fs::write(store.current_file(), format!("{bad}\n")).unwrap();
            assert!(store.current().is_err(), "{bad}");
        }
        std::fs::write(store.current_file(), "r18446744073709551615\n").unwrap();
        assert!(store.current().is_ok());
        std::fs::create_dir(store.dir.join("r18446744073709551615")).unwrap();
        assert!(
            store.next_id().is_err(),
            "exhausted numbering is an error, not a panic"
        );
    }
}
