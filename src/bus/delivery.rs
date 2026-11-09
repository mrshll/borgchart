//! Pull immutable messages along parent edges. Peer snapshots release their
//! read-only shared locks before the receiving store takes its own lock.

use std::time::{Duration, Instant};

use super::context::{Actor, Caller};
use super::fsx::{self, Written};
use super::outcome::{Code, Failure};
use super::record::{MAX_RECORD_BYTES, Origin, Recipient, Record, record_id};
use super::store::{self, Bus, Snapshot, WaitSnapshot};
use crate::{address, state::StateDir};

/// Full addresses are accepted only at the other end of a parent edge.
pub(super) fn check_route(bus: &Bus, actor: &Actor, to: &str) -> Result<(), String> {
    let Actor::Member { member, .. } = actor else {
        return Err("an outside caller can only publish to this instance's members".into());
    };
    let (instance, target) = address::resolve(to)?;
    let address::Target::Member(recipient) = target else {
        return Err(format!("`{to}` is not a member; name a member in full"));
    };
    address::member(&instance, &recipient)?;
    let peer = store::open(&instance.state).map_err(|e| e.message)?;
    let sender = format!("{}/{member}", bus.instance);
    let parent = bus
        .meta
        .members
        .get(member)
        .and_then(|m| m.parent.as_deref());
    let recipient_parent = peer
        .meta
        .members
        .get(&recipient)
        .and_then(|m| m.parent.as_deref());
    if peer.meta.members.contains_key(&recipient)
        && (parent == Some(to) || recipient_parent == Some(sender.as_str()))
    {
        Ok(())
    } else {
        Err(format!(
            "`{sender}` can publish only to its parent or children, not `{to}`"
        ))
    }
}

/// Stores joined to this recipient by recorded parent edges. No store lock
/// is held while discovering an edge or opening its metadata.
fn peers(bus: &Bus, member: &str) -> Result<Vec<Bus>, Failure> {
    let mut ids = Vec::new();
    if let Some(parent) = bus
        .meta
        .members
        .get(member)
        .and_then(|m| m.parent.as_deref())
        && let Some((instance, _)) = parent.rsplit_once('/')
    {
        ids.push(instance.to_string());
    }
    let own = format!("{}/{member}", bus.instance);
    for (id, _) in bus.record.copies.iter().filter(|(_, copy)| !copy.deleted) {
        let state = StateDir::new(&format!("{}/{id}", bus.instance));
        let record =
            crate::registry::read_instance(&state).map_err(|e| Failure::new(Code::Error, e))?;
        if record.parent.as_deref() == Some(own.as_str()) {
            ids.push(state.id);
        }
    }
    ids.into_iter()
        .map(|id| store::open(&StateDir::new(&id)))
        // A kept copy has no readable store until its first start. It
        // cannot have published anything yet and must not block its parent.
        .filter(|result| !matches!(result, Err(error) if error.code == Code::NotConfigured))
        .collect()
}

/// Only original messages published by the peer itself can cross this edge.
/// Authorization was settled at publish time; its sender may have restarted.
fn candidates(peer: &Bus, snap: Snapshot, to: &str) -> Vec<Record> {
    snap.records
        .into_values()
        .filter(|record| {
            record.origin.is_none()
                && record.to == to
                && record
                    .from
                    .rsplit_once('/')
                    .is_some_and(|(instance, _)| instance == peer.instance)
        })
        .collect()
}

fn local_reference(bus: &Bus, snap: &Snapshot, reference: &str) -> String {
    let Some((instance, id)) = reference.rsplit_once('/') else {
        return reference.to_string();
    };
    if instance == bus.instance {
        return id.to_string();
    }
    snap.records
        .values()
        .find(|record| {
            record
                .origin
                .as_ref()
                .is_some_and(|origin| origin.instance == instance && origin.id == id)
        })
        .map_or_else(|| reference.to_string(), |record| record.id.clone())
}

pub(super) fn import(
    bus: &Bus,
    caller: &Caller,
    to: &Recipient,
    deadline: Instant,
) -> Result<(), Failure> {
    let Recipient::Member(member) = to else {
        return Ok(());
    };
    let recipient = format!("{}/{member}", bus.instance);
    let mut pending = Vec::new();
    for peer in peers(bus, member)? {
        let until = deadline.min(Instant::now() + Duration::from_millis(50));
        let snap = match store::wait_snapshot(&peer, until)? {
            WaitSnapshot::Ready(snap) => snap,
            WaitSnapshot::TimedOut => {
                return Err(Failure::new(
                    Code::Error,
                    format!(
                        "instance `{}`'s message store is busy; try its inbox again",
                        peer.instance
                    ),
                ));
            }
        };
        snap.require_healthy()?;
        for record in candidates(&peer, snap, &recipient) {
            let origin = Origin {
                instance: peer.instance.clone(),
                id: record.id.clone(),
            };
            pending.push((origin, record));
        }
    }
    if pending.is_empty() {
        return Ok(());
    }
    #[cfg(feature = "test-support")]
    import_gate(deadline)?;
    // All peer snapshots (and therefore their shared locks) are gone.
    let (bus, _lock) = store::lock_for_mutation(bus)?;
    if matches!(caller, Caller::Member { .. }) {
        store::actor_for(&bus, caller)?;
    }
    let mut snap = store::scan(&bus.paths);
    snap.require_healthy()?;
    for (origin, mut record) in pending {
        if snap
            .records
            .values()
            .any(|r| r.origin.as_ref() == Some(&origin))
        {
            continue;
        }
        let seq = snap.records.len() as u64 + 1;
        record.seq = seq;
        record.id = record_id(seq);
        record.origin = Some(origin);
        record.reply_to = record
            .reply_to
            .iter()
            .map(|reference| local_reference(&bus, &snap, reference))
            .collect();
        let bytes = serde_json::to_vec_pretty(&record)
            .map_err(|e| Failure::new(Code::Error, e.to_string()))?;
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(Failure::new(
                Code::Invalid,
                "the imported record would be too large; shorten the message before publishing",
            ));
        }
        #[cfg(feature = "test-support")]
        crash("before-append");
        match fsx::write_new(&bus.paths.record(seq), &bytes)
            .map_err(|e| Failure::io("cannot import the record", e))?
        {
            Written::Created => {}
            Written::Exists => {
                return Err(Failure::new(
                    Code::Corrupt,
                    "the next record already exists; `brg bus check` lists the problems",
                ));
            }
        }
        #[cfg(feature = "test-support")]
        crash("after-append");
        snap.records.insert(seq, record);
    }
    Ok(())
}

#[cfg(feature = "test-support")]
fn crash(point: &str) {
    if std::env::var("BORGCHART_TEST_IMPORT_CRASH").as_deref() == Ok(point) {
        std::process::exit(86);
    }
}

#[cfg(feature = "test-support")]
fn import_gate(deadline: Instant) -> Result<(), Failure> {
    let Some(path) = std::env::var_os("BORGCHART_TEST_IMPORT_GATE") else {
        return Ok(());
    };
    let gate = std::path::PathBuf::from(path);
    std::fs::write(gate.join(std::process::id().to_string()), b"")
        .map_err(|e| Failure::io("cannot signal the test import", e))?;
    while !gate.join("proceed").exists() {
        if Instant::now() >= deadline {
            return Err(Failure::new(Code::Timeout, "test import gate timed out"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}
