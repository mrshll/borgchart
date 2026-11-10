//! Read delivery receipts from receiving stores without writing acknowledgements.
//! Every snapshot releases its lock before another store is inspected.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use super::outcome::Code;
use super::store::{self, Bus, Snapshot, WaitSnapshot};
use crate::state::StateDir;

/// Blocking outgoing work and informational notes about deleted recipients.
#[derive(Default)]
pub(crate) struct Pending {
    pub items: Vec<String>,
    pub notes: Vec<String>,
}

/// Outgoing originals remain the sender's work until the receiver holds
/// their origin. Call before taking the sender's publication lock.
pub(crate) fn pending(state: &StateDir, only: Option<&str>) -> Result<Pending, String> {
    let bus = match store::open(state) {
        Ok(bus) => bus,
        Err(f) if f.code == Code::NotConfigured => return Ok(Pending::default()),
        Err(f) => return Err(f.message),
    };
    let snap = store::snapshot(&bus).map_err(|f| f.message)?;
    snap.require_healthy().map_err(|f| f.message)?;
    let sender = only.map(|member| format!("{}/{member}", bus.instance));
    let mut receipts = BTreeMap::new();
    let mut pending = Pending::default();
    for record in snap.records.values().filter(|record| {
        record.origin.is_none()
            && record
                .from
                .rsplit_once('/')
                .is_some_and(|(instance, _)| instance == bus.instance)
            && sender.as_ref().is_none_or(|sender| &record.from == sender)
    }) {
        let Some((receiver, member)) = record.to.rsplit_once('/') else {
            continue;
        };
        if let Some(copy) = receiver
            .strip_prefix(&format!("{}/", bus.instance))
            .and_then(|copy| bus.record.copies.get(copy))
            .filter(|copy| copy.deleted)
        {
            if copy.settled.contains(&record.id) {
                continue;
            }
            pending.items.push(format!(
                "{} to a deleted copy (delivery unknown): {}",
                record.id, record.to
            ));
            continue;
        }
        let receipt = receipts
            .entry(receiver.to_string())
            .or_insert_with(|| delivered(&bus.instance, receiver));
        match receipt {
            Ok(receipt) if receipt.deleted.contains(member) => pending
                .notes
                .push(format!("{} to a deleted member: {}", record.id, record.to)),
            Ok(receipt) if receipt.delivered.contains(&record.id) => {}
            Ok(_) => pending
                .items
                .push(format!("undelivered {} to {}", record.id, record.to)),
            Err(note) => pending.items.push(format!(
                "delivery unknown for {} to {}: {note}",
                record.id, record.to
            )),
        }
    }
    Ok(pending)
}

struct Receipt {
    delivered: BTreeSet<String>,
    deleted: BTreeSet<String>,
}

fn receipt_snapshot(bus: &Bus) -> Result<Snapshot, String> {
    let receiver = &bus.instance;
    let snap = match store::wait_snapshot(bus, Instant::now() + Duration::from_millis(50))
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
            "instance `{receiver}`'s messages are incomplete; tell your parent or the owner to run `brg bus check --instance {receiver}` outside a member, then try deleting again"
        )
    })?;
    Ok(snap)
}

/// Preserve completion evidence before a copy's receipt store is removed.
/// A forced delete must not settle unacknowledged or unreadable work.
pub(crate) fn settled(sender: &str, receiver: &StateDir) -> Result<BTreeSet<String>, String> {
    let bus = match store::open(receiver) {
        Ok(bus) => bus,
        Err(f) if f.code == Code::NotConfigured => return Ok(BTreeSet::new()),
        Err(f) => return Err(f.message),
    };
    let snap = receipt_snapshot(&bus)?;
    Ok(snap
        .records
        .values()
        .filter(|record| !record.ack_required || snap.is_acked(record.seq))
        .filter_map(|record| record.origin.as_ref())
        .filter(|origin| origin.instance == sender)
        .map(|origin| origin.id.clone())
        .collect())
}

fn delivered(sender: &str, receiver: &str) -> Result<Receipt, String> {
    let bus = store::open(&StateDir::new(receiver)).map_err(|f| f.message)?;
    let snap = receipt_snapshot(&bus)?;
    let delivered = snap
        .records
        .into_values()
        .filter_map(|record| record.origin)
        .filter(|origin| origin.instance == sender)
        .map(|origin| origin.id)
        .collect();
    let deleted = bus
        .record
        .members
        .into_iter()
        .filter_map(|(id, member)| member.deleted.then_some(id))
        .collect();
    Ok(Receipt { delivered, deleted })
}
