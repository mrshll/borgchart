//! Shared by the integration tests: keep what a test starts away from the
//! caller's team and tmux server.
//!
//! The tests also run inside team members (the git hooks, or a member that
//! runs `cargo test`). There `BORGCHART_*` names the member's real
//! deployment and message storage, and `TMUX` its tmux server. A process
//! that a test starts with those inherited can write into that team.
#![allow(dead_code)]

use std::ffi::OsStr;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::process::Command;

/// Remove every inherited variable that could point at the caller's team or
/// tmux server. Variables the test sets afterwards still apply.
pub fn scrub(cmd: &mut Command) -> &mut Command {
    for (key, _) in std::env::vars_os() {
        if points_at_a_team(&key) {
            cmd.env_remove(&key);
        }
    }
    cmd
}

fn points_at_a_team(key: &OsStr) -> bool {
    let key = key.to_string_lossy();
    key.starts_with("BORGCHART_") || matches!(&*key, "TMUX" | "TMUX_PANE" | "TMUX_TMPDIR")
}

/// `program`, scrubbed.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);
    scrub(&mut cmd);
    cmd
}

/// `tmux -L <socket>`, scrubbed.
pub fn tmux(socket: &str) -> Command {
    let mut cmd = command("tmux");
    cmd.arg("-L").arg(socket);
    cmd
}

/// Stop a test's tmux server and remove its socket file, which tmux leaves
/// behind after `kill-server`.
pub fn kill_server(socket: &str) {
    let _ = tmux(socket).arg("kill-server").output();
    let _ = std::fs::remove_file(socket_dir().join(socket));
}

/// Where tmux keeps `-L` sockets when `TMUX_TMPDIR` is unset, as it is for
/// brg's own tmux commands: `/tmp/tmux-<uid>`.
fn socket_dir() -> PathBuf {
    let mine = tempfile::tempdir().unwrap();
    let uid = std::fs::metadata(mine.path()).unwrap().uid();
    PathBuf::from(format!("/tmp/tmux-{uid}"))
}
