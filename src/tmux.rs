//! A thin wrapper over borgchart's private tmux server.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::state::tmux_socket;

/// The tmux config brg's private server runs with. Your own `~/.tmux.conf` is
/// read only when the deployment opts in (`tmux.user_config`).
pub const CONFIG: &str = "\
set -g remain-on-exit on
set -g history-limit 20000
set -g update-environment ''
set -g allow-rename off
set -g status-left '[brg #{session_name}] '
set -g status-left-length 40
set -as terminal-features ',*:RGB'
";

/// What brg relies on, applied again after your own config so it can't change
/// them: windows keep their member names, dead panes, history and pane titles
/// stay readable, nothing ends brg's detached server or sessions, members get
/// no variables from an attaching client, windows start through the shell the
/// server started with, and the terminal stays the one tmux was built with
/// (both saved before your config) with RGB colour.
const REQUIRED: &str = "\
set -g remain-on-exit on
set -g history-limit 20000
set -g update-environment ''
set -g allow-rename off
set -g automatic-rename off
set -g destroy-unattached off
set -s exit-unattached off
set -gq allow-set-title on
set -gF default-terminal '#{@brg-default-terminal}'
set -gF default-shell '#{@brg-default-shell}'
set -as terminal-features ',*:RGB'
";

/// The server option recording which of your configs it loaded ('' for none),
/// so a later `up` can tell when the running server differs from what it asks.
pub const USER_CONFIG_OPTION: &str = "@brg-user-config";

/// The config text: brg's own, or, with `user` (a path without `'` or a
/// newline), brg's, then that file, then brg's required options again.
pub fn config_text(user: Option<&Path>) -> String {
    let Some(user) = user else {
        return CONFIG.to_string();
    };
    let user = user.display();
    format!(
        "{CONFIG}set -gF @brg-default-terminal '#{{default-terminal}}'\nset -gF @brg-default-shell '#{{default-shell}}'\nsource-file -q '{user}'\n{REQUIRED}set -g {USER_CONFIG_OPTION} '{user}'\n"
    )
}

pub struct Tmux {
    socket: String,
    config: PathBuf,
    /// The environment the server is started with (and every client runs with).
    env: BTreeMap<String, String>,
}

/// One window's live facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub name: String,
    pub dead: bool,
    pub dead_status: Option<i32>,
    pub pid: Option<u32>,
    pub title: String,
}

impl Tmux {
    pub fn new(config: &Path, env: BTreeMap<String, String>) -> Self {
        Tmux {
            socket: tmux_socket(),
            config: config.to_path_buf(),
            env,
        }
    }

    /// A client for a named socket, whatever `BORGCHART_TMUX_SOCKET` says.
    pub fn on_socket(socket: &str, config: &Path, env: BTreeMap<String, String>) -> Self {
        Tmux {
            socket: socket.to_string(),
            config: config.to_path_buf(),
            env,
        }
    }

    pub fn socket(&self) -> &str {
        &self.socket
    }

    /// The same client settings for another server (a name or an absolute path).
    pub fn with_socket(&self, socket: &str) -> Self {
        Tmux {
            socket: socket.to_string(),
            config: self.config.clone(),
            env: self.env.clone(),
        }
    }

    pub fn write_config(&self, user: Option<&Path>) -> std::io::Result<()> {
        crate::state::write_script(&self.config, &config_text(user))?;
        std::fs::set_permissions(
            &self.config,
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new("tmux");
        cmd.env_clear().envs(&self.env);
        if let Some(dir) = self.env.get(crate::state::ENV_TMUX_TMPDIR) {
            cmd.env("TMUX_TMPDIR", dir);
        }
        // An absolute socket path (as tmux reports it) or a socket name.
        let flag = if self.socket.starts_with('/') {
            "-S"
        } else {
            "-L"
        };
        cmd.arg(flag).arg(&self.socket).arg("-f").arg(&self.config);
        cmd
    }

    pub fn run(&self, args: &[&str]) -> Result<Output, String> {
        let out = self
            .command()
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot start tmux: {e}"))?;
        Ok(out)
    }

    pub fn ok(&self, args: &[&str]) -> Result<String, String> {
        let out = self.run(args)?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).to_string())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    pub fn run_with_stdin(&self, args: &[&str], input: &[u8]) -> Result<(), String> {
        let mut child = self
            .command()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot start tmux: {e}"))?;
        child
            .stdin
            .take()
            .ok_or("no stdin")?
            .write_all(input)
            .map_err(|e| format!("cannot write to tmux: {e}"))?;
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    pub fn has_session(&self, session: &str) -> bool {
        self.run(&["has-session", "-t", &format!("={session}")])
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    pub fn windows(&self, session: &str) -> Vec<WindowInfo> {
        let format =
            "#{window_name}\t#{pane_dead}\t#{pane_dead_status}\t#{pane_pid}\t#{pane_title}";
        let Ok(out) = self.ok(&["list-windows", "-t", &format!("={session}"), "-F", format]) else {
            return Vec::new();
        };
        out.lines()
            .filter_map(|line| {
                let mut parts = line.splitn(5, '\t');
                let name = parts.next()?.to_string();
                let dead = parts.next()? == "1";
                let dead_status = parts.next().and_then(|s| s.parse().ok());
                let pid = parts.next().and_then(|s| s.parse().ok());
                let title = parts.next().unwrap_or("").to_string();
                Some(WindowInfo {
                    name,
                    dead,
                    dead_status,
                    pid,
                    title,
                })
            })
            .collect()
    }

    pub fn window(&self, session: &str, name: &str) -> Option<WindowInfo> {
        self.windows(session).into_iter().find(|w| w.name == name)
    }

    /// Start `command` in a new window, creating the session if needed.
    pub fn start_window(&self, session: &str, name: &str, command: &str) -> Result<(), String> {
        if self.has_session(session) {
            self.ok(&[
                "new-window",
                "-d",
                "-t",
                &format!("={session}:"),
                "-n",
                name,
                command,
            ])?;
        } else {
            self.ok(&[
                "new-session",
                "-d",
                "-s",
                session,
                "-n",
                name,
                "-x",
                "200",
                "-y",
                "50",
                command,
            ])?;
        }
        Ok(())
    }

    pub fn kill_window(&self, session: &str, name: &str) -> Result<(), String> {
        self.ok(&["kill-window", "-t", &target(session, name)])
            .map(|_| ())
    }

    pub fn kill_session(&self, session: &str) -> Result<(), String> {
        self.ok(&["kill-session", "-t", &format!("={session}")])
            .map(|_| ())
    }

    /// The visible screen (never the scrolled copy-mode view).
    pub fn capture(&self, session: &str, name: &str) -> String {
        self.ok(&["capture-pane", "-p", "-t", &target(session, name)])
            .unwrap_or_default()
    }

    /// Recent output, joined across wrapped lines.
    pub fn history(&self, session: &str, name: &str, lines: usize) -> String {
        let start = format!("-{lines}");
        self.ok(&[
            "capture-pane",
            "-p",
            "-J",
            "-S",
            &start,
            "-t",
            &target(session, name),
        ])
        .unwrap_or_default()
    }
}

/// An exact-match target for a window.
pub fn target(session: &str, window: &str) -> String {
    format!("={session}:={window}")
}
