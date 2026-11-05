//! Run identity: the manifest written when a run starts, runtime
//! generations (which binary/state/socket currently serves the run) and
//! member incarnations (one per member start).
//!
//! A run's identity (ID, schema, frozen topology and configuration digest)
//! never changes. The runtime serving it can: each generation is recorded,
//! and moving to a new one is an explicit rebind.

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

/// One runtime generation: `runtime/generations/<n>.json`, immutable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Generation {
    pub n: u64,
    pub at: String,
    /// Where the binary was when the generation was recorded.
    pub binary: PathBuf,
    pub binary_sha256: String,
    /// The run's own copy of that binary (`runtime/bin/<sha256>/brg`), which
    /// every window, shim, member control call and worker of this
    /// generation runs, so rebuilding the original can't change it.
    pub pinned: PathBuf,
    pub state_dir: PathBuf,
    pub socket: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<u64>,
    /// Why this generation exists (`start`, or the rebind's stated reason).
    pub reason: String,
}

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
    pub generation: u64,
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
    /// The configuration snapshot: SHA-256 of each input file's bytes and of
    /// each member's expanded instruction, combined under a versioned header.
    pub fn config_snapshot(&self) -> ConfigSnapshot {
        let file_sha = |p: &std::path::Path| {
            std::fs::read(p)
                .map(|b| hex(&Sha256::digest(b)))
                .unwrap_or_else(|_| "unreadable".to_string())
        };
        let mut inputs = BTreeMap::new();
        if let Some(d) = &self.deployment {
            inputs.insert("deployment".to_string(), file_sha(&d.file));
        }
        inputs.insert(
            crate::team::TEAM_FILE.to_string(),
            file_sha(&self.team.dir.join(crate::team::TEAM_FILE)),
        );
        for (id, m) in &self.team.members {
            inputs.insert(format!("members/{id}/prompt"), file_sha(&m.prompt_file));
            inputs.insert(
                format!("members/{id}/instruction"),
                hex(&Sha256::digest(m.instruction.as_bytes())),
            );
        }
        let mut text = format!("borgchart config digest v{CONFIG_DIGEST_VERSION}\n");
        for (name, sha) in &inputs {
            text.push_str(&format!("{name} {sha}\n"));
        }
        ConfigSnapshot {
            version: CONFIG_DIGEST_VERSION,
            digest: hex(&Sha256::digest(text.as_bytes())),
            inputs,
        }
    }
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
pub fn load_manifest(paths: &RunPaths) -> Result<Option<RunManifest>, String> {
    let m: Option<RunManifest> = read_json(&paths.manifest())?;
    if let Some(m) = &m
        && m.schema != super::SCHEMA
    {
        return Err(format!(
            "run `{}` has schema {}, this brg reads schema {}",
            m.run,
            m.schema,
            super::SCHEMA
        ));
    }
    Ok(m)
}

pub fn load_incarnation(paths: &RunPaths, member: &str) -> Result<Option<Incarnation>, String> {
    read_json(&paths.current_incarnation(member))
}

pub fn load_generation(paths: &RunPaths, n: u64) -> Result<Option<Generation>, String> {
    read_json(&paths.generation(n))
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
}
