//! Checkouts: the folders members of other instances hold, and the check a
//! member start makes against them.
//!
//! A folder is held by another instance's member while that member runs,
//! or while brg can't tell that it stopped (`unknown` liveness).
//! Members of one tree (an instance and its copies) may share a folder, as
//! a copy without `worktree: true` shares its parent's; the check is across
//! trees only. Folders are compared with symlinks resolved: the same,
//! inside, or around counts. The caller holds the registry lock from the
//! check until its members' windows run, so two starts can't both win.

use std::path::{Path, PathBuf};

use crate::address;
use crate::lifecycle::Liveness;
use crate::paths::overlaps;
use crate::registry;

/// One folder another instance holds, and why.
#[derive(Debug, Clone)]
pub struct Held {
    pub instance: String,
    pub folder: PathBuf,
    pub why: String,
}

/// Every folder held by an instance outside `except`'s tree. Called with
/// the registry lock held.
pub fn held(_lock: &registry::Lock, except: &str) -> Result<Vec<Held>, String> {
    let registry = registry::load()?;
    let root = |id: &str| id.split('/').next().unwrap_or_default().to_string();
    let mut out = Vec::new();
    for (id, entry) in &registry.instances {
        if !except.is_empty() && root(id) == root(except) {
            continue;
        }
        let inst = match address::instance(id) {
            Ok(i) => i,
            Err(_) => {
                // Its record can't be read: its folder stays held.
                out.push(Held {
                    instance: id.clone(),
                    folder: entry.folder.clone(),
                    why: "its record can't be read".into(),
                });
                continue;
            }
        };
        if inst.record.stopping {
            out.push(Held {
                instance: id.clone(),
                folder: inst.record.folder.clone(),
                why: format!("its cascade is unfinished; run brg stop {id}"),
            });
            continue;
        }
        if crate::state::read_meta(&inst.state).is_err() {
            // Its start record can't be read: brg can't tell which server
            // runs it, so its folder stays held.
            out.push(Held {
                instance: id.clone(),
                folder: inst.record.folder.clone(),
                why: "its start.json can't be read".into(),
            });
            continue;
        }
        let folder_of = |m: &str| {
            inst.record
                .members
                .get(m)
                .and_then(|r| r.cwd.clone())
                .unwrap_or_else(|| inst.record.folder.clone())
        };
        for (m, l) in crate::runtime::liveness_of(&inst) {
            let why = match l {
                Liveness::Gone => continue,
                Liveness::Alive => format!("{m} is running there"),
                Liveness::Unknown => format!("brg can't tell that {m} stopped"),
            };
            out.push(Held {
                instance: id.clone(),
                folder: folder_of(&m),
                why,
            });
        }
    }
    Ok(out)
}

/// The checkout reservation: refuse if any of `wanted` (member, folder) is
/// the same as, inside or around a folder another instance holds. Called
/// with the registry lock held; the caller records the folders it starts
/// in before releasing it.
pub fn reserve(
    lock: &registry::Lock,
    instance: &str,
    wanted: &[(String, PathBuf)],
) -> Result<(), String> {
    let held = held(lock, instance)?;
    let mut clashes = Vec::new();
    for (member, folder) in wanted {
        for h in &held {
            if overlaps(folder, &h.folder) {
                clashes.push(format!(
                    "{member}'s folder {} overlaps {} of instance `{}` ({})",
                    folder.display(),
                    h.folder.display(),
                    h.instance,
                    h.why
                ));
            }
        }
    }
    clashes.dedup();
    if clashes.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "another instance holds this folder, so nothing was started:\n  {}",
            clashes.join("\n  ")
        ))
    }
}

/// Whether `folder` overlaps another instance's held folder (for `create`,
/// before anything is registered).
pub fn check_folder(lock: &registry::Lock, folder: &Path) -> Result<(), String> {
    reserve(
        lock,
        "",
        &[("the instance".to_string(), folder.to_path_buf())],
    )
}
