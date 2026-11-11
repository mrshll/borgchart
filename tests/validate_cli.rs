//! `brg validate` and `borgchart validate` as programs: JSON output and exit
//! codes. They run the binaries, so they are not in the fast tier.

mod common;
mod team_fixture;

use team_fixture::Fixture;

#[test]
fn roles_load_and_a_role_may_not_hide_another_roles_member() {
    let f = Fixture::new();
    f.write("team.yaml", "roles: {lead: {prompt: lead.md}}\n");
    f.write("lead.md", "---\nmodels: {claude: opus}\n---\nLead.\n");
    let validate = || {
        common::command(env!("CARGO_BIN_EXE_brg"))
            .arg("validate")
            .arg(f.dir.path())
            .env("BORGCHART_CONFIG", f.path("config.yaml"))
            .output()
            .unwrap()
    };
    let out = validate();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Each role's source files follow the summary.
    let lead = f.dir.path().canonicalize().unwrap().join("lead.md");
    let listed = format!("  lead: {}", lead.display());
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|l| l == listed),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    f.write(
        "team.yaml",
        "roles: {lead: {prompt: lead.md}, lead-k3f9: {prompt: lead.md}}\n",
    );
    assert!(
        !validate().status.success(),
        "a role must not hide another role's member"
    );
}

#[test]
fn cli_reports_json_and_exit_codes() {
    let f = Fixture::new();
    let ok = common::command(env!("CARGO_BIN_EXE_brg"))
        .args(["validate", "--json"])
        .arg(f.dir.path())
        .env("BORGCHART_CONFIG", f.path("config.yaml"))
        .output()
        .unwrap();
    assert!(ok.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&ok.stdout).unwrap();
    assert_eq!(parsed["ok"], true);
    let sources = parsed["roles"]["triager"]["sources"].as_array().unwrap();
    assert!(sources[0].as_str().unwrap().ends_with("triager.md"));
    assert!(
        sources.len() > 1,
        "its include is listed after it: {sources:?}"
    );

    f.edit("team.yaml", "parent: lead", "parent: boss");
    let bad = common::command(env!("CARGO_BIN_EXE_borgchart"))
        .arg("validate")
        .arg(f.dir.path())
        .env("BORGCHART_CONFIG", f.path("config.yaml"))
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
