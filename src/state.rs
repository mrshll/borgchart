//! Where a team's state lives, and the private files in it.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config_snapshot::ConfigSnapshot;

/// Overrides for tests and unusual setups.
pub const ENV_STATE_HOME: &str = "BORGCHART_STATE_HOME";
pub const ENV_TMUX_SOCKET: &str = "BORGCHART_TMUX_SOCKET";
/// The directory brg's tmux servers keep their sockets in, passed to tmux as
/// `TMUX_TMPDIR`. brg never takes `TMUX_TMPDIR` itself from your environment:
/// a member's own setting must not move its team's server.
pub const ENV_TMUX_TMPDIR: &str = "BORGCHART_TMUX_TMPDIR";
pub const DEFAULT_SOCKET: &str = "borgchart";

/// brg's state folder, holding the registry and every team's state folder.
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

/// Whether `path` is somewhere cleaned by age (`/tmp`, `/var/tmp`), where a
/// team's messages could vanish. Test builds may use it.
pub fn age_cleaned(path: &Path) -> bool {
    let real = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    ["/tmp", "/var/tmp", "/private/tmp", "/private/var/tmp"]
        .iter()
        .any(|dir| path.starts_with(dir) || real.starts_with(dir))
}

/// Refuse a state home that age-based cleaning would empty, except in test
/// builds.
pub fn check_state_home() -> Result<(), String> {
    let home = state_home();
    if cfg!(feature = "test-support") || !age_cleaned(&home) {
        return Ok(());
    }
    Err(format!(
        "brg's state folder {} is under a folder cleaned by age, so teams' messages could vanish; set {ENV_STATE_HOME} to a lasting folder",
        home.display()
    ))
}

/// How to restart a team: stop it, then start it.
pub fn restart_hint(instance: &str) -> String {
    format!("brg stop {instance} && brg start {instance}")
}

/// Paths inside one team's state folder, `<state home>/<team>/`.
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
        let mut state = StateDir {
            root: root.to_path_buf(),
            id,
        };
        if let Ok(record) = crate::registry::read_instance(&state) {
            state.id = record.id;
        }
        state
    }

    pub fn session(&self) -> String {
        format!("brg-{}", self.id)
    }

    /// The team's record (`team.json`): see `registry`.
    pub fn instance_file(&self) -> PathBuf {
        self.root.join("team.json")
    }
    /// The message store, shared by every team in this tree.
    pub fn messages_dir(&self) -> PathBuf {
        let mut root = self.root.clone();
        for _ in self.id.split('/').skip(1) {
            root.pop();
        }
        root.join("messages")
    }
    /// Files the members share (`BORGCHART_NOTES`).
    pub fn notes_dir(&self) -> PathBuf {
        self.root.join("notes")
    }
    /// What the last start recorded (brg's own).
    pub fn meta_file(&self) -> PathBuf {
        self.root.join("start.json")
    }
    pub fn exec_file(&self, window: &str) -> PathBuf {
        self.root.join("exec").join(format!("{window}.json"))
    }
    pub fn member_dir(&self, member: &str) -> PathBuf {
        self.root.join("members").join(member)
    }
    /// The current start of a member (replaced atomically).
    pub fn current_incarnation(&self, member: &str) -> PathBuf {
        self.member_dir(member).join("incarnation.json")
    }
    /// Immutable history of a member's starts.
    pub fn incarnation_history(&self, member: &str, incarnation: &str) -> PathBuf {
        self.member_dir(member)
            .join("incarnations")
            .join(format!("{incarnation}.json"))
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
    /// The vendor session the member's latest start reported, for resuming.
    pub fn member_session(&self, member: &str) -> PathBuf {
        self.member_dir(member).join("session")
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
    /// Held by `start` and `stop` while they change the team.
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

/// Facts written at a start and read by `status`, `msg send`, shims and the gate.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Meta {
    #[serde(rename = "template_dir")]
    pub team_dir: PathBuf,
    /// The team's working folder.
    pub folder: PathBuf,
    /// The absolute path of the `brg` that started the team (plugins
    /// get it as `BORGCHART_BIN`).
    pub brg: PathBuf,
    /// The configuration the team started with. Members started into
    /// it later must see the same.
    pub config: ConfigSnapshot,
    /// The tmux socket name the team was started on.
    pub socket: String,
    /// The tmux server's absolute socket path, read from tmux once the
    /// first window started. `stop` and liveness address the
    /// server by this, never by the caller's tmux environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socket_path: Option<PathBuf>,
    /// Set by a `stop` that stopped the whole team (or found it gone
    /// on a server that answered); cleared by the next fresh start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<String>,
    /// Whether a start may have started a window, and so a tmux server, since
    /// the team was last confirmed stopped: set before each start.
    /// `false` lets `stop` say a team whose server can't be reached isn't
    /// running.
    pub windows_started: bool,
    /// The small environment every window and plugin starts from (no secrets).
    pub base_env: BTreeMap<String, String>,
    pub members: BTreeMap<String, MemberMeta>,
    pub plugins: BTreeMap<String, PluginMeta>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemberMeta {
    pub interactive: bool,
    pub role: String,
    pub vendor: String,
    pub posture: String,
    pub parent: Option<String>,
    pub vendor_version: Option<String>,
    /// Whether a start has started the member's window since the team last
    /// started afresh. Incarnation records supply the current start identity.
    pub started: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginMeta {
    /// The plugin's directory, or `None` when its commands come from `PATH`.
    pub dir: Option<PathBuf>,
    pub has_process: bool,
}

/// The team's start record: `None` when nothing was recorded, an error
/// when the file can't be read or parsed.
pub fn read_meta(state: &StateDir) -> Result<Option<Meta>, String> {
    let path = state.meta_file();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))
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
    fn age_cleaned_folders() {
        assert!(age_cleaned(Path::new("/tmp/x/state")));
        assert!(age_cleaned(Path::new("/var/tmp/state")));
        assert!(!age_cleaned(Path::new(
            "/home/someone/.local/state/borgchart"
        )));
    }
}
