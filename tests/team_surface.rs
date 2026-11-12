//! Running-team vocabulary through launches, commands and persisted state.
#![cfg(feature = "test-support")]

mod common;
mod instance_env;

use std::collections::BTreeMap;
use std::fs;

use instance_env::*;
use serde_json::Value;

#[test]
fn member_team_environment_selects_messages_without_the_retired_variable_or_flag() {
    let env = Env::new();
    let team = env.create("oncall", "work/a");
    let member = env.member(&team, "lead");
    let start = &env.starts(&member)[0];
    assert!(start["env"].get("BORGCHART_INSTANCE").is_none());
    let member_env: BTreeMap<String, String> =
        serde_json::from_value(start["env"].clone()).unwrap();
    let out = common::command(env!("CARGO_BIN_EXE_brg"))
        .env_clear()
        .envs(member_env)
        .args(["msg", "list", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    let out = env.brg(&["msg", "--instance", &team, "list"]);
    assert_eq!(out.status.code(), Some(2), "{}", both(&out));
    env.ok(&["stop", &team]);
}

#[test]
fn team_status_catalog_and_records_expose_the_new_machine_names() {
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
        assert!(row.get("team").is_some() && row.get("instance").is_none());
    }
    let catalog: Value = serde_json::from_slice(&env.ok(&["templates", "--json"]).stdout).unwrap();
    let oncall = catalog
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["template"] == "oncall")
        .unwrap();
    assert!(oncall.get("instances").is_none());
    assert_eq!(oncall["teams"][0]["team"], team);
    assert!(oncall["teams"][0].get("instance").is_none());
    let registry: Value =
        serde_json::from_slice(&fs::read(env.path("state/registry.json")).unwrap()).unwrap();
    assert!(registry["teams"].get(&team).is_some());
    assert!(registry.get("instances").is_none());
    let record_path = env.path(&format!("state/{team}/team.json"));
    let record: Value = serde_json::from_slice(&fs::read(record_path).unwrap()).unwrap();
    assert!(
        record["child_teams"]
            .get(child.rsplit('/').next().unwrap())
            .is_some()
    );
    assert!(record.get("copies").is_none());
    assert!(!env.path(&format!("state/{team}/instance.json")).exists());
    env.ok(&["stop", &team]);
}

#[test]
fn schema_one_registry_is_refused_before_message_storage_is_opened() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let store = state.join("sample-2222/messages/store.json");
    fs::create_dir_all(store.parent().unwrap()).unwrap();
    fs::write(&store, b"retired store metadata").unwrap();
    fs::write(
        state.join("registry.json"),
        br#"{"schema":1,"instances":{},"issued":{}}"#,
    )
    .unwrap();
    let out = common::command(env!("CARGO_BIN_EXE_brg"))
        .env("BORGCHART_STATE_HOME", &state)
        .env("BORGCHART_CONFIG", root.path().join("missing.yaml"))
        .args(["msg", "--team", "sample-2222", "list"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        both(&out).contains("written by an older brg"),
        "{}",
        both(&out)
    );
    assert_eq!(fs::read(&store).unwrap(), b"retired store metadata");
}
