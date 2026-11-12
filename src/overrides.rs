//! Resolve create-time choices against the current role and its vendor pins.

use crate::config::Auth;
use crate::team::{EFFORTS, Model, Role, Vendor};

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
        if !role.args.is_empty() {
            return Err(refuse(
                "remove its vendor-specific `args` before choosing another vendor",
            ));
        }
        if !role.connectors.is_empty() {
            return Err(refuse(
                "remove its vendor-specific `connectors` before choosing another vendor",
            ));
        }
    }
    if role.remote_control && resolved.vendor != Vendor::Claude {
        return Err(refuse(
            "Remote Control needs Claude; choose Claude or remove `remote_control`",
        ));
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
        Some(name) if !name.trim().is_empty() => Some(Model {
            name: name.to_string(),
            effort: effort.map(str::to_string),
        }),
        Some(_) => return Err(refuse("give a nonempty model name")),
        None => role.pins.get(&resolved.vendor).cloned(),
    };
    if resolved.model.is_none() && (resolved.vendor != role.vendor || effort.is_some()) {
        return Err(refuse(&format!(
            "add a `models.{}` pin or provide a model",
            resolved.vendor.as_str()
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
    fn model_replacement_drops_pinned_effort_unless_given() {
        let role = role();
        for vendor in [None, Some(Vendor::Codex)] {
            for effort in [None, Some("high")] {
                let resolved = apply(&role, vendor, Some("custom"), effort, Auth::Login).unwrap();
                assert_eq!(
                    resolved.model,
                    Some(Model {
                        name: "custom".into(),
                        effort: effort.map(str::to_string)
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
    fn empty_model_is_refused() {
        for model in ["", "   "] {
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
}
