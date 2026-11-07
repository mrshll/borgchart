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

/// The registered instance `id`. The error says what to write instead when
/// `id` is a team's name, a member or a path.
pub fn instance(id: &str) -> Result<Instance, String> {
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
    if let Some((inst, rest)) = arg.split_once('/') {
        if rest.is_empty() || rest.contains('/') {
            return Err(format!(
                "`{arg}`: write `<instance>/<member>` or `<instance>/<role>`"
            ));
        }
        let inst = instance(inst).map_err(|e| {
            if registry::load().is_ok_and(|r| !r.instances.contains_key(inst)) {
                format!("{e}. For a folder, write `./{arg}`")
            } else {
                e
            }
        })?;
        let target = target(&inst, rest);
        return Ok((inst, target));
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
    if inst.record.members.contains_key(name) {
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
    let all: Vec<&String> = inst.record.members.keys().collect();
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
    let all: Vec<&str> = inst.record.members.keys().map(|m| m.as_str()).collect();
    if all.is_empty() {
        "none".into()
    } else {
        all.join(", ")
    }
}
