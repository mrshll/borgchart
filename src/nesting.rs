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

/// Make a copy's folders and registration, without starting any member.
pub fn create(
    mut inst: Instance,
    v: &Validated,
    role_name: &str,
    role: &TeamRole,
) -> Result<UpOutcome, String> {
    let lock = registry::lock()?;
    inst.reload()?;
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
    let child = crate::validate::validate(&role.dir);
    let child = child.1.ok_or_else(|| {
        child
            .0
            .errors()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    })?;
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
