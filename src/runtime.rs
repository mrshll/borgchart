//! The tmux runtime: `create`, `start`, `stop`, `delete`, `status`, `ls`,
//! `attach`, `logs` and `send`, on instances and their members.

use std::collections::{BTreeMap, BTreeSet};
use std::io::IsTerminal;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;

use crate::address::{self, Instance};
use crate::bus::context;
use crate::bus::layout::StorePaths;
use crate::bus::time::utc_now;
use crate::config::Auth;
use crate::exec::{ExecSpec, PluginEnv, identity_env};
use crate::launch;
use crate::lifecycle;
use crate::lifecycle::{Incarnation, Liveness, PaneIdentity};
use crate::paths::{overlaps, resolve_existing_prefix};
use crate::proctree;
use crate::registry;
use crate::rules::{self, State};
use crate::state::{
    self, MemberMeta, Meta, PluginMeta, StateDir, shell_quote, write_private, write_script,
};
use crate::team::{Posture, Vendor};
use crate::tmux::{self, Tmux, target};
use crate::tree;
use crate::validate::{Validated, validate};

/// Environment names copied from your shell into every window. Nothing else is.
const BASE_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "TMPDIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "XDG_RUNTIME_DIR",
    "CODEX_HOME",
    "CLAUDE_CONFIG_DIR",
    state::ENV_STATE_HOME,
    crate::config::ENV_CONFIG,
    state::ENV_TMUX_SOCKET,
    state::ENV_TMUX_TMPDIR,
];

/// How long a start waits for a `locked` member's gate to report in.
fn gate_wait() -> Duration {
    std::env::var("BORGCHART_GATE_WAIT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(20))
}
pub const MAX_SEND_BYTES: usize = 16 * 1024;

pub fn base_env() -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for name in BASE_ENV {
        if let Ok(value) = std::env::var(name) {
            env.insert((*name).to_string(), value);
        }
    }
    // Members run in other folders, so the configuration they inherit is
    // named by its absolute path.
    if let Some(value) = env.get_mut(crate::config::ENV_CONFIG)
        && let Ok(abs) = std::path::absolute(&*value)
    {
        *value = abs.display().to_string();
    }
    env.entry("LANG".into()).or_insert_with(|| "C.UTF-8".into());
    env
}

fn brg_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("brg"))
}

/// The instance's tmux server: the socket path recorded at a start once
/// there is one, whatever the caller's tmux settings say; until then, the
/// socket brg's settings name.
fn tmux_for(state: &StateDir) -> Tmux {
    let tmux = Tmux::new(&state.tmux_conf(), base_env());
    match state::read_meta(state).and_then(|m| m.socket_path) {
        Some(path) => tmux.with_socket(&path.display().to_string()),
        None => tmux,
    }
}

fn vendor_version(vendor: Vendor, env: &BTreeMap<String, String>) -> Option<String> {
    let out = Command::new(vendor.as_str())
        .arg("--version")
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !text.is_empty())
        .then(|| text.lines().next().unwrap_or("").to_string())
}

/// Run `program --help` with the base environment, capped in time and size.
fn capture_help(program: &Path, cwd: &Path, env: &BTreeMap<String, String>) -> Option<String> {
    let mut child = Command::new(program)
        .arg("--help")
        .env_clear()
        .envs(env)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let start = Instant::now();
    loop {
        if child.try_wait().ok()?.is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output().ok()?;
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.truncate(4096);
    Some(text)
}

struct Started {
    id: String,
    vendor: Vendor,
    locked: bool,
    /// The declared cwd whose Claude folder-trust dialog a start may accept.
    trust_dir: Option<PathBuf>,
    /// A Codex member started without session trust: the start watches for
    /// its trust screen, only to say so; it never answers it.
    watch_codex_trust: bool,
    /// The pane this start launched; nothing after the wait touches another.
    pane: Option<PaneIdentity>,
}

/// Whether `s`'s window still runs the pane this start launched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Launched {
    Same,
    /// Another pane or process: someone else's start. Leave it alone.
    Replaced,
    /// Its pane can't be read just now.
    Unknown,
}

fn launched(tmux: &Tmux, session: &str, s: &Started) -> Launched {
    match (&s.pane, lifecycle::pane_identity(tmux, session, &s.id)) {
        (Some(a), Some(b)) if a.pane_id == b.pane_id && a.pane_pid == b.pane_pid => Launched::Same,
        (Some(_), Some(_)) => Launched::Replaced,
        _ => Launched::Unknown,
    }
}

/// The id of the rules that recognize each vendor's folder-trust screen.
const TRUST_RULE: &str = "trust-dialog";
const TRUST_NO_SELECTED: &str = " ❯ No, exit";
const TRUST_YES_SELECTED: &str = " ❯ Yes, I trust this folder";

fn has_line(screen: &str, line: &str) -> bool {
    screen.lines().any(|l| l.trim_end() == line)
}

/// The folder Claude's trust dialog names: the first non-blank line after "Accessing workspace:".
pub fn trust_dialog_path(screen: &str) -> Option<PathBuf> {
    let mut lines = screen.lines();
    lines.find(|l| l.trim() == "Accessing workspace:")?;
    let path = lines.find(|l| !l.trim().is_empty())?.trim();
    path.starts_with('/').then(|| PathBuf::from(path))
}

/// What a start reports for a member another start replaced while this one
/// waited on it.
const REPLACED: &str = "replaced while starting; left alone";

/// Record a member's outcome, replacing what was there.
fn set(outcomes: &mut BTreeMap<String, String>, id: &str, text: String) {
    outcomes.insert(id.to_string(), text);
}

/// Add to a member's outcome (`started; accepted folder trust for …`).
fn append(outcomes: &mut BTreeMap<String, String>, id: &str, text: String) {
    let entry = outcomes.entry(id.to_string()).or_default();
    if !entry.is_empty() {
        entry.push_str("; ");
    }
    entry.push_str(&text);
}

pub struct UpOutcome {
    /// The team as a tree, each member with what happened to it, then the
    /// plugin processes.
    pub tree: String,
    /// Anything else to tell the person, after the tree.
    pub notes: Vec<String>,
    pub failed: bool,
}

/// What a fresh start needs from your environment: each plugin's values,
/// and the names that are missing.
type PluginValues = BTreeMap<String, BTreeMap<String, String>>;

fn plugin_values(v: &Validated) -> (PluginValues, Vec<String>) {
    let mut all = BTreeMap::new();
    let mut missing = Vec::new();
    for (name, plugin) in &v.team.plugins {
        let mut values = plugin.values.clone();
        for env_name in plugin.secrets() {
            match std::env::var(env_name) {
                Ok(value) => {
                    values.insert(env_name.clone(), value);
                }
                Err(_) => missing.push(format!("{name}: {env_name}")),
            }
        }
        all.insert(name.clone(), values);
    }
    (all, missing)
}

/// The vendor's own key a member gets with `auth: api_key`.
fn key_name(vendor: Vendor) -> &'static str {
    match vendor {
        Vendor::Claude => "ANTHROPIC_API_KEY",
        Vendor::Codex => "OPENAI_API_KEY",
    }
}

/// With `auth: api_key`, each starting member's key, from your environment.
/// With `login`, none: a key in your shell never replaces your login.
fn api_keys(
    v: &Validated,
    record: &registry::InstanceRecord,
    ids: &[String],
) -> Result<BTreeMap<String, (String, String)>, String> {
    let mut keys = BTreeMap::new();
    if v.auth() != Auth::ApiKey {
        return Ok(keys);
    }
    let mut missing = Vec::new();
    for id in ids {
        let Some(role) = record.role_of(id).and_then(|r| v.team.roles.get(r)) else {
            continue;
        };
        let key = key_name(role.vendor);
        match std::env::var(key) {
            Ok(value) => {
                keys.insert(id.clone(), (key.to_string(), value));
            }
            Err(_) => missing.push(format!("{id}: {key}")),
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "`auth: api_key` needs these in your environment; nothing was started: {}",
            missing.join(", ")
        ));
    }
    Ok(keys)
}

/// Where a member starts: its role's `cwd` inside the instance's folder
/// (with `{member_id}` expanded), else the folder. The folder must exist, and a
/// role's `cwd` must stay inside it once symlinks are resolved.
fn member_cwd(folder: &Path, cwd: Option<&str>, member: &str) -> Result<PathBuf, String> {
    let folder = folder.canonicalize().map_err(|_| {
        format!(
            "the instance's working folder {} is gone; create a new instance for another folder",
            folder.display()
        )
    })?;
    let Some(cwd) = cwd else {
        return Ok(folder);
    };
    let rel = cwd.replace("{member_id}", member);
    let dir = folder.join(&rel);
    let real = dir
        .canonicalize()
        .map_err(|_| format!("`{member}`'s cwd {} does not exist", dir.display()))?;
    if !real.starts_with(&folder) {
        return Err(format!(
            "`{member}`'s cwd `{rel}` resolves outside the instance's folder {} (through a symlink)",
            folder.display()
        ));
    }
    Ok(real)
}

/// Start `member_ids` of the instance, from validated files `v`. `full` is a
/// start of the whole instance (its plugin processes too); otherwise one
/// member's start.
///
/// Everything is checked before anything is written. Starting into a
/// running instance (its session exists and its start record is there)
/// reuses the base environment and plugin values it started with, and
/// refuses a changed configuration. A bus keeps one store; changed files apply
/// on a full start only once every member is positively stopped. A start of
/// one member with changed files is refused. The registry and lifecycle locks
/// are held while files are written and windows started, and released before
/// waiting for gates and folder trust.
fn start_members(
    lock: registry::Lock,
    inst: &Instance,
    v: &Validated,
    member_ids: Vec<String>,
    full: bool,
) -> Result<UpOutcome, String> {
    let only = !full;
    let state = &inst.state;
    let record = &inst.record;
    let session = state.session();
    state::check_state_home()?;
    if state
        .root
        .display()
        .to_string()
        .chars()
        .any(char::is_whitespace)
    {
        return Err(format!(
            "the state folder {} contains whitespace, which `locked` command rules can't match; set BORGCHART_STATE_HOME to a path without spaces",
            state.root.display()
        ));
    }
    check_tmux_tmpdir()?;
    let shared = shared_folders(state)?;
    // Every window, shim, gate hook and plugin process this start starts
    // runs the brg running now, as in v0: brg isn't pinned.
    let brg = brg_path();
    let team = inst.id.clone();
    let restart = state::restart_hint(&inst.id);
    let snapshot = v.config_snapshot();
    let bus = v.team.bus.clone();
    let role_of = |id: &str| -> Result<&crate::team::Role, String> {
        record
            .role_of(id)
            .and_then(|r| v.team.roles.get(r))
            .ok_or_else(|| format!("`{id}`'s role is not in the team any more"))
    };
    let mut cwds: BTreeMap<String, PathBuf> = BTreeMap::new();
    // Checks that need nothing written.
    for id in &member_ids {
        let member = role_of(id)?;
        check_copy_folder(inst, member, id)?;
        let granted = launch::granted(&v.team, member);
        if let Some((plugin, command)) = launch::shim_commands(&granted)
            .into_iter()
            .find(|(_, c)| launch::RESERVED_COMMANDS.contains(&c.as_str()))
        {
            return Err(format!(
                "plugin `{plugin}`'s command `{command}` would hide the installed `{command}` from {team}/{id}, so its message commands would call the plugin; rename it. Nothing was started"
            ));
        }
        cwds.insert(
            id.clone(),
            member_cwd(&record.folder, member.cwd.as_deref(), id)
                .map_err(|e| format!("{e}; nothing was started"))?,
        );
    }
    let mut pre_notes = Vec::new();
    for (id, cwd) in &cwds {
        let member = role_of(id)?;
        check_state_overlap(cwd, &state.root)
            .map_err(|e| format!("{}'s {e}; nothing was started", inst.at(id)))?;
        if overlaps(cwd, &v.team.dir) {
            if v.posture(&member.id) == Posture::Locked {
                return Err(format!(
                    "{} is `locked`, and its folder {} overlaps the team folder {}, so it could change its own role files; give it a folder apart from the team. Nothing was started",
                    inst.at(id),
                    cwd.display(),
                    v.team.dir.display()
                ));
            }
            let note = format!(
                "note: the team folder {} is inside the members' folder {}; they can change the team's files",
                v.team.dir.display(),
                cwd.display()
            );
            if !pre_notes.contains(&note) {
                pre_notes.push(note);
            }
        }
    }
    for (id, cwd) in &cwds {
        let member = role_of(id)?;
        if let Some(note) = project_settings_note(v, member, cwd) {
            pre_notes.push(format!("note: {}: {note}", inst.at(id)));
        }
    }
    refuse_legacy_sessions(&tmux_for(state))?;
    let wanted: Vec<(String, PathBuf)> = cwds.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    crate::checkouts::reserve(&lock, &inst.id, &wanted)?;
    let (values, missing) = plugin_values(v);
    let keys = api_keys(v, record, &member_ids)?;
    let missing_error = || {
        format!(
            "set these secrets in your environment first (brg stores none); nothing was started: {}",
            missing.join(", ")
        )
    };
    let probe = tmux_for(state);
    let looks_running = probe.has_session(&session) && state.meta_file().exists();
    if !looks_running && !missing.is_empty() {
        return Err(missing_error());
    }

    // Serialize lifecycle changes, then decide for real.
    state::ensure_dir(&state.root)
        .map_err(|e| format!("cannot create {}: {e}", state.root.display()))?;
    let state_lock = lifecycle::lock_state(state)?;
    let previous = state::read_meta(state);
    // Unless the last start here was confirmed stopped, or never started a
    // window, a server this caller can't see may still run it.
    let may_still_run = previous
        .as_ref()
        .is_some_and(|m| m.stopped_at.is_none() && m.windows_started != Some(false));
    let existing = previous.clone().filter(|_| probe.has_session(&session));
    let joining = existing.is_some();
    match &existing {
        Some(meta) => lifecycle::check_join(meta, &snapshot, state, &team, &restart)?,
        None if !missing.is_empty() => return Err(missing_error()),
        None => {}
    }
    if !joining && let Some(meta) = &previous {
        let changed = meta.config.as_ref().is_none_or(|old| old != &snapshot);
        if changed {
            if only {
                return Err(match &meta.config {
                    Some(old) => lifecycle::changed_since_start(&team, &restart, old, &snapshot),
                    None => format!(
                        "instance `{team}` has no configuration snapshot; start the whole instance: {restart}"
                    ),
                });
            }
            lifecycle::check_stopped(state, meta, &restart)?;
        }
    }
    let storage = bus.as_ref().map(|_| StorePaths::new(state));
    if let Some(paths) = &storage {
        if paths.validate(&state.id)? {
            lifecycle::open_work(state).map_err(|e| unreadable_store(inst, &e))?;
        }
        paths.initialize(state)?;
    }
    let mut notes: Vec<String> = pre_notes;
    let user_config = v.config.tmux_user_config.as_ref().and_then(|u| u.resolve());
    // The server reads its config once, when it starts, and every instance
    // on it shares it.
    let wanted = user_config
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    if let Ok(loaded) =
        tmux_for(state).ok(&["show-options", "-gqv", crate::tmux::USER_CONFIG_OPTION])
        && loaded.trim() != wanted
    {
        notes.push(format!(
            "brg's tmux server is already running {}, so `tmux.user_config` ({}) takes effect only once every instance on it is stopped and it starts again",
            if loaded.trim().is_empty() {
                "without your tmux config".to_string()
            } else {
                format!("with {}", loaded.trim())
            },
            if wanted.is_empty() { "off" } else { wanted.as_str() }
        ));
    }

    let parent_member = |member: &crate::team::Role| {
        member
            .parent
            .as_ref()
            .map(|p| {
                record
                    .members_of(p)
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| p.clone())
            })
            .or_else(|| record.parent.clone())
    };
    let member_meta = |id: &str, base: &BTreeMap<String, String>| -> Option<MemberMeta> {
        let role = record.role_of(id)?;
        let member = v.team.roles.get(role)?;
        Some(MemberMeta {
            role: role.to_string(),
            vendor: member.vendor.as_str().into(),
            posture: match v.posture(role) {
                Posture::Vendor => "vendor".into(),
                Posture::Locked => "locked".into(),
            },
            parent: parent_member(member),
            vendor_version: vendor_version(member.vendor, base),
            started: Some(false),
        })
    };
    let fresh = existing.is_none();
    let mut meta = match existing {
        Some(meta) => meta,
        None => {
            let base = base_env();
            Tmux::new(&state.tmux_conf(), base.clone())
                .write_config(user_config.as_deref())
                .map_err(|e| format!("cannot write tmux config: {e}"))?;
            let mut meta = Meta {
                team_dir: v.team.dir.clone(),
                folder: record.folder.clone(),
                brg: brg.clone(),
                socket: Some(state::tmux_socket()),
                config: Some(snapshot.clone()),
                base_env: base.clone(),
                windows_started: Some(may_still_run),
                ..Meta::default()
            };
            // Plugins: their values (secrets included) go to private files for the shims.
            for (name, plugin) in &v.team.plugins {
                let body = serde_json::to_vec(&PluginEnv {
                    env: values.get(name).cloned().unwrap_or_default(),
                })
                .map_err(|e| e.to_string())?;
                write_private(&state.plugin_env_file(name), &body)
                    .map_err(|e| format!("cannot write plugin settings: {e}"))?;
                state::ensure_dir(&state.plugin_dir(name)).map_err(|e| e.to_string())?;
                meta.plugins.insert(
                    name.clone(),
                    PluginMeta {
                        dir: v.plugin_dir(name),
                        has_process: plugin.process.is_some(),
                    },
                );
            }
            meta
        }
    };
    // Every member of the instance is known to the start record, including
    // ones created since the instance started.
    let mut added = fresh;
    for (id, _) in record.kept_members() {
        if !meta.members.contains_key(id)
            && let Some(m) = member_meta(id, &meta.base_env)
        {
            meta.members.insert(id.clone(), m);
            added = true;
        }
        if let Some(entry) = meta.members.get_mut(id)
            && let Some(role) = record.role_of(id).and_then(|r| v.team.roles.get(r))
        {
            let parent = parent_member(role);
            if entry.parent != parent {
                entry.parent = parent;
                added = true;
            }
        }
        state::open_lock(&state.lock_file(id))
            .map_err(|e| format!("cannot create send lock: {e}"))?;
    }
    if added {
        write_private(
            &state.meta_file(),
            &serde_json::to_vec_pretty(&meta).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("cannot write state: {e}"))?;
    }
    let base = meta.base_env.clone();
    let tmux = Tmux::new(&state.tmux_conf(), base.clone());
    // A running instance's server is the one recorded for it.
    let tmux = match &meta.socket_path {
        Some(path) => tmux.with_socket(&path.display().to_string()),
        None => tmux,
    };

    // The folders these members start in, held from now on: recorded
    // under the registry lock, before any window starts.
    {
        let mut updated = record.clone();
        for (id, cwd) in &cwds {
            if let Some(m) = updated.members.get_mut(id) {
                m.cwd = Some(cwd.clone());
            }
        }
        if updated != registry::read_instance(state)? {
            lock.save_instance(state, &updated)?;
        }
    }
    // Whether this start has created a window yet.
    let mut any_window = false;
    let mut outcomes: BTreeMap<String, String> = BTreeMap::new();
    let mut plugin_outcomes: Vec<(String, String)> = Vec::new();
    let mut failed = false;
    let mut started: Vec<Started> = Vec::new();

    for id in &member_ids {
        let member = role_of(id)?;
        if let Some(w) = tmux.window(&session, id) {
            if !w.dead {
                set(&mut outcomes, id, "already running".into());
                continue;
            }
            let _ = tmux.kill_window(&session, id);
        }
        let posture = v.posture(&member.id);
        let granted = launch::granted(&v.team, member);

        // Shims for granted commands.
        let shim_dir = state.member_bin(id);
        let _ = std::fs::remove_dir_all(&shim_dir);
        let mut help = BTreeMap::new();
        for (plugin, command) in launch::shim_commands(&granted) {
            let script = launch::shim_script(&brg, state, id, &plugin, &command);
            write_script(&shim_dir.join(&command), &script)
                .map_err(|e| format!("cannot write shim: {e}"))?;
            if granted
                .iter()
                .any(|g| g.command == command && g.line.ends_with("--help"))
                && let Some(dir) = meta.plugins.get(&plugin).and_then(|p| p.dir.clone())
                && let Some(text) = capture_help(&dir.join(&command), &dir, &base)
            {
                help.insert(command.clone(), text);
            }
        }

        let auto_trust = v.config.auto_trust;
        // Codex's sandbox can write its cwd, so a `locked` Codex member whose
        // role sets none starts in its own temp dir rather than the folder.
        let cwd =
            if member.cwd.is_none() && posture == Posture::Locked && member.vendor == Vendor::Codex
            {
                state.member_tmp(id)
            } else {
                cwds[id].clone()
            };
        let in_folder = cwd != state.member_tmp(id);
        // Claude: brg answers its trust dialog for the member's folder. By
        // default only for `locked` members, which ignore project settings.
        let trust_dir = (in_folder
            && member.vendor == Vendor::Claude
            && auto_trust.unwrap_or(posture == Posture::Locked))
        .then(|| cwd.clone());
        // Codex: trust passed as session config, only with `auto_trust: true`
        // and only in `vendor` posture. A trusted folder's `.codex/` loads, and a
        // `locked` Codex member can write its folder, so it could configure its
        // own next start outside its gate. The role's own `projects` setting
        // wins.
        let codex_trust = (in_folder
            && member.vendor == Vendor::Codex
            && auto_trust == Some(true)
            && posture == Posture::Vendor
            && !launch::codex_args_set(&member.args, "projects"))
        .then(|| cwd.clone());

        let settings_path = state.member_dir(id).join("claude-settings.json");
        let spec = launch::member_launch(
            &brg,
            state,
            &v.team,
            member,
            id,
            posture,
            &help,
            &settings_path,
            codex_trust.as_deref(),
            &shared,
        );
        if let Some(rules) = &spec.gate {
            write_private(
                &state.gate_file(id),
                &serde_json::to_vec_pretty(rules).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        }
        if let Some(settings) = &spec.claude_settings {
            write_private(
                &settings_path,
                &serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        }
        let _ = std::fs::remove_file(state.gate_marker(id));
        let _ = std::fs::remove_file(state.member_dir(id).join("trust-accepted"));

        let mut env = base.clone();
        let path = format!(
            "{}:{}",
            shim_dir.display(),
            base.get("PATH").cloned().unwrap_or_default()
        );
        env.insert("PATH".into(), path);
        env.insert(address::ENV_INSTANCE.into(), state.id.clone());
        env.insert(context::ENV_MEMBER.into(), id.clone());
        if member.parent.is_none()
            && let Some(parent) = &record.parent
        {
            env.insert("BORGCHART_PARENT".into(), parent.clone());
        }
        env.insert(crate::exec::ENV_ROLE.into(), member.id.clone());
        env.insert(
            crate::exec::ENV_NOTES.into(),
            state.notes_dir().display().to_string(),
        );
        // The brg that starts this member, so its own brg calls don't depend
        // on what its shell puts first on `PATH`.
        env.insert("BORGCHART_BIN".into(), brg.display().to_string());
        let tmp = state.member_tmp(id);
        state::ensure_dir(&tmp).map_err(|e| e.to_string())?;
        env.insert("TMPDIR".into(), tmp.display().to_string());
        if let Some((key, value)) = keys.get(id) {
            env.insert(key.clone(), value.clone());
        }
        // With a bus, each start is a new incarnation, recorded before its
        // window exists and again once its pane is known.
        let incarnation = match &storage {
            Some(_) => {
                let inc = Incarnation {
                    member: id.clone(),
                    incarnation: lifecycle::new_incarnation_id(state, id),
                    started_at: utc_now(),
                    vendor_version: if joining {
                        vendor_version(member.vendor, &base)
                    } else {
                        meta.members.get(id).and_then(|m| m.vendor_version.clone())
                    },
                    pane: None,
                    ended_at: None,
                    ended_reason: None,
                };
                lifecycle::begin_incarnation(state, &inc)?;
                env.insert(context::ENV_INCARNATION.into(), inc.incarnation.clone());
                Some(inc)
            }
            None => None,
        };
        let result = start_window(
            &tmux,
            &brg,
            state,
            &mut meta,
            &session,
            id,
            &ExecSpec {
                argv: spec.argv,
                env,
                cwd,
            },
        );
        if let (Some(_), Err(_)) = (&incarnation, &result) {
            // tmux didn't create the window, so nothing runs it.
            let _ = lifecycle::end_incarnation(state, id, lifecycle::ENDED_START_FAILED);
        }
        if result.is_err() {
            unrecord_windows(state, &mut meta, fresh && !any_window, may_still_run);
        }
        result?;
        any_window = true;
        // Without a bus, the instance's state is where `status` learns that a
        // member ran.
        if storage.is_none()
            && let Some(m) = meta.members.get_mut(id)
            && m.started != Some(true)
        {
            m.started = Some(true);
            write_private(
                &state.meta_file(),
                &serde_json::to_vec_pretty(&meta).map_err(|e| e.to_string())?,
            )
            .map_err(|e| format!("cannot write state: {e}"))?;
        }
        let pane = lifecycle::pane_identity(&tmux, &session, id);
        if pane.is_none() && posture == Posture::Locked {
            // Without its pane, later checks couldn't tell this member from a
            // replacement, so it couldn't be stopped safely if its gate never
            // reported in. Stop it now, while the locks are held.
            let _ = tmux.kill_window(&session, id);
            if incarnation.is_some() {
                let _ = lifecycle::end_incarnation(state, id, lifecycle::ENDED_START_FAILED);
            }
            set(
                &mut outcomes,
                id,
                "stopped: brg couldn't read its pane, so its `locked` gate couldn't be checked"
                    .to_string(),
            );
            failed = true;
            continue;
        }
        if let Some(mut inc) = incarnation {
            inc.pane = pane.clone();
            lifecycle::complete_incarnation(state, &inc)?;
        }
        started.push(Started {
            id: id.clone(),
            vendor: member.vendor,
            locked: posture == Posture::Locked,
            trust_dir,
            watch_codex_trust: member.vendor == Vendor::Codex && codex_trust.is_none(),
            pane,
        });
        set(&mut outcomes, id, "started".into());
        if let Some(dir) = &codex_trust {
            let dir = dir.display().to_string();
            let _ = write_private(
                &state.member_dir(id).join("trust-accepted"),
                format!("{dir}\n").as_bytes(),
            );
            append(
                &mut outcomes,
                id,
                format!("trusted folder {dir} for this Codex session"),
            );
        }
    }

    // Plugin processes start with the whole instance.
    if full {
        for (name, plugin) in &v.team.plugins {
            let window = format!("_{name}");
            let dir = meta.plugins.get(name).and_then(|p| p.dir.clone());
            let Some((argv, cwd)) = launch::process_argv(plugin, dir.as_deref()) else {
                continue;
            };
            if let Some(w) = tmux.window(&session, &window) {
                if !w.dead {
                    plugin_outcomes.push((name.clone(), "already running".into()));
                    continue;
                }
                let _ = tmux.kill_window(&session, &window);
            }
            let secrets: PluginEnv = std::fs::read_to_string(state.plugin_env_file(name))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default();
            let mut env = base.clone();
            env.extend(secrets.env);
            env.extend(identity_env(&meta, state, "", None, name));
            if let Some(role) = &plugin.delivers_to
                && let Some(target) = record.members_of(role).into_iter().next()
            {
                env.insert("BORGCHART_DELIVER_TO".into(), target);
            }
            let cwd = cwd.unwrap_or_else(|| meta.team_dir.clone());
            let result = start_window(
                &tmux,
                &brg,
                state,
                &mut meta,
                &session,
                &window,
                &ExecSpec { argv, env, cwd },
            );
            if result.is_err() {
                unrecord_windows(state, &mut meta, fresh && !any_window, may_still_run);
            }
            result?;
            any_window = true;
            plugin_outcomes.push((name.clone(), "started".into()));
        }
    }

    // Record where the tmux server is, so `stop` and liveness
    // checks ask it directly, whatever the caller's tmux environment.
    if fresh {
        let path = started
            .iter()
            .find_map(|s| s.pane.as_ref().map(|p| p.socket.clone()))
            .or_else(|| {
                tmux.ok(&["display-message", "-p", "#{socket_path}"])
                    .ok()
                    .map(|t| t.trim().to_string())
                    .filter(|t| t.starts_with('/'))
            });
        if let Some(path) = path {
            meta.socket_path = Some(PathBuf::from(path));
            write_private(
                &state.meta_file(),
                &serde_json::to_vec_pretty(&meta).map_err(|e| e.to_string())?,
            )
            .map_err(|e| format!("cannot write state: {e}"))?;
        }
    }

    // Members may use the bus while they start up; another a member start
    // may proceed too. The checks below read screens without the locks, and
    // take them again for anything they change (a key press, a trust marker,
    // stopping a `locked` member), and only while the window still runs the
    // pane this start launched.
    drop(state_lock);
    drop(lock);
    let relock = |s: &Started| -> Result<(Launched, Vec<std::fs::File>), String> {
        let locks = vec![lifecycle::lock_state(state)?];
        Ok((launched(&tmux, &session, s), locks))
    };

    // Accept Claude's folder trust for a declared cwd, and make every locked
    // member prove its gate is active, or stop it.
    for s in started
        .iter()
        .filter(|s| s.locked || s.trust_dir.is_some() || s.watch_codex_trust)
    {
        let id = &s.id;
        let deadline = Instant::now() + gate_wait();
        let mut trusted = false;
        loop {
            if s.locked && state.gate_marker(id).exists() {
                break;
            }
            let Some(w) = tmux.window(&session, id) else {
                set(&mut outcomes, id, "failed: its window disappeared".into());
                failed = true;
                break;
            };
            if launched(&tmux, &session, s) == Launched::Replaced {
                set(&mut outcomes, id, REPLACED.to_string());
                // Its own gate never reported in to this start.
                failed |= s.locked;
                break;
            }
            let pane = s
                .pane
                .as_ref()
                .map(|p| p.pane_id.clone())
                .unwrap_or_default();
            if w.dead {
                set(
                    &mut outcomes,
                    id,
                    format!(
                        "failed: exited during start (status {})",
                        w.dead_status.unwrap_or(-1)
                    ),
                );
                failed = true;
                break;
            }
            let screen = tmux.capture(&session, id);
            let reading = rules::load(s.vendor)
                .map(|r| r.read(&w.title, &screen))
                .ok();
            let is_trust = reading.as_ref().and_then(|r| r.rule.as_deref()) == Some(TRUST_RULE);
            let accept = is_trust
                && !trusted
                && s.trust_dir.is_some()
                && trust_dialog_path(&screen).as_deref() == s.trust_dir.as_deref()
                && has_line(&screen, TRUST_NO_SELECTED);
            if accept {
                let dir = s
                    .trust_dir
                    .as_ref()
                    .map(|d| d.display().to_string())
                    .unwrap_or_default();
                // Move to "Yes, I trust this folder", confirm it is now the
                // highlighted choice on the same dialog, and only then press
                // Enter. Nothing else is ever answered.
                let (now, _locks) = relock(s)?;
                if now != Launched::Same {
                    append(
                        &mut outcomes,
                        id,
                        format!(
                            "couldn't confirm it is still the member this start started, so the folder-trust dialog was left for you; run `brg attach {}`",
                            inst.at(id)
                        ),
                    );
                    failed |= s.locked;
                    break;
                }
                // Keys would go to copy mode, not the dialog: read the very
                // pane they go to, with the locks held. Wait for it to leave
                // copy mode, and answer then.
                match tmux.pane_in_mode(&pane) {
                    Some(false) => {}
                    Some(true) if Instant::now() > deadline => {
                        append(
                            &mut outcomes,
                            id,
                            format!(
                                "waiting at Claude's folder-trust prompt: {}; {}, then answer it",
                                tmux::IN_MODE,
                                tmux::leave_mode_hint(&inst.at(id))
                            ),
                        );
                        break;
                    }
                    Some(true) => {
                        std::thread::sleep(Duration::from_millis(200));
                        continue;
                    }
                    None => {
                        append(
                            &mut outcomes,
                            id,
                            format!(
                                "couldn't read its pane, so the folder-trust dialog was left for you; run `brg attach {}`",
                                inst.at(id)
                            ),
                        );
                        break;
                    }
                }
                let t = pane.as_str();
                tmux.ok(&["send-keys", "-t", t, "Down"])?;
                std::thread::sleep(Duration::from_millis(250));
                let after = tmux.capture(&session, id);
                if tmux.pane_in_mode(t) != Some(false)
                    || !(has_line(&after, TRUST_YES_SELECTED)
                        && trust_dialog_path(&after).as_deref() == s.trust_dir.as_deref())
                {
                    append(
                        &mut outcomes,
                        id,
                        format!(
                            "the folder-trust dialog didn't look as expected, so it was left for you: brg attach {}",
                            inst.at(id)
                        ),
                    );
                    break;
                }
                tmux.ok(&["send-keys", "-t", t, "Enter"])?;
                let _ = write_private(
                    &state.member_dir(id).join("trust-accepted"),
                    format!("{dir}\n").as_bytes(),
                );
                append(
                    &mut outcomes,
                    id,
                    format!("accepted folder trust for {dir}"),
                );
                trusted = true;
                std::thread::sleep(Duration::from_millis(300));
                continue;
            }
            match reading.map(|r| r.state) {
                // Left for a person, never answered here; a `locked` member's
                // gate must still report in afterwards (`send` checks).
                Some(State::Blocked) if is_trust => {
                    append(
                        &mut outcomes,
                        id,
                        format!(
                            "waiting at {}'s folder-trust prompt; answer it with `brg attach {}`",
                            s.vendor.display_name(),
                            inst.at(id)
                        ),
                    );
                    break;
                }
                Some(State::Blocked) if s.locked => {
                    let (now, _locks) = relock(s)?;
                    if now == Launched::Replaced {
                        set(&mut outcomes, id, REPLACED.to_string());
                        failed = true;
                        break;
                    }
                    // Its own pane, or one that can't be read: stop it, so a
                    // `locked` member never runs without its gate.
                    let _ = match now {
                        Launched::Same => tmux.ok(&["kill-window", "-t", &pane]).map(|_| ()),
                        _ => tmux.kill_window(&session, id),
                    };
                    set(
                        &mut outcomes,
                        id,
                        "stopped: it was at a prompt before its `locked` gate reported in"
                            .to_string(),
                    );
                    failed = true;
                    break;
                }
                Some(State::Blocked) => {
                    append(
                        &mut outcomes,
                        id,
                        format!(
                            "waiting at a prompt; answer it with `brg attach {}`",
                            inst.at(id)
                        ),
                    );
                    break;
                }
                Some(State::Idle) | Some(State::Working) if !s.locked => break,
                _ => {}
            }
            if Instant::now() > deadline {
                if s.locked {
                    let (now, _locks) = relock(s)?;
                    if now == Launched::Replaced {
                        set(&mut outcomes, id, REPLACED.to_string());
                        failed = true;
                        break;
                    }
                    // Its own pane, or one that can't be read: stop it, so a
                    // `locked` member never runs without its gate.
                    let _ = match now {
                        Launched::Same => tmux.ok(&["kill-window", "-t", &pane]).map(|_| ()),
                        _ => tmux.kill_window(&session, id),
                    };
                    set(
                        &mut outcomes,
                        id,
                        "stopped: its `locked` gate did not report in, so it would be unprotected"
                            .to_string(),
                    );
                    failed = true;
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    let nodes = member_nodes(inst, v, |id| outcomes.get(id).cloned().unwrap_or_default());
    let plugins: Vec<(String, Vec<String>)> = plugin_outcomes
        .into_iter()
        .map(|(name, o)| (name, vec![o]))
        .collect();
    Ok(UpOutcome {
        tree: tree::render(
            &header(inst),
            &nodes,
            &plugins,
            std::io::stdout().is_terminal(),
        ),
        notes,
        failed,
    })
}

/// The first line of an instance's tree: its ID, team folder and working
/// folder.
pub fn header(inst: &Instance) -> String {
    format!(
        "{}  team {} · folder {}",
        inst.id,
        inst.record.team_dir.display(),
        inst.record.folder.display()
    )
}

/// What `status` and a start show for a role with no member yet.
fn not_created(role: &crate::team::Role) -> String {
    if role.max > 1 {
        ON_DEMAND.into()
    } else {
        format!("not created (start: {})", role.start)
    }
}

/// One node per member of the instance (each under its parent role's
/// member), and one per role with no member yet. `outcome` gives a member's
/// last column.
fn member_nodes(
    inst: &Instance,
    v: &Validated,
    outcome: impl Fn(&str) -> String,
) -> Vec<tree::Node> {
    let parent_of = |role: &crate::team::Role| {
        role.parent.as_ref().map(|p| {
            inst.record
                .members_of(p)
                .into_iter()
                .next()
                .unwrap_or_else(|| p.clone())
        })
    };
    let mut nodes = Vec::new();
    for (role_name, role) in &v.team.roles {
        let ids = inst.record.members_of(role_name);
        if ids.is_empty() {
            nodes.push(tree::Node {
                id: role_name.clone(),
                parent: parent_of(role),
                cols: vec![not_created(role)],
                dim: role.max > 1,
            });
        }
        for id in ids {
            nodes.push(tree::Node {
                cols: vec![tree::vendor_model(role), outcome(&id)],
                id,
                parent: parent_of(role),
                dim: false,
            });
        }
    }
    nodes
}

/// The folders every `vendor`-posture member may write: the instance's
/// `messages/` and `notes/`, never brg's own files. Each must be a real
/// folder (not a symlink), yours, and not writable by group or others, and
/// resolve to exactly its own place in the instance's state folder: anything
/// else, or any error finding out, refuses the start.
fn shared_folders(state: &StateDir) -> Result<Vec<PathBuf>, String> {
    let refuse = |dir: &Path, why: String| {
        format!(
            "the instance's {} folder {} {why}, so members can't be given it; nothing was started",
            dir.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            dir.display()
        )
    };
    let root = state
        .root
        .canonicalize()
        .map_err(|e| format!("cannot resolve {}: {e}", state.root.display()))?;
    let mut out = Vec::new();
    for dir in [state.messages_dir(), state.notes_dir()] {
        crate::bus::fsx::check_private_dir(&dir).map_err(|e| {
            let text = e.to_string();
            let why = text
                .strip_prefix(&format!("{} ", dir.display()))
                .unwrap_or(&text)
                .to_string();
            refuse(&dir, why)
        })?;
        let real = dir
            .canonicalize()
            .map_err(|e| refuse(&dir, format!("can't be resolved ({e})")))?;
        let want = root.join(dir.file_name().unwrap_or_default());
        if real != want {
            return Err(refuse(&dir, format!("resolves to {}", real.display())));
        }
        out.push(real);
    }
    Ok(out)
}

/// What `auto_trust: true` will load in a `vendor` member's folder: its
/// project settings, which brg's trust lets the vendor read.
fn project_settings_note(v: &Validated, member: &crate::team::Role, cwd: &Path) -> Option<String> {
    if v.config.auto_trust != Some(true) || v.posture(&member.id) != Posture::Vendor {
        return None;
    }
    match member.vendor {
        Vendor::Claude if cwd.join(".claude").exists() || cwd.join(".mcp.json").exists() => {
            Some(format!(
                "`auto_trust: true` trusts {}, which has project settings (`.claude/` or `.mcp.json`) that will load in `vendor` posture",
                cwd.display()
            ))
        }
        Vendor::Codex
            if cwd.join(".codex").exists() && !launch::codex_args_set(&member.args, "projects") =>
        {
            Some(format!(
                "`auto_trust: true` trusts {}, which has project settings (`.codex/`): its config, hooks and MCP servers will load",
                cwd.display()
            ))
        }
        _ => None,
    }
}

/// Members get `BORGCHART_TMUX_TMPDIR` in their environment and run in
/// their own folders, so it must be absolute.
fn check_tmux_tmpdir() -> Result<(), String> {
    match std::env::var_os(state::ENV_TMUX_TMPDIR) {
        Some(dir) if !Path::new(&dir).is_absolute() => Err(format!(
            "{} must be an absolute path; nothing was started",
            state::ENV_TMUX_TMPDIR
        )),
        _ => Ok(()),
    }
}

/// Refuse a member folder that is the same as, inside or around brg's state
/// folder or the instance's own: Codex makes a member's folder writable.
fn check_state_overlap(folder: &Path, instance_state: &Path) -> Result<(), String> {
    for (what, dir) in [
        ("brg's state folder", state::state_home()),
        ("the instance's state folder", instance_state.to_path_buf()),
    ] {
        if overlaps(folder, &dir) {
            return Err(format!(
                "folder {} is the same as, inside or around {what} {}, so members could change brg's own files",
                folder.display(),
                dir.display()
            ));
        }
    }
    Ok(())
}

/// Refuse every member start while a tmux session from before instances
/// (any `brg-*` session that isn't a registered instance's) is alive on
/// brg's server: brg can't tell which folders it uses.
fn refuse_legacy_sessions(tmux: &Tmux) -> Result<(), String> {
    let Ok(out) = tmux.ok(&["list-sessions", "-F", "#{session_name}"]) else {
        return Ok(());
    };
    let registry = registry::load()?;
    let old: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|s| {
            s.strip_prefix("brg-")
                .is_some_and(|id| !registry.instances.contains_key(id))
        })
        .collect();
    if old.is_empty() {
        return Ok(());
    }
    Err(format!(
        "tmux sessions from before instances are still running on brg's server ({}); brg can't tell which folders they use, so it starts nothing until they stop. Stop them with the brg that started them",
        old.join(", ")
    ))
}

/// Validate a team folder, printing what validation found; `Err` on errors.
fn validate_printing(dir: &Path) -> Result<Validated, String> {
    let (report, validated) = validate(dir);
    for diag in &report.diags {
        eprintln!("{diag}");
    }
    validated.ok_or_else(|| "validation failed; nothing was started".to_string())
}

/// Checks a start would fail on, made before anything is registered: the
/// secrets the plugins need (unless the instance runs, and so has its
/// values already) and, with `auth: api_key`, the starting roles' keys.
fn precheck(v: &Validated, roles: &[&crate::team::Role], secrets: bool) -> Result<(), String> {
    let (_, missing) = plugin_values(v);
    if secrets && !missing.is_empty() {
        return Err(format!(
            "set these secrets in your environment first (brg stores none); nothing was created: {}",
            missing.join(", ")
        ));
    }
    if v.auth() == Auth::ApiKey {
        let missing: Vec<String> = roles
            .iter()
            .map(|r| key_name(r.vendor))
            .filter(|k| std::env::var_os(k).is_none())
            .map(str::to_string)
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "`auth: api_key` needs these in your environment; nothing was created: {}",
                missing.join(", ")
            ));
        }
    }
    Ok(())
}

/// `brg create <team> <folder>`: a new instance of the instance working in
/// `folder`, with its `start: true` roles' members, started.
pub fn create(team_arg: &str, folder: &str) -> Result<UpOutcome, String> {
    let mut report = crate::diag::Report::default();
    let config = crate::config::load(&mut report).ok_or_else(|| {
        report
            .errors()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    })?;
    let dir = crate::team::find(team_arg, &config.teams)?;
    let v = validate_printing(&dir)?;
    let folder_path = match folder.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest),
        None => PathBuf::from(folder),
    };
    let folder = folder_path
        .canonicalize()
        .ok()
        .filter(|p| p.is_dir())
        .ok_or_else(|| {
            format!(
                "the working folder {} does not exist; create it first",
                folder_path.display()
            )
        })?;
    state::check_state_home()?;
    let starting: Vec<&crate::team::Role> = v.team.roles.values().filter(|m| m.start).collect();
    check_tmux_tmpdir()?;
    precheck(&v, &starting, true)?;
    check_state_overlap(&folder, &state::state_home())
        .map_err(|e| format!("the working {e}; nothing was created"))?;
    refuse_legacy_sessions(&Tmux::new(
        &state::state_home().join("tmux.conf"),
        base_env(),
    ))?;
    let lock = registry::lock()?;
    crate::checkouts::check_folder(&lock, &folder)?;
    let (state, mut record) =
        registry::create(&lock, &v.team.name, &v.team.dir, &folder, &utc_now())?;
    for role in &starting {
        record.issue_member(&role.id, |id| v.team.roles.contains_key(id))?;
    }
    lock.save_instance(&state, &record)?;
    let ids: Vec<String> = record.members.keys().cloned().collect();
    let inst = Instance {
        id: state.id.clone(),
        state,
        record,
    };
    let mut created_folders = Vec::new();
    let result = (|| {
        for id in &ids {
            let role = &v.team.roles[inst.record.role_of(id).expect("issued role")];
            if let Some(folder) = create_copy_folder(&inst, role, id)? {
                created_folders.push((id.clone(), folder));
            }
        }
        start_members(lock, &inst, &v, ids, true)
    })();
    if result.is_err() {
        unregister_if_never_started(&inst);
        remove_unkept_copy_folders(&inst, created_folders);
    }
    result
}

/// After a `create` whose start failed before any window could start:
/// remove the new instance again (its suffix stays issued). Decided under
/// the registry lock, from what is true then: another start may have
/// started the instance since this one failed.
fn unregister_if_never_started(inst: &Instance) {
    rollback_pause();
    let Ok(lock) = registry::lock() else { return };
    let Ok(mut registry) = lock.load() else {
        return;
    };
    if !registry.instances.contains_key(&inst.id) {
        return;
    }
    let started = state::read_meta(&inst.state).is_some_and(|m| m.windows_started != Some(false));
    let live = liveness_of(inst).values().any(|l| *l != Liveness::Gone);
    if started || live || is_running(inst) {
        return;
    }
    let _ = std::fs::remove_dir_all(&inst.state.root);
    registry.instances.remove(&inst.id);
    let _ = lock.save(&registry);
}

/// Test builds: `BORGCHART_TEST_ROLLBACK_PAUSE=<path>` makes a failed
/// `create` write `<path>.waiting` and wait (up to 30 s) for `<path>` before
/// it takes its instance back, so a test can start the instance meanwhile.
#[cfg(feature = "test-support")]
fn rollback_pause() {
    let Some(path) = std::env::var_os("BORGCHART_TEST_ROLLBACK_PAUSE").map(PathBuf::from) else {
        return;
    };
    let _ = std::fs::write(path.with_extension("waiting"), "");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(not(feature = "test-support"))]
fn rollback_pause() {}

/// `brg create <instance>/<role>`: a new member of the role, started.
pub fn create_member(mut inst: Instance, role: &str) -> Result<UpOutcome, String> {
    let v = validated(&inst)?;
    if let Some(team_role) = v.team.team_roles.get(role) {
        return crate::nesting::create(inst, &v, role, team_role);
    }
    let Some(member) = v.team.roles.get(role) else {
        return Err(format!(
            "instance `{}`'s team has no role `{role}`; its roles: {}",
            inst.id,
            v.team.roles.keys().cloned().collect::<Vec<_>>().join(", ")
        ));
    };
    precheck(&v, &[member], !is_running(&inst))?;
    let lock = registry::lock()?;
    inst.reload()?;
    check_parent_authority(&inst, role, Some(&v), None)?;
    let existing = inst.record.members_of(role);
    if existing.len() >= member.max {
        if member.max == 1 && existing.len() == 1 {
            return Err(format!(
                "role `{role}` already has its member `{}` (max: 1), running or stopped; start it with `brg start {}`",
                existing[0],
                inst.at(&existing[0])
            ));
        }
        return Err(format!(
            "role `{role}` already has {} kept members (max: {}), running or stopped: {}; delete a stopped member before creating another",
            existing.len(),
            member.max,
            existing.join(", ")
        ));
    }
    let id = inst
        .record
        .issue_member(role, |id| v.team.roles.contains_key(id))?;
    let _ = create_copy_folder(&inst, member, &id)?;
    lock.save_instance(&inst.state, &inst.record)?;
    start_members(lock, &inst, &v, vec![id], false)
}

/// Create only a new member's missing `{member_id}` leaf, after checking its
/// unresolved path. Ancestors must exist; an existing leaf is kept as is.
/// The caller holds the registry lock across the checks and creation.
/// Returns the new leaf so an unkept member's failed start can remove it.
fn create_copy_folder(
    inst: &Instance,
    role: &crate::team::Role,
    id: &str,
) -> Result<Option<PathBuf>, String> {
    check_copy_folder(inst, role, id)?;
    let Some(cwd) = role.cwd.as_deref().filter(|c| {
        Path::new(c)
            .file_name()
            .is_some_and(|name| name == "{member_id}")
    }) else {
        return Ok(None);
    };
    let folder = member_cwd(&inst.record.folder, None, id)?;
    let rel = cwd.replace("{member_id}", id);
    let dir = folder.join(&rel);
    if !resolve_existing_prefix(&dir).starts_with(&folder) {
        return Err(format!(
            "`{id}`'s cwd `{rel}` resolves outside the instance's folder {} (through a symlink)",
            folder.display()
        ));
    }
    match std::fs::create_dir(&dir) {
        Ok(()) => Ok(Some(dir)),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(format!(
            "`{id}`'s cwd {} exists but isn't a folder; remove it or change `cwd`",
            dir.display()
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(format!(
            "cannot create `{id}`'s cwd {}: {e}; its parent folder must already exist",
            dir.display()
        )),
        Err(e) => Err(format!("cannot create `{id}`'s cwd {}: {e}", dir.display())),
    }
}

fn remove_unkept_copy_folders(inst: &Instance, created_folders: Vec<(String, PathBuf)>) {
    // A failed start may already have saved members or launched them.
    let registered = match registry::load() {
        Ok(registry) => registry.instances.contains_key(&inst.id),
        Err(_) => return,
    };
    let saved = if registered {
        match registry::read_instance(&inst.state) {
            Ok(record) => Some(record),
            Err(_) if matches!(inst.state.instance_file().try_exists(), Ok(false)) => None,
            Err(_) => return,
        }
    } else {
        None
    };
    for (id, folder) in created_folders {
        if saved
            .as_ref()
            .is_some_and(|record| record.role_of(&id).is_some())
        {
            continue;
        }
        let _ = std::fs::remove_dir(folder);
    }
}

/// Same-role copies with `{member_id}` keep separate folders until deleted,
/// even while stopped. Resolve existing prefixes and check recorded folders
/// so a missing folder or a moved symlink cannot hide an overlap.
fn check_copy_folder(inst: &Instance, role: &crate::team::Role, id: &str) -> Result<(), String> {
    let Some(cwd) = role.cwd.as_deref().filter(|c| c.contains("{member_id}")) else {
        return Ok(());
    };
    let others: Vec<String> = inst
        .record
        .members_of(&role.id)
        .into_iter()
        .filter(|m| m != id)
        .collect();
    if others.is_empty() {
        return Ok(());
    }
    let cwd = inst.record.folder.join(cwd.replace("{member_id}", id));
    for other in others {
        let issued = &inst.record.members[&other];
        let other_cwd = inst.record.folder.join(
            role.cwd
                .as_deref()
                .unwrap_or("")
                .replace("{member_id}", &other),
        );
        if overlaps(&cwd, &other_cwd) || issued.cwd.as_ref().is_some_and(|old| overlaps(&cwd, old))
        {
            return Err(format!(
                "`{id}`'s folder {} overlaps kept member `{other}`'s folder {}; give copies of `{}` separate folders before creating or starting another",
                cwd.display(),
                other_cwd.display(),
                role.id
            ));
        }
    }
    Ok(())
}

/// Parent authority for member lifecycle operations. The registry lock
/// holds both the target's membership and the parent's latest start still.
/// Teams without a bus retain their existing caller rules.
fn check_parent_authority(
    inst: &Instance,
    role: &str,
    validated_team: Option<&Validated>,
    target: Option<&str>,
) -> Result<(), String> {
    let creating = target.is_none();
    let has_member_context = [context::ENV_MEMBER, context::ENV_INCARNATION]
        .iter()
        .any(|key| std::env::var(key).is_ok_and(|value| !value.is_empty()));
    if !has_member_context && !creating {
        return Ok(());
    }
    let recorded = target.and_then(|id| state::read_meta(&inst.state)?.members.get(id).cloned());
    let parent = if let Some(recorded) = recorded {
        // The store records whether the instance started with a bus. A
        // damaged bus must not make a caller a person or prevent stopping.
        if crate::bus::store::open(&inst.state)
            .is_err_and(|f| f.code == crate::bus::outcome::Code::NotConfigured)
        {
            return Ok(());
        }
        recorded.parent
    } else {
        let loaded;
        let v = match validated_team {
            Some(v) => v,
            None => {
                loaded = validated(inst)?;
                &loaded
            }
        };
        if v.team.bus.is_none() {
            return Ok(());
        }
        let definition = v.team.roles.get(role).ok_or_else(|| {
            format!(
                "role `{role}` is no longer in the team; ask a person to stop or delete its members"
            )
        })?;
        let parent_role = definition.parent.as_deref();
        let parent = parent_role.and_then(|p| inst.record.members_of(p).into_iter().next());
        if creating
            && parent.is_none()
            && let Some(parent_role) = parent_role
        {
            return Err(format!(
                "role `{role}`'s parent role `{}` has no member; create it first: brg create {}",
                parent_role,
                inst.at(parent_role)
            ));
        }
        parent
    };
    let caller = context::caller(None).map_err(|e| e.message)?;
    let context::Caller::Member {
        member,
        incarnation,
    } = caller
    else {
        return Ok(());
    };
    context::instance(Some(&inst.id)).map_err(|e| e.message)?;
    if parent.as_deref() != Some(member.as_str()) {
        return Err(format!(
            "only a person or role `{role}`'s parent member may manage its members; this caller is `{member}`"
        ));
    }
    let current = lifecycle::load_incarnation(&inst.state, &member)?;
    if !current.is_some_and(|inc| {
        inc.member == member && inc.incarnation == incarnation && inc.ended_at.is_none()
    }) {
        return Err(format!(
            "`{member}`'s start was replaced or stopped; only its latest running start may manage role `{role}`"
        ));
    }
    Ok(())
}

/// Whether any member or plugin process of the instance has a live window
/// on the server its start recorded.
pub fn is_running(inst: &Instance) -> bool {
    let tmux = tmux_for(&inst.state);
    tmux.windows(&inst.state.session()).iter().any(|w| !w.dead)
}

/// `brg start <instance> --team <path>`: point a stopped instance at its
/// team folder's new place.
fn repoint(inst: &mut Instance, team: &Path) -> Result<(), String> {
    let dir = team
        .canonicalize()
        .map_err(|_| format!("{} does not exist", team.display()))?;
    if !dir.join(crate::team::TEAM_FILE).is_file() {
        return Err(format!(
            "{} is not a team folder (no {} in it)",
            dir.display(),
            crate::team::TEAM_FILE
        ));
    }
    let name = crate::team::team_name(&dir);
    if name != inst.record.team {
        return Err(format!(
            "{} is team `{name}`, not instance `{}`'s team `{}`",
            dir.display(),
            inst.id,
            inst.record.team
        ));
    }
    let lock = registry::lock()?;
    inst.reload()?;
    if is_running(inst) {
        return Err(format!(
            "instance `{}` is running; a new team folder applies only from a full stop: brg stop {}",
            inst.id, inst.id
        ));
    }
    let mut registry = lock.load()?;
    if let Some(entry) = registry.instances.get_mut(&inst.id) {
        entry.team_dir = dir.clone();
    }
    inst.record.team_dir = dir;
    lock.save_instance(&inst.state, &inst.record)?;
    lock.save(&registry)
}

/// Which members a full start brings up: the `start: true` roles' members
/// (issuing one for a role that has none), the members running at the last
/// stop. Called with the registry lock held; new IDs are saved only after
/// all start checks pass.
fn full_start_members(
    _lock: &registry::Lock,
    inst: &mut Instance,
    v: &Validated,
    created_folders: &mut Vec<(String, PathBuf)>,
) -> Result<Vec<String>, String> {
    let mut ids = std::collections::BTreeSet::new();
    for role in v.team.roles.values().filter(|m| m.start) {
        let members = inst.record.members_of(&role.id);
        if members.is_empty() {
            let id = inst
                .record
                .issue_member(&role.id, |id| v.team.roles.contains_key(id))?;
            if let Some(folder) = create_copy_folder(inst, role, &id)? {
                created_folders.push((id.clone(), folder));
            }
            ids.insert(id);
        }
        ids.extend(members);
    }
    if let Some(set) = &inst.record.running_at_stop {
        ids.extend(set.iter().cloned());
    }
    // Running members are left alone, and shown as running.
    let windows = tmux_for(&inst.state).windows(&inst.state.session());
    ids.extend(
        windows
            .iter()
            .filter(|w| !w.dead && inst.record.members.contains_key(&w.name))
            .map(|w| w.name.clone()),
    );
    // Only members whose role is still in the instance.
    Ok(ids
        .into_iter()
        .filter(|id| {
            inst.record
                .role_of(id)
                .is_some_and(|r| v.team.roles.contains_key(r))
        })
        .collect())
}

/// Why a start refuses an instance whose messages can't be read.
fn unreadable_store(inst: &Instance, e: &str) -> String {
    format!(
        "instance `{}`'s messages can't be read ({e}), so nothing was started; create a new instance instead (`brg create`), and send its open work again",
        inst.id
    )
}

/// `brg start <instance>[/<member>] [--team <path>]`.
pub fn start(
    mut inst: Instance,
    member: Option<&str>,
    team: Option<&Path>,
) -> Result<UpOutcome, String> {
    if let Some(dir) = team {
        if member.is_some() {
            return Err(
                "`--team` points the whole instance at a team folder; leave the member out".into(),
            );
        }
        repoint(&mut inst, dir)?;
    }
    let member = member.map(|m| address::member(&inst, m)).transpose()?;
    if !inst.record.team_dir.join(crate::team::TEAM_FILE).is_file() {
        return Err(format!(
            "instance `{}`'s team folder {} is gone; if it moved, point the instance at it: brg start {} --team <path>",
            inst.id,
            inst.record.team_dir.display(),
            inst.id
        ));
    }
    if !inst.record.folder.is_dir() {
        return Err(format!(
            "instance `{}`'s working folder {} is gone; create a new instance (`brg create`) for another folder",
            inst.id,
            inst.record.folder.display()
        ));
    }
    let v = validate_printing(&inst.record.team_dir)?;
    let lock = registry::lock()?;
    inst.reload()?;
    if let Some(id) = &member {
        address::member(&inst, id)?;
        check_parent_authority(&inst, inst.record.role_of(id).unwrap(), Some(&v), Some(id))?;
    }
    match member {
        Some(id) => start_members(lock, &inst, &v, vec![id], false),
        None => {
            let mut created_folders = Vec::new();
            let result = (|| {
                let ids = full_start_members(&lock, &mut inst, &v, &mut created_folders)?;
                start_members(lock, &inst, &v, ids, true)
            })();
            if result.is_err() {
                remove_unkept_copy_folders(&inst, created_folders);
            }
            result
        }
    }
}

/// Whether each member's current start may still be running, as `stop`
/// and liveness see it: `Gone` only when that is certain.
pub fn liveness_of(inst: &Instance) -> BTreeMap<String, Liveness> {
    let state = &inst.state;
    let session = state.session();
    let meta = state::read_meta(state);
    let rows = meta.as_ref().map(|m| lifecycle::Server::of(m).panes());
    let mut out = BTreeMap::new();
    for (id, _) in inst.record.kept_members() {
        let by_window = match (&meta, &rows) {
            // Never started a window here.
            (None, _) => Liveness::Gone,
            (Some(_), Some(Ok(rows))) => {
                if rows
                    .iter()
                    .any(|r| r.session == session && &r.window == id && !r.dead)
                {
                    Liveness::Alive
                } else {
                    Liveness::Gone
                }
            }
            (Some(m), _) if m.stopped_at.is_some() || m.windows_started == Some(false) => {
                Liveness::Gone
            }
            _ => Liveness::Unknown,
        };
        let by_record = match lifecycle::load_incarnation(state, id) {
            Ok(None) => Liveness::Gone,
            Ok(Some(inc)) => lifecycle::liveness(state, id, &inc.incarnation),
            Err(_) => Liveness::Unknown,
        };
        let l = match (by_window, by_record) {
            (Liveness::Alive, _) | (_, Liveness::Alive) => Liveness::Alive,
            (Liveness::Gone, Liveness::Gone) => Liveness::Gone,
            _ => Liveness::Unknown,
        };
        out.insert(id.clone(), l);
    }
    out
}

/// `brg delete <instance>/<member>`: discard a stopped member's live state.
/// Its issued ID and immutable messages stay as history. Registry, lifecycle
/// and publication locks cover the unread check and removal from routing.
pub fn delete_member(mut inst: Instance, name: &str, force: bool) -> Result<Vec<String>, String> {
    let lock = registry::lock()?;
    let _state_lock = lifecycle::lock_state(&inst.state)?;
    inst.reload()?;
    let id = address::member(&inst, name)?;
    check_parent_authority(&inst, inst.record.role_of(&id).unwrap(), None, Some(&id))?;
    if tmux_for(&inst.state)
        .window(&inst.state.session(), &id)
        .is_some_and(|w| !w.dead)
    {
        return Err(format!(
            "member `{}` is running; stop it first: brg stop {}",
            inst.at(&id),
            inst.at(&id)
        ));
    }
    match liveness_of(&inst).get(&id) {
        Some(Liveness::Gone) => {}
        Some(Liveness::Alive) => {
            return Err(format!(
                "member `{}` is running; stop it first: brg stop {}",
                inst.at(&id),
                inst.at(&id)
            ));
        }
        _ => {
            return Err(format!(
                "brg can't tell that member `{}` stopped, so it can't be deleted, even with --force; stop it again once its tmux server can be reached: brg stop {}",
                inst.at(&id),
                inst.at(&id)
            ));
        }
    }
    let mut children = BTreeSet::new();
    let mut team_loaded = false;
    if let Ok(v) = validated(&inst) {
        team_loaded = true;
        for node in member_nodes(&inst, &v, |_| String::new()) {
            if node.parent.as_deref() == Some(&id)
                && inst
                    .record
                    .members
                    .get(&node.id)
                    .is_some_and(|m| !m.deleted)
            {
                children.insert(node.id);
            }
        }
    }
    let meta = state::read_meta(&inst.state);
    if !team_loaded
        && meta.as_ref().is_none_or(|meta| {
            inst.record
                .kept_members()
                .any(|(other, _)| other != &id && !meta.members.contains_key(other))
        })
    {
        return Err(format!(
            "brg can't tell whether member `{}` has kept children, so it can't be deleted, even with --force; restore its team and start the instance: brg start {} --team <path>",
            inst.at(&id),
            inst.id
        ));
    }
    let mut recorded_only = false;
    if let Some(meta) = meta {
        for (child, _) in inst.record.kept_members() {
            if meta.members.get(child).and_then(|m| m.parent.as_deref()) == Some(&id) {
                recorded_only |= children.insert(child.clone());
            }
        }
    }
    if !children.is_empty() {
        return Err(format!(
            "member `{}` has kept children; delete them first:\n  {}{}",
            inst.at(&id),
            children
                .iter()
                .map(|child| inst.at(child))
                .collect::<Vec<_>>()
                .join("\n  "),
            if recorded_only {
                format!(
                    "\nthe team file changed since the last start; brg start {} applies it",
                    inst.id
                )
            } else {
                String::new()
            }
        ));
    }
    // No bus means no publication lock to create; a stray lock would make
    // an otherwise empty messages folder look damaged on its next start.
    let no_bus = crate::bus::store::open(&inst.state)
        .is_err_and(|f| f.code == crate::bus::outcome::Code::NotConfigured);
    let _bus_lock = if no_bus {
        None
    } else {
        Some(lifecycle::lock_bus(&inst.state)?)
    };
    let work = (|| {
        use crate::bus::{outcome::Code, store};
        let bus = match store::open(&inst.state) {
            Ok(bus) => bus,
            Err(f) if f.code == Code::NotConfigured => return Ok(lifecycle::OpenWork::default()),
            Err(f) => return Err(f.message),
        };
        let snap = store::scan(&bus.paths);
        if !snap.healthy() {
            return Err("messages are incomplete".into());
        }
        Ok(lifecycle::unacked_work(&bus, &snap, Some(&id)))
    })();
    match work {
        Ok(work) if work.items.is_empty() || force => {}
        Ok(work) => {
            return Err(format!(
                "member `{}` holds {} unread message{}; settle them, or delete it anyway with --force:\n  {}",
                inst.at(&id),
                work.items.len(),
                if work.items.len() == 1 { "" } else { "s" },
                work.items.join("\n  ")
            ));
        }
        Err(_) if force => {}
        Err(e) => {
            return Err(format!(
                "member `{}`'s messages can't be read ({e}), so its contents can't be inspected; delete it anyway with --force",
                inst.at(&id)
            ));
        }
    }
    let issued = inst.record.members.get_mut(&id).expect("resolved member");
    issued.deleted = true;
    issued.cwd = None;
    if let Some(running) = &mut inst.record.running_at_stop {
        running.remove(&id);
    }
    // Save the tombstone first: even a partial cleanup cannot restore routing.
    lock.save_instance(&inst.state, &inst.record)?;
    if let Some(mut meta) = state::read_meta(&inst.state) {
        meta.members.remove(&id);
        write_private(
            &inst.state.meta_file(),
            &serde_json::to_vec_pretty(&meta).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("cannot write state: {e}"))?;
    }
    for dir in [inst.state.member_dir(&id), inst.state.member_tmp(&id)] {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(format!(
                    "member `{id}` was deleted, but its state could not be removed: {e}"
                ));
            }
        }
    }
    let _ = std::fs::remove_file(inst.state.exec_file(&id));
    let _ = std::fs::remove_file(inst.state.lock_file(&id));
    Ok(vec![format!(
        "deleted {}; its ID stays issued",
        inst.at(&id)
    )])
}

/// `brg delete <instance> [--force]`: remove a stopped instance's state
/// folder (messages, notes, brg's files) and its registration. Its working
/// folder is never touched.
pub fn delete(inst: Instance, force: bool) -> Result<Vec<String>, String> {
    let lock = registry::lock()?;
    let state_lock = lifecycle::lock_state(&inst.state)?;
    let copies = crate::nesting::descendants(&inst)?;
    if !copies.is_empty() {
        return Err(format!(
            "instance `{}` has kept copies; delete them first:\n  {}",
            inst.id,
            copies
                .iter()
                .map(|copy| copy.id.as_str())
                .collect::<Vec<_>>()
                .join("\n  ")
        ));
    }
    let live = liveness_of(&inst);
    let alive: Vec<&String> = live
        .iter()
        .filter(|(_, l)| **l == Liveness::Alive)
        .map(|(id, _)| id)
        .collect();
    if !alive.is_empty() || is_running(&inst) {
        return Err(format!(
            "instance `{}` is running{}; stop it first: brg stop {}",
            inst.id,
            if alive.is_empty() {
                String::new()
            } else {
                format!(
                    " ({})",
                    alive
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            },
            inst.id
        ));
    }
    let unknown: Vec<&String> = live
        .iter()
        .filter(|(_, l)| **l == Liveness::Unknown)
        .map(|(id, _)| id)
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "brg can't tell that {} of instance `{}` stopped, so it can't be deleted, even with --force; stop it again once its tmux server can be reached: brg stop {}",
            unknown
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            inst.id,
            inst.id
        ));
    }
    match lifecycle::open_work(&inst.state) {
        Ok(work) if work.items.is_empty() || force => {}
        Ok(work) => {
            return Err(format!(
                "instance `{}` holds open work; settle it, or delete it anyway with --force:\n  {}",
                inst.id,
                work.items.join("\n  ")
            ));
        }
        Err(_) if force => {}
        Err(e) => {
            return Err(format!(
                "instance `{}`'s messages can't be read ({e}), so its contents can't be inspected; delete it anyway with --force",
                inst.id
            ));
        }
    }
    drop(state_lock);
    std::fs::remove_dir_all(&inst.state.root)
        .map_err(|e| format!("cannot remove {}: {e}", inst.state.root.display()))?;
    let mut registry = lock.load()?;
    registry.instances.remove(&inst.id);
    if let Some((parent_id, copy_id)) = inst.id.rsplit_once('/') {
        let parent_state = StateDir::new(parent_id);
        let mut parent = registry::read_instance(&parent_state)?;
        if let Some(copy) = parent.copies.get_mut(copy_id) {
            copy.deleted = true;
            copy.cwd = None;
        }
        lock.save_instance(&parent_state, &parent)?;
    }
    lock.save(&registry)?;
    Ok(vec![format!(
        "deleted {}; its working folder {} was left as it is",
        inst.id,
        inst.record.folder.display()
    )])
}

/// `brg secret <team>`: each secret its plugins need (plugin, name), and
/// whether the caller's environment sets it. No value is read out.
pub fn secrets(team: &str) -> Result<Vec<(String, String, bool)>, String> {
    let mut report = crate::diag::Report::default();
    let config = crate::config::load(&mut report).ok_or_else(|| {
        report
            .errors()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    })?;
    let dir = crate::team::find(team, &config.teams)?;
    let (report, validated) = crate::validate::validate_with(&dir, config);
    let v = validated.ok_or_else(|| {
        report
            .errors()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    })?;
    Ok(v.team
        .plugins
        .values()
        .flat_map(|p| {
            p.secrets()
                .map(|n| (p.name.clone(), n.clone(), std::env::var_os(n).is_some()))
                .collect::<Vec<_>>()
        })
        .collect())
}

/// One team in `brg ls`, with its instances.
#[derive(Debug, Serialize)]
pub struct Listed {
    /// `None` for instances whose team is outside the search paths.
    pub team: Option<String>,
    pub dir: Option<PathBuf>,
    pub instances: Vec<ListedInstance>,
}

#[derive(Debug, Serialize)]
pub struct ListedInstance {
    pub instance: String,
    pub team: String,
    pub team_dir: PathBuf,
    pub folder: PathBuf,
    pub running: bool,
    pub created_at: String,
}

/// `brg ls`: the teams in the search paths, each with its instances, then
/// instances whose team is elsewhere.
pub fn ls() -> Result<Vec<Listed>, String> {
    let mut report = crate::diag::Report::default();
    let config = crate::config::load(&mut report).ok_or_else(|| {
        report
            .errors()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    })?;
    let registry = registry::load()?;
    let listed = |id: &String, e: &registry::Entry| ListedInstance {
        instance: id.clone(),
        team: e.team.clone(),
        team_dir: e.team_dir.clone(),
        folder: e.folder.clone(),
        running: address::instance(id).is_ok_and(|i| is_running(&i)),
        created_at: e.created_at.clone(),
    };
    let mut out = Vec::new();
    let mut shown = std::collections::BTreeSet::new();
    for (name, dir) in crate::team::list(&config.teams) {
        let instances = registry
            .instances
            .iter()
            .filter(|(_, e)| e.team_dir == dir)
            .map(|(id, e)| {
                shown.insert(id.clone());
                listed(id, e)
            })
            .collect();
        out.push(Listed {
            team: Some(name),
            dir: Some(dir),
            instances,
        });
    }
    let elsewhere: Vec<ListedInstance> = registry
        .instances
        .iter()
        .filter(|(id, _)| !shown.contains(*id))
        .map(|(id, e)| listed(id, e))
        .collect();
    if !elsewhere.is_empty() {
        out.push(Listed {
            team: None,
            dir: None,
            instances: elsewhere,
        });
    }
    Ok(out)
}

/// `brg ls` as text.
pub fn render_ls(listed: &[Listed]) -> String {
    let mut out = String::new();
    for l in listed {
        match (&l.team, &l.dir) {
            (Some(name), Some(dir)) => out.push_str(&format!("{name}  {}\n", dir.display())),
            _ => out.push_str("instances of teams outside the search paths\n"),
        }
        if l.instances.is_empty() {
            out.push_str("  (no instances)\n");
        }
        for i in &l.instances {
            out.push_str(&format!(
                "  {}  {}  folder {}{}\n",
                i.instance,
                if i.running { "running" } else { "stopped" },
                i.folder.display(),
                if l.team.is_none() {
                    format!("  team {}", i.team_dir.display())
                } else {
                    String::new()
                }
            ));
        }
    }
    if out.is_empty() {
        out.push_str("no teams in the search paths and no instances\n");
    }
    out
}

/// After a window failed to start: when `nothing_started` (a fresh start
/// that created no window of the instance), the instance is as stopped as
/// before, so `stop`, liveness and `delete` aren't left unsure of a server
/// that never ran it.
fn unrecord_windows(state: &StateDir, meta: &mut Meta, nothing_started: bool, before: bool) {
    if !nothing_started {
        return;
    }
    meta.windows_started = Some(before);
    if let Ok(body) = serde_json::to_vec_pretty(meta) {
        let _ = write_private(&state.meta_file(), &body);
    }
}

fn start_window(
    tmux: &Tmux,
    brg: &Path,
    state: &StateDir,
    meta: &mut Meta,
    session: &str,
    window: &str,
    spec: &ExecSpec,
) -> Result<(), String> {
    // Recorded first: once tmux runs, `stop` must not take an unreachable
    // server for one that never started.
    if meta.windows_started != Some(true) {
        meta.windows_started = Some(true);
        write_private(
            &state.meta_file(),
            &serde_json::to_vec_pretty(meta).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("cannot write state: {e}"))?;
    }
    let body = serde_json::to_vec(spec).map_err(|e| e.to_string())?;
    write_private(&state.exec_file(window), &body)
        .map_err(|e| format!("cannot write launch file: {e}"))?;
    let command = format!(
        "{} _exec {} {}",
        shell_quote(&brg.display().to_string()),
        shell_quote(&state.root.display().to_string()),
        shell_quote(window)
    );
    tmux.start_window(session, window, &command)
        .map_err(|e| format!("cannot start `{window}`: {e}"))
}

/// `brg stop <instance>[/<member>]`.
///
/// Works on the tmux server recorded at the start, refusing a caller whose
/// `BORGCHART_TMUX_SOCKET` names another, and changes nothing unless that
/// server answers. A member's incarnation is marked ended, and launch files
/// and secrets removed, only once its pane was stopped or the server showed
/// it gone. A full stop of a running instance records which members were
/// running, for the next `start`; stopping a stopped one changes nothing.
pub fn stop(inst: &Instance, only: Option<&str>) -> Result<Vec<String>, String> {
    let lock = registry::lock()?;
    let mut messages = Vec::new();
    if only.is_none() {
        for copy in crate::nesting::descendants(inst)? {
            messages.extend(
                stop_one(&lock, &copy, None)?
                    .into_iter()
                    .map(|message| format!("{}: {message}", copy.id)),
            );
        }
    }
    let own = stop_one(&lock, inst, only)?;
    if messages.is_empty() {
        return Ok(own);
    }
    messages.extend(
        own.into_iter()
            .map(|message| format!("{}: {message}", inst.id)),
    );
    Ok(messages)
}

fn stop_one(
    lock: &registry::Lock,
    inst: &Instance,
    only: Option<&str>,
) -> Result<Vec<String>, String> {
    let mut current = inst.clone();
    current.reload()?;
    let inst = &current;
    let state = &inst.state;
    let session = state.session();
    let team = inst.id.clone();
    if let Some(id) = only {
        address::member(inst, id)?;
        check_parent_authority(inst, inst.record.role_of(id).unwrap(), None, Some(id))?;
    }
    let _state_lock = lifecycle::lock_state(state)?;
    let Some(mut meta) = state::read_meta(state) else {
        // Nothing was ever recorded here, so brg started nothing to stop.
        return Ok(vec!["not running".into()]);
    };
    let caller_socket = state::tmux_socket();
    if let Some(name) = &meta.socket
        && *name != caller_socket
    {
        return Err(format!(
            "instance `{team}` is on tmux socket `{name}`, not `{caller_socket}`; stop it with BORGCHART_TMUX_SOCKET={name} brg stop {team}. Nothing was changed"
        ));
    }
    let server = lifecycle::Server::of(&meta);
    let rows = match server.panes() {
        Ok(rows) => rows,
        // A stop was already confirmed, or nothing ever started, and
        // nothing has started since.
        Err(_) if meta.stopped_at.is_some() || meta.windows_started == Some(false) => Vec::new(),
        Err(e) => {
            return Err(format!(
                "{e}; nothing was changed. If instance `{team}` is really gone (for example after a reboot), `brg start {team}` and then `brg stop {team}` clear it"
            ));
        }
    };
    let in_session = |r: &&lifecycle::PaneRow| r.session == session;
    let mut messages = Vec::new();
    // The members running now, for the next full start.
    let running: std::collections::BTreeSet<String> = rows
        .iter()
        .filter(in_session)
        .filter(|r| !r.dead && inst.record.members.contains_key(&r.window))
        .map(|r| r.window.clone())
        .collect();
    // The processes under the panes this `stop` stops, found before tmux
    // kills the panes: some may have left the pane's session, and would
    // outlive it. Only live panes in this session on the recorded server,
    // and with `--member`, only those in the one window it kills.
    let target = only.and_then(|id| rows.iter().filter(in_session).find(|r| r.window == id));
    let panes: Vec<u32> = rows
        .iter()
        .filter(in_session)
        .filter(|r| !r.dead)
        .filter(|r| match only {
            Some(_) => target.is_some_and(|t| t.window_id == r.window_id),
            None => true,
        })
        .map(|r| r.pane_pid)
        .collect();
    let procs = if panes.is_empty() {
        Ok(Vec::new())
    } else {
        server
            .identity()
            .and_then(|(socket, pid)| proctree::snapshot(&panes, pid, &socket))
    };
    // Panes that are certainly stopped now: killed by this `stop`, or not on
    // the server that answered.
    let mut killed: Vec<String> = Vec::new();
    let mut whole = false;
    let members: Vec<String> = match only {
        Some(id) => {
            match target {
                Some(w) => {
                    if rows.iter().filter(in_session).count() == 1 {
                        server.act(&["kill-session", "-t", &format!("={session}")])?;
                        whole = true;
                    } else {
                        server.act(&["kill-window", "-t", &w.pane_id])?;
                    }
                    killed.push(w.pane_id.clone());
                    messages.push(format!("{id}: stopped"));
                }
                None => messages.push(format!("{id}: not running")),
            }
            let _ = std::fs::remove_file(state.gate_marker(id));
            let _ = std::fs::remove_file(state.member_dir(id).join("trust-accepted"));
            let _ = std::fs::remove_file(state.exec_file(id));
            vec![id.to_string()]
        }
        None => {
            if rows.iter().any(|r| r.session == session) {
                server.act(&["kill-session", "-t", &format!("={session}")])?;
                killed.extend(rows.iter().filter(in_session).map(|r| r.pane_id.clone()));
                messages.push("stopped".into());
            } else {
                messages.push("not running".into());
            }
            whole = true;
            // Remove the private files that held secrets and launch details.
            let _ = std::fs::remove_dir_all(state.root.join("exec"));
            for name in meta.plugins.keys() {
                let _ = std::fs::remove_file(state.plugin_env_file(name));
            }
            for id in meta.members.keys() {
                let _ = std::fs::remove_file(state.gate_marker(id));
                let _ = std::fs::remove_file(state.member_dir(id).join("trust-accepted"));
            }
            meta.members.keys().cloned().collect()
        }
    };
    // Now the panes are gone, end what ran under them, before anything
    // below can return early.
    match procs.and_then(|p| proctree::end(&p)) {
        Ok(left) if left.is_empty() => {}
        Ok(left) => messages.push(format!(
            "processes {} didn't exit after KILL",
            left.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")
        )),
        Err(e) => messages.push(format!(
            "{e}; processes the stopped members left running in the background, if any, weren't checked"
        )),
    }
    {
        for id in &members {
            let Some(inc) = lifecycle::load_incarnation(state, id).map_err(|_| {
                format!("{team}/{id}'s start record can't be read; nothing more was changed")
            })?
            else {
                continue;
            };
            if inc.ended_at.is_some() {
                continue;
            }
            let stopped = match &inc.pane {
                // Its window, if it ever had one, was in this session.
                None => true,
                Some(p) if lifecycle::Server::from_recorded(&p.socket) == server => {
                    killed.contains(&p.pane_id) || !rows.iter().any(|r| r.pane_id == p.pane_id)
                }
                Some(p) => lifecycle::pane_liveness(p) == Liveness::Gone,
            };
            if stopped {
                lifecycle::end_incarnation(state, id, lifecycle::ENDED_STOPPED)?;
            } else {
                messages.push(format!(
                    "{id}: may still be running on another tmux server; it was left as is"
                ));
            }
        }
    }
    if whole {
        meta.stopped_at = Some(utc_now());
        write_private(
            &state.meta_file(),
            &serde_json::to_vec_pretty(&meta).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("cannot write state: {e}"))?;
    }
    // A full stop of a running instance records who ran; a second stop
    // keeps the first's record. Stopping one member takes it off.
    let mut record = registry::read_instance(state)?;
    let before = record.running_at_stop.clone();
    match only {
        None if !running.is_empty() => record.running_at_stop = Some(running),
        None => {}
        Some(id) => {
            if let Some(set) = record.running_at_stop.as_mut() {
                set.remove(id);
            }
        }
    }
    if record.running_at_stop != before {
        lock.save_instance(state, &record)?;
    }
    Ok(messages)
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusRow {
    /// The local member or copy ID, a role with nothing created yet, or
    /// a plugin's name.
    pub name: String,
    /// `member`, `role` (no member or copy yet), `copy` or `plugin`.
    pub kind: &'static str,
    pub instance: String,
    /// The instance's name.
    pub team: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// The current start's incarnation ID, retained after a stop.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub vendor: Option<String>,
    /// `"<model>"` or `"<model> <effort>"`, as declared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// A local parent member ID, a copy's full parent address, or the
    /// instance path that a nested root belongs to. Rows are in tree order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub posture: Option<String>,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor_version: Option<String>,
    /// The folder a start accepted Claude's trust dialog for, if it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust_accepted: Option<String>,
    /// With a bus: whether the member's current incarnation may still be
    /// running (`alive`, `gone` or `unknown`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub liveness: Option<Liveness>,
    /// Unacked messages; absent when the instance has no readable bus.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unread: Option<usize>,
    /// A copy's running members, excluding members of its own copies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running: Option<usize>,
    /// A `start: true` member that ran and is `stopped` while the rest of
    /// the instance runs: it should be running.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub expected_running: bool,
    /// The member's declared `start`.
    #[serde(skip)]
    pub start: bool,
}

/// A member's state when it has never started in the instance.
pub const NOT_STARTED: &str = "not started";
/// A member's state when it ran and has ended.
pub const STOPPED: &str = "stopped";
/// A role that allows copies but has no kept member.
pub const ON_DEMAND: &str = "on demand (none created)";

/// What a start and `status` print for a member that has never run.
pub fn not_started(start: bool) -> String {
    if start {
        NOT_STARTED.into()
    } else {
        format!("{NOT_STARTED} (start: false)")
    }
}

/// What `status` prints in a member's state column.
pub fn state_label(row: &StatusRow) -> String {
    match row.state.as_str() {
        "blocked" if row.rule.as_deref() == Some(TRUST_RULE) => "blocked (folder trust)".into(),
        NOT_STARTED => not_started(row.start),
        STOPPED if row.expected_running => format!("{STOPPED} (should be running)"),
        s => s.to_string(),
    }
}

/// Validate the instance's team folder, as recorded.
fn validated(inst: &Instance) -> Result<Validated, String> {
    if !inst.record.team_dir.join(crate::team::TEAM_FILE).is_file() {
        return Err(format!(
            "instance `{}`'s team folder {} is gone; if it moved, point the instance at it after a full stop: brg start {} --team <path>",
            inst.id,
            inst.record.team_dir.display(),
            inst.id
        ));
    }
    let (report, validated) = validate(&inst.record.team_dir);
    validated.ok_or_else(|| {
        report
            .errors()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    })
}

/// One snapshot of the instance.
pub fn status(inst: &Instance) -> Result<Vec<StatusRow>, String> {
    let v = validated(inst)?;
    let state = &inst.state;
    let session = state.session();
    let tmux = tmux_for(state);
    let windows = tmux.windows(&session);
    let meta = state::read_meta(state).unwrap_or_default();
    let (counts, bus_note) = if v.team.bus.is_none() {
        (None, None)
    } else {
        match crate::bus::store::open(state).and_then(|bus| {
            let snap = crate::bus::store::snapshot(&bus)?;
            if !snap.healthy() {
                return Err(crate::bus::outcome::Failure::new(
                    crate::bus::outcome::Code::Corrupt,
                    "incomplete messages",
                ));
            }
            Ok(crate::bus::store::progress(&bus, &snap))
        }) {
            Ok(counts) => (Some(counts), None),
            Err(_) => (
                None,
                Some("messages unavailable; brg bus check".to_string()),
            ),
        }
    };
    let nodes = member_nodes(inst, &v, |_| String::new());
    // The instance runs while any member has a live window; one stopped on
    // purpose expects no one to be running.
    let team_running = windows
        .iter()
        .any(|w| !w.dead && inst.record.members.contains_key(&w.name));
    let mut rows = Vec::new();
    for (i, _) in tree::order(&nodes) {
        let id = &nodes[i].id;
        let Some(role) = inst.record.role_of(id) else {
            // A role with no member yet.
            let member = &v.team.roles[id];
            rows.push(StatusRow {
                name: id.clone(),
                kind: "role",
                instance: inst.id.clone(),
                team: inst.record.team.clone(),
                member: None,
                start_id: None,
                role: Some(id.clone()),
                vendor: Some(member.vendor.as_str().into()),
                model: member.model.as_ref().map(|m| match &m.effort {
                    Some(e) => format!("{} {e}", m.name),
                    None => m.name.clone(),
                }),
                parent: nodes[i].parent.clone(),
                posture: Some(match member.posture {
                    Posture::Vendor => "vendor".into(),
                    Posture::Locked => "locked".into(),
                }),
                state: not_created(member),
                rule: None,
                detail: None,
                vendor_version: None,
                trust_accepted: None,
                liveness: None,
                unread: None,
                running: None,
                expected_running: false,
                start: member.start,
            });
            continue;
        };
        let member = &v.team.roles[role];
        let posture = v.posture(role);
        // A current start record survives full stops and configuration changes.
        let incarnation = lifecycle::load_incarnation(state, id);
        let ran = match &incarnation {
            Ok(Some(_)) | Err(_) => true,
            Ok(None) => meta
                .members
                .get(id)
                .is_some_and(|m| m.started != Some(false)),
        };
        let mut row = StatusRow {
            name: id.clone(),
            kind: "member",
            instance: inst.id.clone(),
            team: inst.record.team.clone(),
            member: Some(id.clone()),
            start_id: incarnation
                .as_ref()
                .ok()
                .and_then(|i| i.as_ref())
                .map(|i| i.incarnation.clone()),
            role: Some(role.to_string()),
            vendor: Some(member.vendor.as_str().into()),
            model: member.model.as_ref().map(|m| match &m.effort {
                Some(e) => format!("{} {e}", m.name),
                None => m.name.clone(),
            }),
            parent: nodes[i].parent.clone(),
            liveness: incarnation
                .as_ref()
                .ok()
                .and_then(|i| i.as_ref())
                .map(|inc| lifecycle::liveness(state, id, &inc.incarnation)),
            posture: Some(match posture {
                Posture::Vendor => "vendor".into(),
                Posture::Locked => "locked".into(),
            }),
            state: if ran { STOPPED } else { NOT_STARTED }.into(),
            unread: counts
                .as_ref()
                .map(|all| all.get(id).copied().unwrap_or_default().unread),
            running: None,
            expected_running: false,
            start: member.start,
            rule: None,
            detail: None,
            vendor_version: meta.members.get(id).and_then(|m| m.vendor_version.clone()),
            trust_accepted: std::fs::read_to_string(state.member_dir(id).join("trust-accepted"))
                .ok()
                .map(|s| s.trim().to_string()),
        };
        if let Some(w) = windows.iter().find(|w| &w.name == id) {
            if w.dead {
                row.state = "exited".into();
                row.detail = w.dead_status.map(|s| format!("status {s}"));
            } else {
                let screen = tmux.capture(&session, id);
                match rules::load(member.vendor) {
                    Ok(rules) => {
                        let reading = rules.read(&w.title, &screen);
                        row.state = reading.state.as_str().into();
                        row.rule = reading.rule;
                    }
                    Err(e) => {
                        row.state = State::Unknown.as_str().into();
                        row.detail = Some(e);
                    }
                }
                let mut details: Vec<String> = row.detail.take().into_iter().collect();
                // It waits for a person to answer it.
                if row.state == "blocked" && row.rule.as_deref() == Some(TRUST_RULE) {
                    details.insert(
                        0,
                        format!("waiting at folder trust: `brg attach {}`", inst.at(id)),
                    );
                }
                if posture == Posture::Locked && !state.gate_marker(id).exists() {
                    details.push("gate not active: gets no input".to_string());
                    if row.state != "blocked" {
                        row.state = "gate-inactive".into();
                    }
                }
                // Nothing is typed into it (`send`, the wake, folder trust)
                // until it leaves copy mode.
                if w.in_mode {
                    details.push(format!(
                        "{}: {}",
                        tmux::IN_MODE,
                        tmux::leave_mode_hint(&inst.at(id))
                    ));
                }
                if !details.is_empty() {
                    row.detail = Some(details.join("; "));
                }
            }
        }
        row.expected_running = team_running && row.state == STOPPED && member.start;
        rows.push(row);
    }
    for (name, plugin) in &v.team.plugins {
        if plugin.process.is_none() {
            continue;
        }
        let window = format!("_{name}");
        let mut row = StatusRow {
            name: name.clone(),
            kind: "plugin",
            instance: inst.id.clone(),
            team: inst.record.team.clone(),
            member: None,
            start_id: None,
            role: None,
            vendor: None,
            model: None,
            parent: None,
            liveness: None,
            posture: None,
            state: "stopped".into(),
            rule: None,
            detail: None,
            vendor_version: None,
            trust_accepted: None,
            unread: None,
            running: None,
            expected_running: false,
            start: false,
        };
        if let Some(w) = windows.iter().find(|w| w.name == window) {
            row.state = if w.dead {
                "exited".into()
            } else {
                "alive".into()
            };
            let mut details = Vec::new();
            if w.dead
                && let Some(s) = w.dead_status
            {
                details.push(format!("status {s}"));
            }
            if !w.dead
                && let Some(h) = health(&state.plugin_dir(name))
            {
                details.push(h);
            }
            if let Some(n) = pending(&state.plugin_dir(name)) {
                details.push(n);
            }
            if !details.is_empty() {
                row.detail = Some(details.join(", "));
            }
        }
        rows.push(row);
    }
    if let Some(note) = bus_note
        && let Some(row) = rows.iter_mut().find(|r| r.kind == "member")
    {
        row.detail = Some(match row.detail.take() {
            Some(detail) => format!("{detail}; {note}"),
            None => note,
        });
    }
    for (role_name, role) in &v.team.team_roles {
        let copies: Vec<_> = inst
            .record
            .copies
            .iter()
            .filter(|(_, c)| !c.deleted && c.role == *role_name)
            .collect();
        let parent = role.parent.as_ref().map(|p| {
            inst.record
                .members_of(p)
                .into_iter()
                .next()
                .unwrap_or_else(|| p.clone())
        });
        if copies.is_empty() {
            rows.push(StatusRow {
                name: role_name.clone(),
                kind: "role",
                instance: inst.id.clone(),
                team: inst.record.team.clone(),
                member: None,
                start_id: None,
                role: Some(role_name.clone()),
                vendor: None,
                model: None,
                parent,
                posture: None,
                state: if role.max > 1 {
                    ON_DEMAND.into()
                } else {
                    "not created (start: false)".into()
                },
                rule: None,
                detail: None,
                vendor_version: None,
                trust_accepted: None,
                liveness: None,
                unread: None,
                running: None,
                expected_running: false,
                start: false,
            });
        }
        for (id, _) in copies {
            let copy = address::instance(&inst.at(id))?;
            let mut children = status(&copy)?;
            let running = children
                .iter()
                .filter(|r| {
                    r.kind == "member"
                        && r.instance == copy.id
                        && !matches!(r.state.as_str(), STOPPED | NOT_STARTED | "exited")
                })
                .count();
            rows.push(StatusRow {
                name: id.clone(),
                kind: "copy",
                instance: copy.id.clone(),
                team: copy.record.team.clone(),
                member: None,
                start_id: None,
                role: Some(role_name.clone()),
                vendor: None,
                model: None,
                parent: copy.record.parent.clone(),
                posture: None,
                state: if running > 0 { "running" } else { STOPPED }.into(),
                rule: None,
                detail: None,
                vendor_version: None,
                trust_accepted: None,
                liveness: None,
                unread: None,
                running: Some(running),
                expected_running: false,
                start: false,
            });
            for child in &mut children {
                if child.parent.is_none() {
                    child.parent = Some(copy.id.clone());
                }
            }
            rows.extend(children);
        }
    }
    let nodes = status_nodes(&rows);
    let positions: BTreeMap<_, _> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| (status_key(r), i))
        .collect();
    let mut ordered: Vec<_> = tree::order(&nodes)
        .into_iter()
        .map(|(i, _)| rows[positions[&nodes[i].id]].clone())
        .collect();
    ordered.extend(
        rows.into_iter()
            .filter(|r| r.kind == "plugin" && r.parent.is_none()),
    );
    Ok(ordered)
}

/// Drop control characters (terminal escapes) from plugin-written text.
fn printable(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(200).collect()
}

/// A plugin's `health.json`: `{ok, at, detail}`.
fn health(dir: &Path) -> Option<String> {
    let path = dir.join("health.json");
    if !std::fs::symlink_metadata(&path).ok()?.is_file() {
        return Some("error: health.json is not a regular file".into());
    }
    let text = std::fs::read_to_string(&path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let age = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .unwrap_or_default();
    let ok = value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let detail = value.get("detail").and_then(|v| v.as_str()).unwrap_or("");
    Some(if age > Duration::from_secs(300) {
        format!("stale ({}s since last report)", age.as_secs())
    } else if ok {
        "ok".to_string()
    } else {
        format!("error: {}", printable(detail))
    })
}

/// Lines in a plugin's `undelivered.jsonl`, if any.
fn pending(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("undelivered.jsonl")).ok()?;
    let n = text.lines().filter(|l| !l.trim().is_empty()).count();
    (n > 0).then(|| format!("{n} undelivered"))
}

/// `brg status` as text: the instance's tree with each member's vendor, model,
/// state and details, then the plugin processes.
/// The columns `status` and `status -i` show after a row's name: a
/// member's vendor and model, state and details; a role with no member
/// yet, `not created (start: <setting>)`; a plugin's state and detail.
pub fn status_cells(r: &StatusRow) -> Vec<String> {
    match r.kind {
        "member" => {}
        "copy" => {
            return vec![
                r.team.clone(),
                format!("{} running", r.running.unwrap_or(0)),
                String::new(),
            ];
        }
        "role" => return vec![r.state.clone()],
        _ => return vec![r.state.clone(), r.detail.clone().unwrap_or_default()],
    }
    let mut detail = Vec::new();
    if r.posture.as_deref() == Some("locked") {
        detail.push("locked".to_string());
    }
    detail.extend(r.detail.clone());
    if let Some(unread) = r.unread.filter(|&n| n > 0) {
        detail.push(format!("{unread} unread"));
    }
    vec![
        format!(
            "{} {}",
            r.vendor.as_deref().unwrap_or("-"),
            r.model.as_deref().unwrap_or("default")
        ),
        state_label(r),
        detail.join("; "),
    ]
}

pub fn render_status(header: &str, rows: &[StatusRow]) -> String {
    let nodes = status_nodes(rows);
    let plugins: Vec<(String, Vec<String>)> = rows
        .iter()
        .filter(|r| r.kind == "plugin" && r.parent.is_none())
        .map(|r| (r.name.clone(), status_cells(r)))
        .collect();
    let names: BTreeMap<_, _> = rows
        .iter()
        .map(|r| (status_key(r), status_name(r)))
        .collect();
    tree::render_named(
        header,
        &nodes,
        &plugins,
        std::io::stdout().is_terminal(),
        |id| names[id].clone(),
    )
}

/// A row's identity stays unique when local IDs repeat in different copies.
pub fn status_key(row: &StatusRow) -> String {
    match row.kind {
        "copy" => row.instance.clone(),
        "plugin" => format!("{}/plugin:{}", row.instance, row.name),
        _ => format!("{}/{}", row.instance, row.name),
    }
}

/// The short label shared by the text and interactive trees.
pub fn status_name(row: &StatusRow) -> String {
    if row.kind == "plugin" {
        format!("plugin {}", row.name)
    } else {
        row.name.clone()
    }
}

/// Qualified nodes shared by status ordering and both tree renderers.
pub fn status_nodes(rows: &[StatusRow]) -> Vec<tree::Node> {
    rows.iter()
        .filter(|r| r.kind != "plugin" || r.parent.is_some())
        .map(|r| tree::Node {
            id: status_key(r),
            parent: r.parent.as_ref().map(|p| {
                if p.contains('/') {
                    p.clone()
                } else {
                    format!("{}/{p}", r.instance)
                }
            }),
            cols: status_cells(r),
            dim: r.kind == "role" && r.state == ON_DEMAND,
        })
        .collect()
}

/// Why `send` refused, with its exit code.
#[derive(Debug, PartialEq, Eq)]
pub enum SendError {
    Usage(String),
    NotAlive(String),
    Blocked(String),
    Refused(String),
    Busy(String),
}

impl SendError {
    pub fn code(&self) -> u8 {
        match self {
            SendError::Usage(_) => 2,
            SendError::NotAlive(_) => 3,
            SendError::Blocked(_) => 4,
            SendError::Refused(_) => 5,
            SendError::Busy(_) => 6,
        }
    }
    pub fn message(&self) -> &str {
        match self {
            SendError::Usage(m)
            | SendError::NotAlive(m)
            | SendError::Blocked(m)
            | SendError::Refused(m)
            | SendError::Busy(m) => m,
        }
    }
}

/// Text checks `send` makes before touching the member.
pub fn check_text(text: &str) -> Result<(), SendError> {
    if text.trim().is_empty() {
        return Err(SendError::Usage("nothing to send".into()));
    }
    if text.len() > MAX_SEND_BYTES {
        return Err(SendError::Refused(format!(
            "text is over {MAX_SEND_BYTES} bytes"
        )));
    }
    if let Some(c) = text
        .chars()
        .find(|&c| (c.is_control() && c != '\n' && c != '\t') || c == '\u{7f}')
    {
        return Err(SendError::Refused(format!(
            "text contains a control character (U+{:04X})",
            c as u32
        )));
    }
    let first = text.trim_start().chars().next();
    if matches!(first, Some('/') | Some('!')) {
        return Err(SendError::Refused(
            "text may not start with `/` or `!` (slash commands and shell escapes)".into(),
        ));
    }
    Ok(())
}

/// `brg send <instance>/<member> <text>`.
pub fn send(
    inst: &Instance,
    member: &str,
    text: &str,
    force: bool,
    from: Option<&str>,
) -> Result<String, SendError> {
    check_text(text)?;
    let member = &address::member(inst, member).map_err(SendError::Usage)?;
    let from = from
        .map(|f| address::member(inst, f).map_err(SendError::Usage))
        .transpose()?;
    let from = from.as_deref();
    let state = &inst.state;
    let meta = state::read_meta(state)
        .ok_or_else(|| SendError::NotAlive(format!("instance `{}` isn't running", inst.id)))?;
    let member_meta = meta
        .members
        .get(member)
        .ok_or_else(|| SendError::NotAlive(format!("`{}` hasn't started", inst.at(member))))?;
    let vendor = match member_meta.vendor.as_str() {
        "codex" => Vendor::Codex,
        _ => Vendor::Claude,
    };
    let locked = member_meta.posture == "locked";
    if let Some(from) = from {
        let parent = meta.members.get(from).and_then(|m| m.parent.as_deref());
        if parent != Some(member) {
            return Err(SendError::Refused(format!(
                "`{from}` may only send to its parent, and `{member}` is not it"
            )));
        }
    }
    if locked && !state.gate_marker(member).exists() {
        return Err(SendError::Refused(format!(
            "`{member}` is `locked` but its gate is not active; it gets no input until it is (restart it with `brg stop {0}` and `brg start {0}`)",
            inst.at(member)
        )));
    }
    let session = state.session();
    let tmux = tmux_for(state);

    // Lock files are created at a start, outside any sandbox; inside one, `send`
    // only opens its member's existing file read-only.
    let lock = state::open_lock(&state.lock_file(member))
        .map_err(|e| SendError::Refused(format!("cannot open the send lock: {e}")))?;
    lock.lock()
        .map_err(|e| SendError::Refused(format!("cannot lock: {e}")))?;

    let w = tmux
        .window(&session, member)
        .ok_or_else(|| SendError::NotAlive(format!("`{member}` is not running")))?;
    if w.dead {
        return Err(SendError::NotAlive(format!("`{member}` has exited")));
    }
    let rules = rules::load(vendor).map_err(SendError::Refused)?;
    let reading = rules.read(&w.title, &tmux.capture(&session, member));
    match reading.state {
        State::Blocked => {
            return Err(SendError::Blocked(format!(
                "`{member}` is waiting at a prompt; attach to answer it (`brg attach {}`)",
                inst.at(member)
            )));
        }
        State::Unknown if !force => {
            return Err(SendError::Refused(format!(
                "`{member}`'s state is unknown; pass --force to send anyway"
            )));
        }
        // In `vendor` posture a permission dialog can appear mid-turn, and an
        // Enter meant for the prompt would answer it. `locked` members can't
        // be prompted, so they take input while working.
        State::Working if !locked => {
            return Err(SendError::Busy(format!(
                "`{member}` is in the middle of a turn; try again when it is idle"
            )));
        }
        _ => {}
    }
    // Keys would go to copy mode, not the member: its Enter would be lost.
    if w.in_mode {
        return Err(SendError::Blocked(format!(
            "`{member}`: {}; nothing was sent. To send, {}",
            tmux::IN_MODE,
            tmux::leave_mode_hint(&inst.at(member))
        )));
    }

    let buffer = format!(
        "brg-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    tmux.run_with_stdin(&["load-buffer", "-b", &buffer, "-"], text.as_bytes())
        .map_err(SendError::Refused)?;
    let t = target(&session, member);
    let pasted = tmux
        .ok(&["paste-buffer", "-p", "-d", "-b", &buffer, "-t", &t])
        .map_err(SendError::Refused);
    if pasted.is_err() {
        let _ = tmux.run(&["delete-buffer", "-b", &buffer]);
    }
    pasted?;
    // Check again before pressing Enter, in case a dialog appeared or the
    // pane went into copy mode meanwhile.
    let now = tmux.window(&session, member);
    let title = now.as_ref().map(|w| w.title.clone()).unwrap_or_default();
    if rules.read(&title, &tmux.capture(&session, member)).state == State::Blocked {
        return Err(SendError::Blocked(format!(
            "`{member}` showed a prompt while the text was being pasted; Enter was not pressed. Attach to check it (`brg attach {}`)",
            inst.at(member)
        )));
    }
    // A window that can't be read now counts as one in copy mode: Enter is
    // only pressed on a pane known to take it.
    if now.as_ref().is_none_or(|w| w.in_mode) {
        return Err(SendError::Blocked(format!(
            "`{member}`: {} after the text was pasted; Enter was not pressed, and the text is in its input box. Check it: {}",
            tmux::IN_MODE,
            tmux::leave_mode_hint(&inst.at(member))
        )));
    }
    tmux.ok(&["send-keys", "-t", &t, "Enter"])
        .map_err(SendError::Refused)?;
    drop(lock);
    Ok(format!("sent to {member}"))
}

/// `brg logs <instance>/<member>` (or a plugin process).
pub fn logs(inst: &Instance, name: &str, lines: usize) -> Result<String, String> {
    let session = inst.state.session();
    let tmux = tmux_for(&inst.state);
    let window = window_name(inst, &tmux, &session, name)?;
    Ok(tmux.history(&session, &window, lines))
}

/// A member's window (named in full), or a plugin process's (`_<plugin>`).
fn window_name(inst: &Instance, tmux: &Tmux, session: &str, name: &str) -> Result<String, String> {
    let plugin = format!("_{}", name.strip_prefix('_').unwrap_or(name));
    if !inst.record.members.contains_key(name) && tmux.window(session, &plugin).is_some() {
        return Ok(plugin);
    }
    let member = address::member(inst, name)?;
    if tmux.window(session, &member).is_some() {
        return Ok(member);
    }
    Err(format!("`{}` is not running", inst.at(&member)))
}

fn attach_command(inst: &Instance, name: &str) -> Result<Command, String> {
    let session = inst.state.session();
    let tmux = tmux_for(&inst.state);
    let window = window_name(inst, &tmux, &session, name)?;
    let mut cmd = Command::new("tmux");
    match state::read_meta(&inst.state).and_then(|m| m.socket_path) {
        Some(path) => cmd.arg("-S").arg(path),
        None => cmd.arg("-L").arg(state::tmux_socket()),
    };
    cmd.arg("-f")
        .arg(inst.state.tmux_conf())
        .args(["attach-session", "-t", &target(&session, &window)])
        // Works from inside your own tmux too: brg's server is a separate one.
        .env_remove("TMUX");
    match std::env::var_os(state::ENV_TMUX_TMPDIR) {
        Some(dir) => cmd.env("TMUX_TMPDIR", dir),
        None => cmd.env_remove("TMUX_TMPDIR"),
    };
    Ok(cmd)
}

/// `brg attach <instance>/<member>`: replaces this process with a tmux client.
pub fn attach(inst: &Instance, name: &str) -> Result<(), String> {
    let err = attach_command(inst, name)?.exec();
    Err(format!("cannot start tmux: {err}"))
}

/// Attach, and return when the client detaches (used by `status -i`).
pub fn attach_and_wait(inst: &Instance, name: &str) -> Result<(), String> {
    let status = attach_command(inst, name)?
        .status()
        .map_err(|e| format!("cannot start tmux: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("tmux attach ended with {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_checks() {
        assert!(check_text("hello\nworld\tok").is_ok());
        assert_eq!(check_text("").unwrap_err().code(), 2);
        assert_eq!(check_text("/clear").unwrap_err().code(), 5);
        assert_eq!(check_text("  !rm -rf").unwrap_err().code(), 5);
        assert_eq!(check_text("a\u{1b}[201~b").unwrap_err().code(), 5);
        assert_eq!(check_text("a\u{7f}").unwrap_err().code(), 5);
        assert_eq!(check_text("a\rb").unwrap_err().code(), 5);
        assert_eq!(
            check_text(&"x".repeat(MAX_SEND_BYTES + 1))
                .unwrap_err()
                .code(),
            5
        );
    }

    #[test]
    fn trust_dialog_path_is_read_from_its_own_line() {
        let screen = include_str!("../tests/fixtures/screens/claude-trust.txt");
        assert_eq!(
            trust_dialog_path(screen),
            Some(PathBuf::from("/tmp/example/untrusted"))
        );
        assert_eq!(
            trust_dialog_path("Accessing workspace:\nrelative/path\n"),
            None
        );
        assert_eq!(trust_dialog_path("no dialog here\n"), None);
    }

    #[test]
    fn member_cwds_stay_inside_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("work");
        std::fs::create_dir_all(folder.join("copies/lead-2a3b")).unwrap();
        std::fs::create_dir_all(dir.path().join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), folder.join("out")).unwrap();
        let real = folder.canonicalize().unwrap();
        assert_eq!(member_cwd(&folder, None, "lead-2a3b").unwrap(), real);
        assert_eq!(
            member_cwd(&folder, Some("copies/{member_id}"), "lead-2a3b").unwrap(),
            real.join("copies/lead-2a3b")
        );
        assert!(
            member_cwd(&folder, Some("out"), "lead-2a3b")
                .unwrap_err()
                .contains("outside")
        );
        assert!(
            member_cwd(&folder, Some("missing"), "lead-2a3b")
                .unwrap_err()
                .contains("does not exist")
        );
        assert!(
            member_cwd(&dir.path().join("gone"), None, "lead-2a3b")
                .unwrap_err()
                .contains("is gone")
        );
    }

    #[test]
    fn base_env_is_small() {
        let env = base_env();
        for key in env.keys() {
            assert!(BASE_ENV.contains(&key.as_str()) || key == "LANG", "{key}");
        }
    }
}
