//! Child teams: stopped nested teams, each working in its own git
//! worktree (`worktree: true`) or in its parent's folder.

use std::path::Path;

use crate::address::{self, Instance};
use crate::registry;
use crate::runtime::UpOutcome;
use crate::team::TeamRole;
use crate::validate::Validated;

/// All kept child teams below a team, deepest first. The caller holds the
/// registry lock when using this list for a lifecycle change.
pub fn descendants(inst: &Instance) -> Result<Vec<Descendant>, String> {
    let prefix = format!("{}/", inst.id);
    let registry = registry::load()?;
    let mut copies = registry
        .instances
        .keys()
        .filter(|id| id.starts_with(&prefix))
        .map(|id| Descendant {
            id: id.clone(),
            instance: address::instance(id),
        })
        .collect::<Vec<_>>();
    copies.sort_by_key(|copy| std::cmp::Reverse(copy.id.split('/').count()));
    Ok(copies)
}

/// A registered child team, including one whose state cannot currently be read.
pub struct Descendant {
    pub id: String,
    pub instance: Result<Instance, String>,
}

/// The outermost unfinished cascade enclosing this team, if any.
pub fn stopping_ancestor(inst: &Instance) -> Result<Option<String>, String> {
    let mut id = String::new();
    for segment in inst.id.split('/') {
        if !id.is_empty() {
            id.push('/');
        }
        id.push_str(segment);
        if address::instance(&id)?.record.stopping {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// Called under the registry lock before a create or start changes anything.
pub fn check_not_stopping(inst: &Instance) -> Result<(), String> {
    if let Some(id) = stopping_ancestor(inst)? {
        return Err(format!(
            "team `{id}` is stopping; finish its cascade first: brg stop {id}"
        ));
    }
    Ok(())
}

/// Make a child team's registration, and its worktree for a `worktree: true` role
/// (detached at the working folder's `HEAD`, or on a new `branch`), without
/// starting any member.
pub fn create(
    mut inst: Instance,
    v: &Validated,
    role_name: &str,
    name: Option<&str>,
    role: &TeamRole,
    branch: Option<&str>,
    start: Option<&crate::worktree::Start>,
) -> Result<UpOutcome, String> {
    let lock = registry::lock()?;
    inst.reload()?;
    check_not_stopping(&inst)?;
    crate::runtime::check_parent_authority(&inst, role_name, Some(v), None)?;
    crate::state::read_meta(&inst.state)?;
    let parent = role.parent.as_deref().map(|parent_role| {
        inst.record.members_of(parent_role).first().cloned()
            .map(|member| inst.at(&member))
            .ok_or_else(|| format!("role `{role_name}`'s parent role `{parent_role}` has no member; create it first: brg create {}", inst.at(parent_role)))
    }).transpose()?;
    let kept: Vec<_> = inst
        .record
        .copies
        .iter()
        .filter(|(_, copy)| !copy.deleted && copy.role == role_name)
        .map(|(id, _)| inst.at(id))
        .collect();
    if kept.len() >= role.max {
        return Err(format!(
            "role `{role_name}` has {} kept child teams (max: {}): {}; delete a stopped child team before creating another",
            kept.len(),
            role.max,
            kept.join(", ")
        ));
    }
    crate::team::find(&role.name, &v.config.teams)?;
    let id = inst
        .record
        .issue_copy(role_name, name, |id| crate::runtime::is_role(v, id))?;
    let full_id = inst.at(&id);
    let folder = if role.worktree {
        crate::worktree::path(&full_id)
    } else {
        inst.record.folder.clone()
    };
    crate::checkouts::reserve(
        &lock,
        &inst.id,
        &[("the child team".into(), folder.clone())],
    )?;
    validate_child(&inst, &role.dir)?;
    // Reserve the ID before touching disk: a failed creation cannot reuse it.
    inst.record
        .copies
        .get_mut(&id)
        .expect("issued child team")
        .deleted = true;
    lock.save_instance(&inst.state, &inst.record)?;
    if role.worktree {
        let root = crate::worktree::root_folder(&inst.id)?;
        if let Err(e) = crate::worktree::add(&root, &inst.record.folder, &folder, branch, start) {
            // A name is the caller's choice: give it back if nothing reached disk.
            if name.is_some() && !folder.exists() {
                inst.record.copies.remove(&id);
                lock.save_instance(&inst.state, &inst.record)?;
            }
            return Err(format!(
                "child team `{full_id}` needs its own worktree: {e}"
            ));
        }
    }
    registry::create_copy(&lock, &full_id, &role.dir, &folder, parent)?;
    let issued = inst.record.copies.get_mut(&id).expect("issued child team");
    issued.deleted = false;
    issued.cwd = Some(folder.clone());
    lock.save_instance(&inst.state, &inst.record)?;
    Ok(UpOutcome {
        tree: format!(
            "{full_id}  folder {}{} · stopped\n",
            folder.display(),
            start.map(|s| s.detail()).unwrap_or_default()
        ),
        notes: vec![format!("start it: brg start {full_id}")],
        failed: false,
    })
}

/// Validate a child against its declaring team and every kept ancestor.
/// The caller holds the registry lock, so explicit repointing cannot race it.
pub fn validate_child(parent: &Instance, dir: &Path) -> Result<Validated, String> {
    let mut ancestors = Vec::new();
    let mut current = parent.clone();
    loop {
        let folder = current.record.team_dir.canonicalize().map_err(|_| {
            format!(
                "`{}`'s template folder is gone; restore it before creating or starting a child team",
                current.id
            )
        })?;
        ancestors.push(folder);
        let Some((id, _)) = current.id.rsplit_once('/') else {
            break;
        };
        current = address::instance(id)?;
    }
    let (report, validated) = crate::validate::validate_against(dir, &ancestors);
    validated.ok_or_else(|| {
        report
            .errors()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    })
}
