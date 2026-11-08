//! `brg validate` and `borgchart validate` as programs: JSON output and exit
//! codes. They run the binaries, so they are not in the fast tier.

mod common;
mod team_fixture;

use team_fixture::Fixture;

#[test]
fn roles_load_but_legacy_members_and_frontmatter_role_are_rejected() {
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
    f.edit("team.yaml", "roles:", "members:");
    let out = validate();
    assert!(!out.status.success());
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(error.contains("unknown field `members`"), "{error}");
    f.edit("team.yaml", "members:", "roles:");
    f.edit("lead.md", "models:", "role: lead\nmodels:");
    let out = validate();
    assert!(!out.status.success());
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(error.contains("unknown field `role`"), "{error}");
    f.edit("lead.md", "role: lead\n", "");
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
