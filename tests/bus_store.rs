//! The instance's one message store and the checks made before a start.
#![cfg(feature = "test-support")]

mod common;
mod instance_env;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use instance_env::*;
use serde_json::{Value, json};

fn bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, path: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(root, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

fn publish(env: &Env, instance: &str, draft: Value) -> Value {
    let out = env.publish(instance, &draft);
    assert!(out.status.success(), "{}", both(&out));
    serde_json::from_slice::<Value>(&out.stdout).unwrap()
}

fn member_command(env: &Env, instance: &str, member: &str, args: &[&str]) -> std::process::Output {
    let start = env.starts(member).last().unwrap().clone();
    let mut command = env.cmd(args);
    for name in [
        "BORGCHART_INSTANCE",
        "BORGCHART_MEMBER",
        "BORGCHART_INCARNATION",
    ] {
        command.env(name, start["env"][name].as_str().unwrap());
    }
    command
        .env("BORGCHART_INSTANCE", instance)
        .output()
        .unwrap()
}

#[test]
fn one_store_keeps_ids_references_and_acks_across_a_restart() {
    let env = Env::new();
    let instance = env.create("oncall", "work/a");
    let lead = env.member(&instance, "lead");
    assert_eq!(
        publish(&env, &instance, json!({"to": lead, "body": "original"}))["id"],
        "m1"
    );
    assert_eq!(
        publish(&env, &instance, json!({"to": lead, "body": "second"}))["id"],
        "m2"
    );
    env.ok(&["stop", &instance]);
    env.ok(&["start", &instance]);
    env.wait_for("lead idle", || env.state_of(&instance, "lead") == "idle");
    assert_eq!(
        publish(
            &env,
            &instance,
            json!({"to": lead, "reply_to": ["m1"], "body": "reply"})
        )["id"],
        "m3"
    );
    assert!(
        env.starts(&lead)
            .iter()
            .all(|s| s["env"].get("BORGCHART_RUN").is_none())
    );
    let out = member_command(&env, &instance, &lead, &["msg", "ack", "m1", "--json"]);
    assert!(out.status.success(), "{}", both(&out));
    let original: Value = serde_json::from_slice(
        &env.ok(&["msg", "read", "m1", "--instance", &instance, "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(original["record"]["body"], "original");
    let store = env.path(&format!("state/{instance}/messages"));
    let identity: Value =
        serde_json::from_slice(&fs::read(store.join("store.json")).unwrap()).unwrap();
    assert_eq!(identity, json!({"format": 2, "instance": instance}));
    let record: Value =
        serde_json::from_slice(&fs::read(store.join("records/0000000001.json")).unwrap()).unwrap();
    let ack: Value =
        serde_json::from_slice(&fs::read(store.join("acks/m1.json")).unwrap()).unwrap();
    assert_eq!(record["schema"], 2);
    assert_eq!(ack["schema"], 2);
    assert!(record.get("run").is_none() && ack.get("run").is_none());
    assert!(
        env.starts(&lead)
            .iter()
            .all(|s| s["env"].get("BORGCHART_RUN").is_none())
    );
    assert!(!store.join("current").exists());
    env.ok(&["stop", &instance]);
}

#[test]
fn changed_roles_keep_the_store_and_replies_to_removed_members() {
    let env = Env::new();
    let instance = env.create("oncall", "work/a");
    env.ok(&["create", &format!("{instance}/helper")]);
    env.ok(&["create", &format!("{instance}/scout")]);
    let lead = env.member(&instance, "lead");
    let helper = env.member(&instance, "helper");
    let scout = env.member(&instance, "scout");
    publish(&env, &instance, json!({"to": scout, "body": "keep this"}));
    env.ok(&["stop", &instance]);
    env.write(
        "teams/oncall/team.yaml",
        &TEAM
            .replace("    parent: lead\n", "")
            .replace("  scout:\n    prompt: scout.md\n    start: false\n", ""),
    );
    env.write(
        "teams/oncall/lead.md",
        "---\nrole: lead\nmodels: {claude: opus high}\n---\nThe lead, edited.\n",
    );
    env.ok(&["start", &instance]);
    env.wait_for("helper idle", || {
        env.state_of(&instance, "helper") == "idle"
    });
    let record = publish(
        &env,
        &instance,
        json!({"to": lead, "reply_to": ["m1"], "body": "reply after role removal"}),
    );
    assert_eq!(record["id"], "m2");
    env.write(
        "root-note.json",
        &json!({"to": "external:ops", "body": "new root"}).to_string(),
    );
    let out = member_command(
        &env,
        &instance,
        &helper,
        &["msg", "publish", "--record", "root-note.json", "--json"],
    );
    assert!(out.status.success(), "{}", both(&out));
    let record: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(record["id"], "m3");
    let old: Value = serde_json::from_slice(
        &env.ok(&["msg", "read", "m1", "--instance", &instance, "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(old["record"]["to"], scout);
    env.ok(&["stop", &instance]);
}

#[test]
fn a_stopped_member_start_with_changed_files_writes_nothing() {
    let env = Env::new();
    let instance = env.create("oncall", "work/a");
    let lead = env.member(&instance, "lead");
    env.ok(&["stop", &instance]);
    env.write(
        "teams/oncall/lead.md",
        "---\nrole: lead\nmodels: {claude: opus high}\n---\nThe lead, edited.\n",
    );
    let state = env.path(&format!("state/{instance}"));
    let before = bytes(&state);
    let err = env.fails(&["start", &format!("{instance}/{lead}")]);
    assert!(
        err.contains("changed since") && err.contains("brg stop"),
        "{err}"
    );
    assert!(
        bytes(&state) == before,
        "refused start changed instance state"
    );
}

#[test]
fn changed_files_require_every_previous_member_positively_stopped() {
    let env = Env::new();
    let instance = env.create("oncall", "work/a");
    env.ok(&["create", &format!("{instance}/helper")]);
    let helper = env.member(&instance, "helper");
    env.ok(&["stop", &instance]);
    let state = env.path(&format!("state/{instance}"));
    let current = bytes(&state)
        .into_iter()
        .find_map(|(path, data)| {
            let name = path.file_name()?.to_str()?;
            if name != "incarnation.json" && name != format!("{helper}.json") {
                return None;
            }
            let record: Value = serde_json::from_slice(&data).ok()?;
            (record["member"] == helper && record.get("incarnation").is_some())
                .then(|| state.join(path))
        })
        .unwrap();
    fs::write(current, json!({"member": helper, "incarnation": "helper.20261105000000000.abcd", "started_at": "2026-11-05T00:00:00Z"}).to_string()).unwrap();
    env.write(
        "teams/oncall/team.yaml",
        &TEAM.replace("    parent: lead\n", "").replace(
            "  scout:\n    prompt: scout.md\n    start: false",
            "  scout:\n    prompt: scout.md\n    start: true",
        ),
    );
    let before = bytes(&state);
    let err = env.fails(&["start", &instance]);
    assert!(err.contains(&helper) && err.contains("can't tell"), "{err}");
    assert!(
        bytes(&state) == before,
        "refused start changed instance state"
    );
}

fn damaged_store(case: &str) {
    let env = Env::new();
    let instance = env.create("oncall", "work/a");
    let lead = env.member(&instance, "lead");
    env.ok(&["stop", &instance]);
    let store = env.path(&format!("state/{instance}/messages"));
    fs::remove_dir_all(&store).unwrap();
    fs::create_dir(&store).unwrap();
    fs::set_permissions(&store, fs::Permissions::from_mode(0o700)).unwrap();
    match case {
        "foreign" => fs::write(
            store.join("store.json"),
            json!({"format": 2, "instance": "other-2345"}).to_string(),
        )
        .unwrap(),
        "format" => fs::write(
            store.join("store.json"),
            json!({"format": 1, "instance": instance}).to_string(),
        )
        .unwrap(),
        "current" => fs::write(store.join("current"), "r1\n").unwrap(),
        "directory" => fs::create_dir(store.join("r1")).unwrap(),
        "records" => fs::create_dir(store.join("records")).unwrap(),
        _ => unreachable!(),
    }
    let before = bytes(&store);
    let err = env.fails(&["start", &instance]);
    assert!(err.contains("damaged"), "{err}");
    let check = env.brg(&["bus", "check", "--instance", &instance, "--json"]);
    assert!(!check.status.success(), "{}", both(&check));
    let out = env.publish(&instance, &json!({"to": lead, "body": "refuse me"}));
    assert!(!out.status.success(), "{}", both(&out));
    assert!(bytes(&store) == before, "damaged store changed");
}

#[test]
fn a_store_for_another_instance_is_refused_without_writes() {
    damaged_store("foreign");
}
#[test]
fn another_store_format_is_refused_without_writes() {
    damaged_store("format");
}
#[test]
fn a_current_file_is_refused_without_writes() {
    damaged_store("current");
}
#[test]
fn a_generation_directory_is_refused_without_writes() {
    damaged_store("directory");
}
#[test]
fn records_without_store_identity_are_refused_without_writes() {
    damaged_store("records");
}
