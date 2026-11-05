//! Safe lifecycle and runtime binding: the lifecycle locks, the checks a
//! start makes before it changes anything, run identity (`run.json`),
//! runtime generations and their pinned binaries, member incarnations and
//! their liveness, and the `brg run show|rebind` commands.
//!
//! A run's identity (ID, schema, frozen topology, configuration digest) never
//! changes. Which binary, state directory and tmux socket serve it is a
//! *generation*; moving to another is an explicit rebind while stopped.

use std::fs::{self, File, TryLockError};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::bus::fsx::{self, TMP_PREFIX, Written};
use crate::bus::layout::{self, RunPaths};
use crate::bus::manifest::{
    self, ConfigSnapshot, Generation, Incarnation, LinkedRun, Liveness, PaneIdentity, RunManifest,
    hex,
};
use crate::bus::outcome::{Code, Failure, ok_json};
use crate::bus::time::utc_now;
use crate::deploy::BusConfig;
use crate::runtime::state_for;
use crate::state::{self, Meta, StateDir, shell_quote};
use crate::validate::{Input, Validated, validate};

/// `ended_reason` for an incarnation stopped by `brg down`.
pub const ENDED_STOPPED: &str = "stopped";
/// `ended_reason` for an incarnation whose window could not be started.
pub const ENDED_START_FAILED: &str = "start-failed";

pub fn sha256_file(path: &Path) -> io::Result<String> {
    Ok(hex(&Sha256::digest(fs::read(path)?)))
}

/// The binary running this command.
pub fn current_brg() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("brg"))
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

// --- locks ---

/// Take an exclusive lock on `path` (created if needed), waiting for it.
fn lock_exclusive(path: &Path) -> Result<File, String> {
    let file =
        state::open_lock(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    file.lock()
        .map_err(|e| format!("cannot lock {}: {e}", path.display()))?;
    Ok(file)
}

/// The deployment's lifecycle lock in brg's state directory, held by `up`,
/// `down` and `run rebind` while they change anything.
pub fn lock_state(state: &StateDir) -> Result<File, String> {
    lock_exclusive(&state.lifecycle_lock())
}

/// The run's `runtime/lifecycle.lock`, taken after [`lock_state`].
pub fn lock_run(paths: &RunPaths) -> Result<File, String> {
    lock_exclusive(&paths.lifecycle_lock())
}

// --- child starts into a running deployment ---

/// Refuse to start members into a running deployment from a different
/// binary or a changed configuration. Nothing has been written when this
/// fails.
pub fn check_join(
    meta: &Meta,
    caller: &Path,
    caller_sha: &str,
    snapshot: &ConfigSnapshot,
    state: &StateDir,
    retry: &str,
) -> Result<(), String> {
    let Some(sha) = &meta.brg_sha256 else {
        return Err(
            "this deployment was started by an older brg that didn't record its binary; stop it (`brg down`) and start it again"
                .into(),
        );
    };
    if sha != caller_sha {
        return Err(format!(
            "this brg ({}, sha256 {}) is not the binary the running deployment started with (sha256 {}); nothing was changed. Use that one: {} {retry}",
            caller.display(),
            short(caller_sha),
            short(sha),
            shell_quote(&meta.brg.display().to_string()),
        ));
    }
    match &meta.config {
        Some(c) if c.digest == snapshot.digest => {}
        Some(c) => {
            let mut changed: Vec<&str> = snapshot
                .inputs
                .iter()
                .filter(|(k, v)| c.inputs.get(*k) != Some(*v))
                .map(|(k, _)| k.as_str())
                .collect();
            changed.extend(
                c.inputs
                    .keys()
                    .filter(|k| !snapshot.inputs.contains_key(*k))
                    .map(String::as_str),
            );
            return Err(format!(
                "the configuration changed since the deployment started ({}); nothing was changed. Undo the change, or stop the team (`brg down`) and start it again{}",
                changed.join(", "),
                if meta.run.is_some() {
                    " in a new run (`run:` in borgchart.yaml)"
                } else {
                    ""
                }
            ));
        }
        None => {
            return Err(
                "this deployment was started by an older brg that didn't record its configuration; stop it (`brg down`) and start it again"
                    .into(),
            );
        }
    }
    for name in meta.plugins.keys() {
        if !state.plugin_env_file(name).exists() {
            return Err(format!(
                "plugin `{name}`'s values from `brg up` are missing; stop the team (`brg down`) and start it again"
            ));
        }
    }
    Ok(())
}

// --- run identity and generations ---

/// The run a start is bound to, with its lifecycle lock held.
pub struct Binding {
    pub paths: RunPaths,
    pub run: String,
    pub generation: Generation,
    pub lock: File,
}

/// Every generation of the run, oldest first.
pub fn generations(paths: &RunPaths) -> Result<Vec<Generation>, String> {
    let dir = paths.generations_dir();
    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read {}: {e}", dir.display())),
    };
    let mut ns = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(TMP_PREFIX) {
            continue;
        }
        let n = name
            .strip_suffix(".json")
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| format!("unexpected file {}", entry.path().display()))?;
        ns.push(n);
    }
    ns.sort();
    let mut out = Vec::new();
    for n in ns {
        let g = manifest::load_generation(paths, n)?
            .ok_or_else(|| format!("generation {n} disappeared"))?;
        if g.n != n {
            return Err(format!("generation file {n} says it is {}", g.n));
        }
        out.push(g);
    }
    Ok(out)
}

/// Copy `binary` (whose bytes must hash to `sha`) to the run's pinned path
/// for that hash, unless it is there already, and check the copy.
pub fn pin_binary(paths: &RunPaths, binary: &Path, sha: &str) -> Result<PathBuf, String> {
    let pinned = paths.pinned_binary(sha);
    let bytes = fs::read(binary).map_err(|e| format!("cannot read {}: {e}", binary.display()))?;
    if hex(&Sha256::digest(&bytes)) != sha {
        return Err(format!(
            "{} changed while it was being pinned; try again",
            binary.display()
        ));
    }
    let written = fsx::write_new(&pinned, &bytes)
        .map_err(|e| format!("cannot write {}: {e}", pinned.display()))?;
    if written == Written::Created {
        fs::set_permissions(&pinned, fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("cannot make {} executable: {e}", pinned.display()))?;
    }
    let got = sha256_file(&pinned).map_err(|e| format!("cannot read {}: {e}", pinned.display()))?;
    if got != sha {
        return Err(format!(
            "the pinned binary {} does not match its hash; the run's runtime directory is damaged",
            pinned.display()
        ));
    }
    Ok(pinned)
}

fn rebind_hint(latest: &Generation) -> String {
    format!(
        "use the run's binary ({}), or stop the team and run `brg run rebind --reason TEXT` with this one",
        shell_quote(&latest.pinned.display().to_string())
    )
}

/// Check (and on a run's first start, create) the run's identity and
/// generation for this start. Every check is made before anything is
/// written; the run's lifecycle lock is held from the first write.
#[allow(clippy::too_many_arguments)]
pub fn bind_run(
    v: &Validated,
    bus: &BusConfig,
    state: &StateDir,
    source: &Path,
    caller: &Path,
    caller_sha: &str,
    joining: bool,
) -> Result<Binding, String> {
    let paths = RunPaths::new(bus);
    // A link to an earlier run must point at a run that exists, checked
    // before the new run's directory is made.
    let linked = match &bus.continues {
        Some(prev) if !paths.manifest().exists() => {
            let prev_paths = RunPaths::from_root(&bus.root.join(prev));
            match manifest::load_manifest(&prev_paths)? {
                Some(_) => Some(LinkedRun {
                    run: prev.clone(),
                    root: bus.root.clone(),
                    reason: "continues".into(),
                }),
                None => {
                    return Err(format!(
                        "bus: `continues: {prev}` names a run with no run.json under {}; nothing was started",
                        bus.root.display()
                    ));
                }
            }
        }
        _ => None,
    };
    let paths = layout::ensure_run_dir(bus)
        .map_err(|e| format!("cannot use the run directory {}: {e}", paths.root.display()))?;
    let lock = lock_run(&paths)?;

    let existing = manifest::load_manifest(&paths)?;
    let snapshot = v.config_snapshot();
    let new_manifest = match &existing {
        Some(m) => {
            if m.deployment != state.id {
                return Err(format!(
                    "run `{}` belongs to deployment `{}`, not this one (`{}`); give this deployment its own `run:`",
                    bus.run, m.deployment, state.id
                ));
            }
            if m.config.digest != snapshot.digest {
                let changed: Vec<&str> = snapshot
                    .inputs
                    .iter()
                    .filter(|(k, val)| m.config.inputs.get(*k) != Some(*val))
                    .map(|(k, _)| k.as_str())
                    .chain(
                        m.config
                            .inputs
                            .keys()
                            .filter(|k| !snapshot.inputs.contains_key(*k))
                            .map(String::as_str),
                    )
                    .collect();
                return Err(format!(
                    "the configuration changed since run `{}` started ({}); a run's configuration is frozen. Undo the change, or start a new run (`run:` in borgchart.yaml, with `continues: {}` to carry on from this one)",
                    bus.run,
                    changed.join(", "),
                    bus.run
                ));
            }
            None
        }
        None if joining => {
            return Err(format!(
                "the deployment is running but run `{}` has no run.json; stop it (`brg down`) and start it again",
                bus.run
            ));
        }
        None => {
            let mut m =
                manifest::manifest_for(v, &bus.run, &state.id, source.to_path_buf(), utc_now());
            m.linked_from = linked;
            Some(m)
        }
    };

    let gens = generations(&paths)?;
    let socket = state::tmux_socket();
    let current = match gens.last() {
        Some(latest) => {
            if latest.binary_sha256 != caller_sha {
                return Err(format!(
                    "rebind needed: run `{}` generation {} runs brg sha256 {}, this brg ({}) is sha256 {}; {}",
                    bus.run,
                    latest.n,
                    short(&latest.binary_sha256),
                    caller.display(),
                    short(caller_sha),
                    rebind_hint(latest)
                ));
            }
            if latest.state_dir != state.root || latest.socket != socket {
                return Err(format!(
                    "rebind needed: run `{}` generation {} uses state directory {} and tmux socket `{}`, this brg uses {} and `{}`; stop the team and run `brg run rebind --reason TEXT`",
                    bus.run,
                    latest.n,
                    latest.state_dir.display(),
                    latest.socket,
                    state.root.display(),
                    socket
                ));
            }
            Some(latest.clone())
        }
        None if joining => {
            return Err(format!(
                "the deployment is running but run `{}` has no generation; stop it (`brg down`) and start it again",
                bus.run
            ));
        }
        None => None,
    };

    // Every check passed: record the run and its first generation.
    if let Some(m) = &new_manifest {
        write_json_new(&paths.manifest(), m)?;
    }
    let generation = match current {
        Some(g) => g,
        None => {
            let pinned = pin_binary(&paths, caller, caller_sha)?;
            let g = Generation {
                n: 1,
                at: utc_now(),
                binary: caller.to_path_buf(),
                binary_sha256: caller_sha.to_string(),
                pinned,
                state_dir: state.root.clone(),
                socket,
                previous: None,
                reason: "start".into(),
            };
            write_json_new(&paths.generation(1), &g)?;
            g
        }
    };
    Ok(Binding {
        paths,
        run: bus.run.clone(),
        generation,
        lock,
    })
}

fn write_json_new<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    match fsx::write_new(path, &body) {
        Ok(Written::Created) => Ok(()),
        Ok(Written::Exists) => Err(format!(
            "{} already exists (another start got there first?)",
            path.display()
        )),
        Err(e) => Err(format!("cannot write {}: {e}", path.display())),
    }
}

fn write_json_replace<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    fsx::replace(path, &body).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

// --- incarnations ---

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A new incarnation ID for `member`, unique in the run:
/// `<member>.<generation>.<utc yyyymmddhhmmssmmm>.<4 hex>`.
pub fn new_incarnation_id(paths: &RunPaths, member: &str, generation: u64) -> String {
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
        let id = format!("{member}.{generation}.{stamp}.{tag}");
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
/// generation → bus.
fn with_bus_lock<T>(paths: &RunPaths, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let _lock = lock_exclusive(&paths.bus_lock())?;
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
            .map_err(|e| format!("cannot write {}: {e}", path.display()))
    })
}

/// Mark a member's current incarnation ended, if it hasn't already. Call
/// this only once its pane is known to have stopped.
pub fn end_incarnation(paths: &RunPaths, member: &str, reason: &str) -> Result<bool, String> {
    with_bus_lock(paths, || {
        let Some(mut inc) = manifest::load_incarnation(paths, member)? else {
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
        // Only what's needed to find tmux; nothing that moves the socket.
        cmd.env_clear();
        if let Some(path) = std::env::var_os("PATH") {
            cmd.env("PATH", path);
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
            .map_err(|e| format!("cannot run tmux: {e}"))?;
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
            .map_err(|e| format!("cannot run tmux: {e}"))?;
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

// --- brg run show / rebind ---

fn bus_for(input: &Input) -> Result<(Validated, BusConfig), Failure> {
    let (report, validated) = validate(input);
    let v = validated.ok_or_else(|| {
        Failure::new(
            Code::Invalid,
            report
                .errors()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        )
    })?;
    let bus = v
        .deployment
        .as_ref()
        .and_then(|d| d.bus.clone())
        .ok_or_else(|| Failure::new(Code::NotConfigured, "this deployment has no `bus:`"))?;
    Ok((v, bus))
}

fn load_run(paths: &RunPaths, run: &str) -> Result<RunManifest, Failure> {
    match manifest::load_manifest(paths) {
        Ok(Some(m)) => Ok(m),
        Ok(None) => Err(Failure::new(
            Code::NotConfigured,
            format!("run `{run}` hasn't started (no run.json); `brg up` starts it"),
        )),
        Err(e) if e.contains("schema") => Err(Failure::new(Code::Refused, e)),
        Err(e) => Err(Failure::new(Code::Corrupt, e)),
    }
}

/// The exact command that stops the run with its own binary.
pub fn stop_command(pinned: &Path, source: &Path) -> String {
    let flag = if source.is_dir() { "--team" } else { "--file" };
    format!(
        "{} {flag} {} down",
        shell_quote(&pinned.display().to_string()),
        shell_quote(&source.display().to_string())
    )
}

/// `brg run show`: read-only.
pub fn show(input: &Input) -> Result<Value, Failure> {
    let (_, bus) = bus_for(input)?;
    let paths = RunPaths::new(&bus);
    let m = load_run(&paths, &bus.run)?;
    let gens = generations(&paths).map_err(|e| Failure::new(Code::Corrupt, e))?;
    let latest = gens.last();
    let mut members = Vec::new();
    for id in m.members.keys() {
        let inc =
            manifest::load_incarnation(&paths, id).map_err(|e| Failure::new(Code::Corrupt, e))?;
        let live = inc.as_ref().map(|i| liveness(&paths, id, &i.incarnation));
        members.push(json!({"member": id, "incarnation": inc, "liveness": live}));
    }
    Ok(ok_json(
        "ok",
        json!({
            "run_dir": paths.root,
            "run": m,
            "generation": latest,
            "generations": gens,
            "pinned": latest.map(|g| g.pinned.clone()),
            "stop": latest.map(|g| stop_command(&g.pinned, &m.source)),
            "members": members,
        }),
    ))
}

/// `brg run show` as text.
pub fn render_show(v: &Value) -> String {
    let s = |v: &Value| v.as_str().unwrap_or("-").to_string();
    let run = &v["run"];
    let mut out = format!(
        "run {}  (schema {}, deployment {}, created {})\n",
        s(&run["run"]),
        run["schema"],
        s(&run["deployment"]),
        s(&run["created_at"])
    );
    out.push_str(&format!("  dir      {}\n", s(&v["run_dir"])));
    out.push_str(&format!("  config   {}\n", s(&run["config"]["digest"])));
    if let Some(l) = run.get("linked_from").filter(|l| !l.is_null()) {
        out.push_str(&format!(
            "  {}  {} (in {})\n",
            s(&l["reason"]),
            s(&l["run"]),
            s(&l["root"])
        ));
    }
    let g = &v["generation"];
    if g.is_null() {
        out.push_str("generation: none yet\n");
    } else {
        out.push_str(&format!(
            "generation {}  ({}, {})\n",
            g["n"],
            s(&g["reason"]),
            s(&g["at"])
        ));
        out.push_str(&format!("  pinned   {}\n", s(&g["pinned"])));
        out.push_str(&format!(
            "  from     {} (sha256 {})\n",
            s(&g["binary"]),
            short(&s(&g["binary_sha256"]))
        ));
        out.push_str(&format!(
            "  state    {}  socket {}\n",
            s(&g["state_dir"]),
            s(&g["socket"])
        ));
        out.push_str(&format!("stop with: {}\n", s(&v["stop"])));
    }
    out.push_str("members:\n");
    for m in v["members"].as_array().into_iter().flatten() {
        let inc = &m["incarnation"];
        let line = if inc.is_null() {
            format!("  {}  never started\n", s(&m["member"]))
        } else {
            let pane = &inc["pane"];
            let pane = if pane.is_null() {
                "no pane".to_string()
            } else {
                format!("pane {} pid {}", s(&pane["pane_id"]), pane["pane_pid"])
            };
            let ended = if inc["ended_at"].is_null() {
                String::new()
            } else {
                format!(
                    ", ended {} ({})",
                    s(&inc["ended_at"]),
                    s(&inc["ended_reason"])
                )
            };
            format!(
                "  {}  {}  {}  {}{}\n",
                s(&m["member"]),
                s(&inc["incarnation"]),
                s(&m["liveness"]),
                pane,
                ended
            )
        };
        out.push_str(&line);
    }
    out
}

/// `brg run rebind --reason TEXT`: move a stopped run to this binary (and
/// this state directory and tmux socket). Rollback is a rebind with the old
/// binary. Nothing is ever deleted.
pub fn rebind(input: &Input, reason: &str) -> Result<Value, Failure> {
    let reason = reason.trim();
    if reason.is_empty() || reason.len() > 200 || reason.chars().any(char::is_control) {
        return Err(Failure::new(
            Code::Usage,
            "--reason must be 1-200 characters of plain text",
        ));
    }
    let (_, bus) = bus_for(input)?;
    let dep = state_for(input);
    let state = &dep.state;
    let paths = RunPaths::new(&bus);
    if !paths.manifest().exists() {
        return Err(Failure::new(
            Code::NotConfigured,
            format!(
                "run `{}` hasn't started (no run.json); `brg up` starts it",
                bus.run
            ),
        ));
    }
    let paths = layout::ensure_run_dir(&bus).map_err(|e| Failure::io("run directory", e))?;
    state::ensure_dir(&state.root).map_err(|e| Failure::io("state directory", e))?;
    let _state_lock = lock_state(state).map_err(|e| Failure::new(Code::Error, e))?;
    let _run_lock = lock_run(&paths).map_err(|e| Failure::new(Code::Error, e))?;

    let m = load_run(&paths, &bus.run)?;
    if m.deployment != state.id {
        return Err(Failure::new(
            Code::Refused,
            format!(
                "run `{}` belongs to deployment `{}`, not this one (`{}`)",
                bus.run, m.deployment, state.id
            ),
        ));
    }
    let gens = generations(&paths).map_err(|e| Failure::new(Code::Corrupt, e))?;
    let Some(latest) = gens.last() else {
        return Err(Failure::new(
            Code::NotConfigured,
            format!(
                "run `{}` has no generation yet; `brg up` records it",
                bus.run
            ),
        ));
    };
    // Only a positively stopped run may move: its server answers without
    // the session, or the last `down` recorded a confirmed stop; and every
    // member's incarnation is gone.
    let socket = state::tmux_socket();
    let stop = stop_command(&latest.pinned, &m.source);
    let ran = StateDir::from_root(&latest.state_dir);
    let meta = state::read_meta(&ran);
    let server = meta
        .as_ref()
        .map(Server::of)
        .unwrap_or_else(|| Server::Name(latest.socket.clone()));
    let running = |rows: &[PaneRow]| rows.iter().any(|r| r.session == ran.session());
    match server.panes() {
        Ok(rows) if running(&rows) => {
            return Err(Failure::new(
                Code::Refused,
                format!(
                    "the deployment is running ({}); stop it first: {stop}",
                    server.describe()
                ),
            ));
        }
        Ok(_) => {}
        Err(_) if meta.as_ref().is_some_and(|m| m.stopped_at.is_some()) => {}
        Err(e) => {
            return Err(Failure::new(
                Code::Refused,
                format!(
                    "can't tell whether the deployment is stopped ({e}); stop it first: {stop}"
                ),
            ));
        }
    }
    if Server::Name(socket.clone()) != server
        && let Ok(rows) = Server::Name(socket.clone()).panes()
        && rows.iter().any(|r| r.session == state.session())
    {
        return Err(Failure::new(
            Code::Refused,
            format!("the deployment is running (tmux socket `{socket}`); stop it first"),
        ));
    }
    for id in m.members.keys() {
        let inc = manifest::load_incarnation(&paths, id).map_err(|e| {
            Failure::new(
                Code::Refused,
                format!("can't tell whether member `{id}` is stopped: {e}"),
            )
        })?;
        let Some(inc) = inc else { continue };
        match liveness(&paths, id, &inc.incarnation) {
            Liveness::Gone => {}
            Liveness::Alive => {
                return Err(Failure::new(
                    Code::Refused,
                    format!(
                        "member `{id}` ({}) is still running; stop it first: {stop}",
                        inc.incarnation
                    ),
                ));
            }
            Liveness::Unknown => {
                return Err(Failure::new(
                    Code::Refused,
                    format!(
                        "can't tell whether member `{id}` ({}) is stopped: its tmux server can't be asked; stop it first: {stop}",
                        inc.incarnation
                    ),
                ));
            }
        }
    }
    let gen_lock = state::open_lock(&paths.generation_lock())
        .map_err(|e| Failure::io("generation lock", e))?;
    match gen_lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            return Err(Failure::new(
                Code::Refused,
                "a bus command or the attention worker is using the run (runtime/generation.lock is held); try again when it is done",
            ));
        }
        Err(TryLockError::Error(e)) => return Err(Failure::io("generation lock", e)),
    }
    let inflight: Vec<String> = match fs::read_dir(paths.inflight_dir()) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(Failure::io("attention/inflight", e)),
    };
    if !inflight.is_empty() {
        return Err(Failure::new(
            Code::Refused,
            format!(
                "wake attempts are in flight ({}); resolve them first (`brg bus attention resolve <member>`)",
                inflight.join(", ")
            ),
        ));
    }

    let caller = current_brg();
    let caller_sha = sha256_file(&caller).map_err(|e| Failure::io("this brg", e))?;
    if latest.binary_sha256 == caller_sha
        && latest.state_dir == state.root
        && latest.socket == socket
    {
        return Ok(ok_json(
            "unchanged",
            json!({
                "message": format!("run `{}` is already bound to this brg (generation {})", bus.run, latest.n),
                "generation": latest,
            }),
        ));
    }
    let pinned =
        pin_binary(&paths, &caller, &caller_sha).map_err(|e| Failure::new(Code::Error, e))?;
    let g = Generation {
        n: latest.n + 1,
        at: utc_now(),
        binary: caller,
        binary_sha256: caller_sha,
        pinned,
        state_dir: state.root.clone(),
        socket,
        previous: Some(latest.n),
        reason: reason.to_string(),
    };
    write_json_new(&paths.generation(g.n), &g).map_err(|e| Failure::new(Code::Conflict, e))?;
    drop(gen_lock);
    Ok(ok_json(
        "rebound",
        json!({
            "message": format!(
                "run `{}`: generation {} (was {}), pinned {}",
                bus.run, g.n, latest.n, g.pinned.display()
            ),
            "generation": g,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incarnation_ids_are_well_formed_and_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let paths = RunPaths::from_root(dir.path());
        let a = new_incarnation_id(&paths, "helper", 3);
        let b = new_incarnation_id(&paths, "helper", 3);
        assert_ne!(a, b);
        assert!(a.starts_with("helper.3."), "{a}");
        assert!(
            a.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c)),
            "{a}"
        );
    }

    #[test]
    fn liveness_without_a_pane_or_after_ending() {
        let dir = tempfile::tempdir().unwrap();
        let paths = RunPaths::from_root(dir.path());
        let mut inc = Incarnation {
            member: "lead".into(),
            incarnation: "lead.1.1.abcd".into(),
            generation: 1,
            started_at: utc_now(),
            vendor_version: None,
            pane: None,
            ended_at: None,
            ended_reason: None,
        };
        begin_incarnation(&paths, &inc).unwrap();
        assert_eq!(liveness(&paths, "lead", "lead.1.1.abcd"), Liveness::Unknown);
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
        assert_eq!(liveness(&paths, "lead", "lead.1.1.abcd"), Liveness::Unknown);
        assert!(end_incarnation(&paths, "lead", ENDED_STOPPED).unwrap());
        assert!(!end_incarnation(&paths, "lead", ENDED_STOPPED).unwrap());
        assert_eq!(liveness(&paths, "lead", "lead.1.1.abcd"), Liveness::Gone);
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

    #[test]
    fn pinning_copies_once_and_checks_the_hash() {
        let dir = tempfile::tempdir().unwrap();
        let paths = RunPaths::from_root(&dir.path().join("run"));
        let bin = dir.path().join("brg");
        fs::write(&bin, b"binary-one").unwrap();
        let sha = sha256_file(&bin).unwrap();
        let pinned = pin_binary(&paths, &bin, &sha).unwrap();
        assert_eq!(fs::read(&pinned).unwrap(), b"binary-one");
        let mode = fs::metadata(&pinned).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        assert_eq!(pin_binary(&paths, &bin, &sha).unwrap(), pinned);
        assert!(pin_binary(&paths, &bin, "00").is_err());
        fs::write(&pinned, b"tampered").unwrap();
        assert!(pin_binary(&paths, &bin, &sha).is_err());
    }
}
