//! The heartbeat: the watcher's last word about itself, written each pass,
//! and [`health`], which says what a reader should make of it.
//!
//! A parked watcher stays alive (tmux would respawn an exit) and keeps
//! beating, saying `parked`, but does no wakes and no scans.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use super::lock::Lock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Running,
    Parked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heartbeat {
    #[serde(with = "super::utc")]
    pub at: SystemTime,
    /// The running brg's version.
    pub version: String,
    pub state: State,
    /// When the last scan finished; before the first one, when the watcher
    /// started, so a watcher that never finishes a scan goes stale too.
    #[serde(with = "super::utc")]
    pub last_scan: SystemTime,
}

fn path(dir: &Path) -> PathBuf {
    dir.join("heartbeat.json")
}

pub fn write(_lock: &Lock, dir: &Path, beat: &Heartbeat) -> Result<(), String> {
    super::save(&path(dir), beat)
}

/// The heartbeat on disk: `None` if there is none, `Some(Err)` if it can't
/// be read or parsed. Pass the result to [`health`].
pub fn read(dir: &Path) -> Option<Result<Heartbeat, String>> {
    super::load(&path(dir)).transpose()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// A heartbeat older than this is stale: the watcher isn't running.
    pub stale_after: Duration,
    /// A last scan older than this, under a fresh heartbeat, means a
    /// watcher stuck mid-scan.
    pub scan_stale_after: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    Missing,
    Malformed(String),
    /// The heartbeat is this old.
    Stale(Duration),
    /// The heartbeat is fresh, but the last scan is this old.
    ScanStale(Duration),
    Parked,
    Ok,
}

/// Times in the future (clock skew) count as fresh.
pub fn health(now: SystemTime, beat: Option<Result<Heartbeat, String>>, limits: &Limits) -> Health {
    let beat = match beat {
        None => return Health::Missing,
        Some(Err(e)) => return Health::Malformed(e),
        Some(Ok(b)) => b,
    };
    let age = |t: SystemTime| now.duration_since(t).unwrap_or_default();
    if age(beat.at) > limits.stale_after {
        return Health::Stale(age(beat.at));
    }
    if beat.state == State::Parked {
        // A parked watcher doesn't scan; its last scan is old by design.
        return Health::Parked;
    }
    if age(beat.last_scan) > limits.scan_stale_after {
        return Health::ScanStale(age(beat.last_scan));
    }
    Health::Ok
}
