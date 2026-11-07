//! Who is calling: a member (from its launch environment), an outside caller
//! naming itself with `--external`, or an operator with neither.
//!
//! This prevents mistakes, not spoofing: anything running as the same user
//! can set these variables.

pub use crate::address::ENV_INSTANCE;
use crate::names::is_valid_id;

use super::outcome::{Code, Failure};

/// Set in every member's environment at start.
pub const ENV_MEMBER: &str = "BORGCHART_MEMBER";
pub const ENV_RUN: &str = "BORGCHART_RUN";
/// A unique ID for one start of one member.
pub const ENV_INCARNATION: &str = "BORGCHART_INCARNATION";
/// Set to `bus` for runtime-invoked plugin actions (alerts).
pub const ENV_ACTOR: &str = "BORGCHART_ACTOR";
pub const ACTOR_BUS: &str = "bus";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    Member {
        member: String,
        run: String,
        incarnation: String,
    },
    External {
        name: String,
    },
    /// No member context and no `--external`: may read and inspect, not publish.
    Operator,
}

impl Caller {
    /// The slot operation keys are scoped by: the member name, or `external.<name>`.
    pub fn slot(&self) -> Option<String> {
        match self {
            Caller::Member { member, .. } => Some(member.clone()),
            Caller::External { name } => Some(format!("external.{name}")),
            Caller::Operator => None,
        }
    }
}

/// The instance a bus command acts on: inside a member (or a plugin
/// process), its own, and `--instance` may only repeat it; outside, the
/// `--instance` given, which is required.
pub fn instance(flag: Option<&str>) -> Result<String, Failure> {
    instance_from(flag, |k| std::env::var(k).ok())
}

pub fn instance_from(
    flag: Option<&str>,
    get: impl Fn(&str) -> Option<String>,
) -> Result<String, Failure> {
    // A partial member context is refused before any store is chosen, for
    // reads as well as writes: it would otherwise act as an operator. An
    // instance alone (a plugin process) is not a member context.
    let set = |k: &str| get(k).is_some_and(|v| !v.is_empty());
    let parts = [ENV_MEMBER, ENV_RUN, ENV_INCARNATION];
    if parts.iter().any(|k| set(k)) && !(parts.iter().all(|k| set(k)) && set(ENV_INSTANCE)) {
        return Err(Failure::new(
            Code::Refused,
            format!(
                "incomplete member context: {ENV_INSTANCE}, {ENV_MEMBER}, {ENV_RUN} and {ENV_INCARNATION} must all be set (was this member started by a brg with bus support?)"
            ),
        ));
    }
    match (get(ENV_INSTANCE).filter(|v| !v.is_empty()), flag) {
        (Some(own), Some(other)) if other != own => Err(Failure::new(
            Code::Refused,
            format!("this is a member of instance `{own}`; it can't act on instance `{other}`"),
        )),
        (Some(own), _) => Ok(own),
        (None, Some(id)) => Ok(id.to_string()),
        (None, None) => Err(Failure::new(
            Code::Usage,
            format!(
                "name the instance with `--instance ID`; {}",
                crate::address::known()
            ),
        )),
    }
}

/// Work out the caller from the environment and an optional `--external NAME`.
pub fn caller(external: Option<&str>) -> Result<Caller, Failure> {
    caller_from(external, |k| std::env::var(k).ok())
}

pub fn caller_from(
    external: Option<&str>,
    get: impl Fn(&str) -> Option<String>,
) -> Result<Caller, Failure> {
    let member = get(ENV_MEMBER).filter(|v| !v.is_empty());
    let incarnation = get(ENV_INCARNATION).filter(|v| !v.is_empty());
    let run = get(ENV_RUN).filter(|v| !v.is_empty());
    let instance = get(ENV_INSTANCE).filter(|v| !v.is_empty());
    if member.is_some() || incarnation.is_some() {
        if external.is_some() {
            return Err(Failure::new(
                Code::Refused,
                "`--external` can't be used from inside a member; members publish as themselves",
            ));
        }
        return match (member, run, incarnation, instance) {
            (Some(member), Some(run), Some(incarnation), Some(_)) => Ok(Caller::Member {
                member,
                run,
                incarnation,
            }),
            _ => Err(Failure::new(
                Code::Refused,
                format!(
                    "incomplete member context: {ENV_INSTANCE}, {ENV_MEMBER}, {ENV_RUN} and {ENV_INCARNATION} must all be set (was this member started by a brg with bus support?)"
                ),
            )),
        };
    }
    match external {
        Some(name) if is_valid_id(name) => Ok(Caller::External {
            name: name.to_string(),
        }),
        Some(name) => Err(Failure::new(
            Code::Usage,
            format!("`--external {name}`: names match [a-z][a-z0-9_-]{{0,31}}"),
        )),
        None => Ok(Caller::Operator),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn member_context() {
        let e = env(&[
            (ENV_INSTANCE, "oncall-2a3b"),
            (ENV_MEMBER, "lead"),
            (ENV_RUN, "r1"),
            (ENV_INCARNATION, "lead-1"),
        ]);
        assert_eq!(
            caller_from(None, &e).unwrap(),
            Caller::Member {
                member: "lead".into(),
                run: "r1".into(),
                incarnation: "lead-1".into()
            }
        );
        assert_eq!(
            caller_from(Some("supervisor"), &e).unwrap_err().code,
            Code::Refused
        );
    }

    #[test]
    fn the_instance_is_the_members_own() {
        let inside = env(&[(ENV_INSTANCE, "oncall-2a3b")]);
        assert_eq!(instance_from(None, &inside).unwrap(), "oncall-2a3b");
        assert_eq!(
            instance_from(Some("oncall-2a3b"), &inside).unwrap(),
            "oncall-2a3b"
        );
        assert_eq!(
            instance_from(Some("other-4c5d"), &inside).unwrap_err().code,
            Code::Refused
        );
        let outside = env(&[]);
        assert_eq!(
            instance_from(Some("other-4c5d"), &outside).unwrap(),
            "other-4c5d"
        );
    }

    #[test]
    fn partial_context_is_refused() {
        let e = env(&[
            (ENV_MEMBER, "lead"),
            (ENV_RUN, "r1"),
            (ENV_INCARNATION, "lead-1"),
        ]);
        assert_eq!(caller_from(None, &e).unwrap_err().code, Code::Refused);
        let e = env(&[(ENV_MEMBER, "lead")]);
        assert_eq!(caller_from(None, &e).unwrap_err().code, Code::Refused);
        let e = env(&[(ENV_INCARNATION, "x")]);
        assert_eq!(
            caller_from(Some("supervisor"), &e).unwrap_err().code,
            Code::Refused
        );
    }

    #[test]
    fn external_and_operator() {
        let e = env(&[]);
        assert_eq!(
            caller_from(Some("supervisor"), &e)
                .unwrap()
                .slot()
                .as_deref(),
            Some("external.supervisor")
        );
        assert_eq!(caller_from(None, &e).unwrap(), Caller::Operator);
        assert_eq!(
            caller_from(Some("Bad Name"), &e).unwrap_err().code,
            Code::Usage
        );
    }
}
