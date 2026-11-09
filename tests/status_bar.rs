//! The status line brg's tmux bar shows (`brg status --line`), the bar and
//! `prefix S` pop-up in brg's tmux config, and Enter in `status -i` inside
//! brg's own server: real tmux on a private socket, fake agents.
#![cfg(feature = "test-support")]

mod common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Env {
    /// First, so its servers stop before the test's directory goes.
    tmux: common::PrivateTmux,
    dir: tempfile::TempDir,
    socket: String,
    brg: PathBuf,
}

impl Env {
    /// A team with a bus: alpha, beta under it (Codex), and gamma under it,
    /// not started.
    fn new() -> Self {
        Self::with_brg(PathBuf::from(env!("CARGO_BIN_EXE_brg")))
    }

    fn with_brg(brg: PathBuf) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let tmux = common::PrivateTmux::new();
        let socket = tmux.socket("brg-bar");
        let env = Env {
            tmux,
            dir,
            socket,
            brg,
        };
        let bin = env.path("bin");
        fs::create_dir_all(&bin).unwrap();
        let fake = env!("CARGO_BIN_EXE_fake-agent");
        std::os::unix::fs::symlink(fake, bin.join("claude")).unwrap();
        std::os::unix::fs::symlink(fake, bin.join("codex")).unwrap();
        fs::create_dir_all(env.path("home")).unwrap();
        fs::create_dir_all(env.path("work")).unwrap();
        env.write(
            "team/team.yaml",
            "bus: {}\nroles:\n  alpha: {prompt: alpha.md}\n  beta: {prompt: beta.md, vendor: codex, parent: alpha, accept_risk: true}\n  gamma: {prompt: gamma.md, parent: alpha, start: false}\n",
        );
        env.write("team/alpha.md", "---\n---\nYou are alpha.\n");
        env.write(
            "team/beta.md",
            "---\nmodels: {codex: example-model low}\n---\nYou are beta.\n",
        );
        env.write("team/gamma.md", "---\n---\nYou are gamma.\n");
        env.write("config.yaml", "schema: 1\n");
        env
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    /// The environment brg runs with, as `export` lines for a shell.
    fn vars(&self) -> Vec<(String, String)> {
        vec![
            (
                "PATH".into(),
                self.tmux.path_with(&format!(
                    "{}:/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin",
                    self.path("bin").display()
                )),
            ),
            ("HOME".into(), self.path("home").display().to_string()),
            ("LANG".into(), "C.UTF-8".into()),
            (
                "BORGCHART_STATE_HOME".into(),
                self.path("state").display().to_string(),
            ),
            (
                "BORGCHART_CONFIG".into(),
                self.path("config.yaml").display().to_string(),
            ),
            (
                "BORGCHART_TMUX_TMPDIR".into(),
                self.tmux.dir().display().to_string(),
            ),
            ("BORGCHART_TMUX_SOCKET".into(), self.socket.clone()),
        ]
    }

    fn brg_cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.brg);
        cmd.env_clear()
            .envs(self.vars())
            .current_dir(self.path("team"))
            .args(args);
        cmd
    }

    fn brg(&self, args: &[&str]) -> Output {
        self.brg_cmd(args).output().unwrap()
    }

    fn tmux(&self, args: &[&str]) -> String {
        let out = self.tmux.tmux(&self.socket).args(args).output().unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn create(&self) -> String {
        let team = self.path("team").display().to_string();
        let work = self.path("work").display().to_string();
        let out = self.brg(&["create", &team, &work]);
        assert!(out.status.success(), "{out:?}");
        let id = self.instance();
        self.wait_for("alpha and beta idle", || {
            self.state_of(&id, "alpha") == "idle" && self.state_of(&id, "beta") == "idle"
        });
        id
    }

    fn instance(&self) -> String {
        let reg: serde_json::Value =
            serde_json::from_slice(&fs::read(self.path("state/registry.json")).unwrap()).unwrap();
        reg["instances"]
            .as_object()
            .unwrap()
            .keys()
            .next()
            .unwrap()
            .clone()
    }

    fn rows(&self, id: &str) -> Vec<serde_json::Value> {
        let out = self.brg(&["status", id, "--json"]);
        assert!(out.status.success(), "{out:?}");
        serde_json::from_slice::<serde_json::Value>(&out.stdout)
            .unwrap()
            .as_array()
            .unwrap()
            .clone()
    }

    fn row(&self, id: &str, role: &str) -> serde_json::Value {
        self.rows(id)
            .into_iter()
            .find(|r| r["role"] == role)
            .unwrap_or_default()
    }

    fn state_of(&self, id: &str, role: &str) -> String {
        self.row(id, role)["state"]
            .as_str()
            .unwrap_or("")
            .to_string()
    }

    fn member(&self, id: &str, role: &str) -> String {
        self.row(id, role)["name"].as_str().unwrap().to_string()
    }

    fn line(&self, id: &str) -> String {
        let out = self.brg(&["status", id, "--line"]);
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).unwrap()
    }

    fn wait_for(&self, what: &str, f: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if f() {
                return;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        let mut dump = String::new();
        for w in self
            .tmux(&["list-windows", "-a", "-F", "#{session_name}:#{window_name}"])
            .lines()
        {
            let (session, window) = w.split_once(':').unwrap();
            dump.push_str(&format!("--- {w}\n"));
            dump.push_str(&self.tmux(&[
                "capture-pane",
                "-p",
                "-t",
                &format!("={session}:={window}"),
            ]));
        }
        panic!("timed out waiting for {what}\n{dump}");
    }

    /// Publish a message to `to` from outside the team.
    fn publish(&self, id: &str, to: &str) {
        let draft = self.path("message.json");
        fs::write(
            &draft,
            serde_json::json!({"to": to, "body": "pending"}).to_string(),
        )
        .unwrap();
        let draft = draft.display().to_string();
        let out = self.brg(&[
            "msg",
            "publish",
            "--instance",
            id,
            "--external",
            "test",
            "--record",
            &draft,
        ]);
        assert!(out.status.success(), "{out:?}");
    }
}

/// tmux's styles around one member's mark, as the bar shows it.
fn mark(style: &str, mark: &str, member: &str) -> String {
    format!("#[fg={style}]{mark}#[default] {member}")
}

#[test]
fn status_line_marks_each_running_member_and_hides_stopped_ones() {
    let env = Env::new();
    let id = env.create();
    let alpha = env.member(&id, "alpha");
    let beta = env.member(&id, "beta");
    // gamma has not started: it isn't running, so it isn't shown.
    assert_eq!(
        env.line(&id),
        format!(
            "{}  {}\n",
            mark("colour245", "○", &alpha),
            mark("colour245", "○", &beta)
        )
    );

    env.publish(&id, &alpha);
    env.publish(&id, &alpha);
    assert!(
        env.brg(&["create", &format!("{id}/gamma")])
            .status
            .success()
    );
    let gamma = env.member(&id, "gamma");
    env.wait_for("gamma idle", || env.state_of(&id, "gamma") == "idle");
    let send = |member: &str, text: &str| {
        let out = env.brg(&["send", &format!("{id}/{member}"), text]);
        assert!(out.status.success(), "{out:?}");
    };
    send(&gamma, "work 60");
    send(&beta, "dialog");
    env.wait_for("gamma working, beta blocked", || {
        env.state_of(&id, "gamma") == "working" && env.state_of(&id, "beta") == "blocked"
    });
    assert_eq!(
        env.line(&id),
        format!(
            "{} #[fg=cyan]2✉#[default]  {}  {}\n",
            mark("colour245", "○", &alpha),
            mark("red", "◆", &beta),
            mark("green", "●", &gamma)
        )
    );

    // A stopped member is left out; a state with no mark of its own gets `?`.
    assert!(
        env.brg(&["stop", &format!("{id}/{gamma}")])
            .status
            .success()
    );
    send(&alpha, "mute");
    env.wait_for("alpha unknown", || env.state_of(&id, "alpha") == "unknown");
    assert_eq!(
        env.line(&id),
        format!(
            "{} #[fg=cyan]2✉#[default]  {}\n",
            mark("yellow", "?", &alpha),
            mark("red", "◆", &beta)
        )
    );

    assert!(env.brg(&["stop", &id]).status.success());
    assert_eq!(env.line(&id), "no members running\n");

    // Refusals print no line, which would say that nothing runs.
    let out = env.brg(&["status", "team-nope", "--line"]);
    assert!(!out.status.success(), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    fs::rename(env.path("team"), env.path("moved")).unwrap();
    let out = env
        .brg_cmd(&["status", &id, "--line"])
        .current_dir(env.path("work"))
        .output()
        .unwrap();
    assert!(!out.status.success(), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
}

/// A copy of brg in a folder whose name a shell or tmux would misread
/// unquoted.
fn awkward_brg(root: &Path) -> PathBuf {
    let dir = root.join("it's a \"#{x}\" $HOME (y);");
    fs::create_dir_all(&dir).unwrap();
    let brg = dir.join("brg");
    fs::copy(env!("CARGO_BIN_EXE_brg"), &brg).unwrap();
    brg
}

#[test]
fn brg_tmux_config_puts_the_status_line_on_top_and_binds_the_pop_up() {
    let paths = tempfile::tempdir().unwrap();
    let env = Env::with_brg(awkward_brg(paths.path()));
    let id = env.create();
    let session = format!("brg-{id}");

    assert_eq!(env.tmux(&["show", "-gv", "status-position"]), "top\n");

    // The bar's command, expanded for the session and run by the server
    // itself, with its own environment, as tmux runs the bar's. (`display
    // -p` would show the expansion with `$` escaped.)
    let right = env.tmux(&["show", "-gv", "status-right"]);
    let command = right
        .trim_end()
        .strip_prefix("#(")
        .and_then(|c| c.strip_suffix(')'))
        .unwrap_or_else(|| panic!("status-right runs no command: {right}"));
    let printed = paths.path().join("printed");
    let out = env
        .tmux
        .tmux(&env.socket)
        .args(["run-shell", "-t", &format!("={session}:")])
        .arg(format!("{command} >'{}' 2>&1", printed.display()))
        .output()
        .unwrap();
    assert!(out.status.success(), "{command}: {out:?}");
    assert_eq!(fs::read_to_string(&printed).unwrap(), env.line(&id));

    // `prefix S` in a real client (attached from a pane of an outer server,
    // which gives it a terminal) opens `status -i` in a pop-up, top right,
    // 2 cells from the edges; Enter moves the client to alpha and closes it.
    let alpha = env.member(&id, "alpha");
    let socket = env.tmux(&["display", "-p", "#{socket_path}"]);
    let outer = env.tmux.socket("brg-bar-out");
    let status = env
        .tmux
        .tmux(&outer)
        .args(["new-session", "-d", "-s", "o", "-x", "120", "-y", "30"])
        .arg(format!(
            "env -u TMUX {} -S '{}' attach -t '={session}:'",
            env.tmux.guard().display(),
            socket.trim_end()
        ))
        .status()
        .unwrap();
    assert!(status.success());
    let screen = || {
        let out = env
            .tmux
            .tmux(&outer)
            .args(["capture-pane", "-p", "-t", "=o:"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    let client_window = || env.tmux(&["list-clients", "-F", "#{window_name}"]);
    env.wait_for("a client on brg's server", || !client_window().is_empty());
    let first = client_window();
    let keys = |keys: &[&str]| {
        let status = env
            .tmux
            .tmux(&outer)
            .args(["send-keys", "-t", "=o:"])
            .args(keys)
            .status()
            .unwrap();
        assert!(status.success());
    };
    keys(&["C-b", "S"]);
    env.wait_for("the pop-up with alpha's row", || {
        let s = screen();
        let lines: Vec<&str> = s.lines().collect();
        lines.len() > 3
            && lines[2]
                .chars()
                .skip(54)
                .collect::<String>()
                .starts_with("┌─ brg status")
            && lines[2].chars().count() == 118
            && s.contains(&alpha)
    });
    // The client's current window is the one it attached to until Enter.
    if first == format!("{alpha}\n") {
        keys(&["j"]);
        std::thread::sleep(Duration::from_millis(300));
    }
    let selected = if first == format!("{alpha}\n") {
        env.member(&id, "beta")
    } else {
        alpha
    };
    keys(&["Enter"]);
    env.wait_for(
        "the client on the selected member, the pop-up closed",
        || client_window() == format!("{selected}\n") && !screen().contains("brg status"),
    );

    // On a client narrower and shorter than the pop-up, it fills the width
    // and keeps 2 rows above it; q closes it.
    let status = env
        .tmux
        .tmux(&outer)
        .args(["resize-window", "-t", "=o:", "-x", "50", "-y", "14"])
        .status()
        .unwrap();
    assert!(status.success());
    env.wait_for("the narrow client", || {
        env.tmux(&["list-clients", "-F", "#{client_width}"]) == "50\n"
    });
    keys(&["C-b", "S"]);
    env.wait_for("the narrow pop-up", || {
        let s = screen();
        let lines: Vec<&str> = s.lines().collect();
        lines.len() == 14 && lines[2].starts_with("┌─ brg status") && lines[13].starts_with("└")
    });
    keys(&["q"]);
    env.wait_for("the narrow pop-up closed", || {
        !screen().contains("brg status")
    });
}

#[test]
fn enter_in_status_i_inside_brgs_server_switches_the_client_to_the_member() {
    let env = Env::new();
    let id = env.create();
    let session = format!("brg-{id}");
    let alpha = env.member(&id, "alpha");

    // `status -i` in a window of brg's own server, as the pop-up runs it.
    let script = env.path("screen.sh");
    let mut text = String::from("#!/bin/sh\n");
    for (k, v) in env.vars() {
        text.push_str(&format!("export {k}='{v}'\n"));
    }
    text.push_str(&format!(
        "export TERM=xterm-256color\nexec '{}' status {id} -i\n",
        env.brg.display()
    ));
    common::write_executable(&script, &text);
    env.tmux(&[
        "new-window",
        "-d",
        "-t",
        &format!("={session}:"),
        "-n",
        "screen",
        &script.display().to_string(),
    ]);

    // A client on that window: control mode, held open on a pipe.
    let mut client = env
        .tmux
        .tmux(&env.socket)
        .args(["-C", "attach", "-t", &format!("={session}:=screen")])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let windows = || env.tmux(&["list-clients", "-F", "#{window_name}"]);
    env.wait_for("the client on the screen", || windows() == "screen\n");
    let screen = || env.tmux(&["capture-pane", "-p", "-t", &format!("={session}:=screen")]);
    env.wait_for("alpha on the screen", || screen().contains("> "));

    env.tmux(&["send-keys", "-t", &format!("={session}:=screen"), "Enter"]);
    env.wait_for("the client on alpha's window", || {
        windows() == format!("{alpha}\n")
    });
    // And the screen ended, as a pop-up would close.
    env.wait_for("the screen to exit", || {
        env.tmux(&[
            "display",
            "-p",
            "-t",
            &format!("={session}:=screen"),
            "#{pane_dead}",
        ]) == "1\n"
    });

    let _ = client.stdin.take().unwrap().write_all(b"detach\n");
    let _ = client.kill();
    let _ = client.wait();
}
