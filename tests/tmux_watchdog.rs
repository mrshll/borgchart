//! The tmux watchdog (`PrivateTmux::with_watchdog`): whether the test
//! process is killed, its time limit passes or it drops the directory as
//! usual, the servers in the private directory stop and the directory and
//! every path given to `also_remove` go; nothing outside them is touched.
//! A leaked server would keep its agents running (for real agents,
//! spending quota).
//!
//! These test the test harness, not brg, and take several seconds of
//! waiting, so they are outside the default tier: `.githooks/checks harness`
//! runs them (CI does, once, on Linux).
#![cfg(feature = "test-support")]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::PrivateTmux;

/// Set (to the file to report in) only when a test starts the helper.
const HELPER_VAR: &str = "BRG_WATCHDOG_HELPER";

fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// A short directory for the test's own files (socket paths stay short).
fn root() -> tempfile::TempDir {
    let parent = std::env::var_os(common::ROOT_VAR)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    tempfile::Builder::new()
        .prefix("brgw")
        .tempdir_in(parent)
        .unwrap()
}

/// What a watched test left running and registered.
struct Started {
    dir: PathBuf,
    server: u32,
    agent: u32,
    scratch: PathBuf,
}

/// Start a server running the fake agent in `private`, and register a
/// scratch directory under `root` holding a file and a link to `keep`
/// (outside it), the way the live tier keeps its vendor configuration.
fn start(private: &PrivateTmux, root: &Path) -> Started {
    let scratch = root.join("scratch");
    fs::create_dir_all(scratch.join("home")).unwrap();
    fs::write(scratch.join("credentials"), "not-a-real-secret").unwrap();
    std::os::unix::fs::symlink(root.join("keep"), scratch.join("link-out")).unwrap();
    let claude = scratch.join("claude");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_fake-agent"), &claude).unwrap();
    private.also_remove(&scratch);
    let socket = private.socket("w");
    let ok = private
        .tmux(&socket)
        .env("HOME", scratch.join("home"))
        .args(["new-session", "-d", "-s", "w"])
        .arg(&claude)
        .status()
        .unwrap();
    assert!(ok.success());
    let out = private
        .tmux(&socket)
        .args(["display-message", "-p", "-t", "=w:", "#{pid} #{pane_pid}"])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&out.stdout).to_string();
    let (server, agent) = out.trim().split_once(' ').unwrap();
    let started = Started {
        dir: private.dir().to_path_buf(),
        server: server.parse().unwrap(),
        agent: agent.parse().unwrap(),
        scratch,
    };
    assert!(alive(started.server) && alive(started.agent));
    started
}

/// Wait until the servers, the agent, the directory and the scratch
/// directory are all gone, and check what's outside them is left.
fn cleaned_up(s: &Started, root: &Path, within: Duration) {
    let deadline = Instant::now() + within;
    let left = || {
        let mut left = Vec::new();
        if alive(s.server) {
            left.push(format!("tmux server {}", s.server));
        }
        if alive(s.agent) {
            left.push(format!("agent {}", s.agent));
        }
        for p in [&s.dir, &s.scratch] {
            if fs::symlink_metadata(p).is_ok() {
                left.push(format!("{p:?}"));
            }
        }
        left
    };
    while !left().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(left().is_empty(), "still there: {:?}", left());
    assert_eq!(fs::read_to_string(root.join("keep")).unwrap(), "kept");
    assert!(root.join("beside").exists());
}

fn outside(root: &Path) {
    fs::write(root.join("keep"), "kept").unwrap();
    fs::write(root.join("beside"), "").unwrap();
}

#[test]
#[ignore = "harness self-test; run with `.githooks/checks harness`"]
fn a_killed_test_process_leaves_no_server_and_no_registered_path() {
    // Catches: cleanup that only runs in Drop, so a test killed mid-run
    // leaves its servers (and their agents) and its scratch files behind.
    let root = root();
    outside(root.path());
    let ready = root.path().join("ready");
    let mut helper = common::command(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "watched_helper",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(HELPER_VAR, &ready)
        .env(common::ROOT_VAR, root.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let text = fs::read_to_string(&ready).expect("the helper never started its server");
    let f: Vec<&str> = text.lines().collect();
    let started = Started {
        dir: PathBuf::from(f[0]),
        server: f[1].parse().unwrap(),
        agent: f[2].parse().unwrap(),
        scratch: PathBuf::from(f[3]),
    };
    helper.kill().unwrap();
    helper.wait().unwrap();
    cleaned_up(&started, root.path(), Duration::from_secs(10));
}

/// Started by the test above, which kills it: a watched test that never
/// gets to drop its directory.
#[test]
#[ignore = "started by a_killed_test_process_leaves_no_server_and_no_registered_path"]
fn watched_helper() {
    let Some(ready) = std::env::var_os(HELPER_VAR) else {
        return;
    };
    let root = PathBuf::from(std::env::var_os(common::ROOT_VAR).unwrap());
    let private = PrivateTmux::with_watchdog(Duration::from_secs(120));
    let s = start(&private, &root);
    let tmp = PathBuf::from(&ready).with_extension("tmp");
    fs::write(
        &tmp,
        format!(
            "{}\n{}\n{}\n{}\n",
            s.dir.display(),
            s.server,
            s.agent,
            s.scratch.display()
        ),
    )
    .unwrap();
    fs::rename(&tmp, &ready).unwrap();
    std::thread::sleep(Duration::from_secs(120));
}

#[test]
#[ignore = "harness self-test; run with `.githooks/checks harness`"]
fn the_time_limit_stops_the_servers_while_the_test_still_runs() {
    // Catches: a hung test that keeps its servers running for ever.
    let root = root();
    outside(root.path());
    let private = PrivateTmux::with_watchdog(Duration::from_secs(2));
    let s = start(&private, root.path());
    cleaned_up(&s, root.path(), Duration::from_secs(10));
    // Dropping it afterwards finds nothing to do, and doesn't fail.
    drop(private);
}

#[test]
#[ignore = "harness self-test; run with `.githooks/checks harness`"]
fn a_socket_linked_from_elsewhere_is_not_followed() {
    // Catches: the cleanup stopping a server outside the private
    // directory through a symbolic link planted in it.
    let other = PrivateTmux::new();
    let socket = other.socket("o");
    let ok = other
        .tmux(&socket)
        .args(["new-session", "-d", "-s", "o", "sleep 300"])
        .status()
        .unwrap();
    assert!(ok.success());
    let target = other
        .dir()
        .join(format!("tmux-{}", other.uid()))
        .join(&socket);
    let private = PrivateTmux::with_watchdog(Duration::from_secs(1));
    let linked = private.dir().join(format!("tmux-{}", private.uid()));
    fs::create_dir_all(&linked).unwrap();
    std::os::unix::fs::symlink(&target, linked.join("linked")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while private.dir().exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!private.dir().exists(), "the watchdog didn't clean up");
    let still = other
        .tmux(&socket)
        .args(["has-session", "-t", "=o"])
        .status()
        .unwrap();
    assert!(still.success(), "the server outside was stopped");
}

/// Resume and kill `pid` if a test fails while it is stopped.
struct Resume(u32);

impl Drop for Resume {
    fn drop(&mut self) {
        if std::thread::panicking() && alive(self.0) {
            for signal in ["-CONT", "-KILL"] {
                let _ = Command::new("kill")
                    .args([signal, &self.0.to_string()])
                    .status();
            }
        }
    }
}

#[test]
#[ignore = "harness self-test; run with `.githooks/checks harness`"]
fn a_server_that_stops_answering_is_still_stopped() {
    // Catches: one stopped or hung server blocking the cleanup for ever,
    // keeping its agents and every registered path.
    let root = root();
    outside(root.path());
    let private = PrivateTmux::with_watchdog(Duration::from_secs(3));
    let s = start(&private, root.path());
    let _resume = Resume(s.server);
    // Once the watchdog has noted the server, stop it.
    let noted = private.dir().join(common::SERVERS);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !fs::read_to_string(&noted).is_ok_and(|t| t.starts_with(&format!("{} ", s.server))) {
        assert!(
            Instant::now() < deadline,
            "the watchdog never noted the server"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let stopped = Command::new("kill")
        .args(["-STOP", &s.server.to_string()])
        .status()
        .unwrap();
    assert!(stopped.success());
    cleaned_up(&s, root.path(), Duration::from_secs(20));
}

#[test]
#[ignore = "harness self-test; run with `.githooks/checks harness`"]
fn dropping_it_cleans_up_and_stops_the_watchdog() {
    // Catches: registered paths removed only by the watchdog, a removal
    // that follows a link out of the scratch directory, or a watchdog left
    // running after the test.
    let root = root();
    outside(root.path());
    let private = PrivateTmux::with_watchdog(Duration::from_secs(120));
    let s = start(&private, root.path());
    let watchdog = private.watchdog_pid().unwrap();
    assert!(alive(watchdog));
    drop(private);
    assert!(fs::symlink_metadata(&s.scratch).is_err(), "{:?}", s.scratch);
    cleaned_up(&s, root.path(), Duration::from_secs(5));
    assert!(!alive(watchdog), "the watchdog {watchdog} still runs");
}
