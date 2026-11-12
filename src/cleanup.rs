//! `brg cleanup [<team>]`: remove what brg left behind for members and
//! child teams that no longer exist.
//!
//! brg removes only what it can find from its own records, never anything
//! matched by name alone:
//! - **Worktrees it added**: the git worktree of each deleted member and
//!   child team of a `worktree: true` role, at its fixed place beside brg's state
//!   folder ([`crate::worktree::path`]). It is removed with `git worktree
//!   remove` when it is clean: nothing modified, staged or untracked, and no
//!   commit that no remote-tracking ref reaches, and when no registered
//!   team, kept member or child team works at or inside it. A worktree with work
//!   in it is listed and kept; a worktree git can't check fails the run.
//! - **State of deleted members and child teams** inside a registered team's
//!   state folder, as an interrupted `delete` leaves it.
//! - **Unregistered team state** in brg's state folder: a folder named
//!   for a team ID brg issued, with no registered team (without an
//!   team argument only).
//!
//! Work folders, kept members' and child teams' folders, repositories, other
//! worktrees and messages are never touched. `brg destroy` applies
//! the same folder rules to every member and child team of the tree it forgets
//! ([`tree_folders`]).

use std::path::{Path, PathBuf};

use crate::address::{self, Instance};
use crate::lifecycle;
use crate::paths::resolve_existing_prefix;
use crate::registry::{self, IssuedMember};
use crate::state::{self, StateDir};

/// What one cleanup did, line by line, and whether anything failed.
#[derive(Debug, Default)]
pub struct Outcome {
    pub lines: Vec<String>,
    pub failed: bool,
}

impl Outcome {
    fn removed(&mut self, path: &Path, what: &str) {
        self.lines
            .push(format!("removed {} ({what})", path.display()));
    }

    fn kept(&mut self, path: &Path, why: &str) {
        self.lines.push(format!("left {}: {why}", path.display()));
    }

    fn error(&mut self, text: String) {
        self.failed = true;
        self.lines.push(text);
    }
}

/// Clean up under `only` and its child teams, or everywhere.
pub fn cleanup(only: Option<&Instance>) -> Result<Outcome, String> {
    let lock = registry::lock()?;
    let registry = lock.load()?;
    let mut out = Outcome::default();
    let held = held(&registry, |_| true);
    let ids: Vec<&String> = registry
        .instances
        .keys()
        .filter(|id| {
            only.is_none_or(|inst| *id == &inst.id || id.starts_with(&format!("{}/", inst.id)))
        })
        .collect();
    for id in ids {
        match address::instance(id) {
            Ok(inst) => instance(&inst, &registry, &held, &mut out),
            Err(e) => out.error(format!("skipped {id}: {e}")),
        }
    }
    if only.is_none() {
        unregistered(&registry, &mut out);
    }
    drop(lock);
    if out.lines.is_empty() {
        out.lines.push("nothing to clean up".into());
    }
    Ok(out)
}

/// Every folder a registered team (of those `counts`) works in: its own
/// folder and its kept members' and child teams' folders. No leaf holding one is
/// removed.
fn held(registry: &registry::Registry, counts: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let mut held = Vec::new();
    for (id, entry) in registry.instances.iter().filter(|(id, _)| counts(id)) {
        held.push(entry.folder.clone());
        if let Ok(inst) = address::instance(id) {
            held.extend(
                inst.record
                    .kept_members()
                    .chain(inst.record.copies.iter().filter(|(_, c)| !c.deleted))
                    .filter_map(|(_, m)| m.cwd.clone()),
            );
        }
    }
    held
}

/// `destroy`'s folder pass over a tree about to be forgotten, deepest first:
/// every member's and child team's worktree, kept or deleted, by the same rules as
/// cleanup. Folders that teams outside the tree work in stay protected.
pub(crate) fn tree_folders(tree: &[Instance], registry: &registry::Registry) -> Outcome {
    let mut out = Outcome::default();
    let held = held(registry, |id| !tree.iter().any(|inst| inst.id == id));
    for inst in tree {
        let members = issued(inst, &inst.record.members, |_| true, &mut out);
        let copies = issued(inst, &inst.record.copies, |_| true, &mut out);
        for (id, kind) in members
            .into_iter()
            .map(|(id, _)| (id, "member"))
            .chain(copies.into_iter().map(|(id, _)| (id, "child team")))
        {
            worktree(
                root_of(registry, inst),
                &inst.at(id),
                &held,
                &format!("{kind} {}", inst.at(id)),
                &mut out,
            );
        }
    }
    out
}

/// One registered team: its deleted members' and child teams' folders and
/// state.
fn instance(inst: &Instance, registry: &registry::Registry, held: &[PathBuf], out: &mut Outcome) {
    let _state_lock = match lifecycle::lock_state(&inst.state) {
        Ok(lock) => lock,
        Err(e) => return out.error(format!("skipped {}: {e}", inst.id)),
    };
    let deleted_members = issued(inst, &inst.record.members, |m| m.deleted, out);
    let deleted_copies: Vec<(&String, &IssuedMember)> =
        issued(inst, &inst.record.copies, |m| m.deleted, out)
            .into_iter()
            .filter(|(id, _)| !registry.instances.contains_key(&inst.at(id)))
            .collect();
    if deleted_members.is_empty() && deleted_copies.is_empty() {
        return;
    }
    for (id, _) in &deleted_members {
        let what = format!("deleted member {}", inst.at(id));
        worktree(root_of(registry, inst), &inst.at(id), held, &what, out);
    }
    for (id, _) in &deleted_copies {
        let what = format!("deleted child team {}", inst.at(id));
        worktree(root_of(registry, inst), &inst.at(id), held, &what, out);
    }
    for (id, _) in &deleted_members {
        let what = format!("state of deleted member {}", inst.at(id));
        for path in [inst.state.member_dir(id), inst.state.member_tmp(id)] {
            remove_state(&path, &what, out);
        }
        for path in [inst.state.exec_file(id), inst.state.lock_file(id)] {
            if path.symlink_metadata().is_ok() {
                match std::fs::remove_file(&path) {
                    Ok(()) => out.removed(&path, &what),
                    Err(e) => out.error(format!("cannot remove {}: {e}", path.display())),
                }
            }
        }
    }
    for (id, _) in &deleted_copies {
        let what = format!("state of deleted child team {}", inst.at(id));
        remove_state(&StateDir::new(&inst.at(id)).root, &what, out);
    }
}

/// The folder of the top team of `inst`'s tree, as registered.
/// `None` when it isn't registered: then no worktree in the tree is touched.
fn root_of<'a>(registry: &'a registry::Registry, inst: &Instance) -> Option<&'a Path> {
    let root = inst.id.split('/').next().unwrap_or_default();
    registry
        .instances
        .get(root)
        .map(|entry| entry.folder.as_path())
}

/// The entries of `issued` that `pick` keeps. An ID that isn't one brg
/// issues (a damaged record) is reported and never turned into a path.
fn issued<'a>(
    inst: &Instance,
    issued: &'a std::collections::BTreeMap<String, IssuedMember>,
    pick: impl Fn(&IssuedMember) -> bool,
    out: &mut Outcome,
) -> Vec<(&'a String, &'a IssuedMember)> {
    let mut found = Vec::new();
    for (id, member) in issued.iter().filter(|(_, m)| pick(m)) {
        if registry::is_id_of(id, &member.role) {
            found.push((id, member));
        } else {
            out.error(format!(
                "skipped `{id}` in {}'s record: not an ID brg issues",
                inst.id
            ));
        }
    }
    found
}

/// Remove the worktree brg added for the member or child team at `address`, in the
/// tree whose top folder is `root`, if
/// it is there, clean, and nobody works in it; otherwise say why it stays.
fn worktree(root: Option<&Path>, address: &str, held: &[PathBuf], what: &str, out: &mut Outcome) {
    let dir = crate::worktree::path(address);
    if dir.symlink_metadata().is_err() {
        return;
    }
    let Some(root) = root else {
        return out.error(format!(
            "can't check git worktree {}, so it was left as it is ({what}): its tree's top team isn't registered",
            dir.display()
        ));
    };
    let real = resolve_existing_prefix(&dir);
    if held
        .iter()
        .any(|held| resolve_existing_prefix(held).starts_with(&real))
    {
        return out.kept(
            &dir,
            &format!("a team, kept member or child team works there ({what})"),
        );
    }
    match crate::worktree::unsaved(root, &dir) {
        Ok(None) => match crate::worktree::remove(root, &dir) {
            Ok(()) => out.removed(&dir, &format!("clean git worktree of {what}")),
            Err(e) => out.error(format!("cannot remove git worktree {}: {e}", dir.display())),
        },
        Ok(Some(why)) => out.kept(
            &dir,
            &format!("git worktree with {why} ({what}); left as it is"),
        ),
        Err(e) => out.error(format!(
            "can't check git worktree {}, so it was left as it is ({what}): {e}",
            dir.display()
        )),
    }
}

/// Why the git worktree at `dir` isn't clean, or `None` when it is:
/// - no other repository or worktree inside it, even in ignored folders
///   (`git worktree remove` would delete it with the rest);
/// - no modified, staged or untracked (non-ignored) files, and no file git
///   is told to skip or assume unchanged;
/// - no commit that no remote-tracking ref reaches, from its HEAD, its HEAD
///   reflog or its own `refs/worktree/` refs.
///
/// Every check runs against `admin`, the worktree's own folder in the
/// repository (from [`crate::worktree::admin_dir`]), never through `dir`'s
/// `.git` file, which whoever works in `dir` can point at a repository whose
/// configuration runs commands; and with no hooks or file-system monitor.
pub(crate) fn clean_worktree(admin: &Path, dir: &Path) -> Result<Option<String>, String> {
    if nested_git(dir).map_err(|e| format!("cannot read it: {e}"))? {
        return Ok(Some("another repository or worktree inside it".into()));
    }
    let trusted: Vec<String> = [
        "--git-dir".to_string(),
        admin.display().to_string(),
        "--work-tree".to_string(),
        dir.display().to_string(),
    ]
    .into_iter()
    .chain(HARDENED.iter().map(|s| s.to_string()))
    .collect();
    let git = |dir: &Path, args: &[&str]| git_with(dir, &trusted, args);
    let top = git(dir, &["rev-parse", "--show-toplevel"])?;
    let same = |a: &Path, b: &Path| a.canonicalize().ok() == b.canonicalize().ok();
    if !same(Path::new(top.trim()), dir) {
        return Err(format!("git places its top at {}", top.trim()));
    }
    let status = git(
        dir,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain",
            "--untracked-files=normal",
        ],
    )?;
    if !status.trim().is_empty() {
        return Ok(Some("uncommitted or untracked changes".into()));
    }
    let tagged = git(dir, &["ls-files", "-v"])?;
    if tagged.lines().any(|line| !line.starts_with("H ")) {
        return Ok(Some("files git is told to skip or assume unchanged".into()));
    }
    let mut tips = git(dir, &["rev-parse", "HEAD"])?;
    tips.push_str(&git(dir, &["log", "-g", "--format=%H", "HEAD"])?);
    tips.push_str(&git(
        dir,
        &["for-each-ref", "--format=%(objectname)", "refs/worktree/"],
    )?);
    let tips: std::collections::BTreeSet<&str> = tips.split_whitespace().collect();
    let mut args = vec!["rev-list", "--count"];
    args.extend(tips);
    args.extend(["--not", "--remotes"]);
    let unpushed = git(dir, &args)?;
    match unpushed.trim() {
        "0" => Ok(None),
        n => Ok(Some(format!("{n} commit(s) on no remote"))),
    }
}

/// Whether a `.git` (file or folder) sits anywhere below `dir`'s top,
/// never following a symlink.
fn nested_git(dir: &Path) -> std::io::Result<bool> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in std::fs::read_dir(&next)? {
            let entry = entry?;
            if next != dir && entry.file_name() == ".git" {
                return Ok(true);
            }
            if entry.file_type()?.is_dir() && !(next == dir && entry.file_name() == ".git") {
                stack.push(entry.path());
            }
        }
    }
    Ok(false)
}

/// How long one git command may take before it is stopped and its check
/// counts as failed.
const GIT_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);

/// git settings that keep a command from running what a repository's
/// configuration names: no hooks and no file-system monitor.
pub(crate) const HARDENED: [&str; 4] = [
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.hooksPath=/dev/null",
];

/// Run git in `dir`, with git options `pre` before the command and none of
/// the caller's `GIT_*` settings, within [`GIT_LIMIT`]; its output, or why
/// it failed.
pub(crate) fn git_with(dir: &Path, pre: &[String], args: &[&str]) -> Result<String, String> {
    let mut command = std::process::Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    run_git(&mut command, dir, pre, args)
}

/// Reach origin using the caller's credential configuration, without letting
/// its environment redirect git to another repository or index.
pub(crate) fn git_network(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = std::process::Command::new("git");
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_COMMON_DIR",
        "GIT_NAMESPACE",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_SHALLOW_FILE",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    ] {
        command.env_remove(key);
    }
    command.env("GIT_TERMINAL_PROMPT", "0");
    let hardened = HARDENED.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    run_git(&mut command, dir, &hardened, args)
}

fn run_git(
    command: &mut std::process::Command,
    dir: &Path,
    pre: &[String],
    args: &[&str],
) -> Result<String, String> {
    // Untranslated messages: callers tell "not a git repository" from other
    // failures by git's English text.
    command.env("LC_ALL", "C");
    command.arg("-C").arg(dir).args(pre).args(args);
    let what = args
        .iter()
        .find(|arg| !arg.starts_with('-'))
        .copied()
        .unwrap_or_default();
    let limits = crate::bounded_process::Limits {
        deadline: std::time::Instant::now() + GIT_LIMIT,
        stdout_bytes: 64 << 20,
        stderr_bytes: 64 << 10,
    };
    let output = crate::bounded_process::run(command, None, limits).map_err(|e| match e {
        crate::bounded_process::Error::Deadline => {
            format!(
                "git {what} did not finish within {} seconds",
                GIT_LIMIT.as_secs()
            )
        }
        e => format!("cannot run git {what}: {e:?}"),
    })?;
    if !output.status.success() {
        return Err(format!(
            "git {what} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Remove a state folder brg owns, if it is there.
fn remove_state(path: &Path, what: &str, out: &mut Outcome) {
    let Ok(meta) = path.symlink_metadata() else {
        return;
    };
    let removed = if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match removed {
        Ok(()) => out.removed(path, what),
        Err(e) => out.error(format!("cannot remove {}: {e}", path.display())),
    }
}

/// State folders in brg's state folder named for a team ID brg
/// issued that is no longer registered.
fn unregistered(registry: &registry::Registry, out: &mut Outcome) {
    let home = state::state_home();
    let Ok(entries) = std::fs::read_dir(&home) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let issued = registry::split_id(&name).is_some_and(|(team, suffix)| {
            registry
                .issued
                .get(team)
                .is_some_and(|suffixes| suffixes.contains(suffix))
        });
        if issued
            && !registry.instances.contains_key(&name)
            && entry.file_type().is_ok_and(|t| t.is_dir())
        {
            remove_state(
                &entry.path(),
                &format!("state of unregistered team {name}"),
                out,
            );
        }
    }
}
