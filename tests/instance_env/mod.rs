//! The shared environment for the instance tests: a team in a search path
//! of a test configuration, fake agents, a private state home and tmux.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::common;

pub struct Env {
    /// First, so its servers stop before the test's directory goes.
    pub tmux: common::PrivateTmux,
    pub dir: tempfile::TempDir,
    pub socket: String,
}

/// `lead` starts with the instance; `helper` (its child) and `scout` (a
/// second root) are created on demand.
pub const TEAM: &str = r#"
bus: {}
roles:
  lead:
    prompt: lead.md
  helper:
    prompt: helper.md
    vendor: codex
    parent: lead
    start: false
  scout:
    prompt: scout.md
    start: false
"#;

pub const ALPHABET: &str = "23456789abcdefghjkmnpqrstuvwxyz";

pub fn well_formed(id: &str, base: &str) -> bool {
    id.strip_prefix(base)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|s| s.len() == 4 && s.chars().all(|c| ALPHABET.contains(c)))
}

impl Env {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let tmux = common::PrivateTmux::new();
        let socket = tmux.socket("brg-in");
        let env = Env { tmux, dir, socket };
        let bin = env.path("bin");
        fs::create_dir_all(&bin).unwrap();
        let fake = env!("CARGO_BIN_EXE_fake-agent");
        std::os::unix::fs::symlink(fake, bin.join("claude")).unwrap();
        std::os::unix::fs::symlink(fake, bin.join("codex")).unwrap();
        fs::create_dir_all(env.path("home")).unwrap();
        fs::create_dir_all(env.path("work/a")).unwrap();
        fs::create_dir_all(env.path("work/b")).unwrap();
        env.team("teams/oncall");
        env.config("");
        env
    }

    /// A copy of the test team at `rel`.
    pub fn team(&self, rel: &str) {
        self.write(&format!("{rel}/team.yaml"), TEAM);
        for (role, model) in [
            ("lead", "claude: opus high"),
            ("helper", "codex: example-model"),
            ("scout", "claude: haiku"),
        ] {
            self.write(
                &format!("{rel}/{role}.md"),
                &format!("---\nmodels: {{{model}}}\n---\nYou are the {role}.\n"),
            );
        }
    }

    /// The configuration: the test's search path, plus `extra`.
    pub fn config(&self, extra: &str) {
        self.write(
            "config.yaml",
            &format!(
                "schema: 1\nteams: [{}]\n{extra}",
                self.path("teams").display()
            ),
        );
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    pub fn write(&self, rel: &str, text: &str) {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    pub fn cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_brg"));
        cmd.env_clear()
            .env(
                "PATH",
                self.tmux.path_with(&format!(
                    "{}:/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin",
                    self.path("bin").display()
                )),
            )
            .env("HOME", self.path("home"))
            .env("LANG", "C.UTF-8")
            .env("BORGCHART_STATE_HOME", self.path("state"))
            .env("BORGCHART_CONFIG", self.path("config.yaml"))
            .current_dir(self.dir.path())
            .args(args);
        self.tmux.brg_env(&mut cmd, &self.socket);
        cmd
    }

    pub fn brg(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    pub fn ok(&self, args: &[&str]) -> Output {
        let out = self.brg(args);
        assert!(out.status.success(), "brg {args:?} failed:\n{}", both(&out));
        out
    }

    pub fn fails(&self, args: &[&str]) -> String {
        let out = self.brg(args);
        assert!(
            !out.status.success(),
            "brg {args:?} succeeded:\n{}",
            both(&out)
        );
        both(&out)
    }

    /// `brg create <team> <folder>`; the new instance's ID.
    pub fn create(&self, team: &str, folder: &str) -> String {
        let out = self.ok(&["create", team, &self.path(folder).display().to_string()]);
        let id = stdout(&out)
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();
        assert!(well_formed(&id, "oncall"), "{id}\n{}", both(&out));
        self.wait_for("lead idle", || self.state_of(&id, "lead") == "idle");
        id
    }

    pub fn rows(&self, inst: &str) -> Vec<Value> {
        let out = self.ok(&["status", inst, "--json"]);
        serde_json::from_slice(&out.stdout).unwrap()
    }

    /// The row of `role`'s member (or of the role, with none).
    pub fn row(&self, inst: &str, role: &str) -> Value {
        self.rows(inst)
            .into_iter()
            .find(|r| r["role"] == role)
            .unwrap_or(Value::Null)
    }

    pub fn member(&self, inst: &str, role: &str) -> String {
        self.row(inst, role)["member"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    pub fn state_of(&self, inst: &str, role: &str) -> String {
        self.row(inst, role)["state"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    pub fn starts(&self, member: &str) -> Vec<Value> {
        fs::read_to_string(self.path(&format!("home/fake-agent/{member}.jsonl")))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|e| e["event"] == "start")
            .collect()
    }

    pub fn wait_for(&self, what: &str, f: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if f() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("timed out waiting for {what}");
    }

    pub fn tmux(&self, args: &[&str]) -> String {
        let out = self.tmux.tmux(&self.socket).args(args).output().unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// Publish `record` into `inst` as the outside caller `ops`.
    pub fn publish(&self, inst: &str, record: &Value) -> Output {
        let target = format!("{inst}/{}", record["to"].as_str().unwrap());
        let mut command = self.cmd(&["send", &target, "-", "--external", "ops", "--json"]);
        if let Some(refs) = record["reply_to"].as_array() {
            for reference in refs {
                command.args(["--reply-to", reference.as_str().unwrap()]);
            }
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        // brg may refuse (a damaged store, say) and exit before reading
        // the draft; its answer is in the output either way.
        match child
            .stdin
            .take()
            .unwrap()
            .write_all(record["body"].as_str().unwrap_or("").as_bytes())
        {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
            Err(e) => panic!("cannot write the draft: {e}"),
        }
        child.wait_with_output().unwrap()
    }

    pub fn registry(&self) -> Value {
        serde_json::from_slice(&fs::read(self.path("state/registry.json")).unwrap()).unwrap()
    }
}

pub fn both(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

pub fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

pub fn script(path: &Path, text: &str) {
    common::write_executable(path, text);
}
