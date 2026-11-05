//! Run identity: the manifest written when a run starts, and member
//! incarnations (one per member start).
//!
//! A run is internal: one stretch of a team's message storage with a fixed
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
    /// The deployment ID (`brg-<id>` is its tmux session).
    pub deployment: String,
    /// The deployment file or team directory the run was started from.
    pub source: PathBuf,
    /// What configuration the run was started with.
    pub config: ConfigSnapshot,
    /// The frozen topology.
    pub members: BTreeMap<String, ManifestMember>,
    /// Set when this run continues an earlier one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_from: Option<LinkedRun>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestMember {
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
    /// Input name → SHA-256: `deployment`, `team.yaml`,
    /// `members/<id>/prompt` (the member file) and `members/<id>/instruction`
    /// (its body with includes expanded).
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
        if let Some(d) = &self.deployment {
            inputs.insert("deployment".to_string(), d.file_sha256.clone());
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
    deployment: &str,
    source: PathBuf,
    created_at: String,
) -> RunManifest {
    let members = v
        .team
        .members
        .iter()
        .map(|(id, m)| {
            (
                id.clone(),
                ManifestMember {
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
        deployment: deployment.to_string(),
        source,
        config: v.config_snapshot(),
        members,
        linked_from: None,
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
        found: u32,
    },
    Unreadable(String),
}

impl ManifestError {
    /// The message to show, in terms of the team. `restart` is how to
    /// restart this team (see `state::restart_hint`).
    pub fn message(&self, team: &str, restart: &str) -> String {
        match self {
            ManifestError::Format { found } => format_mismatch(team, *found, restart),
            ManifestError::Unreadable(e) => e.clone(),
        }
    }
}

/// "team `oncall`'s messages use format 1; this brg uses format 2. …"
///
/// Restarting starts fresh storage in this brg's format. Messages in the old
/// format can't be read by this brg, so they are left where they are rather
/// than carried over; `up` lists what was left behind.
pub fn format_mismatch(team: &str, found: u32, restart: &str) -> String {
    format!(
        "team `{team}`'s messages use format {found}; this brg uses format {}.\nRestart the team to start fresh storage in the new format: {restart}\n(Open work in the old format isn't carried over; the old messages stay on disk.)",
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
        return Err(ManifestError::Format {
            found: found as u32,
        });
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
                vendor: "claude".into(),
                parent: Some("lead".into()),
                model: Some("example-model medium".into()),
            },
        );
        let m = RunManifest {
            schema: 1,
            run: "r1".into(),
            created_at: "2026-11-05T19:00:00.000Z".into(),
            deployment: "team-0a1b2c3d".into(),
            source: "/tmp/example/borgchart.yaml".into(),
            config: ConfigSnapshot {
                version: CONFIG_DIGEST_VERSION,
                digest: "00".into(),
                inputs: BTreeMap::new(),
            },
            members,
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
        assert!(
            msg.contains("team `oncall`'s messages use format 2; this brg uses format 1"),
            "{msg}"
        );
        assert!(msg.contains("brg down && brg up"), "{msg}");
        assert!(!super::super::mentions_runs(&msg), "no run wording: {msg}");
    }
}
