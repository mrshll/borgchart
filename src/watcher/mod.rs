//! The watcher's policy and its own state, as a library.
//!
//! The watcher notices members that have unread messages and nothing armed
//! to wake them, and decides how to wake each one. This module holds the
//! parts that need no tmux, no message store and no vendor CLI:
//!
//! - [`lock`]: at most one watcher per state folder;
//! - [`heartbeat`]: what the watcher last said about itself, and how
//!   healthy that looks;
//! - [`restart`]: counting unplanned starts, parking after a crash loop,
//!   and planned restarts for a new installed brg;
//! - [`binary`]: noticing that the installed brg changed, debounced;
//! - [`alerts`]: what needs a person, for `status` to show; nothing is
//!   invoked;
//! - [`attempts`]: the record of each member's last wake attempt;
//! - [`methods`]: which wake methods each vendor and version allows, from
//!   `rules/wake.yaml`;
//! - [`decide`]: the wake decision, a pure function over plain-value
//!   observations of one member.
//!
//! Every function here takes the watcher's state folder (the team's
//! `watcher/`) as a path, and the time as a value. Its files are small JSON,
//! replaced atomically and privately with [`crate::bus::fsx`]. Unlike
//! records, they ignore fields they don't know, so a newer watcher's state
//! doesn't stop an older one.

pub mod alerts;
pub mod attempts;
pub mod binary;
pub mod decide;
pub mod heartbeat;
pub mod lock;
pub mod methods;
pub mod restart;

use std::path::Path;
use std::time::SystemTime;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::bus::time::{format_utc, parse_utc};

/// Make sure the state folder exists and is private.
fn ensure_dir(dir: &Path) -> Result<(), String> {
    crate::bus::fsx::ensure_private_tree(dir, dir).map_err(|e| format!("{}: {e}", dir.display()))
}

/// Read a state file; a missing one is `None`.
fn load<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("{} is unreadable: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// Replace a state file atomically (mode 0600, in a private folder).
fn save<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        ensure_dir(dir)?;
    }
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    crate::bus::fsx::replace(path, &bytes)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Times in state files, as UTC strings with millisecond precision.
mod utc {
    use super::*;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(t: &SystemTime, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format_utc(*t))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SystemTime, D::Error> {
        let s = String::deserialize(d)?;
        parse_utc(&s).ok_or_else(|| serde::de::Error::custom(format!("bad timestamp `{s}`")))
    }
}
