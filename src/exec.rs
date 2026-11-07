//! The hidden helpers brg runs inside tmux windows and member shells:
//! `_exec` (start a window's program with its private environment) and
//! `_run` (run a plugin command for a member, with the plugin's secrets).

use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde::{Deserialize, Serialize};

use crate::state::{self, StateDir};

/// What `_exec` starts: written at a start, read once and deleted.
#[derive(Debug, Serialize, Deserialize)]
pub struct ExecSpec {
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
}

/// The plugin's secret and non-secret values, kept in a 0600 file for the
/// shims while the instance runs.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PluginEnv {
    pub env: BTreeMap<String, String>,
}

fn fail(message: String) -> ExitCode {
    eprintln!("brg: {message}");
    ExitCode::from(127)
}

/// `brg _exec <state> <window>`.
pub fn exec_main(state_root: &Path, window: &str) -> ExitCode {
    let state = StateDir::from_root(state_root);
    let path = state.exec_file(window);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => return fail(format!("cannot read launch file for `{window}`: {e}")),
    };
    // Remove it before starting, so secrets don't sit on disk.
    let _ = std::fs::remove_file(&path);
    let spec: ExecSpec = match serde_json::from_str(&text) {
        Ok(s) => s,
        Err(e) => return fail(format!("bad launch file for `{window}`: {e}")),
    };
    let Some((program, args)) = spec.argv.split_first() else {
        return fail(format!("empty command for `{window}`"));
    };
    let mut env = spec.env;
    // The window's terminal: tmux sets TERM from the server's
    // `default-terminal`, and the config declares RGB, so colour reaches the
    // pane. A value the launch file names itself wins.
    if let Ok(term) = std::env::var("TERM") {
        env.entry("TERM".into()).or_insert(term);
    }
    env.entry("COLORTERM".into())
        .or_insert_with(|| "truecolor".into());
    let err = Command::new(program)
        .args(args)
        .env_clear()
        .envs(&env)
        .current_dir(&spec.cwd)
        .exec();
    fail(format!("cannot start `{program}`: {err}"))
}

/// `brg _run <state> <member> <plugin> <command> [args…]`.
pub fn run_main(
    state_root: &Path,
    member: &str,
    plugin: &str,
    command: &str,
    args: &[String],
) -> ExitCode {
    let state = StateDir::from_root(state_root);
    let Some(meta) = state::read_meta(&state) else {
        return fail(format!(
            "instance `{}` isn't running (no state); start it with `brg start {}`",
            state.id, state.id
        ));
    };
    let Some(plugin_meta) = meta.plugins.get(plugin) else {
        return fail(format!("unknown plugin `{plugin}`"));
    };
    let secrets: PluginEnv = match std::fs::read_to_string(state.plugin_env_file(plugin)) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(e) => return fail(format!("cannot read plugin `{plugin}` settings: {e}")),
    };
    let parent = meta.members.get(member).and_then(|m| m.parent.clone());
    let mut env = meta.base_env.clone();
    env.extend(secrets.env);
    env.extend(identity_env(
        &meta,
        &state,
        member,
        parent.as_deref(),
        plugin,
    ));

    let (program, cwd) = match &plugin_meta.dir {
        Some(dir) => (dir.join(command), dir.clone()),
        None => (
            PathBuf::from(command),
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
        ),
    };
    let err = Command::new(&program)
        .args(args)
        .env_clear()
        .envs(&env)
        .current_dir(cwd)
        .exec();
    fail(format!("cannot run `{}`: {err}", program.display()))
}

/// Set for members and plugin actions: the member's role.
pub const ENV_ROLE: &str = "BORGCHART_ROLE";
/// Set for members: the instance's notes folder.
pub const ENV_NOTES: &str = "BORGCHART_NOTES";

/// The `BORGCHART_*` variables a plugin program sees.
pub fn identity_env(
    meta: &state::Meta,
    state: &StateDir,
    member: &str,
    parent: Option<&str>,
    plugin: &str,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert(crate::address::ENV_INSTANCE.into(), state.id.clone());
    env.insert("BORGCHART_BIN".into(), meta.brg.display().to_string());
    env.insert(
        "BORGCHART_STATE_DIR".into(),
        state.plugin_dir(plugin).display().to_string(),
    );
    if !member.is_empty() {
        env.insert("BORGCHART_MEMBER".into(), member.to_string());
        if let Some(role) = meta.members.get(member).map(|m| m.role.clone()) {
            env.insert(ENV_ROLE.into(), role);
        }
    }
    if let Some(parent) = parent {
        env.insert("BORGCHART_PARENT".into(), parent.to_string());
    }
    env
}
