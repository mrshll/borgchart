//! The tmux runtime: `create`, `start`, `stop`, `delete`, `status`, `templates`,
//! `attach`, `logs` and `msg send`, on teams and their members.

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
use crate::bus::store;
use crate::bus::time::utc_now;
use crate::config::Auth;
use crate::exec::{ExecSpec, PluginEnv, identity_env};
use crate::launch;
use crate::lifecycle;
use crate::lifecycle::{Incarnation, Liveness, PaneIdentity};
use crate::paths::overlaps;
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
    // A child team must not capture its creator's member-only shims:
    // those commands are fenced to that member, not to the child's members.
    if let (Some(path), Ok(instance), Ok(member)) = (
        env.get_mut("PATH"),
        std::env::var(crate::address::ENV_INSTANCE),
        std::env::var(crate::bus::context::ENV_MEMBER),
    ) && instance.split('/').all(crate::names::is_valid_member_id)
        && crate::names::is_valid_member_id(&member)
    {
        *path = without_member_shim(path, &StateDir::new(&instance).member_bin(&member));
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

fn without_member_shim(path: &str, shim: &Path) -> String {
    std::env::join_paths(std::env::split_paths(path).filter(|entry| entry != shim))
        .expect("entries split from PATH can be joined")
        .into_string()
        .expect("entries split from a UTF-8 PATH stay UTF-8")
}

fn brg_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("brg"))
}

/// The team's tmux server: the socket path recorded at a start once
/// there is one, whatever the caller's tmux settings say; until then, the
/// socket brg's settings name.
fn tmux_for(state: &StateDir) -> Tmux {
    let tmux = Tmux::new(&state.tmux_conf(), base_env());
    match state::read_meta(state)
        .ok()
        .flatten()
        .and_then(|m| m.socket_path)
    {
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

/// The vendor's own key a member gets with `auth: api_key`.
fn key_name(vendor: Vendor) -> &'static str {
    match vendor {
        Vendor::Claude => "ANTHROPIC_API_KEY",
        Vendor::Codex => "OPENAI_API_KEY",
    }
}

/// Explicit choices in precedence order: team, then member.
fn member_overrides(record: &registry::InstanceRecord, id: &str) -> crate::overrides::Overrides {
    let Some(member) = record.members.get(id) else {
        return Default::default();
    };
    record
        .overrides
        .get(&member.role)
        .cloned()
        .unwrap_or_default()
        .merge(&member.overrides)
}

fn stored_choice_error(
    error: &str,
    target: &str,
    role: Option<&str>,
    choice: &crate::overrides::Overrides,
) -> String {
    let cause = error.split(';').next().unwrap_or(error);
    let prefix = role.map(|role| format!("{role}.")).unwrap_or_default();
    let keys = [
        ("vendor", choice.vendor.map(|v| v.as_str())),
        ("model", choice.model.as_deref()),
        ("effort", choice.effort.as_deref()),
    ]
    .into_iter()
    .filter_map(|(key, value)| value.map(|value| format!("{prefix}{key}={value}")))
    .collect::<Vec<_>>()
    .join(",");
    format!(
        "{cause}; `{target}` was created with `{keys}`: delete it and create it again (`brg delete {target}`). Nothing was started"
    )
}

fn role_choice(
    v: &Validated,
    record: &registry::InstanceRecord,
    role: &str,
) -> Result<crate::team::Role, String> {
    let template = v
        .team
        .roles
        .get(role)
        .ok_or_else(|| format!("role `{role}` is not in the team any more"))?;
    let choice = record.overrides.get(role).cloned().unwrap_or_default();
    choice
        .resolve(template, v.auth())
        .map_err(|e| stored_choice_error(&e, &record.id, Some(role), &choice))
}

/// One resolver for every consumer of an issued member's vendor and model.
fn member_role(
    v: &Validated,
    record: &registry::InstanceRecord,
    id: &str,
) -> Result<crate::team::Role, String> {
    let role = record
        .role_of(id)
        .and_then(|r| v.team.roles.get(r))
        .ok_or_else(|| format!("`{id}`'s role is not in the team any more"))?;
    member_overrides(record, id)
        .resolve(role, v.auth())
        .map_err(|e| {
            let member = &record.members[id].overrides;
            // Prefer the member when both layers independently fail. An inherited
            // choice still belongs to the team, even when the member adds keys.
            if (member.is_empty() || member.resolve(role, v.auth()).is_ok())
                && let Some(team) = record.overrides.get(&role.id)
                && team.resolve(role, v.auth()).is_err()
            {
                return stored_choice_error(&e, &record.id, Some(&role.id), team);
            }
            stored_choice_error(&e, &format!("{}/{id}", record.id), None, member)
        })
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
        let role = member_role(v, record, id)?;
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

/// Where a member starts: its own worktree for a `worktree: true` role,
/// else the team's folder. Either must exist.
fn member_cwd(inst: &Instance, role: &crate::team::Role, member: &str) -> Result<PathBuf, String> {
    let folder = &inst.record.folder;
    if role.worktree {
        let dir = crate::worktree::path(&inst.at(member));
        return dir.canonicalize().map_err(|_| {
            format!(
                "`{member}`'s worktree {} is gone; delete the member and create another",
                dir.display()
            )
        });
    }
    folder.canonicalize().map_err(|_| {
        format!(
            "the team's working folder {} is gone; create a new team for another folder",
            folder.display()
        )
    })
}

/// Start `member_ids` of the team, from validated files `v`. `full` is a
/// start of the whole team (its plugin processes too); otherwise one
/// member's start.
///
/// Everything is checked before anything is written. Starting into a
/// running team (its session exists and its start record is there)
/// reuses its base environment and plugin settings, and reads current template files.
/// Already-running members keep their launch settings. A bus keeps one store.
/// The registry and lifecycle locks
/// are held while files are written and windows started, and released before
/// waiting for gates and folder trust.
fn start_members(
    lock: registry::Lock,
    inst: &Instance,
    v: &Validated,
    member_ids: Vec<String>,
    full: bool,
    resume: bool,
    values: Option<crate::secrets::PluginValues>,
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
    let mut resolved = BTreeMap::new();
    for (id, _) in record.kept_members() {
        match member_role(v, record, id) {
            Ok(role) => {
                resolved.insert(id.clone(), role);
            }
            Err(error) if member_ids.contains(id) => return Err(error),
            Err(_) => {}
        }
    }
    let role_of = |id: &str| -> Result<&crate::team::Role, String> {
        resolved
            .get(id)
            .ok_or_else(|| format!("`{id}`'s role is not in the team any more"))
    };
    let mut cwds: BTreeMap<String, PathBuf> = BTreeMap::new();
    // Checks that need nothing written.
    for id in &member_ids {
        let member = role_of(id)?;
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
            member_cwd(inst, member, id).map_err(|e| format!("{e}; nothing was started"))?,
        );
    }
    // With `--resume`, the session each member's latest start reported. A
    // member started on its own must have one; a full start starts fresh
    // whoever has none, and says so. A session belongs to the vendor that
    // reported it: after a role's vendor changes,
    // the member starts fresh.
    let mut sessions: BTreeMap<String, String> = BTreeMap::new();
    let mut vendor_changed: BTreeSet<String> = BTreeSet::new();
    if resume {
        let last = state::read_meta(state).ok().flatten();
        for id in &member_ids {
            let last_vendor = last
                .as_ref()
                .and_then(|m| m.members.get(id))
                .map(|m| m.vendor.as_str());
            let vendor = role_of(id)?.vendor.as_str();
            match recorded_session(state, id) {
                Some(_) if last_vendor.is_some_and(|v| v != vendor) => {
                    vendor_changed.insert(id.clone());
                }
                Some(s) => {
                    sessions.insert(id.clone(), s);
                }
                None if only
                    && !tmux_for(state)
                        .window(&session, id)
                        .is_some_and(|w| !w.dead) =>
                {
                    return Err(format!(
                        "{} has no recorded session to resume; start it fresh: brg start {}. Nothing was started",
                        inst.at(id),
                        inst.at(id)
                    ));
                }
                None => {}
            }
        }
    }
    let mut pre_notes = Vec::new();
    for (id, cwd) in &cwds {
        let member = role_of(id)?;
        check_state_overlap(cwd, &state.root)
            .map_err(|e| format!("{}'s {e}; nothing was started", inst.at(id)))?;
        if overlaps(cwd, &v.team.dir) {
            if v.posture(&member.id) == Posture::Locked {
                return Err(format!(
                    "{} is `locked`, and its folder {} overlaps the template folder {}, so it could change its own role files; give it a folder apart from the template. Nothing was started",
                    inst.at(id),
                    cwd.display(),
                    v.team.dir.display()
                ));
            }
            let note = format!(
                "note: the template folder {} is inside the members' folder {}; they can change the template's files",
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
    let wanted: Vec<(String, PathBuf)> = cwds.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    crate::checkouts::reserve(&lock, &inst.id, &wanted)?;
    let keys = api_keys(v, record, &member_ids)?;
    let probe = tmux_for(state);
    let looks_running = probe.has_session(&session) && state.meta_file().exists();
    if !looks_running && values.is_none() {
        return Err(
            "the team stopped during this start; run the command again; nothing was started".into(),
        );
    }

    // Serialize lifecycle changes, then decide for real.
    state::ensure_dir(&state.root)
        .map_err(|e| format!("cannot create {}: {e}", state.root.display()))?;
    let state_lock = lifecycle::lock_state(state)?;
    let previous = state::read_meta(state)?;
    // Unless the last start here was confirmed stopped, or never started a
    // window, a server this caller can't see may still run it.
    let may_still_run = previous
        .as_ref()
        .is_some_and(|m| m.stopped_at.is_none() && m.windows_started);
    let existing = previous.clone().filter(|_| probe.has_session(&session));
    let joining = existing.is_some();
    if !joining
        && let Some(meta) = &previous
        && meta.config != snapshot
    {
        lifecycle::check_stopped(state, meta, &restart)?;
    }
    if !joining && values.is_none() {
        return Err(
            "the team stopped during this start; run the command again; nothing was started".into(),
        );
    }
    if let Some(existing) = &existing {
        for name in existing.plugins.keys() {
            let settings = std::fs::read(state.plugin_env_file(name))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<PluginEnv>(&bytes).ok());
            if settings.is_none() {
                return Err(format!(
                    "plugin {name}'s settings are missing or corrupt; stop the whole team and start it again; nothing was started"
                ));
            }
        }
    }
    {
        let paths = StorePaths::new(state);
        if paths.validate(&state.id)? {
            let snap =
                store::snapshot_of(&paths).map_err(|e| unreadable_store(inst, &e.message))?;
            if !snap.healthy() {
                return Err(unreadable_store(inst, "incomplete messages"));
            }
            if let Some(old) = &previous {
                let bus = store::open(state).map_err(|e| e.message)?;
                let snap = store::snapshot(&bus).map_err(|e| e.message)?;
                for (member, meta) in &old.members {
                    if (!joining || member_ids.contains(member))
                        && probe
                            .window(&session, member)
                            .is_none_or(|window| window.dead)
                        && meta.posture != "locked"
                        && v.posture(&meta.role) == Posture::Locked
                        && !lifecycle::unacked_work(&bus, &snap, Some(member))
                            .items
                            .is_empty()
                    {
                        return Err(format!(
                            "{}/{member} has unacknowledged messages; settle them before changing to locked posture",
                            state.id
                        ));
                    }
                }
            }
        }
        paths.initialize(state)?;
    }
    let mut notes: Vec<String> = pre_notes;
    let user_config = v.config.tmux_user_config.as_ref().and_then(|u| u.resolve());
    // The server reads its config once, when it starts, and every team
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
            "brg's tmux server is already running {}, so `tmux.user_config` ({}) takes effect only once every team on it is stopped and it starts again",
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
        let member = role_of(id).ok()?;
        Some(MemberMeta {
            interactive: member.interactive,
            role: role.to_string(),
            vendor: member.vendor.as_str().into(),
            posture: match v.posture(role) {
                Posture::Vendor => "vendor".into(),
                Posture::Locked => "locked".into(),
            },
            parent: parent_member(member),
            vendor_version: vendor_version(member.vendor, base),
            started: false,
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
                socket: state::tmux_socket(),
                config: snapshot.clone(),
                base_env: base.clone(),
                windows_started: may_still_run,
                ..Meta::default()
            };
            // Plugins: their values (secrets included) go to private files for the shims.
            for (name, plugin) in &v.team.plugins {
                let body = serde_json::to_vec(&PluginEnv {
                    env: values
                        .as_ref()
                        .unwrap()
                        .get(name)
                        .cloned()
                        .unwrap_or_default(),
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
    // Every member of the team is known to the start record, including
    // ones created since the team started.
    let mut added = fresh;
    for (id, _) in record.kept_members() {
        if !meta.members.contains_key(id)
            && let Some(m) = member_meta(id, &meta.base_env)
        {
            meta.members.insert(id.clone(), m);
            added = true;
        }
        if let Some(entry) = meta.members.get_mut(id)
            && let Some(role) = resolved.get(id)
            && role.parent.is_some()
            && entry.parent == role.parent
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
    // A running team's server is the one recorded for it.
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
        if let Some(current) = member_meta(id, &base) {
            meta.members.insert(id.clone(), current);
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
        // Codex's sandbox can write its cwd, so a `locked` Codex member
        // without its own worktree starts in its temp dir, not the folder.
        let cwd =
            if !member.worktree && posture == Posture::Locked && member.vendor == Vendor::Codex {
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
        let codex_mcp_servers = if member.vendor == Vendor::Codex
            && member.connectors.declared
            && member.connectors.is_empty()
        {
            let list_args = codex_trust
                .as_deref()
                .map(|dir| vec!["-c".into(), launch::codex_trust_config(dir)])
                .unwrap_or_default();
            let listing = crate::probe::list(Vendor::Codex, &cwd, &env, &list_args)
                .map_err(|e| {
                    format!(
                        "{e}; run `codex mcp list --json` in {} and retry",
                        cwd.display()
                    )
                })
                .and_then(|names| {
                    if let Some(name) = names.iter().find(|name| {
                        name.is_empty()
                            || !name
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    }) {
                        Err(format!(
                            "Codex server {name:?} can't be turned off for this member; rename it in the Codex config or drop `connectors: []`"
                        ))
                    } else {
                        Ok(names)
                    }
                });
            match listing {
                Ok(names) => names,
                Err(error) => {
                    set(&mut outcomes, id, format!("not started: {error}"));
                    failed = true;
                    continue;
                }
            }
        } else {
            Default::default()
        };

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
            &codex_mcp_servers,
            &shared,
            sessions.get(id).map(String::as_str),
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

        // Record every start before its window exists.
        let incarnation = {
            let inc = Incarnation {
                member: id.clone(),
                incarnation: lifecycle::new_incarnation_id(state, id),
                started_at: utc_now(),
                boot_id: lifecycle::boot_id(),
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
            inc
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
        if result.is_err() {
            // tmux didn't create the window, so nothing runs it.
            let _ = lifecycle::end_incarnation(state, id, lifecycle::ENDED_START_FAILED);
        }
        if result.is_err() {
            unrecord_windows(state, &mut meta, fresh && !any_window, may_still_run);
        }
        result?;
        any_window = true;
        let pane = lifecycle::pane_identity(&tmux, &session, id);
        if pane.is_none() && posture == Posture::Locked {
            // Without its pane, later checks couldn't tell this member from a
            // replacement, so it couldn't be stopped safely if its gate never
            // reported in. Stop it now, while the locks are held.
            let _ = tmux.kill_window(&session, id);
            let _ = lifecycle::end_incarnation(state, id, lifecycle::ENDED_START_FAILED);
            set(
                &mut outcomes,
                id,
                "stopped: brg couldn't read its pane, so its `locked` gate couldn't be checked"
                    .to_string(),
            );
            failed = true;
            continue;
        }
        let mut inc = incarnation;
        inc.pane = pane.clone();
        lifecycle::complete_incarnation(state, &inc)?;
        if let Some(member_meta) = meta.members.get_mut(id) {
            member_meta.started = true;
        }
        write_private(
            &state.meta_file(),
            &serde_json::to_vec_pretty(&meta).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("cannot record member start: {e}"))?;
        started.push(Started {
            id: id.clone(),
            vendor: member.vendor,
            locked: posture == Posture::Locked,
            trust_dir,
            watch_codex_trust: member.vendor == Vendor::Codex && codex_trust.is_none(),
            pane,
        });
        set(
            &mut outcomes,
            id,
            match (resume, sessions.contains_key(id)) {
                (true, true) => "resumed".into(),
                (true, false) if vendor_changed.contains(id) => {
                    "started fresh: its vendor changed since its session".into()
                }
                (true, false) => "started fresh: no recorded session to resume".into(),
                (false, _) => "started".into(),
            },
        );
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

    // Plugin processes start with the whole team.
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
                .ok_or_else(|| format!("plugin {name}'s settings are missing or corrupt; stop the whole team and start it again"))?;
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
                // gate must still report in afterwards (`msg send` checks).
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
    let plugins: Vec<(String, Vec<String>, tree::Look)> = plugin_outcomes
        .into_iter()
        .map(|(name, o)| (name, vec![o], tree::Look::default()))
        .collect();
    Ok(UpOutcome {
        tree: tree::render(
            &header(inst),
            &nodes,
            &plugins,
            tree::colour_wanted(std::io::stdout().is_terminal()),
        ),
        notes,
        failed,
    })
}

/// The first line of a team's tree: its ID, template folder and working
/// folder. On a terminal, only the ID and the working folder, with the home
/// folder as `~`: the template folder is long and rarely what a person looks
/// for. Piped text keeps the whole line.
pub fn header(inst: &Instance) -> String {
    header_as(inst, std::io::stdout().is_terminal())
}

pub fn header_as(inst: &Instance, short: bool) -> String {
    if short {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        format!(
            "{}  folder {}",
            inst.id,
            tilde(&inst.record.folder, home.as_deref())
        )
    } else {
        format!(
            "{}  template {} · folder {}",
            inst.id,
            inst.record.team_dir.display(),
            inst.record.folder.display()
        )
    }
}

/// `path` with a leading `home` written `~`.
pub(crate) fn tilde(path: &std::path::Path, home: Option<&std::path::Path>) -> String {
    // A home of `/`, or an empty one, would shorten every path.
    match home
        .filter(|h| h.parent().is_some())
        .and_then(|h| path.strip_prefix(h).ok())
    {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// What `status` and a start show for a role with no member yet.
fn not_created(role: &crate::team::Role) -> String {
    if role.max > 1 {
        ON_DEMAND.into()
    } else {
        format!("not created (start: {})", role.start)
    }
}

/// One node per member of the team (each under its parent role's
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
                cols: if inst.record.overrides.contains_key(role_name) {
                    role_choice(v, &inst.record, role_name)
                        .map(|member| vec![tree::vendor_model(&member), not_created(&member)])
                        .unwrap_or_else(|_| vec![not_created(role)])
                } else {
                    vec![not_created(role)]
                },
                look: tree::Look {
                    mark: None,
                    tone: tree::Tone::Faded,
                    italic: true,
                },
            });
        }
        for id in ids {
            nodes.push(tree::Node {
                cols: vec![
                    // Deletion also uses this topology when a choice no longer resolves.
                    // A stale choice must not hide the member's row.
                    member_role(v, &inst.record, &id)
                        .map(|member| tree::vendor_model(&member))
                        .unwrap_or_default(),
                    outcome(&id),
                ],
                id,
                parent: parent_of(role),
                look: tree::Look::default(),
            });
        }
    }
    nodes
}

/// The folders every `vendor`-posture member may write: the tree's
/// `messages/` and the team's `notes/`, never brg's own files. Each must be a real
/// folder (not a symlink), yours, and not writable by group or others, and
/// resolve to exactly its own place in the team's state folder: anything
/// else, or any error finding out, refuses the start.
fn shared_folders(state: &StateDir) -> Result<Vec<PathBuf>, String> {
    let refuse = |dir: &Path, why: String| {
        format!(
            "the team's {} folder {} {why}, so members can't be given it; nothing was started",
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
        let mut base = root.clone();
        if dir == state.messages_dir() {
            for _ in state.id.split('/').skip(1) {
                base.pop();
            }
        }
        let want = base.join(dir.file_name().unwrap_or_default());
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
/// folder or the team's own: Codex makes a member's folder writable.
fn check_state_overlap(folder: &Path, instance_state: &Path) -> Result<(), String> {
    for (what, dir) in [
        ("brg's state folder", state::state_home()),
        ("the team's state folder", instance_state.to_path_buf()),
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

/// Validate a template folder, printing what validation found; `Err` on errors.
fn validate_printing(dir: &Path) -> Result<Validated, String> {
    let (report, validated) = validate(dir);
    for diag in &report.diags {
        eprintln!("{diag}");
    }
    validated.ok_or_else(|| "validation failed; nothing was started".to_string())
}

/// Check the starting roles' API keys before anything is registered.
fn precheck(v: &Validated, roles: &[&crate::team::Role]) -> Result<(), String> {
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

/// `brg create <template> <folder>`: a new team from the template working in
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
    if has_member_context() {
        return Err("members cannot create an independent team; ask a person, or declare it as a team role and run brg create <team>/<role>".into());
    }
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
    precheck(&v, &starting)?;
    let values = Some(crate::secrets::values(&v, &v.team.name)?);
    check_state_overlap(&folder, &state::state_home())
        .map_err(|e| format!("the working {e}; nothing was created"))?;
    let lock = registry::lock()?;
    crate::checkouts::check_folder(&lock, &folder)?;
    let (state, mut record) =
        registry::create(&lock, &v.team.name, &v.team.dir, &folder, &utc_now())?;
    for role in &starting {
        record.issue_member(&role.id, None, |id| v.team.roles.contains_key(id))?;
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
            let role = member_role(&v, &inst.record, id)?;
            if let Some(folder) = create_worktree(&inst, &role, id, None, None)? {
                created_folders.push((id.clone(), folder));
            }
        }
        start_members(lock, &inst, &v, ids, true, false, values)
    })();
    if result.is_err() {
        unregister_if_never_started(&inst);
        remove_unkept_worktrees(&inst, created_folders);
        return result;
    }
    result.map(|outcome| name_worktrees(&inst, &created_folders, outcome))
}

/// The tree leaves worktrees to `--json`, so whatever adds one names it
/// after the tree: `<address>  folder <path>`, as `create` does for a child team.
fn name_worktrees(
    inst: &Instance,
    added: &[(String, PathBuf)],
    mut outcome: UpOutcome,
) -> UpOutcome {
    outcome.notes.extend(
        added
            .iter()
            .map(|(id, dir)| format!("{}  folder {}", inst.at(id), dir.display())),
    );
    outcome
}

/// After a `create` whose start failed before any window could start:
/// remove the new team again (its suffix stays issued). Decided under
/// the registry lock, from what is true then: another start may have
/// started the team since this one failed.
fn unregister_if_never_started(inst: &Instance) {
    rollback_pause();
    let Ok(lock) = registry::lock() else { return };
    let Ok(mut registry) = lock.load() else {
        return;
    };
    if !registry.instances.contains_key(&inst.id) {
        return;
    }
    // A record that can't be read may describe windows that started.
    let started =
        state::read_meta(&inst.state).map_or(true, |m| m.is_some_and(|m| m.windows_started));
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
/// it takes its team back, so a test can start the team meanwhile.
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

/// Whether `id` is one of the team's roles, agent or team.
pub(crate) fn is_role(v: &Validated, id: &str) -> bool {
    v.team.roles.contains_key(id) || v.team.team_roles.contains_key(id)
}

/// `brg create <team>/<role>`: a new member of the role, started; with
/// `name`, its ID is `<role>-<name>`.
pub fn create_member(
    mut inst: Instance,
    role: &str,
    name: Option<&str>,
    branch: Option<&str>,
    repo: Option<&str>,
    choices: &[String],
) -> Result<UpOutcome, String> {
    let v = validated(&inst)?;
    if let Some(name) = name {
        // Refuse a taken name before any fetch; issuing checks again under the lock.
        inst.record.named(role, name, |id| is_role(&v, id))?;
    }
    let worktree = v
        .team
        .team_roles
        .get(role)
        .map(|r| r.worktree)
        .or_else(|| v.team.roles.get(role).map(|r| r.worktree));
    if let Some(branch) = branch
        && worktree == Some(false)
    {
        return Err(format!(
            "`--branch {branch}` names a branch for a new worktree, but role `{role}` works in the team's folder (no `worktree: true`)"
        ));
    }
    if let Some(repo) = repo
        && worktree == Some(false)
    {
        return Err(format!(
            "`--repo {repo}` names a clone for a new worktree, but role `{role}` works in the team's folder (no `worktree: true`)"
        ));
    }
    if let Some(team_role) = v.team.team_roles.get(role) {
        check_parent_authority(&inst, role, Some(&v), None)?;
        let overrides = if choices.is_empty() {
            Default::default()
        } else {
            let child = crate::nesting::validate_child(&inst, &team_role.dir)?;
            crate::overrides::child_choices(choices, role, &child)?
        };
        create_authority_pause();
        let start = repo
            .map(|name| crate::worktree::fetch(&crate::worktree::root_folder(&inst.id)?, name))
            .transpose()?;
        return crate::nesting::create(inst, &v, role, name, branch, start.as_ref(), overrides);
    }
    let Some(member) = v.team.roles.get(role) else {
        return Err(format!(
            "team `{}`'s template has no role `{role}`; its roles: {}",
            inst.id,
            v.team.roles.keys().cloned().collect::<Vec<_>>().join(", ")
        ));
    };
    let overrides = crate::overrides::agent_choice(choices, &inst.at(role))?;
    let member = inst
        .record
        .overrides
        .get(role)
        .cloned()
        .unwrap_or_default()
        .merge(&overrides)
        .resolve(member, v.auth())?;
    precheck(&v, &[&member])?;
    let values = (!is_running(&inst))
        .then(|| crate::secrets::values(&v, &inst.id))
        .transpose()?;
    check_parent_authority(&inst, role, Some(&v), None)?;
    create_authority_pause();
    let start = repo
        .map(|name| crate::worktree::fetch(&crate::worktree::root_folder(&inst.id)?, name))
        .transpose()?;
    let lock = registry::lock()?;
    inst.reload()?;
    crate::nesting::check_not_stopping(&inst)?;
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
    let id = inst.record.issue_member(role, name, |id| is_role(&v, id))?;
    inst.record
        .members
        .get_mut(&id)
        .expect("issued member")
        .overrides = overrides;
    let added: Vec<_> = create_worktree(&inst, &member, &id, branch, start.as_ref())?
        .map(|dir| (id.clone(), dir))
        .into_iter()
        .collect();
    lock.save_instance(&inst.state, &inst.record)?;
    let mut outcome = start_members(lock, &inst, &v, vec![id.clone()], false, false, values)?;
    if let Some(start) = start {
        outcome.tree = outcome
            .tree
            .lines()
            .enumerate()
            .map(|(index, line)| {
                if index > 0 && line.contains(&id) {
                    format!("{line}{}\n", start.detail())
                } else {
                    format!("{line}\n")
                }
            })
            .collect();
    }
    Ok(name_worktrees(&inst, &added, outcome))
}

/// Pause after the preliminary create check, before taking the registry
/// lock. Tests can restart the caller here to exercise the locked recheck.
#[cfg(feature = "test-support")]
fn create_authority_pause() {
    let Some(path) = std::env::var_os("BORGCHART_TEST_CREATE_AUTHORITY_PAUSE").map(PathBuf::from)
    else {
        return;
    };
    let _ = std::fs::write(path.with_extension("waiting"), "");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(not(feature = "test-support"))]
fn create_authority_pause() {}

/// Add a new member's worktree, for a `worktree: true` role, from the
/// team's folder at its `HEAD` (detached, or on a new `branch`); an
/// existing one is kept as is. The caller holds the registry lock. Returns
/// the new worktree so an unkept member's failed start can remove it.
fn create_worktree(
    inst: &Instance,
    role: &crate::team::Role,
    id: &str,
    branch: Option<&str>,
    start: Option<&crate::worktree::Start>,
) -> Result<Option<PathBuf>, String> {
    if !role.worktree {
        return Ok(None);
    }
    let dir = crate::worktree::path(&inst.at(id));
    if dir.is_dir() {
        return Ok(None);
    }
    let root = crate::worktree::root_folder(&inst.id)?;
    crate::worktree::add(&root, &inst.record.folder, &dir, branch, start)
        .map_err(|e| format!("`{id}` needs its own worktree: {e}"))?;
    Ok(Some(dir))
}

fn remove_unkept_worktrees(inst: &Instance, created: Vec<(String, PathBuf)>) {
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
    for (id, dir) in created {
        if saved
            .as_ref()
            .is_some_and(|record| record.role_of(&id).is_some())
        {
            continue;
        }
        if let Ok(root) = crate::worktree::root_folder(&inst.id) {
            let _ = crate::worktree::remove(&root, &dir);
        }
    }
}

/// Member authority follows recorded parent edges, at any depth. Creation
/// checks early, then every change repeats the check under the registry lock.
pub(crate) fn check_parent_authority(
    inst: &Instance,
    role: &str,
    validated_team: Option<&Validated>,
    target: Option<&str>,
) -> Result<(), String> {
    let creating = target.is_none();
    if !has_member_context() && !creating {
        return Ok(());
    }
    let recorded = match target {
        Some(id) => state::read_meta(&inst.state)?.and_then(|m| m.members.get(id).cloned()),
        None => None,
    };
    let parent = if let Some(recorded) = recorded {
        recorded.parent.map(|parent| parent_address(inst, &parent))
    } else {
        let loaded;
        let v = match validated_team {
            Some(v) => v,
            None => {
                loaded = validated(inst)?;
                &loaded
            }
        };
        if !creating {
            return Err(format!(
                "member `{}` has no recorded parent; ask a person to manage it",
                inst.at(target.expect("existing member"))
            ));
        }
        let parent_role = v.team.roles.get(role).map(|r| r.parent.as_deref())
            .or_else(|| v.team.team_roles.get(role).map(|r| r.parent.as_deref()))
            .ok_or_else(|| {
            format!(
                "role `{role}` is no longer in the team; ask a person to stop or delete its members"
            )
        })?;
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
            .map(|parent| inst.at(&parent))
            .or_else(|| inst.record.parent.clone())
    };
    check_subtree_authority(parent, &inst.at(target.unwrap_or(role)))
}

pub(crate) fn has_member_context() -> bool {
    [context::ENV_MEMBER, context::ENV_INCARNATION]
        .iter()
        .any(|key| std::env::var(key).is_ok_and(|value| !value.is_empty()))
}

fn parent_address(inst: &Instance, parent: &str) -> String {
    if parent.contains('/') {
        parent.to_string()
    } else {
        inst.at(parent)
    }
}

/// A child team belongs below its recorded parent member, never below its roots.
fn check_instance_authority(inst: &Instance) -> Result<(), String> {
    if !has_member_context() {
        return Ok(());
    }
    check_subtree_authority(inst.record.parent.clone(), &inst.id)
}

fn check_subtree_authority(mut parent: Option<String>, target: &str) -> Result<(), String> {
    let context::Caller::Member {
        member,
        incarnation,
    } = context::caller(None).map_err(|e| e.message)?
    else {
        return Ok(());
    };
    // Targets may cross team boundaries; the caller's start is always
    // checked in its own team, never in the target's store.
    let own = context::instance(None).map_err(|e| e.message)?;
    let caller = address::instance(&own)?;
    let current = lifecycle::load_incarnation(&caller.state, &member)?;
    if caller.record.role_of(&member).is_none()
        || !current.is_some_and(|inc| {
            inc.member == member
                && inc.incarnation == incarnation
                && inc.ended_at.is_none()
                && inc
                    .pane
                    .is_some_and(|pane| lifecycle::pane_liveness(&pane) == Liveness::Alive)
        })
    {
        return Err(format!(
            "`{own}/{member}`'s start is incomplete, replaced or stopped; only its latest running start may manage its subtree"
        ));
    }
    let caller_address = caller.at(&member);
    let refusal = format!(
        "`{target}` is outside `{caller_address}`'s subtree; ask a person or an ancestor member to manage it"
    );
    if target == caller_address {
        return Err(refusal);
    }
    let mut seen = std::collections::BTreeSet::new();
    while let Some(address) = parent {
        if address == caller_address {
            return Ok(());
        }
        if !seen.insert(address.clone()) {
            break;
        }
        let Some((instance, member)) = address.rsplit_once('/') else {
            break;
        };
        let ancestor = address::instance(instance)?;
        if ancestor.record.role_of(member).is_none() {
            break;
        }
        let meta = state::read_meta(&ancestor.state)?.ok_or_else(|| {
            format!(
                "team `{instance}`'s start record cannot be read; ask a person to manage `{target}`"
            )
        })?;
        let recorded = meta.members.get(member).ok_or_else(|| {
            format!("member `{address}` has no recorded parent; ask a person to manage `{target}`")
        })?;
        parent = recorded
            .parent
            .as_ref()
            .map(|p| parent_address(&ancestor, p));
    }
    Err(refusal)
}

/// Refuse when the team's start.json exists but can't be read: brg
/// can't tell which tmux server runs it, so it can't tell that it stopped.
fn unreadable_start_record(inst: &Instance, consequence: &str) -> Result<(), String> {
    match state::read_meta(&inst.state) {
        Ok(_) => Ok(()),
        Err(e) => Err(format!(
            "team `{}`'s start record can't be read ({e}), so brg can't tell that it stopped; {consequence}",
            inst.id
        )),
    }
}

/// Whether any member or plugin process of the team has a live window
/// on the server its start recorded.
pub fn is_running(inst: &Instance) -> bool {
    let tmux = tmux_for(&inst.state);
    tmux.windows(&inst.state.session()).iter().any(|w| !w.dead)
}

/// `brg start <team> --template <path>`: point a stopped team at its
/// template folder's new place.
fn repoint(inst: &mut Instance, team: &Path) -> Result<(), String> {
    let dir = team
        .canonicalize()
        .map_err(|_| format!("{} does not exist", team.display()))?;
    if !dir.join(crate::team::TEAM_FILE).is_file() {
        return Err(crate::team::missing_template(&dir));
    }
    let name = crate::team::team_name(&dir);
    if name != inst.record.team {
        return Err(format!(
            "{} is template `{name}`, not team `{}`'s template `{}`",
            dir.display(),
            inst.id,
            inst.record.team
        ));
    }
    let lock = registry::lock()?;
    inst.reload()?;
    crate::nesting::check_not_stopping(inst)?;
    check_instance_authority(inst)?;
    unreadable_start_record(inst, "its template folder can't be changed")?;
    if is_running(inst) {
        return Err(format!(
            "team `{}` is running; a new template folder applies only from a full stop: brg stop {}",
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
    for template in v.team.roles.values().filter(|m| m.start) {
        let role = role_choice(v, &inst.record, &template.id)?;
        let members = inst.record.members_of(&role.id);
        if members.is_empty() {
            let id = inst
                .record
                .issue_member(&role.id, None, |id| v.team.roles.contains_key(id))?;
            if let Some(folder) = create_worktree(inst, &role, &id, None, None)? {
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
    // Only members whose role is still in the team.
    Ok(ids
        .into_iter()
        .filter(|id| {
            inst.record
                .role_of(id)
                .is_some_and(|r| v.team.roles.contains_key(r))
        })
        .collect())
}

/// Why a start refuses a team whose messages can't be read.
fn unreadable_store(inst: &Instance, e: &str) -> String {
    format!(
        "team `{}`'s messages can't be read ({e}), so nothing was started; create a new team instead (`brg create`), and send its open work again",
        inst.id
    )
}

/// The session `member`'s latest start reported, if it can be resumed.
fn recorded_session(state: &StateDir, member: &str) -> Option<String> {
    let text = std::fs::read_to_string(state.member_session(member)).ok()?;
    let id = text.trim();
    crate::gate::usable_session_id(id).then(|| id.to_string())
}

/// `brg start <team>[/<member>] [--template <path>] [--resume]`.
pub fn start(
    inst: Instance,
    member: Option<&str>,
    team: Option<&Path>,
    resume: bool,
) -> Result<UpOutcome, String> {
    start_instance(inst, member, team, true, resume)
}

fn start_instance(
    mut inst: Instance,
    member: Option<&str>,
    team: Option<&Path>,
    restore_copies: bool,
    resume: bool,
) -> Result<UpOutcome, String> {
    if let Some(dir) = team {
        if member.is_some() {
            return Err(
                "`--template` points the whole team at a template folder; leave the member out"
                    .into(),
            );
        }
        repoint(&mut inst, dir)?;
    }
    let member = member.map(|m| address::member(&inst, m)).transpose()?;
    if !inst.record.team_dir.join(crate::team::TEAM_FILE).is_file() {
        return Err(format!(
            "team `{}`'s template folder {} is gone; if it moved, point the team at it: brg start {} --template <path>",
            inst.id,
            inst.record.team_dir.display(),
            inst.id
        ));
    }
    if !inst.record.folder.is_dir() {
        return Err(format!(
            "team `{}`'s working folder {} is gone; create a new team (`brg create`) for another folder",
            inst.id,
            inst.record.folder.display()
        ));
    }
    let fresh = state::read_meta(&inst.state)?.is_none_or(|meta| meta.stopped_at.is_some())
        || !tmux_for(&inst.state).has_session(&inst.state.session());
    let v = if member.is_none() && (fresh || inst.id.contains('/')) {
        validate_printing(&inst.record.team_dir)?
    } else {
        validated(&inst)?
    };
    let values = fresh
        .then(|| crate::secrets::values(&v, &inst.id))
        .transpose()?;
    let lock = registry::lock()?;
    inst.reload()?;
    crate::nesting::check_not_stopping(&inst)?;
    if let Some(id) = &member {
        address::member(&inst, id)?;
        check_parent_authority(&inst, inst.record.role_of(id).unwrap(), Some(&v), Some(id))?;
    } else {
        check_instance_authority(&inst)?;
    }
    match member {
        Some(id) => start_members(lock, &inst, &v, vec![id], false, resume, values),
        None => {
            // Check every role and kept member this full start brings back,
            // before any role can issue an ID or add a worktree.
            let windows = tmux_for(&inst.state).windows(&inst.state.session());
            for (id, member) in inst.record.kept_members() {
                if v.team.roles.get(&member.role).is_some_and(|role| {
                    role.start
                        || inst
                            .record
                            .running_at_stop
                            .as_ref()
                            .is_some_and(|set| set.contains(id))
                        || windows.iter().any(|w| !w.dead && &w.name == id)
                }) {
                    member_role(&v, &inst.record, id)?;
                }
            }
            for role in v.team.roles.values().filter(|role| role.start) {
                role_choice(&v, &inst.record, &role.id)?;
            }
            if let Some((parent, _)) = inst.id.rsplit_once('/') {
                crate::nesting::validate_child(&address::instance(parent)?, &v.team.dir)?;
            }
            let mut created_folders = Vec::new();
            let result = (|| {
                let ids = full_start_members(&lock, &mut inst, &v, &mut created_folders)?;
                start_members(lock, &inst, &v, ids, true, resume, values)
            })();
            if result.is_err() {
                remove_unkept_worktrees(&inst, created_folders);
                return result;
            }
            let mut outcome = name_worktrees(&inst, &created_folders, result?);
            if !restore_copies {
                return Ok(outcome);
            }
            let registered = registry::load()?;
            let mut copies: Vec<_> = inst
                .record
                .copies_running_at_stop
                .iter()
                .flatten()
                .collect();
            copies.sort_by_key(|id| id.split('/').count());
            for id in copies {
                let full_id = inst.at(id);
                if !registered.instances.contains_key(&full_id) {
                    continue;
                }
                let copy = address::instance(&full_id);
                if copy.as_ref().is_ok_and(is_running) {
                    continue;
                }
                match copy.and_then(|copy| start_instance(copy, None, None, false, resume)) {
                    Ok(copy) => {
                        outcome.tree.push_str(&copy.tree);
                        outcome.notes.extend(copy.notes);
                        outcome.failed |= copy.failed;
                    }
                    Err(error) => {
                        outcome.notes.push(format!(
                            "{full_id}: {}; start it with: brg start {full_id}",
                            error
                                .replace(" and run the command again; nothing was started", "")
                                .replace("; run the command again; nothing was started", "")
                        ));
                        outcome.failed = true;
                    }
                }
            }
            Ok(outcome)
        }
    }
}

/// Whether each member's current start may still be running, as `stop`
/// and liveness see it: `Gone` only when that is certain.
pub fn liveness_of(inst: &Instance) -> BTreeMap<String, Liveness> {
    let state = &inst.state;
    let session = state.session();
    let read = state::read_meta(state);
    let unreadable = read.is_err();
    let meta = read.ok().flatten();
    let rows = meta.as_ref().map(|m| lifecycle::Server::of(m).panes());
    let mut out = BTreeMap::new();
    for (id, _) in inst.record.kept_members() {
        let incarnation = lifecycle::load_incarnation(state, id);
        let by_window = match (&meta, &rows) {
            // The start record can't be read: which server runs it is unknown.
            (None, _) if unreadable => Liveness::Unknown,
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
            (Some(m), _) if m.stopped_at.is_some() || !m.windows_started => Liveness::Gone,
            (Some(m), Some(Err(_))) if lifecycle::no_live_start(m, id, &incarnation) => {
                Liveness::Gone
            }
            (Some(_), Some(Err(_))) if inst.record.stopping => match &incarnation {
                Ok(Some(inc)) => inc
                    .pane
                    .as_ref()
                    .map(lifecycle::pane_liveness)
                    .unwrap_or(Liveness::Unknown),
                _ => Liveness::Unknown,
            },
            _ => Liveness::Unknown,
        };
        let by_record = match incarnation {
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

/// `brg delete <team>/<member>`: discard a stopped member's live state.
/// Its issued ID and immutable messages stay as history. Registry, lifecycle
/// and publication locks cover the unread check and removal from routing.
pub fn delete_member(mut inst: Instance, name: &str, force: bool) -> Result<Vec<String>, String> {
    let lock = registry::lock()?;
    let _state_lock = lifecycle::lock_state(&inst.state)?;
    inst.reload()?;
    let id = address::member(&inst, name)?;
    check_parent_authority(&inst, inst.record.role_of(&id).unwrap(), None, Some(&id))?;
    let copies: Vec<_> = crate::nesting::descendants(&inst)?
        .into_iter()
        .filter(|copy| {
            copy.instance.as_ref().map_or(true, |copy| {
                copy.record.parent.as_deref() == Some(&inst.at(&id))
            })
        })
        .map(|copy| copy.id)
        .collect();
    if !copies.is_empty() {
        return Err(format!(
            "member `{}` has kept child teams; delete them first:\n  {}",
            inst.at(&id),
            copies.join("\n  ")
        ));
    }
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
    let meta = state::read_meta(&inst.state)?;
    if !team_loaded
        && meta.as_ref().is_none_or(|meta| {
            inst.record
                .kept_members()
                .any(|(other, _)| other != &id && !meta.members.contains_key(other))
        })
    {
        return Err(format!(
            "brg can't tell whether member `{}` has kept children, so it can't be deleted, even with --force; restore its template and start the team: brg start {} --template <path>",
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
                    "\nthe template file changed since the last start; brg start {} applies it",
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
        lifecycle::lock_bus(&inst.state)?
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
        let work = lifecycle::unacked_work(&bus, &snap, Some(&id));
        Ok(work)
    })();
    match work {
        Ok(work) if work.items.is_empty() || force => {}
        Ok(work) => {
            return Err(format!(
                "member `{}` holds {} open message{}; settle them, or delete it anyway with --force:\n  {}",
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
    let mut lines = Vec::new();
    let worktrees = worktrees_to_remove(
        &[(crate::worktree::root_folder(&inst.id)?, inst.at(&id))],
        force,
        &mut lines,
    )?;
    let issued = inst.record.members.get_mut(&id).expect("resolved member");
    issued.deleted = true;
    issued.cwd = None;
    if let Some(running) = &mut inst.record.running_at_stop {
        running.remove(&id);
    }
    // Save the tombstone first: even a partial cleanup cannot restore routing.
    lock.save_instance(&inst.state, &inst.record)?;
    if let Some(mut meta) = state::read_meta(&inst.state)? {
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
    lines.insert(0, format!("deleted {}; its ID stays issued", inst.at(&id)));
    remove_worktrees(worktrees, &mut lines);
    Ok(lines)
}

/// Before a delete, the worktrees brg added for `targets` (each its tree's
/// top folder, and the member's or child team's address) that it can
/// remove afterwards. One with work in it (or that git can't check) refuses
/// the delete, or with `force` stays where it is, which `lines` says.
fn worktrees_to_remove(
    targets: &[(PathBuf, String)],
    force: bool,
    lines: &mut Vec<String>,
) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    let mut remove = Vec::new();
    let mut refused = Vec::new();
    for (from, address) in targets {
        let dir = crate::worktree::path(address);
        let why = match crate::worktree::unsaved(from, &dir) {
            Ok(None) => {
                if dir.symlink_metadata().is_ok() {
                    remove.push((from.clone(), dir));
                }
                continue;
            }
            Ok(Some(why)) => why,
            Err(e) => format!("a state git can't check ({e})"),
        };
        if force {
            lines.push(format!(
                "left {address}'s worktree {} as it is: it has {why}",
                dir.display()
            ));
        } else {
            refused.push(format!("{address}'s worktree {} has {why}", dir.display()));
        }
    }
    if refused.is_empty() {
        Ok(remove)
    } else {
        Err(format!(
            "work would be lost, so nothing was deleted; push or remove it, or delete anyway with --force, which leaves these worktrees as they are:\n  {}",
            refused.join("\n  ")
        ))
    }
}

/// Remove worktrees [`worktrees_to_remove`] found clean, saying so in `lines`.
fn remove_worktrees(worktrees: Vec<(PathBuf, PathBuf)>, lines: &mut Vec<String>) {
    for (from, dir) in worktrees {
        lines.push(match crate::worktree::remove(&from, &dir) {
            Ok(()) => format!("removed worktree {}", dir.display()),
            Err(e) => format!(
                "worktree {} could not be removed ({e}); brg cleanup tries again",
                dir.display()
            ),
        });
    }
}

/// `brg delete <team> [--force]`: remove a stopped team's state
/// folder (messages, notes, brg's files) and its registration. Its working
/// folder is never touched.
pub fn delete(mut inst: Instance, force: bool) -> Result<Vec<String>, String> {
    let lock = registry::lock()?;
    inst.reload()?;
    check_instance_authority(&inst)?;
    let state_lock = lifecycle::lock_state(&inst.state)?;
    let copies = crate::nesting::descendants(&inst)?;
    if !copies.is_empty() {
        return Err(format!(
            "team `{}` has kept child teams; delete them first:\n  {}",
            inst.id,
            copies
                .iter()
                .map(|copy| copy.id.as_str())
                .collect::<Vec<_>>()
                .join("\n  ")
        ));
    }
    if inst.record.stopping {
        crate::nesting::check_not_stopping(&inst)?;
    }
    unreadable_start_record(&inst, "it can't be deleted, even with --force")?;
    let live = liveness_of(&inst);
    let alive: Vec<&String> = live
        .iter()
        .filter(|(_, l)| **l == Liveness::Alive)
        .map(|(id, _)| id)
        .collect();
    if !alive.is_empty() || is_running(&inst) {
        return Err(format!(
            "team `{}` is running{}; stop it first: brg stop {}",
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
            "brg can't tell that {} of team `{}` stopped, so it can't be deleted, even with --force; stop it again once its tmux server can be reached: brg stop {}",
            unknown
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            inst.id,
            inst.id
        ));
    }
    // Serialize the open-work check and tombstone with publications anywhere in the tree.
    let paths = StorePaths::new(&inst.state);
    let bus_lock = if paths.identity().exists() {
        lifecycle::lock_bus(&inst.state)?
    } else {
        None
    };
    let work = (|| {
        let bus = match store::open(&inst.state) {
            Ok(bus) => bus,
            Err(f) if f.code == crate::bus::outcome::Code::NotConfigured => {
                return Ok(lifecycle::OpenWork::default());
            }
            Err(f) => return Err(f.message),
        };
        let snap = store::scan(&bus.paths);
        if !snap.healthy() {
            return Err("messages are incomplete".into());
        }
        Ok(lifecycle::unacked_work(&bus, &snap, None))
    })();
    match work {
        Ok(work) if work.items.is_empty() || force => {}
        Ok(work) => {
            return Err(format!(
                "team `{}` holds open work; settle it, or delete it anyway with --force:\n  {}",
                inst.id,
                work.items.join("\n  ")
            ));
        }
        Err(_) if force => {}
        Err(e) => {
            return Err(format!(
                "team `{}`'s messages can't be read ({e}), so its contents can't be inspected; delete it anyway with --force",
                inst.id
            ));
        }
    }
    let mut lines = Vec::new();
    let root = crate::worktree::root_folder(&inst.id)?;
    let mut targets: Vec<(PathBuf, String)> = inst
        .record
        .kept_members()
        .map(|(id, _)| (root.clone(), inst.at(id)))
        .collect();
    if inst.id.contains('/') {
        targets.push((root, inst.id.clone()));
    }
    let worktrees = worktrees_to_remove(&targets, force, &mut lines)?;
    #[cfg(feature = "test-support")]
    store::mutation_pause("BORGCHART_TEST_DELETE_PAUSE").map_err(|e| e.message)?;
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
    drop(bus_lock);
    drop(state_lock);
    std::fs::remove_dir_all(&inst.state.root)
        .map_err(|e| format!("cannot remove {}: {e}", inst.state.root.display()))?;
    let own_worktree = crate::worktree::path(&inst.id);
    lines.insert(
        0,
        if worktrees.iter().any(|(_, dir)| dir == &own_worktree) {
            format!("deleted {}", inst.id)
        } else {
            format!(
                "deleted {}; its working folder {} was left as it is",
                inst.id,
                inst.record.folder.display()
            )
        },
    );
    remove_worktrees(worktrees, &mut lines);
    Ok(lines)
}

/// `brg destroy <root>`: stop the whole tree, then forget it: every
/// registration and the root's state folder (all child teams' state, notes and
/// the tree's messages), and the clean worktrees brg added for it. No open
/// work refuses it; anything brg can't confirm stopped does. The working
/// folders, checkouts and other worktrees stay. `warn` gets
/// the warning line once the target is accepted, before anything stops.
/// The tree is forgotten even when some folders couldn't be checked or
/// removed; the outcome then says so and fails.
pub fn reset(inst: Instance, warn: impl FnOnce(String)) -> Result<crate::cleanup::Outcome, String> {
    if has_member_context() {
        return Err(
            "`destroy` stops a whole tree, the member running it included; run it from your own shell"
                .into(),
        );
    }
    if inst.id.contains('/') {
        return Err(format!(
            "`{}` is a child team; destroy takes a tree's root. For a child team, use brg stop -r and brg delete",
            inst.id
        ));
    }
    warn(format!(
        "destroying {}: this deletes its notes and messages and every child team's; save anything you need from {} first",
        inst.id,
        inst.state.root.display()
    ));
    let stopped = stop(&inst, None)?;
    let lock = registry::lock()?;
    let mut root = inst;
    root.reload()?;
    let mut tree = Vec::new();
    for copy in crate::nesting::descendants(&root)? {
        tree.push(copy.instance.map_err(|e| {
            format!(
                "child team `{}` can't be read ({e}); nothing was deleted. Restore it, then: brg destroy {}",
                copy.id, root.id
            )
        })?);
    }
    tree.push(root.clone());
    let mut state_locks = Vec::new();
    for inst in &tree {
        state_locks.push(lifecycle::lock_state(&inst.state)?);
        unreadable_start_record(inst, "nothing was deleted")?;
        let live = liveness_of(inst);
        let unsure: Vec<&String> = live
            .iter()
            .filter(|(_, l)| **l != Liveness::Gone)
            .map(|(id, _)| id)
            .collect();
        if inst.record.stopping || is_running(inst) || !unsure.is_empty() {
            return Err(format!(
                "brg can't tell that `{}` stopped{}, so nothing was deleted; stop it again once its tmux server can be reached: brg stop {}",
                inst.id,
                if unsure.is_empty() {
                    String::new()
                } else {
                    format!(
                        " ({})",
                        unsure
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                },
                root.id
            ));
        }
    }
    // Publications anywhere in the tree wait here, then find it gone.
    let bus_lock = lifecycle::lock_bus(&root.state)?;
    let mut registry = lock.load()?;
    let mut out = crate::cleanup::tree_folders(&tree, &registry);
    out.lines.splice(0..0, stopped);
    for inst in &tree {
        registry.instances.remove(&inst.id);
    }
    lock.save(&registry)?;
    std::fs::remove_dir_all(&root.state.root).map_err(|e| {
        format!(
            "{} is forgotten, but its state folder {} could not be removed ({e}); remove it with: brg cleanup",
            root.id,
            root.state.root.display()
        )
    })?;
    drop(bus_lock);
    drop(state_locks);
    out.lines.push(format!(
        "destroyed {}: its registrations, state, notes and messages are deleted; its working folder {} was left as it is",
        root.id,
        root.record.folder.display()
    ));
    out.lines.push(format!(
        "start it fresh: brg create {} {}",
        state::shell_quote(&root.record.team_dir.to_string_lossy()),
        state::shell_quote(&root.record.folder.to_string_lossy())
    ));
    Ok(out)
}

/// `brg secrets <template>`: each secret its plugins need (plugin, name), and
/// its source and whether the caller's environment sets it. No value is read out.
#[derive(Serialize)]
pub struct RequiredSecret {
    pub plugin: String,
    pub name: String,
    pub source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set: Option<bool>,
}

pub fn secrets(team: &str) -> Result<Vec<RequiredSecret>, String> {
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
                .map(|n| {
                    let (source, set) = if p.sources.contains_key(n) {
                        ("resolver", None)
                    } else {
                        ("environment", Some(std::env::var_os(n).is_some()))
                    };
                    RequiredSecret {
                        plugin: p.name.clone(),
                        name: n.clone(),
                        source,
                        set,
                    }
                })
                .collect::<Vec<_>>()
        })
        .collect())
}

/// One template in `brg templates`, with its teams.
#[derive(Debug, Serialize)]
pub struct Listed {
    /// `None` for teams whose template is outside the search paths.
    #[serde(rename = "template")]
    pub team: Option<String>,
    #[serde(rename = "template_dir")]
    pub dir: Option<PathBuf>,
    #[serde(rename = "teams")]
    pub instances: Vec<ListedInstance>,
}

#[derive(Debug, Serialize)]
pub struct ListedInstance {
    #[serde(rename = "team")]
    pub instance: String,
    #[serde(rename = "template")]
    pub team: String,
    #[serde(rename = "template_dir")]
    pub team_dir: PathBuf,
    pub folder: PathBuf,
    pub running: bool,
    pub created_at: String,
}

/// `brg templates`: the templates in the search paths, each with its teams, then
/// teams whose template is elsewhere.
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

/// `brg templates` as text.
pub fn render_ls(listed: &[Listed]) -> String {
    let mut out = String::new();
    for l in listed {
        match (&l.team, &l.dir) {
            (Some(name), Some(dir)) => out.push_str(&format!("{name}  {}\n", dir.display())),
            _ => out.push_str("teams of templates outside the search paths\n"),
        }
        if l.instances.is_empty() {
            out.push_str("  (no teams)\n");
        }
        for i in &l.instances {
            out.push_str(&format!(
                "  {}  {}  folder {}{}\n",
                i.instance,
                if i.running { "running" } else { "stopped" },
                i.folder.display(),
                if l.team.is_none() {
                    format!("  template {}", i.team_dir.display())
                } else {
                    String::new()
                }
            ));
        }
    }
    if out.is_empty() {
        out.push_str("no templates in the search paths and no teams\n");
    }
    out
}

/// After a window failed to start: when `nothing_started` (a fresh start
/// that created no window of the team), the team is as stopped as
/// before, so `stop`, liveness and `delete` aren't left unsure of a server
/// that never ran it.
fn unrecord_windows(state: &StateDir, meta: &mut Meta, nothing_started: bool, before: bool) {
    if !nothing_started {
        return;
    }
    meta.windows_started = before;
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
    if !meta.windows_started {
        meta.windows_started = true;
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

/// `brg stop <team>[/<member>]`.
///
/// Works on the tmux server recorded at the start, refusing a caller whose
/// `BORGCHART_TMUX_SOCKET` names another. Every full stop persists a cascade;
/// an absent server requires any recorded pane processes to have exited. A member's
/// incarnation is marked ended, and launch files and secrets removed, only
/// once its pane was stopped or confirmed gone. A full stop records which members were
/// running, for the next `start`; stopping a stopped one changes nothing.
pub fn stop(inst: &Instance, only: Option<&str>) -> Result<Vec<String>, String> {
    let lock = registry::lock()?;
    let mut current = inst.clone();
    current.reload()?;
    let inst = &current;
    if only.is_some() {
        return stop_one(&lock, inst, only);
    }
    check_instance_authority(inst)?;
    let descendants = crate::nesting::descendants(inst)?;
    let mut copies = Vec::new();
    let mut errors = Vec::new();
    for copy in descendants {
        match copy.instance {
            Ok(copy) => copies.push(copy),
            Err(error) => errors.push(format!(
                "{}: {error}; restore its state folder and team.json, then retry: brg stop {}",
                copy.id, inst.id
            )),
        }
    }
    mark_cascade(&lock, inst, &copies)?;
    drop(lock);
    stop_pause();
    copies.push(inst.clone());
    let mut messages = Vec::new();
    let nested = copies.len() > 1;
    for mut copy in copies {
        let lock = registry::lock()?;
        finish_marked(
            &lock,
            &mut copy,
            &inst.id,
            nested,
            &mut messages,
            &mut errors,
        )?;
    }
    if errors.is_empty() {
        Ok(messages)
    } else {
        messages.extend(errors);
        Err(messages.join("\n"))
    }
}

/// Persist D1's child team cascade while the caller holds the registry lock.
fn mark_cascade(lock: &registry::Lock, inst: &Instance, copies: &[Instance]) -> Result<(), String> {
    for target in std::iter::once(inst).chain(copies.iter()) {
        check_stop_socket(target)?;
    }
    forget_copy_restore(lock, inst)?;
    // Mark the enclosing team first. Even an interruption while writing
    // descendant records fences the entire subtree through its ancestor.
    for target in std::iter::once(inst).chain(copies.iter().rev()) {
        mark_stopping(lock, target)?;
    }
    Ok(())
}

/// Finish one persisted child team step. Plain full stops release the registry lock
/// between steps; recursive member stops keep it for their entire walk.
fn finish_marked(
    lock: &registry::Lock,
    copy: &mut Instance,
    root: &str,
    nested: bool,
    messages: &mut Vec<String>,
    errors: &mut Vec<String>,
) -> Result<(), String> {
    copy.reload()?;
    // Another stop may have finished this team while the lock was
    // released. Never stop a new start after its cascade has finished.
    if !copy.record.stopping {
        return Ok(());
    }
    let result = stop_one(lock, copy, None);
    let confirmed = result.is_ok();
    match result {
        Ok(own) if nested => messages.extend(
            own.into_iter()
                .map(|message| format!("{}: {message}", copy.id)),
        ),
        Ok(own) => messages.extend(own),
        Err(error) => errors.push(format!("{}: {error}", copy.id)),
    }
    let stopped = liveness_of(copy)
        .values()
        .all(|live| *live == Liveness::Gone);
    let descendants_stopped = crate::nesting::descendants(copy)?.iter().all(|child| {
        child
            .instance
            .as_ref()
            .is_ok_and(|child| !child.record.stopping)
    });
    if confirmed && stopped && descendants_stopped && !is_running(copy) {
        copy.reload()?;
        copy.record.stopping = false;
        lock.save_instance(&copy.state, &copy.record)?;
    } else {
        errors.push(format!(
            "team `{}` is still stopping; finish its cascade: brg stop {}",
            copy.id, root
        ));
    }
    Ok(())
}

/// One member stop or a complete existing D1 child team cascade.
enum SubtreeStop {
    Member(String),
    Copy {
        root: Box<Instance>,
        descendants: Vec<Instance>,
    },
}

/// Collect recorded parent edges, not roles from a possibly edited template file.
fn member_stop_walk(
    inst: &Instance,
    meta: &Meta,
    member: &str,
    copies: &[Instance],
    seen: &mut BTreeSet<String>,
    walk: &mut Vec<SubtreeStop>,
) -> Result<(), String> {
    if !seen.insert(member.to_string()) {
        return Err(format!("recorded parent cycle at `{}`", inst.at(member)));
    }
    let parent = inst.at(member);
    for (child, _) in inst.record.kept_members() {
        if meta
            .members
            .get(child)
            .and_then(|m| m.parent.as_ref())
            .is_some_and(|p| parent_address(inst, p) == parent)
        {
            member_stop_walk(inst, meta, child, copies, seen, walk)?;
        }
    }
    for root in copies
        .iter()
        .filter(|copy| copy.record.parent.as_deref() == Some(&parent))
    {
        let descendants = crate::nesting::descendants(root)?
            .into_iter()
            .map(|copy| copy.instance.map_err(|error| format!(
                "{}: {error}; restore its state folder and team.json, then retry: brg stop {}", copy.id, root.id)))
            .collect::<Result<Vec<_>, _>>()?;
        walk.push(SubtreeStop::Copy {
            root: Box::new(root.clone()),
            descendants,
        });
    }
    walk.push(SubtreeStop::Member(member.to_string()));
    Ok(())
}

/// Stop a member and everything below it without allowing new children
/// between steps. No team state lock spans two nodes in this walk.
pub fn stop_recursive(inst: &Instance, member: &str) -> Result<Vec<String>, String> {
    let lock = registry::lock()?;
    let mut inst = inst.clone();
    inst.reload()?;
    address::member(&inst, member)?;
    check_parent_authority(
        &inst,
        inst.record.role_of(member).unwrap(),
        None,
        Some(member),
    )?;
    let meta = state::read_meta(&inst.state)?
        .ok_or_else(|| format!("team `{}`'s start record cannot be read", inst.id))?;
    let copies = inst
        .record
        .copies
        .iter()
        .filter(|(_, copy)| !copy.deleted)
        .map(|(id, _)| {
            let copy = inst.at(id);
            address::instance(&copy).map_err(|error| format!(
                "{copy}: {error}; restore its state folder and team.json, then retry: brg stop -r {}", inst.at(member)))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut walk = Vec::new();
    member_stop_walk(
        &inst,
        &meta,
        member,
        &copies,
        &mut BTreeSet::new(),
        &mut walk,
    )?;
    // Refuse authority or socket mismatches before changing any target.
    check_stop_socket(&inst)?;
    for target in &walk {
        match target {
            SubtreeStop::Member(id) => {
                check_parent_authority(&inst, inst.record.role_of(id).unwrap(), None, Some(id))?
            }
            SubtreeStop::Copy { root, descendants } => {
                for copy in std::iter::once(root.as_ref()).chain(descendants.iter()) {
                    check_instance_authority(copy)?;
                    check_stop_socket(copy)?;
                }
            }
        }
    }
    let target = inst.at(member);
    let mut messages = Vec::new();
    let result = (|| {
        stop_pause();
        for node in walk {
            match node {
                SubtreeStop::Member(id) => {
                    let own = stop_one(&lock, &inst, Some(&id)).map_err(|error| {
                        let hint = format!(", then retry: brg stop {}", inst.id);
                        error.strip_suffix(&hint).unwrap_or(&error).to_string()
                    })?;
                    messages.extend(own);
                    inst.reload()?;
                    if liveness_of(&inst).get(&id) != Some(&Liveness::Gone) {
                        return Err(format!("{}: still running or unconfirmed", inst.at(&id)));
                    }
                }
                SubtreeStop::Copy {
                    root,
                    mut descendants,
                } => {
                    mark_cascade(&lock, &root, &descendants)?;
                    let cascade = root.id.clone();
                    descendants.push(*root);
                    for mut copy in descendants {
                        let mut errors = Vec::new();
                        finish_marked(
                            &lock,
                            &mut copy,
                            &cascade,
                            true,
                            &mut messages,
                            &mut errors,
                        )?;
                        if !errors.is_empty() {
                            return Err(errors.join("\n"));
                        }
                    }
                }
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        messages.push(format!(
            "{error}; retry the subtree stop: brg stop -r {target}"
        ));
        return Err(messages.join("\n"));
    }
    Ok(messages)
}

/// Check every target before persisting any stop marks or stopping panes.
fn check_stop_socket(inst: &Instance) -> Result<(), String> {
    if let Some(name) = state::read_meta(&inst.state)?.map(|meta| meta.socket)
        && name != state::tmux_socket()
    {
        return Err(format!(
            "team `{}` is on tmux socket `{name}`, not `{}`; stop it with BORGCHART_TMUX_SOCKET={name} brg stop {}. Nothing was changed",
            inst.id,
            state::tmux_socket(),
            inst.id
        ));
    }
    Ok(())
}

/// An explicit child team stop removes its whole subtree from every ancestor's
/// restore snapshot. Called under the same registry lock as the stop marks.
fn forget_copy_restore(lock: &registry::Lock, inst: &Instance) -> Result<(), String> {
    let mut id = inst.id.as_str();
    while let Some((parent_id, _)) = id.rsplit_once('/') {
        let mut parent = address::instance(parent_id)?;
        if let Some(copies) = &mut parent.record.copies_running_at_stop {
            let relative = &inst.id[parent_id.len() + 1..];
            let prefix = format!("{relative}/");
            copies.retain(|copy| copy != relative && !copy.starts_with(&prefix));
            lock.save_instance(&parent.state, &parent.record)?;
        }
        id = parent_id;
    }
    Ok(())
}

/// Persist the restore snapshots before killing any pane. A resumed stop
/// keeps the original snapshots, even if its panes have disappeared since.
fn mark_stopping(lock: &registry::Lock, inst: &Instance) -> Result<(), String> {
    if inst.record.stopping {
        return Ok(());
    }
    let mut record = inst.record.clone();
    let running: std::collections::BTreeSet<_> = liveness_of(inst)
        .into_iter()
        .filter(|(_, live)| *live != Liveness::Gone)
        .map(|(id, _)| id)
        .collect();
    if !running.is_empty() {
        record.running_at_stop = Some(running);
    }
    let mut running_copies = std::collections::BTreeSet::new();
    for copy in crate::nesting::descendants(inst)? {
        let Ok(copy) = copy.instance else {
            continue;
        };
        if is_running(&copy)
            || liveness_of(&copy)
                .values()
                .any(|live| *live != Liveness::Gone)
        {
            running_copies.insert(copy.id[inst.id.len() + 1..].to_string());
        }
    }
    if is_running(inst) || !running_copies.is_empty() || record.copies_running_at_stop.is_none() {
        record.copies_running_at_stop = Some(running_copies);
    }
    record.stopping = true;
    lock.save_instance(&inst.state, &record)
}

/// Pause after preparation so tests can exercise racing create/start.
/// Full stops release the registry lock; recursive member stops keep it.
#[cfg(feature = "test-support")]
fn stop_pause() {
    let Some(path) = std::env::var_os("BORGCHART_TEST_STOP_PAUSE").map(PathBuf::from) else {
        return;
    };
    let _ = std::fs::write(path.with_extension("waiting"), "");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(not(feature = "test-support"))]
fn stop_pause() {}

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
    let Some(mut meta) = state::read_meta(state)? else {
        // Nothing was ever recorded here, so brg started nothing to stop.
        return Ok(vec!["not running".into()]);
    };
    if only.is_some() {
        check_stop_socket(inst)?;
    }
    let server = lifecycle::Server::of(&meta);
    let rows = match server.panes() {
        Ok(rows) => rows,
        // A stop was already confirmed, or nothing ever started, and
        // nothing has started since.
        Err(_) if meta.stopped_at.is_some() || !meta.windows_started => Vec::new(),
        Err(e) => {
            let not_running = only.is_some_and(|id| {
                lifecycle::no_live_start(&meta, id, &lifecycle::load_incarnation(state, id))
            });
            // Member start records say nothing about plugin windows. A full
            // stop still needs proof that the recorded server is absent.
            if (only.is_some() && !not_running) || !server.absent() {
                return Err(format!("{e}; fix this error, then retry: brg stop {team}"));
            }
            for (id, _) in inst
                .record
                .kept_members()
                .filter(|(id, _)| only.is_none_or(|only| id.as_str() == only))
            {
                let inc = lifecycle::load_incarnation(state, id).map_err(|error| {
                    format!(
                        "{error}; restore `{team}/{id}`'s start record, then retry: brg stop {team}"
                    )
                })?;
                if let Some(inc) = inc
                    && inc.ended_at.is_none()
                    && !lifecycle::previous_boot(&inc)
                    && let Some(pane) = inc.pane
                    && !lifecycle::process_exited(pane.pane_pid)
                {
                    return Err(format!(
                        "{e}; brg can't confirm that `{team}/{id}`'s recorded process {} exited; end that process and ensure `ps` can check it, then retry: brg stop {team}",
                        pane.pane_pid
                    ));
                }
            }
            Vec::new()
        }
    };
    let in_session = |r: &&lifecycle::PaneRow| r.session == session;
    let mut messages = Vec::new();
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
                Some(_) => lifecycle::liveness(state, id, &inc.incarnation) == Liveness::Gone,
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
    // Stopping one member removes it from the next full-start snapshot.
    if let Some(id) = only {
        let mut record = registry::read_instance(state)?;
        if record
            .running_at_stop
            .as_mut()
            .is_some_and(|set| set.remove(id))
        {
            lock.save_instance(state, &record)?;
        }
    }
    Ok(messages)
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusRow {
    /// The local member or child team ID, a role with nothing created yet, or
    /// a plugin's name.
    pub name: String,
    /// `member`, `role` (no member or child team yet), `team` or `plugin`.
    pub kind: &'static str,
    #[serde(rename = "team")]
    pub instance: String,
    /// The template's name.
    #[serde(rename = "template")]
    pub team: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// The current start's incarnation ID, retained after a stop.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub vendor: Option<String>,
    /// Resolved model name, separate from its effort.
    pub model: Option<String>,
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "crate::overrides::Overrides::is_empty")]
    pub overrides: crate::overrides::Overrides,
    /// A local parent member ID, a child team's full parent address, or the
    /// team path that a nested root belongs to. Rows are in tree order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub posture: Option<String>,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    /// What a live member's session is about, in its vendor's own words
    /// (the terminal title), when its rules can read that.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    /// Claude's live Remote Control session, when its session file is readable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_control_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor_version: Option<String>,
    /// The folder a start accepted Claude's trust dialog for, if it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust_accepted: Option<String>,
    /// The git worktree brg added for a `worktree: true` member or child team.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    /// Whether the member's current incarnation may still be
    /// running (`alive`, `gone` or `unknown`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub liveness: Option<Liveness>,
    /// Unacked messages; absent when the team has no readable bus.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unacked: Option<usize>,
    /// A child team's running members, excluding members of its own child teams.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running: Option<usize>,
    /// A `start: true` member that ran and is `stopped` while the rest of
    /// the team runs: it should be running.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub expected_running: bool,
    /// The member's declared `start`.
    #[serde(skip)]
    pub start: bool,
}

/// A member's state when it has never started in the team.
pub const NOT_STARTED: &str = "not started";
/// A member's state when it ran and has ended.
pub const STOPPED: &str = "stopped";
/// A role that allows several members but has none kept.
pub const ON_DEMAND: &str = "on demand (none created)";
/// A `start: false` role with no member.
pub const NOT_STARTING: &str = "not created (start: false)";

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

/// Validate the team's template folder, as recorded.
pub(crate) fn validated(inst: &Instance) -> Result<Validated, String> {
    if !inst.record.team_dir.join(crate::team::TEAM_FILE).is_file() {
        return Err(format!(
            "team `{}`'s template folder {} is gone; if it moved, point the team at it after a full stop: brg start {} --template <path>",
            inst.id,
            inst.record.team_dir.display(),
            inst.id
        ));
    }
    let frozen = state::read_meta(&inst.state)
        .ok()
        .flatten()
        .unwrap_or_default()
        .config
        .team_dirs;
    let (report, validated) = crate::validate::validate_own(&inst.record.team_dir, &frozen);
    validated.ok_or_else(|| {
        report
            .errors()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    })
}

/// One snapshot of the team.
pub fn status(inst: &Instance) -> Result<Vec<StatusRow>, String> {
    let (counts, bus_note) = {
        match crate::bus::store::open(&inst.state).and_then(|bus| {
            let snap = match crate::bus::store::wait_snapshot(
                &bus,
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            )? {
                crate::bus::store::WaitSnapshot::Ready(snap) => snap,
                crate::bus::store::WaitSnapshot::TimedOut => {
                    return Err(crate::bus::outcome::Failure::new(
                        crate::bus::outcome::Code::Timeout,
                        "messages are busy",
                    ));
                }
            };
            if !snap.healthy() {
                return Err(crate::bus::outcome::Failure::new(
                    crate::bus::outcome::Code::Corrupt,
                    "incomplete messages",
                ));
            }
            Ok(crate::bus::store::progress(&bus, &snap))
        }) {
            Ok(counts) => (Some(counts), None),
            Err(error) if error.code == crate::bus::outcome::Code::Timeout => {
                (None, Some("messages busy; try again".to_string()))
            }
            Err(_) => (
                None,
                Some("messages unavailable; brg msg integrity-check".to_string()),
            ),
        }
    };
    status_with_counts(inst, counts.as_ref(), bus_note.as_deref())
}

/// Ignore missing or changed Claude session data and unsafe hyperlink targets.
fn remote_control_url(text: &str) -> Option<String> {
    let session: serde_json::Value = serde_json::from_str(text).ok()?;
    let id = session.get("bridgeSessionId")?.as_str()?;
    let suffix = id.strip_prefix("session_")?;
    if id.len() > 128 || suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    Some(format!("https://claude.ai/code/{id}"))
}

fn remote_control_url_for_session(text: &str, session_id: &str) -> Option<String> {
    let session: serde_json::Value = serde_json::from_str(text).ok()?;
    if session.get("sessionId")?.as_str()? != session_id {
        return None;
    }
    remote_control_url(text)
}

fn claude_remote_control_url(session_id: &str) -> Option<String> {
    use std::io::Read;

    const MAX_SESSION_BYTES: u64 = 64 * 1024;
    let config_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")))?;
    std::fs::read_dir(config_dir.join("sessions"))
        .ok()?
        .find_map(|entry| {
            let entry = entry.ok()?;
            if entry.path().extension()? != "json" || !entry.file_type().ok()?.is_file() {
                return None;
            }
            let file = std::fs::File::open(entry.path()).ok()?;
            if file.metadata().ok()?.len() > MAX_SESSION_BYTES {
                return None;
            }
            let mut text = String::new();
            file.take(MAX_SESSION_BYTES + 1)
                .read_to_string(&mut text)
                .ok()?;
            if text.len() as u64 > MAX_SESSION_BYTES {
                return None;
            }
            remote_control_url_for_session(&text, session_id)
        })
}

fn status_with_counts(
    inst: &Instance,
    counts: Option<&std::collections::BTreeMap<String, store::Progress>>,
    bus_note: Option<&str>,
) -> Result<Vec<StatusRow>, String> {
    let v = validated(inst)?;
    let state = &inst.state;
    let session = state.session();
    let tmux = tmux_for(state);
    let windows = tmux.windows(&session);
    let meta = state::read_meta(state).ok().flatten().unwrap_or_default();
    let nodes = member_nodes(inst, &v, |_| String::new());
    // The team runs while any member has a live window; one stopped on
    // purpose expects no one to be running.
    let team_running = windows
        .iter()
        .any(|w| !w.dead && inst.record.members.contains_key(&w.name));
    let mut rows = Vec::new();
    for (i, _) in tree::order(&nodes) {
        let id = &nodes[i].id;
        let Some(role) = inst.record.role_of(id) else {
            // A role with no member yet.
            let resolved = role_choice(&v, &inst.record, id);
            let member = resolved.as_ref().unwrap_or(&v.team.roles[id]);
            let choice = resolved.as_ref().ok();
            rows.push(StatusRow {
                name: id.clone(),
                kind: "role",
                instance: inst.id.clone(),
                team: inst.record.team.clone(),
                member: None,
                start_id: None,
                role: Some(id.clone()),
                vendor: choice.map(|m| m.vendor.as_str().into()),
                model: choice
                    .and_then(|m| m.model.as_ref())
                    .map(|m| m.name.clone()),
                effort: choice
                    .and_then(|m| m.model.as_ref())
                    .and_then(|m| m.effort.clone()),
                overrides: inst.record.overrides.get(id).cloned().unwrap_or_default(),
                parent: nodes[i].parent.clone(),
                posture: Some(match member.posture {
                    Posture::Vendor => "vendor".into(),
                    Posture::Locked => "locked".into(),
                }),
                state: not_created(member),
                rule: None,
                activity: None,
                remote_control_url: None,
                detail: resolved.as_ref().err().cloned(),
                vendor_version: None,
                trust_accepted: None,
                worktree: None,
                liveness: None,
                unacked: None,
                running: None,
                expected_running: false,
                start: member.start,
            });
            continue;
        };
        let resolved = member_role(&v, &inst.record, id);
        let member = resolved.as_ref().unwrap_or(&v.team.roles[role]);
        let choice = resolved.as_ref().ok();
        let posture = v.posture(role);
        // A current start record survives full stops and configuration changes.
        let incarnation = lifecycle::load_incarnation(state, id);
        let ran = match &incarnation {
            Ok(Some(_)) | Err(_) => true,
            Ok(None) => meta.members.get(id).is_some_and(|m| m.started),
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
            vendor: choice
                .map(|m| m.vendor.as_str().into())
                .or_else(|| meta.members.get(id).map(|m| m.vendor.clone())),
            model: choice
                .and_then(|m| m.model.as_ref())
                .map(|m| m.name.clone()),
            effort: choice
                .and_then(|m| m.model.as_ref())
                .and_then(|m| m.effort.clone()),
            overrides: member_overrides(&inst.record, id),
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
            unacked: counts.map(|all| all.get(&inst.at(id)).copied().unwrap_or_default().unread),
            running: None,
            expected_running: false,
            start: member.start,
            rule: None,
            activity: None,
            remote_control_url: None,
            detail: None,
            vendor_version: meta.members.get(id).and_then(|m| m.vendor_version.clone()),
            trust_accepted: std::fs::read_to_string(state.member_dir(id).join("trust-accepted"))
                .ok()
                .map(|s| s.trim().to_string()),
            worktree: member
                .worktree
                .then(|| crate::worktree::path(&inst.at(id)).display().to_string()),
        };
        if let Some(w) = windows.iter().find(|w| &w.name == id) {
            if w.dead {
                row.state = "exited".into();
                row.detail = w.dead_status.map(|s| format!("status {s}"));
            } else {
                let vendor = match row.vendor.as_deref() {
                    Some("claude") => Some(Vendor::Claude),
                    Some("codex") => Some(Vendor::Codex),
                    _ => None,
                };
                if vendor == Some(Vendor::Claude) {
                    row.remote_control_url = recorded_session(state, id)
                        .as_deref()
                        .and_then(claude_remote_control_url);
                }
                let screen = tmux.capture(&session, id);
                match vendor.map(rules::load) {
                    Some(Ok(rules)) => {
                        let reading = rules.read(&w.title, &screen);
                        row.state = reading.state.as_str().into();
                        row.rule = reading.rule;
                        row.activity = rules.activity(&w.title);
                    }
                    Some(Err(e)) => {
                        row.state = State::Unknown.as_str().into();
                        row.detail = Some(e);
                    }
                    None => row.state = State::Unknown.as_str().into(),
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
                // Nothing is typed into it (`msg send`, the wake, folder trust)
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
        if let Err(error) = resolved {
            row.detail = Some(match row.detail.take() {
                Some(detail) => format!("{error}; {detail}"),
                None => error,
            });
        }
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
            effort: None,
            overrides: Default::default(),
            parent: None,
            liveness: None,
            posture: None,
            state: "stopped".into(),
            rule: None,
            activity: None,
            remote_control_url: None,
            detail: None,
            vendor_version: None,
            trust_accepted: None,
            worktree: None,
            unacked: None,
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
            None => note.to_string(),
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
                effort: None,
                overrides: Default::default(),
                parent: parent.clone(),
                posture: None,
                state: if role.max > 1 {
                    ON_DEMAND.into()
                } else {
                    NOT_STARTING.into()
                },
                rule: None,
                activity: None,
                remote_control_url: None,
                detail: None,
                vendor_version: None,
                trust_accepted: None,
                worktree: None,
                liveness: None,
                unacked: None,
                running: None,
                expected_running: false,
                start: false,
            });
        }
        for (id, _) in copies {
            let full_id = inst.at(id);
            let (copy, mut children, error) = match address::instance(&full_id).and_then(|copy| {
                status_with_counts(&copy, counts, bus_note).map(|children| (copy, children))
            }) {
                Ok((copy, children)) => (Some(copy), children, None),
                Err(error) => (None, Vec::new(), Some(error)),
            };
            let running = children
                .iter()
                .filter(|r| {
                    r.kind == "member"
                        && r.instance == full_id
                        && (r.state != "stopping" || r.liveness == Some(Liveness::Alive))
                        && !matches!(r.state.as_str(), STOPPED | NOT_STARTED | "exited")
                })
                .count();
            rows.push(StatusRow {
                name: id.clone(),
                kind: "team",
                instance: full_id.clone(),
                team: copy
                    .as_ref()
                    .map(|copy| copy.record.team.clone())
                    .unwrap_or_else(|| crate::team::team_name(&role.dir)),
                member: None,
                start_id: None,
                role: Some(role_name.clone()),
                vendor: None,
                model: None,
                effort: None,
                overrides: Default::default(),
                parent: copy
                    .as_ref()
                    .and_then(|copy| copy.record.parent.clone())
                    .or_else(|| parent.as_deref().map(|parent| inst.at(parent))),
                posture: None,
                state: if error.is_some() {
                    "unreadable"
                } else if copy.as_ref().is_some_and(|copy| copy.record.stopping) {
                    "stopping"
                } else if running > 0 {
                    "running"
                } else {
                    STOPPED
                }
                .into(),
                rule: None,
                activity: None,
                remote_control_url: None,
                detail: error.clone(),
                vendor_version: None,
                trust_accepted: None,
                worktree: role
                    .worktree
                    .then(|| crate::worktree::path(&full_id).display().to_string()),
                liveness: None,
                unacked: None,
                running: error.is_none().then_some(running),
                expected_running: false,
                start: false,
            });
            for child in &mut children {
                if child.parent.is_none() {
                    child.parent = Some(full_id.clone());
                }
            }
            rows.extend(children);
        }
    }
    if inst.record.stopping {
        for row in rows
            .iter_mut()
            .filter(|row| row.instance == inst.id && row.kind != "role")
        {
            row.state = "stopping".into();
            row.expected_running = false;
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

/// `brg status` as text: the team's tree with each member's vendor, model,
/// state and details, then the plugin processes.
/// The columns `status` and `status -i` show after a row's name: a
/// member's vendor and model, state and details; a role with no member
/// yet, `not created (start: <setting>)` and any chosen vendor/model; a plugin's state and detail.
///
/// `short` is for a person's terminal: the vendor is its one-cell mark
/// ([`vendor_mark`]) and an undeclared model says nothing. Piped text keeps
/// the vendor's name, so what a script reads never changes.
pub fn status_cells(r: &StatusRow, short: bool) -> Vec<String> {
    match r.kind {
        "member" => {}
        "team" => {
            return vec![
                r.team.clone(),
                if matches!(r.state.as_str(), "stopping" | "unreadable") {
                    r.state.clone()
                } else {
                    format!("{} running", r.running.unwrap_or(0))
                },
                r.detail.clone().unwrap_or_default(),
            ];
        }
        "role" if r.overrides.is_empty() => return vec![r.state.clone()],
        "role" => {}
        _ => return vec![r.state.clone(), r.detail.clone().unwrap_or_default()],
    }
    let mut detail = Vec::new();
    if r.posture.as_deref() == Some("locked") {
        detail.push("locked".to_string());
    }
    // A turn's topic leads; an idle member's would be its last one.
    if matches!(r.state.as_str(), "working" | "blocked") {
        detail.extend(r.activity.clone());
    }
    detail.extend(r.detail.clone());
    if let Some(unacked) = r.unacked.filter(|&n| n > 0) {
        detail.push(format!("{unacked} unacked"));
    }
    let vendor = r.vendor.as_deref().unwrap_or("-");
    let model = r.model.as_ref().map(|name| match &r.effort {
        Some(effort) => format!("{name} {effort}"),
        None => name.clone(),
    });
    vec![
        match (short.then(|| vendor_mark(vendor)).flatten(), &model) {
            (Some(mark), Some(model)) => format!("{mark} {model}"),
            (Some(mark), None) => mark.to_string(),
            (None, model) => format!("{vendor} {}", model.as_deref().unwrap_or("default")),
        },
        state_label(r),
        detail.join("; "),
    ]
}

/// A vendor's one-cell mark, using ordinary Unicode that any terminal font draws.
pub fn vendor_mark(vendor: &str) -> Option<char> {
    match vendor {
        "claude" => Some('§'),
        "codex" => Some('Λ'),
        _ => None,
    }
}

pub fn render_status(header: &str, rows: &[StatusRow]) -> String {
    let terminal = std::io::stdout().is_terminal();
    render_status_as(header, rows, terminal, tree::colour_wanted(terminal), None)
}

/// [`render_status`] for a person's terminal (`short`) or for a pipe. A
/// terminal leaves out the roles [`hide_empty_roles`] hides.
pub fn render_status_as(
    header: &str,
    rows: &[StatusRow],
    short: bool,
    colour: bool,
    spin: Option<usize>,
) -> String {
    let shown;
    let rows = if short {
        shown = hide_empty_roles(rows.to_vec());
        &shown
    } else {
        rows
    };
    let nodes = status_nodes_as(rows, short);
    let plugins: Vec<(String, Vec<String>, tree::Look)> = rows
        .iter()
        .filter(|r| r.kind == "plugin" && r.parent.is_none())
        .map(|r| (r.name.clone(), status_cells(r, short), status_look(r)))
        .collect();
    let names: BTreeMap<_, _> = rows
        .iter()
        .map(|r| {
            let name = status_name(r);
            let name = match (short, r.remote_control_url.as_deref()) {
                (true, Some(url)) => format!("\x1b]8;;{url}\x1b\\{name}\x1b]8;;\x1b\\"),
                _ => name,
            };
            (status_key(r), name)
        })
        .collect();
    tree::render_named(header, &nodes, &plugins, colour, spin, short, |id| {
        names[id].clone()
    })
}

/// A row's identity stays unique when local IDs repeat in different child teams.
pub fn status_key(row: &StatusRow) -> String {
    match row.kind {
        "team" => row.instance.clone(),
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
    status_nodes_as(rows, false)
}

fn status_nodes_as(rows: &[StatusRow], short: bool) -> Vec<tree::Node> {
    rows.iter()
        .filter(|r| r.kind != "plugin" || r.parent.is_some())
        .map(|r| tree::Node {
            id: status_key(r),
            parent: parent_key(r),
            cols: {
                let mut cols = status_cells(r, short);
                if short && r.kind == "plugin" {
                    cols.insert(0, String::new());
                }
                if short {
                    match r.kind {
                        "member" | "team" | "plugin" => cols.swap(0, 1),
                        "role" if !r.overrides.is_empty() => cols.swap(0, 1),
                        "role" => {}
                        _ => cols.insert(1, String::new()),
                    }
                }
                cols
            },
            look: status_look(r),
        })
        .collect()
}

/// The [`status_key`] of a row's parent.
fn parent_key(r: &StatusRow) -> Option<String> {
    r.parent.as_ref().map(|p| {
        if p.contains('/') {
            p.clone()
        } else {
            format!("{}/{p}", r.instance)
        }
    })
}

/// The rows a person's terminal shows: every row but a role with no member
/// that is on demand or `start: false`, which says nothing at a glance. Such
/// a role stays when a shown row hangs beneath it, so the tree keeps its
/// shape. A role with choices or `start: true` stays: it has something to show.
/// Piped text and `--json` keep every row.
pub fn hide_empty_roles(rows: Vec<StatusRow>) -> Vec<StatusRow> {
    let empty = |r: &StatusRow| {
        r.kind == "role"
            && r.overrides.is_empty()
            && !r.start
            && (r.state == ON_DEMAND || r.state == NOT_STARTING)
    };
    let by_key: BTreeMap<String, &StatusRow> = rows.iter().map(|r| (status_key(r), r)).collect();
    let mut keep = BTreeSet::new();
    for row in rows.iter().filter(|r| !empty(r)) {
        // The row and every row above it.
        let mut at = Some(row);
        while let Some(r) = at {
            if !keep.insert(status_key(r)) {
                break;
            }
            at = parent_key(r).and_then(|p| by_key.get(&p).copied());
        }
    }
    rows.into_iter()
        .filter(|r| keep.contains(&status_key(r)))
        .collect()
}

/// How a row is drawn on a terminal: its state mark and tone.
///
/// Live rows get a mark; rows that are not running get none and recede, so a
/// glance at the tree finds what is running. Red is kept for the rows that
/// need a person: blocked, ended when it should run, or taking no input.
pub fn status_look(r: &StatusRow) -> tree::Look {
    use tree::{Look, Tone};
    let look = |mark, tone| Look {
        mark: Some(mark),
        tone,
        italic: false,
    };
    let faded = Look {
        mark: None,
        tone: Tone::Faded,
        italic: false,
    };
    match (r.kind, r.state.as_str()) {
        ("role", _) => Look {
            italic: true,
            ..faded
        },
        ("team", "unreadable") => look('◌', Tone::Odd),
        ("team", "stopping") => look('◌', Tone::Faded),
        ("team", _) if r.running.unwrap_or(0) > 0 => look('◆', Tone::Team),
        ("team", _) => faded,
        ("plugin", "alive")
            if r.detail
                .as_deref()
                .is_some_and(|d| d.starts_with("error") || d.starts_with("stale")) =>
        {
            look('!', Tone::Alert)
        }
        ("plugin", "alive") => look('●', Tone::Plain),
        (_, "working") => look('●', Tone::Busy),
        (_, "idle") => look('·', Tone::Plain),
        (_, "blocked") => look('?', Tone::Alert),
        (_, "gate-inactive") => look('!', Tone::Alert),
        (_, "exited") => look('✗', Tone::Alert),
        (_, STOPPED) if r.expected_running => look('!', Tone::Alert),
        (_, "stopping") => look('◌', Tone::Faded),
        (_, "unknown" | "unreadable") => look('◌', Tone::Odd),
        _ => faded,
    }
}

/// The terminal summary, in parts: how many members are working and
/// idle, how many rows need a person (the alert tone), and
/// unacked messages. Counts of zero are left out; with nothing to count, it
/// says so.
pub fn summary_parts(rows: &[StatusRow]) -> Vec<(String, tree::Tone)> {
    use tree::Tone;
    let members = || rows.iter().filter(|r| r.kind == "member");
    let working = members().filter(|r| r.state == "working").count();
    let idle = members().filter(|r| r.state == "idle").count();
    let need = rows
        .iter()
        .filter(|r| status_look(r).tone == tree::Tone::Alert)
        .count();
    let unacked: usize = members().filter_map(|r| r.unacked).sum();
    let mut parts = Vec::new();
    if working > 0 {
        parts.push((format!("● {working} working"), Tone::Busy));
    }
    if idle > 0 {
        parts.push((format!("· {idle} idle"), Tone::Faded));
    }
    if need > 0 {
        parts.push((format!("! {need} need you"), Tone::Alert));
    }
    if unacked > 0 {
        parts.push((format!("{unacked} unacked"), Tone::Faded));
    }
    if parts.is_empty() {
        parts.push(("nothing running".into(), Tone::Faded));
    }
    parts
}

/// [`summary_parts`] as one line, with working counts in teal and rows
/// that need a person in red when painted.
pub fn summary(rows: &[StatusRow], colour: bool) -> String {
    paint_summary(&summary_parts(rows), colour)
}

/// Attention first for the terminal band; pipe summaries retain their order.
pub fn summary_band_parts(rows: &[StatusRow]) -> Vec<(String, tree::Tone)> {
    let mut parts = summary_parts(rows);
    parts.sort_by_key(|(_, tone)| match tone {
        tree::Tone::Alert => 0,
        tree::Tone::Busy => 1,
        _ => 2,
    });
    parts
}

/// One band for the entire terminal view, above all team headers.
pub fn summary_band(rows: &[StatusRow], colour: bool) -> String {
    let brand = if colour {
        "\x1b[1mborgchart\x1b[0m"
    } else {
        "borgchart"
    };
    format!(
        "{brand}    {}",
        paint_summary(&summary_band_parts(rows), colour)
    )
}

fn paint_summary(parts: &[(String, tree::Tone)], colour: bool) -> String {
    parts
        .iter()
        .map(|(text, tone)| match (colour, tone) {
            (false, _) => text.clone(),
            (true, tree::Tone::Busy) => tree::ink_text(text, tree::palette::BUSY, true),
            (true, tree::Tone::Alert) => tree::alert_chip(text),
            (true, _) => tree::ink_text(text, tree::palette::FADED, false),
        })
        .collect::<Vec<_>>()
        .join("  ")
}

/// Why `msg send` refused, with its exit code.
#[derive(Debug, PartialEq, Eq)]
pub enum SendError {
    Usage(String),
    NotAlive(String),
    Blocked(String),
    Refused(String),
    Uncertain(String),
    Committed {
        id: String,
        delivery: &'static str,
        message: String,
    },
}

impl SendError {
    pub fn code(&self) -> u8 {
        match self {
            SendError::Usage(_) => 2,
            SendError::NotAlive(_) => 3,
            SendError::Blocked(_) => 4,
            SendError::Refused(_) => 5,
            SendError::Uncertain(_) | SendError::Committed { .. } => 5,
        }
    }
    pub fn message(&self) -> &str {
        match self {
            SendError::Usage(m)
            | SendError::NotAlive(m)
            | SendError::Blocked(m)
            | SendError::Refused(m)
            | SendError::Uncertain(m) => m,
            SendError::Committed { message, .. } => message,
        }
    }
}

/// Text checks `msg send` makes before touching the member.
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

/// The durable result of a send. Typing is an optional notification for ordinary recipients.
#[derive(Debug, Serialize)]
pub struct SendOutcome {
    pub id: String,
    pub delivery: &'static str,
    pub reason: String,
    pub message: String,
}

fn send_failure(f: crate::bus::outcome::Failure) -> SendError {
    if f.code == crate::bus::outcome::Code::NotConfigured {
        SendError::NotAlive(f.message)
    } else {
        SendError::Refused(f.message)
    }
}

/// Store a message, then optionally submit its envelope to the same incarnation.
pub fn send(
    inst: &Instance,
    recipient: &str,
    text: &str,
    force: bool,
    from: Option<&str>,
    external: Option<&str>,
    reply_to: &[String],
) -> Result<SendOutcome, SendError> {
    use crate::bus::{
        record::{Draft, Recipient},
        store,
    };
    check_text(text)?;
    let caller = context::caller(external).map_err(send_failure)?;
    if matches!(caller, context::Caller::Operator) {
        return Err(SendError::Usage(
            "a plugin process with no member identity must pass --external NAME to send".into(),
        ));
    }
    let own = context::instance(None)
        .or_else(|e| {
            if matches!(caller, context::Caller::External { .. }) {
                Ok(inst.id.clone())
            } else {
                Err(e)
            }
        })
        .map_err(send_failure)?;
    if own.split('/').next() != inst.id.split('/').next() {
        return Err(SendError::Refused(
            "messages cannot cross independent team trees".into(),
        ));
    }
    if from.is_some() && own != inst.id {
        return Err(SendError::Refused(
            "--from applies only within one team".into(),
        ));
    }
    let to = if matches!(Recipient::parse(recipient), Some(Recipient::External(_))) {
        recipient.to_string()
    } else {
        address::member(inst, recipient).map_err(SendError::NotAlive)?
    };
    let sender = address::instance(&own).map_err(SendError::NotAlive)?;
    let bus = store::open(&sender.state).map_err(send_failure)?;
    let target_bus = store::open(&inst.state).map_err(send_failure)?;
    let full_to = if to.starts_with(crate::bus::record::EXTERNAL_PREFIX) {
        to.clone()
    } else {
        inst.at(&to)
    };
    let from = from
        .map(|id| address::member(inst, id).map_err(SendError::Refused))
        .transpose()?;
    if let Some(from) = &from {
        if let context::Caller::Member { member, .. } = &caller
            && member != from
        {
            return Err(SendError::Refused(
                "--from must name the calling member".into(),
            ));
        }
        if bus.parent(from).as_deref() != Some(full_to.as_str()) {
            return Err(SendError::Refused(format!(
                "`{from}` may only send to its parent, and `{to}` is not it"
            )));
        }
    }
    let target_meta = target_bus.meta.members.get(&to);
    if target_meta.is_none() && !matches!(Recipient::parse(&to), Some(Recipient::External(_))) {
        return Err(SendError::NotAlive(format!(
            "`{to}` has no recorded start; start it before sending"
        )));
    }
    let locked = target_meta.is_some_and(|m| m.posture == "locked");
    let interactive = target_meta.is_some_and(|m| m.interactive);
    let vendor = if target_meta.is_some_and(|m| m.vendor == "codex") {
        Vendor::Codex
    } else {
        Vendor::Claude
    };
    let original = if target_meta.is_some() {
        lifecycle::load_incarnation(&inst.state, &to).map_err(SendError::Refused)?
    } else {
        None
    };
    let pane = original
        .as_ref()
        .filter(|inc| inc.ended_at.is_none())
        .and_then(|inc| inc.pane.clone());
    let tmux = tmux_for(&inst.state);
    if locked {
        let pane = pane
            .as_ref()
            .ok_or_else(|| SendError::NotAlive(format!("`{to}` is not running")))?;
        let before = send_snapshot(
            &tmux.with_socket(&pane.socket),
            &inst.state,
            &to,
            pane,
            vendor,
            force,
            true,
        )?;
        let mut view = crate::composer::view(vendor, &before.screen, Some(before.geometry))
            .map_err(SendError::Refused)?;
        view.busy = false;
        if view.reading() != crate::composer::Reading::Empty {
            return Err(SendError::Refused(
                "locked input box is not positively empty; draft preserved".into(),
            ));
        }
        let actor = store::actor_for(&bus, &caller).map_err(send_failure)?;
        // Reserve the widest possible record ID before allocating one.
        let preview = send_envelope(
            "m18446744073709551615",
            &store::actor_label(&actor),
            from.as_deref(),
            text,
            false,
        );
        if !view.can_verify_submission(&preview, before.screen.lines().count()) {
            return Err(SendError::Refused(
                "message exceeds the locked screen-verification budget; nothing recorded or pasted"
                    .into(),
            ));
        }
    }
    let draft = serde_json::to_vec(&Draft {
        to: full_to,
        body: text.into(),
        reply_to: reply_to.to_vec(),
    })
    .map_err(|e| SendError::Refused(e.to_string()))?;
    let record = store::publish(&bus, &caller, &draft).map_err(send_failure)?;
    let mut outcome = SendOutcome {
        id: record.id.clone(),
        delivery: "stored",
        reason: "recipient is not running".into(),
        message: format!("committed: {} (to {to})", record.id),
    };
    if target_meta.is_none() {
        outcome.reason = "external inbox".into();
        return Ok(outcome);
    }
    if interactive && vendor == Vendor::Claude {
        outcome.reason = "interactive recipient".into();
        return Ok(outcome);
    }
    let envelope = send_envelope(
        &record.id,
        &record.from,
        from.as_deref(),
        &record.body,
        record.ack_required,
    );
    let typed = (|| {
        let pane = pane
            .as_ref()
            .ok_or_else(|| SendError::NotAlive(format!("`{to}` is not running")))?;
        let lock = state::open_lock(&inst.state.lock_file(&to))
            .map_err(|e| SendError::Refused(format!("cannot open send lock: {e}")))?;
        lock.lock()
            .map_err(|e| SendError::Refused(format!("cannot lock send: {e}")))?;
        let current = lifecycle::load_incarnation(&inst.state, &to).map_err(SendError::Refused)?;
        if current.as_ref().is_none_or(|inc| {
            inc.ended_at.is_some()
                || inc.incarnation != original.as_ref().expect("pane has incarnation").incarnation
                || inc.pane.as_ref() != Some(pane)
        }) {
            return Err(SendError::Refused(
                "recipient incarnation changed after commit".into(),
            ));
        }
        let meta = state::read_meta(&inst.state)
            .map_err(SendError::Refused)?
            .ok_or_else(|| SendError::Refused("recipient metadata unavailable".into()))?;
        if meta
            .members
            .get(&to)
            .is_none_or(|m| m.interactive != interactive || (m.posture == "locked") != locked)
        {
            return Err(SendError::Refused(
                "recipient delivery posture changed after commit".into(),
            ));
        }
        if interactive {
            // A person may be typing in the box, and Codex starts no turn when
            // a background wait ends: hand the message over as its next turn.
            let inc = current.as_ref().expect("checked above");
            return queue_send(&inst.state, &to, &inc.started_at, &envelope)
                .map(|()| "queued")
                .map_err(SendError::Refused);
        }
        let tmux = tmux.with_socket(&pane.socket);
        let before = send_snapshot(&tmux, &inst.state, &to, pane, vendor, force, locked)?;
        let mut view = crate::composer::view(vendor, &before.screen, Some(before.geometry))
            .map_err(SendError::Refused)?;
        view.busy = false; // Working is allowed; the input box must still be empty.
        if view.reading() != crate::composer::Reading::Empty {
            return Err(SendError::Refused(
                "input box is not positively empty; draft preserved".into(),
            ));
        }
        if !view.can_verify_submission(&envelope, before.screen.lines().count()) {
            return Err(SendError::Refused(
                "message exceeds the screen-verification budget; no text pasted".into(),
            ));
        }
        paste_send(&tmux, &inst.state, &to, pane, vendor, locked, &envelope).map(|()| "typed")
    })();
    match typed {
        Ok(delivery) => {
            outcome.delivery = delivery;
            outcome.reason = match delivery {
                "queued" => "codex queue: the member's next turn",
                _ => "submitted",
            }
            .into();
            Ok(outcome)
        }
        Err(error) => {
            let uncertain = matches!(error, SendError::Uncertain(_));
            if locked {
                Err(SendError::Committed {
                    id: record.id,
                    delivery: if uncertain {
                        "uncertain"
                    } else {
                        "recorded-not-typed"
                    },
                    message: error.message().to_string(),
                })
            } else {
                outcome.delivery = if uncertain { "uncertain" } else { "stored" };
                outcome.reason = error.message().to_string();
                Ok(outcome)
            }
        }
    }
}

/// Give an interactive Codex member `envelope` with `codex queue`, on the
/// session its current start recorded. Err says why it stays stored.
fn queue_send(
    state: &StateDir,
    member: &str,
    started_at: &str,
    envelope: &str,
) -> Result<(), String> {
    let started = crate::bus::time::parse_utc(started_at)
        .ok_or("this start's time can't be read; not queued")?;
    let recorded = std::fs::metadata(state.member_session(member))
        .and_then(|m| m.modified())
        .ok();
    let id = recorded
        .filter(|&t| t >= started)
        .and_then(|_| recorded_session(state, member))
        .ok_or("no Codex session recorded for this start yet; not queued")?;
    let mut command = Command::new("codex");
    command.args(["queue", "--thread", &id, "--message", envelope]);
    let limits = crate::bounded_process::Limits {
        deadline: Instant::now() + Duration::from_secs(10),
        stdout_bytes: 64 * 1024,
        stderr_bytes: 64 * 1024,
    };
    match crate::bounded_process::run(&mut command, None, limits) {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(format!("`codex queue` failed ({}); not queued", out.status)),
        Err(e) => Err(format!("`codex queue` did not finish ({e}); not queued")),
    }
}

fn send_envelope(
    id: &str,
    author: &str,
    from: Option<&str>,
    body: &str,
    ack_required: bool,
) -> String {
    let relay = from
        .map(|from| format!("; relayed for {from}"))
        .unwrap_or_default();
    if ack_required {
        format!(
            "[brg {id} from {author}{relay}]\n{body}\n[Ignore duplicate delivery if already acked; otherwise brg msg ack {id} when taking responsibility.]"
        )
    } else {
        format!("[brg {id} from {author}{relay}; ack not required]\n{body}")
    }
}

fn send_snapshot(
    tmux: &Tmux,
    state: &StateDir,
    member: &str,
    pane: &PaneIdentity,
    vendor: Vendor,
    force: bool,
    locked: bool,
) -> Result<crate::wake::Snapshot, SendError> {
    if locked && !state.gate_marker(member).exists() {
        return Err(SendError::Refused(
            "locked recipient's gate is not active".into(),
        ));
    }
    let snap = crate::wake::snapshot(tmux, pane)
        .ok_or_else(|| SendError::NotAlive("recipient pane is not alive or changed".into()))?;
    let rules = rules::load(vendor).map_err(SendError::Refused)?;
    let plain = crate::composer::plain(&crate::composer::parse(&snap.screen));
    match rules.read(&snap.title, &plain).state {
        State::Blocked => {
            return Err(SendError::Blocked(
                "recipient is waiting at a dialog; no input sent".into(),
            ));
        }
        State::Unknown if !force => {
            return Err(SendError::Refused(
                "recipient state is unknown; --allow-unknown permits the state check only".into(),
            ));
        }
        _ => {}
    }
    if snap.in_mode {
        return Err(SendError::Blocked("recipient pane is in copy mode".into()));
    }
    Ok(snap)
}

fn paste_send(
    tmux: &Tmux,
    state: &StateDir,
    member: &str,
    pane: &PaneIdentity,
    vendor: Vendor,
    locked: bool,
    text: &str,
) -> Result<(), SendError> {
    let buffer = format!(
        "brg-send-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    tmux.run_with_stdin(&["load-buffer", "-b", &buffer, "-"], text.as_bytes())
        .map_err(SendError::Refused)?;
    if let Err(e) = tmux.ok(&[
        "paste-buffer",
        "-p",
        "-d",
        "-b",
        &buffer,
        "-t",
        &pane.pane_id,
    ]) {
        let _ = tmux.run(&["delete-buffer", "-b", &buffer]);
        return Err(SendError::Uncertain(format!("paste failed: {e}")));
    }
    // After paste, Unknown alone is expected when a vendor hides its idle footer.
    // The exact composer check below and dialog/copy-mode/pane checks still apply.
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let after = send_snapshot(tmux, state, member, pane, vendor, true, locked)
            .map_err(|e| SendError::Uncertain(e.message().into()))?;
        if crate::composer::view(vendor, &after.screen, Some(after.geometry))
            .is_ok_and(|view| view.holds_submission(text))
        {
            break;
        }
        if Instant::now() >= deadline {
            return Err(SendError::Uncertain(
                "pasted text cannot be verified exactly; Enter withheld, inspect the draft".into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    tmux.ok(&["send-keys", "-t", &pane.pane_id, "Enter"])
        .map_err(SendError::Uncertain)?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let after = send_snapshot(tmux, state, member, pane, vendor, true, locked)
            .map_err(|e| SendError::Uncertain(e.message().into()))?;
        if crate::wake::after_enter(vendor, Some(&after)).is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(SendError::Uncertain(
                "Enter sent but submission could not be confirmed; do not automatically retry"
                    .into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `brg logs <team>/<member>` (or a plugin process).
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
    match state::read_meta(&inst.state)?.and_then(|m| m.socket_path) {
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

/// `brg attach <team>/<member>`: replaces this process with a tmux client.
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
    fn remote_control_parser_finds_a_valid_session() {
        assert_eq!(
            remote_control_url(r#"{"bridgeSessionId":"session_Example42","other":true}"#)
                .as_deref(),
            Some("https://claude.ai/code/session_Example42")
        );
        let longest = format!("session_{}", "a".repeat(120));
        assert!(
            remote_control_url(&serde_json::json!({"bridgeSessionId": longest}).to_string())
                .is_some()
        );
    }

    #[test]
    fn remote_control_parser_ignores_missing_or_changed_session_data() {
        for text in [
            "{}",
            "{",
            "null",
            r#"{"bridgeSessionId":null}"#,
            r#"{"bridgeSessionId":42}"#,
        ] {
            assert_eq!(remote_control_url(text), None, "{text:?}");
        }
        for text in [
            r#"{"bridgeSessionId":"session_Example42"}"#,
            r#"{"sessionId":null,"bridgeSessionId":"session_Example42"}"#,
            r#"{"sessionId":42,"bridgeSessionId":"session_Example42"}"#,
        ] {
            assert_eq!(
                remote_control_url_for_session(text, "example-session"),
                None,
                "{text:?}"
            );
        }
    }

    #[test]
    fn remote_control_parser_rejects_hyperlink_injection_and_unbounded_ids() {
        for id in [
            "session_".into(),
            "Example42".into(),
            "session_a/b".into(),
            "session_a b".into(),
            "session_é".into(),
            "session_a\x1b\\evil".into(),
            format!("session_{}", "a".repeat(121)),
        ] {
            assert_eq!(
                remote_control_url(&serde_json::json!({"bridgeSessionId": id}).to_string()),
                None,
                "{id:?}"
            );
        }
    }

    #[test]
    fn short_cells_use_the_vendor_mark() {
        let row = |vendor: &str, model: Option<&str>| StatusRow {
            name: "m".into(),
            kind: "member",
            instance: "i".into(),
            team: "t".into(),
            member: None,
            start_id: None,
            role: None,
            vendor: Some(vendor.into()),
            model: model.map(String::from),
            effort: None,
            overrides: Default::default(),
            parent: None,
            posture: None,
            state: "working".into(),
            rule: None,
            activity: Some("Review issues".into()),
            remote_control_url: None,
            detail: None,
            vendor_version: None,
            trust_accepted: None,
            worktree: None,
            liveness: None,
            unacked: Some(2),
            running: None,
            expected_running: false,
            start: true,
        };
        let cells = |r: &StatusRow, short| status_cells(r, short).join("|");
        let mut claude = row("claude", Some("opus"));
        claude.effort = Some("high".into());
        assert_eq!(
            cells(&claude, true),
            "§ opus high|working|Review issues; 2 unacked"
        );
        assert_eq!(
            cells(&claude, false),
            "claude opus high|working|Review issues; 2 unacked"
        );
        let codex = row("codex", None);
        assert!(cells(&codex, true).starts_with("Λ|"));
        // Working is one colour whatever the vendor: red is only for alerts.
        assert_eq!(status_look(&claude).tone, tree::Tone::Busy);
        assert_eq!(status_look(&codex).tone, tree::Tone::Busy);
        assert!(cells(&codex, false).starts_with("codex default|"));
        // An idle member's topic is its last one: not shown.
        let mut idle = claude.clone();
        idle.state = "idle".into();
        assert_eq!(cells(&idle, true), "§ opus high|idle|2 unacked");
    }

    #[test]
    fn the_tree_leaves_the_worktree_to_json() {
        let member = StatusRow {
            name: "m".into(),
            kind: "member",
            instance: "i".into(),
            team: "t".into(),
            member: None,
            start_id: None,
            role: None,
            vendor: Some("claude".into()),
            model: None,
            effort: None,
            overrides: Default::default(),
            parent: None,
            posture: None,
            state: "idle".into(),
            rule: None,
            activity: None,
            remote_control_url: None,
            detail: None,
            vendor_version: None,
            trust_accepted: None,
            worktree: Some("/srv/example/worktrees/team.m".into()),
            liveness: None,
            unacked: Some(1),
            running: None,
            expected_running: false,
            start: true,
        };
        let copy = StatusRow {
            kind: "team",
            detail: Some("note".into()),
            running: Some(2),
            ..member.clone()
        };
        for short in [true, false] {
            assert_eq!(status_cells(&member, short)[2], "1 unacked");
            assert_eq!(status_cells(&copy, short)[1..], ["2 running", "note"]);
        }
    }

    #[test]
    fn summary_counts_what_needs_a_glance() {
        let row = |kind: &'static str, state: &str, unacked: Option<usize>| StatusRow {
            name: "m".into(),
            kind,
            instance: "i".into(),
            team: "t".into(),
            member: None,
            start_id: None,
            role: None,
            vendor: Some("claude".into()),
            model: None,
            effort: None,
            overrides: Default::default(),
            parent: None,
            posture: None,
            state: state.into(),
            rule: None,
            activity: None,
            remote_control_url: None,
            detail: None,
            vendor_version: None,
            trust_accepted: None,
            worktree: None,
            liveness: None,
            unacked,
            running: None,
            expected_running: false,
            start: true,
        };
        let rows = vec![
            row("member", "working", Some(2)),
            row("member", "working", None),
            row("member", "idle", Some(1)),
            row("member", "blocked", None),
            row("member", "exited", None),
            row("role", ON_DEMAND, None),
        ];
        assert_eq!(
            summary(&rows, false),
            "● 2 working  · 1 idle  ! 2 need you  3 unacked"
        );
        // Only what needs a person is bold red.
        let painted = summary(&rows, true);
        assert!(painted.contains(&tree::ink_text("● 2 working", tree::palette::BUSY, true)));
        assert!(painted.contains(&tree::alert_chip("! 2 need you")));
        assert!(painted.contains(&tree::ink_text("· 1 idle", tree::palette::FADED, false)));
        assert_eq!(summary(&rows[5..], false), "nothing running");
        // A role with no member yet is drawn as a slot: italic.
        assert!(status_look(&rows[5]).italic);
        assert!(!status_look(&rows[0]).italic);
    }

    #[test]
    fn a_terminal_hides_roles_that_say_nothing() {
        let row =
            |kind: &'static str, instance: &str, name: &str, state: &str, parent: Option<&str>| {
                StatusRow {
                    name: name.into(),
                    kind,
                    instance: instance.into(),
                    team: "t".into(),
                    member: None,
                    start_id: None,
                    role: None,
                    vendor: None,
                    model: None,
                    effort: None,
                    overrides: Default::default(),
                    parent: parent.map(Into::into),
                    posture: None,
                    state: state.into(),
                    rule: None,
                    activity: None,
                    remote_control_url: None,
                    detail: None,
                    vendor_version: None,
                    trust_accepted: None,
                    worktree: None,
                    liveness: None,
                    unacked: None,
                    running: None,
                    expected_running: false,
                    start: false,
                }
            };
        let rows = vec![
            row("member", "i", "lead", "idle", None),
            row("role", "i", "helper", ON_DEMAND, Some("lead")),
            row("role", "i", "scout", NOT_STARTING, Some("lead")),
            // It should have a member and has none: worth a glance.
            row(
                "role",
                "i",
                "keeper",
                "not created (start: true)",
                Some("lead"),
            ),
            // So does a `start: true` role that allows several members and has none kept.
            StatusRow {
                start: true,
                ..row("role", "i", "pool", ON_DEMAND, Some("lead"))
            },
            // A member-less role with a member beneath it keeps its row.
            row("role", "i", "boss", NOT_STARTING, None),
            row("member", "i", "orphan", "idle", Some("boss")),
            row("role", "i", "spare", NOT_STARTING, None),
            row("team", "i/sub", "sub", "0 running", Some("i/spare")),
            row("plugin", "i", "watch", "alive", None),
            row("team", "i/dev", "dev", "1 running", Some("i/lead")),
            row("member", "i/dev", "devlead", "working", Some("i/dev")),
            row("role", "i/dev", "reviewer", NOT_STARTING, Some("devlead")),
        ];
        let names: Vec<String> = hide_empty_roles(rows.clone())
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(
            names,
            [
                "lead", "keeper", "pool", "boss", "orphan", "spare", "sub", "watch", "dev",
                "devlead"
            ]
        );
        let terminal = render_status_as("i", &rows, true, true, None);
        let piped = render_status_as("i", &rows, false, false, None);
        for hidden in ["helper", "scout", "reviewer"] {
            assert!(!terminal.contains(hidden), "{terminal}");
            assert!(piped.contains(hidden), "{piped}");
        }
        assert!(terminal.contains("orphan") && terminal.contains("keeper"));
    }

    #[test]
    fn tilde_shortens_only_the_home_folder() {
        let home = Some(std::path::Path::new("/home/m"));
        let t = |p: &str| tilde(std::path::Path::new(p), home);
        assert_eq!(t("/home/m/workspace/x"), "~/workspace/x");
        assert_eq!(t("/home/m"), "~");
        assert_eq!(t("/home/mm/x"), "/home/mm/x");
        assert_eq!(t("/srv/x"), "/srv/x");
        assert_eq!(tilde(std::path::Path::new("/home/m/x"), None), "/home/m/x");
        for odd in ["/", ""] {
            let odd = Some(std::path::Path::new(odd));
            assert_eq!(tilde(std::path::Path::new("/srv/x"), odd), "/srv/x");
        }
    }

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
    fn captured_path_removes_only_the_callers_member_shim() {
        let shim = Path::new("/state/example-a1b2/members/lead-c3d4/bin");
        let path = format!(
            "{}:/pinned/bin:{}:/other/member/bin:/usr/bin",
            shim.display(),
            shim.display()
        );
        assert_eq!(
            without_member_shim(&path, shim),
            "/pinned/bin:/other/member/bin:/usr/bin"
        );
        assert_eq!(
            without_member_shim("/pinned/bin:/usr/bin", shim),
            "/pinned/bin:/usr/bin"
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
