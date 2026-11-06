//! Where a deployment's runtime state lives, and the private files in it.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::bus::manifest::ConfigSnapshot;

/// Overrides for tests and unusual setups.
pub const ENV_STATE_HOME: &str = "BORGCHART_STATE_HOME";
pub const ENV_TMUX_SOCKET: &str = "BORGCHART_TMUX_SOCKET";
/// Set inside members and plugins: the deployment file, so `brg` run there finds it.
pub const ENV_FILE: &str = "BORGCHART_FILE";
pub const DEFAULT_SOCKET: &str = "borgchart";

/// The root for all deployments' state.
pub fn state_home() -> PathBuf {
    if let Some(dir) = std::env::var_os(ENV_STATE_HOME) {
        return PathBuf::from(dir);
    }
    if let Some(dir) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(dir).join("borgchart");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join(".local/state/borgchart")
}

pub fn tmux_socket() -> String {
    std::env::var(ENV_TMUX_SOCKET).unwrap_or_else(|_| DEFAULT_SOCKET.to_string())
}

/// How to restart a team, as a command line that works from where the
/// person is: the bare commands in the deployment's own folder, otherwise
/// with its `--file` (or `--team`).
pub fn restart_hint(source: &Path) -> String {
    let canonical = source
        .canonicalize()
        .unwrap_or_else(|_| source.to_path_buf());
    let dir = canonical
        .parent()
        .filter(|_| canonical.is_file())
        .unwrap_or(&canonical);
    let here = std::env::current_dir()
        .ok()
        .and_then(|c| c.canonicalize().ok());
    if here.as_deref() == Some(dir) {
        return "brg down && brg up".to_string();
    }
    let flag = if canonical.is_file() {
        "--file"
    } else {
        "--team"
    };
    let path = shell_quote(&canonical.display().to_string());
    format!("brg down {flag} {path} && brg up {flag} {path}")
}

/// The name a team goes by in messages: its deployment's directory name,
/// lowercased, with anything outside `[a-z0-9_-]` turned into `-`.
pub fn team_name(source: &Path) -> String {
    let canonical = source
        .canonicalize()
        .unwrap_or_else(|_| source.to_path_buf());
    let dir = canonical
        .parent()
        .filter(|_| canonical.is_file())
        .unwrap_or(&canonical);
    let clean: String = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(32)
        .collect();
    if clean.is_empty() {
        "team".to_string()
    } else {
        clean
    }
}

/// A stable id for a deployment: its directory name plus a short hash of its path.
pub fn deployment_id(source: &Path) -> String {
    let canonical = source
        .canonicalize()
        .unwrap_or_else(|_| source.to_path_buf());
    let digest = Sha256::digest(canonical.as_os_str().as_encoded_bytes());
    let hex: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    let stem = canonical
        .parent()
        .filter(|_| canonical.is_file())
        .unwrap_or(&canonical)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "team".to_string());
    let clean: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(24)
        .collect();
    let clean = if clean.is_empty() {
        "team".to_string()
    } else {
        clean
    };
    format!("{clean}-{hex}")
}

/// Paths inside one deployment's state directory.
#[derive(Debug, Clone)]
pub struct StateDir {
    pub root: PathBuf,
    pub id: String,
}

impl StateDir {
    pub fn new(id: &str) -> Self {
        StateDir {
            root: state_home().join(id),
            id: id.to_string(),
        }
    }

    pub fn from_root(root: &Path) -> Self {
        let id = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        StateDir {
            root: root.to_path_buf(),
            id,
        }
    }

    pub fn session(&self) -> String {
        format!("brg-{}", self.id)
    }

    pub fn meta_file(&self) -> PathBuf {
        self.root.join("deployment.json")
    }
    pub fn exec_file(&self, window: &str) -> PathBuf {
        self.root.join("exec").join(format!("{window}.json"))
    }
    pub fn member_dir(&self, member: &str) -> PathBuf {
        self.root.join("members").join(member)
    }
    pub fn member_bin(&self, member: &str) -> PathBuf {
        self.member_dir(member).join("bin")
    }
    pub fn gate_file(&self, member: &str) -> PathBuf {
        self.member_dir(member).join("gate.json")
    }
    pub fn gate_marker(&self, member: &str) -> PathBuf {
        self.member_dir(member).join("gate-active")
    }
    pub fn plugin_dir(&self, plugin: &str) -> PathBuf {
        self.root.join("plugins").join(plugin)
    }
    /// A plugin's values (secrets included), kept apart from its writable data dir.
    pub fn plugin_env_file(&self, plugin: &str) -> PathBuf {
        self.root.join("secrets").join(format!("{plugin}.json"))
    }
    /// A member's own temp directory (its TMPDIR), apart from its enforcement files.
    pub fn member_tmp(&self, member: &str) -> PathBuf {
        self.root.join("tmp").join(member)
    }
    /// Held by `up` and `down` while they change the deployment.
    pub fn lifecycle_lock(&self) -> PathBuf {
        self.root.join("lifecycle.lock")
    }
    pub fn lock_file(&self, member: &str) -> PathBuf {
        self.root.join("locks").join(format!("{member}.lock"))
    }
    pub fn tmux_conf(&self) -> PathBuf {
        state_home().join("tmux.conf")
    }
}

/// Facts written at `up` and read by `status`, `send`, shims and the gate.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Meta {
    pub source: PathBuf,
    pub team_dir: PathBuf,
    /// The absolute path of the `brg` that started the deployment (plugins
    /// get it as `BORGCHART_BIN`).
    pub brg: PathBuf,
    /// The configuration the deployment started with. Members started into
    /// it later must see the same.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigSnapshot>,
    /// The run (internal) holding the team's messages, if it has a bus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunBinding>,
    /// The tmux socket name the deployment was started on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socket: Option<String>,
    /// The tmux server's absolute socket path, read from tmux once the
    /// first window started. `down` and liveness address the
    /// server by this, never by the caller's tmux environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socket_path: Option<PathBuf>,
    /// Set by a `down` that stopped the whole deployment (or found it gone
    /// on a server that answered); cleared by the next fresh `up`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<String>,
    /// The small environment every window and plugin starts from (no secrets).
    pub base_env: BTreeMap<String, String>,
    pub members: BTreeMap<String, MemberMeta>,
    pub plugins: BTreeMap<String, PluginMeta>,
    /// The bus identity and alert action as configured at `up`; bus commands
    /// that act for the deployment use these and refuse a changed file.
    #[serde(default)]
    pub bus: Option<BusMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunBinding {
    pub run: String,
    /// The run directory, `<bus.root>/<run>`.
    pub dir: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemberMeta {
    pub vendor: String,
    pub posture: String,
    pub parent: Option<String>,
    pub vendor_version: Option<String>,
    /// Whether `up` has started the member's window since the team last
    /// started afresh (`status` tells `not started` from `stopped` by it
    /// when the team has no bus). `None` in state from an earlier brg,
    /// which didn't record it: history unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusMeta {
    pub root: PathBuf,
    /// `(plugin, action)` of `bus.alert`.
    pub alert: Option<(String, String)>,
}

impl BusMeta {
    pub fn of(config: &crate::deploy::BusConfig) -> Self {
        BusMeta {
            root: config.root.clone(),
            alert: config
                .alert
                .as_ref()
                .map(|a| (a.plugin.clone(), a.action.clone())),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginMeta {
    /// The plugin's directory, or `None` when its commands come from `PATH`.
    pub dir: Option<PathBuf>,
    pub has_process: bool,
    /// Each action's literal prefix (command first), as declared at `up`, so
    /// brg-invoked actions (alerts) use the frozen declaration.
    #[serde(default)]
    pub actions: BTreeMap<String, Vec<String>>,
}

pub fn read_meta(state: &StateDir) -> Option<Meta> {
    let text = fs::read_to_string(state.meta_file()).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn ensure_dir(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

/// Write a file readable only by you, replacing it atomically.
pub fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let tmp = path.with_extension("tmp");
    let _ = fs::remove_file(&tmp);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(contents)?;
    file.sync_all()?;
    fs::rename(&tmp, path)
}

/// Open (creating if needed) a member's send-lock file without following
/// symlinks or truncating, and check it is a regular file.
pub fn open_lock(path: &Path) -> std::io::Result<fs::File> {
    let nofollow = rustix::fs::OFlags::NOFOLLOW.bits() as i32;
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(nofollow)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                ensure_dir(parent)?;
            }
            fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(nofollow)
                .open(path)?
        }
        Err(e) => return Err(e),
    };
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("lock path is not a regular file"));
    }
    Ok(file)
}

/// Write an executable script (not secret: it holds paths only).
pub fn write_script(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    fs::write(path, contents)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

/// Quote a value for a POSIX shell.
pub fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._:/@%+=,-".contains(c))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("plain/path-1"), "plain/path-1");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn restart_hints_work_from_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("borgchart.yaml");
        std::fs::write(&file, "schema: 1\n").unwrap();
        let real = file.canonicalize().unwrap();
        let q = shell_quote(&real.display().to_string());
        assert_eq!(
            restart_hint(&file),
            format!("brg down --file {q} && brg up --file {q}")
        );
    }

    #[test]
    fn team_names_come_from_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let team = dir.path().join("On Call");
        std::fs::create_dir(&team).unwrap();
        let file = team.join("borgchart.yaml");
        std::fs::write(&file, "schema: 1\n").unwrap();
        assert_eq!(team_name(&file), "on-call");
        assert_eq!(team_name(&team), "on-call");
    }

    #[test]
    fn ids_are_stable_and_tidy() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("borgchart.yaml");
        std::fs::write(&file, "schema: 1\n").unwrap();
        let a = deployment_id(&file);
        assert_eq!(a, deployment_id(&file));
        assert!(
            a.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        );
    }
}
