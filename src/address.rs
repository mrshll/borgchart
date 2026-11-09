//! Addresses: `[instance/]member` and `[instance/]<role>`, resolved to an
//! instance plus a member or a role.
//!
//! Members always carry a suffix (`implementer-x7k2`), so a bare name is a
//! role. IDs are parsed from the right, so roles and teams with `-` in their
//! names work. The instance can be left out only inside a member, where it
//! comes from `BORGCHART_INSTANCE`; everywhere else it is named in full. brg
//! never infers one from a team's name or from which instances run.

use crate::registry::{self, InstanceRecord};
use crate::state::StateDir;

/// Set in every member's and plugin's environment: its instance.
pub const ENV_INSTANCE: &str = "BORGCHART_INSTANCE";

/// One registered instance, with its record.
#[derive(Debug, Clone)]
pub struct Instance {
    pub id: String,
    pub state: StateDir,
    pub record: InstanceRecord,
}

impl Instance {
    /// Read the record again (after a change under the registry lock).
    pub fn reload(&mut self) -> Result<(), String> {
        self.record = registry::read_instance(&self.state)?;
        Ok(())
    }

    /// `instance/name`, for messages.
    pub fn at(&self, name: &str) -> String {
        format!("{}/{name}", self.id)
    }
}

/// What an address names inside its instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// The instance itself.
    Instance,
    /// A member, by its full ID.
    Member(String),
    /// A role, by its bare name.
    Role(String),
}

/// The caller's own instance, when it runs inside a member.
pub fn own() -> Option<String> {
    std::env::var(ENV_INSTANCE).ok().filter(|v| !v.is_empty())
}

/// Every registered instance's ID, for messages.
pub fn known() -> String {
    let ids: Vec<String> = registry::load()
        .map(|r| r.instances.keys().cloned().collect())
        .unwrap_or_default();
    if ids.is_empty() {
        "there are no instances; create one with `brg create <team> <folder>`".into()
    } else {
        format!("instances: {}", ids.join(", "))
    }
}

/// Every top-level instance, read now. Status nests copies beneath these.
pub fn every() -> Result<Vec<Instance>, String> {
    let registry = registry::load()?;
    Ok(registry
        .instances
        .keys()
        .filter(|id| !id.contains('/'))
        .filter_map(|id| instance(id).ok())
        .collect())
}

/// The registered instance `id`. The error says what to write instead when
/// `id` is a team's name, a member or a path.
pub fn instance(id: &str) -> Result<Instance, String> {
    if !id.split('/').all(crate::names::is_valid_member_id) {
        return Err(format!(
            "`{id}` has an invalid address segment; use instance and member IDs"
        ));
    }
    let registry = registry::load()?;
    if registry.instances.contains_key(id) {
        let state = StateDir::new(id);
        let record = registry::read_instance(&state)?;
        return Ok(Instance {
            id: id.to_string(),
            state,
            record,
        });
    }
    let of_team: Vec<&String> = registry
        .instances
        .iter()
        .filter(|(_, e)| e.team == id)
        .map(|(i, _)| i)
        .collect();
    if !of_team.is_empty() {
        let list: Vec<&str> = of_team.iter().map(|s| s.as_str()).collect();
        return Err(format!(
            "`{id}` is a team, not an instance; name one of its instances in full: {}",
            list.join(", ")
        ));
    }
    if registry::split_id(id).is_some() {
        return Err(format!(
            "no instance `{id}`. A member is named with its instance outside a member: `<instance>/{id}`; {}",
            known()
        ));
    }
    Err(format!("no instance `{id}`; {}", known()))
}

/// Parse an argument naming an instance, or something in one: `instance`,
/// `instance/name`, or, inside a member, a bare `name` of its own instance.
pub fn resolve(arg: &str) -> Result<(Instance, Target), String> {
    if !arg.split('/').all(crate::names::is_valid_member_id) {
        return Err(format!(
            "`{arg}` has an invalid address segment; use instance and member IDs"
        ));
    }
    if arg.contains('/') {
        let registry = registry::load()?;
        let prefix = registry
            .instances
            .keys()
            .filter(|id| {
                arg == id.as_str()
                    || arg
                        .strip_prefix(id.as_str())
                        .is_some_and(|tail| tail.starts_with('/'))
            })
            .max_by_key(|id| id.len());
        if let Some(prefix) = prefix {
            let inst = instance(prefix)?;
            if arg == prefix {
                return Ok((inst, Target::Instance));
            }
            let rest = &arg[prefix.len() + 1..];
            if !rest.contains('/') {
                let target = target(&inst, rest);
                return Ok((inst, target));
            }
            let missing = rest.split('/').next().expect("nonempty address tail");
            let kept: Vec<_> = inst
                .record
                .copies
                .iter()
                .filter(|(_, copy)| !copy.deleted)
                .map(|(id, _)| inst.at(id))
                .collect();
            return Err(format!(
                "`{prefix}` has no copy `{missing}`; {}",
                if kept.is_empty() {
                    format!(
                        "it has no kept copies; create one with brg create {prefix}/<team-role>"
                    )
                } else {
                    format!("its kept copies: {}", kept.join(", "))
                }
            ));
        }
        return Err(format!(
            "no instance prefix for `{arg}`; {}. For a folder, write `./{arg}`",
            known()
        ));
    }
    if let Some(own) = own() {
        let inst = instance(&own)
            .map_err(|e| format!("this member's instance `{own}` can't be used: {e}"))?;
        if arg == inst.id {
            return Ok((inst, Target::Instance));
        }
        if inst.record.members.contains_key(arg) {
            return Ok((inst, Target::Member(arg.to_string())));
        }
        if registry::load().is_ok_and(|r| r.instances.contains_key(arg)) {
            return Ok((instance(arg)?, Target::Instance));
        }
        return Ok((inst, Target::Role(arg.to_string())));
    }
    Ok((instance(arg)?, Target::Instance))
}

/// A member if `name` is an issued member, else a role.
fn target(inst: &Instance, name: &str) -> Target {
    if inst.record.members.contains_key(name) {
        Target::Member(name.to_string())
    } else {
        Target::Role(name.to_string())
    }
}

/// `name` must be one of the instance's members. A role's name is refused,
/// listing the role's members.
pub fn member(inst: &Instance, name: &str) -> Result<String, String> {
    if let Some(issued) = inst.record.members.get(name) {
        if issued.deleted {
            return Err(format!(
                "member `{}` was deleted; create a new member with `brg create {}`",
                inst.at(name),
                inst.at(&issued.role)
            ));
        }
        return Ok(name.to_string());
    }
    let of_role = inst.record.members_of(name);
    if !of_role.is_empty() {
        return Err(format!(
            "`{name}` is a role; name its member in full: {}",
            of_role
                .iter()
                .map(|m| inst.at(m))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let mut report = crate::diag::Report::default();
    let frozen = crate::state::read_meta(&inst.state)
        .ok()
        .flatten()
        .unwrap_or_default()
        .config
        .team_dirs;
    if crate::team::load_with(&inst.record.team_dir, &[], Some(&frozen), &mut report)
        .is_some_and(|team| team.roles.contains_key(name))
    {
        return Err(format!(
            "role `{name}` has no member; create one with `brg create {}`",
            inst.at(name)
        ));
    }
    let all: Vec<&String> = inst.record.kept_members().map(|(id, _)| id).collect();
    Err(format!(
        "no member `{name}` in instance `{}`; {}",
        inst.id,
        if all.is_empty() {
            "it has no members".to_string()
        } else {
            format!(
                "its members: {}",
                all.iter()
                    .map(|m| m.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    ))
}

/// The members of the instance, for messages: `a, b` or `none`.
pub fn members_list(inst: &Instance) -> String {
    let all: Vec<&str> = inst
        .record
        .kept_members()
        .map(|(id, _)| id.as_str())
        .collect();
    if all.is_empty() {
        "none".into()
    } else {
        all.join(", ")
    }
}
