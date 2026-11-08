//! `brg … --help` is how agents in a team learn the bus, so it must not
//! drift from the code: every record kind and draft field
//! the validator knows appears in `msg publish --help`.

use std::process::Command;

use borgchart::bus::record::Draft;

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
fn publish_help_names_every_draft_field() {
    let text = help(&["msg", "publish"]);
    let lines: Vec<&str> = text.lines().collect();
    let starts = |line: &str, name: &str| line.trim_start().starts_with(&format!("{name} "));
    let fields = serde_names(
        serde_json::from_str::<Draft>(r#"{"no_such_field": 1}"#)
            .unwrap_err()
            .to_string(),
    );
    assert_eq!(fields, ["to", "body", "reply_to"]);
    for field in &fields {
        assert!(
            lines.iter().any(|l| starts(l, field)),
            "draft field `{field}` missing from msg publish --help"
        );
    }
}
