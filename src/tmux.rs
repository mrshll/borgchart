//! A thin wrapper over borgchart's private tmux server.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::state::tmux_socket;

/// The tmux config brg's private server runs with. Your own `~/.tmux.conf` is
/// read only when brg's configuration opts in (`tmux.user_config`).
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
/// so a later start can tell when the running server differs from what it asks.
pub const USER_CONFIG_OPTION: &str = "@brg-user-config";

/// The instance a session is for, as a tmux format: its name without
/// `brg-`, keeping only the characters an instance ID can have, so it is a
/// plain word for the shell and for tmux's command parser.
const SESSION_INSTANCE: &str = "#{s|[^-_a-z0-9/]||:#{s|^brg-||:session_name}}";

/// The status bar and `prefix S` pop-up, with `brg` the absolute path of the
/// brg writing the config: the bar on top, its right side `brg status
/// --line` for the attached session's instance, and the pop-up `brg status
/// -i` for it, top right, 2 cells from the edges (narrower and shorter on a
/// small client; tmux's `-y` is the pop-up's bottom line). tmux runs both
/// with its server's environment, which brg started it with.
fn status_config(brg: &Path) -> String {
    let brg = brg.display().to_string();
    // The bar's command is expanded as a format, then run by sh: the path
    // comes from an option, quoted for sh by `q:` (a `)` in it written
    // inline would end `#(`).
    let right = format!("#(#{{q:@brg-bin}} status --line {SESSION_INSTANCE})");
    // The pop-up's command is not expanded, so `run-shell -C` expands the
    // whole `display-popup` command (`##` is a literal `#`), then parses it
    // as tmux commands; the path, a word of its own there, runs without a
    // shell.
    let popup = format!(
        "display-popup -x '#{{?#{{e|<:#{{client_width}},66}},0,#{{e|-:#{{client_width}},66}}}}' -y '#{{?#{{e|<:#{{client_height}},18}},#{{client_height}},18}}' -w '#{{?#{{e|<:#{{client_width}},66}},#{{client_width}},64}}' -h '#{{?#{{e|<:#{{client_height}},18}},#{{e|-:#{{client_height}},2}},16}}' -T ' brg status ' -E {} status -i {SESSION_INSTANCE}",
        quote(&brg.replace('#', "##"))
    );
    format!(
        "set -g @brg-bin {}\nset -g status-position top\nset -g status-interval 5\nset -g status-right-length 200\nset -g status-right {}\nbind-key S run-shell -C {}\n",
        quote(&brg),
        quote(&right),
        quote(&popup)
    )
}

/// `text` as one word for tmux's command parser, which reads nothing inside
/// single quotes: `'` itself goes in double quotes.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\"'\"'"))
}

/// The config text: brg's own, or, with `user` (a path without `'` or a
/// newline), brg's, then that file, then brg's required options again.
/// `brg` is the brg the bar and pop-up run.
pub fn config_text(user: Option<&Path>, brg: &Path) -> String {
    let own = format!("{CONFIG}{}", status_config(brg));
    let Some(user) = user else {
        return own;
    };
    let user = user.display();
    format!(
        "{own}set -gF @brg-default-terminal '#{{default-terminal}}'\nset -gF @brg-default-shell '#{{default-shell}}'\nsource-file -q '{user}'\n{REQUIRED}set -g {USER_CONFIG_OPTION} '{user}'\n"
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
    /// tmux copy mode (or another mode) is showing over the program.
    pub in_mode: bool,
    pub title: String,
}

/// A pane in a tmux mode (copy mode, view mode) sends keys to the mode, not
/// the program, though a paste still reaches the program. `send`, folder-trust
/// answering and the wake all refuse to type into one; this reads the
/// `#{pane_in_mode}` field they all fetch.
pub fn in_mode(pane_in_mode: &str) -> bool {
    pane_in_mode != "0"
}

/// Why nothing is typed into a pane in a mode.
pub const IN_MODE: &str = "the pane is in tmux copy mode";

/// How a person gets `member`'s pane out of copy mode.
pub fn leave_mode_hint(member: &str) -> String {
    format!("attach (`brg attach {member}`) and leave copy mode (q, unless your tmux keys differ)")
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

    pub fn write_config(&self, user: Option<&Path>, brg: &Path) -> std::io::Result<()> {
        crate::state::write_script(&self.config, &config_text(user, brg))?;
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
        let format = "#{window_name}\t#{pane_dead}\t#{pane_dead_status}\t#{pane_pid}\t#{pane_in_mode}\t#{pane_title}";
        let Ok(out) = self.ok(&["list-windows", "-t", &format!("={session}"), "-F", format]) else {
            return Vec::new();
        };
        out.lines()
            .filter_map(|line| {
                let mut parts = line.splitn(6, '\t');
                let name = parts.next()?.to_string();
                let dead = parts.next()? == "1";
                let dead_status = parts.next().and_then(|s| s.parse().ok());
                let pid = parts.next().and_then(|s| s.parse().ok());
                let in_mode = in_mode(parts.next()?);
                let title = parts.next().unwrap_or("").to_string();
                Some(WindowInfo {
                    name,
                    dead,
                    dead_status,
                    pid,
                    in_mode,
                    title,
                })
            })
            .collect()
    }

    /// Whether the pane with this ID is in a mode, if it can be read.
    pub fn pane_in_mode(&self, pane_id: &str) -> Option<bool> {
        let out = self
            .ok(&[
                "display-message",
                "-p",
                "-t",
                pane_id,
                "#{pane_id}\t#{pane_in_mode}",
            ])
            .ok()?;
        let (id, mode) = out.trim_end_matches('\n').split_once('\t')?;
        (id == pane_id).then(|| in_mode(mode))
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

/// Whether `tmux_env`, a `$TMUX` value (`<socket path>,<pid>,<session>`),
/// names the server listening at `socket`: the caller runs inside it.
pub fn names_server(tmux_env: Option<&str>, socket: Option<&Path>) -> bool {
    let (Some(env), Some(socket)) = (tmux_env, socket) else {
        return false;
    };
    // The path itself may hold commas; the last two fields can't.
    let mut fields = env.rsplitn(3, ',');
    match (fields.next(), fields.next(), fields.next()) {
        (Some(_), Some(_), Some(path)) => Path::new(path) == socket,
        _ => false,
    }
}

/// An exact-match target for a window.
pub fn target(session: &str, window: &str) -> String {
    format!("={session}:={window}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Enter in `status -i` switches the client only inside the member's own
    /// server; anywhere else a switch would move some other server's client,
    /// or fail where an attach works.
    #[test]
    fn names_server_only_for_the_same_socket() {
        let socket = Path::new("/run/tmux-1000/borg,chart");
        assert!(names_server(
            Some("/run/tmux-1000/borg,chart,4242,3"),
            Some(socket)
        ));
        assert!(!names_server(
            Some("/run/tmux-1000/default,4242,3"),
            Some(socket)
        ));
        assert!(!names_server(
            Some("/run/tmux-1000/borg,chart"),
            Some(socket)
        ));
        assert!(!names_server(Some(""), Some(socket)));
        assert!(!names_server(None, Some(socket)));
        assert!(!names_server(
            Some("/run/tmux-1000/borg,chart,4242,3"),
            None
        ));
    }
}
