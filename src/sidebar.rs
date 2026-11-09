//! The status sidebar: a pane beside each member window someone views,
//! running `brg status -w` for every instance.
//!
//! tmux runs `brg _sidebar <socket>` from hooks in brg's config whenever a
//! client attaches, detaches, resizes, or moves to another session or
//! window, and `prefix S` runs it with `--toggle <client pid>`. Each run
//! makes the whole server match what its clients view: one sidebar in each
//! member window a client views (unless every viewer hid it, or the window
//! is too narrow), and none anywhere else. Runs take a lock in brg's state
//! home, so two at once can't both add one.

use std::collections::BTreeSet;
use std::path::Path;

use crate::tmux::{OWN_PANE, Tmux, own_pane};

/// The pane option marking a sidebar pane.
pub const SIDEBAR: &str = "@brg-sidebar";

/// The server option listing the clients (by process ID) that hid the
/// sidebar with `prefix S`.
pub const HIDDEN: &str = "@brg-sidebar-hidden";

/// The sidebar's width.
pub const WIDTH: u32 = 42;

/// The narrowest window that gets a sidebar.
pub const MIN_WINDOW: u32 = 80;

/// The sidebar's width in a window `window` columns wide: [`WIDTH`], or a
/// third of a window narrower than three times that, and none below
/// [`MIN_WINDOW`], where the member would be left too little room.
pub fn width(window: u32) -> Option<u32> {
    (window >= MIN_WINDOW).then(|| WIDTH.min(window / 3))
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
    pub window_width: u32,
    pub width: u32,
}

/// What to change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Split a sidebar this wide off the member's pane.
    Add {
        pane: String,
        width: u32,
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
        let others: Vec<&Pane> = in_window.iter().copied().filter(|p| !p.sidebar).collect();
        let marked: Vec<bool> = others.iter().map(|p| p.own).collect();
        let member = own_pane(&marked).map(|i| others[i]);
        let want = match member {
            Some(m)
                if first.session.starts_with("brg-")
                    && !first.window_name.starts_with('_')
                    && !m.dead
                    && viewed.contains(window) =>
            {
                width(first.window_width).map(|w| (m, w))
            }
            _ => None,
        };
        let mut sidebars = in_window.iter().filter(|p| p.sidebar);
        match want {
            Some((m, w)) => {
                let keep = in_window.iter().find(|p| p.sidebar && !p.dead);
                match keep {
                    Some(k) if k.width != w => changes.push(Change::Resize {
                        pane: k.id.clone(),
                        width: w,
                    }),
                    Some(_) => {}
                    None => changes.push(Change::Add {
                        pane: m.id.clone(),
                        width: w,
                    }),
                }
                for p in sidebars.filter(|p| Some(&p.id) != keep.map(|k| &k.id)) {
                    changes.push(Change::Remove(p.id.clone()));
                }
            }
            None => {
                for p in sidebars.by_ref() {
                    changes.push(Change::Remove(p.id.clone()));
                }
            }
        }
    }
    changes
}

/// `brg _sidebar <socket> [--toggle <client pid>]`.
pub fn run(socket: &str, toggle: Option<&str>) -> Result<(), String> {
    let lock = crate::state::open_lock(&crate::state::state_home().join("sidebar.lock"))
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
        "#{{session_name}}\t#{{window_id}}\t#{{pane_id}}\t#{{{OWN_PANE}}}\t#{{{SIDEBAR}}}\t#{{pane_dead}}\t#{{window_width}}\t#{{pane_width}}\t#{{window_name}}"
    );
    let panes: Vec<Pane> = tmux
        .ok(&["list-panes", "-a", "-F", &format])?
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(9, '\t').collect();
            Some(Pane {
                session: f.first()?.to_string(),
                window: f.get(1)?.to_string(),
                id: f.get(2)?.to_string(),
                own: !f.get(3)?.is_empty(),
                sidebar: !f.get(4)?.is_empty(),
                dead: *f.get(5)? == "1",
                window_width: f.get(6)?.parse().ok()?,
                width: f.get(7)?.parse().ok()?,
                window_name: f.get(8)?.to_string(),
            })
        })
        .collect();

    let brg = std::env::current_exe().map_err(|e| format!("cannot find brg: {e}"))?;
    let brg = brg.display().to_string();
    for change in plan(&panes, &viewed) {
        match change {
            Change::Add { pane, width } => {
                let width = width.to_string();
                // `-d`: the member's pane keeps the focus.
                let id = tmux.ok(&[
                    "split-window",
                    "-h",
                    "-d",
                    "-l",
                    &width,
                    "-t",
                    &pane,
                    "-P",
                    "-F",
                    "#{pane_id}",
                    &brg,
                    "status",
                    "-w",
                ])?;
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

    /// The member keeps most of a small window, and a tiny one gets none.
    #[test]
    fn width_shrinks_with_the_window_and_stops_at_the_minimum() {
        assert_eq!(width(200), Some(42));
        assert_eq!(width(126), Some(42));
        assert_eq!(width(90), Some(30));
        assert_eq!(width(80), Some(26));
        assert_eq!(width(79), None);
    }
}
