//! Run identity: the manifest written when a run starts, and member
//! incarnations (one per member start).
//!
//! A run is internal: one stretch of a instance's message storage with a fixed
//! message format and frozen configuration. Users never see or name runs;
//! brg starts a new one (carrying unfinished work) when the configuration
//! or format changes.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::validate::Validated;

use super::layout::RunPaths;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunManifest {
    pub schema: u32,
    pub run: String,
    pub created_at: String,
    /// The instance (`brg-<instance>` is its tmux session).
    pub instance: String,
    /// The team folder the run was started from.
    pub source: PathBuf,
    /// What configuration the run was started with.
    pub config: ConfigSnapshot,
    /// The frozen topology, by role. A role's `parent` names a role.
    pub roles: BTreeMap<String, ManifestMember>,
    /// The instance's members, by ID, each with its role's entry and its
    /// parent as a member ID: filled from the instance's record when the
    /// storage is opened, never stored. A parent role with no member yet
    /// stays named by its role, which no caller can act as.
    #[serde(skip)]
    pub members: BTreeMap<String, ManifestMember>,
    /// Set when this run continues an earlier one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_from: Option<LinkedRun>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestMember {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub vendor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// `"<model>"` or `"<model> <effort>"`, as declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkedRun {
    pub run: String,
    pub root: PathBuf,
    pub reason: String,
}

/// A digest of the run's configuration inputs, computed from their bytes
/// (not from brg's internal types, so a compatible brg upgrade computes the
/// same digest for the same files).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigSnapshot {
    pub version: u32,
    pub digest: String,
    /// Input name → SHA-256: `team.yaml`, `members/<role>/prompt` (the
    /// role file), `members/<role>/instruction` (its body with includes
    /// expanded), and the global settings that reach members:
    /// `config/auth`, `config/auto_trust` and `config/plugins/<name>` (where
    /// a shared plugin resolves).
    pub inputs: BTreeMap<String, String>,
}

pub const CONFIG_DIGEST_VERSION: u32 = 1;

/// A member's current incarnation: `runtime/incarnations/<member>.json`.
/// Written when the member starts (before its window exists, with no
/// `pane`), again once its pane is known, and again with `ended_at` when it
/// is stopped through brg.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Incarnation {
    pub member: String,
    /// `[a-z0-9._-]`, unique within the run.
    pub incarnation: String,
    pub started_at: String,
    /// The vendor CLI version measured at this start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<PaneIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_reason: Option<String>,
}

/// Which tmux pane an incarnation runs in. A pane whose `pane_id` or
/// `pane_pid` differs is a different process, whatever its window is called.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneIdentity {
    pub socket: String,
    pub session: String,
    pub window: String,
    /// tmux's `%N`.
    pub pane_id: String,
    pub pane_pid: u32,
}

/// Whether an incarnation's process may still be running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Liveness {
    /// Its pane exists with the recorded ID and PID, and is not dead.
    Alive,
    /// Its pane is gone or dead, or it ended through brg.
    Gone,
    /// It can't be told (no pane recorded yet, tmux unreachable): treat as
    /// possibly alive. Never free its checkout on this.
    Unknown,
}

impl Validated {
    /// The configuration snapshot: the SHA-256 of each input file's bytes as
    /// they were read and validated, and of each member's expanded
    /// instruction, combined under a versioned header. Nothing is re-read,
    /// so the snapshot always describes this validated configuration.
    pub fn config_snapshot(&self) -> ConfigSnapshot {
        let mut inputs = BTreeMap::new();
        inputs.insert(
            "config/auth".to_string(),
            sha256_hex(self.config.auth.as_str().as_bytes()),
        );
        inputs.insert(
            "config/auto_trust".to_string(),
            sha256_hex(format!("{:?}", self.config.auto_trust).as_bytes()),
        );
        for (name, plugin) in &self.team.plugins {
            if matches!(
                plugin.location,
                Some(crate::team::PluginLocation::Shared(_))
            ) {
                let dir = self.plugin_dir(name).unwrap_or_default();
                inputs.insert(
                    format!("config/plugins/{name}"),
                    sha256_hex(dir.as_os_str().as_encoded_bytes()),
                );
            }
        }
        inputs.insert(
            crate::team::TEAM_FILE.to_string(),
            self.team.file_sha256.clone(),
        );
        for (id, m) in &self.team.members {
            inputs.insert(format!("members/{id}/prompt"), m.prompt_sha256.clone());
            inputs.insert(
                format!("members/{id}/instruction"),
                sha256_hex(m.instruction.as_bytes()),
            );
        }
        let mut text = format!("borgchart config digest v{CONFIG_DIGEST_VERSION}\n");
        for (name, sha) in &inputs {
            text.push_str(&format!("{name} {sha}\n"));
        }
        ConfigSnapshot {
            version: CONFIG_DIGEST_VERSION,
            digest: sha256_hex(text.as_bytes()),
            inputs,
        }
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The manifest a new run of `v` gets.
pub fn manifest_for(
    v: &Validated,
    run: &str,
    instance: &str,
    source: PathBuf,
    created_at: String,
) -> RunManifest {
    let roles = v
        .team
        .members
        .iter()
        .map(|(id, m)| {
            (
                id.clone(),
                ManifestMember {
                    role: None,
                    vendor: m.vendor.as_str().into(),
                    parent: m.parent.clone(),
                    model: m.model.as_ref().map(|model| match &model.effort {
                        Some(e) => format!("{} {e}", model.name),
                        None => model.name.clone(),
                    }),
                },
            )
        })
        .collect();
    RunManifest {
        schema: super::SCHEMA,
        run: run.to_string(),
        created_at,
        instance: instance.to_string(),
        source,
        config: v.config_snapshot(),
        roles,
        members: BTreeMap::new(),
        linked_from: None,
    }
}

impl RunManifest {
    /// Fill [`RunManifest::members`] from the instance's issued members.
    pub fn resolve_members(&mut self, issued: &BTreeMap<String, String>) {
        let member_of = |role: &str| {
            issued
                .iter()
                .find(|(_, r)| r.as_str() == role)
                .map(|(id, _)| id.clone())
        };
        self.members = issued
            .iter()
            .filter_map(|(id, role)| {
                let entry = self.roles.get(role)?;
                let parent = entry
                    .parent
                    .as_ref()
                    .map(|p| member_of(p).unwrap_or_else(|| p.clone()));
                Some((
                    id.clone(),
                    ManifestMember {
                        role: Some(role.clone()),
                        vendor: entry.vendor.clone(),
                        parent,
                        model: entry.model.clone(),
                    },
                ))
            })
            .collect();
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &std::path::Path) -> Result<Option<T>, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("{} is unreadable: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// The run's manifest, `None` if the run hasn't started.
/// Why a run's manifest couldn't be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    /// The run's messages use a format this brg doesn't read.
    Format {
        found: u64,
    },
    Unreadable(String),
}

impl ManifestError {
    /// The message to show, in terms of the instance. `restart` is how to
    /// restart this team (see `state::restart_hint`).
    pub fn message(&self, team: &str, _restart: &str) -> String {
        match self {
            ManifestError::Format { found } => format_mismatch(team, *found),
            ManifestError::Unreadable(e) => e.clone(),
        }
    }
}

/// "instance `oncall-2a3b`'s messages use format 1; this brg uses format 2. …"
///
/// This brg can't read messages in another format, nor check them for open
/// work, so the instance can't start again: a new instance has fresh storage.
/// The old messages are left where they are.
pub fn format_mismatch(team: &str, found: u64) -> String {
    format!(
        "instance `{team}`'s messages use format {found}; this brg uses format {}.\nbrg can't read them or check them for open work, so the instance can't start again; create a new instance for fresh storage: brg create <team> <folder>\n(The old messages stay on disk; send their open work again.)",
        super::SCHEMA
    )
}

/// The run's manifest, `None` if the run hasn't started.
pub fn load_manifest(paths: &RunPaths) -> Result<Option<RunManifest>, ManifestError> {
    // Read the format first, so a manifest from another format (whose other
    // fields may differ) is reported as such rather than as unreadable.
    let bytes = match std::fs::read(paths.manifest()) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(ManifestError::Unreadable(format!(
                "cannot read {}: {e}",
                paths.manifest().display()
            )));
        }
    };
    let schema = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|v| v.get("schema").and_then(|s| s.as_u64()));
    if let Some(found) = schema
        && found != super::SCHEMA as u64
    {
        return Err(ManifestError::Format { found });
    }
    serde_json::from_slice(&bytes).map(Some).map_err(|e| {
        ManifestError::Unreadable(format!("{} is unreadable: {e}", paths.manifest().display()))
    })
}

pub fn load_incarnation(paths: &RunPaths, member: &str) -> Result<Option<Incarnation>, String> {
    read_json(&paths.current_incarnation(member))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_round_trips() {
        let mut members = BTreeMap::new();
        members.insert(
            "helper".to_string(),
            ManifestMember {
                role: None,
                vendor: "claude".into(),
                parent: Some("lead".into()),
                model: Some("example-model medium".into()),
            },
        );
        let m = RunManifest {
            schema: 1,
            run: "r1".into(),
            created_at: "2026-11-05T19:00:00.000Z".into(),
            instance: "team-2a3b".into(),
            source: "/tmp/example/team".into(),
            config: ConfigSnapshot {
                version: CONFIG_DIGEST_VERSION,
                digest: "00".into(),
                inputs: BTreeMap::new(),
            },
            roles: members,
            members: BTreeMap::new(),
            linked_from: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let paths = RunPaths::from_root(dir.path());
        assert_eq!(load_manifest(&paths).unwrap(), None);
        super::super::fsx::write_new(&paths.manifest(), &serde_json::to_vec(&m).unwrap()).unwrap();
        assert_eq!(load_manifest(&paths).unwrap(), Some(m));
    }

    #[test]
    fn another_format_is_reported_in_team_terms() {
        let dir = tempfile::tempdir().unwrap();
        let paths = RunPaths::from_root(dir.path());
        std::fs::write(paths.manifest(), r#"{"schema": 2, "whatever": true}"#).unwrap();
        let err = load_manifest(&paths).unwrap_err();
        assert_eq!(err, ManifestError::Format { found: 2 });
        let msg = err.message("oncall", "brg down && brg up");
        let big = RunPaths::from_root(&dir.path().join("big"));
        std::fs::create_dir(&big.root).unwrap();
        std::fs::write(big.manifest(), r#"{"schema": 4294967297}"#).unwrap();
        let big_msg = load_manifest(&big).unwrap_err().message("oncall", "x");
        assert!(
            big_msg.contains("use format 4294967297; this brg uses format 1"),
            "{big_msg}"
        );
        assert!(
            msg.contains("instance `oncall`'s messages use format 2; this brg uses format 1"),
            "{msg}"
        );
        assert!(msg.contains("brg create <team> <folder>"), "{msg}");
        assert!(!super::super::mentions_runs(&msg), "no run wording: {msg}");
    }
}
