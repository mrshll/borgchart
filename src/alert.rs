//! Alerts: when a person is needed, brg runs the deployment's declared
//! `bus.alert` plugin action with fixed, payload-free arguments, and keeps
//! one file per alert under the run's `attention/alerts/` until someone
//! resolves it.
//!
//! The action runs as the bus (`BORGCHART_ACTOR=bus`), never as a member: its
//! environment is built from scratch from the deployment's frozen base
//! environment and that plugin's values as written at `up`. Nothing from the
//! caller's environment, a member's identity or another plugin's values
//! reaches it. Running it successfully means the alert was handed off, not
//! that anyone saw it.

use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::bus::outcome::{Code, Failure, ok_json};
use crate::deploy::BusConfig;
use crate::exec::PluginEnv;
use crate::names::is_valid_id;
use crate::paths::{is_executable, resolve_within};
use crate::state::{self, BusMeta, Meta, StateDir};

/// Why a person is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    Blocked,
    Draft,
    HumanRequired,
    Unqualified,
    Overdue,
    Unacked,
    WorkerUnavailable,
    Test,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Blocked => "blocked",
            Reason::Draft => "draft",
            Reason::HumanRequired => "human-required",
            Reason::Unqualified => "unqualified",
            Reason::Overdue => "overdue",
            Reason::Unacked => "unacked",
            Reason::WorkerUnavailable => "worker-unavailable",
            Reason::Test => "test",
        }
    }
}

/// What the action is told: these four values, and nothing from any record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertRequest {
    /// `[a-z0-9][a-z0-9._-]{0,63}`.
    pub id: String,
    /// A member name.
    pub member: String,
    pub reason: Reason,
    /// How many records are waiting.
    pub pending: u32,
}

impl AlertRequest {
    /// Whether every argument is a bounded plain word.
    pub fn check(&self) -> Result<(), String> {
        if !crate::bus::is_valid_run_id(&self.id) {
            return Err(format!(
                "alert ID `{}` must match [a-z0-9][a-z0-9._-]{{0,63}}",
                self.id
            ));
        }
        if !is_valid_id(&self.member) {
            return Err(format!("`{}` is not a valid member name", self.member));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// How long the action may run before its process group is killed.
    pub timeout: Duration,
    /// How much of its combined output (the tail) is kept.
    pub max_output: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            timeout: Duration::from_secs(10),
            max_output: 4096,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlertOutcome {
    /// The action exited zero: the alert was handed off (not seen).
    Invoked,
    /// The action ran and failed (`status` is `None` when killed by a signal).
    Failed { status: Option<i32> },
    /// The action ran past its time limit and was killed.
    TimedOut,
    /// There is no usable action: nothing ran.
    Unavailable(String),
}

impl AlertOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            AlertOutcome::Invoked => "invoked",
            AlertOutcome::Failed { .. } => "failed",
            AlertOutcome::TimedOut => "timed-out",
            AlertOutcome::Unavailable(_) => "unavailable",
        }
    }
}

/// One run (or non-run) of the action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub outcome: AlertOutcome,
    /// The tail of stdout then stderr, at most `Limits::max_output` bytes,
    /// with control characters other than newline and tab replaced
    /// and the plugin's own values masked as `[redacted]`.
    pub output: String,
    pub elapsed_ms: u64,
}

fn unavailable(why: impl Into<String>) -> Invocation {
    Invocation {
        outcome: AlertOutcome::Unavailable(why.into()),
        output: String::new(),
        elapsed_ms: 0,
    }
}

/// Identity variables a values file may not supply or override.
/// Plugin values shorter than this are not masked in output (they would
/// mask ordinary words).
const MIN_REDACT: usize = 4;

const RESERVED: &[&str] = &[
    "BORGCHART_MEMBER",
    "BORGCHART_PARENT",
    "BORGCHART_INCARNATION",
    "BORGCHART_DELIVER_TO",
];

/// The environment the action runs with, or why there is none.
fn action_env(
    state: &StateDir,
    meta: &Meta,
    bus: &BusConfig,
    plugin: &str,
) -> Result<(BTreeMap<String, String>, Vec<String>), String> {
    let text = std::fs::read_to_string(state.plugin_env_file(plugin)).map_err(|e| {
        format!(
            "cannot read plugin `{plugin}` values written at `brg up` ({e}); is the deployment up?"
        )
    })?;
    let values: PluginEnv = serde_json::from_str(&text)
        .map_err(|e| format!("plugin `{plugin}` values are unreadable: {e}"))?;
    // Values long enough to recognize are masked in any output brg keeps.
    let secrets: Vec<String> = values
        .env
        .values()
        .filter(|v| v.len() >= MIN_REDACT)
        .cloned()
        .collect();
    let mut env = meta.base_env.clone();
    env.extend(values.env);
    for name in RESERVED {
        env.remove(*name);
    }
    env.insert("BORGCHART_ACTOR".into(), "bus".into());
    env.insert("BORGCHART_DEPLOYMENT".into(), state.id.clone());
    env.insert("BORGCHART_RUN".into(), bus.run.clone());
    env.insert(state::ENV_FILE.into(), meta.source.display().to_string());
    env.insert("BORGCHART_BIN".into(), meta.brg.display().to_string());
    env.insert(
        "BORGCHART_STATE_DIR".into(),
        state.plugin_dir(plugin).display().to_string(),
    );
    Ok((env, secrets))
}

/// Run the deployment's alert action for `req`. Never panics or fails: every
/// problem is an [`AlertOutcome`].
pub fn invoke(
    state: &StateDir,
    meta: &Meta,
    bus: &BusConfig,
    req: &AlertRequest,
    limits: &Limits,
) -> Invocation {
    let Some(config) = &bus.alert else {
        return unavailable("no `bus.alert` is configured");
    };
    if meta.bus.as_ref() != Some(&BusMeta::of(bus)) {
        return unavailable(
            "the bus root, run or alert action differs from the one recorded at `brg up`",
        );
    }
    if let Err(e) = req.check() {
        return unavailable(e);
    }
    let plugin = &config.plugin;
    let Some(plugin_meta) = meta.plugins.get(plugin) else {
        return unavailable(format!(
            "plugin `{plugin}` was not started with this deployment"
        ));
    };
    let Some(dir) = &plugin_meta.dir else {
        return unavailable(format!(
            "plugin `{plugin}` runs from PATH; an alert action must live in the plugin's directory"
        ));
    };
    let Some(prefix) = plugin_meta.actions.get(&config.action) else {
        return unavailable(format!(
            "plugin `{plugin}` had no action `{}` at `brg up`",
            config.action
        ));
    };
    let Some((command, words)) = prefix.split_first() else {
        return unavailable(format!("action `{}` has no command", config.action));
    };
    let program = match resolve_within(dir, command) {
        Ok(p) if is_executable(&p) => p,
        Ok(p) => return unavailable(format!("{} is not executable", p.display())),
        Err(e) => return unavailable(format!("plugin `{plugin}`: {e}")),
    };
    let (env, secrets) = match action_env(state, meta, bus, plugin) {
        Ok(found) => found,
        Err(e) => return unavailable(e),
    };

    let mut cmd = Command::new(&program);
    cmd.args(words)
        .arg(&req.id)
        .arg(&req.member)
        .arg(req.reason.as_str())
        .arg(req.pending.to_string())
        .env_clear()
        .envs(&env)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    run_bounded(cmd, &program, limits, &secrets)
}

/// Keeps the last `cap` bytes written to it.
#[derive(Default)]
struct Tail {
    bytes: Vec<u8>,
}

impl Tail {
    fn push(&mut self, chunk: &[u8], cap: usize) {
        self.bytes.extend_from_slice(chunk);
        if self.bytes.len() > cap {
            let cut = self.bytes.len() - cap;
            self.bytes.drain(..cut);
        }
    }
}

fn drain(mut reader: impl Read + Send + 'static, cap: usize) -> Arc<Mutex<Tail>> {
    let tail = Arc::new(Mutex::new(Tail::default()));
    let sink = Arc::clone(&tail);
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            if let Ok(mut t) = sink.lock() {
                t.push(&buf[..n], cap);
            }
        }
    });
    tail
}

fn run_bounded(
    mut cmd: Command,
    program: &std::path::Path,
    limits: &Limits,
    secrets: &[String],
) -> Invocation {
    let start = Instant::now();
    // A program that was just written can be briefly "text file busy" while
    // another thread's fork still holds its write descriptor: retry a little.
    let mut tries = 0;
    let mut child = loop {
        match cmd.spawn() {
            Ok(c) => break c,
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 10 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return unavailable(format!("cannot run {}: {e}", program.display())),
        }
    };
    let cap = limits.max_output;
    // Each stream keeps `cap` plus the longest value's length. A value whose
    // start a stream's tail dropped leaves a fragment shorter than that in
    // the stream's first `keep - cap` bytes, and the final cut to `cap`
    // always removes at least that much from the front of a truncated
    // stream (stdout + stderr, keeping the end). A value crossing the final
    // cut is whole in the buffer, so it is masked before the cut.
    let keep = cap + secrets.iter().map(String::len).max().unwrap_or(0);
    let out = child.stdout.take().map(|r| drain(r, keep));
    let err = child.stderr.take().map(|r| drain(r, keep));
    let pgid = rustix::process::Pid::from_raw(child.id() as i32);
    let outcome = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                break if status.success() {
                    AlertOutcome::Invoked
                } else {
                    AlertOutcome::Failed {
                        status: status.code(),
                    }
                };
            }
            Ok(None) if start.elapsed() >= limits.timeout => {
                if let Some(pgid) = pgid {
                    let _ =
                        rustix::process::kill_process_group(pgid, rustix::process::Signal::KILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                break AlertOutcome::TimedOut;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break AlertOutcome::Failed { status: None };
            }
        }
    };
    // Kill anything the action left behind in its group, so it can't hold
    // the pipes open, then give the readers a moment to finish.
    if let Some(pgid) = pgid {
        let _ = rustix::process::kill_process_group(pgid, rustix::process::Signal::KILL);
    }
    let settle = Instant::now() + Duration::from_secs(1);
    let mut bytes = Vec::new();
    for tail in [out, err].into_iter().flatten() {
        while Arc::strong_count(&tail) > 1 && Instant::now() < settle {
            std::thread::sleep(Duration::from_millis(5));
        }
        if let Ok(t) = tail.lock() {
            bytes.extend_from_slice(&t.bytes);
        }
    }
    if bytes.len() > keep {
        bytes.drain(..bytes.len() - keep);
    }
    Invocation {
        outcome,
        output: clean_output(&bytes, cap, secrets),
        elapsed_ms: start.elapsed().as_millis() as u64,
    }
}

/// Lossy UTF-8 with `secrets` masked and control characters (other than
/// newline and tab) replaced, trimmed to at most `cap` bytes on a character boundary.
fn clean_output(bytes: &[u8], cap: usize, secrets: &[String]) -> String {
    // Cut on the raw bytes (masking can shorten the text), with the mask
    // computed over the whole buffer so a value crossing the cut is hidden.
    let start = bytes.len().saturating_sub(cap);
    let text: String = String::from_utf8_lossy(&mask(bytes, secrets, start))
        .chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                '?'
            } else {
                c
            }
        })
        .collect();
    if text.len() <= cap {
        return text;
    }
    let mut cut = text.len() - cap;
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    text[cut..].to_string()
}

/// Replace every byte covered by any occurrence of any secret, found in the
/// original bytes (so overlapping values are masked as their union), with
/// one `[redacted]` per masked run, emitting only bytes from `start` on.
fn mask(bytes: &[u8], secrets: &[String], start: usize) -> Vec<u8> {
    let mut hidden = vec![false; bytes.len()];
    for secret in secrets.iter().map(String::as_bytes) {
        if secret.is_empty() || secret.len() > bytes.len() {
            continue;
        }
        for start in 0..=bytes.len() - secret.len() {
            if &bytes[start..start + secret.len()] == secret {
                hidden[start..start + secret.len()].fill(true);
            }
        }
    }
    let mut out = Vec::with_capacity(bytes.len());
    for (i, b) in bytes.iter().enumerate().skip(start) {
        if !hidden[i] {
            out.push(*b);
        } else if i == start || !hidden[i - 1] {
            out.extend_from_slice(b"[redacted]");
        }
    }
    out
}

/// A fresh alert ID: `<prefix>-<utc millis>-<pid>`.
pub fn new_id(prefix: &str) -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{prefix}-{millis}-{}", std::process::id())
}

/// One file per alert under `attention/alerts/`.
pub mod store {
    use std::fs;
    use std::io;

    use serde::{Deserialize, Serialize};

    use super::{AlertOutcome, AlertRequest, Invocation, Reason};
    use crate::bus::fsx::{self, TMP_PREFIX, Written};
    use crate::bus::layout::RunPaths;
    use crate::bus::outcome::{Code, Failure};
    use crate::bus::time::{parse_utc, utc_now};

    /// How many invocations an alert keeps in full; `invocation_count` keeps
    /// counting past it.
    pub const MAX_INVOCATIONS: usize = 10;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub enum State {
        Open,
        Resolved,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct InvocationEntry {
        pub at: String,
        /// `invoked`, `failed`, `timed-out` or `unavailable`.
        pub outcome: String,
        pub status: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub message: Option<String>,
        pub output_tail: String,
        pub elapsed_ms: u64,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Alert {
        pub schema: u32,
        pub id: String,
        pub member: String,
        pub reason: Reason,
        pub pending: u32,
        pub opened_at: String,
        pub state: State,
        /// Every invocation so far, including those no longer listed.
        pub invocation_count: u32,
        /// The most recent invocations, oldest first.
        pub invocations: Vec<InvocationEntry>,
        pub resolved_at: Option<String>,
        pub resolved_by: Option<String>,
    }

    /// Serializes read-modify-write of alert files. Taken on its own, never
    /// while running an action.
    struct StoreLock(#[allow(dead_code)] fs::File);

    /// The bus root, run, `attention/` and `attention/alerts/` directories that
    /// exist must be private ([`fsx::check_private_dir`]): real directories,
    /// yours, not group- or other-writable. Writers also need the root and run
    /// to exist already (created by `ensure_run_dir`).
    pub fn check_tree(paths: &RunPaths, writing: bool) -> Result<(), Failure> {
        let root = paths
            .root
            .parent()
            .ok_or_else(|| Failure::new(Code::Error, "run directory has no parent"))?;
        for (dir, required) in [
            (root.to_path_buf(), writing),
            (paths.root.clone(), writing),
            (paths.attention_dir(), false),
            (paths.alerts_dir(), false),
        ] {
            match fs::symlink_metadata(&dir) {
                Err(e) if e.kind() == io::ErrorKind::NotFound && !required => continue,
                _ => fsx::check_private_dir(&dir)
                    .map_err(|e| Failure::new(Code::Refused, e.to_string()))?,
            }
        }
        Ok(())
    }

    fn lock(paths: &RunPaths) -> Result<StoreLock, Failure> {
        let path = paths.alerts_dir().join(".lock");
        let file = crate::state::open_lock(&path).map_err(|e| Failure::io("alerts lock", e))?;
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
            .map_err(|e| Failure::io("alerts lock", e.into()))?;
        Ok(StoreLock(file))
    }

    fn read(paths: &RunPaths, id: &str) -> Result<Option<Alert>, Failure> {
        let path = paths.alert(id);
        match fs::read(&path) {
            Ok(bytes) => {
                let corrupt = |why: String| {
                    Failure::new(
                        Code::Corrupt,
                        format!("{} is unreadable: {why}", path.display()),
                    )
                };
                let alert: Alert =
                    serde_json::from_slice(&bytes).map_err(|e| corrupt(e.to_string()))?;
                check(&alert, id).map_err(corrupt)?;
                Ok(Some(alert))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Failure::io(&path.display().to_string(), e)),
        }
    }

    /// An alert file holds only what brg writes: its own ID, plain-word
    /// arguments, UTC timestamps and known outcomes. Anything else (a planted
    /// or edited file) is refused rather than printed.
    fn check(alert: &Alert, id: &str) -> Result<(), String> {
        if alert.id != id {
            return Err("its id doesn't match its file name".into());
        }
        AlertRequest {
            id: alert.id.clone(),
            member: alert.member.clone(),
            reason: alert.reason,
            pending: alert.pending,
        }
        .check()
        .map_err(|_| "bad id or member name".to_string())?;
        let times = std::iter::once(&alert.opened_at)
            .chain(alert.resolved_at.as_ref())
            .chain(alert.invocations.iter().map(|i| &i.at));
        for t in times {
            if parse_utc(t).is_none() {
                return Err("bad timestamp".into());
            }
        }
        let outcomes = ["invoked", "failed", "timed-out", "unavailable"];
        if alert
            .invocations
            .iter()
            .any(|i| !outcomes.contains(&i.outcome.as_str()))
        {
            return Err("unknown invocation outcome".into());
        }
        if let Some(by) = &alert.resolved_by
            && !crate::names::is_plain_word(by)
        {
            return Err("bad resolved_by".into());
        }
        Ok(())
    }

    /// Replace the alert file for `id` (the validated, requested ID; never one
    /// read from a file).
    fn write(paths: &RunPaths, id: &str, alert: &Alert) -> Result<(), Failure> {
        let bytes = serde_json::to_vec_pretty(alert)
            .map_err(|e| Failure::new(Code::Error, e.to_string()))?;
        fsx::replace(&paths.alert(id), &bytes).map_err(|e| Failure::io("write alert", e))
    }

    /// Open the alert for `req`. Opening an ID again with the same member and
    /// reason returns the existing alert; anything else is a conflict.
    pub fn open(paths: &RunPaths, req: &AlertRequest) -> Result<Alert, Failure> {
        req.check().map_err(|e| Failure::new(Code::Invalid, e))?;
        check_tree(paths, true)?;
        let alert = Alert {
            schema: crate::bus::SCHEMA,
            id: req.id.clone(),
            member: req.member.clone(),
            reason: req.reason,
            pending: req.pending,
            opened_at: utc_now(),
            state: State::Open,
            invocation_count: 0,
            invocations: Vec::new(),
            resolved_at: None,
            resolved_by: None,
        };
        let bytes = serde_json::to_vec_pretty(&alert)
            .map_err(|e| Failure::new(Code::Error, e.to_string()))?;
        match fsx::write_new(&paths.alert(&req.id), &bytes)
            .map_err(|e| Failure::io("open alert", e))?
        {
            Written::Created => Ok(alert),
            Written::Exists => {
                let existing = read(paths, &req.id)?
                    .ok_or_else(|| Failure::new(Code::Error, "alert vanished"))?;
                if existing.member == req.member && existing.reason == req.reason {
                    Ok(existing)
                } else {
                    Err(Failure::new(
                        Code::Conflict,
                        format!(
                            "alert `{}` already exists for {} ({})",
                            req.id,
                            existing.member,
                            existing.reason.as_str()
                        ),
                    ))
                }
            }
        }
    }

    /// Add one invocation to an alert.
    pub fn record_invocation(
        paths: &RunPaths,
        id: &str,
        inv: &Invocation,
    ) -> Result<Alert, Failure> {
        check_tree(paths, true)?;
        if !crate::bus::is_valid_run_id(id) {
            return Err(Failure::new(Code::Invalid, "not an alert ID"));
        }
        let _lock = lock(paths)?;
        let mut alert = read(paths, id)?
            .ok_or_else(|| Failure::new(Code::NotFound, format!("no alert `{id}`")))?;
        let (status, message) = match &inv.outcome {
            AlertOutcome::Failed { status } => (*status, None),
            AlertOutcome::Unavailable(why) => (None, Some(why.clone())),
            _ => (None, None),
        };
        alert.invocations.push(InvocationEntry {
            at: utc_now(),
            outcome: inv.outcome.as_str().into(),
            status,
            message,
            output_tail: inv.output.clone(),
            elapsed_ms: inv.elapsed_ms,
        });
        if alert.invocations.len() > MAX_INVOCATIONS {
            let cut = alert.invocations.len() - MAX_INVOCATIONS;
            alert.invocations.drain(..cut);
        }
        alert.invocation_count = alert.invocation_count.saturating_add(1);
        write(paths, id, &alert)?;
        Ok(alert)
    }

    /// Mark an alert resolved by `by`. Resolving twice keeps the first.
    pub fn resolve(paths: &RunPaths, id: &str, by: &str) -> Result<Alert, Failure> {
        check_tree(paths, true)?;
        if !crate::bus::is_valid_run_id(id) {
            return Err(Failure::new(Code::Invalid, "not an alert ID"));
        }
        let _lock = lock(paths)?;
        let mut alert = read(paths, id)?
            .ok_or_else(|| Failure::new(Code::NotFound, format!("no alert `{id}`")))?;
        if alert.state == State::Resolved {
            return Ok(alert);
        }
        alert.state = State::Resolved;
        alert.resolved_at = Some(utc_now());
        alert.resolved_by = Some(by.to_string());
        write(paths, id, &alert)?;
        Ok(alert)
    }

    /// Every alert, oldest first.
    pub fn list(paths: &RunPaths) -> Result<Vec<Alert>, Failure> {
        if !paths.root.exists() {
            return Ok(Vec::new());
        }
        check_tree(paths, false)?;
        let dir = paths.alerts_dir();
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Failure::io(&dir.display().to_string(), e)),
        };
        let mut alerts = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| Failure::io(&dir.display().to_string(), e))?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(TMP_PREFIX) || name.starts_with('.') {
                continue;
            }
            let Some(id) = name.strip_suffix(".json") else {
                continue;
            };
            if !crate::bus::is_valid_run_id(id) {
                return Err(Failure::new(
                    Code::Corrupt,
                    format!("{} holds a file that isn't an alert", dir.display()),
                ));
            }
            if let Some(alert) = read(paths, id)? {
                alerts.push(alert);
            }
        }
        alerts.sort_by(|a, b| (&a.opened_at, &a.id).cmp(&(&b.opened_at, &b.id)));
        Ok(alerts)
    }
}

// ---- CLI -------------------------------------------------------------------

/// `brg bus alert …` and `brg bus alerts`; flattened into the `bus` group.
#[derive(Subcommand)]
pub enum Commands {
    /// Test or resolve alerts.
    Alert {
        #[command(subcommand)]
        command: AlertCommand,
    },
    /// List open alerts (`--all` includes resolved ones).
    Alerts(ListArgs),
}

#[derive(Subcommand)]
pub enum AlertCommand {
    /// Run the alert action once with reason `test`, and record the alert.
    Test {
        /// The member the alert names (default: the team's root).
        #[arg(long)]
        member: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Mark an alert resolved.
    Resolve {
        id: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Args)]
pub struct ListArgs {
    #[arg(long)]
    all: bool,
    #[arg(long)]
    json: bool,
}

/// Outcome name when no alert action can run.
pub const ATTENTION_UNAVAILABLE: &str = "attention-unavailable";

struct Ctx {
    state: StateDir,
    meta: Option<Meta>,
    bus: BusConfig,
}

fn context(input: &crate::validate::Input) -> Result<Ctx, Failure> {
    let crate::validate::Input::Deployment(file) = input else {
        return Err(Failure::new(
            Code::NotConfigured,
            "a team on its own has no bus; use a deployment file with `bus:`",
        ));
    };
    let mut report = crate::diag::Report::default();
    let dep = crate::deploy::load(file, &mut report).ok_or_else(|| {
        let why: Vec<String> = report.errors().map(|d| d.to_string()).collect();
        Failure::new(Code::Invalid, why.join("; "))
    })?;
    let bus = dep.bus.ok_or_else(|| {
        Failure::new(Code::NotConfigured, "this deployment has no `bus:` section")
    })?;
    let state = crate::runtime::state_for(input).state;
    let meta = state::read_meta(&state);
    // Once started, the deployment acts for the bus it was started with.
    if let Some(meta) = &meta
        && meta.bus.as_ref() != Some(&BusMeta::of(&bus))
    {
        return Err(Failure::new(
            Code::Refused,
            "the bus root, run or alert action in the deployment file differs from the one recorded at `brg up`; restore it, or run `brg down` and `brg up` again",
        ));
    }
    Ok(Ctx { state, meta, bus })
}

fn finish(json: bool, result: Result<(Value, String), (Failure, Option<Value>)>) -> ExitCode {
    match result {
        Ok((value, text)) => {
            if json {
                println!("{value}");
            } else {
                println!("{text}");
            }
            ExitCode::SUCCESS
        }
        Err((failure, extra)) => {
            let mut value = failure.to_json();
            if let (Some(map), Some(Value::Object(extra))) = (value.as_object_mut(), extra) {
                map.extend(extra);
            }
            if json {
                println!("{value}");
            } else {
                eprintln!("brg: {}", failure.message);
            }
            ExitCode::from(failure.code.exit())
        }
    }
}

type CliResult = Result<(Value, String), (Failure, Option<Value>)>;

/// Run `brg bus alert …` / `brg bus alerts`.
pub fn main(input: &crate::validate::Input, command: Commands) -> ExitCode {
    match command {
        Commands::Alert {
            command: AlertCommand::Test { member, json },
        } => finish(json, cli_test(input, member.as_deref())),
        Commands::Alert {
            command: AlertCommand::Resolve { id, json },
        } => finish(json, cli_resolve(input, &id)),
        Commands::Alerts(args) => finish(args.json, cli_list(input, args.all)),
    }
}

fn plain(f: Failure) -> (Failure, Option<Value>) {
    (f, None)
}

fn cli_test(input: &crate::validate::Input, member: Option<&str>) -> CliResult {
    let ctx = context(input).map_err(plain)?;
    let meta = ctx.meta.ok_or_else(|| {
        plain(Failure::new(
            Code::NotConfigured,
            "this deployment hasn't been started; run `brg up` first",
        ))
    })?;
    let member = match member {
        Some(m) => m.to_string(),
        None => meta
            .members
            .iter()
            .find(|(_, m)| m.parent.is_none())
            .map(|(id, _)| id.clone())
            .ok_or_else(|| plain(Failure::new(Code::Invalid, "the team has no members")))?,
    };
    if !meta.members.contains_key(&member) {
        return Err(plain(Failure::new(
            Code::Invalid,
            format!("`{member}` is not a member of this deployment"),
        )));
    }
    if ctx.bus.alert.is_none() {
        return Err((
            Failure::new(
                Code::NotConfigured,
                "no `bus.alert` is configured, so attention can't reach a person",
            ),
            Some(json!({"outcome": ATTENTION_UNAVAILABLE})),
        ));
    }
    // `ensure_run_dir` reports an unsafe tree (symlink, not yours, shared) as
    // `Other`; anything else is an ordinary I/O error.
    let paths = crate::bus::layout::ensure_run_dir(&ctx.bus).map_err(|e| {
        plain(if e.kind() == std::io::ErrorKind::Other {
            Failure::new(Code::Refused, format!("run directory: {e}"))
        } else {
            Failure::io("run directory", e)
        })
    })?;
    let req = AlertRequest {
        id: new_id("test"),
        member,
        reason: Reason::Test,
        pending: 0,
    };
    store::open(&paths, &req).map_err(plain)?;
    let inv = invoke(&ctx.state, &meta, &ctx.bus, &req, &Limits::default());
    let alert = store::record_invocation(&paths, &req.id, &inv).map_err(plain)?;
    let alert_json = serde_json::to_value(&alert).unwrap_or(Value::Null);
    match &inv.outcome {
        AlertOutcome::Invoked => Ok((
            ok_json("invoked", json!({"alert": alert_json})),
            format!(
                "alert {}: handed off to {} (not proof anyone saw it); resolve with `brg bus alert resolve {}`",
                req.id,
                ctx.bus
                    .alert
                    .as_ref()
                    .map(|a| format!("{}.{}", a.plugin, a.action))
                    .unwrap_or_default(),
                req.id
            ),
        )),
        AlertOutcome::Unavailable(why) => Err((
            Failure::new(Code::NotConfigured, format!("alert {}: {why}", req.id)),
            Some(json!({"outcome": ATTENTION_UNAVAILABLE, "alert": alert_json})),
        )),
        AlertOutcome::Failed { status } => Err((
            Failure::new(
                Code::Error,
                format!(
                    "alert {}: the action failed ({}): {}",
                    req.id,
                    status.map_or("killed".to_string(), |s| format!("exit {s}")),
                    inv.output.trim()
                ),
            ),
            Some(json!({"outcome": "alert-failed", "alert": alert_json})),
        )),
        AlertOutcome::TimedOut => Err((
            Failure::new(
                Code::Error,
                format!("alert {}: the action timed out and was killed", req.id),
            ),
            Some(json!({"outcome": "alert-timed-out", "alert": alert_json})),
        )),
    }
}

/// The run's paths for reading alerts, or `None` if the run directory doesn't
/// exist yet. The root, run and alert directories that exist must be
/// private (see [`crate::bus::fsx::check_private_dir`]), as for writers.
fn run_paths(
    input: &crate::validate::Input,
) -> Result<Option<crate::bus::layout::RunPaths>, Failure> {
    let ctx = context(input)?;
    let paths = crate::bus::layout::RunPaths::new(&ctx.bus);
    if !paths.root.exists() {
        return Ok(None);
    }
    store::check_tree(&paths, false)?;
    Ok(Some(paths))
}

fn cli_resolve(input: &crate::validate::Input, id: &str) -> CliResult {
    if !crate::bus::is_valid_run_id(id) {
        return Err(plain(Failure::new(
            Code::Usage,
            format!("`{id}` is not an alert ID"),
        )));
    }
    let paths = run_paths(input)
        .map_err(plain)?
        .ok_or_else(|| plain(Failure::new(Code::NotFound, format!("no alert `{id}`"))))?;
    let alert = store::resolve(&paths, id, "operator").map_err(plain)?;
    let text = format!("alert {id}: resolved");
    Ok((ok_json("resolved", json!({"alert": alert})), text))
}

fn cli_list(input: &crate::validate::Input, all: bool) -> CliResult {
    let listed = match run_paths(input).map_err(plain)? {
        Some(paths) => store::list(&paths).map_err(plain)?,
        None => Vec::new(),
    };
    let alerts: Vec<store::Alert> = listed
        .into_iter()
        .filter(|a| all || a.state == store::State::Open)
        .collect();
    let mut text = String::new();
    if alerts.is_empty() {
        text.push_str(if all { "no alerts" } else { "no open alerts" });
    }
    for a in &alerts {
        let last = a
            .invocations
            .last()
            .map(|i| i.outcome.as_str())
            .unwrap_or("not invoked");
        text.push_str(&format!(
            "{}  {}  {}  {}  pending={}  invocations={} (last: {})  opened {}\n",
            a.id,
            match a.state {
                store::State::Open => "open",
                store::State::Resolved => "resolved",
            },
            a.member,
            a.reason.as_str(),
            a.pending,
            a.invocation_count,
            last,
            a.opened_at
        ));
    }
    Ok((
        ok_json("ok", json!({"alerts": alerts})),
        text.trim_end().to_string(),
    ))
}
