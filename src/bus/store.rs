//! The record store: publish, list, read, acknowledge, check and inspect.
//!
//! Records are immutable files numbered from 1 with no gaps. Every mutation
//! takes `bus/lock` exclusive, rescans the store, and refuses (`corrupt`) if
//! anything in it is unreadable or missing. Sequence numbers and operation-key idempotency
//! come from the records themselves: the record is the commit point, and the
//! key index beside it is a cache that is repaired when found missing.
//!
//! Everything works on the team's current run (internal: users never see
//! runs, so messages here talk about the team and its members). Reads hold
//! `bus/lock` shared and never create anything.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::deploy::{BusConfig, Deployment};
use crate::state;

use super::attention_hooks::{self, Attention};
use super::context::Caller;
use super::fsx::{self, TMP_PREFIX, Written};
use super::layout::{RunPaths, TeamStore};
use super::manifest::{self, ManifestError, RunManifest, hex};
use super::outcome::{Code, Failure};
use super::record::{
    self, Actor, Artifact, Draft, Kind, MAX_RECORD_BYTES, Recipient, Record, parse_record_id,
    record_id,
};
use super::time;

/// Largest artifact brg will measure.
pub const MAX_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;
pub const DEFAULT_PAGE: usize = 20;
pub const MAX_PAGE: usize = 100;
pub const MAX_WAIT_SECS: u64 = 600;
pub const MIN_BACKGROUND_WAIT_SECS: u64 = 60;
pub const MAX_BACKGROUND_WAIT_SECS: u64 = 86400;
pub const POLL: Duration = Duration::from_millis(250);

/// The loop guard also applies to errors before an inbox can be opened.
pub fn background_wait_minimum() -> Duration {
    #[cfg(feature = "test-support")]
    if let Some(ms) = std::env::var("BORGCHART_WAIT_MIN_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    {
        return Duration::from_millis(ms);
    }
    Duration::from_secs(MIN_BACKGROUND_WAIT_SECS)
}

/// A team's opened message storage: the deployment's bus configuration and
/// its current run's manifest.
#[derive(Debug, Clone)]
pub struct Bus {
    pub deployment: Deployment,
    pub config: BusConfig,
    /// The team's name in messages.
    pub team: String,
    /// How to restart the team, from where the caller is (`state::restart_hint`).
    pub restart: String,
    pub store: TeamStore,
    /// The current run's ID (internal; never shown).
    pub run: String,
    pub paths: RunPaths,
    pub manifest: RunManifest,
}

/// Open the team's current run, read-only. Creates nothing.
pub fn open(deployment: &Deployment) -> Result<Bus, Failure> {
    let config = deployment.bus.clone().ok_or_else(|| {
        Failure::new(
            Code::NotConfigured,
            format!("{} has no `bus:` section", deployment.file.display()),
        )
    })?;
    let team = state::team_name(&deployment.file);
    let restart = state::restart_hint(&deployment.file);
    let id = state::deployment_id(&deployment.file);
    let store = TeamStore::new(&config, &id);
    let unreadable =
        |detail: &str| format!("team `{team}`'s message storage is unreadable ({detail})");
    let not_started = || {
        Failure::new(
            Code::NotConfigured,
            format!("team `{team}` hasn't been started yet"),
        )
    };
    let run = store
        .current()
        .map_err(|e| {
            let detail = if e.kind() == io::ErrorKind::Other {
                "its current storage is not named properly".to_string()
            } else {
                e.to_string()
            };
            Failure::new(Code::Error, unreadable(&detail))
        })?
        .map_or_else(
            || {
                // Runs without `current`: not a first start, and never
                // something to start over on top of.
                match store.runs() {
                    Ok(runs) if runs.is_empty() => Err(not_started()),
                    Ok(_) => Err(Failure::new(
                        Code::Corrupt,
                        format!(
                            "team `{team}`'s message storage is damaged: it holds messages but no longer says which are current. Repair it by hand; brg won't start the team over it"
                        ),
                    )),
                    Err(e) => Err(Failure::new(Code::Error, unreadable(&e.to_string()))),
                }
            },
            Ok,
        )?;
    let paths = store.run(&run);
    let manifest = match manifest::load_manifest(&paths) {
        Ok(Some(m)) => m,
        // `current` names storage that isn't set up yet.
        Ok(None) => return Err(not_started()),
        Err(e @ ManifestError::Format { .. }) => {
            return Err(Failure::new(Code::Refused, e.message(&team, &restart)));
        }
        Err(ManifestError::Unreadable(e)) => {
            let detail = reason(&e, &paths.manifest());
            return Err(Failure::new(Code::Error, unreadable(&detail)));
        }
    };
    if manifest.run != run {
        return Err(Failure::new(
            Code::Corrupt,
            format!("team `{team}`'s message storage is inconsistent (its manifest is misplaced)"),
        ));
    }
    if manifest.deployment != id {
        return Err(Failure::new(
            Code::Refused,
            format!(
                "team `{team}`'s message storage belongs to another deployment (`{}`), not this one (`{id}`)",
                manifest.deployment
            ),
        ));
    }
    Ok(Bus {
        deployment: deployment.clone(),
        config,
        team,
        restart,
        store,
        run,
        paths,
        manifest,
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

/// `text` without the internal paths of a team's storage: paths inside the
/// run become relative (`bus/records`), the run itself "its messages
/// folder", and the team's directory "its folder".
fn hide_paths(text: &str, run_root: &Path, team_dir: Option<&Path>) -> String {
    let root = run_root.display().to_string();
    let mut out = text
        .replace(&format!("{root}/"), "")
        .replace(&root, "its messages folder");
    if let Some(dir) = team_dir {
        out = out.replace(&dir.display().to_string(), "its folder");
    }
    out
}

impl Bus {
    /// `text` without this team's internal storage paths.
    fn hide_paths(&self, text: &str) -> String {
        hide_paths(text, &self.paths.root, Some(&self.store.dir))
    }

    /// Refuse a reference (a record ID the caller saw before) when the
    /// mutation followed a rollover: in the new storage that ID may name a
    /// different message.
    fn stale_reference(&self, id: &str) -> Failure {
        Failure::new(
            Code::Fenced,
            format!(
                "team `{}` was restarted, so `{id}` may now be a different message; list the inbox again",
                self.team
            ),
        )
    }
}

// --- callers ---

/// The actor a caller publishes and acknowledges as. Members are checked
/// against the run (`fenced` if stale); operators may not mutate.
pub fn actor_for(bus: &Bus, caller: &Caller) -> Result<Actor, Failure> {
    match caller {
        Caller::Operator => Err(Failure::new(
            Code::Refused,
            "no caller identity: members publish as themselves, outside callers pass `--external NAME`",
        )),
        Caller::External { name } => Ok(Actor::External { name: name.clone() }),
        Caller::Member {
            member,
            run,
            incarnation,
        } => {
            fence_member(bus, member, run, incarnation)?;
            Ok(Actor::Member {
                member: member.clone(),
                incarnation: incarnation.clone(),
            })
        }
    }
}

/// How to start one member, from where the caller is: the `up` half of the
/// team's restart hint, with `--member`.
fn member_hint(bus: &Bus, member: &str) -> String {
    let up = bus
        .restart
        .split_once(" && ")
        .map_or("brg up", |(_, up)| up);
    format!("{up} --member {member}")
}

fn fence_member(bus: &Bus, member: &str, run: &str, incarnation: &str) -> Result<(), Failure> {
    let fenced = |m: String| Err(Failure::new(Code::Fenced, m));
    let team = &bus.team;
    let start = member_hint(bus, member);
    if !bus.manifest.members.contains_key(member) {
        return fenced(format!("`{member}` is not a member of team `{team}`"));
    }
    if run != bus.run {
        return fenced(format!(
            "{team}/{member} is from before the team was restarted; restart it: {start}"
        ));
    }
    let current = manifest::load_incarnation(&bus.paths, member).map_err(|e| {
        Failure::new(
            Code::Error,
            format!(
                "{team}/{member}'s session record is unreadable ({})",
                reason(&e, &bus.paths.current_incarnation(member))
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

/// How many times a writer follows a rollover before giving up.
const MAX_ROLLOVER_FOLLOWS: usize = 3;

/// Create the store if needed and take `bus/lock` exclusive. The lock is
/// released when the returned file drops.
///
/// If the run was retired (the team's storage rolled over) by the time the
/// lock is held, release it, re-resolve the team's current run and lock
/// that instead, so nothing is written to storage that rollover has already
/// carried from. Returns the run that is locked, and whether that meant
/// following a rollover: the caller re-checks everything it checked before
/// against it, and refuses references to records it saw in the old storage.
fn lock_for_mutation(bus: &Bus) -> Result<(Bus, fs::File, bool), Failure> {
    let mut bus = bus.clone();
    let mut followed = false;
    for _ in 0..MAX_ROLLOVER_FOLLOWS {
        bus.store.ensure_run(&bus.run).map_err(|e| {
            Failure::new(
                Code::Error,
                format!(
                    "team `{}`'s message storage can't be used: {}",
                    bus.team,
                    bus.hide_paths(&e.to_string())
                ),
            )
        })?;
        for dir in [bus.paths.runtime_dir(), bus.paths.bus_dir()] {
            fsx::create_private_dirs(&dir)
                .map_err(|e| Failure::io("cannot create the store", e))?;
        }
        let lock = state::open_lock(&bus.paths.bus_lock())
            .map_err(|e| Failure::io("cannot open bus/lock", e))?;
        lock.lock()
            .map_err(|e| Failure::io("cannot lock bus/lock", e))?;
        if !bus.paths.retired().exists() {
            return Ok((bus, lock, followed));
        }
        followed = true;
        drop(lock);
        let next = open(&bus.deployment)?;
        if next.run == bus.run {
            return Err(Failure::new(
                Code::Corrupt,
                format!(
                    "team `{}`'s message storage is damaged: its current messages are marked as moved, but nothing replaced them",
                    bus.team
                ),
            ));
        }
        bus = next;
    }
    Err(Failure::new(
        Code::Error,
        format!(
            "team `{}`'s message storage kept changing (is the team being restarted?); try again",
            bus.team
        ),
    ))
}

/// The authoritative caller check, made while holding `bus/lock`:
/// incarnations change only under it, so what is checked here holds until
/// the write. The check before the lock only rejects early.
fn recheck_authority(bus: &Bus, caller: &Caller) -> Result<(), Failure> {
    actor_for(bus, caller).map(|_| ())
}

/// Hold `bus/lock` shared while reading, so a scan never sees a record
/// mid-publication without its key or ack. Opens the lock read-only and
/// creates nothing: with no lock file there is no store to read yet.
fn read_lock(paths: &RunPaths) -> Result<Option<fs::File>, Failure> {
    match read_lock_until(paths, None)? {
        ReadLock::Held(f) => Ok(Some(f)),
        ReadLock::NoStore => Ok(None),
        ReadLock::TimedOut => Err(Failure::new(Code::Error, "cannot lock bus/lock")),
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
fn read_lock_until(paths: &RunPaths, deadline: Option<Instant>) -> Result<ReadLock, Failure> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(paths.bus_lock())
    {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(ReadLock::NoStore),
        Err(e) => return Err(Failure::io("cannot open bus/lock", e)),
    };
    let Some(deadline) = deadline else {
        file.lock_shared()
            .map_err(|e| Failure::io("cannot lock bus/lock", e))?;
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
                return Err(Failure::io("cannot lock bus/lock", e));
            }
        }
    }
}

/// What one poll of a wait saw.
pub enum WaitSnapshot {
    Ready(Snapshot),
    /// The storage was retired (the team rolled over): resolve it again.
    Retired,
    /// The deadline passed while a writer held the lock.
    TimedOut,
}

/// A consistent read for a bounded wait: like [`snapshot`], but never waits
/// for the lock past `deadline`, and reports storage that rollover retired
/// (checked while holding the lock, so a poll queued behind a rollover
/// never reads the old storage as current).
pub fn wait_snapshot(bus: &Bus, deadline: Instant) -> Result<WaitSnapshot, Failure> {
    let _lock = match read_lock_until(&bus.paths, Some(deadline))? {
        ReadLock::TimedOut => return Ok(WaitSnapshot::TimedOut),
        ReadLock::NoStore => None,
        ReadLock::Held(f) => Some(f),
    };
    if bus.paths.retired().exists() {
        return Ok(WaitSnapshot::Retired);
    }
    Ok(WaitSnapshot::Ready(scan(&bus.paths, &bus.run)))
}

/// A consistent read of the whole store, under `bus/lock` shared.
pub fn snapshot(bus: &Bus) -> Result<Snapshot, Failure> {
    snapshot_of(&bus.paths, &bus.run)
}

/// The same for any run's store (an earlier run being carried from).
pub fn snapshot_of(paths: &RunPaths, run: &str) -> Result<Snapshot, Failure> {
    let _lock = read_lock(paths)?;
    Ok(scan(paths, run))
}

// --- the store on disk ---

/// A recipient's acknowledgement of one record: `bus/acks/<id>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ack {
    pub schema: u32,
    pub run: String,
    pub id: String,
    pub by: Actor,
    pub at: String,
}

/// One operation key: `bus/keys/<slot>/<sha256(key)>.json`. A cache; the
/// records are authoritative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyEntry {
    pub slot: String,
    pub key: String,
    pub id: String,
    pub digest: String,
}

/// Everything readable in the store, plus what isn't.
#[derive(Debug, Default)]
pub struct Snapshot {
    /// Readable records, by sequence number.
    pub records: BTreeMap<u64, Record>,
    pub acks: BTreeMap<u64, Ack>,
    pub keys: Vec<KeyEntry>,
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
pub fn scan(paths: &RunPaths, run: &str) -> Snapshot {
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
            Ok(r) if r.run != run => problems.push(format!("records/{name}: from other storage")),
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

    // Acks: `<id>.json`, by the record's recipient.
    let acks_dir = paths.acks_dir();
    for name in list_dir(&acks_dir, &mut problems, &mut strays) {
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
            Ok(a) if a.run != run => problems.push(format!("acks/{name}: from other storage")),
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

    // Keys: `keys/<slot>/<sha256(key)>.json`.
    let keys_root = paths.bus_dir().join("keys");
    for slot in list_dir(&keys_root, &mut problems, &mut strays) {
        let dir = keys_root.join(&slot);
        for name in list_dir(&dir, &mut problems, &mut strays) {
            let parsed = read_limited(&dir.join(&name), 4096).and_then(|b| {
                serde_json::from_slice::<KeyEntry>(&b).map_err(|e| format!("unreadable: {e}"))
            });
            match parsed {
                Err(e) => problems.push(format!("keys/{slot}/{name}: {e}")),
                Ok(k) if k.slot != slot || format!("{}.json", key_hash(&k.key)) != name => {
                    problems.push(format!("keys/{slot}/{name}: misfiled"))
                }
                Ok(k) => {
                    match s.get(&k.id) {
                        Some(r) if slot_of(&r.actor) != k.slot || r.key != k.key => {
                            problems.push(format!(
                                "keys/{slot}/{name}: points at {}, which has another key",
                                k.id
                            ))
                        }
                        None if !parse_record_id(&k.id).is_some_and(|n| seqs.contains(&n)) => {
                            problems.push(format!("keys/{slot}/{name}: no record {}", k.id))
                        }
                        _ => {}
                    }
                    s.keys.push(k);
                }
            }
        }
    }
    let _ = list_dir(&paths.bus_dir(), &mut Vec::new(), &mut strays);
    s.problems = problems
        .iter()
        .map(|p| hide_paths(p, &paths.root, None))
        .collect();
    // Shown relative to the run, whose own path is internal.
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

pub fn slot_of(actor: &Actor) -> String {
    match actor {
        Actor::Member { member, .. } => member.clone(),
        Actor::External { name } => format!("external.{name}"),
        Actor::Bus => "bus".to_string(),
    }
}

pub fn actor_label(actor: &Actor) -> String {
    match actor {
        Actor::Member { member, .. } => member.clone(),
        Actor::External { name } => format!("external:{name}"),
        Actor::Bus => "bus".to_string(),
    }
}

fn is_recipient(actor: &Actor, record: &Record) -> bool {
    match (actor, Recipient::parse(&record.to)) {
        (Actor::Member { member, .. }, Some(Recipient::Member(to))) => *member == to,
        (Actor::External { name }, Some(Recipient::External(to))) => *name == to,
        _ => false,
    }
}

fn key_hash(key: &str) -> String {
    hex(&Sha256::digest(key.as_bytes()))
}

/// Size and SHA-256 of a regular file, refusing anything over `limit` bytes.
pub fn sha256_file(path: &Path, limit: u64) -> io::Result<(u64, String)> {
    use std::os::unix::fs::OpenOptionsExt;
    // Refuse a FIFO or device before opening it (opening a FIFO blocks), and
    // open without blocking in case the path is swapped for one meanwhile;
    // the descriptor is checked again below. Symlinks to files are followed.
    if !fs::metadata(path)?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    if meta.len() > limit {
        return Err(io::Error::other(format!("larger than {limit} bytes")));
    }
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > limit {
            return Err(io::Error::other(format!("larger than {limit} bytes")));
        }
        hasher.update(&buf[..n]);
    }
    Ok((total, hex(&hasher.finalize())))
}

// --- publishing ---

/// What a publication produced.
#[derive(Debug, Clone)]
pub struct Published {
    pub record: Record,
    /// The key was used before for the same draft: this is the original.
    pub duplicate: bool,
    pub attention: Attention,
}

/// The content an operation key commits to: the draft as parsed plus each
/// artifact's measured size and digest.
#[derive(Serialize)]
struct DigestView<'a> {
    to: &'a str,
    kind: Kind,
    task: &'a Option<String>,
    attempt: &'a Option<u32>,
    reply_to: &'a [String],
    body: &'a str,
    artifacts: &'a [Artifact],
    facts: &'a Map<String, Value>,
}

fn digest_of(view: &DigestView) -> String {
    let bytes = serde_json::to_vec(view).unwrap_or_default();
    hex(&Sha256::digest(&bytes))
}

fn record_digest(r: &Record) -> String {
    digest_of(&DigestView {
        to: &r.to,
        kind: r.kind,
        task: &r.task,
        attempt: &r.attempt,
        reply_to: &r.reply_to,
        body: &r.body,
        artifacts: &r.artifacts,
        facts: &r.facts,
    })
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
    check_facts(&draft).map_err(invalid)?;
    Ok(draft)
}

fn text_fact(facts: &Map<String, Value>, name: &str, kind: Kind) -> Result<(), String> {
    match facts.get(name) {
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(()),
        _ => Err(format!(
            "a `{}` record needs `facts.{name}` (non-empty text)",
            kind.as_str()
        )),
    }
}

fn one_of(
    facts: &Map<String, Value>,
    name: &str,
    allowed: &[&str],
    kind: Kind,
) -> Result<(), String> {
    match facts.get(name).and_then(Value::as_str) {
        Some(v) if allowed.contains(&v) => Ok(()),
        _ => Err(format!(
            "a `{}` record needs `facts.{name}`: {}",
            kind.as_str(),
            allowed.join(", ")
        )),
    }
}

fn checks_fact(facts: &Map<String, Value>) -> Result<(), String> {
    let Some(checks) = facts.get("checks") else {
        return Ok(());
    };
    let ok = checks.as_array().is_some_and(|a| {
        a.iter().all(|c| {
            c.get("name").and_then(Value::as_str).is_some()
                && c.get("outcome").and_then(Value::as_str).is_some()
        })
    });
    if ok {
        Ok(())
    } else {
        Err("`facts.checks` must be a list of {name, outcome}".into())
    }
}

/// The facts each kind requires (docs/bus.md "Records"). Unknown fact keys
/// are allowed: `facts` is the envelope's extension point.
pub fn check_facts(d: &Draft) -> Result<(), String> {
    let f = &d.facts;
    match d.kind {
        Kind::Assign => {
            if d.attempt.is_none() {
                return Err("an `assign` record needs `attempt`".into());
            }
            let agreement = f.get("agreement");
            let path_ok = agreement
                .and_then(|a| a.get("path"))
                .and_then(Value::as_str)
                .is_some_and(|p| p.starts_with('/'));
            let sha_ok = agreement
                .and_then(|a| a.get("sha256"))
                .and_then(Value::as_str)
                .is_some_and(|s| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()));
            if !path_ok || !sha_ok {
                return Err(
                    "an `assign` record needs `facts.agreement: {path (absolute), sha256}`".into(),
                );
            }
            text_fact(f, "scope", d.kind)?;
            if let Some(c) = f.get("checkout")
                && !c.as_str().is_some_and(|p| p.starts_with('/'))
            {
                return Err("`facts.checkout` must be an absolute path".into());
            }
            Ok(())
        }
        Kind::Decline | Kind::CancelRequest | Kind::Reopen => text_fact(f, "reason", d.kind),
        Kind::Result => {
            one_of(f, "status", &["complete", "partial", "blocked"], d.kind)?;
            checks_fact(f)
        }
        Kind::Verify => {
            if !f
                .get("result")
                .and_then(Value::as_str)
                .is_some_and(|r| parse_record_id(r).is_some())
            {
                return Err("a `verify` record needs `facts.result` (a record ID)".into());
            }
            one_of(f, "outcome", &["accepted", "rejected"], d.kind)?;
            checks_fact(f)
        }
        Kind::CancelConfirm => {
            if f.get("quiescent") != Some(&Value::Bool(true)) {
                return Err("a `cancel-confirm` record needs `facts.quiescent: true`".into());
            }
            text_fact(f, "evidence", d.kind)
        }
        Kind::Release => text_fact(f, "evidence", d.kind),
        Kind::Note | Kind::Accept | Kind::Question | Kind::Decision => Ok(()),
    }
}

/// Who may address whom, from the run's frozen topology.
pub fn check_route(manifest: &RunManifest, actor: &Actor, to: &str) -> Result<(), String> {
    let to = Recipient::parse(to).ok_or_else(|| format!("`to`: `{to}` is not a recipient"))?;
    match (actor, &to) {
        (_, Recipient::Member(m)) if !manifest.members.contains_key(m) => {
            Err(format!("`{m}` is not a member of the team"))
        }
        (Actor::External { .. }, Recipient::Member(_)) => Ok(()),
        (Actor::External { .. }, Recipient::External(_)) => {
            Err("an outside caller can only publish to members".into())
        }
        (Actor::Member { member, .. }, Recipient::Member(m)) => {
            let parent_of = |x: &str| manifest.members.get(x).and_then(|mm| mm.parent.clone());
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
            match manifest.members.get(member).and_then(|m| m.parent.as_ref()) {
                None => Ok(()),
                Some(parent) => Err(format!(
                    "only the team's root may publish outside the team; `{member}` reports to `{parent}`"
                )),
            }
        }
        (Actor::Bus, _) => Ok(()),
    }
}

fn measure(draft: &Draft) -> Result<Vec<Artifact>, Failure> {
    draft
        .artifacts
        .iter()
        .map(|a| {
            let (bytes, sha256) = sha256_file(&a.path, MAX_ARTIFACT_BYTES).map_err(|e| {
                Failure::new(Code::Invalid, format!("artifact {}: {e}", a.path.display()))
            })?;
            Ok(Artifact {
                path: a.path.clone(),
                label: a.label.clone(),
                bytes,
                sha256,
            })
        })
        .collect()
}

/// Publish a draft as `caller`, under operation key `key`.
pub fn publish(
    bus: &Bus,
    caller: &Caller,
    key: &str,
    draft_bytes: &[u8],
) -> Result<Published, Failure> {
    check_key(key)?;
    let actor = actor_for(bus, caller)?;
    publish_actor(bus, Some(caller), actor, key, draft_bytes)
}

fn check_key(key: &str) -> Result<(), Failure> {
    if record::is_valid_key(key) {
        Ok(())
    } else {
        Err(Failure::new(
            Code::Invalid,
            "`--key`: 1–128 characters of [A-Za-z0-9._:/@#+=-]",
        ))
    }
}

/// Publish a draft as an already-checked actor. brg itself publishes as
/// [`Actor::Bus`] when it carries work into a linked run.
pub fn publish_as(
    bus: &Bus,
    actor: Actor,
    key: &str,
    draft_bytes: &[u8],
) -> Result<Published, Failure> {
    publish_actor(bus, None, actor, key, draft_bytes)
}

/// `caller` is rechecked under the lock; brg's own records have none.
fn publish_actor(
    bus: &Bus,
    caller: Option<&Caller>,
    actor: Actor,
    key: &str,
    draft_bytes: &[u8],
) -> Result<Published, Failure> {
    check_key(key)?;
    let draft = parse_draft(draft_bytes)?;
    check_route(&bus.manifest, &actor, &draft.to).map_err(|e| Failure::new(Code::Invalid, e))?;
    let artifacts = measure(&draft)?;
    let digest = digest_of(&DigestView {
        to: &draft.to,
        kind: draft.kind,
        task: &draft.task,
        attempt: &draft.attempt,
        reply_to: &draft.reply_to,
        body: &draft.body,
        artifacts: &artifacts,
        facts: &draft.facts,
    });
    let slot = slot_of(&actor);

    let (bus, lock, followed) = lock_for_mutation(bus)?;
    let bus = &bus;
    let mut refs: Vec<&str> = draft.reply_to.iter().map(String::as_str).collect();
    if draft.kind == Kind::Verify
        && let Some(r) = draft.facts.get("result").and_then(Value::as_str)
    {
        refs.push(r);
    }
    if followed && let Some(first) = refs.first() {
        return Err(bus.stale_reference(first));
    }
    if let Some(caller) = caller {
        recheck_authority(bus, caller)?;
    }
    check_route(&bus.manifest, &actor, &draft.to).map_err(|e| Failure::new(Code::Invalid, e))?;
    let snap = scan(&bus.paths, &bus.run);
    snap.require_healthy()?;

    if let Some(existing) = snap
        .records
        .values()
        .find(|r| slot_of(&r.actor) == slot && r.key == key)
    {
        if record_digest(existing) != digest {
            return Err(Failure::new(
                Code::Conflict,
                format!(
                    "key `{key}` was already used by {slot} for {} with different content",
                    existing.id
                ),
            ));
        }
        write_key(&bus.paths, &slot, key, &existing.id, &digest)?;
        ensure_attention(bus);
        drop(lock);
        return Ok(Published {
            record: existing.clone(),
            duplicate: true,
            attention: Attention::None,
        });
    }

    if let Some(missing) = refs.iter().find(|r| snap.get(r).is_none()) {
        return Err(Failure::new(
            Code::Invalid,
            format!("`{missing}` names no record"),
        ));
    }
    // Task transitions, authority and liveness, against exactly the
    // records this one will follow.
    let probe = |member: &str, incarnation: &str| {
        super::tasks::recorded_liveness(&bus.paths, member, incarnation)
    };
    super::tasks::check_publish(bus, &snap, &actor, &draft, &probe)?;

    // A healthy store is contiguous from 1, so the next number is one past its size.
    let seq = snap.records.len() as u64 + 1;
    let rec = Record {
        schema: super::SCHEMA,
        run: bus.run.clone(),
        id: record_id(seq),
        seq,
        at: time::utc_now(),
        actor,
        key: key.to_string(),
        to: draft.to,
        kind: draft.kind,
        task: draft.task,
        attempt: draft.attempt,
        reply_to: draft.reply_to,
        body: draft.body,
        artifacts,
        facts: draft.facts,
    };
    let mut bytes =
        serde_json::to_vec_pretty(&rec).map_err(|e| Failure::new(Code::Error, e.to_string()))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(Failure::new(
            Code::Invalid,
            format!("the record would be over {MAX_RECORD_BYTES} bytes"),
        ));
    }
    match fsx::write_new(&bus.paths.record(seq), &bytes)
        .map_err(|e| Failure::io("cannot write the record", e))?
    {
        Written::Created => {}
        Written::Exists => {
            return Err(Failure::new(
                Code::Corrupt,
                format!(
                    "{} already exists: something wrote outside bus/lock",
                    rec.id
                ),
            ));
        }
    }
    write_key(&bus.paths, &slot, key, &rec.id, &digest)?;
    drop(lock);
    let attention = attention_hooks::after_publish(&bus.paths, &bus.deployment, &rec);
    ensure_attention(bus);
    Ok(Published {
        record: rec,
        duplicate: false,
        attention,
    })
}

/// Write (or repair) a key index entry. Must hold `bus/lock`.
fn write_key(
    paths: &RunPaths,
    slot: &str,
    key: &str,
    id: &str,
    digest: &str,
) -> Result<(), Failure> {
    let entry = KeyEntry {
        slot: slot.to_string(),
        key: key.to_string(),
        id: id.to_string(),
        digest: digest.to_string(),
    };
    let bytes = serde_json::to_vec(&entry).map_err(|e| Failure::new(Code::Error, e.to_string()))?;
    let path = paths.keys_dir(slot).join(format!("{}.json", key_hash(key)));
    fsx::write_new(&path, &bytes).map_err(|e| Failure::io("cannot write the key index", e))?;
    Ok(())
}

fn ensure_attention(bus: &Bus) {
    attention_hooks::ensure_if_pending(&bus.paths, &bus.deployment);
}

// --- listing ---

/// One page of an inbox.
#[derive(Debug, Clone)]
pub struct Page {
    pub records: Vec<Record>,
    /// Pass as `--after` for the next page; `None` when this was the last.
    pub next: Option<String>,
    /// Unacknowledged records addressed to the recipient (all of them).
    pub unacked: usize,
    pub total: usize,
}

/// Records addressed to `to`, oldest first, after `after`, at most `limit`;
/// unacknowledged only unless `all`.
pub fn list(snap: &Snapshot, to: &Recipient, after: Option<u64>, limit: usize, all: bool) -> Page {
    let to = to.to_string();
    let mine: Vec<&Record> = snap.records.values().filter(|r| r.to == to).collect();
    let unacked = mine.iter().filter(|r| !snap.is_acked(r.seq)).count();
    let matching: Vec<&Record> = mine
        .iter()
        .copied()
        .filter(|r| after.is_none_or(|a| r.seq > a))
        .filter(|r| all || !snap.is_acked(r.seq))
        .collect();
    let records: Vec<Record> = matching.iter().take(limit).map(|r| (*r).clone()).collect();
    let next = if matching.len() > records.len() {
        records.last().map(|r| r.id.clone())
    } else {
        None
    };
    Page {
        records,
        next,
        unacked,
        total: mine.len(),
    }
}

/// After a member listed its own inbox.
pub fn observed(bus: &Bus, member: &str, incarnation: &str, page: &Page) {
    let ids: Vec<String> = page.records.iter().map(|r| r.id.clone()).collect();
    if !ids.is_empty() && bus.store.ensure_run(&bus.run).is_ok() {
        attention_hooks::after_list(&bus.paths, member, incarnation, &ids);
    }
    ensure_attention(bus);
}

/// A record as shown: everything but the run it was stored in (internal).
fn shown(r: &Record) -> Value {
    let mut v = serde_json::to_value(r).unwrap_or(Value::Null);
    if let Some(map) = v.as_object_mut() {
        map.remove("run");
    }
    v
}

/// A record's header: everything but its body and facts.
pub fn header(r: &Record, acked: bool) -> Value {
    let mut v = shown(r);
    if let Some(map) = v.as_object_mut() {
        map.remove("body");
        map.remove("facts");
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
    let (bus, lock, followed) = lock_for_mutation(bus)?;
    let bus = &bus;
    if followed {
        return Err(bus.stale_reference(&ids[0]));
    }
    recheck_authority(bus, caller)?;
    let snap = scan(&bus.paths, &bus.run);
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
            run: bus.run.clone(),
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
    ensure_attention(bus);
    Ok(out)
}

// --- check and inspect ---

#[derive(Debug, Clone, Default)]
pub struct CheckReport {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub records: usize,
    pub age_days: Option<f64>,
    /// True when an error is about the store itself (rather than its age).
    pub corrupt: bool,
    /// The linked runs this run's work was carried into.
    pub carried_into: Vec<super::carry::Marker>,
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
    report.carried_into = super::carry::carried_into(&bus.store.dir, &bus.run);
    let carry = super::carry::check_notes(bus, &snap, &report.carried_into);
    report.warnings.extend(carry.warnings);
    match time::parse_utc(&bus.manifest.created_at) {
        None => report.errors.push(format!(
            "the storage's manifest: `created_at` is not a UTC time: {}",
            bus.manifest.created_at
        )),
        Some(created) => {
            let age = std::time::SystemTime::now()
                .duration_since(created)
                .unwrap_or_default()
                .as_secs_f64()
                / 86_400.0;
            report.age_days = Some((age * 10.0).round() / 10.0);
            let team = &bus.team;
            let restart = &bus.restart;
            if age >= bus.config.max_age_days as f64 && carry.retired {
                report.warnings.push(format!(
                    "run `{}` is {age:.1} days old, past max_age_days ({}), and was carried into a new run",
                    bus.run, bus.config.max_age_days
                ));
            } else if age >= bus.config.max_age_days as f64 {
                report.errors.push(format!(
                    "team `{team}`'s message storage is {age:.1} days old, past max_age_days ({}); restart the team to roll it over: {restart}",
                    bus.config.max_age_days
                ));
            } else if age >= bus.config.warn_age_days as f64 {
                report.warnings.push(format!(
                    "team `{team}`'s message storage is {age:.1} days old (warn_age_days {}); it rolls over at the first restart after {} days: {restart}",
                    bus.config.warn_age_days, bus.config.max_age_days
                ));
            }
        }
    }
    Ok(report)
}

/// A summary of the team's messages and their store, for `brg bus inspect`.
pub fn inspect(bus: &Bus, snap: &Snapshot, last: usize) -> Value {
    let mut recipients: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for m in bus.manifest.members.keys() {
        recipients.insert(m.clone(), (0, 0));
    }
    for r in snap.records.values() {
        let e = recipients.entry(r.to.clone()).or_default();
        e.0 += 1;
        if !snap.is_acked(r.seq) {
            e.1 += 1;
        }
    }
    let members: Map<String, Value> = bus
        .manifest
        .members
        .iter()
        .map(|(id, m)| {
            (
                id.clone(),
                json!({"vendor": m.vendor, "parent": m.parent, "model": m.model}),
            )
        })
        .collect();
    let recent: Vec<Value> = snap
        .records
        .values()
        .rev()
        .take(last)
        .rev()
        .map(|r| header(r, snap.is_acked(r.seq)))
        .collect();
    json!({
        "team": bus.team,
        "deployment": bus.manifest.deployment,
        "created_at": bus.manifest.created_at,
        "members": members,
        "recipients": recipients.iter().map(|(to, (total, unacked))| {
            json!({"to": to, "total": total, "unacked": unacked})
        }).collect::<Vec<_>>(),
        "records": snap.records.len(),
        "recent": recent,
        "store": {
            "healthy": snap.healthy(),
            "problems": snap.problems,
            "strays": snap.strays.len(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::manifest::ManifestMember;

    fn manifest(edges: &[(&str, Option<&str>)]) -> RunManifest {
        RunManifest {
            schema: 1,
            run: "r1".into(),
            created_at: "2026-11-05T19:00:00.000Z".into(),
            deployment: "d".into(),
            source: "/x/borgchart.yaml".into(),
            config: manifest::ConfigSnapshot {
                version: 1,
                digest: "00".into(),
                inputs: BTreeMap::new(),
            },
            members: edges
                .iter()
                .map(|(m, p)| {
                    (
                        m.to_string(),
                        ManifestMember {
                            vendor: "claude".into(),
                            parent: p.map(str::to_string),
                            model: None,
                        },
                    )
                })
                .collect(),
            linked_from: None,
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
        let m = manifest(&[("lead", None), ("a", Some("lead")), ("b", Some("lead"))]);
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
    fn required_facts() {
        let ok = |v: Value| parse_draft(&serde_json::to_vec(&v).unwrap());
        let sha = "a".repeat(64);
        assert!(
            ok(
                json!({"to": "a", "kind": "assign", "task": "t", "attempt": 1,
            "facts": {"agreement": {"path": "/x", "sha256": sha}, "scope": "all"}})
            )
            .is_ok()
        );
        assert!(
            ok(json!({"to": "a", "kind": "assign", "task": "t",
            "facts": {"agreement": {"path": "/x", "sha256": sha}, "scope": "all"}}))
            .is_err(),
            "attempt"
        );
        assert!(
            ok(
                json!({"to": "a", "kind": "assign", "task": "t", "attempt": 1,
            "facts": {"agreement": {"path": "x", "sha256": sha}, "scope": "all"}})
            )
            .is_err(),
            "relative agreement"
        );
        assert!(ok(json!({"to": "a", "kind": "decline", "task": "t"})).is_err());
        assert!(
            ok(json!({"to": "a", "kind": "decline", "task": "t", "facts": {"reason": "busy"}}))
                .is_ok()
        );
        assert!(
            ok(json!({"to": "a", "kind": "result", "task": "t", "facts": {"status": "done"}}))
                .is_err()
        );
        assert!(
            ok(
                json!({"to": "a", "kind": "result", "task": "t", "facts": {"status": "partial",
            "checks": [{"name": "tests", "outcome": "pass"}]}})
            )
            .is_ok()
        );
        assert!(
            ok(json!({"to": "a", "kind": "verify", "task": "t",
            "facts": {"result": "m2", "outcome": "accepted"}}))
            .is_ok()
        );
        assert!(
            ok(json!({"to": "a", "kind": "cancel-confirm", "task": "t",
            "facts": {"quiescent": false, "evidence": "x"}}))
            .is_err()
        );
        assert!(
            ok(json!({"to": "a", "kind": "release", "task": "t", "facts": {"evidence": "x"}}))
                .is_ok()
        );
        assert!(ok(json!({"to": "a", "kind": "note", "facts": {"anything": 1}})).is_ok());
    }

    #[test]
    fn digest_covers_artifact_bytes() {
        let a = |sha: &str| Artifact {
            path: "/x".into(),
            label: None,
            bytes: 1,
            sha256: sha.into(),
        };
        let view = |arts: &[Artifact]| {
            digest_of(&DigestView {
                to: "a",
                kind: Kind::Note,
                task: &None,
                attempt: &None,
                reply_to: &[],
                body: "hi",
                artifacts: arts,
                facts: &Map::new(),
            })
        };
        assert_eq!(view(&[a("11")]), view(&[a("11")]));
        assert_ne!(view(&[a("11")]), view(&[a("22")]));
    }

    #[test]
    fn scan_finds_gaps_and_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let paths = RunPaths::from_root(dir.path());
        assert!(scan(&paths, "r1").healthy());
        let rec = |seq: u64| Record {
            schema: 1,
            run: "r1".into(),
            id: record_id(seq),
            seq,
            at: "2026-11-05T19:00:00.000Z".into(),
            actor: member("a"),
            key: format!("k{seq}"),
            to: "lead".into(),
            kind: Kind::Note,
            task: None,
            attempt: None,
            reply_to: vec![],
            body: String::new(),
            artifacts: vec![],
            facts: Map::new(),
        };
        for seq in [1, 3] {
            fsx::write_new(&paths.record(seq), &serde_json::to_vec(&rec(seq)).unwrap()).unwrap();
        }
        let s = scan(&paths, "r1");
        assert_eq!(s.records.len(), 2);
        assert!(
            s.problems.iter().any(|p| p.contains("gap")),
            "{:?}",
            s.problems
        );
        fsx::write_new(&paths.record(2), b"{not json").unwrap();
        let s = scan(&paths, "r1");
        assert!(!s.healthy());
        assert!(s.problems.iter().all(|p| !p.contains("gap")));
        assert!(s.problems.iter().any(|p| p.contains("0000000002")));
        assert!(!scan(&paths, "other-run").problems.is_empty());
    }
}
