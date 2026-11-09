//! Immutable messages, acknowledgements, inboxes and progress.
//! Mutations lock and rescan the store, refusing unreadable data or gaps.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::state::{self, StateDir};

use super::context::{Actor, Caller};
use super::fsx::{self, TMP_PREFIX, Written};
use super::layout::StorePaths;
use super::outcome::{Code, Failure};
use super::record::{Draft, MAX_RECORD_BYTES, Recipient, Record, parse_record_id, record_id};
use super::time;
use crate::lifecycle;

pub const MIN_WAIT_SECS: u64 = 60;
pub const MAX_WAIT_SECS: u64 = 86400;
pub const POLL: Duration = Duration::from_millis(250);

/// The loop guard also applies to errors before an inbox can be opened.
pub fn wait_minimum() -> Duration {
    #[cfg(feature = "test-support")]
    if let Some(ms) = std::env::var("BORGCHART_WAIT_MIN_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    {
        return Duration::from_millis(ms);
    }
    Duration::from_secs(MIN_WAIT_SECS)
}

/// An opened store and the routing recorded at the instance's latest start.
#[derive(Debug, Clone)]
pub struct Bus {
    pub state: StateDir,
    pub instance: String,
    pub paths: StorePaths,
    pub meta: state::Meta,
    pub record: crate::registry::InstanceRecord,
}

/// Open read-only; never initialise an unrecognised store.
pub fn open(state: &StateDir) -> Result<Bus, Failure> {
    let record =
        crate::registry::read_instance(state).map_err(|e| Failure::new(Code::NotFound, e))?;
    let paths = StorePaths::new(state);
    if !paths
        .validate(&state.id)
        .map_err(|e| Failure::new(Code::Corrupt, e))?
    {
        return Err(Failure::new(
            Code::NotConfigured,
            format!(
                "instance `{}` has no messages: its team has no `bus:`, or it hasn't started with one",
                state.id
            ),
        ));
    }
    let mut meta = state::read_meta(state).ok_or_else(|| {
        Failure::new(
            Code::Corrupt,
            format!("instance `{}`'s start record cannot be read", state.id),
        )
    })?;
    meta.members
        .retain(|id, _| !record.members.get(id).is_some_and(|m| m.deleted));
    Ok(Bus {
        state: state.clone(),
        instance: state.id.clone(),
        paths,
        meta,
        record,
    })
}

/// The reason in a "<path> is unreadable: …" or "cannot read <path>: …"
/// error, without the path (which is internal).
fn reason(e: &str, path: &Path) -> String {
    let shown = path.display().to_string();
    e.strip_prefix(&format!("{shown} is unreadable: "))
        .or_else(|| e.strip_prefix(&format!("cannot read {shown}: ")))
        .map(str::to_string)
        .unwrap_or_else(|| hide_paths(e, path.parent().unwrap_or(path), None))
}

/// `text` without the internal paths of a instance's storage: paths inside the
/// store become relative (`records`), the store itself "its messages
/// folder", and the instance's directory "its folder".
fn hide_paths(text: &str, store_root: &Path, team_dir: Option<&Path>) -> String {
    let root = store_root.display().to_string();
    let mut out = text
        .replace(&format!("{root}/"), "")
        .replace(&root, "its messages folder");
    if let Some(dir) = team_dir {
        out = out.replace(&dir.display().to_string(), "its folder");
    }
    out
}

impl Bus {
    /// Refuse deleted recipients and name the concrete create command for roles.
    fn check_route(&self, actor: &Actor, to: &str) -> Result<(), String> {
        if self.record.members.get(to).is_some_and(|m| m.deleted) {
            return Err(format!(
                "member `{to}` was deleted; it cannot receive messages"
            ));
        }
        check_route(&self.meta, actor, to).map_err(|e| {
            let mut report = crate::diag::Report::default();
            if crate::team::load(&self.record.team_dir, &mut report)
                .is_some_and(|team| team.roles.contains_key(to))
            {
                e.replace(
                    "brg create <instance>/<role>",
                    &format!("brg create {}/{to}", self.instance),
                )
            } else {
                e
            }
        })
    }

    /// `text` without this instance's internal storage paths.
    fn hide_paths(&self, text: &str) -> String {
        hide_paths(text, &self.paths.root, Some(&self.state.root))
    }
}

// --- callers ---

/// The actor a caller publishes and acknowledges as. Members are checked
/// against the current incarnation (`fenced` if stale); operators may not mutate.
pub fn actor_for(bus: &Bus, caller: &Caller) -> Result<Actor, Failure> {
    match caller {
        Caller::Operator => Err(Failure::new(
            Code::Refused,
            "no caller identity: members publish as themselves, outside callers pass `--external NAME`",
        )),
        Caller::External { name } => Ok(Actor::External { name: name.clone() }),
        Caller::Member {
            member,
            incarnation,
        } => {
            fence_member(bus, member, incarnation)?;
            Ok(Actor::Member {
                member: member.clone(),
                incarnation: incarnation.clone(),
            })
        }
    }
}

/// How to start one member.
fn member_hint(bus: &Bus, member: &str) -> String {
    format!("brg start {}/{member}", bus.instance)
}

fn fence_member(bus: &Bus, member: &str, incarnation: &str) -> Result<(), Failure> {
    let fenced = |m: String| Err(Failure::new(Code::Fenced, m));
    let team = &bus.instance;
    let start = member_hint(bus, member);
    if !bus.meta.members.contains_key(member) {
        return fenced(format!("`{member}` is not a member of instance `{team}`"));
    }
    let current = lifecycle::load_incarnation(&bus.state, member).map_err(|e| {
        Failure::new(
            Code::Error,
            format!(
                "{team}/{member}'s session record is unreadable ({})",
                reason(&e, &bus.state.current_incarnation(member))
            ),
        )
    })?;
    match current {
        None => fenced(format!("{team}/{member} isn't running; start it: {start}")),
        Some(inc) if inc.member != member || inc.incarnation != incarnation => fenced(format!(
            "{team}/{member} was restarted after this session of it started; only its current session can do this"
        )),
        Some(inc) if inc.ended_at.is_some() => fenced(format!(
            "{team}/{member} was stopped; start it again: {start}"
        )),
        Some(_) => Ok(()),
    }
}

/// Take the publication lock and refresh the routing under it. A store must
/// already be recognised; mutations never create storage over damaged data.
fn lock_for_mutation(bus: &Bus) -> Result<(Bus, fs::File), Failure> {
    let fresh = open(&bus.state)?;
    fsx::ensure_private_tree(&fresh.state.root, &fresh.paths.root).map_err(|e| {
        Failure::new(
            Code::Error,
            format!(
                "instance `{}`'s message storage can't be used: {}",
                fresh.instance,
                fresh.hide_paths(&e.to_string())
            ),
        )
    })?;
    let lock = state::open_lock(&fresh.paths.bus_lock())
        .map_err(|e| Failure::io("cannot open messages/lock", e))?;
    lock.lock()
        .map_err(|e| Failure::io("cannot lock messages/lock", e))?;
    Ok((open(&bus.state)?, lock))
}

/// The authoritative caller check, made while holding `messages/lock`:
/// incarnations change only under it, so what is checked here holds until
/// the write. The check before the lock only rejects early.
fn recheck_authority(bus: &Bus, caller: &Caller) -> Result<(), Failure> {
    actor_for(bus, caller).map(|_| ())
}

/// Hold `messages/lock` shared while reading, so a scan never sees a record
/// mid-publication. Opens the lock read-only and
/// creates nothing: with no lock file there is no store to read yet.
fn read_lock(paths: &StorePaths) -> Result<Option<fs::File>, Failure> {
    match read_lock_until(paths, None)? {
        ReadLock::Held(f) => Ok(Some(f)),
        ReadLock::NoStore => Ok(None),
        ReadLock::TimedOut => Err(Failure::new(Code::Error, "cannot lock messages/lock")),
    }
}

enum ReadLock {
    /// No lock file: nothing has been written yet.
    NoStore,
    Held(fs::File),
    /// The deadline passed while a writer held the lock.
    TimedOut,
}

/// [`read_lock`], giving up at `deadline` (if any) instead of waiting for a
/// writer indefinitely.
fn read_lock_until(paths: &StorePaths, deadline: Option<Instant>) -> Result<ReadLock, Failure> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(paths.bus_lock())
    {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(ReadLock::NoStore),
        Err(e) => return Err(Failure::io("cannot open messages/lock", e)),
    };
    let Some(deadline) = deadline else {
        file.lock_shared()
            .map_err(|e| Failure::io("cannot lock messages/lock", e))?;
        return Ok(ReadLock::Held(file));
    };
    loop {
        match file.try_lock_shared() {
            Ok(()) => return Ok(ReadLock::Held(file)),
            Err(fs::TryLockError::WouldBlock) => {
                let now = Instant::now();
                if now >= deadline {
                    return Ok(ReadLock::TimedOut);
                }
                std::thread::sleep(Duration::from_millis(20).min(deadline - now));
            }
            Err(fs::TryLockError::Error(e)) => {
                return Err(Failure::io("cannot lock messages/lock", e));
            }
        }
    }
}

/// What one poll of a wait saw.
pub enum WaitSnapshot {
    Ready(Snapshot),
    /// The deadline passed while a writer held the lock.
    TimedOut,
}

/// A consistent read for a bounded wait; never waits past `deadline`.
pub fn wait_snapshot(bus: &Bus, deadline: Instant) -> Result<WaitSnapshot, Failure> {
    let _lock = match read_lock_until(&bus.paths, Some(deadline))? {
        ReadLock::TimedOut => return Ok(WaitSnapshot::TimedOut),
        ReadLock::NoStore => None,
        ReadLock::Held(f) => Some(f),
    };
    Ok(WaitSnapshot::Ready(scan(&bus.paths)))
}

/// A consistent read of the whole store, under `messages/lock` shared.
pub fn snapshot(bus: &Bus) -> Result<Snapshot, Failure> {
    snapshot_of(&bus.paths)
}

/// Read a store by its paths.
pub fn snapshot_of(paths: &StorePaths) -> Result<Snapshot, Failure> {
    let _lock = read_lock(paths)?;
    Ok(scan(paths))
}

// --- the store on disk ---

/// A recipient's acknowledgement of one record: `bus/acks/<id>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ack {
    pub schema: u32,
    pub id: String,
    pub by: Actor,
    pub at: String,
}

/// Everything readable in the store, plus what isn't.
#[derive(Debug, Default)]
pub struct Snapshot {
    /// Readable records, by sequence number.
    pub records: BTreeMap<u64, Record>,
    pub acks: BTreeMap<u64, Ack>,
    /// Unreadable files, gaps and inconsistencies: any of these stops mutations.
    pub problems: Vec<String>,
    /// Stray temporary files left by a crash (harmless).
    pub strays: Vec<PathBuf>,
}

impl Snapshot {
    pub fn healthy(&self) -> bool {
        self.problems.is_empty()
    }

    pub(super) fn require_healthy(&self) -> Result<(), Failure> {
        match self.problems.first() {
            None => Ok(()),
            Some(first) => Err(Failure::new(
                Code::Corrupt,
                format!(
                    "the store is corrupt ({first}{}); mutations are stopped. `brg bus check` lists the problems",
                    if self.problems.len() > 1 {
                        format!(", and {} more", self.problems.len() - 1)
                    } else {
                        String::new()
                    }
                ),
            )),
        }
    }

    pub fn get(&self, id: &str) -> Option<&Record> {
        parse_record_id(id).and_then(|seq| self.records.get(&seq))
    }

    pub fn is_acked(&self, seq: u64) -> bool {
        self.acks.contains_key(&seq)
    }
}

/// Names in a store directory, split into regular names and stray temporaries.
fn list_dir(dir: &Path, problems: &mut Vec<String>, strays: &mut Vec<PathBuf>) -> Vec<String> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            problems.push(format!("cannot list {}: {e}", dir.display()));
            return Vec::new();
        }
    };
    let mut names = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with(TMP_PREFIX) {
                    strays.push(entry.path());
                } else {
                    names.push(name);
                }
            }
            Err(e) => problems.push(format!("cannot list {}: {e}", dir.display())),
        }
    }
    names.sort();
    names
}

fn read_limited(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("not a regular file".into());
    }
    if meta.len() > limit as u64 {
        return Err(format!("larger than {limit} bytes"));
    }
    fs::read(path).map_err(|e| e.to_string())
}

/// Read the whole store. Never writes.
pub fn scan(paths: &StorePaths) -> Snapshot {
    let mut s = Snapshot::default();
    let mut problems = Vec::new();
    let mut strays = Vec::new();

    // Records: `<seq:010>.json`, contiguous from 1.
    let mut seqs = BTreeSet::new();
    let records_dir = paths.records_dir();
    for name in list_dir(&records_dir, &mut problems, &mut strays) {
        let seq = name
            .strip_suffix(".json")
            .filter(|n| n.len() == 10 && n.chars().all(|c| c.is_ascii_digit()))
            .and_then(|n| n.parse::<u64>().ok())
            .filter(|n| *n > 0);
        let Some(seq) = seq else {
            problems.push(format!("records/{name}: not a record name"));
            continue;
        };
        seqs.insert(seq);
        let parsed = read_limited(&records_dir.join(&name), MAX_RECORD_BYTES).and_then(|b| {
            serde_json::from_slice::<Record>(&b).map_err(|e| format!("unreadable: {e}"))
        });
        match parsed {
            Err(e) => problems.push(format!("records/{name}: {e}")),
            Ok(r) if r.schema != super::SCHEMA => {
                problems.push(format!("records/{name}: schema {}", r.schema))
            }
            Ok(r) if r.seq != seq || r.id != record_id(seq) => {
                problems.push(format!("records/{name}: holds `{}` (seq {})", r.id, r.seq))
            }
            Ok(r) => {
                s.records.insert(seq, r);
            }
        }
    }
    // Never enumerate the missing numbers: a stray high-numbered file would
    // make that unbounded.
    if let Some(&highest) = seqs.last() {
        let missing = highest - seqs.len() as u64;
        if missing > 0 {
            let first = seqs
                .iter()
                .zip(1u64..)
                .find(|(seq, expected)| **seq != *expected)
                .map(|(_, expected)| expected)
                .unwrap_or(highest);
            problems.push(format!(
                "gap: {} missing (first: {})",
                if missing == 1 {
                    "1 record is".to_string()
                } else {
                    format!("{missing} records are")
                },
                record_id(first)
            ));
        }
    }

    scan_acks(paths, &mut s, &seqs, &mut problems, &mut strays);

    let _ = list_dir(&paths.bus_dir(), &mut Vec::new(), &mut strays);
    s.problems = problems
        .iter()
        .map(|p| hide_paths(p, &paths.root, None))
        .collect();
    // Shown relative to the message store, whose path is internal.
    s.strays = strays
        .into_iter()
        .map(|p| {
            p.strip_prefix(&paths.root)
                .map(Path::to_path_buf)
                .unwrap_or(p)
        })
        .collect();
    s
}

pub fn actor_label(actor: &Actor) -> String {
    match actor {
        Actor::Member { member, .. } => member.clone(),
        Actor::External { name } => format!("external:{name}"),
    }
}

fn is_recipient(actor: &Actor, record: &Record) -> bool {
    actor_label(actor) == record.to
}

fn scan_acks(
    paths: &StorePaths,
    s: &mut Snapshot,
    seqs: &BTreeSet<u64>,
    problems: &mut Vec<String>,
    strays: &mut Vec<PathBuf>,
) {
    // Acks: `<id>.json`, by the record's recipient.
    let acks_dir = paths.acks_dir();
    for name in list_dir(&acks_dir, problems, strays) {
        let Some(seq) = name.strip_suffix(".json").and_then(parse_record_id) else {
            problems.push(format!("acks/{name}: not an ack name"));
            continue;
        };
        let parsed = read_limited(&acks_dir.join(&name), 4096).and_then(|b| {
            serde_json::from_slice::<Ack>(&b).map_err(|e| format!("unreadable: {e}"))
        });
        match parsed {
            Err(e) => problems.push(format!("acks/{name}: {e}")),
            Ok(a) if a.schema != super::SCHEMA => {
                problems.push(format!("acks/{name}: schema {}", a.schema))
            }
            Ok(a) if a.id != record_id(seq) => {
                problems.push(format!("acks/{name}: names `{}`", a.id))
            }
            // Only a well-formed ack by the record's recipient counts as consumption.
            Ok(a) => match s.records.get(&seq) {
                None if !seqs.contains(&seq) => {
                    problems.push(format!("acks/{name}: no such record"))
                }
                Some(r) if !is_recipient(&a.by, r) => problems.push(format!(
                    "acks/{name}: by {}, but the record is addressed to {}",
                    actor_label(&a.by),
                    r.to
                )),
                _ => {
                    s.acks.insert(seq, a);
                }
            },
        }
    }
}

/// Parse and shape-check a draft (nothing needs the store yet).
pub fn parse_draft(bytes: &[u8]) -> Result<Draft, Failure> {
    let invalid = |m: String| Failure::new(Code::Invalid, m);
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(invalid(format!(
            "the draft is over {MAX_RECORD_BYTES} bytes"
        )));
    }
    let draft: Draft =
        serde_json::from_slice(bytes).map_err(|e| invalid(format!("the draft: {e}")))?;
    draft.check_shape().map_err(invalid)?;
    Ok(draft)
}

/// Who may address whom, from the latest start record.
pub fn check_route(meta: &state::Meta, actor: &Actor, to: &str) -> Result<(), String> {
    let to = Recipient::parse(to).ok_or_else(|| format!("`to`: `{to}` is not a recipient"))?;
    match (actor, &to) {
        (_, Recipient::Member(m)) if !meta.members.contains_key(m) => Err(not_a_member(meta, m)),
        (Actor::External { .. }, Recipient::Member(_)) => Ok(()),
        (Actor::External { .. }, Recipient::External(_)) => {
            Err("an outside caller can only publish to members".into())
        }
        (Actor::Member { member, .. }, Recipient::Member(m)) => {
            let parent_of = |x: &str| meta.members.get(x).and_then(|mm| mm.parent.clone());
            if parent_of(member).as_deref() == Some(m.as_str())
                || parent_of(m).as_deref() == Some(member.as_str())
            {
                Ok(())
            } else {
                Err(format!(
                    "`{member}` can publish only to its parent or its children, not `{m}`"
                ))
            }
        }
        (Actor::Member { member, .. }, Recipient::External(_)) => {
            match meta.members.get(member).and_then(|m| m.parent.as_ref()) {
                None => Ok(()),
                Some(parent) => Err(format!(
                    "only the team's root may publish outside the instance; `{member}` reports to `{parent}`"
                )),
            }
        }
    }
}

/// Why `m` can't receive messages: an instance, a role, or no one.
pub(crate) fn not_a_member(meta: &state::Meta, m: &str) -> String {
    let members: Vec<&str> = meta.members.keys().map(String::as_str).collect();
    let all = if members.is_empty() {
        "it has no members".to_string()
    } else {
        format!("its members: {}", members.join(", "))
    };
    let of_role: Vec<&str> = meta
        .members
        .iter()
        .filter(|(_, entry)| entry.role == m)
        .map(|(id, _)| id.as_str())
        .collect();
    if !of_role.is_empty() {
        return format!(
            "`{m}` is a role; send to its member in full: {}",
            of_role.join(", ")
        );
    }
    format!("`{m}` is not a member ({all}); create a member with `brg create <instance>/<role>`")
}

/// Every successful publish commits a new immutable message.
pub fn publish(bus: &Bus, caller: &Caller, draft_bytes: &[u8]) -> Result<Record, Failure> {
    let draft = parse_draft(draft_bytes)?;
    let actor = actor_for(bus, caller)?;
    bus.check_route(&actor, &draft.to)
        .map_err(|e| Failure::new(Code::Invalid, e))?;
    let (bus, _lock) = lock_for_mutation(bus)?;
    recheck_authority(&bus, caller)?;
    bus.check_route(&actor, &draft.to)
        .map_err(|e| Failure::new(Code::Invalid, e))?;
    let snap = scan(&bus.paths);
    snap.require_healthy()?;
    if let Some(missing) = draft.reply_to.iter().find(|id| snap.get(id).is_none()) {
        return Err(Failure::new(
            Code::Invalid,
            format!("`{missing}` names no record"),
        ));
    }
    let seq = snap.records.len() as u64 + 1;
    let rec = Record {
        schema: super::SCHEMA,
        id: record_id(seq),
        seq,
        at: time::utc_now(),
        from: actor_label(&actor),
        to: draft.to,
        reply_to: draft.reply_to,
        body: draft.body,
    };
    let bytes =
        serde_json::to_vec_pretty(&rec).map_err(|e| Failure::new(Code::Error, e.to_string()))?;
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(Failure::new(
            Code::Invalid,
            format!("the record would be over {MAX_RECORD_BYTES} bytes"),
        ));
    }
    match fsx::write_new(&bus.paths.record(seq), &bytes)
        .map_err(|e| Failure::io("cannot write the record", e))?
    {
        Written::Created => Ok(rec),
        Written::Exists => Err(Failure::new(
            Code::Corrupt,
            "the next record already exists; `brg bus check` lists the problems",
        )),
    }
}

// --- listing ---

/// A recipient's progress, computed in one pass over the readable messages.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Progress {
    pub unread: usize,
    pub newest: u64,
    pub lowest_unacked: Option<u64>,
    pub total: usize,
}

pub fn progress(bus: &Bus, snap: &Snapshot) -> BTreeMap<String, Progress> {
    let mut out: BTreeMap<String, Progress> = bus
        .meta
        .members
        .keys()
        .map(|id| (id.clone(), Progress::default()))
        .collect();
    for r in snap.records.values() {
        if bus.record.members.get(&r.to).is_some_and(|m| m.deleted) {
            continue;
        }
        let p = out.entry(r.to.clone()).or_default();
        p.total += 1;
        p.newest = r.seq;
        if !snap.is_acked(r.seq) {
            p.unread += 1;
            p.lowest_unacked.get_or_insert(r.seq);
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct Inbox {
    pub records: Vec<Record>,
    pub unacked: usize,
    pub total: usize,
}

/// Every unacked message to this recipient, oldest first.
pub fn list(bus: &Bus, snap: &Snapshot, to: &Recipient) -> Inbox {
    let to = to.to_string();
    let p = progress(bus, snap).get(&to).copied().unwrap_or_default();
    Inbox {
        records: snap
            .records
            .values()
            .filter(|r| r.to == to && !snap.is_acked(r.seq))
            .cloned()
            .collect(),
        unacked: p.unread,
        total: p.total,
    }
}

/// A stored record as shown to callers.
fn shown(r: &Record) -> Value {
    serde_json::to_value(r).unwrap_or(Value::Null)
}

/// A message's header, with its body length.
pub fn header(r: &Record, acked: bool) -> Value {
    let mut v = shown(r);
    if let Some(map) = v.as_object_mut() {
        map.remove("body");
        map.insert("body_bytes".into(), json!(r.body.len()));
        map.insert("acked".into(), json!(acked));
    }
    v
}

pub fn full(r: &Record, acked: bool) -> Value {
    let mut v = shown(r);
    if let Some(map) = v.as_object_mut() {
        map.insert("acked".into(), json!(acked));
    }
    v
}

/// Read only the next immutable record. Missing means no new publication;
/// an existing unreadable or inconsistent record is never skipped.
fn next_record(paths: &StorePaths, seq: u64) -> Result<Option<Record>, Failure> {
    let path = paths.record(seq);
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Failure::io("cannot read the next message", e)),
        Ok(_) => {}
    }
    let bytes = read_limited(&path, MAX_RECORD_BYTES).map_err(|e| {
        Failure::new(
            Code::Corrupt,
            format!("record {} is unreadable: {e}", record_id(seq)),
        )
    })?;
    let record: Record = serde_json::from_slice(&bytes).map_err(|e| {
        Failure::new(
            Code::Corrupt,
            format!("record {} is unreadable: {e}", record_id(seq)),
        )
    })?;
    if record.schema != super::SCHEMA || record.seq != seq || record.id != record_id(seq) {
        return Err(Failure::new(
            Code::Corrupt,
            format!("record {} is inconsistent", record_id(seq)),
        ));
    }
    Ok(Some(record))
}

fn refresh_acks(
    bus: &Bus,
    snap: &mut Snapshot,
    next: &mut u64,
    deadline: Instant,
    check: &mut impl FnMut() -> Result<(), Failure>,
) -> Result<(), Failure> {
    #[cfg(feature = "test-support")]
    if let Some(gate) = std::env::var_os("BORGCHART_TEST_WAIT_BEFORE_ACK_REFRESH") {
        let gate = std::path::PathBuf::from(gate);
        fs::write(gate.join("ready"), b"")
            .map_err(|e| Failure::io("cannot signal the test ack refresh", e))?;
        while !gate.join("proceed").exists() {
            check()?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Failure::new(Code::Timeout, "test ack refresh timed out"));
            }
            std::thread::sleep(POLL.min(remaining));
        }
    }
    let _lock = wait_read_lock(bus, deadline, check)?;
    // A publisher and recipient may have committed both a message and its
    // ack since the last poll. Catch up under the same lock as the ack scan.
    while let Some(record) = next_record(&bus.paths, *next)? {
        snap.records.insert(*next, record);
        *next += 1;
        if Instant::now() >= deadline {
            return Err(Failure::new(
                Code::Timeout,
                "nothing new before the timeout",
            ));
        }
    }
    snap.acks.clear();
    let seqs = snap.records.keys().copied().collect();
    let mut problems = Vec::new();
    let mut strays = Vec::new();
    scan_acks(&bus.paths, snap, &seqs, &mut problems, &mut strays);
    snap.problems = problems;
    snap.require_healthy()
}

/// A publisher must not keep a wait from noticing that its member ended.
fn wait_read_lock(
    bus: &Bus,
    deadline: Instant,
    check: &mut impl FnMut() -> Result<(), Failure>,
) -> Result<Option<fs::File>, Failure> {
    loop {
        check()?;
        match read_lock_until(&bus.paths, Some(deadline.min(Instant::now() + POLL)))? {
            ReadLock::NoStore => return Ok(None),
            ReadLock::Held(lock) => return Ok(Some(lock)),
            ReadLock::TimedOut if Instant::now() >= deadline => {
                return Err(Failure::new(
                    Code::Timeout,
                    "nothing new before the timeout",
                ));
            }
            ReadLock::TimedOut => {}
        }
    }
}

/// One scan, then incremental reads. Already-present messages wait for the
/// minimum; a new unacked message can return sooner. CLI errors back off too.
pub fn wait(
    bus: &Bus,
    caller: &Caller,
    to: &Recipient,
    started: Instant,
    duration: Duration,
) -> Result<Snapshot, Failure> {
    let deadline = started + duration;
    let minimum = started + wait_minimum();
    let timeout = || Failure::new(Code::Timeout, "nothing new before the timeout");
    let mut next_liveness = Instant::now();
    let mut check = || {
        actor_for(bus, caller)?;
        if let Caller::Member {
            member,
            incarnation,
        } = caller
            && Instant::now() >= next_liveness
        {
            let live = lifecycle::wait_liveness(&bus.state, member, incarnation);
            next_liveness = Instant::now() + Duration::from_secs(1);
            if live == lifecycle::Liveness::Gone {
                return Err(Failure::new(
                    Code::Fenced,
                    format!("{}/{member}'s start ended; stop waiting", bus.instance),
                ));
            }
        }
        Ok(())
    };
    let mut snap = loop {
        check()?;
        match wait_snapshot(bus, deadline.min(Instant::now() + POLL))? {
            WaitSnapshot::Ready(snap) => break snap,
            WaitSnapshot::TimedOut if Instant::now() >= deadline => return Err(timeout()),
            WaitSnapshot::TimedOut => {}
        }
    };
    snap.require_healthy()?;
    let mark = snap.records.last_key_value().map_or(0, |(&seq, _)| seq);
    let mut next = mark + 1;
    let recipient = to.to_string();
    let mut old_pending = snap
        .records
        .values()
        .any(|r| r.to == recipient && !snap.is_acked(r.seq));
    loop {
        check()?;
        let mut arrived = false;
        while let Some(record) = next_record(&bus.paths, next)? {
            check()?;
            arrived |= record.to == recipient;
            snap.records.insert(next, record);
            next += 1;
            if Instant::now() >= deadline {
                break;
            }
        }
        if arrived || (old_pending && Instant::now() >= minimum) {
            refresh_acks(bus, &mut snap, &mut next, deadline, &mut check)?;
            check()?;
            let inbox = list(bus, &snap, to);
            if (old_pending && Instant::now() >= minimum && !inbox.records.is_empty())
                || inbox.records.iter().any(|r| r.seq > mark)
            {
                return Ok(snap);
            }
            if inbox.records.is_empty() {
                // Everything pending at the start was acked: from now on only
                // a new message returns, without rescanning acks every poll.
                old_pending = false;
            }
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(timeout());
        }
        let next_check = if old_pending && minimum > now {
            deadline.min(minimum)
        } else {
            deadline
        };
        std::thread::sleep(POLL.min(next_check.saturating_duration_since(now)));
    }
}

// --- acknowledging ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckOutcome {
    Acked,
    /// It was already acknowledged: nothing changed.
    Already,
}

impl AckOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            AckOutcome::Acked => "acked",
            AckOutcome::Already => "already",
        }
    }
}

/// Acknowledge records as their recipient. All IDs are checked before any
/// is written.
pub fn ack(
    bus: &Bus,
    caller: &Caller,
    ids: &[String],
) -> Result<Vec<(String, AckOutcome)>, Failure> {
    if ids.is_empty() {
        return Err(Failure::new(Code::Usage, "name at least one record ID"));
    }
    let seqs = ids
        .iter()
        .map(|id| {
            parse_record_id(id)
                .ok_or_else(|| Failure::new(Code::Usage, format!("`{id}` is not a record ID")))
        })
        .collect::<Result<Vec<u64>, Failure>>()?;
    let actor = actor_for(bus, caller)?;
    let (bus, lock) = lock_for_mutation(bus)?;
    let bus = &bus;
    recheck_authority(bus, caller)?;
    let snap = scan(&bus.paths);
    snap.require_healthy()?;
    for (id, seq) in ids.iter().zip(&seqs) {
        let rec = snap
            .records
            .get(seq)
            .ok_or_else(|| Failure::new(Code::NotFound, format!("no record `{id}`")))?;
        if !is_recipient(&actor, rec) {
            return Err(Failure::new(
                Code::Refused,
                format!(
                    "{id} is addressed to {}; only its recipient may acknowledge it",
                    rec.to
                ),
            ));
        }
    }
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for (id, seq) in ids.iter().zip(&seqs) {
        if snap.is_acked(*seq) || !seen.insert(*seq) {
            out.push((id.clone(), AckOutcome::Already));
            continue;
        }
        let a = Ack {
            schema: super::SCHEMA,
            id: id.clone(),
            by: actor.clone(),
            at: time::utc_now(),
        };
        let bytes = serde_json::to_vec(&a).map_err(|e| Failure::new(Code::Error, e.to_string()))?;
        let outcome = match fsx::write_new(&bus.paths.ack(id), &bytes)
            .map_err(|e| Failure::io("cannot write the ack", e))?
        {
            Written::Created => AckOutcome::Acked,
            Written::Exists => AckOutcome::Already,
        };
        out.push((id.clone(), outcome));
    }
    drop(lock);
    Ok(out)
}

// --- check ---

#[derive(Debug, Clone, Default)]
pub struct CheckReport {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub records: usize,
    /// True when the store is damaged.
    pub corrupt: bool,
}

pub fn check(bus: &Bus) -> Result<CheckReport, Failure> {
    let snap = snapshot(bus)?;
    let mut report = CheckReport {
        records: snap.records.len(),
        corrupt: !snap.healthy(),
        errors: snap.problems.clone(),
        ..CheckReport::default()
    };
    for stray in &snap.strays {
        report.warnings.push(format!(
            "stray temporary file (left by a crash; safe to remove): {}",
            stray.display()
        ));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn routing(edges: &[(&str, Option<&str>)]) -> state::Meta {
        state::Meta {
            members: edges
                .iter()
                .map(|(m, p)| {
                    (
                        (*m).into(),
                        state::MemberMeta {
                            vendor: "claude".into(),
                            parent: p.map(str::to_string),
                            ..state::MemberMeta::default()
                        },
                    )
                })
                .collect(),
            ..state::Meta::default()
        }
    }

    fn member(m: &str) -> Actor {
        Actor::Member {
            member: m.into(),
            incarnation: format!("{m}-1"),
        }
    }

    #[test]
    fn routes_follow_parent_edges() {
        let m = routing(&[("lead", None), ("a", Some("lead")), ("b", Some("lead"))]);
        assert!(check_route(&m, &member("a"), "lead").is_ok());
        assert!(check_route(&m, &member("lead"), "b").is_ok());
        assert!(check_route(&m, &member("a"), "b").is_err(), "siblings");
        assert!(check_route(&m, &member("a"), "a").is_err(), "self");
        assert!(check_route(&m, &member("lead"), "nobody").is_err());
        assert!(check_route(&m, &member("lead"), "external:ops").is_ok());
        assert!(check_route(&m, &member("a"), "external:ops").is_err());
        let ext = Actor::External { name: "ops".into() };
        assert!(check_route(&m, &ext, "b").is_ok());
        assert!(check_route(&m, &ext, "external:other").is_err());
    }

    #[test]
    fn scan_finds_gaps_and_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let paths = StorePaths::from_root(dir.path());
        assert!(scan(&paths).healthy());
        let rec = |seq: u64| Record {
            schema: super::super::SCHEMA,
            id: record_id(seq),
            seq,
            at: "2026-11-05T19:00:00.000Z".into(),
            from: "a".into(),
            to: "lead".into(),
            reply_to: vec![],
            body: String::new(),
        };
        for seq in [1, 3] {
            fsx::write_new(&paths.record(seq), &serde_json::to_vec(&rec(seq)).unwrap()).unwrap();
        }
        let s = scan(&paths);
        assert_eq!(s.records.len(), 2);
        assert!(
            s.problems.iter().any(|p| p.contains("gap")),
            "{:?}",
            s.problems
        );
        fsx::write_new(&paths.record(2), b"{not json").unwrap();
        let s = scan(&paths);
        assert!(!s.healthy());
        assert!(s.problems.iter().all(|p| !p.contains("gap")));
        assert!(s.problems.iter().any(|p| p.contains("0000000002")));
    }
}
