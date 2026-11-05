//! `brg status -i` in a real terminal: an outer tmux server provides the pty.
//! Enter attaches to a member, the detach key returns to the screen, q quits.
#![cfg(feature = "test-support")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

struct Servers {
    inner: String,
    outer: String,
}

impl Drop for Servers {
    fn drop(&mut self) {
        for s in [&self.inner, &self.outer] {
            let _ = Command::new("tmux").args(["-L", s, "kill-server"]).output();
        }
    }
}

fn tmux(socket: &str, args: &[&str]) -> String {
    let out = Command::new("tmux")
        .arg("-L")
        .arg(socket)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn wait(what: &str, f: impl Fn() -> bool, dump: impl Fn() -> String) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    panic!("timed out waiting for {what}\n{}", dump());
}

#[test]
fn status_i_attaches_detaches_and_quits() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let write = |rel: &str, text: &str| {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    };
    write("team/team.yaml", "members:\n  alpha: {prompt: alpha.md}\n");
    write("team/alpha.md", "---\nrole: helper\n---\nYou are alpha.\n");
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::create_dir_all(root.join("home")).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_fake-agent"), root.join("bin/claude")).unwrap();

    let servers = Servers {
        inner: format!("brg-tui-in-{}", std::process::id()),
        outer: format!("brg-tui-out-{}", std::process::id()),
    };
    let env_line = format!(
        "export PATH={bin}:/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin HOME={home} LANG=C.UTF-8 TERM=xterm-256color BORGCHART_STATE_HOME={state} BORGCHART_TMUX_SOCKET={sock}\ncd {team}\n",
        bin = root.join("bin").display(),
        home = root.join("home").display(),
        state = root.join("state").display(),
        sock = servers.inner,
        team = root.join("team").display(),
    );
    let brg = env!("CARGO_BIN_EXE_brg");
    let script = root.join("run.sh");
    fs::write(
        &script,
        format!("#!/bin/sh\n{env_line}{brg} up >/dev/null || exit 9\nexec {brg} status -i\n"),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

    let outer = servers.outer.clone();
    Command::new("tmux")
        .args([
            "-L",
            &outer,
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            "o",
            "-x",
            "140",
            "-y",
            "30",
        ])
        .arg(script.display().to_string())
        .env_remove("TMUX")
        .status()
        .unwrap();
    let screen = || tmux(&outer, &["capture-pane", "-p", "-t", "=o:"]);
    let clients = || tmux(&servers.inner, &["list-clients"]).lines().count();
    let dump = || format!("outer screen:\n{}", screen());

    wait(
        "the status screen",
        || screen().contains("brg status") && screen().contains("alpha"),
        dump,
    );
    wait("alpha idle", || screen().contains("idle"), dump);

    tmux(&outer, &["send-keys", "-t", "=o:", "Enter"]);
    wait(
        "attached to alpha",
        || clients() == 1 && screen().contains("don't ask on"),
        dump,
    );

    // The detach key reaches brg's tmux client, which returns to the screen.
    tmux(&outer, &["send-keys", "-t", "=o:", "C-b", "d"]);
    wait(
        "back in the status screen",
        || clients() == 0 && screen().contains("back from alpha"),
        dump,
    );

    tmux(&outer, &["send-keys", "-t", "=o:", "q"]);
    wait(
        "the screen to close",
        || {
            !Command::new("tmux")
                .args(["-L", &outer, "has-session", "-t", "=o"])
                .status()
                .unwrap()
                .success()
        },
        dump,
    );
    let _ = Path::new(brg);
}
