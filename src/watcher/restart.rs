//! Crash accounting, parking, and planned restarts.
//!
//! tmux respawns the watcher whenever it exits and keeps no state, so the
//! watcher counts its own starts, under its [`Lock`] (a second watcher never
//! gets that far, so it doesn't count). Too many unplanned starts within a
//! window parks it: it stays alive, beats `parked`, and does no wakes.
//! Parking clears when the instance is started again
//! ([`instance_started`]) or a different brg starts the watcher.
//!
//! A restart for a new installed brg is marked before the exit
//! ([`mark_planned`]), so the start after it doesn't count. It's allowed
//! only when no wake is in flight.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use super::lock::Lock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrashLimits {
    /// More unplanned starts than this within `window` parks the watcher.
    pub max_starts: usize,
    pub window: Duration,
}

impl Default for CrashLimits {
    fn default() -> Self {
        CrashLimits {
            max_starts: 5,
            window: Duration::from_secs(120),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Begin {
    /// Go ahead. `planned` if the previous exit was a planned restart.
    Run { planned: bool },
    /// Stay alive, beat `parked`, do no wakes. The reason says why.
    Park(String),
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Starts {
    #[serde(default)]
    starts: Vec<Stamp>,
    #[serde(default)]
    parked: Option<Parked>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Stamp(#[serde(with = "super::utc")] SystemTime);

#[derive(Debug, Serialize, Deserialize)]
struct Parked {
    #[serde(with = "super::utc")]
    at: SystemTime,
    reason: String,
    /// The identity of the brg that parked ([`super::binary::BinaryId::sha256`]).
    binary: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Planned {
    #[serde(with = "super::utc")]
    at: SystemTime,
}

fn starts_path(dir: &Path) -> PathBuf {
    dir.join("starts.json")
}

fn planned_path(dir: &Path) -> PathBuf {
    dir.join("planned-restart.json")
}

/// Count this start and decide whether to run. `binary` is the running
/// brg's identity: a start by a different brg than the one that parked
/// clears the parking.
pub fn count_start(
    _lock: &Lock,
    dir: &Path,
    limits: &CrashLimits,
    now: SystemTime,
    binary: &str,
) -> Result<Begin, String> {
    let planned_file = planned_path(dir);
    let planned = super::load::<Planned>(&planned_file)?.is_some();
    let mut st: Starts = super::load(&starts_path(dir))?.unwrap_or_default();
    match &st.parked {
        Some(p) if p.binary == binary => return Ok(Begin::Park(p.reason.clone())),
        Some(_) => st = Starts::default(),
        None => {}
    }
    st.starts
        .retain(|s| now.duration_since(s.0).unwrap_or_default() < limits.window);
    if !planned {
        st.starts.push(Stamp(now));
    }
    let begin = if st.starts.len() > limits.max_starts {
        let reason = format!(
            "the watcher crashed repeatedly: {} starts within {}s",
            st.starts.len(),
            limits.window.as_secs()
        );
        st.parked = Some(Parked {
            at: now,
            reason: reason.clone(),
            binary: binary.to_string(),
        });
        Begin::Park(reason)
    } else {
        Begin::Run { planned }
    };
    super::save(&starts_path(dir), &st)?;
    if planned {
        // Used once, and only after the start is recorded.
        std::fs::remove_file(&planned_file)
            .map_err(|e| format!("cannot remove {}: {e}", planned_file.display()))?;
    }
    Ok(begin)
}

/// Why the watcher is parked, if it is. A parked watcher checks this each
/// pass, so [`instance_started`] wakes it without a restart.
pub fn parked(dir: &Path) -> Result<Option<String>, String> {
    Ok(super::load::<Starts>(&starts_path(dir))?
        .and_then(|st| st.parked)
        .map(|p| p.reason))
}

/// The instance was started: forget the start history and any parking.
/// Called by the start command, which doesn't hold the watcher's lock.
pub fn instance_started(dir: &Path) -> Result<(), String> {
    super::save(&starts_path(dir), &Starts::default())
}

/// Mark the coming exit as a planned restart, so the next start doesn't
/// count. Refused (`Ok(false)`, nothing written) while any member's wake
/// may be in flight (see [`super::decide::Attempt::in_flight`]); the caller
/// tries again at a later pass.
pub fn mark_planned(
    _lock: &Lock,
    dir: &Path,
    now: SystemTime,
    retry_after: Duration,
) -> Result<bool, String> {
    if super::attempts::load(dir)?
        .values()
        .any(|a| a.in_flight(now, retry_after))
    {
        return Ok(false);
    }
    super::save(&planned_path(dir), &Planned { at: now })?;
    Ok(true)
}
