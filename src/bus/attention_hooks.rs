//! Where the record store hands over to the attention worker.
//!
//! Own inbox listings persist which IDs a session has been shown. Until the
//! attention worker exists, the other hooks do nothing and every publish
//! reports attention `none`.

use std::collections::BTreeSet;
use std::fs;

use serde::{Deserialize, Serialize};

use super::fsx;
use super::layout::RunPaths;
use super::record::{Record, parse_record_id, record_id};

/// The union of IDs returned by this incarnation's own inbox listings.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Observed {
    schema: u32,
    member: String,
    incarnation: String,
    ids: Vec<String>,
}

/// Read-only, best effort: damaged state leaves IDs unseen; wait timing
/// never relies on this state being available.
pub(super) fn shown_ids(paths: &RunPaths, member: &str, incarnation: &str) -> BTreeSet<u64> {
    let read = || -> Option<BTreeSet<u64>> {
        let bytes = fs::read(paths.observation(member, incarnation)).ok()?;
        let observed: Observed = serde_json::from_slice(&bytes).ok()?;
        if observed.schema != 1 || observed.member != member || observed.incarnation != incarnation
        {
            return None;
        }
        observed.ids.iter().map(|id| parse_record_id(id)).collect()
    };
    read().unwrap_or_default()
}

/// What happened about waking the recipient of a new record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attention {
    /// Nothing was arranged (no attention worker in this brg).
    None,
    /// The attention worker will look at it.
    Queued,
    /// Attention can't be arranged (e.g. the instance isn't running).
    Unavailable(String),
}

impl Attention {
    /// Shown as `attention` in `brg msg publish --json`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Attention::None => "none",
            Attention::Queued => "queued",
            Attention::Unavailable(_) => "attention-unavailable",
        }
    }
}

/// After a record is committed (not after a duplicate), outside `bus/lock`.
pub fn after_publish(_paths: &RunPaths, _instance: &str, _record: &Record) -> Attention {
    Attention::None
}

/// After a member lists its own inbox (`inbox`, `inbox wait`): `ids` were
/// returned to that incarnation. Never called for `--member`, `--external`
/// or operator listings, nor for `msg read`.
pub fn after_list(paths: &RunPaths, member: &str, incarnation: &str, ids: &[String]) {
    // The caller checked the bus root through the run directory. Check the
    // attention directories too, before reading or replacing anything there.
    // No lock: concurrent listings of one incarnation may lose one merge
    // and leave IDs unseen. Background waits still rate-limit old IDs.
    // A listing must never fail on observation errors.
    let write = || -> std::io::Result<()> {
        let path = paths.observation(member, incarnation);
        let parent = path.parent().expect("observation has a parent");
        fsx::ensure_private_tree(&paths.root, parent)?;
        let mut shown = shown_ids(paths, member, incarnation);
        shown.extend(ids.iter().filter_map(|id| parse_record_id(id)));
        let observed = Observed {
            schema: 1,
            member: member.to_string(),
            incarnation: incarnation.to_string(),
            ids: shown.into_iter().map(record_id).collect(),
        };
        fsx::replace(&path, &serde_json::to_vec(&observed)?)
    };
    let _ = write();
}

/// From `publish`, `inbox`, `read` and `ack`: start the attention worker if
/// attention is pending and none is running.
pub fn ensure_if_pending(_paths: &RunPaths, _instance: &str) {}
