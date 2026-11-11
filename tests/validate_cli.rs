//! `brg validate` and `borgchart validate` as programs: JSON output and exit
//! codes. They run the binaries, so they are not in the fast tier.

mod common;
mod team_fixture;

use team_fixture::Fixture;

fn included_team() -> Fixture {
    let f = Fixture::new();
    f.write(
        "config.yaml",
        "schema: 1\nteams: [./teams]\ntmux: {user_config: ./tmux.conf}\n",
    );
    f.write(
        "tmux.conf",
        "set-hook -g client-attached 'display-message hello'\n",
    );
    f.write(
        "teams/dev-team/team.yaml",
        "roles:\n  lead: {prompt: lead.md}\n  dev: {team: base-team, parent: lead}\n",
    );
    f.write(
        "teams/dev-team/lead.md",
        "---\nmodels: {claude: opus}\n---\n{{includes/before}}\n{{teams/base-team/helper}}\n{{includes/after}}\n",
    );
    f.write("teams/dev-team/includes/before.md", "Before.\n");
    f.write("teams/dev-team/includes/after.md", "After.\n");
    f.write(
        "teams/base-team/team.yaml",
        "includes: catalog:shared/includes\nroles: {helper: {prompt: helper.md}}\n",
    );
    f.write(
        "teams/base-team/helper.md",
        "---\nmodels: {claude: sonnet}\n---\n{{includes/common}}\n",
    );
    f.write("teams/shared/includes/common.md", "Common.\n");
    f
}

fn validate_in_home(f: &Fixture, json: bool) -> std::process::Output {
    let mut cmd = common::command(env!("CARGO_BIN_EXE_brg"));
    cmd.args(["validate", "dev-team"])
        .env("HOME", f.path(""))
        .env("BORGCHART_CONFIG", f.path("config.yaml"))
        .env("BORGCHART_STATE_HOME", f.path("state"))
        .env_remove("NO_COLOR");
    if json {
        cmd.arg("--json");
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

#[test]
fn nested_teams_do_not_repeat_config_warnings_or_inflate_the_count() {
    let f = included_team();
    let out = validate_in_home(&f, false);
    let stderr = String::from_utf8(out.stderr).unwrap();
    let warnings = stderr
        .lines()
        .filter(|line| line.starts_with("warning:"))
        .count();
    assert_eq!(warnings, 1, "{stderr}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains(&format!(", {warnings} warning(s)")),
        "{stdout}"
    );
}

#[test]
fn piped_sources_show_the_includer_hierarchy_and_home_paths_without_ansi() {
    let f = included_team();
    let out = validate_in_home(&f, false);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains(concat!(
            "\n\nRoles:\n",
            "  lead\n",
            "    ~/teams/dev-team/lead.md\n",
            "      ~/teams/dev-team/includes/before.md\n",
            "      ~/teams/base-team/helper.md\n",
            "        ~/teams/shared/includes/common.md\n",
            "      ~/teams/dev-team/includes/after.md\n",
        )),
        "{stdout}"
    );
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("~/config.yaml"), "{stderr}");
    assert!(
        stderr.ends_with("\n\n"),
        "warnings need separation: {stderr:?}"
    );
    assert!(!stdout.contains('\x1b') && !stderr.contains('\x1b'));
}

#[test]
fn json_sources_stay_flat_and_absolute_without_new_fields() {
    let f = included_team();
    let out = validate_in_home(&f, true);
    assert!(out.stderr.is_empty());
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    // Diagnostic messages can evolve; their public JSON fields cannot.
    for warning in parsed["warnings"].as_array().unwrap() {
        assert_eq!(
            warning,
            &serde_json::json!({
                "level": "warning",
                "file": f.path("config.yaml"),
                "message": warning["message"].as_str().unwrap(),
            })
        );
    }
    assert_eq!(
        parsed,
        serde_json::json!({
            "ok": true,
            "errors": [],
            "warnings": parsed["warnings"],
            "roles": {"lead": {"sources": [
                f.path("teams/dev-team/lead.md"),
                f.path("teams/dev-team/includes/before.md"),
                f.path("teams/base-team/helper.md"),
                f.path("teams/shared/includes/common.md"),
                f.path("teams/dev-team/includes/after.md"),
            ]}},
        })
    );
}

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
            .env("HOME", f.path("home"))
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
    let listed = format!("    {}", lead.display());
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
