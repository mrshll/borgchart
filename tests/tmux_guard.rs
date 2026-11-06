//! The tests' tmux guard: it must refuse every call that could reach a
//! server outside the test's private directory, or the isolation the other
//! tests rely on goes unchecked.

mod common;

use std::process::Output;

use common::PrivateTmux;

fn refused(private: &PrivateTmux, out: &Output) {
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!private.refused().is_empty());
    private.clear_refused();
}

#[test]
fn the_guard_refuses_servers_outside_the_private_directory() {
    let private = PrivateTmux::new();
    let elsewhere = tempfile::tempdir().unwrap();
    let outside = elsewhere.path().join("sock");
    let outside = outside.to_str().unwrap();
    let escape = format!("{}/../sock", private.dir().display());
    let guard = || common::command(private.guard());

    // -L without the private TMUX_TMPDIR: unset, or another directory.
    refused(
        &private,
        &guard().args(["-L", "s", "list-sessions"]).output().unwrap(),
    );
    refused(
        &private,
        &guard()
            .env("TMUX_TMPDIR", elsewhere.path())
            .args(["-L", "s", "list-sessions"])
            .output()
            .unwrap(),
    );
    let inside = || {
        let mut cmd = guard();
        cmd.env("TMUX_TMPDIR", private.dir());
        cmd
    };
    // No -L or -S: tmux would use $TMUX or the default server.
    refused(&private, &inside().arg("list-sessions").output().unwrap());
    // -S outside the directory, or climbing out of it.
    for path in [outside, &escape] {
        refused(
            &private,
            &inside()
                .args(["-S", path, "list-sessions"])
                .output()
                .unwrap(),
        );
    }
    // A later -S after a harmless flag still counts.
    refused(
        &private,
        &inside()
            .args(["-L", "s", "-C", "-S", outside, "attach"])
            .output()
            .unwrap(),
    );
    // A global option it can't read past.
    refused(
        &private,
        &inside()
            .args(["-L", "s", "-u", "list-sessions"])
            .output()
            .unwrap(),
    );

    // Inside: the real tmux runs, and a command's own -S isn't a socket.
    let ok = |args: &[&str]| {
        let out = inside().args(args).output().unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    ok(&[
        "-L",
        "s",
        "-f",
        "/dev/null",
        "new-session",
        "-d",
        "-s",
        "g",
        "sleep 60",
    ]);
    let path = ok(&["-L", "s", "display-message", "-p", "#{socket_path}"]);
    ok(&[
        "-S",
        path.trim(),
        "capture-pane",
        "-p",
        "-S",
        "-20",
        "-t",
        "=g:",
    ]);
    assert!(
        private
            .tmux("s")
            .args(["has-session", "-t", "=g"])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(private.refused(), "");
}

#[test]
fn a_refused_call_fails_the_test_and_nothing_is_left() {
    let private = PrivateTmux::new();
    let dir = private.dir().to_path_buf();
    let started = private
        .tmux("s")
        .args(["new-session", "-d", "-s", "g", "sleep 60"])
        .status()
        .unwrap();
    assert!(started.success());
    let pid = private
        .tmux("s")
        .args(["display-message", "-p", "#{pid}"])
        .output()
        .unwrap();
    let pid = String::from_utf8_lossy(&pid.stdout).trim().to_string();
    assert!(!pid.is_empty());
    let _ = common::command(private.guard())
        .arg("list-sessions")
        .output();
    let dropped = std::panic::catch_unwind(move || drop(private));
    assert!(
        dropped.is_err(),
        "dropping after a refusal must fail the test"
    );
    assert!(!dir.exists(), "{dir:?} is left");
    // The server stopped too (its socket going isn't enough).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let alive = || {
        std::process::Command::new("kill")
            .args(["-0", &pid])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    };
    while alive() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(!alive(), "tmux server {pid} still runs");
}
