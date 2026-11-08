//! `brg … --help` is how agents in a team learn the bus, so it must not
//! drift from the code: every record kind and draft field
//! the validator knows appears in `msg publish --help`.

use std::process::Command;

use borgchart::bus::record::{Draft, Kind};

fn help(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_brg"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .args(args)
        .arg("--help")
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}: {:?}", out);
    String::from_utf8(out.stdout).unwrap()
}

/// The names serde lists when it refuses an unknown one: every variant of
/// an enum, or every field of a struct, straight from the derive.
fn serde_names(err: String) -> Vec<String> {
    let (_, list) = err
        .split_once("expected ")
        .unwrap_or_else(|| panic!("no list in: {err}"));
    // The names are the backquoted words: `a`, `b`, … `z`
    list.split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

#[test]
fn publish_help_names_every_kind_and_field() {
    let text = help(&["msg", "publish"]);
    let kinds = serde_names(
        serde_json::from_str::<Kind>("\"no-such-kind\"")
            .unwrap_err()
            .to_string(),
    );
    assert_eq!(kinds, ["note"]);
    let lines: Vec<&str> = text.lines().collect();
    let starts = |l: &str, name: &str| l.trim_start().starts_with(&format!("{name} "));
    for name in &kinds {
        lines
            .iter()
            .position(|l| starts(l, name))
            .unwrap_or_else(|| panic!("kind `{name}` missing from msg publish --help"));
    }
    let fields = serde_names(
        serde_json::from_str::<Draft>(r#"{"no_such_field": 1}"#)
            .unwrap_err()
            .to_string(),
    );
    assert!(fields.contains(&"facts".to_string()), "{fields:?}");
    for field in &fields {
        assert!(
            lines.iter().any(|l| starts(l, field)),
            "draft field `{field}` missing from msg publish --help"
        );
    }
}

#[test]
fn bus_help_speaks_of_teams_and_members() {
    for args in [
        &["msg", "publish"][..],
        &["msg", "read"],
        &["msg", "ack"],
        &["inbox"],
        &["inbox", "wait"],
    ] {
        let text = help(args);
        assert!(
            !borgchart::bus::mentions_runs(&text),
            "{args:?} --help mentions runs:\n{text}"
        );
    }
}
