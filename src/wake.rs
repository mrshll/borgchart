//! Pane snapshots, submission checks, and vendor-version verification for message delivery.

use crate::composer::{self, Geometry};
use crate::lifecycle::PaneIdentity;
use crate::team::Vendor;
use crate::tmux::{self, Tmux};

/// Recognized by the submission check when this payload remains in the composer.
pub const REMINDER: &str =
    "brg: new records in your inbox. Run `brg msg list` at your next stopping point.";

/// Whether the measured CLI version is in the shared verified-version list.
pub fn verified(
    rules: &crate::watcher::methods::WakeRules,
    vendor: Vendor,
    version: Option<&str>,
) -> bool {
    let version = version.map(|v| match vendor {
        Vendor::Claude => v.strip_suffix(" (Claude Code)").unwrap_or(v),
        Vendor::Codex => v.strip_prefix("codex-cli ").unwrap_or(v),
    });
    rules.find(vendor, version).is_some()
}

/// One snapshot of the pane: title, styled screen and geometry.
pub struct Snapshot {
    pub title: String,
    pub screen: String,
    pub geometry: Geometry,
    /// tmux copy mode (or another mode) is showing over the program.
    pub in_mode: bool,
}

/// Read the pane if it is still the incarnation's own.
pub fn snapshot(tmux: &Tmux, expect: &PaneIdentity) -> Option<Snapshot> {
    let format = "#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{session_name}\t#{window_name}\t#{pane_in_mode}\t#{pane_width}\t#{cursor_x}\t#{cursor_y}\t#{pane_title}";
    let out = tmux
        .ok(&["display-message", "-p", "-t", &expect.pane_id, format])
        .ok()?;
    let line = out.lines().next()?;
    let f: Vec<&str> = line.splitn(10, '\t').collect();
    if f.len() < 10
        || f[0] != expect.pane_id
        || f[1] != expect.pane_pid.to_string()
        || f[2] != "0"
        || f[3] != expect.session
        || f[4] != expect.window
    {
        return None;
    }
    let geometry = Geometry {
        width: f[6].parse().ok()?,
        cursor_x: f[7].parse().ok()?,
        cursor_y: f[8].parse().ok()?,
    };
    let screen = tmux
        .ok(&["capture-pane", "-p", "-e", "-t", &expect.pane_id])
        .ok()?;
    Some(Snapshot {
        title: f[9].to_string(),
        screen,
        geometry,
        in_mode: tmux::in_mode(f[5]),
    })
}

/// After Enter: `Ok` once the input box is empty or shows Claude's queued-message
/// placeholder (a turn may be running); otherwise why the outcome isn't known.
pub fn after_enter(snap: Option<&Snapshot>) -> Result<(), String> {
    let snap = snap.ok_or("the pane changed or exited after Enter")?;
    if snap.in_mode {
        return Err(format!("{} after Enter", tmux::IN_MODE));
    }
    let view = composer::view(&snap.screen, Some(snap.geometry))
        .map_err(|why| format!("the input box can't be read after Enter ({why})"))?;
    if view.lines.iter().all(|l| l.trim().is_empty()) || view.shows_queued_placeholder() {
        Ok(())
    } else if view.holds(REMINDER) {
        Err("Enter was pressed but the reminder is still in the input box".into())
    } else {
        Err("the input box isn't empty after Enter".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_queued_placeholder_confirms_submission_without_accepting_a_draft() {
        let screen =
            include_str!("../tests/fixtures/screens/claude-queued-placeholder-assembled.txt");
        let placeholder = "Press up to edit queued messages";
        let snapshot = |screen: String| Snapshot {
            title: "✳ Example".into(),
            screen,
            geometry: composer::Geometry {
                width: 80,
                cursor_x: 2,
                cursor_y: 35,
            },
            in_mode: false,
        };
        for screen in [
            screen.to_string(),
            screen.replace(placeholder, &format!("\x1b[2m{placeholder}\x1b[0m")),
        ] {
            assert_eq!(after_enter(Some(&snapshot(screen))), Ok(()));
        }
        for draft in [
            "a draft",
            "Press up to edit queued messages extra",
            "Press up to edit queued messages\n  more text",
        ] {
            assert!(after_enter(Some(&snapshot(screen.replace(placeholder, draft)))).is_err());
        }
    }
}
