//! The status line brg's tmux bar shows (`brg status --line`): real tmux on
//! a private socket, fake agents.
#![cfg(feature = "test-support")]

mod common;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
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
