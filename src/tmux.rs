//! A thin wrapper over borgchart's private tmux server.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::sidebar::SIDEBAR;
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

/// The sidebar's hooks and `prefix S`, with `brg` the absolute path of the
/// brg writing the config: each runs `brg _sidebar` for the server, in the
/// background, with the server's environment, which brg started it with.
fn sidebar_config(brg: &Path) -> String {
    // run-shell expands its command as a format, then sh runs it: the path
    // comes from an option, quoted for sh by `q:`.
    let sync = "#{q:@brg-bin} _sidebar #{q:socket_path}";
    let shell = |args: &str| quote(&format!("{sync}{args} >/dev/null 2>&1"));
    let mut text = format!("set -g @brg-bin {}\n", quote(&brg.display().to_string()));
    for hook in [
        "client-attached",
        "client-detached",
        "client-resized",
        "client-session-changed",
        "session-window-changed",
    ] {
        let run = format!("run-shell -b {}", shell(""));
        text.push_str(&format!("set-hook -g {hook} {}\n", quote(&run)));
    }
    text.push_str(&format!(
        "bind-key S run-shell -b {}\n",
        shell(" --toggle #{client_pid}")
    ));
    text
}

/// `text` as one word for tmux's command parser, which reads nothing inside
/// single quotes: `'` itself goes in double quotes.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\"'\"'"))
}

/// The config text: brg's own, then the sidebar's (for `brg`, the brg that
/// writes it), or, with `user` (a path without `'` or a newline), those,
/// then that file, then brg's required options again.
pub fn config_text(user: Option<&Path>, brg: &Path) -> String {
    let own = format!("{CONFIG}{}", sidebar_config(brg));
    let Some(user) = user else {
        return own;
    };
    let user = user.display();
    format!(
        "{own}set -gF @brg-default-terminal '#{{default-terminal}}'\nset -gF @brg-default-shell '#{{default-shell}}'\nsource-file -q '{user}'\n{REQUIRED}set -g {USER_CONFIG_OPTION} '{user}'\n"
    )
}

/// The pane option brg sets on the pane it starts a member (or plugin
/// process) in, so that pane is found whatever else opens in its window.
pub const OWN_PANE: &str = "@brg-own-pane";

/// One pane's marks: whether it carries [`OWN_PANE`], and whether it is
/// brg's sidebar ([`crate::sidebar::SIDEBAR`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Marks {
    pub own: bool,
    pub sidebar: bool,
}

/// Which of a window's panes is the one brg started: never a sidebar; of the
/// rest, the marked one, or with none marked, the only one (a window an older
/// brg opened, or one read before its mark is set). With several and none
/// marked, none: brg doesn't guess.
pub fn own_pane(panes: &[Marks]) -> Option<usize> {
    let rest: Vec<usize> = (0..panes.len()).filter(|&i| !panes[i].sidebar).collect();
    match rest.iter().find(|&&i| panes[i].own) {
        Some(&i) => Some(i),
        None if rest.len() == 1 => Some(rest[0]),
        None => None,
    }
}

/// A window whose every pane is a sidebar: its member's pane is gone.
pub fn only_sidebars(panes: &[Marks]) -> bool {
    panes.iter().all(|p| p.sidebar)
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
    /// The pane brg started in it ([`own_pane`]), if it can be told; the
    /// fields below are that pane's. Without one, nothing is typed into the
    /// window and its screen reads as empty.
    pub pane: Option<String>,
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
        // Every pane: a window's active pane may be another one beside brg's.
        let format = format!(
            "#{{window_id}}\t#{{window_name}}\t#{{pane_id}}\t#{{{OWN_PANE}}}\t#{{{SIDEBAR}}}\t#{{pane_dead}}\t#{{pane_dead_status}}\t#{{pane_pid}}\t#{{pane_in_mode}}\t#{{pane_title}}"
        );
        // The trailing `:` names the session, never a window of that name.
        let Ok(out) = self.ok(&[
            "list-panes",
            "-s",
            "-t",
            &format!("={session}:"),
            "-F",
            &format,
        ]) else {
            return Vec::new();
        };
        // Each window's panes, in tmux's order.
        let mut by_window: Vec<(String, Vec<Vec<&str>>)> = Vec::new();
        for line in out.lines() {
            let fields: Vec<&str> = line.splitn(10, '\t').collect();
            if fields.len() < 9 {
                continue;
            }
            match by_window.iter_mut().find(|(id, _)| id == fields[0]) {
                Some((_, panes)) => panes.push(fields),
                None => by_window.push((fields[0].to_string(), vec![fields])),
            }
        }
        by_window
            .into_iter()
            .map(|(_, panes)| {
                let marks: Vec<Marks> = panes
                    .iter()
                    .map(|p| Marks {
                        own: !p[3].is_empty(),
                        sidebar: !p[4].is_empty(),
                    })
                    .collect();
                let name = panes[0][1].to_string();
                match own_pane(&marks).map(|i| &panes[i]) {
                    Some(p) => WindowInfo {
                        name,
                        pane: Some(p[2].to_string()),
                        dead: p[5] == "1",
                        dead_status: p[6].parse().ok(),
                        pid: p[7].parse().ok(),
                        in_mode: in_mode(p[8]),
                        title: p.get(9).unwrap_or(&"").to_string(),
                    },
                    // Only a sidebar is left: the member's pane is gone.
                    None if only_sidebars(&marks) => WindowInfo {
                        name,
                        pane: None,
                        dead: true,
                        dead_status: None,
                        pid: None,
                        in_mode: true,
                        title: String::new(),
                    },
                    // Running for all brg can tell, but not to be typed into.
                    None => WindowInfo {
                        name,
                        pane: None,
                        dead: false,
                        dead_status: None,
                        pid: None,
                        in_mode: true,
                        title: String::new(),
                    },
                }
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

    /// Start `command` in a new window, creating the session if needed, and
    /// mark its pane as brg's ([`OWN_PANE`]), by the ID tmux gives it: a
    /// hook may already have opened another pane in the window.
    pub fn start_window(&self, session: &str, name: &str, command: &str) -> Result<(), String> {
        let created = !self.has_session(session);
        let out = if !created {
            self.ok(&[
                "new-window",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                &format!("={session}:"),
                "-n",
                name,
                command,
            ])?
        } else {
            self.ok(&[
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-s",
                session,
                "-n",
                name,
                "-x",
                "200",
                "-y",
                "50",
                command,
            ])?
        };
        let pane = out.trim();
        if pane.starts_with('%') {
            self.ok(&["set-option", "-p", "-t", pane, OWN_PANE, "1"])?;
        }
        if created {
            // Which brg deployment the session is for, for its sidebar.
            let abs = |p: PathBuf| std::path::absolute(&p).unwrap_or(p).display().to_string();
            let session = format!("={session}:");
            self.ok(&[
                "set-option",
                "-t",
                &session,
                crate::sidebar::STATE_HOME,
                &abs(crate::state::state_home()),
                ";",
                "set-option",
                "-t",
                &session,
                crate::sidebar::CONFIG,
                &abs(crate::config::path()),
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

    /// The visible screen of the window's own pane (never the scrolled
    /// copy-mode view); empty when it can't be told.
    pub fn capture(&self, session: &str, name: &str) -> String {
        self.window(session, name)
            .and_then(|w| w.pane)
            .map(|pane| self.capture_pane(&pane))
            .unwrap_or_default()
    }

    /// The visible screen of the pane with this ID.
    pub fn capture_pane(&self, pane_id: &str) -> String {
        self.ok(&["capture-pane", "-p", "-t", pane_id])
            .unwrap_or_default()
    }

    /// Recent output of the window's own pane, joined across wrapped lines.
    pub fn history(&self, session: &str, name: &str, lines: usize) -> String {
        let Some(pane) = self.window(session, name).and_then(|w| w.pane) else {
            return String::new();
        };
        let start = format!("-{lines}");
        self.ok(&["capture-pane", "-p", "-J", "-S", &start, "-t", &pane])
            .unwrap_or_default()
    }
}

/// An exact-match target for a window.
pub fn target(session: &str, window: &str) -> String {
    format!("={session}:={window}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pane beside brg's (a sidebar, or one a person split off) is never
    /// taken for it, whatever its place in the window; and in a window with
    /// several panes and no mark, brg picks none rather than guess.
    #[test]
    fn own_pane_is_the_marked_one_or_a_lone_pane() {
        let m = |own, sidebar| Marks { own, sidebar };
        assert_eq!(own_pane(&[m(false, false), m(true, false)]), Some(1));
        assert_eq!(own_pane(&[m(true, false), m(false, false)]), Some(0));
        assert_eq!(own_pane(&[m(false, false)]), Some(0));
        assert_eq!(own_pane(&[m(false, false), m(false, false)]), None);
        assert_eq!(own_pane(&[]), None);
        // A sidebar is never the member, and doesn't hide a lone member.
        assert_eq!(own_pane(&[m(false, false), m(false, true)]), Some(0));
        assert_eq!(own_pane(&[m(false, true)]), None);
        assert!(only_sidebars(&[m(false, true)]));
        assert!(!only_sidebars(&[m(false, true), m(false, false)]));
    }
}
