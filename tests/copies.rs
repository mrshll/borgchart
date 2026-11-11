//! Advisory copy counts, parent authority and checkout separation through the CLI.
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
fn member_create_past_max_keeps_worktrees_and_warns_about_stopped_members() {
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
    assert!(out.status.success(), "{}", both(&out));
    let warning = stdout(&out);
    assert!(
        warning.contains("warning: role `helper` now has 3 kept members, over its max of 2"),
        "{warning}"
    );
    let third = env
        .copies(&inst)
        .into_iter()
        .find(|id| id != &first && id != &second)
        .unwrap();
    env.ok(&["stop", &format!("{inst}/{third}")]);
    env.ok(&["delete", &format!("{inst}/{third}")]);
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
fn concurrent_member_creates_warn_at_the_locked_count_without_reusing_ids() {
    let env = copies_env(2);
    let inst = env.create("oncall", "work/a");
    env.copy(&inst);
    let [a, b] = env.race_copies(&inst);
    assert!(
        a.status.success() && b.status.success(),
        "{}{}",
        both(&a),
        both(&b)
    );
    assert_eq!(env.copies(&inst).len(), 3);
    let warnings = [stdout(&a), stdout(&b)].join("\n");
    assert_eq!(
        warnings.matches("warning: role `helper`").count(),
        1,
        "{warnings}"
    );
    assert!(
        warnings.contains("3 kept members, over its max of 2"),
        "{warnings}"
    );
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
fn create_and_a_full_start_name_the_worktrees_they_add() {
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
    let named = |out: &str, inst: &str, id: &str| {
        let line = format!(
            "{inst}/{id}  folder {}",
            env.worktree(&format!("{inst}/{id}")).display()
        );
        assert!(out.lines().any(|l| l == line), "{out}");
    };
    let out = stdout(&env.ok(&[
        "create",
        "oncall",
        &env.path("work/a").display().to_string(),
    ]));
    let inst = out.split_whitespace().next().unwrap().to_string();
    let first = env.member(&inst, "helper");
    named(&out, &inst, &first);
    env.ok(&["stop", &inst]);
    env.ok(&["delete", &format!("{inst}/{first}")]);
    // A full start gives the `start: true` role a new member and worktree.
    let out = stdout(&env.ok(&["start", &inst]));
    let second = env.member(&inst, "helper");
    assert_ne!(first, second);
    named(&out, &inst, &second);
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
fn lowered_max_keeps_existing_members_and_warns_on_another_create() {
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
    assert!(out.status.success(), "{}", both(&out));
    assert!(
        stdout(&out).contains("3 kept members, over its max of 1"),
        "{}",
        both(&out)
    );
    env.ok(&["stop", &inst]);
    for id in &env.copies(&inst) {
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

// A plain root with two real clones of a local origin, plus folders that
// must never become sources. Nothing in these scenarios reaches the network.
fn repositories_env(default: &str) -> Env {
    let env = Env::new();
    env.team("teams/workers");
    env.write(
        "teams/oncall/team.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md}\n  helper: {team: workers, parent: lead, start: false, max: 8, worktree: true}\n  scout: {prompt: scout.md, start: false}\n",
    );
    let workers = env.path("teams/workers/team.yaml");
    let text = fs::read_to_string(&workers).unwrap();
    fs::write(
        workers,
        text.replace(
            "    vendor: codex\n",
            "    vendor: codex\n    worktree: true\n",
        ),
    )
    .unwrap();
    let widgets = env.checkout("work/a/widgets");
    let origin = git(&widgets, &["remote", "get-url", "origin"]);
    if default != "main" {
        git(&widgets, &["branch", "-m", default]);
        git(&widgets, &["push", "-q", "origin", default]);
        git(
            std::path::Path::new(origin.trim()),
            &["symbolic-ref", "HEAD", &format!("refs/heads/{default}")],
        );
    }
    git(
        env.dir.path(),
        &[
            "clone",
            "-q",
            origin.trim(),
            &env.path("work/a/gadgets").display().to_string(),
        ],
    );
    fs::create_dir(env.path("work/a/scratch")).unwrap();
    fs::create_dir(env.path("work/a/linked")).unwrap();
    env.write("work/a/linked/.git", "gitdir: /nonexistent/ignored\n");
    env
}

fn repo_copy(env: &Env, inst: &str, repo: &str, branch: Option<&str>) -> String {
    let mut args = vec![
        "create".to_string(),
        format!("{inst}/helper"),
        "--repo".into(),
        repo.into(),
    ];
    if let Some(branch) = branch {
        args.extend(["--branch".into(), branch.into()]);
    }
    let out = env.ok(&args.iter().map(String::as_str).collect::<Vec<_>>());
    let copy = stdout(&out).split_whitespace().next().unwrap().to_string();
    let head = git(&env.worktree(&copy), &["rev-parse", "HEAD"]);
    assert!(
        stdout(&out).contains(&format!("· {repo} at {}", &head[..7])),
        "{}",
        both(&out)
    );
    copy
}

#[test]
fn repo_default_branch_options_are_refused_without_running_them_or_issuing_anything() {
    let env = repositories_env("main");
    let widgets = env.path("work/a/widgets");
    let origin = git(&widgets, &["remote", "get-url", "origin"]);
    let origin = std::path::Path::new(origin.trim());
    let marker = widgets.join("option-ran");
    let branch = "--upload-pack=touch${IFS}option-ran";
    let reference = format!("refs/heads/{branch}");
    git(origin, &["update-ref", &reference, "HEAD"]);
    git(origin, &["symbolic-ref", "HEAD", &reference]);
    let inst = env.create("oncall", "work/a");
    let record = env.path(&format!("state/{inst}/instance.json"));
    let before = fs::read(&record).unwrap();
    let out = env.brg(&["create", &format!("{inst}/helper"), "--repo", "widgets"]);
    assert!(!marker.exists(), "origin's branch ran as a fetch option");
    assert!(!out.status.success(), "{}", both(&out));
    assert!(both(&out).contains("widgets"), "{}", both(&out));
    assert_eq!(fs::read(&record).unwrap(), before, "issued an ID");
    env.ok(&["stop", &inst]);
}

#[test]
fn repo_creation_in_a_managed_root_refuses_before_contacting_origin() {
    let env = repositories_env("main");
    let root = env.path("state-worktrees/root");
    fs::create_dir_all(root.parent().unwrap()).unwrap();
    fs::rename(env.path("work/a"), &root).unwrap();
    let widgets = root.join("widgets");
    git(
        &widgets,
        &[
            "config",
            "remote.origin.uploadpack",
            "touch network-ran; git-upload-pack",
        ],
    );
    let inst = env.create("oncall", "state-worktrees/root");
    let record = env.path(&format!("state/{inst}/instance.json"));
    let before = fs::read(&record).unwrap();
    env.fails(&["create", &format!("{inst}/helper"), "--repo", "widgets"]);
    assert!(
        !widgets.join("network-ran").exists(),
        "origin was contacted"
    );
    assert_eq!(fs::read(&record).unwrap(), before, "issued an ID");
    env.ok(&["stop", &inst]);
}

#[test]
fn repo_network_failures_identify_the_clone_and_preserve_the_transport_cause() {
    for stage in ["ls-remote", "targeted", "fallback", "no-default"] {
        let env = repositories_env("main");
        let inst = env.create("oncall", "work/a");
        let widgets = env.path("work/a/widgets");
        let real_git = common::command("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap();
        let real_git = String::from_utf8_lossy(&real_git.stdout);
        if stage == "no-default" {
            let origin = git(&widgets, &["remote", "get-url", "origin"]);
            git(
                std::path::Path::new(origin.trim()),
                &["symbolic-ref", "HEAD", "refs/heads/missing"],
            );
        } else {
            git(&widgets, &["update-ref", "-d", "refs/remotes/origin/main"]);
            script(
                &env.path("bin/git"),
                &format!(
                    r#"#!/bin/sh
stage=
for arg do
    case "$arg" in
        ls-remote) stage=ls-remote ;;
        fetch) stage=fallback ;;
        main|refs/heads/main) test "$stage" != fallback || stage=targeted ;;
    esac
done
if test "$stage" = '{stage}'; then
    echo 'fixture transport unavailable' >&2
    exit 1
fi
if test "$stage" = targeted; then
    exec '{git}' "$@" --refmap=
fi
exec '{git}' "$@"
"#,
                    git = real_git.trim()
                ),
            );
        }
        let record = env.path(&format!("state/{inst}/instance.json"));
        let before = fs::read(&record).unwrap();
        let err = env.fails(&["create", &format!("{inst}/helper"), "--repo", "widgets"]);
        assert!(err.contains("widgets"), "{stage}: {err}");
        if stage != "no-default" {
            assert!(err.contains("fixture transport unavailable"), "{err}");
        }
        assert_eq!(fs::read(&record).unwrap(), before, "{stage} issued an ID");
        env.ok(&["stop", &inst]);
    }
}

#[test]
fn repo_worktree_branch_conflicts_identify_the_selected_clone_without_recommending_repo() {
    let env = repositories_env("main");
    let inst = env.create("oncall", "work/a");
    repo_copy(&env, &inst, "widgets", Some("topic"));
    let err = env.fails(&[
        "create",
        &format!("{inst}/helper"),
        "--repo",
        "widgets",
        "--branch",
        "topic",
    ]);
    assert!(
        err.contains(&env.path("work/a/widgets").display().to_string()),
        "{err}"
    );
    assert!(!err.contains("--repo"), "{err}");
    env.ok(&["reset", &inst]);
}

fn remote_advance_during_fetch(fallback: bool) {
    let env = repositories_env("main");
    let inst = env.create("oncall", "work/a");
    let widgets = env.path("work/a/widgets");
    let gadgets = env.path("work/a/gadgets");
    let fetched = git(&widgets, &["rev-parse", "HEAD"]);
    git(
        &gadgets,
        &["commit", "-q", "--allow-empty", "-m", "remote advance"],
    );
    let advanced = git(&gadgets, &["rev-parse", "HEAD"]);
    if fallback {
        git(&widgets, &["update-ref", "-d", "refs/remotes/origin/main"]);
    }
    let real_git = common::command("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    // Once the direct fetch has a reachable tracking ref, origin becomes
    // unavailable: creation must not require another network operation.
    script(
        &env.path("bin/git"),
        &format!(
            r#"#!/bin/sh
fetch=false
for arg do
    test "$arg" != fetch || fetch=true
    last=$arg
done
if "$fetch"; then
    case "$last" in main|refs/heads/main)
        '{git}' "$@" {refmap} || exit $?
        '{git}' -C '{gadgets}' push -q origin main || exit $?
        if {advance_tracking}; then
            '{git}' -C '{widgets}' fetch --no-write-fetch-head origin || exit $?
        fi
        exit 0
        ;;
    esac
    if {advance_tracking}; then
        echo 'fixture origin is now unavailable' >&2
        exit 1
    fi
fi
exec '{git}' "$@"
"#,
            git = String::from_utf8_lossy(&real_git.stdout).trim(),
            gadgets = gadgets.display(),
            widgets = widgets.display(),
            refmap = if fallback { "--refmap=" } else { "" },
            advance_tracking = !fallback,
        ),
    );
    let copy = repo_copy(&env, &inst, "widgets", None);
    assert_eq!(git(&env.worktree(&copy), &["rev-parse", "HEAD"]), fetched);
    assert_eq!(git(&widgets, &["rev-parse", "origin/main"]), advanced);
    env.ok(&["delete", &copy]);
    assert!(!env.worktree(&copy).exists());
    env.ok(&["stop", &inst]);
}

#[test]
fn repo_fetch_accepts_a_default_ancestor_of_the_tracking_branch() {
    remote_advance_during_fetch(false);
}

#[test]
fn repo_fallback_fetch_accepts_a_default_ancestor_of_the_tracking_branch() {
    remote_advance_during_fetch(true);
}

#[test]
fn worktree_deletion_preserves_root_repository_git_errors() {
    let env = copies_env(2);
    let inst = env.create("oncall", "work/a");
    let copy = env.copy(&inst);
    env.ok(&["stop", &inst]);
    let config = env.path("work/a/.git/config");
    let before = fs::read(&config).unwrap();
    fs::write(&config, "[invalid config\n").unwrap();
    let err = env.fails(&["delete", &format!("{inst}/{copy}")]);
    assert!(err.contains("bad config"), "{err}");
    assert!(env.worktree(&format!("{inst}/{copy}")).exists());
    fs::write(&config, before).unwrap();
    env.ok(&["delete", &format!("{inst}/{copy}")]);
}

#[test]
fn repo_copies_fetch_the_default_without_changing_dirty_clones_and_delete_without_force() {
    for default in ["main", "master"] {
        let env = repositories_env(default);
        let widgets = env.path("work/a/widgets");
        let gadgets = env.path("work/a/gadgets");
        git(&widgets, &["switch", "-q", "-c", "personal"]);
        fs::write(widgets.join("README"), "staged\n").unwrap();
        git(&widgets, &["add", "README"]);
        fs::write(widgets.join("README"), "unstaged\n").unwrap();
        fs::write(widgets.join("draft"), "untracked\n").unwrap();
        let paths = [
            "README",
            "draft",
            ".git/index",
            ".git/HEAD",
            ".git/refs/heads/personal",
        ];
        let before: Vec<_> = paths
            .iter()
            .map(|p| fs::read(widgets.join(p)).unwrap())
            .collect();
        git(
            &gadgets,
            &["commit", "-q", "--allow-empty", "-m", "remote advance"],
        );
        git(&gadgets, &["push", "-q", "origin", default]);
        let tip = git(&gadgets, &["rev-parse", "HEAD"]);
        let inst = env.create("oncall", "work/a");
        let copy = repo_copy(&env, &inst, "widgets", None);
        assert_eq!(git(&env.worktree(&copy), &["rev-parse", "HEAD"]), tip);
        assert_eq!(
            git(&widgets, &["rev-parse", &format!("origin/{default}")]),
            tip
        );
        let after: Vec<_> = paths
            .iter()
            .map(|p| fs::read(widgets.join(p)).unwrap())
            .collect();
        assert_eq!(
            after, before,
            "fetch or worktree add changed the owner's checkout"
        );
        env.ok(&["delete", &copy]);
        assert!(!env.worktree(&copy).exists());
        env.ok(&["stop", &inst]);
    }
}

#[test]
fn repo_selection_uses_the_renamed_remote_default_instead_of_stale_origin_head() {
    let env = repositories_env("main");
    let gadgets = env.path("work/a/gadgets");
    git(&gadgets, &["switch", "-q", "-c", "trunk"]);
    git(
        &gadgets,
        &["commit", "-q", "--allow-empty", "-m", "new default"],
    );
    git(&gadgets, &["push", "-q", "origin", "trunk"]);
    let origin = git(&gadgets, &["remote", "get-url", "origin"]);
    git(
        std::path::Path::new(origin.trim()),
        &["symbolic-ref", "HEAD", "refs/heads/trunk"],
    );
    let inst = env.create("oncall", "work/a");
    let copy = repo_copy(&env, &inst, "widgets", None);
    assert_eq!(
        git(&env.worktree(&copy), &["rev-parse", "HEAD"]),
        git(&gadgets, &["rev-parse", "HEAD"])
    );
    env.ok(&["reset", &inst]);
}

#[test]
fn derived_members_inherit_the_copy_commit_even_after_the_remote_moves() {
    let env = repositories_env("main");
    let inst = env.create("oncall", "work/a");
    let copy = repo_copy(&env, &inst, "widgets", Some("topic"));
    let other = repo_copy(&env, &inst, "gadgets", Some("topic"));
    let folder = env.worktree(&copy);
    git(
        &folder,
        &["commit", "-q", "--allow-empty", "-m", "copy work"],
    );
    let tip = git(&folder, &["rev-parse", "HEAD"]);
    let gadgets = env.path("work/a/gadgets");
    git(
        &gadgets,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "separate remote work",
        ],
    );
    git(&gadgets, &["push", "-q", "origin", "main"]);
    // Rewriting the copy's git file must not redirect the inherited HEAD.
    fs::write(
        folder.join(".git"),
        format!("gitdir: {}\n", gadgets.join(".git").display()),
    )
    .unwrap();
    env.ok(&["start", &copy]);
    env.ok(&["create", &format!("{copy}/helper")]);
    let member = env.member(&copy, "helper");
    let derived = env.worktree(&format!("{copy}/{member}"));
    assert_eq!(git(&derived, &["rev-parse", "HEAD"]), tip);
    assert_eq!(
        git(&derived, &["rev-parse", "--git-common-dir"]).trim(),
        env.path("work/a/widgets/.git").display().to_string()
    );
    env.ok(&["stop", &inst]);
    env.ok(&["delete", &format!("{copy}/{member}"), "--force"]);
    let err = env.fails(&["delete", &copy]);
    assert!(err.contains("commit(s) on no remote"), "{err}");
    assert!(folder.exists());
    env.ok(&["delete", &other]);
}

#[test]
fn invalid_repo_sources_and_unreachable_origins_issue_nothing() {
    let env = repositories_env("main");
    std::os::unix::fs::symlink(env.path("work/a/widgets"), env.path("work/a/alias")).unwrap();
    fs::create_dir(env.path("work/a/git-alias")).unwrap();
    std::os::unix::fs::symlink(
        env.path("work/a/widgets/.git"),
        env.path("work/a/git-alias/.git"),
    )
    .unwrap();
    git(
        env.dir.path(),
        &[
            "init",
            "-q",
            "--bare",
            &env.path("work/a/bare").display().to_string(),
        ],
    );
    env.checkout("work/a/no-origin");
    git(
        &env.path("work/a/no-origin"),
        &["remote", "remove", "origin"],
    );
    env.checkout("work/a/unreachable");
    git(
        &env.path("work/a/unreachable"),
        &[
            "remote",
            "set-url",
            "origin",
            &env.path("missing-origin").display().to_string(),
        ],
    );
    let inst = env.create("oncall", "work/a");
    let copy = repo_copy(&env, &inst, "widgets", None);
    env.ok(&["delete", &copy]);
    let record = env.path(&format!("state/{inst}/instance.json"));
    let before = fs::read(&record).unwrap();
    let absolute = env.path("work/a/widgets").display().to_string();
    for name in [
        "missing",
        "",
        ".",
        "a/b",
        "..",
        &absolute,
        "widgets/",
        "alias",
        "git-alias",
        "linked",
        "bare",
        "scratch",
        "no-origin",
        "unreachable",
    ] {
        env.fails(&["create", &format!("{inst}/helper"), "--repo", name]);
        assert_eq!(fs::read(&record).unwrap(), before, "{name} issued an ID");
    }
    env.ok(&["stop", &inst]);
}

#[test]
fn repo_cleanup_and_reset_remove_clean_worktrees_and_keep_dirty_ones_in_both_clones() {
    let env = repositories_env("main");
    let inst = env.create("oncall", "work/a");
    let mut clean = Vec::new();
    let mut dirty = Vec::new();
    for repo in ["widgets", "gadgets"] {
        let copy = repo_copy(&env, &inst, repo, None);
        fs::write(env.worktree(&copy).join("draft"), "unsaved\n").unwrap();
        env.ok(&["delete", &copy, "--force"]);
        env.ok(&["cleanup", &inst]);
        assert!(env.worktree(&copy).exists());
        fs::remove_file(env.worktree(&copy).join("draft")).unwrap();
        clean.push(env.worktree(&copy));
        let copy = repo_copy(&env, &inst, repo, None);
        clean.push(env.worktree(&copy));
        let copy = repo_copy(&env, &inst, repo, None);
        fs::write(env.worktree(&copy).join("draft"), "unsaved\n").unwrap();
        dirty.push(env.worktree(&copy));
    }
    env.ok(&["cleanup", &inst]);
    for folder in clean.iter().step_by(2) {
        assert!(!folder.exists());
    }
    env.ok(&["reset", &inst]);
    assert!(clean.iter().all(|p| !p.exists()));
    assert!(dirty.iter().all(|p| p.join("draft").is_file()));
}

#[test]
fn repo_selection_refuses_ambiguous_worktree_ownership() {
    let env = repositories_env("main");
    let inst = env.create("oncall", "work/a");
    let copy = repo_copy(&env, &inst, "widgets", None);
    let forged = env.path("work/a/gadgets/.git/worktrees/forged");
    fs::create_dir_all(&forged).unwrap();
    fs::write(
        forged.join("gitdir"),
        env.worktree(&copy).join(".git").display().to_string(),
    )
    .unwrap();
    let err = env.fails(&["delete", &copy]);
    assert!(err.contains("widgets") && err.contains("gadgets"), "{err}");
    assert!(env.worktree(&copy).exists());
    fs::remove_dir_all(forged).unwrap();
    env.ok(&["delete", &copy]);
    env.ok(&["stop", &inst]);
}

#[test]
fn repo_members_can_select_a_clone_at_any_depth_but_shared_roles_cannot() {
    let env = repositories_env("main");
    let inst = env.create("oncall", "work/a");
    let copy = repo_copy(&env, &inst, "widgets", None);
    env.ok(&["start", &copy]);
    let out = env.ok(&[
        "create",
        &format!("{copy}/helper"),
        "--repo",
        "gadgets",
        "--branch",
        "other",
    ]);
    let member = env.member(&copy, "helper");
    let folder = env.worktree(&format!("{copy}/{member}"));
    let head = git(&folder, &["rev-parse", "HEAD"]);
    assert!(
        stdout(&out).contains(&format!("· gadgets at {}", &head[..7])),
        "{}",
        both(&out)
    );
    assert_eq!(
        git(&folder, &["rev-parse", "--git-common-dir"]).trim(),
        env.path("work/a/gadgets/.git").display().to_string()
    );
    env.fails(&["create", "oncall", "--repo", "widgets"]);
    env.fails(&["create", &format!("{inst}/scout"), "--repo", "widgets"]);
    assert!(env.member(&inst, "scout").is_empty());
    env.ok(&["reset", &inst]);
}

#[test]
fn repo_fetch_preserves_credentials_without_blocking_other_registry_changes() {
    for agent in [false, true] {
        let env = repositories_env("main");
        let inst = env.create("oncall", "work/a");
        let parent = if agent {
            let copy = repo_copy(&env, &inst, "gadgets", None);
            env.ok(&["start", &copy]);
            copy
        } else {
            inst.clone()
        };
        let origin = git(
            &env.path("work/a/widgets"),
            &["remote", "get-url", "origin"],
        );
        // Credential setups commonly depend on global git configuration.
        // This URL rewrite has the same environment requirement, with no
        // credentials and no network in the fixture.
        env.write(
            "credential-config",
            &format!(
                "[url \"{}\"]\n    insteadOf = fixture-origin\n",
                origin.trim()
            ),
        );
        git(
            &env.path("work/a/widgets"),
            &["remote", "set-url", "origin", "fixture-origin"],
        );
        let real_git = common::command("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap();
        let gate = env.path("fetch-gate");
        script(
            &env.path("bin/git"),
            &format!(
                r#"#!/bin/sh
network=false
fetch=false
for arg do
    case "$arg" in ls-remote|fetch) network=true ;; esac
    case "$arg" in fetch) fetch=true ;; esac
    last=$arg
done
if "$network"; then
    test "$GIT_TERMINAL_PROMPT" = 0 || exit 41
    test "$GIT_SSH_COMMAND" = fixture-ssh || exit 42
    test "$GIT_ASKPASS" = fixture-askpass || exit 43
    test -z "${{GIT_DIR+x}}${{GIT_WORK_TREE+x}}${{GIT_INDEX_FILE+x}}${{GIT_OBJECT_DIRECTORY+x}}${{GIT_COMMON_DIR+x}}${{GIT_NAMESPACE+x}}" || exit 44
    touch '{gate}.waiting'
    while test ! -f '{gate}'; do sleep 0.05; done
fi
# An explicit fetch with an empty refmap leaves only FETCH_HEAD. The
# fallback fetch must populate the tracking ref so deletion is safe.
if "$fetch" && test "$last" = refs/heads/main; then
    exec '{git}' "$@" --refmap=
fi
exec '{git}' "$@"
"#,
                gate = gate.display(),
                git = String::from_utf8_lossy(&real_git.stdout).trim()
            ),
        );
        // Remove the stale tracking ref so that a successful delete pins
        // the fallback, rather than merely finding the old reachable tip.
        git(
            &env.path("work/a/widgets"),
            &["update-ref", "-d", "refs/remotes/origin/main"],
        );
        let mut create = env.cmd(&["create", &format!("{parent}/helper"), "--repo", "widgets"]);
        create
            .env("GIT_CONFIG_GLOBAL", env.path("credential-config"))
            .env("GIT_SSH_COMMAND", "fixture-ssh")
            .env("GIT_ASKPASS", "fixture-askpass")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_COMMON_DIR",
            "GIT_NAMESPACE",
        ] {
            create.env(key, "/nonexistent/caller-repository");
        }
        let mut create = create.spawn().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !gate.with_extension("waiting").exists() && std::time::Instant::now() < deadline {
            if create.try_wait().unwrap().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if !gate.with_extension("waiting").exists() {
            let _ = create.kill();
            panic!(
                "create failed before reaching origin: {}",
                both(&create.wait_with_output().unwrap())
            );
        }
        let mut other = env
            .cmd(&["create", &format!("{inst}/scout")])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut finished = None;
        while std::time::Instant::now() < deadline {
            finished = other.try_wait().unwrap();
            if finished.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        fs::write(&gate, "release").unwrap();
        let out = create.wait_with_output().unwrap();
        assert!(out.status.success(), "{}", both(&out));
        if finished.is_none() {
            other.kill().unwrap();
        }
        other.wait().unwrap();
        assert!(
            finished.is_some_and(|status| status.success()),
            "fetch held the registry lock"
        );
        let selected = if agent {
            format!("{parent}/{}", env.member(&parent, "helper"))
        } else {
            stdout(&out).split_whitespace().next().unwrap().to_string()
        };
        env.ok(&["stop", &inst]);
        env.ok(&["delete", &selected]);
        env.ok(&["reset", &inst]);
    }
}
