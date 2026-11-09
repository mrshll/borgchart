//! Lifecycle locking, start checks, member incarnations and liveness.
//! Every window, shim and plugin process uses the brg that started it.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

use crate::bus::fsx;
use crate::bus::layout::StorePaths;
use crate::bus::time::utc_now;
use crate::config_snapshot::{ConfigSnapshot, hex};
use crate::state::{self, Meta, StateDir};
use serde::{Deserialize, Serialize};

/// A member's current incarnation: `members/<member>/incarnation.json`.
/// Written when the member starts (before its window exists, with no
/// `pane`), again once its pane is known, and again with `ended_at` when it
/// is stopped through brg.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Incarnation {
    pub member: String,
    /// `[a-z0-9._-]`, unique within the instance.
    pub incarnation: String,
    pub started_at: String,
    /// The vendor CLI version measured at this start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<PaneIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_reason: Option<String>,
}

/// Which tmux pane an incarnation runs in. A pane whose `pane_id` or
/// `pane_pid` differs is a different process, whatever its window is called.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneIdentity {
    pub socket: String,
    pub session: String,
    pub window: String,
    /// tmux's `%N`.
    pub pane_id: String,
    pub pane_pid: u32,
}

/// Whether an incarnation's process may still be running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Liveness {
    /// Its pane exists with the recorded ID and PID, and is not dead.
    Alive,
    /// Its pane is gone or dead, or it ended through brg.
    Gone,
    /// It can't be told (no pane recorded yet, tmux unreachable): treat as
    /// possibly alive. Never free its checkout on this.
    Unknown,
}

/// Read a member's current start record, without creating anything.
pub fn load_incarnation(state: &StateDir, member: &str) -> Result<Option<Incarnation>, String> {
    let path = state.current_incarnation(member);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("{} is unreadable: {e}", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// Changed files apply only once every previously recorded member has stopped.
pub fn check_stopped(state: &StateDir, meta: &Meta, restart: &str) -> Result<(), String> {
    for member in meta.members.keys() {
        match load_incarnation(state, member) {
            Ok(Some(inc))
                if inc.ended_at.is_some()
                    || liveness(state, member, &inc.incarnation) == Liveness::Gone => {}
            Ok(None) => {}
            _ => {
                return Err(format!(
                    "brg can't tell that {}/{member} has stopped, so changed files can't be applied; stop it and start it again: {restart}",
                    state.id
                ));
            }
        }
    }
    Ok(())
}

/// `ended_reason` for an incarnation stopped by `brg stop`.
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

/// The instance's lifecycle lock in its state folder, held by `start`
/// and `stop` while they change anything.
pub fn lock_state(state: &StateDir) -> Result<File, String> {
    let path = state.lifecycle_lock();
    lock_file(&path).map_err(|e| format!("cannot lock {}: {e}", path.display()))
}

/// The publication lock, taken after the instance's lifecycle lock.
pub(crate) fn lock_bus(state: &StateDir) -> Result<File, String> {
    lock_file(&StorePaths::new(state).bus_lock())
        .map_err(|e| format!("cannot lock the team's messages: {e}"))
}

// --- messages ---

/// What changed between two configuration snapshots, in team and member
/// terms: "oncall/helper's instruction file changed since the instance started;
/// stop it fully and start it again: brg stop <instance> && brg start <instance>". `restart` comes from
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
    for (key, what) in [
        ("config/auth", "`auth`"),
        ("config/auto_trust", "`auto_trust`"),
    ] {
        if keys.iter().any(|k| *k == key) {
            things.push(format!("{what} in brg's configuration"));
        }
    }
    for k in &keys {
        if let Some(name) = k.strip_prefix("config/plugins/") {
            things.push(format!("where `plugins_dir` puts plugin `{name}`"));
        }
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
            let thing = format!("{team}/{id}'s role file");
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
    format!(
        "{list} changed since the instance started; stop it fully and start it again to apply it: {restart}"
    )
}

// --- child starts into a running instance ---

/// Refuse to start members into a running instance whose configuration changed
/// since it started (a full start of a running instance too). Nothing has been
/// written when this fails.
pub fn check_join(
    meta: &Meta,
    snapshot: &ConfigSnapshot,
    state: &StateDir,
    team: &str,
    restart: &str,
) -> Result<(), String> {
    if meta.config.digest != snapshot.digest {
        return Err(changed_since_start(team, restart, &meta.config, snapshot));
    }
    for name in meta.plugins.keys() {
        if !state.plugin_env_file(name).exists() {
            return Err(format!(
                "plugin `{name}`'s values from the instance's start are missing; restart it: {restart}"
            ));
        }
    }
    Ok(())
}

// --- the instance's message storage ---

fn write_json_replace<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    fsx::replace(path, &body).map_err(|e| format!("cannot write the team's records: {e}"))
}

// --- open work ---

/// Unacked messages, as lines to show.
#[derive(Debug, Default)]
pub struct OpenWork {
    pub items: Vec<String>,
}

/// The open work in `state`'s messages (none without a bus). `Err` when
/// the messages can't be read: their contents can't be inspected.
pub fn open_work(state: &StateDir) -> Result<OpenWork, String> {
    use crate::bus::outcome::Code;
    use crate::bus::store;
    let bus = match store::open(state) {
        Ok(b) => b,
        Err(f) if f.code == Code::NotConfigured => return Ok(OpenWork::default()),
        Err(f) => return Err(f.message),
    };
    let snap = store::snapshot(&bus).map_err(|f| f.message)?;
    if !snap.healthy() {
        return Err(format!(
            "instance `{}`'s messages are incomplete",
            bus.instance
        ));
    }
    Ok(unacked_work(&bus, &snap, None))
}

/// Open messages for kept recipients, optionally restricted to one member.
/// The snapshot's caller holds the publication lock while deleting a member.
pub(crate) fn unacked_work(
    bus: &crate::bus::store::Bus,
    snap: &crate::bus::store::Snapshot,
    only: Option<&str>,
) -> OpenWork {
    let mut work = OpenWork::default();
    for r in snap.records.values().filter(|r| !snap.is_acked(r.seq)) {
        if only.is_some_and(|member| r.to != member)
            || bus.record.members.get(&r.to).is_some_and(|m| m.deleted)
        {
            continue;
        }
        work.items.push(format!("unread {} to {}", r.id, r.to));
    }
    work
}

// --- incarnations ---

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A new incarnation ID for `member`, unique in the instance:
/// `<member>.<utc yyyymmddhhmmssmmm>.<4 hex>`.
pub fn new_incarnation_id(state: &StateDir, member: &str) -> String {
    let current = load_incarnation(state, member)
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
            && !state.incarnation_history(member, &id).exists()
        {
            return id;
        }
    }
}

/// Hold `bus/lock` (exclusive, briefly) while an incarnation changes, so a
/// bus mutation checking a member's current incarnation under that lock
/// sees either the old record or the new one. Lock order: lifecycle →
/// bus.
fn with_bus_lock<T>(state: &StateDir, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let _lock = lock_bus(state)?;
    f()
}

/// Record a member's new incarnation as current, before its window starts.
pub fn begin_incarnation(state: &StateDir, inc: &Incarnation) -> Result<(), String> {
    with_bus_lock(state, || {
        write_json_replace(&state.current_incarnation(&inc.member), inc)
    })
}

/// Record the pane an incarnation runs in, and add it to the history.
pub fn complete_incarnation(state: &StateDir, inc: &Incarnation) -> Result<(), String> {
    with_bus_lock(state, || {
        write_json_replace(&state.current_incarnation(&inc.member), inc)?;
        let body = serde_json::to_vec_pretty(inc).map_err(|e| e.to_string())?;
        let path = state.incarnation_history(&inc.member, &inc.incarnation);
        fsx::write_new(&path, &body)
            .map(|_| ())
            .map_err(|e| format!("cannot write the team's records: {e}"))
    })
}

/// Mark a member's current incarnation ended, if it hasn't already. Call
/// this only once its pane is known to have stopped.
pub fn end_incarnation(state: &StateDir, member: &str, reason: &str) -> Result<bool, String> {
    with_bus_lock(state, || {
        let Some(mut inc) = load_incarnation(state, member)
            .map_err(|_| format!("{member}'s start record can't be read"))?
        else {
            return Ok(false);
        };
        if inc.ended_at.is_some() {
            return Ok(false);
        }
        inc.ended_at = Some(utc_now());
        inc.ended_reason = Some(reason.to_string());
        write_json_replace(&state.current_incarnation(member), &inc)?;
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
    /// A recorded socket: an absolute path.
    pub fn from_recorded(socket: &str) -> Self {
        Server::Path(PathBuf::from(socket))
    }

    /// The instance's server, as recorded at its start.
    pub fn of(meta: &Meta) -> Self {
        match &meta.socket_path {
            Some(path) => Server::Path(path.clone()),
            // The start never reached a window: only the name is known.
            None => Server::Name(meta.socket.clone()),
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
                "#{session_name}|#{window_name}|#{pane_id}|#{pane_pid}|#{pane_dead}|#{window_id}",
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
                    window_id: p.next()?.to_string(),
                })
            })
            .collect())
    }

    /// An absent or refused recorded socket confirms that the server's
    /// windows are gone. Other connection errors leave that unknown.
    pub fn absent(&self) -> bool {
        let Server::Path(path) = self else {
            return false;
        };
        matches!(
            std::os::unix::net::UnixStream::connect(path).map_err(|e| e.kind()),
            Err(io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused)
        )
    }

    /// The server's absolute socket path and PID, as the server reports
    /// them. Needs a session on the server.
    pub fn identity(&self) -> Result<(PathBuf, u32), String> {
        let out = self
            .command()
            .args(["display-message", "-p", "#{socket_path}|#{pid}"])
            .output()
            .map_err(|e| format!("cannot start tmux: {e}"))?;
        let text = String::from_utf8_lossy(&out.stdout);
        text.trim()
            .rsplit_once('|')
            .filter(|(path, _)| out.status.success() && path.starts_with('/'))
            .and_then(|(path, pid)| Some((PathBuf::from(path), pid.parse().ok()?)))
            .ok_or_else(|| format!("cannot identify the {}", self.describe()))
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
    pub window_id: String,
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

/// Whether the pane recorded for an incarnation still runs it. A server
/// that answers shows it alive or gone. One that can't be asked (exited,
/// hidden, or no tmux here) is `Unknown`, unless the pane's own process has
/// exited: then the pane no longer runs it, as when a listing shows it dead.
pub fn pane_liveness(pane: &PaneIdentity) -> Liveness {
    match Server::from_recorded(&pane.socket).panes() {
        Ok(rows) => pane_in(&rows, &pane.pane_id, pane.pane_pid),
        Err(_) if process_exited(pane.pane_pid) => Liveness::Gone,
        Err(_) => Liveness::Unknown,
    }
}

/// Whether process `pid` has certainly exited: `ps` lists this process but
/// not `pid`. Any process with that ID (even a reused one) is not proof, and
/// neither is a `ps` that can't run or doesn't list this process.
pub(crate) fn process_exited(pid: u32) -> bool {
    let me = std::process::id();
    if pid == 0 || pid == me {
        return false;
    }
    let Ok(out) = Command::new("ps")
        .args(["-o", "pid=", "-p", &format!("{me},{pid}")])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return false;
    };
    let listed: Vec<u32> = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .filter_map(|p| p.parse().ok())
        .collect();
    listed.contains(&me) && !listed.contains(&pid)
}

/// Whether `member`'s incarnation `incarnation` may still be running.
pub fn liveness(state: &StateDir, member: &str, incarnation: &str) -> Liveness {
    let inc = match load_incarnation(state, member) {
        Ok(Some(i)) if i.incarnation == incarnation => Some(i),
        Ok(_) => match fs::read(state.incarnation_history(member, incarnation)) {
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
        let state = StateDir::from_root(dir.path());
        let a = new_incarnation_id(&state, "helper");
        let b = new_incarnation_id(&state, "helper");
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
            version: crate::config_snapshot::CONFIG_DIGEST_VERSION,
            digest: format!("{inputs:?}"),
            inputs: inputs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    #[test]
    fn changes_are_named_in_instance_and_role_terms() {
        let was = snapshot(&[
            ("config/auth", "a"),
            ("team.yaml", "t"),
            ("members/helper/prompt", "hp"),
            ("members/helper/instruction", "hi"),
            ("members/lead/prompt", "lp"),
            ("members/lead/instruction", "li"),
        ]);
        let helper = snapshot(&[
            ("config/auth", "a"),
            ("team.yaml", "t"),
            ("members/helper/prompt", "hp2"),
            ("members/helper/instruction", "hi2"),
            ("members/lead/prompt", "lp"),
            ("members/lead/instruction", "li"),
        ]);
        let restart = "brg stop oncall-2a3b && brg start oncall-2a3b";
        assert_eq!(
            changed_since_start("oncall-2a3b", restart, &was, &helper),
            format!(
                "oncall-2a3b/helper's role file changed since the instance started; stop it fully and start it again to apply it: {restart}"
            )
        );
        let many = snapshot(&[
            ("config/auth", "a2"),
            ("config/plugins/pager", "p"),
            ("team.yaml", "t2"),
            ("members/helper/prompt", "hp"),
            ("members/helper/instruction", "hi"),
            ("members/lead/prompt", "lp"),
            ("members/lead/instruction", "li2"),
        ]);
        let msg = changed_since_start("oncall-2a3b", restart, &was, &many);
        assert!(
            msg.starts_with(
                "`auth` in brg's configuration, where `plugins_dir` puts plugin `pager`, oncall-2a3b's team file (team.yaml) and oncall-2a3b/lead's role file changed since the instance started"
            ),
            "{msg}"
        );
    }

    #[test]
    fn only_a_listing_shows_a_pane_gone() {
        let row = |id: &str, pid: u32, dead: bool| PaneRow {
            session: "brg-x".into(),
            window: "lead".into(),
            pane_id: id.into(),
            pane_pid: pid,
            dead,
            window_id: "@1".into(),
        };
        assert_eq!(pane_in(&[row("%1", 10, false)], "%1", 10), Liveness::Alive);
        assert_eq!(pane_in(&[row("%1", 10, true)], "%1", 10), Liveness::Gone);
        assert_eq!(pane_in(&[row("%1", 11, false)], "%1", 10), Liveness::Gone);
        assert_eq!(pane_in(&[row("%2", 10, false)], "%1", 10), Liveness::Gone);
        assert_eq!(pane_in(&[], "%1", 10), Liveness::Gone);
    }
}
