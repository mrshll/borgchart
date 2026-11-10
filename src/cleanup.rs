//! `brg cleanup [<instance>]`: remove what brg left behind for members and
//! copies that no longer exist.
//!
//! brg removes only what it can find from its own records, never anything
//! matched by name alone:
//! - **Folders it made**: the `{member_id}` leaf of each deleted member and
//!   copy (its role's `cwd` in the instance's folder). A leaf is removed only
//!   when it holds nothing but folders; one holding files is listed and kept,
//!   and named as the lead's git worktree when it holds a `.git`.
//! - **State of deleted members and copies** inside a registered instance's
//!   state folder, as an interrupted `delete` leaves it.
//! - **Unregistered instance state** in brg's state folder: a folder named
//!   for an instance ID brg issued, with no registered instance (without an
//!   instance argument only).
//!
//! Work folders, kept members' and copies' folders, git worktrees and
//! messages are never touched.

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
    let ids: Vec<&String> = registry
        .instances
        .keys()
        .filter(|id| {
            only.is_none_or(|inst| *id == &inst.id || id.starts_with(&format!("{}/", inst.id)))
        })
        .collect();
    for id in ids {
        match address::instance(id) {
            Ok(inst) => instance(&inst, &registry, &mut out),
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

/// One registered instance: its deleted members' and copies' folders and
/// state.
fn instance(inst: &Instance, registry: &registry::Registry, out: &mut Outcome) {
    let _state_lock = match lifecycle::lock_state(&inst.state) {
        Ok(lock) => lock,
        Err(e) => return out.error(format!("skipped {}: {e}", inst.id)),
    };
    let deleted_members = deleted(&inst.record.members);
    let deleted_copies: Vec<(&String, &IssuedMember)> = deleted(&inst.record.copies)
        .into_iter()
        .filter(|(id, _)| !registry.instances.contains_key(&inst.at(id)))
        .collect();
    if deleted_members.is_empty() && deleted_copies.is_empty() {
        return;
    }
    match runtime::validated(inst) {
        Ok(v) => {
            let kept: Vec<PathBuf> = inst
                .record
                .kept_members()
                .chain(inst.record.copies.iter().filter(|(_, c)| !c.deleted))
                .filter_map(|(_, m)| m.cwd.clone())
                .collect();
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
                    &kept,
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
                    &kept,
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

fn deleted(
    issued: &std::collections::BTreeMap<String, IssuedMember>,
) -> Vec<(&String, &IssuedMember)> {
    issued.iter().filter(|(_, m)| m.deleted).collect()
}

/// The `{member_id}` leaf brg made for `id`, if its role's `cwd` has one.
fn leaf(
    inst: &Instance,
    cwd: Option<&str>,
    id: &str,
    kept: &[PathBuf],
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
    if kept
        .iter()
        .any(|held| resolve_existing_prefix(held).starts_with(&real))
    {
        return out.kept(&dir, &format!("a kept member or copy works there ({what})"));
    }
    match contents(&dir) {
        Ok(Contents::Folders) => match remove_folders(&dir) {
            Ok(()) => out.removed(&dir, &format!("folder of {what}")),
            Err(e) => out.error(format!("cannot remove {}: {e}", dir.display())),
        },
        Ok(Contents::Worktree) => out.kept(
            &dir,
            &format!(
                "holds a git worktree ({what}); its lead removes it with `git worktree remove`"
            ),
        ),
        Ok(Contents::Files) => out.kept(&dir, &format!("holds files ({what})")),
        Err(e) => out.error(format!("cannot read {}: {e}", dir.display())),
    }
}

enum Contents {
    Folders,
    Files,
    Worktree,
}

/// What `dir` holds, never following a symlink.
fn contents(dir: &Path) -> std::io::Result<Contents> {
    let mut files = false;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in std::fs::read_dir(&next)? {
            let entry = entry?;
            if entry.file_name() == ".git" {
                return Ok(Contents::Worktree);
            }
            if entry.file_type()?.is_dir() {
                stack.push(entry.path());
            } else {
                files = true;
            }
        }
    }
    Ok(if files {
        Contents::Files
    } else {
        Contents::Folders
    })
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
