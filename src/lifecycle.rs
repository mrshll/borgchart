//! Safe lifecycle: the lifecycle locks, the checks a start makes before it
//! changes anything, the team's message storage at `up` (which run is in
//! use, and rolling it over), member incarnations and their liveness.
//!
//! Runs are internal: users never see or name them, and messages talk
//! about teams and members only. brg isn't pinned: every window, shim and
//! plugin process runs the `brg` that started it.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

use crate::bus::fsx::{self, Written};
use crate::bus::layout::{RunPaths, TeamStore};
use crate::bus::manifest::{
    self, ConfigSnapshot, Incarnation, LinkedRun, Liveness, ManifestError, PaneIdentity, hex,
};
use crate::bus::time::{parse_utc, utc_now};
use crate::deploy::BusConfig;
use crate::state::{self, Meta, StateDir};
use crate::validate::Validated;

/// `ended_reason` for an incarnation stopped by `brg down`.
pub const ENDED_STOPPED: &str = "stopped";
/// `ended_reason` for an incarnation whose window could not be started.
pub const ENDED_START_FAILED: &str = "start-failed";

// --- locks ---

/// Take an exclusive lock on `path` (created if needed), waiting for it.
fn lock_file(path: &Path) -> io::Result<File> {
    let file = state::open_lock(path)?;
    file.lock()?;
    Ok(file)
}

/// The deployment's lifecycle lock in brg's state directory, held by `up`
/// and `down` while they change anything.
pub fn lock_state(state: &StateDir) -> Result<File, String> {
    let path = state.lifecycle_lock();
    lock_file(&path).map_err(|e| format!("cannot lock {}: {e}", path.display()))
}

/// The run's `runtime/lifecycle.lock`, taken after [`lock_state`]. Errors
/// name no storage paths: they reach people.
pub fn lock_run(paths: &RunPaths) -> Result<File, String> {
    lock_file(&paths.lifecycle_lock())
        .map_err(|e| format!("cannot lock the team's message storage: {e}"))
}

/// The run's `bus/lock`, held briefly (see [`with_bus_lock`]).
fn lock_bus(paths: &RunPaths) -> Result<File, String> {
    lock_file(&paths.bus_lock()).map_err(|e| format!("cannot lock the team's messages: {e}"))
}

// --- messages ---

/// What changed between two configuration snapshots, in team and member
/// terms: "oncall/helper's instruction file changed since the team started;
/// restart the team to apply it: brg down && brg up". `restart` comes from
/// [`state::restart_hint`].
pub fn changed_since_start(
    team: &str,
    restart: &str,
    was: &ConfigSnapshot,
    now: &ConfigSnapshot,
) -> String {
    let differs = |k: &String| was.inputs.get(k) != now.inputs.get(k);
    let keys: Vec<&String> = was
        .inputs
        .keys()
        .chain(now.inputs.keys().filter(|k| !was.inputs.contains_key(*k)))
        .filter(|k| differs(k))
        .collect();
    let mut things: Vec<String> = Vec::new();
    if keys.iter().any(|k| *k == "deployment") {
        things.push(format!("{team}'s deployment file (borgchart.yaml)"));
    }
    if keys.iter().any(|k| *k == crate::team::TEAM_FILE) {
        things.push(format!("{team}'s team file ({})", crate::team::TEAM_FILE));
    }
    for k in &keys {
        if let Some(id) = k
            .strip_prefix("members/")
            .and_then(|rest| rest.split_once('/'))
            .map(|(id, _)| id)
        {
            let thing = format!("{team}/{id}'s instruction file");
            if !things.contains(&thing) {
                things.push(thing);
            }
        }
    }
    if things.is_empty() {
        // Only the digest's version differs: a brg that hashes differently.
        things.push(format!("{team}'s configuration"));
    }
    let list = match things.as_slice() {
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
        [] => unreachable!(),
    };
    format!("{list} changed since the team started; restart the team to apply it: {restart}")
}

// --- child starts into a running team ---

/// Refuse to start members into a running team whose configuration changed
/// since it started (a full `up` of a running team too). Nothing has been
/// written when this fails.
pub fn check_join(
    meta: &Meta,
    snapshot: &ConfigSnapshot,
    state: &StateDir,
    team: &str,
    restart: &str,
) -> Result<(), String> {
    match &meta.config {
        Some(c) if c.digest == snapshot.digest => {}
        Some(c) => return Err(changed_since_start(team, restart, c, snapshot)),
        None => {
            return Err(format!(
                "team `{team}` was started by an older brg; restart the team: {restart}"
            ));
        }
    }
    for name in meta.plugins.keys() {
        if !state.plugin_env_file(name).exists() {
            return Err(format!(
                "plugin `{name}`'s values from the team's start are missing; restart the team: {restart}"
            ));
        }
    }
    Ok(())
}

// --- the team's message storage ---

/// The run a start uses, with its lifecycle lock held.
pub struct Binding {
    pub paths: RunPaths,
    pub run: String,
    pub lock: File,
    /// Something to tell the person (what a format change left behind).
    pub note: Option<String>,
}

/// Why `up` started a new run (recorded in `linked_from.reason`).
pub const ROLL_CONFIG: &str = "config";
pub const ROLL_FORMAT: &str = "format";
pub const ROLL_AGE: &str = "age";

/// Carry unfinished work (open tasks, unanswered questions, unread
/// messages) from `old` into the new run `new`, before `new` becomes
/// current. Called with `old`'s `bus/lock` held and its `retired` marker
/// written. It runs again when a later `up` finishes a rollover that was
/// interrupted, so it must be idempotent. Task facts own this and wire it
/// in a later change; until then a configuration or age rollover leaves
/// unfinished work behind.
pub fn carry_into(new: &RunPaths, old: &RunPaths) -> Result<(), String> {
    let _ = (new, old);
    Ok(())
}

fn aged(created_at: &str, max_age_days: u32) -> bool {
    parse_utc(created_at)
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age >= Duration::from_secs(u64::from(max_age_days) * 86_400))
}

/// Records in a run, for saying what a format change left behind.
fn record_count(paths: &RunPaths) -> usize {
    fs::read_dir(paths.records_dir())
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| !e.file_name().to_string_lossy().starts_with(fsx::TMP_PREFIX))
                .count()
        })
        .unwrap_or(0)
}

/// Check that every current incarnation recorded in `paths` (the old
/// storage, whatever the new configuration's members are) is ended or
/// `gone`. Anything that can't be read or listed refuses: the message names
/// the member when it can.
fn check_stopped(paths: &RunPaths, team: &str, restart: &str) -> Result<(), String> {
    let refuse = |who: &str| {
        Err(format!(
            "brg can't tell that {who} has stopped, so the team can't start with fresh messages; stop the team and start it again: {restart}"
        ))
    };
    let dir = paths.runtime_dir().join("incarnations");
    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return refuse(&format!("every member of {team}")),
    };
    for entry in entries {
        let Ok(entry) = entry else {
            return refuse(&format!("every member of {team}"));
        };
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(fsx::TMP_PREFIX) {
            continue;
        }
        // `<member>.json` is the current incarnation; `<member>/` its history.
        let Some(member) = name.strip_suffix(".json") else {
            continue;
        };
        let who = format!("{team}/{member}");
        let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
        if !is_file {
            return refuse(&who);
        }
        match manifest::load_incarnation(paths, member) {
            Ok(Some(inc)) if inc.ended_at.is_some() => {}
            Ok(Some(inc)) if liveness(paths, member, &inc.incarnation) == Liveness::Gone => {}
            Ok(None) => {}
            _ => return refuse(&who),
        }
    }
    Ok(())
}

/// Where an interrupted rollover of `old` was going: its `retired` marker
/// names a run that exists, belongs to this deployment and links back to
/// `old`. Returns that run and the rollover's reason.
fn successor(store: &TeamStore, old: &str, deployment: &str) -> Option<(String, String)> {
    let next = fs::read_to_string(store.run(old).retired()).ok()?;
    let next = next.trim();
    if !store.runs().ok()?.iter().any(|r| r == next) {
        return None;
    }
    let m = manifest::load_manifest(&store.run(next)).ok()??;
    let link = m.linked_from?;
    (m.deployment == deployment && link.run == old).then(|| (next.to_string(), link.reason))
}

enum Plan {
    /// Use this run as it is.
    Reuse { id: String },
    /// Start a new run, linked to the previous one if there was one.
    New {
        previous: Option<(String, &'static str)>,
    },
}

/// Pick (and on a full `up` of a stopped team, roll over) the run this
/// start uses. Every check is made before anything is written.
///
/// - `joining`: the team is running; use its run as it is.
/// - `child`: `up --member` of a stopped team; it never rolls over, so it
///   refuses when the configuration or message format changed.
/// - otherwise a full `up` of a stopped team: a new run, linked to the
///   previous one, when the configuration or message format changed or the
///   run reached `max_age_days`, but only once every member is positively
///   stopped. A format change carries nothing.
pub fn bind_run(
    v: &Validated,
    bus: &BusConfig,
    state: &StateDir,
    source: &Path,
    joining: bool,
    child: bool,
) -> Result<Binding, String> {
    let team = state::team_name(source);
    let restart = state::restart_hint(source);
    let store = TeamStore::new(bus, &state.id);
    let unreadable = || format!("team `{team}`'s messages can't be read; nothing was started");
    let current = store.current().map_err(|_| unreadable())?;
    let snapshot = v.config_snapshot();
    let missing = || {
        format!(
            "team `{team}` was started before but its messages are missing; nothing was started"
        )
    };
    // Never start into retired storage. A full `up` finishes a rollover
    // that was interrupted after retiring the old run (switching `current`
    // to the run the marker names, carrying again) when that run is clearly
    // its successor; anything else refuses.
    let interrupted =
        "an earlier start of team `{team}` was interrupted while moving to fresh messages";
    let interrupted = interrupted.replace("{team}", &team);
    let mut finish: Option<(String, String, String)> = None;
    let current = match current {
        Some(id) => match store.run(&id).retired().try_exists() {
            Ok(false) => Some(id),
            Ok(true) if joining || child => {
                return Err(format!(
                    "{interrupted}; start the whole team to finish it: {restart}"
                ));
            }
            Ok(true) => match successor(&store, &id, &state.id) {
                Some((next, _)) if store.run(&next).retired().try_exists().unwrap_or(true) => {
                    return Err(format!(
                        "{interrupted}, and brg can't tell how far it got; nothing was started"
                    ));
                }
                Some((next, reason)) => {
                    finish = Some((id, next.clone(), reason));
                    Some(next)
                }
                None => {
                    return Err(format!(
                        "{interrupted}, and brg can't tell how far it got; nothing was started"
                    ));
                }
            },
            Err(_) => return Err(unreadable()),
        },
        None => None,
    };
    let plan = match current {
        None if joining => return Err(missing()),
        // Runs without `current`: storage was damaged, not a first start.
        None if !store.runs().map_err(|_| unreadable())?.is_empty() => return Err(missing()),
        None => Plan::New { previous: None },
        Some(id) => match manifest::load_manifest(&store.run(&id)) {
            Ok(Some(m)) if m.deployment != state.id => {
                return Err(format!(
                    "team `{team}`'s messages belong to another team; nothing was started"
                ));
            }
            Ok(Some(m)) if m.config.digest != snapshot.digest => {
                if joining || child {
                    return Err(changed_since_start(&team, &restart, &m.config, &snapshot));
                }
                Plan::New {
                    previous: Some((id, ROLL_CONFIG)),
                }
            }
            Ok(Some(m)) if !joining && !child && aged(&m.created_at, bus.max_age_days) => {
                Plan::New {
                    previous: Some((id, ROLL_AGE)),
                }
            }
            Ok(Some(_)) => Plan::Reuse { id },
            // `current` names storage without its manifest: damaged.
            Ok(None) => return Err(missing()),
            Err(ManifestError::Format { found }) if joining || child => {
                return Err(manifest::format_mismatch(&team, found, &restart));
            }
            Err(ManifestError::Format { .. }) => Plan::New {
                previous: Some((id, ROLL_FORMAT)),
            },
            Err(ManifestError::Unreadable(_)) => return Err(unreadable()),
        },
    };
    // A rollover needs every member of the old storage positively stopped.
    if let Plan::New {
        previous: Some((prev, _)),
    } = &plan
    {
        check_stopped(&store.run(prev), &team, &restart)?;
    }

    let storage_error = |e: io::Error| {
        format!("cannot use team `{team}`'s message storage ({e}); nothing was started")
    };
    let team_error = |e: String| format!("cannot use team `{team}`'s message storage ({e})");
    if let Some((old, next, reason)) = &finish {
        let old = store.run(old);
        let _old_bus = lock_bus(&old).map_err(team_error)?;
        if reason != ROLL_FORMAT {
            carry_into(&store.run(next), &old).map_err(team_error)?;
        }
        store.set_current(next).map_err(storage_error)?;
    }
    match plan {
        Plan::Reuse { id } => {
            let paths = store.ensure_run(&id).map_err(storage_error)?;
            let lock = lock_run(&paths).map_err(team_error)?;
            Ok(Binding {
                paths,
                run: id,
                lock,
                note: None,
            })
        }
        Plan::New { previous } => {
            let id = store.next_id().map_err(storage_error)?;
            let paths = store.ensure_run(&id).map_err(storage_error)?;
            let lock = lock_run(&paths).map_err(team_error)?;
            let mut m = manifest::manifest_for(v, &id, &state.id, source.to_path_buf(), utc_now());
            m.linked_from = previous.as_ref().map(|(prev, reason)| LinkedRun {
                run: prev.clone(),
                root: store.dir.clone(),
                reason: (*reason).to_string(),
            });
            write_json_new(&paths.manifest(), &m).map_err(team_error)?;
            let mut note = None;
            match &previous {
                Some((prev, reason)) => {
                    // Writers re-check `retired` under `bus/lock`, so nothing
                    // published to the old run while this runs is stranded.
                    let old = store.run(prev);
                    let _old_bus = lock_bus(&old).map_err(team_error)?;
                    fsx::write_new(&old.retired(), format!("{id}\n").as_bytes())
                        .map_err(storage_error)?;
                    if *reason == ROLL_FORMAT {
                        // This brg can't read the old format, so it can't
                        // carry from it; the old messages stay where they are.
                        let n = record_count(&old);
                        note = Some(format!(
                            "team `{team}`'s messages were in another format, so it starts with fresh ones; {n} message{} in the old format {} left on disk, and open work in it wasn't carried over",
                            if n == 1 { "" } else { "s" },
                            if n == 1 { "was" } else { "were" }
                        ));
                    } else {
                        carry_into(&paths, &old).map_err(team_error)?;
                    }
                    store.set_current(&id).map_err(storage_error)?;
                }
                None => store.set_current(&id).map_err(storage_error)?,
            }
            Ok(Binding {
                paths,
                run: id,
                lock,
                note,
            })
        }
    }
}

fn write_json_new<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    match fsx::write_new(path, &body) {
        Ok(Written::Created) => Ok(()),
        Ok(Written::Exists) => {
            Err("a record brg was writing already exists (another start got there first?)".into())
        }
        Err(e) => Err(format!("cannot write the team's records: {e}")),
    }
}

fn write_json_replace<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    fsx::replace(path, &body).map_err(|e| format!("cannot write the team's records: {e}"))
}

// --- incarnations ---

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A new incarnation ID for `member`, unique in the run:
/// `<member>.<utc yyyymmddhhmmssmmm>.<4 hex>`.
pub fn new_incarnation_id(paths: &RunPaths, member: &str) -> String {
    let current = manifest::load_incarnation(paths, member)
        .ok()
        .flatten()
        .map(|i| i.incarnation);
    loop {
        let stamp: String = utc_now().chars().filter(char::is_ascii_digit).collect();
        let seed = format!(
            "{}-{}-{:?}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
        );
        let tag = &hex(&Sha256::digest(seed.as_bytes()))[..4];
        let id = format!("{member}.{stamp}.{tag}");
        if current.as_deref() != Some(id.as_str())
            && !paths.incarnation_history(member, &id).exists()
        {
            return id;
        }
    }
}

/// Hold `bus/lock` (exclusive, briefly) while an incarnation changes, so a
/// bus mutation checking a member's current incarnation under that lock
/// sees either the old record or the new one. Lock order: lifecycle →
/// bus.
fn with_bus_lock<T>(paths: &RunPaths, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let _lock = lock_bus(paths)?;
    f()
}

/// Record a member's new incarnation as current, before its window starts.
pub fn begin_incarnation(paths: &RunPaths, inc: &Incarnation) -> Result<(), String> {
    with_bus_lock(paths, || {
        write_json_replace(&paths.current_incarnation(&inc.member), inc)
    })
}

/// Record the pane an incarnation runs in, and add it to the history.
pub fn complete_incarnation(paths: &RunPaths, inc: &Incarnation) -> Result<(), String> {
    with_bus_lock(paths, || {
        write_json_replace(&paths.current_incarnation(&inc.member), inc)?;
        let body = serde_json::to_vec_pretty(inc).map_err(|e| e.to_string())?;
        let path = paths.incarnation_history(&inc.member, &inc.incarnation);
        fsx::write_new(&path, &body)
            .map(|_| ())
            .map_err(|e| format!("cannot write the team's records: {e}"))
    })
}

/// Mark a member's current incarnation ended, if it hasn't already. Call
/// this only once its pane is known to have stopped.
pub fn end_incarnation(paths: &RunPaths, member: &str, reason: &str) -> Result<bool, String> {
    with_bus_lock(paths, || {
        let Some(mut inc) = manifest::load_incarnation(paths, member)
            .map_err(|_| format!("{member}'s start record can't be read"))?
        else {
            return Ok(false);
        };
        if inc.ended_at.is_some() {
            return Ok(false);
        }
        inc.ended_at = Some(utc_now());
        inc.ended_reason = Some(reason.to_string());
        write_json_replace(&paths.current_incarnation(member), &inc)?;
        Ok(true)
    })
}

/// Where a tmux server is: a socket name (`-L`, resolved by tmux from the
/// caller's environment) or, once recorded, its absolute path (`-S`), which
/// the caller's `TMUX_TMPDIR` or `BORGCHART_TMUX_SOCKET` can't redirect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Server {
    Name(String),
    Path(PathBuf),
}

impl Server {
    /// A recorded socket: an absolute path, or (in older records) a name.
    pub fn from_recorded(socket: &str) -> Self {
        if socket.starts_with('/') {
            Server::Path(PathBuf::from(socket))
        } else {
            Server::Name(socket.to_string())
        }
    }

    /// The deployment's server, as recorded at `up`.
    pub fn of(meta: &Meta) -> Self {
        match (&meta.socket_path, &meta.socket) {
            (Some(path), _) => Server::Path(path.clone()),
            (None, Some(name)) => Server::Name(name.clone()),
            (None, None) => Server::Name(state::tmux_socket()),
        }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new("tmux");
        // Only what's needed to find tmux, and brg's own socket directory.
        cmd.env_clear();
        if let Some(path) = std::env::var_os("PATH") {
            cmd.env("PATH", path);
        }
        if let Some(dir) = std::env::var_os(state::ENV_TMUX_TMPDIR) {
            cmd.env("TMUX_TMPDIR", dir);
        }
        match self {
            Server::Name(name) => cmd.arg("-L").arg(name),
            Server::Path(path) => cmd.arg("-S").arg(path),
        };
        cmd.stdin(Stdio::null());
        cmd
    }

    fn describe(&self) -> String {
        match self {
            Server::Name(n) => format!("tmux socket `{n}`"),
            Server::Path(p) => format!("tmux server {}", p.display()),
        }
    }

    /// Every pane on the server. `Err` means the server couldn't be asked
    /// (not running, not reachable from here, or tmux missing); that is not
    /// evidence that anything stopped.
    pub fn panes(&self) -> Result<Vec<PaneRow>, String> {
        let out = self
            .command()
            .args([
                "list-panes",
                "-a",
                "-F",
                // `|` rather than a tab: without a UTF-8 locale tmux
                // replaces tabs in its output. No field can contain `|`.
                "#{session_name}|#{window_name}|#{pane_id}|#{pane_pid}|#{pane_dead}",
            ])
            .output()
            .map_err(|e| format!("cannot start tmux: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "cannot reach the {}: {}",
                self.describe(),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| {
                let mut p = line.split('|');
                Some(PaneRow {
                    session: p.next()?.to_string(),
                    window: p.next()?.to_string(),
                    pane_id: p.next()?.to_string(),
                    pane_pid: p.next()?.parse().ok()?,
                    dead: p.next()? == "1",
                })
            })
            .collect())
    }

    /// Run one tmux command that changes something; `Err` on any failure.
    pub fn act(&self, args: &[&str]) -> Result<(), String> {
        let out = self
            .command()
            .args(args)
            .output()
            .map_err(|e| format!("cannot start tmux: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "tmux {}: {}",
                args.first().unwrap_or(&""),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }
}

/// One pane, as `list-panes -a` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneRow {
    pub session: String,
    pub window: String,
    pub pane_id: String,
    pub pane_pid: u32,
    pub dead: bool,
}

/// The pane a window runs in, read from brg's own tmux right after it
/// starts, with the server's absolute socket path.
pub fn pane_identity(
    tmux: &crate::tmux::Tmux,
    session: &str,
    window: &str,
) -> Option<PaneIdentity> {
    let out = tmux
        .ok(&[
            "list-panes",
            "-t",
            &crate::tmux::target(session, window),
            "-F",
            "#{socket_path}\t#{pane_id}\t#{pane_pid}",
        ])
        .ok()?;
    let line = out.lines().next()?;
    let mut parts = line.split('\t');
    let (socket, id, pid) = (parts.next()?, parts.next()?, parts.next()?);
    Some(PaneIdentity {
        socket: socket.to_string(),
        session: session.to_string(),
        window: window.to_string(),
        pane_id: id.to_string(),
        pane_pid: pid.parse().ok()?,
    })
}

/// What a successful listing says about one recorded pane.
fn pane_in(rows: &[PaneRow], pane_id: &str, pane_pid: u32) -> Liveness {
    match rows.iter().find(|r| r.pane_id == pane_id) {
        Some(r) if r.pane_pid == pane_pid && !r.dead => Liveness::Alive,
        // Absent, dead, or another process in a reused pane ID.
        _ => Liveness::Gone,
    }
}

/// Whether the pane recorded for an incarnation still runs it. Only a
/// server that answers can show it gone; one that can't be asked is
/// `Unknown`, however it fails.
pub fn pane_liveness(pane: &PaneIdentity) -> Liveness {
    match Server::from_recorded(&pane.socket).panes() {
        Ok(rows) => pane_in(&rows, &pane.pane_id, pane.pane_pid),
        Err(_) => Liveness::Unknown,
    }
}

/// Whether `member`'s incarnation `incarnation` may still be running.
pub fn liveness(paths: &RunPaths, member: &str, incarnation: &str) -> Liveness {
    let inc = match manifest::load_incarnation(paths, member) {
        Ok(Some(i)) if i.incarnation == incarnation => Some(i),
        Ok(_) => match fs::read(paths.incarnation_history(member, incarnation)) {
            Ok(bytes) => match serde_json::from_slice::<Incarnation>(&bytes) {
                Ok(i) => Some(i),
                Err(_) => return Liveness::Unknown,
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(_) => return Liveness::Unknown,
        },
        Err(_) => return Liveness::Unknown,
    };
    let Some(inc) = inc else {
        return Liveness::Unknown;
    };
    if inc.ended_at.is_some() {
        return Liveness::Gone;
    }
    match &inc.pane {
        Some(pane) => pane_liveness(pane),
        None => Liveness::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn incarnation_ids_are_well_formed_and_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let paths = RunPaths::from_root(dir.path());
        let a = new_incarnation_id(&paths, "helper");
        let b = new_incarnation_id(&paths, "helper");
        assert_ne!(a, b);
        let parts: Vec<&str> = a.split('.').collect();
        assert_eq!(parts.len(), 3, "{a}");
        assert_eq!(parts[0], "helper");
        assert_eq!(parts[1].len(), 17, "{a}");
        assert!(
            a.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c)),
            "{a}"
        );
    }

    fn snapshot(inputs: &[(&str, &str)]) -> ConfigSnapshot {
        ConfigSnapshot {
            version: manifest::CONFIG_DIGEST_VERSION,
            digest: format!("{inputs:?}"),
            inputs: inputs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    #[test]
    fn changes_are_named_in_team_and_member_terms() {
        let was = snapshot(&[
            ("deployment", "d"),
            ("team.yaml", "t"),
            ("members/helper/prompt", "hp"),
            ("members/helper/instruction", "hi"),
            ("members/lead/prompt", "lp"),
            ("members/lead/instruction", "li"),
        ]);
        let helper = snapshot(&[
            ("deployment", "d"),
            ("team.yaml", "t"),
            ("members/helper/prompt", "hp2"),
            ("members/helper/instruction", "hi2"),
            ("members/lead/prompt", "lp"),
            ("members/lead/instruction", "li"),
        ]);
        assert_eq!(
            changed_since_start("oncall", "brg down && brg up", &was, &helper),
            "oncall/helper's instruction file changed since the team started; restart the team to apply it: brg down && brg up"
        );
        let many = snapshot(&[
            ("deployment", "d2"),
            ("team.yaml", "t2"),
            ("members/helper/prompt", "hp"),
            ("members/helper/instruction", "hi"),
            ("members/lead/prompt", "lp"),
            ("members/lead/instruction", "li2"),
            ("members/scout/prompt", "sp"),
            ("members/scout/instruction", "si"),
        ]);
        let msg = changed_since_start("oncall", "brg down && brg up", &was, &many);
        assert!(
            msg.starts_with(
                "oncall's deployment file (borgchart.yaml), oncall's team file (team.yaml), oncall/lead's instruction file and oncall/scout's instruction file changed since the team started"
            ),
            "{msg}"
        );
        assert!(!crate::bus::mentions_runs(&msg), "{msg}");
    }

    #[test]
    fn age_counts_whole_days() {
        assert!(aged("2000-01-01T00:00:00Z", 14));
        assert!(!aged(&utc_now(), 14));
        assert!(!aged("not a time", 14));
    }

    #[test]
    fn liveness_without_a_pane_or_after_ending() {
        let dir = tempfile::tempdir().unwrap();
        let paths = RunPaths::from_root(dir.path());
        let mut inc = Incarnation {
            member: "lead".into(),
            incarnation: "lead.1.abcd".into(),
            started_at: utc_now(),
            vendor_version: None,
            pane: None,
            ended_at: None,
            ended_reason: None,
        };
        begin_incarnation(&paths, &inc).unwrap();
        assert_eq!(liveness(&paths, "lead", "lead.1.abcd"), Liveness::Unknown);
        assert_eq!(liveness(&paths, "lead", "lead.other"), Liveness::Unknown);
        inc.pane = Some(PaneIdentity {
            socket: format!("/nonexistent/brg-no-such-socket-{}", std::process::id()),
            session: "s".into(),
            window: "lead".into(),
            pane_id: "%0".into(),
            pane_pid: 1,
        });
        complete_incarnation(&paths, &inc).unwrap();
        // A server that can't be reached proves nothing.
        assert_eq!(liveness(&paths, "lead", "lead.1.abcd"), Liveness::Unknown);
        assert!(end_incarnation(&paths, "lead", ENDED_STOPPED).unwrap());
        assert!(!end_incarnation(&paths, "lead", ENDED_STOPPED).unwrap());
        assert_eq!(liveness(&paths, "lead", "lead.1.abcd"), Liveness::Gone);
        let stored = manifest::load_incarnation(&paths, "lead").unwrap().unwrap();
        assert_eq!(stored.ended_reason.as_deref(), Some(ENDED_STOPPED));
    }

    #[test]
    fn only_a_listing_shows_a_pane_gone() {
        let row = |id: &str, pid: u32, dead: bool| PaneRow {
            session: "brg-x".into(),
            window: "lead".into(),
            pane_id: id.into(),
            pane_pid: pid,
            dead,
        };
        assert_eq!(pane_in(&[row("%1", 10, false)], "%1", 10), Liveness::Alive);
        assert_eq!(pane_in(&[row("%1", 10, true)], "%1", 10), Liveness::Gone);
        assert_eq!(pane_in(&[row("%1", 11, false)], "%1", 10), Liveness::Gone);
        assert_eq!(pane_in(&[row("%2", 10, false)], "%1", 10), Liveness::Gone);
        assert_eq!(pane_in(&[], "%1", 10), Liveness::Gone);
        assert_eq!(
            Server::from_recorded("/tmp/x/sock"),
            Server::Path("/tmp/x/sock".into())
        );
        assert_eq!(Server::from_recorded("name"), Server::Name("name".into()));
    }
}
