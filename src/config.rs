//! brg's global configuration, `~/.config/borgchart/config.yaml` (or
//! `$BORGCHART_CONFIG`): what belongs to you rather than to a template.
//!
//! The file is optional; a missing one means the defaults. Relative paths in
//! it are relative to its folder, and `~/` is your home.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::diag::Report;
use crate::yaml;

pub const ENV_CONFIG: &str = "BORGCHART_CONFIG";
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

impl Auth {
    pub fn as_str(self) -> &'static str {
        match self {
            Auth::Login => "login",
            Auth::ApiKey => "api_key",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigRaw {
    schema: u32,
    auth: Option<Auth>,
    auto_trust: Option<bool>,
    #[serde(rename = "templates")]
    teams: Option<Vec<String>>,
    plugins_dir: Option<String>,
    tmux: Option<TmuxRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TmuxRaw {
    user_config: Option<UserConfigRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(
    untagged,
    expecting = "`tmux.user_config` must be true, false or a path to a tmux config"
)]
enum UserConfigRaw {
    On(bool),
    Path(String),
}

/// Which of your own tmux configs brg's server loads before its own options.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum UserTmuxConfig {
    /// `true`: the first of tmux's own per-user config files that exists.
    Default,
    /// A path (absolute once loaded).
    Path(PathBuf),
}

impl UserTmuxConfig {
    /// The file to load, if there is one.
    pub fn resolve(&self) -> Option<PathBuf> {
        match self {
            UserTmuxConfig::Path(p) => Some(p.clone()),
            UserTmuxConfig::Default => {
                let home = std::env::var_os("HOME").map(PathBuf::from);
                let xdg = std::env::var_os("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute());
                [
                    home.as_ref().map(|h| h.join(".tmux.conf")),
                    xdg.map(|x| x.join("tmux/tmux.conf")),
                    home.map(|h| h.join(".config/tmux/tmux.conf")),
                ]
                .into_iter()
                .flatten()
                .find(|p| p.is_file())
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Config {
    /// The file it came from, when there is one.
    pub file: Option<PathBuf>,
    pub auth: Auth,
    /// Trust each instance's folder without a person. Claude: brg answers its
    /// folder-trust dialog; `None` (the default) for `locked` members only,
    /// which ignore project settings; `Some(true)` for every member;
    /// `Some(false)` for none. Codex: session config, only with `Some(true)`
    /// and only in `vendor` posture.
    pub auto_trust: Option<bool>,
    /// Where template names are looked up, in order.
    #[serde(rename = "templates")]
    pub teams: Vec<PathBuf>,
    /// Where `shared:<name>` plugins live.
    pub plugins_dir: Option<PathBuf>,
    /// Your own tmux config, loaded on brg's server when you opt in.
    pub tmux_user_config: Option<UserTmuxConfig>,
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// `~/.config/borgchart`.
fn default_dir() -> PathBuf {
    home()
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".config/borgchart")
}

/// The configuration file's path: `$BORGCHART_CONFIG`, else
/// `~/.config/borgchart/config.yaml`.
pub fn path() -> PathBuf {
    match std::env::var_os(ENV_CONFIG).filter(|v| !v.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => default_dir().join("config.yaml"),
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            file: None,
            auth: Auth::default(),
            auto_trust: None,
            teams: vec![default_dir().join("templates")],
            plugins_dir: None,
            tmux_user_config: None,
        }
    }
}

/// `~/x` is in your home; anything else relative is relative to `base`.
pub fn expand_home(base: &Path, value: &str) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/")
        && let Some(home) = home()
    {
        return home.join(rest);
    }
    base.join(value)
}

/// Load the configuration. A missing file means the defaults; problems go
/// into `report`, and `None` means there were errors.
pub fn load(report: &mut Report) -> Option<Config> {
    load_file(&path(), report)
}

/// Load the configuration from `file` (a missing file means the defaults).
pub fn load_file(file: &Path, report: &mut Report) -> Option<Config> {
    let file = file.to_path_buf();
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(Config::default()),
        Err(e) => {
            report.error(Some(&file), format!("cannot read: {e}"));
            return None;
        }
    };
    let raw: ConfigRaw = match yaml::parse(&text) {
        Ok(r) => r,
        Err(e) => {
            let message = if e.contains("unknown field `teams`") {
                "`teams:` is no longer read; rename it to `templates:`".to_string()
            } else {
                e
            };
            report.error(Some(&file), message);
            return None;
        }
    };
    if raw.schema != SCHEMA {
        report.error(
            Some(&file),
            format!(
                "unsupported `schema: {}` (this brg reads schema {SCHEMA})",
                raw.schema
            ),
        );
        return None;
    }
    let base = file.parent().unwrap_or(Path::new("."));
    let defaults = Config::default();
    let teams = match raw.teams {
        Some(list) => list.iter().map(|t| expand_home(base, t)).collect(),
        None => defaults.teams,
    };
    let tmux_user_config = match raw.tmux.and_then(|t| t.user_config) {
        None | Some(UserConfigRaw::On(false)) => None,
        Some(UserConfigRaw::On(true)) => Some(UserTmuxConfig::Default),
        Some(UserConfigRaw::Path(p)) => Some(UserTmuxConfig::Path(expand_home(base, &p))),
    };
    Some(Config {
        file: Some(file.clone()),
        auth: raw.auth.unwrap_or_default(),
        auto_trust: raw.auto_trust,
        teams,
        plugins_dir: raw.plugins_dir.as_deref().map(|p| expand_home(base, p)),
        tmux_user_config,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_and_relative_paths() {
        let base = Path::new("/etc/brg");
        assert_eq!(
            expand_home(base, "templates"),
            PathBuf::from("/etc/brg/templates")
        );
        assert_eq!(expand_home(base, "/abs"), PathBuf::from("/abs"));
    }
}
