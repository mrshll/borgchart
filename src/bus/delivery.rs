//! Pull immutable messages along parent edges. Peer snapshots release their
//! read-only shared locks before the receiving store takes its own lock.

use std::collections::{BTreeMap, BTreeSet};
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
        // A locked recipient's input must be verified before committing,
        // which the sender cannot do from its own store.
        if peer.meta.members[&recipient].posture == "locked" {
            return Err(format!(
                "`{to}` is locked; locked members cannot receive messages across a parent edge"
            ));
        }
        Ok(())
    } else {
        Err(format!(
            "`{sender}` can publish only to its parent or children, not `{to}`"
        ))
    }
}

/// Stores joined to this recipient by recorded parent edges. No store lock
/// is held while discovering an edge or opening its metadata.
fn peers(bus: &Bus, member: &str) -> Result<(Vec<Bus>, Vec<String>), Failure> {
    let mut ids = Vec::new();
    let mut notes = Vec::new();
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
    let current =
        crate::registry::read_instance(&bus.state).map_err(|e| Failure::new(Code::Error, e))?;
    for (id, _) in current.copies.iter().filter(|(_, copy)| !copy.deleted) {
        let state = StateDir::new(&format!("{}/{id}", bus.instance));
        let record = match crate::registry::read_instance(&state) {
            Ok(record) => record,
            Err(_) => {
                notes.push(peer_note(&state.id, "copy record cannot be read"));
                continue;
            }
        };
        if record.parent.as_deref() == Some(own.as_str()) {
            ids.push(state.id);
        }
    }
    let mut peers = Vec::new();
    for id in ids {
        let state = StateDir::new(&id);
        match store::open(&state) {
            Ok(peer) => peers.push(peer),
            // A kept copy cannot have published before its first start.
            Err(error) if error.code == Code::NotConfigured && !state.meta_file().exists() => {}
            Err(_) => notes.push(peer_note(&id, "message store cannot be read")),
        }
    }
    Ok((peers, notes))
}

fn peer_note(instance: &str, problem: &str) -> String {
    format!(
        "instance `{instance}`'s {problem}; tell your parent or the owner (`brg bus check --instance {instance}` outside a member), then try your inbox again"
    )
}

struct Pending {
    records: Vec<(Origin, Record)>,
    notes: Vec<String>,
}

#[derive(Default)]
struct PeerPoll {
    next: Option<u64>,
    records: Vec<Record>,
}

/// Each wait remembers immutable peer history and retains undelivered records
/// until the local snapshot confirms their origins were appended.
#[derive(Default)]
pub(super) struct Imports {
    peers: BTreeMap<String, PeerPoll>,
}

/// Read every available edge independently, so one broken peer cannot hide
/// either another peer's records or the receiver's local inbox.
fn pending(
    bus: &Bus,
    member: &str,
    local: &Snapshot,
    polls: &mut Imports,
    deadline: Instant,
) -> Result<Pending, Failure> {
    let recipient = format!("{}/{member}", bus.instance);
    let (peers, mut notes) = peers(bus, member)?;
    let mut pending = Vec::new();
    let held: BTreeSet<_> = local
        .records
        .values()
        .filter_map(|r| r.origin.as_ref())
        .map(|origin| (origin.instance.as_str(), origin.id.as_str()))
        .collect();
    for peer in peers {
        let until = deadline.min(Instant::now() + Duration::from_millis(50));
        let poll = polls.peers.entry(peer.instance.clone()).or_default();
        let snap = match store::peer_snapshot(&peer, until, poll.next) {
            Ok(WaitSnapshot::Ready(snap)) if snap.healthy() => snap,
            Ok(WaitSnapshot::TimedOut) => {
                notes.push(peer_note(&peer.instance, "message store is busy"));
                continue;
            }
            _ => {
                notes.push(peer_note(&peer.instance, "message store cannot be read"));
                continue;
            }
        };
        poll.next = Some(
            snap.records
                .last_key_value()
                .map_or(poll.next.unwrap_or(1), |(&seq, _)| seq + 1),
        );
        poll.records.extend(candidates(&peer, snap, &recipient));
        poll.records
            .retain(|record| !held.contains(&(peer.instance.as_str(), record.id.as_str())));
        for record in &poll.records {
            let origin = Origin {
                instance: peer.instance.clone(),
                id: record.id.clone(),
            };
            pending.push((origin, record.clone()));
        }
    }
    Ok(Pending {
        records: pending,
        notes,
    })
}

/// Pending records count as unread without writing the receiving store. An
/// unavailable edge makes the total unknown, including when the local count is zero.
pub(super) fn unread(
    bus: &Bus,
    snap: &Snapshot,
) -> Result<std::collections::BTreeMap<String, store::Unread>, Failure> {
    let local = store::progress(bus, snap);
    bus.meta
        .members
        .keys()
        .map(|member| {
            let Pending {
                records: pending,
                notes,
            } = pending(
                bus,
                member,
                snap,
                &mut Imports::default(),
                Instant::now() + Duration::from_secs(1),
            )?;
            let missing = pending.len();
            Ok((
                member.clone(),
                store::Unread {
                    count: notes
                        .is_empty()
                        .then(|| local.get(member).copied().unwrap_or_default().unread + missing),
                    notes,
                },
            ))
        })
        .collect()
}

/// Reserve the largest receiving sequence/ID and any reply IDs that could be
/// mapped to a longer local ID, as well as the origin's full instance path.
pub(super) fn import_size(bus: &Bus, record: &Record) -> Result<usize, Failure> {
    let mut imported = record.clone();
    imported.origin = Some(Origin {
        instance: bus.instance.clone(),
        id: record.id.clone(),
    });
    imported.seq = u64::MAX;
    imported.id = record_id(u64::MAX);
    for reference in &mut imported.reply_to {
        if reference.len() < imported.id.len() {
            *reference = imported.id.clone();
        }
    }
    serde_json::to_vec_pretty(&imported)
        .map(|bytes| bytes.len())
        .map_err(|e| Failure::new(Code::Error, e.to_string()))
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

impl Imports {
    pub(super) fn import(
        &mut self,
        bus: &Bus,
        caller: &Caller,
        to: &Recipient,
        local: &mut Snapshot,
        deadline: Instant,
    ) -> Result<(), Failure> {
        local.notes.clear();
        match self.try_import(bus, caller, to, local, deadline) {
            Ok(()) => {}
            Err(error) if error.code == Code::Fenced => {
                return Err(error.with_notes(local.notes.clone()));
            }
            Err(error) => {
                local.notes.extend(error.notes);
                local.notes.push(format!(
                    "message import deferred: {}; try your inbox again",
                    error.message
                ));
            }
        }
        Ok(())
    }

    fn try_import(
        &mut self,
        bus: &Bus,
        caller: &Caller,
        to: &Recipient,
        local: &mut Snapshot,
        deadline: Instant,
    ) -> Result<(), Failure> {
        if matches!(caller, Caller::Member { .. }) {
            store::actor_for(bus, caller)?;
        }
        let Recipient::Member(member) = to else {
            return Ok(());
        };
        let Pending {
            records: pending,
            notes,
        } = pending(bus, member, local, self, deadline)?;
        local.notes = notes;
        if pending.is_empty() {
            return Ok(());
        }
        #[cfg(feature = "test-support")]
        import_gate(deadline)?;
        // All peer snapshots (and therefore their shared locks) are gone.
        let until = deadline.min(Instant::now() + Duration::from_millis(50));
        let Some((bus, _lock)) = store::lock_for_import(bus, until)? else {
            return Err(Failure::new(
                Code::Timeout,
                "this store is busy; import will retry",
            ));
        };
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
        snap.notes = std::mem::take(&mut local.notes);
        *local = snap;
        Ok(())
    }
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
