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
fn a_store_containing_only_empty_codex_masks_refuses_start() {
    let env = Env::new();
    let instance = env.create("oncall", "work/a");
    env.ok(&["stop", &instance]);
    let store = env.path(&format!("state/{instance}/messages"));
    fs::remove_dir_all(&store).unwrap();
    fs::create_dir(&store).unwrap();
    fs::set_permissions(&store, fs::Permissions::from_mode(0o700)).unwrap();
    for name in [".agents", ".aws", ".codex", ".git"] {
        fs::create_dir(store.join(name)).unwrap();
    }
    let error = env.fails(&["start", &instance]);
    assert!(error.contains("unexpected entry"), "{error}");
    assert!(!store.join("store.json").exists());
}

#[test]
fn unexpected_entries_are_not_accepted_as_codex_masks() {
    for case in [
        "empty-agents",
        "empty-aws",
        "empty-codex",
        "empty-git",
        "nonempty",
        "file",
        "symlink",
        "unknown",
    ] {
        let env = Env::new();
        let instance = env.create("oncall", "work/a");
        let lead = env.member(&instance, "lead");
        let message = publish(
            &env,
            &instance,
            json!({"to": lead, "body": "Keep this message."}),
        );
        let store = env.path(&format!("state/{instance}/messages"));
        match case {
            case if case.starts_with("empty-") => {
                fs::create_dir(store.join(format!(".{}", &case[6..]))).unwrap();
            }
            "nonempty" => {
                fs::create_dir(store.join(".git")).unwrap();
                fs::write(store.join(".git/contents"), "keep this file").unwrap();
            }
            "file" => fs::write(store.join(".codex"), "").unwrap(),
            "symlink" => {
                let target = env.path("empty-mask-target");
                fs::create_dir(&target).unwrap();
                std::os::unix::fs::symlink(target, store.join(".aws")).unwrap();
            }
            "unknown" => fs::create_dir(store.join(".sandbox")).unwrap(),
            _ => unreachable!(),
        }
        let inbox = member_command(&env, &instance, &lead, &["msg", "list", "--json"]);
        let publish = env.publish(&instance, &json!({"to": lead, "body": "Do not publish."}));
        let ack = member_command(
            &env,
            &instance,
            &lead,
            &["msg", "ack", message["id"].as_str().unwrap(), "--json"],
        );
        for (out, code) in [(inbox, 7), (publish, 5), (ack, 7)] {
            assert_eq!(out.status.code(), Some(code), "{case}: {}", both(&out));
        }
        env.ok(&["stop", &instance]);
    }
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
    assert_eq!(identity, json!({"format": 4, "root": instance}));
    let record: Value =
        serde_json::from_slice(&fs::read(store.join("records/0000000001.json")).unwrap()).unwrap();
    let ack: Value =
        serde_json::from_slice(&fs::read(store.join("acks/m1.json")).unwrap()).unwrap();
    assert_eq!(record["schema"], 4);
    assert_eq!(ack["schema"], 4);
    assert!(record.get("run").is_none() && ack.get("run").is_none());
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
        "templates/oncall/template.yaml",
        &TEAM
            .replace("    parent: lead\n", "")
            .replace("  scout:\n    prompt: scout.md\n    start: false\n", ""),
    );
    env.write(
        "templates/oncall/lead.md",
        "---\nmodels: {claude: opus high}\n---\nThe lead, edited.\n",
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
    let out = member_command(
        &env,
        &instance,
        &helper,
        &["msg", "send", "external:ops", "new root", "--json"],
    );
    assert!(out.status.success(), "{}", both(&out));
    let record: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(record["id"], "m3");
    let old: Value = serde_json::from_slice(
        &env.ok(&["msg", "read", "m1", "--instance", &instance, "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(old["record"]["to"], format!("{instance}/{scout}"));
    env.ok(&["stop", &instance]);
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
        "templates/oncall/template.yaml",
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
            json!({"format": 4, "root": "other-2345"}).to_string(),
        )
        .unwrap(),
        "format" => fs::write(
            store.join("store.json"),
            json!({"format": 5, "root": instance}).to_string(),
        )
        .unwrap(),
        "format3" => fs::write(
            store.join("store.json"),
            json!({"format": 3, "instance": instance}).to_string(),
        )
        .unwrap(),
        "current" => fs::write(store.join("current"), "r1\n").unwrap(),
        "directory" => fs::create_dir(store.join("r1")).unwrap(),
        "records" => fs::create_dir(store.join("records")).unwrap(),
        _ => unreachable!(),
    }
    let before = bytes(&store);
    let err = env.fails(&["start", &instance]);
    if case == "format3" {
        assert!(err.contains(&instance), "{err}");
        assert!(err.contains("earlier format"), "{err}");
        assert!(err.contains("docs/bus.md#storage"), "{err}");
        assert!(!err.contains("brg create"), "{err}");
    } else {
        assert!(err.contains("damaged"), "{err}");
    }
    let check = env.brg(&["msg", "integrity-check", "--instance", &instance, "--json"]);
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

#[test]
fn full_start_cannot_hide_unacked_work_on_a_kept_unstarted_locked_member() {
    let env = Env::new();
    let instance = env.create("oncall", "work/a");
    env.ok(&["create", &format!("{instance}/scout")]);
    let scout = env.member(&instance, "scout");
    publish(&env, &instance, json!({"to":scout,"body":"ordinary work"}));
    env.ok(&["stop", &format!("{instance}/{scout}")]);
    env.ok(&["stop", &instance]);
    env.write(
        "templates/oncall/template.yaml",
        &TEAM.replace("prompt: scout.md", "prompt: scout.md\n    posture: locked"),
    );
    let out = env.brg(&["start", &instance]);
    assert!(!out.status.success(), "{}", both(&out));
    assert!(env.starts(&scout).len() == 1);
    let out = env.brg(&["start", &format!("{instance}/{scout}")]);
    assert!(!out.status.success(), "{}", both(&out));
    assert!(env.starts(&scout).len() == 1);
}

#[test]
fn joining_locked_member_cannot_abandon_unacked_ordinary_work() {
    let env = Env::new();
    let instance = env.create("oncall", "work/a");
    env.ok(&["create", &format!("{instance}/scout")]);
    let scout = env.member(&instance, "scout");
    publish(&env, &instance, json!({"to":scout,"body":"ordinary work"}));
    env.ok(&["stop", &format!("{instance}/{scout}")]);
    env.write(
        "templates/oncall/template.yaml",
        &TEAM.replace("prompt: scout.md", "prompt: scout.md\n    posture: locked"),
    );
    // Starting a helper must not transition the stopped scout.
    env.ok(&["create", &format!("{instance}/helper")]);
    let out = env.brg(&["start", &format!("{instance}/{scout}")]);
    assert!(!out.status.success(), "{}", both(&out));
    assert_eq!(env.starts(&scout).len(), 1);
    assert_eq!(env.row(&instance, "scout")["unacked"], 1);
    env.ok(&["stop", &instance]);
}

#[test]
fn vendor_to_locked_requires_settling_ordinary_messages() {
    let env = Env::new();
    let instance = env.create("oncall", "work/a");
    let lead = env.member(&instance, "lead");
    publish(&env, &instance, json!({"to":lead,"body":"ordinary work"}));
    env.ok(&["stop", &instance]);
    env.write(
        "templates/oncall/template.yaml",
        &TEAM.replace(
            "    prompt: lead.md",
            "    prompt: lead.md\n    posture: locked",
        ),
    );
    let out = env.cmd(&["start", &instance]).output().unwrap();
    assert!(!out.status.success(), "{}", both(&out));
    assert!(
        both(&out).contains("settle them before changing to locked"),
        "{}",
        both(&out)
    );
    let stored = fs::read(env.path(&format!(
        "state/{instance}/messages/records/0000000001.json"
    )))
    .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&stored).unwrap()["ack_required"],
        true
    );
    // Settle it under the vendor configuration, then the same transition succeeds.
    env.write("templates/oncall/template.yaml", TEAM);
    env.ok(&["start", &instance]);
    env.wait_for("new lead launch", || env.starts(&lead).len() >= 2);
    let ack = member_command(&env, &instance, &lead, &["msg", "ack", "m1", "--json"]);
    assert!(ack.status.success(), "{}", both(&ack));
    env.ok(&["stop", &instance]);
    env.write(
        "templates/oncall/template.yaml",
        &TEAM.replace(
            "    prompt: lead.md",
            "    prompt: lead.md\n    posture: locked",
        ),
    );
    env.ok(&["start", &instance]);
    let meta: Value = serde_json::from_slice(
        &fs::read(env.path(&format!("state/{instance}/start.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(meta["members"][&lead]["posture"], "locked");
    env.ok(&["stop", &instance]);
}

#[test]
fn a_format_3_store_is_refused_without_writes() {
    damaged_store("format3");
}
