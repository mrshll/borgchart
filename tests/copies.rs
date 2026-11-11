//! Copy limits, parent authority and checkout separation through the CLI.
#![cfg(feature = "test-support")]

mod common;
mod instance_env;

use std::collections::BTreeMap;
use std::fs;
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
        // A restarted fake agent writes its start record shortly after `start` returns.
        self.wait_for("the member's start record", || {
            self.starts(member).len() > start
        });
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
    env.role_extra("helper", &format!("    max: {max}\n    worktree: true\n"));
    fs::remove_dir(env.path("work/a")).unwrap();
    env.checkout("work/a");
    env
}

#[test]
fn copies_get_their_own_worktrees_and_keep_slots_without_filling_max() {
    let env = copies_env(2);
    let inst = env.create("oncall", "work/a");
    assert!(env.copies(&inst).is_empty());
    assert_eq!(env.state_of(&inst, "helper"), "on demand (none created)");
    let created = stdout(&env.ok(&["create", &format!("{inst}/helper")]));
    let first = env.copies(&inst)[0].clone();
    assert_eq!(env.copies(&inst), std::slice::from_ref(&first));
    // `create` names the new worktree; the status tree leaves it to --json.
    let folder = env.worktree(&format!("{inst}/{first}"));
    let named = format!("{inst}/{first}  folder {}", folder.display());
    assert!(created.lines().any(|l| l == named), "{created}");
    env.ok(&["create", &format!("{inst}/helper")]);
    let second = env
        .copies(&inst)
        .into_iter()
        .find(|id| id != &first)
        .unwrap();
    let head = git(&env.path("work/a"), &["rev-parse", "HEAD"]);
    for id in [&first, &second] {
        let folder = env.worktree(&format!("{inst}/{id}"));
        assert!(folder.join(".git").is_file(), "{}", folder.display());
        assert_eq!(git(&folder, &["rev-parse", "HEAD"]), head);
        env.wait_for("copy idle", || !env.starts(id).is_empty());
        assert_eq!(
            env.starts(id)[0]["cwd"],
            folder.canonicalize().unwrap().display().to_string()
        );
        let row = env
            .rows(&inst)
            .into_iter()
            .find(|r| r["member"] == **id)
            .unwrap();
        assert_eq!(row["worktree"], folder.display().to_string());
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
    assert!(!tree.contains(&folder.display().to_string()), "{tree}");
    env.ok(&["start", &format!("{inst}/{first}")]);
    assert_eq!(env.starts(&first).len(), 2);
    env.ok(&["stop", &format!("{inst}/{first}")]);
    env.ok(&["delete", &format!("{inst}/{first}")]);
    assert!(!env.worktree(&format!("{inst}/{first}")).exists());
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
fn copies_without_a_worktree_share_the_instance_folder() {
    let env = Env::new();
    env.role_extra("helper", "    max: 2\n");
    env.ok(&["validate", "oncall"]);
    let inst = env.create("oncall", "work/a");
    let first = env.copy(&inst);
    let second = env.copy(&inst);
    let folder = env.path("work/a").canonicalize().unwrap();
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

#[test]
fn a_worktree_copy_needs_a_git_repository() {
    let env = Env::new();
    env.role_extra("helper", "    max: 2\n    worktree: true\n");
    let inst = env.create("oncall", "work/a");
    let err = env.fails(&["create", &format!("{inst}/helper")]);
    assert!(err.contains("needs its own worktree"), "{err}");
    assert!(env.copies(&inst).is_empty());
    assert!(
        fs::read_dir(env.path("state-worktrees"))
            .map(|mut d| d.next().is_none())
            .unwrap_or(true)
    );
    let err = env.fails(&["create", &format!("{inst}/scout"), "--branch", "topic"]);
    assert!(err.contains("no `worktree: true`"), "{err}");
    env.ok(&["stop", &inst]);
}

#[test]
fn branch_puts_a_new_worktree_on_a_new_branch() {
    let env = copies_env(2);
    let inst = env.create("oncall", "work/a");
    env.ok(&["create", &format!("{inst}/helper"), "--branch", "topic"]);
    let id = env.copies(&inst)[0].clone();
    let folder = env.worktree(&format!("{inst}/{id}"));
    assert_eq!(git(&folder, &["branch", "--show-current"]).trim(), "topic");
    env.ok(&["stop", &inst]);
}

#[test]
fn delete_keeps_a_worktree_with_work_in_it() {
    let env = copies_env(2);
    let inst = env.create("oncall", "work/a");
    let id = env.copy(&inst);
    let folder = env.worktree(&format!("{inst}/{id}"));
    fs::write(folder.join("draft.txt"), "unsaved").unwrap();
    env.ok(&["stop", &inst]);
    let err = env.fails(&["delete", &format!("{inst}/{id}")]);
    assert!(err.contains("uncommitted or untracked changes"), "{err}");
    assert!(env.copies(&inst).contains(&id));
    fs::remove_file(folder.join("draft.txt")).unwrap();
    git(
        &folder,
        &["commit", "-q", "--allow-empty", "-m", "local only"],
    );
    let err = env.fails(&["delete", &format!("{inst}/{id}")]);
    assert!(err.contains("on no remote"), "{err}");
    let out = stdout(&env.ok(&["delete", &format!("{inst}/{id}"), "--force"]));
    assert!(out.contains("left") && out.contains("as it is"), "{out}");
    assert!(folder.join(".git").is_file());
    assert!(env.copies(&inst).is_empty());
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
fn a_copy_whose_worktree_is_gone_does_not_start() {
    let env = copies_env(2);
    let inst = env.create("oncall", "work/a");
    let first = env.copy(&inst);
    env.ok(&["stop", &format!("{inst}/{first}")]);
    fs::remove_dir_all(env.worktree(&format!("{inst}/{first}"))).unwrap();
    let err = env.fails(&["start", &format!("{inst}/{first}")]);
    assert!(err.contains("is gone"), "{err}");
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
    env.role_extra("lead", "    worktree: true\n");
    let inst = env.create("oncall", "work/a");
    let first = env.member(&inst, "helper");
    env.ok(&["stop", &inst]);
    env.ok(&["delete", &format!("{inst}/{first}")]);
    // The lead's worktree is gone, so the start is refused before it saves.
    fs::remove_dir_all(env.worktree(&format!("{inst}/{}", env.member(&inst, "lead")))).unwrap();

    env.fails(&["start", &inst]);
    assert!(env.copies(&inst).is_empty());
    let left: Vec<String> = fs::read_dir(env.path("state-worktrees"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(left.iter().all(|name| !name.contains("helper")), "{left:?}");
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
    let tmp = env.path(&format!("state/{inst}/tmp"));
    fs::remove_dir_all(&tmp).unwrap();
    fs::write(&tmp, "obstruct startup").unwrap();

    env.fails(&["start", &inst]);
    let replacement = env.member(&inst, "helper");
    assert!(!replacement.is_empty());
    assert_ne!(replacement, first);
    assert!(
        env.worktree(&format!("{inst}/{replacement}"))
            .join(".git")
            .is_file()
    );
    fs::remove_file(&tmp).unwrap();
    env.ok(&["start", &format!("{inst}/{replacement}")]);
    env.ok(&["stop", &inst]);
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
    let folder = env.worktree(&format!("{inst}/{}", replacement[0]));
    assert!(folder.join(".git").is_file());
    env.wait_for("replacement started", || {
        !env.starts(&replacement[0]).is_empty()
    });
    assert_eq!(
        env.starts(&replacement[0])[0]["cwd"],
        folder.canonicalize().unwrap().display().to_string()
    );
    env.ok(&["stop", &inst]);
}

#[test]
fn delete_and_cleanup_never_follow_a_rewritten_worktree_git_file() {
    // Catches: brg running git through a member-writable `.git` file, so a
    // repository it points at can run a command as the person (here, its
    // file-system monitor).
    let env = copies_env(2);
    let inst = env.create("oncall", "work/a");
    let id = env.copy(&inst);
    let folder = env.worktree(&format!("{inst}/{id}"));
    let hostile = env.path("hostile");
    git(
        env.dir.path(),
        &["init", "-q", &hostile.display().to_string()],
    );
    let ran = env.path("ran");
    git(
        &hostile,
        &[
            "config",
            "core.fsmonitor",
            &format!("touch {}", ran.display()),
        ],
    );
    fs::write(
        folder.join(".git"),
        format!("gitdir: {}\n", hostile.join(".git").display()),
    )
    .unwrap();
    env.ok(&["stop", &inst]);
    let _ = env.brg(&["delete", &format!("{inst}/{id}")]);
    let _ = env.brg(&["cleanup"]);
    assert!(!ran.exists(), "brg ran the hostile repository's command");
}
