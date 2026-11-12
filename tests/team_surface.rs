//! Running teams through launches, commands and persisted state.
#![cfg(feature = "test-support")]

mod common;
mod instance_env;

use std::collections::BTreeMap;
use std::fs;

use instance_env::*;
use serde_json::Value;

#[test]
fn member_team_environment_selects_messages() {
    let env = Env::new();
    let team = env.create("oncall", "work/a");
    let member = env.member(&team, "lead");
    let start = &env.starts(&member)[0];
    let member_env: BTreeMap<String, String> =
        serde_json::from_value(start["env"].clone()).unwrap();
    let out = common::command(env!("CARGO_BIN_EXE_brg"))
        .env_clear()
        .envs(member_env)
        .args(["msg", "list", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    env.ok(&["stop", &team]);
}

#[test]
fn team_status_catalog_and_records_expose_running_teams() {
    let env = Env::new();
    env.team("templates/workers");
    env.write(
        "templates/oncall/template.yaml",
        &format!("{TEAM}  dev: {{template: workers, parent: lead, start: false}}\n"),
    );
    let team = env.create("oncall", "work/a");
    let child_out = env.ok(&["create", &format!("{team}/dev")]);
    let child = stdout(&child_out)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    let rows = env.rows(&team);
    let child_row = rows
        .iter()
        .find(|row| row["kind"] == "team")
        .expect("child team row");
    assert_eq!(child_row["team"], child);
    assert_eq!(child_row["template"], "workers");
    for row in &rows {
        assert!(row.get("team").is_some());
    }
    let catalog: Value = serde_json::from_slice(&env.ok(&["templates", "--json"]).stdout).unwrap();
    let oncall = catalog
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["template"] == "oncall")
        .unwrap();
    assert_eq!(oncall["teams"][0]["team"], team);
    let registry: Value =
        serde_json::from_slice(&fs::read(env.path("state/registry.json")).unwrap()).unwrap();
    assert!(registry["teams"].get(&team).is_some());
    let record_path = env.path(&format!("state/{team}/team.json"));
    let record: Value = serde_json::from_slice(&fs::read(record_path).unwrap()).unwrap();
    assert!(
        record["child_teams"]
            .get(child.rsplit('/').next().unwrap())
            .is_some()
    );
    env.ok(&["stop", &team]);
}
