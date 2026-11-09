//! The status sidebar: a pane beside each member window someone views,
//! running `brg status -w` for every instance.
//!
//! tmux runs `brg _sidebar <socket>` from hooks in brg's config whenever a
//! client attaches, detaches, resizes, or moves to another session or
//! window, and `prefix S` runs it with `--toggle <client pid>`. Each run
//! makes the whole server match what its clients view: one sidebar in each
//! member window a client views (unless every viewer hid it, or the window
//! is too narrow), and none anywhere else. Runs take a lock beside the
//! server's socket, so two at once can't both add one.

use std::collections::BTreeSet;
use std::path::Path;

use crate::tmux::{Marks, OWN_PANE, Tmux, own_pane};

/// The pane option marking a sidebar pane.
pub const SIDEBAR: &str = "@brg-sidebar";

/// The server option listing the clients (by process ID) that hid the
/// sidebar with `prefix S`.
pub const HIDDEN: &str = "@brg-sidebar-hidden";

/// The sidebar's width, where there is room.
pub const WIDTH: u32 = 42;

/// The narrowest sidebar brg adds.
pub const MIN_SIDEBAR: u32 = 24;

/// The narrowest a member's pane is left beside a sidebar. Being wider than
/// 78 also keeps the wake's reminder (76 characters after a 2-column
/// prompt) from ending exactly at the input box's edge, where the wake can't
/// confirm it and so never types it.
pub const MIN_MEMBER: u32 = 80;

/// The sidebar's width when it and the member's pane share `room` columns
/// (the member's pane before the split, or both and the border between them):
/// [`WIDTH`], or less to leave the member [`MIN_MEMBER`], and none if that
/// leaves the sidebar under [`MIN_SIDEBAR`].
pub fn width(room: u32) -> Option<u32> {
    let w = WIDTH.min(room.saturating_sub(MIN_MEMBER + 1));
    (w >= MIN_SIDEBAR).then_some(w)
}

/// One pane on the server, as the sidebar sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pane {
    pub session: String,
    pub window: String,
    pub window_name: String,
    pub id: String,
    pub own: bool,
    pub sidebar: bool,
    pub dead: bool,
    pub width: u32,
    /// The state home and configuration its session's instance was started
    /// with ([`STATE_HOME`], [`CONFIG`]; empty when brg didn't record them).
    pub state_home: String,
    pub config: String,
}

/// The session options recording the state home and configuration brg
/// created a session with, so its sidebar shows that state home's
/// instances, whichever brg started the tmux server.
pub const STATE_HOME: &str = "@brg-state-home";
pub const CONFIG: &str = "@brg-config";

/// What to change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Split a sidebar this wide off the member's pane, running with these
    /// variables.
    Add {
        pane: String,
        width: u32,
        env: Vec<(String, String)>,
    },
    Remove(String),
    Resize {
        pane: String,
        width: u32,
    },
}

/// The changes that leave one live sidebar, of the right width, in each
/// member window in `viewed` (the windows clients that haven't hidden it
/// view) and none in any other window.
pub fn plan(panes: &[Pane], viewed: &BTreeSet<String>) -> Vec<Change> {
    let mut windows: Vec<&str> = panes.iter().map(|p| p.window.as_str()).collect();
    windows.sort_unstable();
    windows.dedup();
    let mut changes = Vec::new();
    for window in windows {
        let in_window: Vec<&Pane> = panes.iter().filter(|p| p.window == window).collect();
        let first = in_window[0];
        let marks: Vec<Marks> = in_window
            .iter()
            .map(|p| Marks {
                own: p.own,
                sidebar: p.sidebar,
            })
            .collect();
        let member = own_pane(&marks).map(|i| in_window[i]);
        let keep = in_window.iter().find(|p| p.sidebar && !p.dead);
        let want = match member {
            Some(m)
                if first.session.starts_with("brg-")
                    && !first.window_name.starts_with('_')
                    && !m.dead
                    && viewed.contains(window) =>
            {
                let room = match keep {
                    Some(k) => m.width + 1 + k.width,
                    None => m.width,
                };
                width(room).map(|w| (m, w))
            }
            _ => None,
        };
        let sidebars = in_window.iter().filter(|p| p.sidebar);
        match want {
            Some((m, w)) => {
                match keep {
                    Some(k) if k.width != w => changes.push(Change::Resize {
                        pane: k.id.clone(),
                        width: w,
                    }),
                    Some(_) => {}
                    None => {
                        let mut env = Vec::new();
                        if !m.state_home.is_empty() {
                            env.push(("BORGCHART_STATE_HOME".into(), m.state_home.clone()));
                        }
                        if !m.config.is_empty() {
                            env.push((crate::config::ENV_CONFIG.into(), m.config.clone()));
                        }
                        changes.push(Change::Add {
                            pane: m.id.clone(),
                            width: w,
                            env,
                        })
                    }
                }
                for p in sidebars.filter(|p| Some(&p.id) != keep.map(|k| &k.id)) {
                    changes.push(Change::Remove(p.id.clone()));
                }
            }
            None => {
                for p in sidebars {
                    changes.push(Change::Remove(p.id.clone()));
                }
            }
        }
    }
    changes
}

/// `brg _sidebar <socket> [--toggle <client pid>]`.
pub fn run(socket: &str, toggle: Option<&str>) -> Result<(), String> {
    // One lock per tmux server, beside its socket: every run for the server
    // takes it, whichever state home the server or the run started with.
    let lock = crate::state::open_lock(Path::new(&format!("{socket}.brg-sidebar.lock")))
        .map_err(|e| format!("cannot open the sidebar lock: {e}"))?;
    lock.lock()
        .map_err(|e| format!("cannot lock the sidebar: {e}"))?;
    // The server is running: the config is never read.
    let tmux = Tmux::on_socket(socket, Path::new("/dev/null"), crate::runtime::base_env());

    let clients: Vec<(String, String)> = tmux
        .ok(&["list-clients", "-F", "#{client_pid}\t#{window_id}"])?
        .lines()
        .filter_map(|l| {
            let (pid, window) = l.split_once('\t')?;
            Some((pid.to_string(), window.to_string()))
        })
        .collect();
    let was: BTreeSet<String> = tmux
        .ok(&["show-options", "-gqv", HIDDEN])
        .unwrap_or_default()
        .split_whitespace()
        .map(String::from)
        .collect();
    // Clients that have gone forget their choice.
    let mut hidden: BTreeSet<String> = was
        .iter()
        .filter(|pid| clients.iter().any(|(p, _)| p == *pid))
        .cloned()
        .collect();
    if let Some(pid) = toggle
        && !hidden.remove(pid)
    {
        hidden.insert(pid.to_string());
    }
    if hidden != was {
        let list: Vec<&str> = hidden.iter().map(String::as_str).collect();
        tmux.ok(&["set-option", "-g", HIDDEN, &list.join(" ")])?;
    }
    let viewed: BTreeSet<String> = clients
        .iter()
        .filter(|(pid, _)| !hidden.contains(pid))
        .map(|(_, w)| w.clone())
        .collect();

    let format = format!(
        "#{{session_name}}\t#{{window_id}}\t#{{pane_id}}\t#{{{OWN_PANE}}}\t#{{{SIDEBAR}}}\t#{{pane_dead}}\t#{{pane_width}}\t#{{{STATE_HOME}}}\t#{{{CONFIG}}}\t#{{window_name}}"
    );
    let panes: Vec<Pane> = tmux
        .ok(&["list-panes", "-a", "-F", &format])?
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(10, '\t').collect();
            Some(Pane {
                session: f.first()?.to_string(),
                window: f.get(1)?.to_string(),
                id: f.get(2)?.to_string(),
                own: !f.get(3)?.is_empty(),
                sidebar: !f.get(4)?.is_empty(),
                dead: *f.get(5)? == "1",
                width: f.get(6)?.parse().ok()?,
                state_home: f.get(7)?.to_string(),
                config: f.get(8)?.to_string(),
                window_name: f.get(9)?.to_string(),
            })
        })
        .collect();

    let brg = std::env::current_exe().map_err(|e| format!("cannot find brg: {e}"))?;
    let brg = brg.display().to_string();
    for change in plan(&panes, &viewed) {
        match change {
            Change::Add { pane, width, env } => {
                let width = width.to_string();
                // `-d`: the member's pane keeps the focus.
                let mut args: Vec<String> = ["split-window", "-h", "-d", "-l", &width, "-t", &pane]
                    .map(String::from)
                    .to_vec();
                for (k, v) in env {
                    args.push("-e".into());
                    args.push(format!("{k}={v}"));
                }
                args.extend(["-P", "-F", "#{pane_id}", &brg, "status", "-w"].map(String::from));
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                let id = tmux.ok(&args)?;
                let id = id.trim();
                // An exited sidebar closes rather than stays, dead.
                tmux.ok(&[
                    "set-option",
                    "-p",
                    "-t",
                    id,
                    SIDEBAR,
                    "1",
                    ";",
                    "set-option",
                    "-p",
                    "-t",
                    id,
                    "remain-on-exit",
                    "off",
                ])?;
            }
            Change::Remove(pane) => {
                let _ = tmux.ok(&["kill-pane", "-t", &pane]);
            }
            Change::Resize { pane, width } => {
                let _ = tmux.ok(&["resize-pane", "-t", &pane, "-x", &width.to_string()]);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The member always keeps 80 columns (never the 78 where the wake's
    /// reminder ends at the box's edge), and a sidebar too narrow to read
    /// isn't added.
    #[test]
    fn width_leaves_the_member_80_columns_and_stops_at_the_minimum() {
        assert_eq!(width(200), Some(42));
        assert_eq!(width(123), Some(42));
        assert_eq!(width(120), Some(39));
        assert_eq!(width(118), Some(37));
        assert_eq!(width(105), Some(24));
        assert_eq!(width(104), None);
        assert_eq!(width(20), None);
        for room in 0..300 {
            if let Some(w) = width(room) {
                assert!(room - w > MIN_MEMBER, "{room}");
            }
        }
    }
}
