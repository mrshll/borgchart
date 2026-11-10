//! `brg cleanup [<instance>]`: remove what brg left behind for members and
//! copies that no longer exist.
//!
//! brg removes only what it can find from its own records, never anything
//! matched by name alone:
//! - **Folders it made**: the `{member_id}` leaf of each deleted member and
//!   copy (its role's `cwd` in the instance's folder). A leaf is removed only
//!   when it holds nothing but folders, and when no registered instance, kept
//!   member or copy works at or inside it. A git worktree in it (a `.git`
//!   file) is removed with `git worktree remove` when it is clean: nothing
//!   modified, staged or untracked, and no commit that no remote-tracking ref
//!   reaches. A worktree with work in it, a repository (a `.git` folder) and
//!   other files are listed and kept; a worktree git can't check fails the
//!   run. Leaves are placed through the instance's current team file.
//! - **State of deleted members and copies** inside a registered instance's
//!   state folder, as an interrupted `delete` leaves it.
//! - **Unregistered instance state** in brg's state folder: a folder named
//!   for an instance ID brg issued, with no registered instance (without an
//!   instance argument only).
//!
//! Work folders, kept members' and copies' folders, repositories, worktrees
//! outside those leaves and messages are never touched. `brg reset` applies
//! the same folder rules to every member and copy of the tree it forgets
//! ([`tree_folders`]).

use std::path::{Path, PathBuf};

use crate::address::{self, Instance};
use crate::paths::resolve_existing_prefix;
use crate::registry::{self, IssuedMember};
use crate::state::{self, StateDir};
use crate::{lifecycle, runtime};

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

/// Clean up under `only` and its copies, or everywhere.
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

/// Every folder a registered instance (of those `counts`) works in: its own
/// folder and its kept members' and copies' folders. No leaf holding one is
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

/// `reset`'s folder pass over a tree about to be forgotten, deepest first:
/// every member's and copy's `{member_id}` folder, kept or deleted, by the
/// same rules as cleanup. A kept one is found where its latest start (or a
/// copy's creation) recorded it, else through the team file. Folders that
/// instances outside the tree work in stay protected.
pub(crate) fn tree_folders(tree: &[Instance], registry: &registry::Registry) -> Outcome {
    let mut out = Outcome::default();
    let held = held(registry, |id| !tree.iter().any(|inst| inst.id == id));
    for inst in tree {
        let team = runtime::validated(inst);
        let mut unplaced = false;
        let entries = issued(inst, &inst.record.members, |_| true, &mut out)
            .into_iter()
            .map(|(id, m)| (id, m, "member"))
            .chain(
                issued(inst, &inst.record.copies, |_| true, &mut out)
                    .into_iter()
                    .map(|(id, c)| (id, c, "copy")),
            )
            .collect::<Vec<_>>();
        for (id, issued, kind) in entries {
            let what = format!("{kind} {}", inst.at(id));
            let recorded = issued
                .cwd
                .as_ref()
                .filter(|cwd| cwd.file_name().is_some_and(|name| name == id.as_str()));
            match (recorded, &team) {
                (Some(dir), _) => leaf_at(inst, dir, &held, &what, &mut out),
                (None, Ok(v)) => {
                    let cwd = if kind == "member" {
                        v.team
                            .roles
                            .get(&issued.role)
                            .and_then(|r| r.cwd.as_deref())
                    } else {
                        v.team.team_roles.get(&issued.role).map(|r| r.cwd.as_str())
                    };
                    leaf(inst, cwd, id, &held, &what, &mut out);
                }
                (None, Err(_)) => unplaced = true,
            }
        }
        if unplaced && let Err(e) = &team {
            out.error(format!(
                "can't place some folders brg made for {}'s members and copies, because its team can't be loaded ({}); remove any left empty by hand",
                inst.id,
                e.split(';').next().unwrap_or(e)
            ));
        }
    }
    out
}

/// One registered instance: its deleted members' and copies' folders and
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
    match runtime::validated(inst) {
        Ok(v) => {
            for (id, member) in &deleted_members {
                let cwd = v
                    .team
                    .roles
                    .get(&member.role)
                    .and_then(|r| r.cwd.as_deref());
                leaf(
                    inst,
                    cwd,
                    id,
                    held,
                    &format!("deleted member {}", inst.at(id)),
                    out,
                );
            }
            for (id, copy) in &deleted_copies {
                let cwd = v.team.team_roles.get(&copy.role).map(|r| r.cwd.as_str());
                leaf(
                    inst,
                    cwd,
                    id,
                    held,
                    &format!("deleted copy {}", inst.at(id)),
                    out,
                );
            }
        }
        Err(e) => out.error(format!(
            "can't place the folders of {}'s deleted members and copies: {e}",
            inst.id
        )),
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
        let what = format!("state of deleted copy {}", inst.at(id));
        remove_state(&StateDir::new(&inst.at(id)).root, &what, out);
    }
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
        if crate::names::is_valid_member_id(id) && registry::split_id(id).is_some() {
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

/// The `{member_id}` leaf brg made for `id`, if its role's `cwd` has one.
fn leaf(
    inst: &Instance,
    cwd: Option<&str>,
    id: &str,
    held: &[PathBuf],
    what: &str,
    out: &mut Outcome,
) {
    let Some(cwd) = cwd.filter(|c| {
        Path::new(c)
            .file_name()
            .is_some_and(|name| name == "{member_id}")
    }) else {
        return;
    };
    let dir = inst.record.folder.join(cwd.replace("{member_id}", id));
    leaf_at(inst, &dir, held, what, out);
}

/// Remove the folder `dir` brg made, if it holds nothing but folders and
/// nobody works in it; otherwise say why it stays.
fn leaf_at(inst: &Instance, dir: &Path, held: &[PathBuf], what: &str, out: &mut Outcome) {
    let dir = dir.to_path_buf();
    let Ok(meta) = dir.symlink_metadata() else {
        return;
    };
    let folder = resolve_existing_prefix(&inst.record.folder);
    if !meta.is_dir() {
        return out.kept(&dir, &format!("not a folder ({what})"));
    }
    if !resolve_existing_prefix(&dir).starts_with(&folder) || dir == folder {
        return out.kept(&dir, &format!("outside the instance's folder ({what})"));
    }
    let real = resolve_existing_prefix(&dir);
    if held
        .iter()
        .any(|held| resolve_existing_prefix(held).starts_with(&real))
    {
        return out.kept(
            &dir,
            &format!("an instance, kept member or copy works there ({what})"),
        );
    }
    let mut found = contents(&dir);
    if let Ok(Contents::Worktrees(worktrees)) = &found {
        for worktree in worktrees {
            match clean_worktree(worktree) {
                Ok(None) => match git(
                    worktree,
                    &["worktree", "remove", &worktree.to_string_lossy()],
                ) {
                    Ok(_) => out.removed(
                        worktree,
                        &format!("clean git worktree in the folder of {what}"),
                    ),
                    Err(e) => out.error(format!(
                        "cannot remove git worktree {}: {e}",
                        worktree.display()
                    )),
                },
                Ok(Some(why)) => out.kept(
                    worktree,
                    &format!("git worktree with {why} ({what}); left as it is"),
                ),
                Err(e) => out.error(format!(
                    "can't check git worktree {}, so it was left as it is ({what}): {e}",
                    worktree.display()
                )),
            }
        }
        // A worktree at the folder's top took the folder with it.
        if dir.symlink_metadata().is_err() {
            return;
        }
        found = contents(&dir);
    }
    match found {
        Ok(Contents::Folders) => match remove_folders(&dir) {
            Ok(()) => out.removed(&dir, &format!("folder of {what}")),
            Err(e) => out.error(format!("cannot remove {}: {e}", dir.display())),
        },
        // Each worktree left was already reported above.
        Ok(Contents::Worktrees(_)) => {}
        Ok(Contents::Repository) => out.kept(&dir, &format!("holds a git repository ({what})")),
        Ok(Contents::Files) => out.kept(&dir, &format!("holds files ({what})")),
        Err(e) => out.error(format!("cannot read {}: {e}", dir.display())),
    }
}

enum Contents {
    Folders,
    Files,
    /// A `.git` folder somewhere inside: a clone, never removed.
    Repository,
    /// Git worktrees (a `.git` file at their top), by folder.
    Worktrees(Vec<PathBuf>),
}

/// What `dir` holds, never following a symlink. Worktrees aren't entered.
fn contents(dir: &Path) -> std::io::Result<Contents> {
    let mut files = false;
    let mut worktrees = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        let git = next.join(".git");
        match git.symlink_metadata() {
            Ok(meta) if meta.is_dir() => return Ok(Contents::Repository),
            Ok(meta) if meta.is_file() => {
                worktrees.push(next);
                continue;
            }
            _ => {}
        }
        for entry in std::fs::read_dir(&next)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                stack.push(entry.path());
            } else {
                files = true;
            }
        }
    }
    Ok(if !worktrees.is_empty() {
        Contents::Worktrees(worktrees)
    } else if files {
        Contents::Files
    } else {
        Contents::Folders
    })
}

/// Why the git worktree at `dir` isn't clean, or `None` when it is:
/// - no other repository or worktree inside it, even in ignored folders
///   (`git worktree remove` would delete it with the rest);
/// - no modified, staged or untracked (non-ignored) files, and no file git
///   is told to skip or assume unchanged;
/// - no commit that no remote-tracking ref reaches, from its HEAD, its HEAD
///   reflog or its own `refs/worktree/` refs.
fn clean_worktree(dir: &Path) -> Result<Option<String>, String> {
    if nested_git(dir).map_err(|e| format!("cannot read it: {e}"))? {
        return Ok(Some("another repository or worktree inside it".into()));
    }
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

/// Run git in `dir` with none of the caller's `GIT_*` settings, within
/// [`GIT_LIMIT`]; its output, or why it failed.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = std::process::Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command.arg("-C").arg(dir).args(args);
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
    let output = crate::bounded_process::run(&mut command, None, limits).map_err(|e| match e {
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

/// Remove a tree of empty folders bottom-up. Never recursive removal: a
/// file that appears meanwhile makes this fail and stay.
fn remove_folders(dir: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            remove_folders(&entry.path())?;
        }
    }
    std::fs::remove_dir(dir)
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

/// State folders in brg's state folder named for an instance ID brg
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
                &format!("state of unregistered instance {name}"),
                out,
            );
        }
    }
}
