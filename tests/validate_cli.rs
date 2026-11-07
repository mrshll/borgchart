//! `brg validate` and `borgchart validate` as programs: JSON output and exit
//! codes. They run the binaries, so they are not in the fast tier.

mod common;
mod team_fixture;

use team_fixture::Fixture;

#[test]
fn cli_reports_json_and_exit_codes() {
    let f = Fixture::new();
    let ok = common::command(env!("CARGO_BIN_EXE_brg"))
        .args(["validate", "--json", "--file"])
        .arg(f.path("borgchart.yaml"))
        .output()
        .unwrap();
    assert!(ok.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&ok.stdout).unwrap();
    assert_eq!(parsed["ok"], true);

    f.edit("team.yaml", "parent: lead", "parent: boss");
    let bad = common::command(env!("CARGO_BIN_EXE_borgchart"))
        .args(["validate", "--file"])
        .arg(f.path("borgchart.yaml"))
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("parent `boss`"));

    let empty = tempfile::tempdir().unwrap();
    let usage = common::command(env!("CARGO_BIN_EXE_brg"))
        .arg("validate")
        .current_dir(empty.path())
        .output()
        .unwrap();
    assert_eq!(usage.status.code(), Some(2));
}
