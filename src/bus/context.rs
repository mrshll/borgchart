//! Who is calling: a member (from its launch environment), an outside caller
//! using `owner` by default or naming itself with `--external`, or an
//! instance-only plugin with no outside identity.
//!
//! This prevents mistakes, not spoofing: anything running as the same user
//! can set these variables.

pub use crate::address::ENV_INSTANCE;
use crate::names::is_valid_id;
use serde::{Deserialize, Serialize};

use super::outcome::{Code, Failure};

/// Set in every member's environment at start.
pub const ENV_MEMBER: &str = "BORGCHART_MEMBER";
/// A unique ID for one start of one member.
pub const ENV_INCARNATION: &str = "BORGCHART_INCARNATION";

const MEMBER_ONLY_ENV: [&str; 5] = [
    ENV_MEMBER,
    ENV_INCARNATION,
    crate::exec::ENV_ROLE,
    crate::exec::ENV_NOTES,
    "BORGCHART_PARENT",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    Member {
        member: String,
        incarnation: String,
    },
    External {
        name: String,
    },
    /// Instance-only context and no `--external`: may read, not publish.
    Operator,
}

/// The fenced identity that acknowledges a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Actor {
    /// A member, from its launch context.
    Member { member: String, incarnation: String },
    /// An outside caller, defaulting to `owner` without team context.
    External { name: String },
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
    let parts = [ENV_MEMBER, ENV_INCARNATION];
    if MEMBER_ONLY_ENV.iter().any(|k| set(k))
        && !(parts.iter().all(|k| set(k)) && set(ENV_INSTANCE))
    {
        return Err(Failure::new(
            Code::Refused,
            format!(
                "incomplete member context: {ENV_INSTANCE}, {ENV_MEMBER} and {ENV_INCARNATION} must all be set (was this member started by a brg with bus support?)"
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
    let instance = get(ENV_INSTANCE).filter(|v| !v.is_empty());
    if MEMBER_ONLY_ENV
        .iter()
        .any(|k| get(k).is_some_and(|v| !v.is_empty()))
    {
        if external.is_some() {
            return Err(Failure::new(
                Code::Refused,
                "`--external` can't be used from inside a member; members publish as themselves",
            ));
        }
        return match (member, incarnation, instance) {
            (Some(member), Some(incarnation), Some(_)) => Ok(Caller::Member {
                member,
                incarnation,
            }),
            _ => Err(Failure::new(
                Code::Refused,
                format!(
                    "incomplete member context: {ENV_INSTANCE}, {ENV_MEMBER} and {ENV_INCARNATION} must all be set (was this member started by a brg with bus support?)"
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
        None if instance.is_none() => Ok(Caller::External {
            name: "owner".into(),
        }),
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
            (ENV_INCARNATION, "lead-1"),
        ]);
        assert_eq!(
            caller_from(None, &e).unwrap(),
            Caller::Member {
                member: "lead".into(),
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
        let e = env(&[(ENV_MEMBER, "lead"), (ENV_INCARNATION, "lead-1")]);
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
    fn member_launch_variables_without_identity_are_refused() {
        for key in [
            crate::exec::ENV_ROLE,
            crate::exec::ENV_NOTES,
            "BORGCHART_PARENT",
        ] {
            for instance in ["", "oncall-2a3b"] {
                let pairs = [(key, "member-context"), (ENV_INSTANCE, instance)];
                let e = env(&pairs);
                assert_eq!(caller_from(None, &e).unwrap_err().code, Code::Refused);
                assert_eq!(
                    instance_from(Some("oncall-2a3b"), &e).unwrap_err().code,
                    Code::Refused
                );
            }
            let pairs = [(key, "")];
            let empty = env(&pairs);
            assert_eq!(
                caller_from(None, &empty).unwrap(),
                Caller::External {
                    name: "owner".into()
                }
            );
        }
    }

    #[test]
    fn external_default_does_not_apply_in_a_plugin_context() {
        let e = env(&[]);
        assert_eq!(
            caller_from(Some("supervisor"), &e).unwrap(),
            Caller::External {
                name: "supervisor".into()
            }
        );
        assert_eq!(
            caller_from(None, &e).unwrap(),
            Caller::External {
                name: "owner".into()
            }
        );
        let plugin = env(&[(ENV_INSTANCE, "oncall-2a3b")]);
        assert_eq!(caller_from(None, &plugin).unwrap(), Caller::Operator);
        assert_eq!(
            caller_from(Some("Bad Name"), &e).unwrap_err().code,
            Code::Usage
        );
    }
}
