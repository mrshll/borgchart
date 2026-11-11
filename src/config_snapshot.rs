//! The configuration bytes recorded at an instance's start.

use crate::validate::Validated;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// A digest of the instance's configuration inputs, computed from their bytes
/// (not from brg's internal types, so a compatible brg upgrade computes the
/// same digest for the same files).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigSnapshot {
    pub version: u32,
    pub digest: String,
    /// Input name → SHA-256: `team.yaml`, `members/<role>/prompt` (the
    /// role file), `members/<role>/instruction` (its body with includes
    /// expanded), and the global settings that reach members:
    /// `config/auth`, `config/auto_trust` and `config/plugins/<name>` (where
    /// a shared plugin resolves), plus `teams/<role>` (the resolved source
    /// folder when this snapshot was taken).
    pub inputs: BTreeMap<String, String>,
    /// Recorded canonical sources, used to observe and manage the declaring
    /// instance when a referenced team is unavailable in the current catalog.
    pub team_dirs: BTreeMap<String, std::path::PathBuf>,
}

pub const CONFIG_DIGEST_VERSION: u32 = 1;

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
                Some(
                    crate::team::PluginLocation::Shared(_)
                        | crate::team::PluginLocation::Catalog(_)
                )
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
        if let Some(dir) = &self.team.catalog_includes {
            inputs.insert(
                "config/includes".into(),
                sha256_hex(dir.as_os_str().as_encoded_bytes()),
            );
        }
        for (id, role) in &self.team.team_roles {
            inputs.insert(
                format!("teams/{id}"),
                sha256_hex(role.dir.as_os_str().as_encoded_bytes()),
            );
        }
        for (id, m) in &self.team.roles {
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
            team_dirs: self
                .team
                .team_roles
                .iter()
                .map(|(id, role)| (id.clone(), role.dir.clone()))
                .collect(),
        }
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
