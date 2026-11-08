//! Copy limits, parent authority and checkout separation through the CLI.
#![cfg(feature = "test-support")]

mod common;
mod instance_env;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::process::Output;

use instance_env::*;
use serde_json::json;

impl Env {
    fn role_extra(&self, role: &str, lines: &str) {
        let path = self.path("teams/oncall/team.yaml");
        let text = fs::read_to_string(&path).unwrap();
        let entry = format!("    prompt: {role}.md\n");
        fs::write(path, text.replace(&entry, &format!("{entry}{lines}"))).unwrap();
    }

    fn copies(&self, inst: &str) -> Vec<String> {
        self.rows(inst)
            .iter()
            .filter(|r| r["role"] == "helper")
            .filter_map(|r| r["member"].as_str().map(str::to_string))
            .collect()
    }

    fn copy(&self, inst: &str) -> String {
        let before = self.copies(inst);
        let out = self
            .cmd(&["create", &format!("{inst}/helper")])
            .env("BORGCHART_SUFFIX_SEED", "7")
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", both(&out));
        self.copies(inst)
            .into_iter()
            .find(|id| !before.contains(id))
            .unwrap()
    }

    fn race_copies(&self, inst: &str) -> [Output; 2] {
        let mut a = self.cmd(&["create", &format!("{inst}/helper")]);
        let mut b = self.cmd(&["create", &format!("{inst}/helper")]);
        for cmd in [&mut a, &mut b] {
            cmd.env("BORGCHART_SUFFIX_SEED", "7")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
        }
        let a = a.spawn().unwrap();
        let b = b.spawn().unwrap();
        [a.wait_with_output().unwrap(), b.wait_with_output().unwrap()]
    }

    fn as_member(&self, member: &str, start: usize, args: &[&str]) -> Output {
        let vars: BTreeMap<String, String> =
            serde_json::from_value(self.starts(member)[start]["env"].clone()).unwrap();
        common::command(env!("CARGO_BIN_EXE_brg"))
            .env_clear()
            .envs(vars)
            .env("BORGCHART_SUFFIX_SEED", "7")
            .args(args)
            .output()
            .unwrap()
    }
}

fn copies_env(max: usize) -> Env {
    let env = Env::new();
    env.role_extra(
        "helper",
        &format!("    max: {max}\n    cwd: copies/{{member}}\n"),
    );
    // The fixed test-support seed 7 issues these suffixes in order. The
    // folders must exist before create; brg does not create checkouts.
    for suffix in ["v4s4", "7gb5", "tqv5", "8vcw", "at6v"] {
        fs::create_dir_all(env.path(&format!("work/a/copies/helper-{suffix}"))).unwrap();
    }
    env
}

#[test]
fn copies_keep_their_slots_and_restart_without_filling_max() {
    let env = copies_env(2);
    let inst = env.create("oncall", "work/a");
    assert!(env.copies(&inst).is_empty());
    assert!(stdout(&env.ok(&["status", &inst])).contains("helper"));
    let first = env.copy(&inst);
    assert_eq!(env.copies(&inst), std::slice::from_ref(&first));
    let out = env.as_member(&first, 0, &["create", &format!("{inst}/helper")]);
    assert!(!out.status.success(), "self replication: {}", both(&out));
    let second = env.copy(&inst);
    for id in [&first, &second] {
        env.wait_for("copy idle", || !env.starts(id).is_empty());
        assert_eq!(
            env.starts(id)[0]["cwd"],
            env.path(&format!("work/a/copies/{id}"))
                .display()
                .to_string()
        );
    }
    env.ok(&["stop", &format!("{inst}/{first}")]);
    let out = env
        .cmd(&["create", &format!("{inst}/helper")])
        .env("BORGCHART_SUFFIX_SEED", "7")
        .output()
        .unwrap();
    assert!(!out.status.success(), "{}", both(&out));
    let refused = both(&out);
    assert!(
        refused.contains(&first) && refused.contains(&second),
        "{refused}"
    );
    let rows = env.rows(&inst);
    assert!(
        rows.iter()
            .any(|r| r["member"] == first && r["state"] == "stopped")
    );
    let tree = stdout(&env.ok(&["status", &inst]));
    assert!(tree.contains(&first) && tree.contains(&second));
    env.ok(&["start", &format!("{inst}/{first}")]);
    assert_eq!(env.starts(&first).len(), 2);
    env.ok(&["stop", &format!("{inst}/{first}")]);
    env.ok(&["delete", &format!("{inst}/{first}")]);
    let next = env.copy(&inst);
    assert_ne!(next, first);
    assert_eq!(env.copies(&inst).len(), 2);
    env.ok(&["stop", &format!("{inst}/{next}")]);
    env.ok(&["delete", &format!("{inst}/{next}")]);
    env.ok(&["stop", &inst]);
    env.ok(&["start", &inst]);
    assert_eq!(env.copies(&inst), [second]);
    env.ok(&["stop", &inst]);
}

#[test]
fn concurrent_creates_cannot_take_the_same_last_slot() {
    let env = copies_env(2);
    let inst = env.create("oncall", "work/a");
    env.copy(&inst);
    let [a, b] = env.race_copies(&inst);
    assert_ne!(
        a.status.success(),
        b.status.success(),
        "{}{}",
        both(&a),
        both(&b)
    );
    assert_eq!(env.copies(&inst).len(), 2);
    env.ok(&["stop", &inst]);
}

#[test]
fn only_the_latest_parent_start_can_manage_children() {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    let lead = env.member(&inst, "lead");
    let out = env.as_member(&lead, 0, &["create", &format!("{inst}/helper")]);
    assert!(out.status.success(), "{}", both(&out));
    let helper = env.member(&inst, "helper");
    env.ok(&["create", &format!("{inst}/scout")]);
    let scout = env.member(&inst, "scout");
    for caller in [&helper, &scout] {
        for verb in ["start", "stop"] {
            let out = env.as_member(caller, 0, &[verb, &helper]);
            assert!(!out.status.success(), "{caller} {verb}: {}", both(&out));
        }
    }
    env.ok(&["stop", &format!("{inst}/{lead}")]);
    env.ok(&["start", &format!("{inst}/{lead}")]);
    for verb in ["start", "stop", "delete"] {
        let out = env.as_member(&lead, 0, &[verb, &helper]);
        assert!(!out.status.success(), "stale {verb}: {}", both(&out));
    }
    let out = env.as_member(&lead, 1, &["stop", &helper]);
    assert!(out.status.success(), "{}", both(&out));
    for (caller, start) in [(&helper, 0), (&scout, 0), (&lead, 0)] {
        for verb in ["start", "delete"] {
            let out = env.as_member(caller, start, &[verb, &helper]);
            assert!(!out.status.success(), "{caller} {verb}: {}", both(&out));
        }
    }
    for verb in ["start", "stop", "delete"] {
        let out = env.as_member(&lead, 1, &[verb, &helper]);
        assert!(out.status.success(), "parent {verb}: {}", both(&out));
    }
    for caller in [&lead, &scout] {
        let out = env.as_member(caller, 0, &["create", &format!("{inst}/helper")]);
        assert!(!out.status.success(), "{}", both(&out));
    }
    let out = env.as_member(&lead, 1, &["create", &format!("{inst}/helper")]);
    assert!(out.status.success(), "{}", both(&out));
    let child = env.member(&inst, "helper");
    env.ok(&["stop", &format!("{inst}/{child}")]);
    env.ok(&["delete", &format!("{inst}/{child}")]);
    let other = env.create("oncall", "work/b");
    let out = env.as_member(&lead, 1, &["create", &format!("{other}/helper")]);
    assert!(!out.status.success(), "{}", both(&out));
    env.ok(&["create", &format!("{inst}/helper")]);
    env.ok(&["stop", &inst]);
    env.ok(&["stop", &other]);
    env.ok(&["delete", &format!("{inst}/{lead}")]);
    let child = env.member(&inst, "helper");
    env.ok(&["delete", &format!("{inst}/{child}")]);
    env.fails(&["create", &format!("{inst}/helper")]);
}

#[test]
fn copies_refuse_aliases_and_nested_folders_even_concurrently() {
    for nested in [false, true] {
        let env = copies_env(3);
        let inst = env.create("oncall", "work/a");
        let first = env.copy(&inst);
        let folder = env.path(&format!("work/a/copies/{first}"));
        for suffix in ["v4s4", "7gb5", "tqv5", "8vcw", "at6v"] {
            let alias = env.path(&format!("work/a/copies/helper-{suffix}"));
            if alias == folder {
                continue;
            }
            fs::remove_dir(&alias).unwrap();
            let target = if nested {
                folder.join("sub")
            } else {
                folder.clone()
            };
            fs::create_dir_all(&target).unwrap();
            symlink(target, alias).unwrap();
        }
        // A stopped copy still owns its folder until deleted.
        env.ok(&["stop", &format!("{inst}/{first}")]);
        for out in env.race_copies(&inst) {
            assert!(!out.status.success(), "{}", both(&out));
        }
        assert_eq!(env.copies(&inst), [first]);
        env.ok(&["stop", &inst]);
    }
}

#[test]
fn concurrent_copies_cannot_claim_overlapping_folders() {
    for nested in [false, true] {
        let env = copies_env(2);
        let folder = env.path("work/a/shared");
        fs::create_dir_all(folder.join("sub")).unwrap();
        for suffix in ["v4s4", "7gb5", "tqv5", "8vcw", "at6v"] {
            let alias = env.path(&format!("work/a/copies/helper-{suffix}"));
            fs::remove_dir(&alias).unwrap();
            let target = if nested && suffix != "v4s4" {
                folder.join("sub")
            } else {
                folder.clone()
            };
            symlink(target, alias).unwrap();
        }
        let inst = env.create("oncall", "work/a");
        let [a, b] = env.race_copies(&inst);
        assert_ne!(
            a.status.success(),
            b.status.success(),
            "{}{}",
            both(&a),
            both(&b)
        );
        assert_eq!(env.copies(&inst).len(), 1);
        env.ok(&["stop", &inst]);
    }
}

#[test]
fn start_true_copies_keep_members_when_max_is_lowered() {
    let env = copies_env(3);
    let path = env.path("teams/oncall/team.yaml");
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace("    start: false", "    start: true");
    fs::write(&path, text).unwrap();
    let out = env
        .cmd(&[
            "create",
            "oncall",
            &env.path("work/a").display().to_string(),
        ])
        .env("BORGCHART_SUFFIX_SEED", "7")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    let inst = stdout(&out).split_whitespace().next().unwrap().to_string();
    assert_eq!(env.copies(&inst).len(), 1);
    env.ok(&["start", &inst]);
    assert_eq!(env.copies(&inst).len(), 1);
    env.copy(&inst);
    let kept = env.copies(&inst);
    env.ok(&["stop", &inst]);
    fs::write(
        &path,
        fs::read_to_string(&path)
            .unwrap()
            .replace("max: 3", "max: 1"),
    )
    .unwrap();
    env.ok(&["start", &inst]);
    assert_eq!(env.copies(&inst), kept);
    let out = env
        .cmd(&["create", &format!("{inst}/helper")])
        .env("BORGCHART_SUFFIX_SEED", "7")
        .output()
        .unwrap();
    assert!(!out.status.success(), "{}", both(&out));
    env.ok(&["stop", &inst]);
    for id in &kept {
        env.ok(&["delete", &format!("{inst}/{id}")]);
    }
    // Restore the fixed seed for Q2's replacement folder.
    let out = env
        .cmd(&["start", &inst])
        .env("BORGCHART_SUFFIX_SEED", "7")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    let replacement = env.copies(&inst);
    assert_eq!(replacement.len(), 1);
    assert!(!kept.contains(&replacement[0]));
    env.ok(&["stop", &inst]);
}

#[test]
fn different_roles_can_still_share_a_working_folder() {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    env.ok(&["create", &format!("{inst}/helper")]);
    let lead = env.member(&inst, "lead");
    let helper = env.member(&inst, "helper");
    assert_eq!(env.starts(&lead)[0]["cwd"], env.starts(&helper)[0]["cwd"]);
    let out = env.publish(&inst, &json!({"to": helper, "body": "Review this sample."}));
    assert!(out.status.success(), "{}", both(&out));
    env.ok(&["stop", &inst]);
}
