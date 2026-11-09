//! Read delivery receipts from receiving stores without writing acknowledgements.
//! Every snapshot releases its lock before another store is inspected.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use super::outcome::Code;
use super::store::{self, WaitSnapshot};
use crate::state::StateDir;

/// Outgoing originals remain the sender's work until the receiver holds
/// their origin. Call before taking the sender's publication lock.
pub(crate) fn pending(state: &StateDir, only: Option<&str>) -> Result<Vec<String>, String> {
    let bus = match store::open(state) {
        Ok(bus) => bus,
        Err(f) if f.code == Code::NotConfigured => return Ok(Vec::new()),
        Err(f) => return Err(f.message),
    };
    let snap = store::snapshot(&bus).map_err(|f| f.message)?;
    snap.require_healthy().map_err(|f| f.message)?;
    let sender = only.map(|member| format!("{}/{member}", bus.instance));
    let mut receipts = BTreeMap::new();
    let mut items = Vec::new();
    for record in snap.records.values().filter(|record| {
        record.origin.is_none()
            && record
                .from
                .rsplit_once('/')
                .is_some_and(|(instance, _)| instance == bus.instance)
            && sender.as_ref().is_none_or(|sender| &record.from == sender)
    }) {
        let Some((receiver, _)) = record.to.rsplit_once('/') else {
            continue;
        };
        if receiver
            .strip_prefix(&format!("{}/", bus.instance))
            .and_then(|copy| bus.record.copies.get(copy))
            .is_some_and(|copy| copy.deleted)
        {
            items.push(format!(
                "undelivered {} to a deleted copy ({})",
                record.id, record.to
            ));
            continue;
        }
        let receipt = receipts
            .entry(receiver.to_string())
            .or_insert_with(|| delivered(&bus.instance, receiver));
        match receipt {
            Ok(ids) if ids.contains(&record.id) => {}
            Ok(_) => items.push(format!("undelivered {} to {}", record.id, record.to)),
            Err(note) => items.push(format!(
                "delivery unknown for {} to {}: {note}",
                record.id, record.to
            )),
        }
    }
    Ok(items)
}

fn delivered(sender: &str, receiver: &str) -> Result<BTreeSet<String>, String> {
    let bus = store::open(&StateDir::new(receiver)).map_err(|f| f.message)?;
    let snap = match store::wait_snapshot(&bus, Instant::now() + Duration::from_millis(50))
        .map_err(|f| f.message)?
    {
        WaitSnapshot::Ready(snap) => snap,
        WaitSnapshot::TimedOut => {
            return Err(format!(
                "instance `{receiver}`'s messages are busy; try deleting again"
            ));
        }
    };
    snap.require_healthy().map_err(|_| {
        format!(
            "instance `{receiver}`'s messages are incomplete; `brg bus check --instance {receiver}` lists the problems"
        )
    })?;
    Ok(snap
        .records
        .into_values()
        .filter_map(|record| record.origin)
        .filter(|origin| origin.instance == sender)
        .map(|origin| origin.id)
        .collect())
}
