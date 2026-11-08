//! Each member's last wake attempt, in `attempts.json`.
//!
//! The caller records an attempt's intent (from [`super::decide`]) before
//! its side effect, and the same attempt with its outcome after. An intent
//! with no outcome is a wake that may be in flight.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::decide::Attempt;
use super::lock::Lock;

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    members: BTreeMap<String, Attempt>,
}

fn path(dir: &Path) -> PathBuf {
    dir.join("attempts.json")
}

/// The last attempt for each member, by member ID.
pub fn load(dir: &Path) -> Result<BTreeMap<String, Attempt>, String> {
    Ok(super::load::<File>(&path(dir))?.unwrap_or_default().members)
}

/// Record `attempt` as `member`'s last, replacing the one before.
pub fn record(_lock: &Lock, dir: &Path, member: &str, attempt: &Attempt) -> Result<(), String> {
    let mut file = super::load::<File>(&path(dir))?.unwrap_or_default();
    file.members.insert(member.to_string(), attempt.clone());
    super::save(&path(dir), &file)
}
