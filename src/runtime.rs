//! The tmux runtime: `up`, `down`, `status`, `attach`, `logs` and `send`.

use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;

use crate::deploy::Auth;
use crate::exec::{ExecSpec, PluginEnv, identity_env};
use crate::launch;
use crate::rules::{self, State};
use crate::state::{
    self, MemberMeta, Meta, PluginMeta, StateDir, shell_quote, write_private, write_script,
};
use crate::team::{PluginLocation, Posture, Vendor};
use crate::tmux::{Tmux, target};
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

fn tmux_for(state: &StateDir) -> Tmux {
    Tmux::new(&state.tmux_conf(), base_env())
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
}

/// The id of the rule that recognizes Claude's folder-trust dialog.
const TRUST_RULE: &str = "trust-dialog";

/// The folder Claude's trust dialog names: the first non-blank line after "Accessing workspace:".
pub fn trust_dialog_path(screen: &str) -> Option<PathBuf> {
    let mut lines = screen.lines();
    lines.find(|l| l.trim() == "Accessing workspace:")?;
    let path = lines.find(|l| !l.trim().is_empty())?.trim();
    path.starts_with('/').then(|| PathBuf::from(path))
}

pub struct UpOutcome {
    pub messages: Vec<String>,
    pub failed: bool,
}

/// `brg up [--member ID]`.
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
    let brg = brg_path();
    let base = base_env();
    let tmux = tmux_for(state);
    state::ensure_dir(&state.root)
        .map_err(|e| format!("cannot create {}: {e}", state.root.display()))?;
    tmux.write_config()
        .map_err(|e| format!("cannot write tmux config: {e}"))?;

    // Plugins: their values (secrets included) go to private files for the shims.
    let mut meta = Meta {
        source: dep.source.clone(),
        team_dir: v.team.dir.canonicalize().unwrap_or(v.team.dir.clone()),
        brg: brg.clone(),
        base_env: base.clone(),
        ..Meta::default()
    };
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
        let body = serde_json::to_vec(&PluginEnv { env: values }).map_err(|e| e.to_string())?;
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
    if !missing.is_empty() {
        return Err(format!(
            "set these before `brg up` (in your environment, or non-secret ones in borgchart.yaml): {}",
            missing.join(", ")
        ));
    }

    let mut messages = Vec::new();
    let mut failed = false;
    let mut started: Vec<Started> = Vec::new();
    for id in v.team.members.keys() {
        state::open_lock(&state.lock_file(id))
            .map_err(|e| format!("cannot create send lock: {e}"))?;
    }

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
            },
        );
    }
    write_private(
        &state.meta_file(),
        &serde_json::to_vec_pretty(&meta).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("cannot write state: {e}"))?;

    for id in &member_ids {
        let member = &v.team.members[id];
        if let Some(w) = tmux.window(&session, id) {
            if !w.dead {
                messages.push(format!("{id}: already running"));
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

        let mut env = base.clone();
        let path = format!(
            "{}:{}",
            shim_dir.display(),
            base.get("PATH").cloned().unwrap_or_default()
        );
        env.insert("PATH".into(), path);
        env.insert("BORGCHART_DEPLOYMENT".into(), state.id.clone());
        env.insert("BORGCHART_MEMBER".into(), id.clone());
        let tmp = state.member_tmp(id);
        state::ensure_dir(&tmp).map_err(|e| e.to_string())?;
        env.insert("TMPDIR".into(), tmp.display().to_string());
        env.insert(state::ENV_FILE.into(), dep.source.display().to_string());
        if v.auth() == Auth::ApiKey {
            let key = match member.vendor {
                Vendor::Claude => "ANTHROPIC_API_KEY",
                Vendor::Codex => "OPENAI_API_KEY",
            };
            match std::env::var(key) {
                Ok(value) => {
                    env.insert(key.into(), value);
                }
                Err(_) => {
                    messages.push(format!(
                        "{id}: not started: `auth: api_key` needs {key} in your environment"
                    ));
                    failed = true;
                    continue;
                }
            }
        }
        let cwd = deploy_member
            .and_then(|d| d.cwd.clone())
            .unwrap_or_else(|| meta.team_dir.clone());
        let trust_dir = deploy_member
            .and_then(|d| d.cwd.as_ref())
            .filter(|_| match v.deployment.as_ref().and_then(|d| d.auto_trust) {
                Some(on) => on,
                // By default only `locked` members, which ignore project settings.
                None => posture == Posture::Locked,
            })
            .filter(|_| member.vendor == Vendor::Claude)
            .map(|c| c.canonicalize().unwrap_or_else(|_| c.clone()));
        start_window(
            &tmux,
            state,
            &session,
            id,
            &ExecSpec {
                argv: spec.argv,
                env,
                cwd,
            },
        )?;
        started.push(Started {
            id: id.clone(),
            vendor: member.vendor,
            locked: posture == Posture::Locked,
            trust_dir,
        });
        messages.push(format!(
            "{id}: started ({}, {})",
            member.vendor.as_str(),
            meta.members[id].posture
        ));
    }

    // Plugin processes start with the whole team.
    if only.is_none() {
        for (name, plugin) in &v.team.plugins {
            let window = format!("_{name}");
            let dir = meta.plugins[name].dir.clone();
            let Some((argv, cwd)) = launch::process_argv(plugin, dir.as_deref()) else {
                continue;
            };
            if let Some(w) = tmux.window(&session, &window) {
                if !w.dead {
                    messages.push(format!("plugin {name}: already running"));
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
                state,
                &session,
                &window,
                &ExecSpec { argv, env, cwd },
            )?;
            messages.push(format!("plugin {name}: started"));
        }
    }

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
                messages.push(format!("{id}: window disappeared"));
                failed = true;
                break;
            };
            if w.dead {
                messages.push(format!(
                    "{id}: exited during start (status {})",
                    w.dead_status.unwrap_or(-1)
                ));
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
                && trust_dialog_path(&screen).as_deref() == s.trust_dir.as_deref();
            if accept {
                let dir = s
                    .trust_dir
                    .as_ref()
                    .map(|d| d.display().to_string())
                    .unwrap_or_default();
                // Down to "Yes, I trust this folder", then Enter. Nothing else is ever answered.
                let t = target(&session, id);
                tmux.ok(&[
                    "send-keys",
                    "-t",
                    &t,
                    "Down",
                    ";",
                    "send-keys",
                    "-t",
                    &t,
                    "Enter",
                ])?;
                let _ = write_private(
                    &state.member_dir(id).join("trust-accepted"),
                    format!("{dir}\n").as_bytes(),
                );
                messages.push(format!("{id}: accepted folder trust for {dir}"));
                trusted = true;
                std::thread::sleep(Duration::from_millis(300));
                continue;
            }
            match reading.map(|r| r.state) {
                Some(State::Blocked) => {
                    messages.push(format!(
                        "{id}: waiting at a prompt (folder trust?); run `brg attach {id}` to answer it"
                    ));
                    break;
                }
                Some(State::Idle) | Some(State::Working) if !s.locked => break,
                _ => {}
            }
            if Instant::now() > deadline {
                if s.locked {
                    let _ = tmux.kill_window(&session, id);
                    messages.push(format!(
                        "{id}: stopped: its `locked` gate did not report in, so it would run unprotected"
                    ));
                    failed = true;
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    Ok(UpOutcome { messages, failed })
}

fn start_window(
    tmux: &Tmux,
    state: &StateDir,
    session: &str,
    window: &str,
    spec: &ExecSpec,
) -> Result<(), String> {
    let body = serde_json::to_vec(spec).map_err(|e| e.to_string())?;
    write_private(&state.exec_file(window), &body)
        .map_err(|e| format!("cannot write launch file: {e}"))?;
    let command = format!(
        "{} _exec {} {}",
        shell_quote(&brg_path().display().to_string()),
        shell_quote(&state.root.display().to_string()),
        shell_quote(window)
    );
    tmux.start_window(session, window, &command)
        .map_err(|e| format!("cannot start `{window}`: {e}"))
}

/// `brg down [--member ID]`.
pub fn down(input: &Input, only: Option<&str>) -> Result<Vec<String>, String> {
    let dep = state_for(input);
    let state = &dep.state;
    let session = state.session();
    let tmux = tmux_for(state);
    let mut messages = Vec::new();
    match only {
        Some(id) => {
            if tmux.window(&session, id).is_some() {
                tmux.kill_window(&session, id)?;
                messages.push(format!("{id}: stopped"));
            } else {
                messages.push(format!("{id}: not running"));
            }
            let _ = std::fs::remove_file(state.gate_marker(id));
            let _ = std::fs::remove_file(state.exec_file(id));
        }
        None => {
            if tmux.has_session(&session) {
                tmux.kill_session(&session)?;
                messages.push("stopped".into());
            } else {
                messages.push("not running".into());
            }
            // Remove the private files that held secrets and launch details.
            let _ = std::fs::remove_dir_all(state.root.join("exec"));
            if let Some(meta) = state::read_meta(state) {
                for name in meta.plugins.keys() {
                    let _ = std::fs::remove_file(state.plugin_env_file(name));
                }
                for id in meta.members.keys() {
                    let _ = std::fs::remove_file(state.gate_marker(id));
                }
            }
        }
    }
    Ok(messages)
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusRow {
    pub name: String,
    pub kind: &'static str,
    pub vendor: Option<String>,
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
    let mut rows = Vec::new();
    for (id, member) in &v.team.members {
        let posture = v.posture(id);
        let mut row = StatusRow {
            name: id.clone(),
            kind: "member",
            vendor: Some(member.vendor.as_str().into()),
            posture: Some(match posture {
                Posture::Vendor => "vendor".into(),
                Posture::Locked => "locked".into(),
            }),
            state: "stopped".into(),
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
                if posture == Posture::Locked && !state.gate_marker(id).exists() {
                    row.detail = Some("gate not active".into());
                }
            }
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
            vendor: None,
            posture: None,
            state: "stopped".into(),
            rule: None,
            detail: None,
            vendor_version: None,
            trust_accepted: None,
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

/// A plugin's `health.json`: `{ok, at, detail}`.
fn health(dir: &Path) -> Option<String> {
    let path = dir.join("health.json");
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
        format!("error: {detail}")
    })
}

/// Lines in a plugin's `undelivered.jsonl`, if any.
fn pending(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("undelivered.jsonl")).ok()?;
    let n = text.lines().filter(|l| !l.trim().is_empty()).count();
    (n > 0).then(|| format!("{n} undelivered"))
}

pub fn render_table(rows: &[StatusRow]) -> String {
    let headers = ["NAME", "KIND", "VENDOR", "POSTURE", "STATE", "DETAIL"];
    let cells: Vec<[String; 6]> = rows
        .iter()
        .map(|r| {
            [
                r.name.clone(),
                r.kind.to_string(),
                r.vendor.clone().unwrap_or_else(|| "-".into()),
                r.posture.clone().unwrap_or_else(|| "-".into()),
                r.state.clone(),
                r.detail.clone().unwrap_or_default(),
            ]
        })
        .collect();
    let mut widths = headers.map(str::len);
    for row in &cells {
        for (i, c) in row.iter().enumerate() {
            widths[i] = widths[i].max(c.chars().count());
        }
    }
    let line = |cols: &[String]| {
        cols.iter()
            .enumerate()
            .map(|(i, c)| format!("{c:<w$}", w = widths[i]))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    let mut out = line(&headers.map(String::from));
    out.push('\n');
    for row in &cells {
        out.push_str(&line(row));
        out.push('\n');
    }
    out
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
pub fn send(input: &Input, member: &str, text: &str, force: bool) -> Result<String, SendError> {
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
    // Check again before pressing Enter, in case a dialog appeared meanwhile.
    let title = tmux
        .window(&session, member)
        .map(|w| w.title)
        .unwrap_or_default();
    if rules.read(&title, &tmux.capture(&session, member)).state == State::Blocked {
        return Err(SendError::Blocked(format!(
            "`{member}` showed a prompt while the text was being pasted; Enter was not pressed. Attach to check it (`brg attach {member}`)"
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

/// `brg attach <member>`: replaces this process with a tmux client.
pub fn attach(input: &Input, name: &str) -> Result<(), String> {
    let dep = state_for(input);
    let session = dep.state.session();
    let tmux = tmux_for(&dep.state);
    let window = window_name(&tmux, &session, name)?;
    let err = Command::new("tmux")
        .arg("-L")
        .arg(state::tmux_socket())
        .arg("-f")
        .arg(dep.state.tmux_conf())
        .args(["attach-session", "-t", &target(&session, &window)])
        .env_remove("TMUX")
        .exec();
    Err(format!("cannot run tmux: {err}"))
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
