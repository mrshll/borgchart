//! Resolve create-time choices against the current role and its vendor pins.

use crate::config::Auth;
use crate::team::{EFFORTS, Model, Posture, Role, Vendor};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Only choices explicitly supplied at create; other values follow the template.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Overrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<Vendor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

impl Overrides {
    pub fn is_empty(&self) -> bool {
        self.vendor.is_none() && self.model.is_none() && self.effort.is_none()
    }

    /// The later layer replaces only keys it supplies.
    pub fn merge(&self, later: &Self) -> Self {
        Self {
            vendor: later.vendor.or(self.vendor),
            model: later.model.clone().or_else(|| self.model.clone()),
            effort: later.effort.clone().or_else(|| self.effort.clone()),
        }
    }

    pub fn resolve(&self, role: &Role, auth: Auth) -> Result<Role, String> {
        apply(
            role,
            self.vendor,
            self.model.as_deref(),
            self.effort.as_deref(),
            auth,
        )
    }

    pub fn keys(&self) -> Vec<&str> {
        [
            ("vendor", self.vendor.is_some()),
            ("model", self.model.is_some()),
            ("effort", self.effort.is_some()),
        ]
        .into_iter()
        .filter_map(|(key, set)| set.then_some(key))
        .collect()
    }
}

/// Parse bare agent choices or choices addressed to a child template role.
pub fn parse(values: &[String]) -> Result<BTreeMap<String, Overrides>, String> {
    let mut choices = BTreeMap::<String, Overrides>::new();
    for value in values.iter().flat_map(|v| v.split(',')) {
        let (address, value) = value.split_once('=').ok_or_else(|| {
            format!("`{value}` needs a key and value: --set [<role>.]<key>=<value>")
        })?;
        if address.starts_with('.') {
            return Err("give a role before `.` or use a bare key for one agent".into());
        }
        let (role, key) = address.split_once('.').unwrap_or(("", address));
        if key.contains('.') {
            return Err("choices reach agent roles one level down: `<role>.<key>=<value>`".into());
        }
        let choice = choices.entry(role.to_string()).or_default();
        if choice.keys().contains(&key) {
            return Err(format!("`{address}` is set twice; supply each key once"));
        }
        match key {
            "vendor" => {
                choice.vendor = Some(match value {
                    "claude" => Vendor::Claude,
                    "codex" => Vendor::Codex,
                    _ => return Err("choose a vendor: claude, codex".into()),
                })
            }
            "model" => {
                check_model(value)?;
                choice.model = Some(value.into());
            }
            "effort" if EFFORTS.contains(&value) => choice.effort = Some(value.into()),
            "effort" => return Err(format!("choose an effort: {}", EFFORTS.join(", "))),
            _ => {
                return Err(format!(
                    "unknown key `{key}`; settable: vendor, model, effort"
                ));
            }
        }
    }
    Ok(choices)
}

fn check_model(model: &str) -> Result<(), String> {
    if model.is_empty() || model.chars().any(char::is_whitespace) || model.starts_with('-') {
        Err("give a nonempty model name without whitespace or a leading `-`".into())
    } else {
        Ok(())
    }
}

/// Check the address and resolve every named role before create touches disk.
pub fn child_choices(
    values: &[String],
    role_name: &str,
    child: &crate::validate::Validated,
) -> Result<BTreeMap<String, Overrides>, String> {
    let choices = parse(values)?;
    for (role, choice) in &choices {
        if role.is_empty() {
            return Err(format!(
                "`{role_name}` is a team role; say which of its roles: `--set lead.vendor=codex` (roles of `{}`: {})",
                child.team.name,
                child
                    .team
                    .roles
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if child.team.team_roles.contains_key(role) {
            return Err(format!(
                "`{role}` is a team role of `{}`; choices reach agent roles one level down",
                child.team.name
            ));
        }
        let agent = child.team.roles.get(role).ok_or_else(|| {
            format!(
                "unknown role `{role}`; roles of `{}`: {}",
                child.team.name,
                child
                    .team
                    .roles
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        choice.resolve(agent, child.auth())?;
    }
    Ok(choices)
}

/// Bare choices for one new agent, checked before issuing its ID.
pub fn agent_choice(values: &[String], target: &str) -> Result<Overrides, String> {
    let mut choices = parse(values)?;
    if let Some((role, choice)) = choices.iter().find(|(role, _)| !role.is_empty()) {
        let key = choice.keys()[0];
        return Err(format!(
            "`{role}.{key}` names a role, but `{target}` is one agent; write `{key}=<value>`"
        ));
    }
    Ok(choices.remove("").unwrap_or_default())
}

/// Resolve optional choices, refusing settings that cannot run with the vendor.
pub fn apply(
    role: &Role,
    vendor: Option<Vendor>,
    model: Option<&str>,
    effort: Option<&str>,
    auth: Auth,
) -> Result<Role, String> {
    let mut resolved = role.clone();
    resolved.vendor = vendor.unwrap_or(role.vendor);
    let refuse = |message: &str| format!("role `{}`: {message}", role.id);
    if resolved.vendor != role.vendor {
        for (key, set) in [
            ("args", !role.args.is_empty()),
            ("connectors", !role.connectors.is_empty()),
            ("remote_control", role.remote_control),
            ("posture: locked", role.posture == Posture::Locked),
        ] {
            if set {
                return Err(format!(
                    "role `{}` sets `{key}`, which is written for {}, so it can't run on {}; keep its vendor (model and effort can still be set) or use a role without `{key}`",
                    role.id,
                    role.vendor.as_str(),
                    resolved.vendor.as_str()
                ));
            }
        }
    }
    if auth == Auth::ApiKey && resolved.vendor == Vendor::Claude && !role.connectors.is_empty() {
        return Err(refuse(
            "Claude connectors need your login; use `auth: login` or remove `connectors`",
        ));
    }
    if let Some(effort) = effort
        && !EFFORTS.contains(&effort)
    {
        return Err(refuse(&format!("choose an effort: {}", EFFORTS.join(", "))));
    }
    resolved.model = match model {
        Some(name) => {
            check_model(name).map_err(|e| refuse(&e))?;
            Some(Model {
                name: name.to_string(),
                effort: effort.map(str::to_string).or_else(|| {
                    role.pins
                        .get(&resolved.vendor)
                        .and_then(|m| m.effort.clone())
                }),
            })
        }
        None => role.pins.get(&resolved.vendor).cloned(),
    };
    if resolved.model.is_none() && (resolved.vendor != role.vendor || effort.is_some()) {
        return Err(refuse(&format!(
            "pins no {} model (`models.{}` in {}.md); add `model=<name>`",
            resolved.vendor.as_str(),
            resolved.vendor.as_str(),
            role.id
        )));
    }
    if let Some(effort) = effort
        && let Some(model) = &mut resolved.model
    {
        model.effort = Some(effort.to_string());
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role() -> Role {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("template.yaml"),
            "roles:\n  worker: {prompt: worker.md}\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("worker.md"), "---\nmodels: {claude: opus low, codex: example-model medium}\n---\nWork on the sample.\n").unwrap();
        let mut report = crate::diag::Report::default();
        crate::team::load_with(dir.path(), &[], None, &mut report)
            .unwrap()
            .roles
            .remove("worker")
            .unwrap()
    }

    #[test]
    fn vendor_change_uses_the_other_vendors_pin() {
        let role = role();
        let resolved = apply(&role, Some(Vendor::Codex), None, None, Auth::Login).unwrap();
        assert_eq!(resolved.vendor, Vendor::Codex);
        assert_eq!(
            resolved.model,
            Some(Model {
                name: "example-model".into(),
                effort: Some("medium".into())
            })
        );
    }

    #[test]
    fn model_replacement_keeps_vendor_pin_effort_unless_given() {
        let role = role();
        for vendor in [None, Some(Vendor::Codex)] {
            for effort in [None, Some("high")] {
                let resolved = apply(&role, vendor, Some("custom"), effort, Auth::Login).unwrap();
                assert_eq!(
                    resolved.model,
                    Some(Model {
                        name: "custom".into(),
                        effort: Some(
                            effort
                                .unwrap_or(if vendor == Some(Vendor::Codex) {
                                    "medium"
                                } else {
                                    "low"
                                })
                                .into()
                        )
                    })
                );
            }
        }
    }

    #[test]
    fn effort_override_keeps_the_pinned_model() {
        let role = role();
        let resolved = apply(&role, None, None, Some("high"), Auth::Login).unwrap();
        assert_eq!(
            resolved.model,
            Some(Model {
                name: "opus".into(),
                effort: Some("high".into())
            })
        );
    }

    #[test]
    fn vendor_change_with_args_is_refused() {
        let mut role = role();
        role.args.push("--verbose".into());
        assert!(apply(&role, Some(Vendor::Codex), None, None, Auth::Login).is_err());
        assert!(apply(&role, None, Some("custom"), None, Auth::Login).is_ok());
    }

    #[test]
    fn vendor_change_with_connectors_is_refused() {
        let mut role = role();
        role.connectors.servers.insert("sample".into(), vec![]);
        assert!(apply(&role, Some(Vendor::Codex), None, None, Auth::Login).is_err());
        assert!(apply(&role, None, Some("custom"), None, Auth::Login).is_ok());
    }

    #[test]
    fn non_claude_remote_control_is_refused() {
        let mut role = role();
        role.remote_control = true;
        assert!(apply(&role, Some(Vendor::Codex), None, None, Auth::Login).is_err());
        assert!(apply(&role, None, Some("custom"), None, Auth::Login).is_ok());
    }

    #[test]
    fn vendor_change_without_a_pin_requires_a_model() {
        let mut role = role();
        role.pins.remove(&Vendor::Codex);
        assert!(apply(&role, Some(Vendor::Codex), None, None, Auth::Login).is_err());
        let resolved = apply(
            &role,
            Some(Vendor::Codex),
            Some("custom"),
            None,
            Auth::Login,
        )
        .unwrap();
        assert_eq!(resolved.model.unwrap().name, "custom");
        role.pins.clear();
        role.model = None;
        for vendor in [None, Some(role.vendor)] {
            assert!(
                apply(&role, vendor, None, None, Auth::Login)
                    .unwrap()
                    .model
                    .is_none()
            );
        }
    }

    #[test]
    fn claude_connectors_with_api_key_are_refused() {
        let mut role = role();
        role.connectors.servers.insert("sample".into(), vec![]);
        assert!(apply(&role, None, Some("custom"), None, Auth::ApiKey).is_err());
        assert!(apply(&role, None, Some("custom"), None, Auth::Login).is_ok());
        role.vendor = Vendor::Codex;
        assert!(apply(&role, None, Some("custom"), None, Auth::ApiKey).is_ok());
    }

    #[test]
    fn unknown_effort_is_refused() {
        let role = role();
        assert!(apply(&role, None, None, Some("extreme"), Auth::Login).is_err());
        for effort in EFFORTS {
            assert!(apply(&role, None, None, Some(effort), Auth::Login).is_ok());
        }
    }

    #[test]
    fn invalid_model_argument_is_refused() {
        for model in ["", "   ", "two words", "-option"] {
            assert!(apply(&role(), None, Some(model), None, Auth::Login).is_err());
        }
    }

    #[test]
    fn effort_without_a_pin_requires_a_model() {
        let mut role = role();
        role.pins.clear();
        role.model = None;
        assert!(apply(&role, None, None, Some("high"), Auth::Login).is_err());
        assert!(apply(&role, None, Some("custom"), Some("high"), Auth::Login).is_ok());
    }
    #[test]
    fn locked_vendor_change_is_refused_but_model_choice_is_allowed() {
        let mut role = role();
        role.posture = Posture::Locked;
        assert!(apply(&role, Some(Vendor::Codex), None, None, Auth::Login).is_err());
        assert!(apply(&role, None, Some("custom"), None, Auth::Login).is_ok());
    }

    #[test]
    fn parser_preserves_model_dots_and_refuses_invalid_or_duplicate_choices() {
        let choices = parse(&[
            "lead.vendor=codex,lead.model=gpt-6.1-sol".into(),
            "lead.effort=high".into(),
        ])
        .unwrap();
        assert_eq!(choices["lead"].model.as_deref(), Some("gpt-6.1-sol"));
        for value in [
            "vendor",
            "vendor=unknown",
            "model=",
            "model=-bad",
            "model=two words",
            "effort=extreme",
            "extra=x",
            "vendor=codex,vendor=claude",
            "lead.model=x,lead.model=y",
            "lead.worker.vendor=codex",
            ".vendor=codex",
        ] {
            assert!(parse(&[value.into()]).is_err(), "{value}");
        }
    }

    #[test]
    fn deeper_choice_paths_explain_the_one_level_limit() {
        let error = parse(&["lead.worker.vendor=codex".into()]).unwrap_err();
        assert!(
            error.contains("choices reach agent roles one level down"),
            "{error}"
        );
        assert!(error.contains("<role>.<key>=<value>"), "{error}");
    }

    #[test]
    fn member_layer_replaces_only_explicit_keys() {
        let team = Overrides {
            vendor: Some(Vendor::Codex),
            model: Some("team-model".into()),
            effort: Some("medium".into()),
        };
        let member = Overrides {
            model: Some("member-model".into()),
            ..Overrides::default()
        };
        let resolved = team.merge(&member).resolve(&role(), Auth::Login).unwrap();
        assert_eq!(resolved.vendor, Vendor::Codex);
        assert_eq!(
            resolved.model,
            Some(Model {
                name: "member-model".into(),
                effort: Some("medium".into())
            })
        );
    }
}
