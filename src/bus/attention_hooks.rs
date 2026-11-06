//! Where the record store hands over to the attention worker.
//!
//! The store calls these at fixed points; until the attention worker exists
//! they do nothing, and every publish reports attention `none`.

use crate::deploy::Deployment;

use super::layout::RunPaths;
use super::record::Record;

/// What happened about waking the recipient of a new record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attention {
    /// Nothing was arranged (no attention worker in this brg).
    None,
    /// The attention worker will look at it.
    Queued,
    /// Attention can't be arranged (e.g. the deployment isn't running).
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
pub fn after_publish(_paths: &RunPaths, _deployment: &Deployment, _record: &Record) -> Attention {
    Attention::None
}

/// After a member lists its own inbox (`inbox`, `inbox wait`): `ids` were
/// returned to that incarnation. Never called for `--member`, `--external`
/// or operator listings, nor for `msg read`.
pub fn after_list(_paths: &RunPaths, _member: &str, _incarnation: &str, _ids: &[String]) {}

/// From `publish`, `inbox`, `read` and `ack`: start the attention worker if
/// attention is pending and none is running.
pub fn ensure_if_pending(_paths: &RunPaths, _deployment: &Deployment) {}
