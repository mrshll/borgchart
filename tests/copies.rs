//! Copy limits, parent authority and checkout separation through the CLI.
#![cfg(feature = "test-support")]

mod common;
mod instance_env;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::process::Output;

use instance_env::*;

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
        &format!("    max: {max}\n    cwd: copies/{{member_id}}\n"),
    );
    fs::create_dir(env.path("work/a/copies")).unwrap();
    env
}

#[test]
fn copies_create_empty_folders_and_keep_slots_without_filling_max() {
    let env = copies_env(2);
    let inst = env.create("oncall", "work/a");
    assert!(env.copies(&inst).is_empty());
    assert_eq!(env.state_of(&inst, "helper"), "on demand (none created)");
    env.ok(&["create", &format!("{inst}/helper")]);
    let first = env.copies(&inst)[0].clone();
    assert_eq!(env.copies(&inst), std::slice::from_ref(&first));
    env.ok(&["create", &format!("{inst}/helper")]);
    let second = env
        .copies(&inst)
        .into_iter()
        .find(|id| id != &first)
        .unwrap();
    for id in [&first, &second] {
        let folder = env.path(&format!("work/a/copies/{id}"));
        assert!(folder.is_dir());
        assert!(fs::read_dir(&folder).unwrap().next().is_none());
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
    assert!(env.path(&format!("work/a/copies/{first}")).is_dir());
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
fn copies_without_member_cwd_share_the_roles_folder() {
    for cwd in [None, Some("shared")] {
        let env = Env::new();
        env.role_extra("helper", "    max: 2\n");
        if let Some(cwd) = cwd {
            env.role_extra("helper", &format!("    cwd: {cwd}\n"));
            fs::create_dir(env.path(&format!("work/a/{cwd}"))).unwrap();
        }
        env.ok(&["validate", "oncall"]);
        let inst = env.create("oncall", "work/a");
        let first = env.copy(&inst);
        let second = env.copy(&inst);
        let folder = env
            .path("work/a")
            .join(cwd.unwrap_or(""))
            .canonicalize()
            .unwrap();
        for id in [&first, &second] {
            env.wait_for("copy started", || !env.starts(id).is_empty());
            assert_eq!(env.starts(id)[0]["cwd"], folder.display().to_string());
        }
        env.ok(&["stop", &inst]);
        env.ok(&["start", &inst]);
        for id in [&first, &second] {
            assert_eq!(env.starts(id).len(), 2);
        }
        env.ok(&["stop", &inst]);
    }
}

#[test]
fn creating_a_copy_requires_existing_ancestors_and_stays_inside_the_instance() {
    for escape in [false, true] {
        let env = copies_env(2);
        fs::remove_dir(env.path("work/a/copies")).unwrap();
        if escape {
            symlink(env.path("work/b"), env.path("work/a/copies")).unwrap();
        }
        let inst = env.create("oncall", "work/a");
        env.fails(&["create", &format!("{inst}/helper")]);
        assert!(env.copies(&inst).is_empty());
        if escape {
            assert!(fs::read_dir(env.path("work/b")).unwrap().next().is_none());
        } else {
            assert!(!env.path("work/a/copies").exists());
        }
        env.ok(&["stop", &inst]);
    }
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
fn creating_a_copy_ignores_a_missing_kept_folder() {
    let env = copies_env(2);
    let inst = env.create("oncall", "work/a");
    let first = env.copy(&inst);
    env.ok(&["stop", &format!("{inst}/{first}")]);
    fs::remove_dir(env.path(&format!("work/a/copies/{first}"))).unwrap();
    env.fails(&["start", &format!("{inst}/{first}")]);
    assert!(!env.path(&format!("work/a/copies/{first}")).exists());
    let second = env.copy(&inst);
    assert_ne!(first, second);
    env.wait_for("second copy idle", || !env.starts(&second).is_empty());
    assert_eq!(env.copies(&inst).len(), 2);
    env.ok(&["stop", &inst]);
}

#[test]
fn refused_replacement_starts_do_not_leave_unkept_copy_folders() {
    let env = copies_env(2);
    let path = env.path("teams/oncall/team.yaml");
    fs::write(
        &path,
        fs::read_to_string(&path).unwrap().replace(
            "    parent: lead\n    start: false",
            "    parent: lead\n    start: true",
        ),
    )
    .unwrap();
    let inst = env.create("oncall", "work/a");
    let first = env.member(&inst, "helper");
    env.ok(&["stop", &inst]);
    env.ok(&["delete", &format!("{inst}/{first}")]);
    fs::remove_dir(env.path(&format!("work/a/copies/{first}"))).unwrap();
    env.role_extra("lead", "    cwd: missing\n");

    env.fails(&["start", &inst]);
    assert!(env.copies(&inst).is_empty());
    assert!(
        fs::read_dir(env.path("work/a/copies"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn refused_starts_keep_folders_for_members_saved_before_launch() {
    let env = copies_env(2);
    let path = env.path("teams/oncall/team.yaml");
    fs::write(
        &path,
        fs::read_to_string(&path).unwrap().replace(
            "    parent: lead\n    start: false",
            "    parent: lead\n    start: true",
        ),
    )
    .unwrap();
    let inst = env.create("oncall", "work/a");
    let first = env.member(&inst, "helper");
    env.ok(&["stop", &inst]);
    env.ok(&["delete", &format!("{inst}/{first}")]);
    fs::remove_dir(env.path(&format!("work/a/copies/{first}"))).unwrap();
    let tmp = env.path(&format!("state/{inst}/tmp"));
    fs::remove_dir_all(&tmp).unwrap();
    fs::write(&tmp, "obstruct startup").unwrap();

    env.fails(&["start", &inst]);
    let replacement = env.member(&inst, "helper");
    assert!(!replacement.is_empty());
    assert_ne!(replacement, first);
    assert!(env.path(&format!("work/a/copies/{replacement}")).is_dir());
    fs::remove_file(&tmp).unwrap();
    env.ok(&["start", &format!("{inst}/{replacement}")]);
    env.ok(&["stop", &inst]);
}

#[test]
fn restarting_copies_refuses_changed_folder_aliases() {
    for full in [false, true] {
        let env = copies_env(2);
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
        let first = env.copies(&inst)[0].clone();
        let second = env.copy(&inst);
        env.ok(&["stop", &inst]);
        if full {
            fs::write(
                &path,
                fs::read_to_string(&path)
                    .unwrap()
                    .replace("max: 2", "max: 1"),
            )
            .unwrap();
        }
        let folder = env.path(&format!("work/a/copies/{first}"));
        let alias = env.path(&format!("work/a/copies/{second}"));
        fs::remove_dir(&alias).unwrap();
        symlink(folder, alias).unwrap();
        let target = if full {
            inst.clone()
        } else {
            format!("{inst}/{second}")
        };
        let out = env.brg(&["start", &target]);
        assert!(!out.status.success(), "full={full}: {}", both(&out));
        assert_eq!(env.starts(&second).len(), 1);
        env.ok(&["stop", &inst]);
    }
}

#[test]
fn recorded_parents_can_manage_children_after_live_team_edits() {
    for invalid in [false, true] {
        let env = Env::new();
        let inst = env.create("oncall", "work/a");
        env.ok(&["create", &format!("{inst}/helper")]);
        env.ok(&["create", &format!("{inst}/scout")]);
        let lead = env.member(&inst, "lead");
        let helper = env.member(&inst, "helper");
        let scout = env.member(&inst, "scout");
        let path = env.path("teams/oncall/team.yaml");
        let original = fs::read_to_string(&path).unwrap();
        let changed = if invalid {
            "roles: [".into()
        } else {
            original.replace("parent: lead", "parent: scout")
        };
        fs::write(&path, changed).unwrap();
        let out = env.as_member(&scout, 0, &["stop", &helper]);
        assert!(!out.status.success(), "live parent: {}", both(&out));
        for verb in ["stop", "delete"] {
            let out = env.as_member(&lead, 0, &[verb, &helper]);
            assert!(
                out.status.success(),
                "invalid={invalid} {verb}: {}",
                both(&out)
            );
        }
        fs::write(path, original).unwrap();
        env.ok(&["stop", &inst]);
    }
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
    // A parent with kept children can't be deleted (#74): the child goes first.
    let child = env.member(&inst, "helper");
    env.fails(&["delete", &format!("{inst}/{lead}")]);
    env.ok(&["delete", &format!("{inst}/{child}")]);
    env.ok(&["delete", &format!("{inst}/{lead}")]);
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
    env.ok(&["start", &inst]);
    let replacement = env.copies(&inst);
    assert_eq!(replacement.len(), 1);
    assert!(!kept.contains(&replacement[0]));
    let folder = env.path(&format!("work/a/copies/{}", replacement[0]));
    assert!(folder.is_dir());
    assert!(fs::read_dir(&folder).unwrap().next().is_none());
    env.wait_for("replacement started", || {
        !env.starts(&replacement[0]).is_empty()
    });
    assert_eq!(
        env.starts(&replacement[0])[0]["cwd"],
        folder.display().to_string()
    );
    env.ok(&["stop", &inst]);
}

#[test]
fn different_roles_can_still_share_a_working_folder() {
    let env = copies_env(2);
    fs::create_dir(env.path("work/a/copies/helper-v4s4")).unwrap();
    env.write(
        "work/a/copies/helper-v4s4/checkout.txt",
        "existing checkout",
    );
    env.role_extra("lead", "    cwd: copies/helper-v4s4\n");
    let inst = env.create("oncall", "work/a");
    let helper = env.copy(&inst);
    let lead = env.member(&inst, "lead");
    env.wait_for("helper idle", || !env.starts(&helper).is_empty());
    assert_eq!(env.starts(&lead)[0]["cwd"], env.starts(&helper)[0]["cwd"]);
    assert_eq!(
        fs::read_to_string(env.path("work/a/copies/helper-v4s4/checkout.txt")).unwrap(),
        "existing checkout"
    );
    env.ok(&["stop", &inst]);
}
