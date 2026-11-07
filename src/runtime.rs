//! The tmux runtime: `up`, `down`, `status`, `attach`, `logs` and `send`.

use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;

use crate::bus::context;
use crate::bus::layout::RunPaths;
use crate::bus::manifest::{Incarnation, Liveness, PaneIdentity};
use crate::bus::time::utc_now;
use crate::deploy::Auth;
use crate::exec::{ExecSpec, PluginEnv, identity_env};
use crate::launch;
use crate::lifecycle;
use crate::proctree;
use crate::rules::{self, State};
use crate::state::{
    self, MemberMeta, Meta, PluginMeta, StateDir, shell_quote, write_private, write_script,
};
use crate::team::{PluginLocation, Posture, Vendor};
use crate::tmux::{self, Tmux, target};
use crate::tree;
use crate::validate::{Input, Validated, validate};

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
    state::ENV_TMUX_SOCKET,
    state::ENV_TMUX_TMPDIR,
];

/// How long `up` waits for a `locked` member's gate to report in.
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
    env.entry("LANG".into()).or_insert_with(|| "C.UTF-8".into());
    env
}

fn brg_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("brg"))
}

/// A deployment, resolved from `--file`/`--team`.
pub struct Deployment {
    pub state: StateDir,
    pub source: PathBuf,
}

pub fn state_for(input: &Input) -> Deployment {
    let source = match input {
        Input::Deployment(f) => f.clone(),
        Input::Team(d) => d.clone(),
    };
    let source = source.canonicalize().unwrap_or(source);
    Deployment {
        state: StateDir::new(&state::deployment_id(&source)),
        source,
    }
}

/// The deployment's tmux server: the socket path recorded at `up` once
/// there is one, whatever the caller's tmux settings say; until then, the
/// socket brg's settings name.
fn tmux_for(state: &StateDir) -> Tmux {
    let tmux = Tmux::new(&state.tmux_conf(), base_env());
    match state::read_meta(state).and_then(|m| m.socket_path) {
        Some(path) => tmux.with_socket(&path.display().to_string()),
        None => tmux,
    }
}

fn plugin_dir(v: &Validated, name: &str) -> Option<PathBuf> {
    let plugin = v.team.plugins.get(name)?;
    let dir = match plugin.location.as_ref()? {
        PluginLocation::Team(rel) => v.team.dir.join(rel),
        PluginLocation::Shared(shared) => v.deployment.as_ref()?.plugins_dir.as_ref()?.join(shared),
    };
    Some(dir.canonicalize().unwrap_or(dir))
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
    /// The declared cwd whose Claude folder-trust dialog `up` may accept.
    trust_dir: Option<PathBuf>,
    /// The pane this `up` launched; nothing after the wait touches another.
    pane: Option<PaneIdentity>,
}

/// Whether `s`'s window still runs the pane this `up` launched.
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

/// The id of the rule that recognizes Claude's folder-trust dialog.
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

/// What `up` reports for a member another start replaced while this one
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
        let mut values = BTreeMap::new();
        let configured = v.deployment.as_ref().and_then(|d| d.plugins.get(name));
        for env_name in &plugin.env {
            if let Some(value) = configured.and_then(|p| p.env.get(env_name)) {
                values.insert(env_name.clone(), value.clone());
            } else if let Ok(value) = std::env::var(env_name) {
                values.insert(env_name.clone(), value);
            } else {
                missing.push(format!("{name}: {env_name}"));
            }
        }
        all.insert(name.clone(), values);
    }
    (all, missing)
}

/// With `auth: api_key`, each starting member's key, from your environment.
fn api_keys(v: &Validated, ids: &[String]) -> Result<BTreeMap<String, (String, String)>, String> {
    let mut keys = BTreeMap::new();
    if v.auth() != Auth::ApiKey {
        return Ok(keys);
    }
    let mut missing = Vec::new();
    for id in ids {
        let key = match v.team.members[id].vendor {
            Vendor::Claude => "ANTHROPIC_API_KEY",
            Vendor::Codex => "OPENAI_API_KEY",
        };
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

/// `brg up [--member ID]`.
///
/// Everything is checked before anything is written. Starting into a
/// running team (its session exists and `deployment.json` is there) reuses
/// the base environment and plugin values it started with, and refuses a
/// changed configuration. With a bus, a full `up` of a stopped team rolls
/// its message storage over when the configuration or format changed. The lifecycle
/// locks are held while files are written and windows started, and released
/// before waiting for gates and folder trust.
pub fn up(input: &Input, only: Option<&str>) -> Result<UpOutcome, String> {
    let (report, validated) = validate(input);
    for diag in &report.diags {
        eprintln!("{diag}");
    }
    let v = validated.ok_or("validation failed; nothing was started")?;
    if let Some(id) = only
        && !v.team.members.contains_key(id)
    {
        return Err(format!("`{id}` is not a member of this team"));
    }
    let dep = state_for(input);
    let state = &dep.state;
    let session = state.session();
    if state
        .root
        .display()
        .to_string()
        .chars()
        .any(char::is_whitespace)
    {
        return Err(format!(
            "the state directory {} contains whitespace, which `locked` command rules can't match; set BORGCHART_STATE_HOME to a path without spaces",
            state.root.display()
        ));
    }
    // Members get it in their environment and run in their own folders.
    if let Some(dir) = std::env::var_os(state::ENV_TMUX_TMPDIR)
        && !Path::new(&dir).is_absolute()
    {
        return Err(format!(
            "{} must be an absolute path; nothing was started",
            state::ENV_TMUX_TMPDIR
        ));
    }
    // Every window, shim, gate hook and plugin process this `up` starts
    // runs the brg running now, as in v0: brg isn't pinned.
    let brg = brg_path();
    let team = state::team_name(&dep.source);
    let restart = state::restart_hint(&dep.source);
    let snapshot = v.config_snapshot();
    let bus = v.deployment.as_ref().and_then(|d| d.bus.clone());
    let member_ids: Vec<String> = v
        .team
        .members
        .values()
        .filter(|m| match only {
            Some(id) => m.id == id,
            None => m.start,
        })
        .map(|m| m.id.clone())
        .collect();
    // Checks that need nothing written.
    for id in &member_ids {
        let granted = launch::granted(&v.team, &v.team.members[id]);
        if let Some((plugin, command)) = launch::shim_commands(&granted)
            .into_iter()
            .find(|(_, c)| launch::RESERVED_COMMANDS.contains(&c.as_str()))
        {
            return Err(format!(
                "plugin `{plugin}`'s command `{command}` would hide the installed `{command}` from {team}/{id}, so its message commands would call the plugin; rename it. Nothing was started"
            ));
        }
    }
    let (values, missing) = plugin_values(&v);
    let keys = api_keys(&v, &member_ids)?;
    let missing_error = || {
        format!(
            "set these before `brg up` (in your environment, or non-secret ones in borgchart.yaml); nothing was started: {}",
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
    // Unless the last deployment here was confirmed stopped, or never
    // started a window, a server this caller can't see may still run it.
    let may_still_run = previous
        .as_ref()
        .is_some_and(|m| m.stopped_at.is_none() && m.windows_started != Some(false));
    let existing = previous.filter(|_| probe.has_session(&session));
    let joining = existing.is_some();
    match &existing {
        Some(meta) => lifecycle::check_join(meta, &snapshot, state, &team, &restart)?,
        None if !missing.is_empty() => return Err(missing_error()),
        None => {}
    }
    let binding = match &bus {
        Some(bus) => Some(lifecycle::bind_run(
            &v,
            bus,
            state,
            &dep.source,
            joining,
            only.is_some(),
        )?),
        None => None,
    };
    let mut notes: Vec<String> = binding
        .as_ref()
        .and_then(|b| b.note.clone())
        .into_iter()
        .collect();
    let user_config = v
        .deployment
        .as_ref()
        .and_then(|d| d.tmux_user_config.as_ref())
        .and_then(|u| u.resolve());
    // The server reads its config once, when it starts, and every team on
    // it shares it.
    let wanted = user_config
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    if let Ok(loaded) =
        tmux_for(state).ok(&["show-options", "-gqv", crate::tmux::USER_CONFIG_OPTION])
        && loaded.trim() != wanted
    {
        notes.push(format!(
            "brg's tmux server is already running {}, so this team's `tmux.user_config` ({}) takes effect only once every team on it is down and it starts again",
            if loaded.trim().is_empty() {
                "without your tmux config".to_string()
            } else {
                format!("with {}", loaded.trim())
            },
            if wanted.is_empty() { "off" } else { wanted.as_str() }
        ));
    }

    let fresh = existing.is_none();
    let mut meta = match existing {
        Some(meta) => meta,
        None => {
            let base = base_env();
            Tmux::new(&state.tmux_conf(), base.clone())
                .write_config(user_config.as_deref())
                .map_err(|e| format!("cannot write tmux config: {e}"))?;
            let mut meta = Meta {
                source: dep.source.clone(),
                team_dir: v.team.dir.canonicalize().unwrap_or(v.team.dir.clone()),
                brg: brg.clone(),
                socket: Some(state::tmux_socket()),
                config: Some(snapshot.clone()),
                run: binding.as_ref().map(|b| state::RunBinding {
                    run: b.run.clone(),
                    dir: b.paths.root.clone(),
                }),
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
                        dir: plugin_dir(&v, name),
                        has_process: plugin.process.is_some(),
                    },
                );
            }
            for id in v.team.members.keys() {
                state::open_lock(&state.lock_file(id))
                    .map_err(|e| format!("cannot create send lock: {e}"))?;
            }
            for (id, member) in &v.team.members {
                let posture = v.posture(id);
                meta.members.insert(
                    id.clone(),
                    MemberMeta {
                        vendor: member.vendor.as_str().into(),
                        posture: match posture {
                            Posture::Vendor => "vendor".into(),
                            Posture::Locked => "locked".into(),
                        },
                        parent: member.parent.clone(),
                        vendor_version: vendor_version(member.vendor, &base),
                        started: Some(false),
                    },
                );
            }
            write_private(
                &state.meta_file(),
                &serde_json::to_vec_pretty(&meta).map_err(|e| e.to_string())?,
            )
            .map_err(|e| format!("cannot write state: {e}"))?;
            meta
        }
    };
    let base = meta.base_env.clone();
    let tmux = Tmux::new(&state.tmux_conf(), base.clone());
    // A running team's server is the one recorded for it.
    let tmux = match &meta.socket_path {
        Some(path) => tmux.with_socket(&path.display().to_string()),
        None => tmux,
    };

    let mut outcomes: BTreeMap<String, String> = BTreeMap::new();
    let mut plugin_outcomes: Vec<(String, String)> = Vec::new();
    let mut failed = false;
    let mut started: Vec<Started> = Vec::new();

    for id in &member_ids {
        let member = &v.team.members[id];
        if let Some(w) = tmux.window(&session, id) {
            if !w.dead {
                set(&mut outcomes, id, "already running".into());
                continue;
            }
            let _ = tmux.kill_window(&session, id);
        }
        let posture = v.posture(id);
        let deploy_member = v.deployment.as_ref().and_then(|d| d.members.get(id));
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

        let auto_trust = v.deployment.as_ref().and_then(|d| d.auto_trust);
        let declared_cwd = deploy_member
            .and_then(|d| d.cwd.as_ref())
            .map(|c| c.canonicalize().unwrap_or_else(|_| c.clone()));
        // Claude: `up` answers its trust dialog. By default only for `locked`
        // members, which ignore project settings.
        let trust_dir = declared_cwd
            .clone()
            .filter(|_| member.vendor == Vendor::Claude)
            .filter(|_| auto_trust.unwrap_or(posture == Posture::Locked));
        // Codex: trust passed as session config, only with `auto_trust: true`
        // and only in `vendor` posture. A trusted folder's `.codex/` loads, and a
        // `locked` Codex member can write its folder, so it could configure its
        // own next start outside its gate. The deployment's own `projects`
        // setting wins.
        let codex_trust = declared_cwd
            .filter(|_| member.vendor == Vendor::Codex)
            .filter(|_| auto_trust == Some(true) && posture == Posture::Vendor)
            .filter(|_| {
                !launch::codex_args_set(
                    deploy_member.map(|d| d.args.as_slice()).unwrap_or_default(),
                    "projects",
                )
            });

        let settings_path = state.member_dir(id).join("claude-settings.json");
        let spec = launch::member_launch(
            &brg,
            state,
            &v.team,
            member,
            deploy_member,
            posture,
            &help,
            &settings_path,
            codex_trust.as_deref(),
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
        env.insert("BORGCHART_DEPLOYMENT".into(), state.id.clone());
        env.insert(context::ENV_MEMBER.into(), id.clone());
        // The brg that starts this member, so its own brg calls don't depend
        // on what its shell puts first on `PATH`.
        env.insert("BORGCHART_BIN".into(), brg.display().to_string());
        let tmp = state.member_tmp(id);
        state::ensure_dir(&tmp).map_err(|e| e.to_string())?;
        env.insert("TMPDIR".into(), tmp.display().to_string());
        env.insert(state::ENV_FILE.into(), dep.source.display().to_string());
        if let Some((key, value)) = keys.get(id) {
            env.insert(key.clone(), value.clone());
        }
        // Codex's sandbox can write its cwd, so a `locked` Codex member that
        // declares none starts in its own temp dir rather than the team dir.
        let cwd = deploy_member
            .and_then(|d| d.cwd.clone())
            .unwrap_or_else(|| {
                if posture == Posture::Locked && member.vendor == Vendor::Codex {
                    state.member_tmp(id)
                } else {
                    meta.team_dir.clone()
                }
            });
        // With a bus, each start is a new incarnation, recorded before its
        // window exists and again once its pane is known.
        let incarnation = match &binding {
            Some(b) => {
                let inc = Incarnation {
                    member: id.clone(),
                    incarnation: lifecycle::new_incarnation_id(&b.paths, id),
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
                lifecycle::begin_incarnation(&b.paths, &inc)?;
                env.insert(context::ENV_RUN.into(), b.run.clone());
                env.insert(context::ENV_INCARNATION.into(), inc.incarnation.clone());
                Some((b, inc))
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
        if let (Some((b, _)), Err(_)) = (&incarnation, &result) {
            // tmux didn't create the window, so nothing runs it.
            let _ = lifecycle::end_incarnation(&b.paths, id, lifecycle::ENDED_START_FAILED);
        }
        result?;
        // Without a bus, the team's state is where `status` learns that a
        // member ran.
        if binding.is_none()
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
            if let Some((b, _)) = &incarnation {
                let _ = lifecycle::end_incarnation(&b.paths, id, lifecycle::ENDED_START_FAILED);
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
        if let Some((b, mut inc)) = incarnation {
            inc.pane = pane.clone();
            lifecycle::complete_incarnation(&b.paths, &inc)?;
        }
        started.push(Started {
            id: id.clone(),
            vendor: member.vendor,
            locked: posture == Posture::Locked,
            trust_dir,
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

    // Plugin processes start with the whole team.
    if only.is_none() {
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
            if let Some(target) = &plugin.delivers_to {
                env.insert("BORGCHART_DELIVER_TO".into(), target.clone());
            }
            let cwd = cwd.unwrap_or_else(|| meta.team_dir.clone());
            start_window(
                &tmux,
                &brg,
                state,
                &mut meta,
                &session,
                &window,
                &ExecSpec { argv, env, cwd },
            )?;
            plugin_outcomes.push((name.clone(), "started".into()));
        }
    }

    // Record where the tmux server is, so `down` and liveness
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

    // Members may use the bus while they start up; another `up --member`
    // may proceed too. The checks below read screens without the locks, and
    // take them again for anything they change (a key press, a trust marker,
    // stopping a `locked` member), and only while the window still runs the
    // pane this `up` launched.
    let run_paths = binding.as_ref().map(|b| b.paths.clone());
    drop(binding);
    drop(state_lock);
    let relock = |s: &Started| -> Result<(Launched, Vec<std::fs::File>), String> {
        let mut locks = vec![lifecycle::lock_state(state)?];
        if let Some(paths) = &run_paths {
            locks.push(lifecycle::lock_run(paths)?);
        }
        Ok((launched(&tmux, &session, s), locks))
    };

    // Accept Claude's folder trust for a declared cwd, and make every locked
    // member prove its gate is active, or stop it.
    for s in started.iter().filter(|s| s.locked || s.trust_dir.is_some()) {
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
                // Its own gate never reported in to this `up`.
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
            // Keys would go to copy mode, not the dialog: wait for the pane
            // to leave it, and answer then.
            if accept && w.in_mode {
                if Instant::now() > deadline {
                    append(
                        &mut outcomes,
                        id,
                        format!(
                            "waiting at Claude's folder-trust prompt: {}; {}, then answer it",
                            tmux::IN_MODE,
                            tmux::leave_mode_hint(id)
                        ),
                    );
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
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
                            "couldn't confirm it is still the member this `up` started, so the folder-trust dialog was left for you; run `brg attach {id}`"
                        ),
                    );
                    failed |= s.locked;
                    break;
                }
                // It may have gone into copy mode since the screen was read.
                if tmux.window(&session, id).is_none_or(|w| w.in_mode) {
                    std::thread::sleep(Duration::from_millis(200));
                    continue;
                }
                let t = pane.as_str();
                tmux.ok(&["send-keys", "-t", t, "Down"])?;
                std::thread::sleep(Duration::from_millis(250));
                let after = tmux.capture(&session, id);
                let in_mode = tmux.window(&session, id).is_none_or(|w| w.in_mode);
                if in_mode
                    || !(has_line(&after, TRUST_YES_SELECTED)
                        && trust_dialog_path(&after).as_deref() == s.trust_dir.as_deref())
                {
                    append(
                        &mut outcomes,
                        id,
                        format!(
                            "the folder-trust dialog didn't look as expected, so it was left for you: brg attach {id}"
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
                // Nothing runs before Claude's folder trust is answered, and the
                // gate must still report in afterwards (`send` checks).
                Some(State::Blocked) if is_trust => {
                    append(
                        &mut outcomes,
                        id,
                        format!(
                            "waiting at Claude's folder-trust prompt; answer it with `brg attach {id}`"
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
                        format!("waiting at a prompt; answer it with `brg attach {id}`"),
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

    let nodes: Vec<tree::Node> = v
        .team
        .members
        .values()
        .map(|m| {
            let outcome = match outcomes.get(&m.id) {
                Some(o) => o.clone(),
                None if only.is_none() && !m.start => not_started(false),
                None => String::new(),
            };
            tree::Node {
                id: m.id.clone(),
                parent: m.parent.clone(),
                cols: vec![tree::vendor_model(m), outcome],
            }
        })
        .collect();
    let plugins: Vec<(String, Vec<String>)> = plugin_outcomes
        .into_iter()
        .map(|(name, o)| (name, vec![o]))
        .collect();
    Ok(UpOutcome {
        tree: tree::render(&team, &nodes, &plugins),
        notes,
        failed,
    })
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
    // Recorded first: once tmux runs, `down` must not take an unreachable
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

/// `brg down [--member ID]`.
///
/// Works on the tmux server recorded at `up`, refusing a caller whose
/// `BORGCHART_TMUX_SOCKET` names another, and changes nothing unless that
/// server answers. A member's incarnation is marked ended, and launch files
/// and secrets removed, only once its pane was stopped or the server showed
/// it gone.
pub fn down(input: &Input, only: Option<&str>) -> Result<Vec<String>, String> {
    let dep = state_for(input);
    let state = &dep.state;
    let session = state.session();
    let team = state::team_name(&dep.source);
    if !state.root.is_dir() {
        return Ok(vec!["not running".into()]);
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
            "team `{team}` is on tmux socket `{name}`, not `{caller_socket}`; stop it with BORGCHART_TMUX_SOCKET={name} brg down. Nothing was changed"
        ));
    }
    let server = lifecycle::Server::of(&meta);
    let run = meta
        .run
        .as_ref()
        .map(|r| RunPaths::from_root(&r.dir))
        .filter(|p| p.runtime_dir().is_dir());
    let _run_lock = match &run {
        Some(paths) => Some(lifecycle::lock_run(paths)?),
        None => None,
    };
    let rows = match server.panes() {
        Ok(rows) => rows,
        // A stop was already confirmed, or nothing ever started, and
        // nothing has started since.
        Err(_) if meta.stopped_at.is_some() || meta.windows_started == Some(false) => Vec::new(),
        Err(e) => {
            return Err(format!(
                "{e}; nothing was changed. If team `{team}` is really gone (for example after a reboot), `brg up` and then `brg down` clear it"
            ));
        }
    };
    let in_session = |r: &&lifecycle::PaneRow| r.session == session;
    let mut messages = Vec::new();
    // The processes under the panes this `down` stops, found before tmux
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
    // Panes that are certainly stopped now: killed by this `down`, or not on
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
    if let Some(paths) = &run {
        for id in &members {
            let Some(inc) = crate::bus::manifest::load_incarnation(paths, id).map_err(|_| {
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
                lifecycle::end_incarnation(paths, id, lifecycle::ENDED_STOPPED)?;
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
    Ok(messages)
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusRow {
    pub name: String,
    pub kind: &'static str,
    pub vendor: Option<String>,
    /// `"<model>"` or `"<model> <effort>"`, as declared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The member's parent, if it has one: rows come in tree order.
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
    /// The folder `up` accepted Claude's trust dialog for, if it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust_accepted: Option<String>,
    /// With a bus: whether the member's current incarnation may still be
    /// running (`alive`, `gone` or `unknown`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub liveness: Option<Liveness>,
    /// A `start: true` member that ran and is `stopped` while the rest of
    /// the team runs: it should be running.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub expected_running: bool,
    /// The member's declared `start`.
    #[serde(skip)]
    pub start: bool,
}

/// A member's state when it has never run in the team's current storage.
pub const NOT_STARTED: &str = "not started";
/// A member's state when it ran and has ended.
pub const STOPPED: &str = "stopped";

/// What `up` and `status` print for a member that has never run.
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
        NOT_STARTED => not_started(row.start),
        STOPPED if row.expected_running => format!("{STOPPED} (should be running)"),
        s => s.to_string(),
    }
}

/// The name a deployment's team goes by in messages.
pub fn team_name(input: &Input) -> String {
    state::team_name(&state_for(input).source)
}

/// One snapshot of the team.
pub fn status(input: &Input) -> Result<Vec<StatusRow>, String> {
    let (report, validated) = validate(input);
    let v = validated.ok_or_else(|| {
        report
            .errors()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    })?;
    let dep = state_for(input);
    let state = &dep.state;
    let session = state.session();
    let tmux = tmux_for(state);
    let windows = tmux.windows(&session);
    let meta = state::read_meta(state).unwrap_or_default();
    let run = meta.run.as_ref().map(|r| RunPaths::from_root(&r.dir));
    let nodes: Vec<tree::Node> = v
        .team
        .members
        .values()
        .map(|m| tree::Node {
            id: m.id.clone(),
            parent: m.parent.clone(),
            cols: Vec::new(),
        })
        .collect();
    // The team runs while any member has a live window; a team stopped on
    // purpose expects no one to be running.
    let team_running = windows
        .iter()
        .any(|w| !w.dead && v.team.members.contains_key(&w.name));
    let mut rows = Vec::new();
    for (i, _) in tree::order(&nodes) {
        let id = &nodes[i].id;
        let member = &v.team.members[id];
        let posture = v.posture(id);
        // Whether it ever ran here: with a bus, an incarnation in the team's
        // current storage (one that can't be read counts); without one, a
        // start recorded since the team last started afresh (or no record).
        let incarnation = match &run {
            Some(paths) => crate::bus::manifest::load_incarnation(paths, id),
            None => Ok(None),
        };
        let ran = match (&run, &incarnation) {
            (Some(_), Ok(inc)) => inc.is_some(),
            (Some(_), Err(_)) => true,
            // State from an earlier brg didn't record starts: assume it ran.
            (None, _) => meta
                .members
                .get(id)
                .is_some_and(|m| m.started != Some(false)),
        };
        let mut row = StatusRow {
            name: id.clone(),
            kind: "member",
            vendor: Some(member.vendor.as_str().into()),
            model: member.model.as_ref().map(|m| match &m.effort {
                Some(e) => format!("{} {e}", m.name),
                None => m.name.clone(),
            }),
            parent: member.parent.clone(),
            liveness: incarnation
                .as_ref()
                .ok()
                .and_then(|i| i.as_ref())
                .zip(run.as_ref())
                .map(|(inc, paths)| lifecycle::liveness(paths, id, &inc.incarnation)),
            posture: Some(match posture {
                Posture::Vendor => "vendor".into(),
                Posture::Locked => "locked".into(),
            }),
            state: if ran { STOPPED } else { NOT_STARTED }.into(),
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
                let mut details = Vec::new();
                if posture == Posture::Locked && !state.gate_marker(id).exists() {
                    details.push("gate not active: gets no input".to_string());
                    if row.state != "blocked" {
                        row.state = "gate-inactive".into();
                    }
                }
                // Nothing is typed into it (`send`, the wake, folder trust)
                // until it leaves copy mode.
                if w.in_mode {
                    details.push(format!("{}: {}", tmux::IN_MODE, tmux::leave_mode_hint(id)));
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
    Ok(rows)
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

/// `brg status` as text: the team's tree with each member's vendor, model,
/// state and details, then the plugin processes.
pub fn render_status(team: &str, rows: &[StatusRow]) -> String {
    let nodes: Vec<tree::Node> = rows
        .iter()
        .filter(|r| r.kind == "member")
        .map(|r| {
            let mut detail = Vec::new();
            if r.posture.as_deref() == Some("locked") {
                detail.push("locked".to_string());
            }
            detail.extend(r.detail.clone());
            tree::Node {
                id: r.name.clone(),
                parent: r.parent.clone(),
                cols: vec![
                    format!(
                        "{} {}",
                        r.vendor.as_deref().unwrap_or("-"),
                        r.model.as_deref().unwrap_or("default")
                    ),
                    state_label(r),
                    detail.join("; "),
                ],
            }
        })
        .collect();
    let plugins: Vec<(String, Vec<String>)> = rows
        .iter()
        .filter(|r| r.kind == "plugin")
        .map(|r| {
            (
                r.name.clone(),
                vec![r.state.clone(), r.detail.clone().unwrap_or_default()],
            )
        })
        .collect();
    tree::render(team, &nodes, &plugins)
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

/// `brg send <member> <text>`.
pub fn send(
    input: &Input,
    member: &str,
    text: &str,
    force: bool,
    from: Option<&str>,
) -> Result<String, SendError> {
    check_text(text)?;
    let dep = state_for(input);
    let state = &dep.state;
    let meta = state::read_meta(state)
        .ok_or_else(|| SendError::NotAlive("the team isn't running".into()))?;
    let member_meta = meta
        .members
        .get(member)
        .ok_or_else(|| SendError::Usage(format!("`{member}` is not a member")))?;
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
            "`{member}` is `locked` but its gate is not active; it gets no input until it is (restart it with `brg up --member {member}`)"
        )));
    }
    let session = state.session();
    let tmux = tmux_for(state);

    // Lock files are created at `up`, outside any sandbox; inside one, `send`
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
                "`{member}` is waiting at a prompt; attach to answer it (`brg attach {member}`)"
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
            tmux::leave_mode_hint(member)
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
            "`{member}` showed a prompt while the text was being pasted; Enter was not pressed. Attach to check it (`brg attach {member}`)"
        )));
    }
    // A window that can't be read now counts as one in copy mode: Enter is
    // only pressed on a pane known to take it.
    if now.as_ref().is_none_or(|w| w.in_mode) {
        return Err(SendError::Blocked(format!(
            "`{member}`: {} after the text was pasted; Enter was not pressed, and the text is in its input box. Check it: {}",
            tmux::IN_MODE,
            tmux::leave_mode_hint(member)
        )));
    }
    tmux.ok(&["send-keys", "-t", &t, "Enter"])
        .map_err(SendError::Refused)?;
    drop(lock);
    Ok(format!("sent to {member}"))
}

/// `brg logs <member>`.
pub fn logs(input: &Input, name: &str, lines: usize) -> Result<String, String> {
    let dep = state_for(input);
    let session = dep.state.session();
    let tmux = tmux_for(&dep.state);
    let window = window_name(&tmux, &session, name)?;
    Ok(tmux.history(&session, &window, lines))
}

fn window_name(tmux: &Tmux, session: &str, name: &str) -> Result<String, String> {
    if tmux.window(session, name).is_some() {
        return Ok(name.to_string());
    }
    let plugin = format!("_{name}");
    if tmux.window(session, &plugin).is_some() {
        return Ok(plugin);
    }
    Err(format!("`{name}` is not running"))
}

fn attach_command(input: &Input, name: &str) -> Result<Command, String> {
    let dep = state_for(input);
    let session = dep.state.session();
    let tmux = tmux_for(&dep.state);
    let window = window_name(&tmux, &session, name)?;
    let mut cmd = Command::new("tmux");
    match state::read_meta(&dep.state).and_then(|m| m.socket_path) {
        Some(path) => cmd.arg("-S").arg(path),
        None => cmd.arg("-L").arg(state::tmux_socket()),
    };
    cmd.arg("-f")
        .arg(dep.state.tmux_conf())
        .args(["attach-session", "-t", &target(&session, &window)])
        // Works from inside your own tmux too: brg's server is a separate one.
        .env_remove("TMUX");
    match std::env::var_os(state::ENV_TMUX_TMPDIR) {
        Some(dir) => cmd.env("TMUX_TMPDIR", dir),
        None => cmd.env_remove("TMUX_TMPDIR"),
    };
    Ok(cmd)
}

/// `brg attach <member>`: replaces this process with a tmux client.
pub fn attach(input: &Input, name: &str) -> Result<(), String> {
    let err = attach_command(input, name)?.exec();
    Err(format!("cannot start tmux: {err}"))
}

/// Attach, and return when the client detaches (used by `status -i`).
pub fn attach_and_wait(input: &Input, name: &str) -> Result<(), String> {
    let status = attach_command(input, name)?
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
    fn base_env_is_small() {
        let env = base_env();
        for key in env.keys() {
            assert!(BASE_ENV.contains(&key.as_str()) || key == "LANG", "{key}");
        }
    }
}
