//! The deployment file, `borgchart.yaml`: values for one deployment of a team.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::diag::Report;
use crate::team::{Posture, parse_posture};
use crate::yaml;

pub const DEPLOY_FILE: &str = "borgchart.yaml";
pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Auth {
    /// Members use your vendor login; no API-key variables are passed.
    #[default]
    Login,
    /// Members get `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` by name.
    ApiKey,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeploymentRaw {
    schema: u32,
    team: Option<String>,
    auth: Option<Auth>,
    auto_trust: Option<bool>,
    plugins_dir: Option<String>,
    #[serde(default)]
    members: BTreeMap<String, MemberDeployRaw>,
    #[serde(default)]
    plugins: BTreeMap<String, PluginDeployRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemberDeployRaw {
    task: Option<String>,
    cwd: Option<String>,
    posture: Option<String>,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginDeployRaw {
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Deployment {
    pub file: PathBuf,
    pub team_dir: PathBuf,
    pub auth: Auth,
    /// Accept Claude's folder-trust dialog for a member's declared `cwd`.
    pub auto_trust: bool,
    pub plugins_dir: Option<PathBuf>,
    pub members: BTreeMap<String, MemberDeploy>,
    pub plugins: BTreeMap<String, PluginDeploy>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MemberDeploy {
    pub task: Option<String>,
    pub cwd: Option<PathBuf>,
    pub posture: Option<Posture>,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PluginDeploy {
    pub env: BTreeMap<String, String>,
}

/// Load `borgchart.yaml`. Relative paths in it are relative to its directory.
pub fn load(file: &Path, report: &mut Report) -> Option<Deployment> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) => {
            report.error(Some(file), format!("cannot read: {e}"));
            return None;
        }
    };
    let raw: DeploymentRaw = match yaml::parse(&text) {
        Ok(r) => r,
        Err(e) => {
            report.error(Some(file), e);
            return None;
        }
    };
    let before = report.errors().count();
    if raw.schema != SCHEMA {
        report.error(
            Some(file),
            format!(
                "unsupported `schema: {}` (this brg reads schema {SCHEMA})",
                raw.schema
            ),
        );
    }
    let base = file.parent().unwrap_or(Path::new("."));
    let team_dir = base.join(raw.team.as_deref().unwrap_or("."));
    let plugins_dir = raw.plugins_dir.as_deref().map(|p| expand_home(base, p));

    let mut members = BTreeMap::new();
    for (id, m) in raw.members {
        let posture = match m.posture.as_deref().map(parse_posture).transpose() {
            Ok(p) => p,
            Err(e) => {
                report.error(Some(file), format!("member `{id}`: {e}"));
                None
            }
        };
        members.insert(
            id,
            MemberDeploy {
                task: m.task,
                cwd: m.cwd.as_deref().map(|c| expand_home(base, c)),
                posture,
                args: m.args,
            },
        );
    }
    let plugins = raw
        .plugins
        .into_iter()
        .map(|(name, p)| (name, PluginDeploy { env: p.env }))
        .collect();

    if report.errors().count() > before {
        return None;
    }
    Some(Deployment {
        file: file.to_path_buf(),
        team_dir,
        auth: raw.auth.unwrap_or_default(),
        auto_trust: raw.auto_trust.unwrap_or(true),
        plugins_dir,
        members,
        plugins,
    })
}

fn expand_home(base: &Path, value: &str) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    base.join(value)
}
