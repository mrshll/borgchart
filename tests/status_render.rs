//! Terminal rendering boundaries, using small fictional status trees.

use borgchart::runtime::{self, StatusRow};

fn member(name: &str, parent: Option<&str>) -> StatusRow {
    StatusRow {
        name: name.into(),
        kind: "member",
        instance: "harbor-k3f9".into(),
        team: "harbor".into(),
        member: Some(name.into()),
        start_id: None,
        role: None,
        vendor: Some("codex".into()),
        model: Some("example-model".into()),
        parent: parent.map(String::from),
        posture: Some("vendor".into()),
        state: "working".into(),
        rule: None,
        activity: None,
        detail: None,
        vendor_version: None,
        trust_accepted: None,
        worktree: None,
        liveness: None,
        unread: None,
        running: None,
        expected_running: false,
        start: true,
    }
}

#[test]
fn nested_plugin_errors_highlight_the_state_instead_of_the_detail() {
    let rows = [
        member("lead", None),
        StatusRow {
            kind: "plugin",
            vendor: None,
            model: None,
            state: "alive".into(),
            detail: Some("error: cannot reach the feed".into()),
            ..member("feed", Some("lead"))
        },
    ];
    let text = runtime::render_status_as("harbor", &rows, true, true, None);
    let plugin = text
        .lines()
        .find(|line| line.contains("plugin feed"))
        .unwrap();
    assert!(plugin.contains("\x1b[7m alive \x1b[0m"), "{plugin:?}");
    assert!(!plugin.contains("\x1b[7m error: cannot reach the feed \x1b[0m"));
}

#[test]
fn team_diamonds_identify_running_copies_without_hiding_copy_failures() {
    let mut rows = [
        member("lead", None),
        StatusRow {
            kind: "copy",
            instance: "harbor-k3f9/design-j5v3".into(),
            vendor: None,
            model: None,
            state: "running".into(),
            running: Some(2),
            ..member("design-j5v3", Some("harbor-k3f9/lead"))
        },
    ];
    let rendered = runtime::render_status_as("harbor", &rows, true, true, None);
    assert!(rendered.lines().any(|line| line.contains("◆ design-j5v3")));
    for (state, running) in [("stopped", 0), ("stopping", 2), ("unreadable", 2)] {
        rows[1].state = state.into();
        rows[1].running = Some(running);
        let rendered = runtime::render_status_as("harbor", &rows, true, true, None);
        let copy = rendered
            .lines()
            .find(|line| line.contains("design-j5v3"))
            .unwrap();
        assert!(!copy.contains('◆'), "{copy}");
        if state != "stopped" {
            assert!(copy.contains(state), "{copy}");
        }
    }
}

#[test]
fn terminal_states_stay_visible_at_80_columns_with_long_models() {
    let rows = [
        member("lead", None),
        StatusRow {
            state: "blocked".into(),
            model: Some("example-model-with-a-very-long-declared-name-and-extra-effort".into()),
            ..member("reviewer", Some("lead"))
        },
    ];
    let escapes = regex::Regex::new("\x1b\\[[0-9;]*m").unwrap();
    for colour in [true, false] {
        let terminal = runtime::render_status_as("harbor", &rows, true, colour, None);
        let plain = escapes.replace_all(&terminal, "");
        let reviewer: String = plain
            .lines()
            .find(|line| line.contains("reviewer"))
            .unwrap()
            .chars()
            .take(80)
            .collect();
        assert!(reviewer.contains("blocked"), "{reviewer}");
        if !colour {
            assert!(!terminal.contains('\x1b'));
        }
    }
    let piped = runtime::render_status_as("harbor", &rows, false, false, None);
    let reviewer = piped
        .lines()
        .find(|line| line.contains("reviewer"))
        .unwrap();
    assert!(reviewer.find("example-model").unwrap() < reviewer.find("blocked").unwrap());
    assert!(!piped.contains('\x1b'));
}
