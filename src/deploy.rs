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
    bus: Option<BusRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BusRaw {
    root: String,
    alert: Option<AlertRaw>,
    warn_age_days: Option<u32>,
    max_age_days: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AlertRaw {
    plugin: String,
    action: String,
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
    /// SHA-256 of the file as it was read and parsed.
    pub file_sha256: String,
    pub team_dir: PathBuf,
    pub auth: Auth,
    /// Accept Claude's folder-trust dialog for a member's declared `cwd`:
    /// `None` (the default) for `locked` members only, which ignore project
    /// settings; `Some(true)` for every member; `Some(false)` for none.
    pub auto_trust: Option<bool>,
    pub plugins_dir: Option<PathBuf>,
    pub members: BTreeMap<String, MemberDeploy>,
    pub plugins: BTreeMap<String, PluginDeploy>,
    /// The coordination bus, when this deployment uses one.
    pub bus: Option<BusConfig>,
}

/// Default age (days) at which a team's message storage is reported as aging.
pub const DEFAULT_WARN_AGE_DAYS: u32 = 7;
/// Default age (days) at which brg rolls a team's message storage over.
pub const DEFAULT_MAX_AGE_DAYS: u32 = 14;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BusConfig {
    /// The directory holding each team's message storage (absolute).
    pub root: PathBuf,
    /// The plugin action brg invokes to alert a person (never a member's).
    pub alert: Option<AlertConfig>,
    pub warn_age_days: u32,
    pub max_age_days: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AlertConfig {
    pub plugin: String,
    pub action: String,
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

    let bus = raw.bus.and_then(|b| load_bus(b, file, report));

    if report.errors().count() > before {
        return None;
    }
    Some(Deployment {
        file: file.to_path_buf(),
        file_sha256: crate::bus::manifest::sha256_hex(text.as_bytes()),
        team_dir,
        auth: raw.auth.unwrap_or_default(),
        auto_trust: raw.auto_trust,
        plugins_dir,
        members,
        plugins,
        bus,
    })
}

fn load_bus(raw: BusRaw, file: &Path, report: &mut Report) -> Option<BusConfig> {
    let before = report.errors().count();
    if !(raw.root.starts_with('/') || raw.root.starts_with("~/")) {
        report.error(
            Some(file),
            "bus: `root` must be an absolute path (or start with ~/)",
        );
    }
    if raw.root.chars().any(char::is_whitespace) {
        report.error(Some(file), "bus: `root` must not contain whitespace");
    }
    let warn = raw.warn_age_days.unwrap_or(DEFAULT_WARN_AGE_DAYS);
    let max = raw.max_age_days.unwrap_or(DEFAULT_MAX_AGE_DAYS);
    if warn == 0 || max == 0 || warn >= max {
        report.error(
            Some(file),
            format!("bus: need 0 < warn_age_days ({warn}) < max_age_days ({max})"),
        );
    }
    if report.errors().count() > before {
        return None;
    }
    let base = file.parent().unwrap_or(Path::new("."));
    Some(BusConfig {
        root: expand_home(base, &raw.root),
        alert: raw.alert.map(|a| AlertConfig {
            plugin: a.plugin,
            action: a.action,
        }),
        warn_age_days: warn,
        max_age_days: max,
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
