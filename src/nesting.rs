//! Team-role copies: stopped nested instances with their own working folders.

use std::path::{Path, PathBuf};

use crate::address::{self, Instance};
use crate::paths::{overlaps, resolve_existing_prefix};
use crate::registry;
use crate::runtime::UpOutcome;
use crate::state;
use crate::team::TeamRole;
use crate::validate::Validated;

/// All kept copies below an instance, deepest first. The caller holds the
/// registry lock when using this list for a lifecycle change.
pub fn descendants(inst: &Instance) -> Result<Vec<Instance>, String> {
    let prefix = format!("{}/", inst.id);
    let registry = registry::load()?;
    let mut copies = registry
        .instances
        .keys()
        .filter(|id| id.starts_with(&prefix))
        .map(|id| address::instance(id))
        .collect::<Result<Vec<_>, _>>()?;
    copies.sort_by_key(|copy| std::cmp::Reverse(copy.id.split('/').count()));
    Ok(copies)
}

/// The outermost unfinished cascade enclosing this instance, if any.
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
            "instance `{id}` is stopping; finish its cascade first: brg stop {id}"
        ));
    }
    Ok(())
}

/// Make a copy's folders and registration, without starting any member.
pub fn create(
    mut inst: Instance,
    v: &Validated,
    role_name: &str,
    role: &TeamRole,
) -> Result<UpOutcome, String> {
    let lock = registry::lock()?;
    inst.reload()?;
    check_not_stopping(&inst)?;
    crate::runtime::check_parent_authority(&inst, role_name, Some(v), None)?;
    if let Some(meta) = state::read_meta(&inst.state)
        && meta.stopped_at.is_none()
    {
        crate::lifecycle::check_join(
            &meta,
            &v.config_snapshot(),
            &inst.state,
            &inst.id,
            &state::restart_hint(&inst.id),
        )?;
    }
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
            "role `{role_name}` has {} kept copies (max: {}): {}; delete a stopped copy before creating another",
            kept.len(),
            role.max,
            kept.join(", ")
        ));
    }
    let id = inst.record.issue_copy(role_name)?;
    let folder = copy_folder(&inst, role, &id)?;
    crate::checkouts::check_folder(&lock, &folder)?;
    let child = validate_child(&inst, &role.dir)?;
    // Reserve the ID before touching disk: a failed creation cannot reuse it.
    inst.record
        .copies
        .get_mut(&id)
        .expect("issued copy")
        .deleted = true;
    lock.save_instance(&inst.state, &inst.record)?;
    std::fs::create_dir_all(&folder)
        .map_err(|e| format!("cannot create copy folder {}: {e}", folder.display()))?;
    let full_id = inst.at(&id);
    registry::create_copy(&lock, &full_id, &role.dir, &folder, parent)?;
    let issued = inst.record.copies.get_mut(&id).expect("issued copy");
    issued.deleted = false;
    issued.cwd = Some(folder.clone());
    lock.save_instance(&inst.state, &inst.record)?;
    let mut prepare: Vec<PathBuf> = child
        .team
        .roles
        .values()
        .filter(|role| role.start)
        .filter_map(|role| role.cwd.as_deref())
        .filter(|cwd| !cwd.contains("{member_id}"))
        .map(|cwd| folder.join(cwd))
        .collect();
    prepare.sort();
    prepare.dedup();
    Ok(UpOutcome {
        tree: format!("{full_id}  folder {} · stopped\n", folder.display()),
        notes: vec![format!(
            "prepare {} before starting: brg start {full_id}",
            if prepare.is_empty() {
                folder.display().to_string()
            } else {
                prepare
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        )],
        failed: false,
    })
}

/// Validate a child against its declaring instance and every kept ancestor.
/// The caller holds the registry lock, so explicit repointing cannot race it.
pub fn validate_child(parent: &Instance, dir: &Path) -> Result<Validated, String> {
    let mut ancestors = Vec::new();
    let mut current = parent.clone();
    loop {
        let folder = current.record.team_dir.canonicalize().map_err(|_| {
            format!(
                "`{}`'s team folder is gone; restore it before creating or starting a copy",
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

fn copy_folder(inst: &Instance, role: &TeamRole, id: &str) -> Result<PathBuf, String> {
    let cwd = Path::new(&role.cwd);
    if cwd.file_name().is_none_or(|name| name != "{member_id}")
        || role.cwd.matches("{member_id}").count() != 1
    {
        return Err(
            "a team role's cwd must end in `{member_id}`, for example `dev/{member_id}`".into(),
        );
    }
    let folder =
        resolve_existing_prefix(&inst.record.folder.join(role.cwd.replace("{member_id}", id)));
    if !folder.starts_with(&inst.record.folder) {
        return Err(format!(
            "copy `{id}`'s folder leaves its instance; change its `cwd`"
        ));
    }
    for (other, copy) in inst.record.copies.iter().filter(|(_, copy)| !copy.deleted) {
        if let Some(held) = &copy.cwd
            && overlaps(&folder, held)
        {
            return Err(format!(
                "copy `{id}`'s folder overlaps kept copy `{other}`; give copies separate folders"
            ));
        }
    }
    Ok(folder)
}
