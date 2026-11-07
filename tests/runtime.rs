//! Runtime tests: real tmux on a private per-test socket, with the
//! `fake-agent` binary standing in for `claude` and `codex`.
//!
//! Run with `cargo test --features test-support`.
#![cfg(feature = "test-support")]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const SECRET: &str = "xoxb-not-a-real-token-41f3";

struct Env {
    /// First, so its servers stop before the test's directory goes.
    tmux: common::PrivateTmux,
    dir: tempfile::TempDir,
    socket: String,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let tmux = common::PrivateTmux::new();
        let socket = tmux.socket("brg-test");
        let env = Env { tmux, dir, socket };
        let bin = env.path("bin");
        fs::create_dir_all(&bin).unwrap();
        let fake = env!("CARGO_BIN_EXE_fake-agent");
        std::os::unix::fs::symlink(fake, bin.join("claude")).unwrap();
        std::os::unix::fs::symlink(fake, bin.join("codex")).unwrap();
        fs::create_dir_all(env.path("home")).unwrap();
        env.write(
            "team/team.yaml",
            r#"
plugins:
  pager:
    path: plugins/pager
    process: [./watch.sh]
    delivers_to: alpha
    env: [PAGER_TOKEN, {PAGER_ROOM: R0000EXAMPLE}]
    actions:
      read: pager read <id>
      ping: pager ping <id>
members:
  alpha:
    prompt: alpha.md
    posture: locked
    untrusted_input: true
    plugins: {pager: [read]}
  beta:
    prompt: beta.md
    vendor: codex
    parent: alpha
    accept_risk: true
  gamma:
    prompt: gamma.md
    start: false
"#,
        );
        env.write(
            "team/alpha.md",
            "---\nrole: triager\nmodels: {claude: haiku}\n---\nYou are alpha.\n",
        );
        env.write(
            "team/beta.md",
            "---\nrole: helper\nmodels: {codex: example-model low}\n---\nYou are beta.\n",
        );
        env.write("team/gamma.md", "---\nrole: spare\n---\nYou are gamma.\n");
        env.script(
            "team/plugins/pager/pager",
            "#!/bin/sh\necho \"pager $* member=$BORGCHART_MEMBER parent=${BORGCHART_PARENT:-} token_len=${#PAGER_TOKEN}\" >> \"$BORGCHART_STATE_DIR/calls.log\"\necho ok\n",
        );
        env.script(
            "team/plugins/pager/watch.sh",
            "#!/bin/sh\nenv | grep -E '^(TERM|COLORTERM)=' > \"$BORGCHART_STATE_DIR/window-env.tmp\"\nmv \"$BORGCHART_STATE_DIR/window-env.tmp\" \"$BORGCHART_STATE_DIR/window-env\"\nwhile true; do printf '{\"ok\": true, \"at\": \"now\", \"detail\": \"fine\"}' > \"$BORGCHART_STATE_DIR/health.json\"; sleep 1; done\n",
        );
        fs::create_dir_all(env.path("work")).unwrap();
        env.config("");
        env
    }

    /// brg's configuration: `extra` after the schema.
    fn config(&self, extra: &str) {
        self.write("config.yaml", &format!("schema: 1\n{extra}"));
    }

    /// Add `lines` to `role`'s entry in the team file.
    fn role_extra(&self, role: &str, lines: &str) {
        let path = self.path("team/team.yaml");
        let text = fs::read_to_string(&path).unwrap();
        let entry = format!("  {role}:\n    prompt: {role}.md\n");
        assert!(text.contains(&entry), "{text}");
        fs::write(&path, text.replace(&entry, &format!("{entry}{lines}"))).unwrap();
    }

    /// The instance, once created.
    fn instance(&self) -> Option<String> {
        let reg: serde_json::Value =
            serde_json::from_slice(&fs::read(self.path("state/registry.json")).ok()?).ok()?;
        reg["instances"].as_object()?.keys().next().cloned()
    }

    /// The member of `role`, once created.
    fn member_id(&self, role: &str) -> Option<String> {
        let inst = self.instance()?;
        let rec: serde_json::Value = serde_json::from_slice(
            &fs::read(self.path(&format!("state/{inst}/instance.json"))).ok()?,
        )
        .ok()?;
        rec["members"]
            .as_object()?
            .iter()
            .find(|(_, m)| m["role"] == role)
            .map(|(id, _)| id.clone())
    }

    /// The member of `role` (it must exist).
    fn m(&self, role: &str) -> String {
        self.member_id(role)
            .unwrap_or_else(|| panic!("no member of `{role}` yet"))
    }

    /// The tests act on the team's one instance, working in `work`, and
    /// name members by role: `start` creates the instance the first time,
    /// `start <role>` creates or starts its member, and `stop`, `status`,
    /// `logs` and `send` (with `--from`) go to the instance.
    fn translate(&self, args: &[&str]) -> Vec<String> {
        let inst = self.instance();
        let at = |name: &str| match (&inst, self.member_id(name)) {
            (Some(i), Some(m)) => format!("{i}/{m}"),
            (Some(i), None) => format!("{i}/{name}"),
            (None, _) => name.to_string(),
        };
        let own = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        match args {
            ["start"] => match &inst {
                None => vec![
                    "create".into(),
                    self.path("team").display().to_string(),
                    self.path("work").display().to_string(),
                ],
                Some(i) => vec!["start".into(), i.clone()],
            },
            ["start", role] => match (&inst, self.member_id(role)) {
                (Some(i), None) => vec!["create".into(), format!("{i}/{role}")],
                _ => vec!["start".into(), at(role)],
            },
            ["stop"] => vec!["stop".into(), inst.clone().unwrap_or_default()],
            ["stop", role] => vec!["stop".into(), at(role)],
            ["status", rest @ ..] => {
                let mut v = vec!["status".to_string()];
                v.extend(inst.clone());
                v.extend(own(rest));
                v
            }
            [cmd @ ("logs" | "attach" | "send"), rest @ ..] => {
                let mut v = vec![cmd.to_string()];
                let mut member_done = false;
                let mut from = false;
                for a in rest {
                    if from {
                        v.push(self.member_id(a).unwrap_or_else(|| a.to_string()));
                        from = false;
                    } else if *a == "--from" {
                        v.push(a.to_string());
                        from = true;
                    } else if a.starts_with('-') || member_done {
                        v.push(a.to_string());
                    } else {
                        v.push(at(a));
                        member_done = true;
                    }
                }
                v
            }
            _ => own(args),
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn script(&self, rel: &str, text: &str) {
        self.write(rel, text);
        fs::set_permissions(self.path(rel), fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn brg_cmd(&self, args: &[&str]) -> Command {
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
            .env("BORGCHART_GATE_WAIT_MS", "8000")
            .env("PAGER_TOKEN", SECRET)
            .current_dir(self.path("team"))
            .args(self.translate(args));
        self.tmux.brg_env(&mut cmd, &self.socket);
        cmd
    }

    fn brg(&self, args: &[&str]) -> Output {
        self.brg_cmd(args).output().unwrap()
    }

    fn tmux(&self, args: &[&str]) -> String {
        let out = self.tmux.tmux(&self.socket).args(args).output().unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn events(&self, role: &str) -> Vec<serde_json::Value> {
        let member = self.member_id(role).unwrap_or_else(|| role.to_string());
        fs::read_to_string(self.path(&format!("home/fake-agent/{member}.jsonl")))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn wait_for<F: Fn(&Env) -> bool>(&self, what: &str, f: F) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if f(self) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let mut dump = String::new();
        for w in self
            .tmux(&["list-windows", "-a", "-F", "#{session_name}:#{window_name}"])
            .lines()
        {
            let (session, window) = w.split_once(':').unwrap();
            dump.push_str(&format!(
                "--- {w} title={}\n",
                self.tmux(&[
                    "display",
                    "-p",
                    "-t",
                    &format!("={session}:={window}"),
                    "#{pane_title}"
                ])
                .trim()
            ));
            dump.push_str(&self.tmux(&[
                "capture-pane",
                "-p",
                "-t",
                &format!("={session}:={window}"),
            ]));
        }
        for m in ["alpha", "beta", "gamma"] {
            dump.push_str(&format!(
                "--- events {m}: {:?}\n",
                self.events(m)
                    .iter()
                    .filter(|e| e["event"] != "start")
                    .collect::<Vec<_>>()
            ));
        }
        panic!("timed out waiting for {what}\n{dump}");
    }

    fn status(&self) -> Vec<serde_json::Value> {
        let out = self.brg(&["status", "--json"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&out.stdout)
            .unwrap()
            .as_array()
            .unwrap()
            .clone()
    }

    fn state_of(&self, name: &str) -> String {
        self.status()
            .iter()
            .find(|r| r["role"] == name || r["name"] == name)
            .map(|r| r["state"].as_str().unwrap_or("").to_string())
            .unwrap_or_default()
    }

    fn up(&self) {
        let out = self.brg(&["start"]);
        assert!(
            out.status.success(),
            "up failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        self.wait_for("alpha idle", |e| e.state_of("alpha") == "idle");
        self.wait_for("beta idle", |e| e.state_of("beta") == "idle");
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn submissions(env: &Env, member: &str) -> Vec<String> {
    env.events(member)
        .iter()
        .filter(|e| e["event"] == "submit")
        .map(|e| e["text"].as_str().unwrap_or("").to_string())
        .collect()
}

#[test]
fn up_status_send_logs_down() {
    let env = Env::new();
    env.up();

    let rows = env.status();
    let alpha = rows.iter().find(|r| r["role"] == "alpha").unwrap();
    assert_eq!(alpha["posture"], "locked");
    assert!(
        alpha.get("detail").is_none(),
        "gate should be active: {alpha}"
    );
    assert_eq!(
        rows.iter().find(|r| r["role"] == "gamma").unwrap()["state"],
        "on demand (none created)"
    );
    env.wait_for("plugin health", |e| {
        e.status()
            .iter()
            .any(|r| r["name"] == "pager" && r["state"] == "alive" && r["detail"] == "ok")
    });

    let out = env.brg(&["send", "alpha", "hello there"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    env.wait_for("submission", |e| {
        submissions(e, "alpha").contains(&"hello there".to_string())
    });

    let logs = env.brg(&["logs", "alpha"]);
    assert!(stdout(&logs).contains("> hello there"), "{}", stdout(&logs));

    let table = env.brg(&["status"]);
    // The instance's tree under its header line, then plugin processes.
    let text = stdout(&table);
    let inst = env.instance().unwrap();
    assert!(text.starts_with(&format!("{inst}  team ")), "{text}");
    assert!(
        text.lines()
            .nth(1)
            .unwrap()
            .starts_with(&format!("├─ {}", env.m("alpha"))),
        "{text}"
    );
    assert!(text.contains("\nplugin "), "{text}");

    let out = env.brg(&["start", "gamma"]);
    assert!(out.status.success());
    env.wait_for("gamma idle", |e| e.state_of("gamma") == "idle");

    let out = env.brg(&["stop"]);
    assert!(out.status.success());
    assert_eq!(env.tmux(&["list-sessions"]), "");
    let state = env.path("state");
    let leftover: Vec<_> = walk(&state)
        .into_iter()
        .filter(|p| p.to_string_lossy().contains("/secrets/"))
        .collect();
    assert!(
        leftover.is_empty(),
        "plugin env files left after down: {leftover:?}"
    );
}

#[test]
fn up_twice_is_harmless() {
    let env = Env::new();
    env.up();
    let out = env.brg(&["start"]);
    assert!(out.status.success());
    assert!(
        stdout(&out)
            .lines()
            .any(|l| l.contains(&format!(" {} ", env.m("alpha")))
                && l.ends_with("  already running")),
        "{}",
        stdout(&out)
    );
    let starts = env
        .events("alpha")
        .iter()
        .filter(|e| e["event"] == "start")
        .count();
    assert_eq!(starts, 1);
}

#[test]
fn crash_shows_as_exited() {
    let env = Env::new();
    env.up();
    assert!(env.brg(&["send", "beta", "crash 3"]).status.success());
    env.wait_for("beta exited", |e| e.state_of("beta") == "exited");
    // tmux occasionally loses a pane's exit status; when it has one, it must be right.
    let beta = env
        .status()
        .into_iter()
        .find(|r| r["role"] == "beta")
        .unwrap();
    assert!(
        beta["detail"].is_null() || beta["detail"] == "status 3",
        "{beta}"
    );
    // Sending to an exited member is refused with exit code 3.
    assert_eq!(env.brg(&["send", "beta", "hi"]).status.code(), Some(3));
}

#[test]
fn send_refusals() {
    let env = Env::new();
    env.up();

    // Blocked at a dialog: refused with 4, and nothing is typed into the dialog.
    assert!(env.brg(&["send", "beta", "dialog"]).status.success());
    env.wait_for("beta blocked", |e| e.state_of("beta") == "blocked");
    assert_eq!(
        env.brg(&["send", "beta", "yes please"]).status.code(),
        Some(4)
    );
    assert!(
        !env.events("beta")
            .iter()
            .any(|e| e["event"] == "dialog-answer")
    );

    // Unknown state: refused with 5 unless --force.
    assert!(env.brg(&["send", "alpha", "mute"]).status.success());
    env.wait_for("alpha unknown", |e| e.state_of("alpha") == "unknown");
    assert_eq!(env.brg(&["send", "alpha", "hi"]).status.code(), Some(5));
    assert!(
        env.brg(&["send", "--force", "alpha", "forced"])
            .status
            .success()
    );
    env.wait_for("forced", |e| {
        submissions(e, "alpha").contains(&"forced".to_string())
    });

    // Control characters and leading slash: refused with 5.
    assert_eq!(
        env.brg(&["send", "gamma", "x\u{1b}[201~y"]).status.code(),
        Some(5)
    );
    assert_eq!(env.brg(&["send", "alpha", "/clear"]).status.code(), Some(5));
    // A role with no member yet: a usage error, 2.
    assert_eq!(env.brg(&["send", "gamma", "hi"]).status.code(), Some(2));
    // A member that isn't running: 3.
    assert!(env.brg(&["start", "gamma"]).status.success());
    assert!(env.brg(&["stop", "gamma"]).status.success());
    assert_eq!(env.brg(&["send", "gamma", "hi"]).status.code(), Some(3));

    let json = env.brg(&["send", "--json", "gamma", "hi"]);
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(v["code"], 3);
}

#[test]
fn vendor_members_mid_turn_are_busy_but_locked_ones_take_input() {
    let env = Env::new();
    env.up();
    assert!(env.brg(&["send", "beta", "work 5"]).status.success());
    env.wait_for("beta working", |e| e.state_of("beta") == "working");
    assert_eq!(env.brg(&["send", "beta", "more"]).status.code(), Some(6));
    assert!(env.brg(&["send", "alpha", "work 5"]).status.success());
    env.wait_for("alpha working", |e| e.state_of("alpha") == "working");
    assert!(env.brg(&["send", "alpha", "queued"]).status.success());
    env.wait_for("queued", |e| {
        submissions(e, "alpha").contains(&"queued".to_string())
    });
}

#[test]
fn concurrent_sends_arrive_as_separate_submissions() {
    let env = Env::new();
    env.up();
    let a = "first line A\nsecond line A";
    let b = "first line B\nsecond line B";
    let mut c1 = env.brg_cmd(&["send", "alpha", a]).spawn().unwrap();
    let mut c2 = env.brg_cmd(&["send", "alpha", b]).spawn().unwrap();
    assert!(c1.wait().unwrap().success());
    assert!(c2.wait().unwrap().success());
    env.wait_for("both submissions", |e| submissions(e, "alpha").len() >= 2);
    let subs = submissions(&env, "alpha");
    assert!(subs.contains(&a.to_string()), "{subs:?}");
    assert!(subs.contains(&b.to_string()), "{subs:?}");
}

#[test]
fn locked_gate_allows_only_granted_commands() {
    let env = Env::new();
    env.up();
    let shim = env
        .path("state")
        .read_dir()
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| {
            e.path()
                .join(format!("members/{}/bin/pager", env.m("alpha")))
        })
        .find(|p| p.exists())
        .expect("alpha's pager shim");
    let side = env.path("side-effect");
    let tries = [
        ("tool pager read 7".to_string(), true),
        (format!("tool {} read 8", shim.display()), true),
        (
            format!("tool pager read 9; touch {}", side.display()),
            false,
        ),
        ("tool pager ping 1".to_string(), false),
        ("tool pager read \"$HOME\"".to_string(), false),
        (format!("tool touch {}", side.display()), false),
    ];
    for (text, _) in &tries {
        assert!(env.brg(&["send", "alpha", text]).status.success());
    }
    env.wait_for("all tool calls", |e| {
        e.events("alpha")
            .iter()
            .filter(|x| x["event"] == "tool")
            .count()
            >= tries.len()
    });
    let tools: Vec<serde_json::Value> = env
        .events("alpha")
        .into_iter()
        .filter(|x| x["event"] == "tool")
        .collect();
    for (text, allowed) in &tries {
        let command = text.strip_prefix("tool ").unwrap();
        let event = tools
            .iter()
            .find(|t| t["command"] == command)
            .unwrap_or_else(|| panic!("no event for {command}"));
        assert_eq!(event["allowed"], *allowed, "{command}: {event}");
    }
    assert!(!side.exists(), "a denied command ran");
    let calls = walk(&env.path("state"))
        .into_iter()
        .find(|p| p.ends_with("plugins/pager/calls.log"))
        .map(|p| fs::read_to_string(p).unwrap())
        .unwrap_or_default();
    assert!(
        calls.contains(&format!("pager read 7 member={}", env.m("alpha"))),
        "{calls}"
    );
    assert!(
        calls.contains(&format!("token_len={}", SECRET.len())),
        "{calls}"
    );
}

#[test]
fn identity_comes_from_the_shim_not_the_environment() {
    let env = Env::new();
    env.up();
    let shim = walk(&env.path("state"))
        .into_iter()
        .find(|p| p.ends_with(format!("members/{}/bin/pager", env.m("alpha"))))
        .unwrap();
    let out = common::command(&shim)
        .args(["read", "5"])
        .env("BORGCHART_MEMBER", env.m("beta"))
        .env("PAGER_TOKEN", "")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let calls = walk(&env.path("state"))
        .into_iter()
        .find(|p| p.ends_with("plugins/pager/calls.log"))
        .map(|p| fs::read_to_string(p).unwrap())
        .unwrap();
    assert!(
        calls.contains(&format!("pager read 5 member={}", env.m("alpha"))),
        "{calls}"
    );
}

#[test]
fn secrets_stay_out_of_argv_logs_agents_and_tmux() {
    let env = Env::new();
    env.up();
    for member in ["alpha", "beta"] {
        let start = env
            .events(member)
            .into_iter()
            .find(|e| e["event"] == "start")
            .unwrap();
        let text = start.to_string();
        assert!(!text.contains(SECRET), "{member} saw the secret: {text}");
        let names: Vec<&str> = start["env"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert!(!names.contains(&"PAGER_TOKEN"), "{member} env: {names:?}");
        assert!(
            start["env"]["PATH"]
                .as_str()
                .unwrap()
                .starts_with(&env.path("state").display().to_string()),
            "shim dir should come first on PATH"
        );
    }
    let server_env = env.tmux(&["show-environment", "-g"]);
    assert!(
        !server_env.contains(SECRET),
        "tmux server env holds the secret"
    );
    assert!(!server_env.contains("PAGER_TOKEN"));
    let logs = stdout(&env.brg(&["logs", "pager"]));
    assert!(!logs.contains(SECRET));
    // Only the plugin's own private settings file may hold it.
    for path in walk(&env.path("state")) {
        let text = fs::read_to_string(&path).unwrap_or_default();
        if text.contains(SECRET) {
            assert!(
                path.ends_with("secrets/pager.json"),
                "secret found in {}",
                path.display()
            );
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{}", path.display());
        }
    }
    let exec_left: Vec<_> = walk(&env.path("state"))
        .into_iter()
        .filter(|p| p.to_string_lossy().contains("/exec/"))
        .collect();
    assert!(
        exec_left.is_empty(),
        "launch files left behind: {exec_left:?}"
    );
    // Processes get the secret; members don't.
    let ps = Command::new("ps").args(["-eo", "args"]).output().unwrap();
    assert!(
        !String::from_utf8_lossy(&ps.stdout).contains(SECRET),
        "secret visible in ps"
    );
}

#[test]
fn windows_get_tmux_terminal_and_colour_not_the_callers() {
    // Catches: members and plugin windows starting with no TERM/COLORTERM, so
    // agents print without colour; and the caller's own TERM (the outer
    // terminal, not tmux) leaking in instead of the server's.
    let env = Env::new();
    let out = env
        .brg_cmd(&["start"])
        .env("TERM", "caller-term")
        .env("COLORTERM", "caller-colour")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    env.wait_for("alpha idle", |e| e.state_of("alpha") == "idle");
    env.wait_for("beta idle", |e| e.state_of("beta") == "idle");
    let server_term = env.tmux(&["show-options", "-gv", "default-terminal"]);
    let server_term = server_term.trim();
    assert!(!server_term.is_empty());
    for member in ["alpha", "beta"] {
        let start = env
            .events(member)
            .into_iter()
            .find(|e| e["event"] == "start")
            .unwrap();
        assert_eq!(start["env"]["TERM"], server_term, "{member}");
        assert_eq!(start["env"]["COLORTERM"], "truecolor", "{member}");
        // Only TERM comes from the pane, not the rest of its environment.
        assert!(start["env"].get("TMUX").is_none(), "{member}");
    }
    env.wait_for("pager window env", |e| {
        walk(&e.path("state"))
            .iter()
            .any(|p| p.ends_with("plugins/pager/window-env"))
    });
    let plugin_env = walk(&env.path("state"))
        .into_iter()
        .find(|p| p.ends_with("plugins/pager/window-env"))
        .map(|p| fs::read_to_string(p).unwrap())
        .unwrap();
    let lines: Vec<&str> = plugin_env.lines().collect();
    assert!(
        lines.contains(&format!("TERM={server_term}").as_str()),
        "{plugin_env}"
    );
    assert!(lines.contains(&"COLORTERM=truecolor"), "{plugin_env}");
}

/// tmux's own `default-terminal`, from a server that reads no config.
fn built_in_default_terminal() -> String {
    // Its server stops when `tmux` drops.
    let tmux = common::PrivateTmux::new();
    let socket = tmux.socket("builtin");
    let out = tmux
        .tmux(&socket)
        .args(["start-server", ";", "show", "-gv", "default-terminal"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn user_tmux_config_loads_only_when_opted_in_and_brg_options_win() {
    // Catches: brg reading your tmux config without the opt-in; your bindings
    // missing with it; and your config's options breaking brg (windows renamed
    // away from member names, a short history, another terminal or shell, a
    // server or session that ends while detached), including options a
    // blocking `run` script sets and lines after an error.
    let built_in = built_in_default_terminal();
    for opted_in in [false, true] {
        let env = Env::new();
        env.write(
            "home/.tmux.conf",
            "set -g prefix C-a\nbind-key -n F12 split-window\nset -g bogus-option 1\nsetw -g automatic-rename on\nset -g default-terminal screen-256color\nset -g default-shell /bin/false\nset -g history-limit 100\nset -g destroy-unattached on\nset -s exit-unattached on\nrun 'tmux -S #{socket_path} set -g @ran yes; tmux -S #{socket_path} set -g history-limit 200'\n",
        );
        if opted_in {
            env.config("tmux: {user_config: true}\n");
        }
        env.up();
        let keys = env.tmux(&["list-keys", "-T", "root"]);
        assert_eq!(keys.contains("F12"), opted_in, "{keys}");
        let prefix = env.tmux(&["show", "-gv", "prefix"]);
        assert_eq!(prefix.trim() == "C-a", opted_in, "prefix {prefix}");
        let ran = env.tmux(&["show", "-gqv", "@ran"]);
        assert_eq!(ran.trim() == "yes", opted_in, "the config's run script");
        let names = env.tmux(&["list-windows", "-a", "-F", "#{window_name}"]);
        assert!(
            names.lines().any(|n| n == env.m("alpha")) && names.lines().any(|n| n == env.m("beta")),
            "{names}"
        );
        for (option, want) in [
            ("history-limit", "20000"),
            ("destroy-unattached", "off"),
            ("default-terminal", built_in.as_str()),
        ] {
            assert_eq!(
                env.tmux(&["show", "-gv", option]).trim(),
                want,
                "{option}, opted in: {opted_in}"
            );
        }
        assert_eq!(env.tmux(&["show", "-sv", "exit-unattached"]).trim(), "off");
        let start = env
            .events("alpha")
            .into_iter()
            .find(|e| e["event"] == "start")
            .unwrap();
        assert_eq!(start["env"]["TERM"], built_in.as_str());
        assert_eq!(start["env"]["COLORTERM"], "truecolor");
        let out = env.brg(&["stop"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(env.tmux(&["list-sessions"]), "");
    }
}

#[test]
fn locked_member_without_an_active_gate_is_stopped() {
    let env = Env::new();
    fs::create_dir_all(env.path("home/fake-agent")).unwrap();
    fs::write(env.path("home/fake-agent/no-hooks"), "").unwrap();
    let out = env
        .brg_cmd(&["start"])
        .env("BORGCHART_GATE_WAIT_MS", "1500")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stdout(&out).contains("did not report in"),
        "{}",
        stdout(&out)
    );
    assert_eq!(env.state_of("alpha"), "stopped");
}

#[test]
fn launch_arguments_follow_posture() {
    let env = Env::new();
    env.up();
    let argv = |member: &str| -> Vec<String> {
        env.events(member)
            .into_iter()
            .find(|e| e["event"] == "start")
            .unwrap()["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    let alpha = argv("alpha");
    for flag in [
        "--restricted",
        "--permission-mode",
        "dontAsk",
        "--settings",
        "--strict-mcp-config",
        "--model",
        "haiku",
    ] {
        assert!(
            alpha.iter().any(|a| a == flag),
            "alpha lacks {flag}: {alpha:?}"
        );
    }
    assert!(
        alpha
            .iter()
            .any(|a| a == "Bash(pager read:*)" || a.contains("Bash(pager read:*)")),
        "{alpha:?}"
    );
    let prompt = alpha.last().unwrap();
    assert!(prompt.contains("You are alpha."));
    assert!(prompt.contains("## Commands you can run"));
    assert!(prompt.contains(&format!("/members/{}/bin/pager read <id>", env.m("alpha"))));
    assert!(!prompt.contains("ping"));

    let beta = argv("beta");
    assert_eq!(
        beta[beta.len() - 2],
        "--",
        "the prompt must follow `--`: {beta:?}"
    );
    assert!(
        !beta.iter().any(|a| a == "-a" || a == "--restricted"),
        "vendor posture passes no permission flags: {beta:?}"
    );
    assert!(
        beta.iter().any(|a| a == "model_reasoning_effort=\"low\""),
        "{beta:?}"
    );
    // An unattended Codex member must not stop at the update prompt.
    assert!(
        beta.windows(2)
            .any(|w| w[0] == "-c" && w[1] == "check_for_update_on_startup=false"),
        "{beta:?}"
    );
}

#[test]
fn role_args_decide_codex_update_checks() {
    // (the role's args, the update-check arguments the launch should carry)
    let cases: [(&str, &[&str]); 3] = [
        (
            "[-c, check_for_update_on_startup=true]",
            &["check_for_update_on_startup=true"],
        ),
        (
            "['--config=check_for_update_on_startup=false']",
            &["--config=check_for_update_on_startup=false"],
        ),
        // A value that only mentions the key doesn't set it.
        (
            "[-c, 'developer_instructions=\"Explain check_for_update_on_startup\"']",
            &[
                "check_for_update_on_startup=false",
                "developer_instructions=\"Explain check_for_update_on_startup\"",
            ],
        ),
    ];
    for (args, want) in cases {
        let env = Env::new();
        env.role_extra("beta", &format!("    args: {args}\n"));
        env.up();
        let beta: Vec<String> = env
            .events("beta")
            .into_iter()
            .find(|e| e["event"] == "start")
            .unwrap()["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        let set: Vec<&String> = beta
            .iter()
            .filter(|a| a.contains("check_for_update_on_startup"))
            .collect();
        assert_eq!(set, want, "{args}: {beta:?}");
    }
}

/// Claude's trust dialog comes up; `config` is brg's configuration.
fn trust_env(config: &str) -> Env {
    let env = Env::new();
    fs::create_dir_all(env.path("home/fake-agent")).unwrap();
    fs::write(env.path("home/fake-agent/trust"), "").unwrap();
    env.config(config);
    env
}

#[test]
fn folder_trust_is_accepted_for_a_locked_members_folder_by_default() {
    let env = trust_env("");
    let out = env.brg(&["start"]);
    assert!(
        out.status.success(),
        "{}{}",
        stdout(&out),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout(&out).contains("started; accepted folder trust for"),
        "{}",
        stdout(&out)
    );
    assert!(
        env.events("alpha")
            .iter()
            .any(|e| e["event"] == "trust-accepted")
    );
    env.wait_for("alpha idle", |e| e.state_of("alpha") == "idle");
    let alpha = env
        .status()
        .into_iter()
        .find(|r| r["role"] == "alpha")
        .unwrap();
    assert!(
        alpha.get("detail").is_none(),
        "gate must be active after trust: {alpha}"
    );
    assert!(
        alpha["trust_accepted"].as_str().unwrap().ends_with("/work"),
        "{alpha}"
    );
}

#[test]
fn folder_trust_is_left_alone_when_turned_off() {
    {
        let env = trust_env("auto_trust: false\n");
        let out = env.brg(&["start"]);
        assert!(
            stdout(&out).contains("started; waiting at Claude's folder-trust prompt"),
            "{}",
            stdout(&out)
        );
        assert!(
            !env.events("alpha")
                .iter()
                .any(|e| e["event"].as_str().unwrap_or("").starts_with("trust-"))
        );
        assert_eq!(env.state_of("alpha"), "blocked");
        // Its gate hasn't reported in, so it gets no input.
        assert_eq!(
            env.brg(&["send", "--force", "alpha", "hi"]).status.code(),
            Some(5)
        );
    }
}

#[test]
fn folder_trust_is_left_alone_when_yes_is_already_highlighted() {
    let env = trust_env("");
    fs::remove_file(env.path("home/fake-agent/trust")).unwrap();
    fs::write(env.path("home/fake-agent/trust-yes"), "").unwrap();
    let out = env.brg(&["start"]);
    assert!(
        stdout(&out).contains("started; waiting at Claude's folder-trust prompt"),
        "{}",
        stdout(&out)
    );
    assert!(
        !env.events("alpha")
            .iter()
            .any(|e| e["event"] == "trust-accepted")
    );
}

/// Codex's trust screen comes up; `config` is brg's configuration and
/// `beta` extra lines for beta's role.
fn codex_trust_env(config: &str, beta: &str) -> Env {
    let env = Env::new();
    fs::create_dir_all(env.path("home/fake-agent")).unwrap();
    fs::write(env.path("home/fake-agent/codex-trust"), "").unwrap();
    env.config(config);
    if !beta.is_empty() {
        env.role_extra("beta", beta);
    }
    env
}

fn start_argv(env: &Env, member: &str) -> Vec<String> {
    env.events(member)
        .into_iter()
        .find(|e| e["event"] == "start")
        .unwrap()["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

fn status_of(env: &Env, name: &str) -> serde_json::Value {
    env.status()
        .into_iter()
        .find(|r| r["role"] == name)
        .unwrap()
}

#[test]
fn codex_auto_trust_trusts_exactly_the_declared_folder_for_the_session() {
    // Catches: a Codex member with `auto_trust: true` sitting at its
    // folder-trust screen, or a folder other than its own trusted.
    let env = codex_trust_env("auto_trust: true\n", "");
    let out = env.brg(&["start"]);
    assert!(
        out.status.success(),
        "{}{}",
        stdout(&out),
        String::from_utf8_lossy(&out.stderr)
    );
    let work = env.path("work").canonicalize().unwrap();
    assert!(
        stdout(&out).contains(&format!(
            "trusted folder {} for this Codex session",
            work.display()
        )),
        "{}",
        stdout(&out)
    );
    env.wait_for("beta idle", |e| e.state_of("beta") == "idle");
    let trusts: Vec<String> = start_argv(&env, "beta")
        .into_iter()
        .filter(|a| a.starts_with("projects="))
        .collect();
    assert_eq!(
        trusts,
        [format!(
            "projects={{{}={{trust_level=\"trusted\"}}}}",
            serde_json::to_string(&work.display().to_string()).unwrap()
        )]
    );
    assert_eq!(
        status_of(&env, "beta")["trust_accepted"],
        work.display().to_string()
    );
}

#[test]
fn codex_trust_screen_is_left_for_a_person_unless_auto_trust_covers_the_folder() {
    // Catches: Codex trust passed without `auto_trust: true` (including the
    // `locked` default that covers Claude), or over the role's own
    // `projects` setting.
    for (config, beta) in [
        ("auto_trust: false\n", ""),
        ("", "    posture: locked\n    cwd: sub\n"),
        ("auto_trust: true\n", "    posture: locked\n    cwd: sub\n"),
        ("auto_trust: true\n", "    args: [-c, 'projects={}']\n"),
    ] {
        let deploy = format!("{config}{beta}");
        let env = codex_trust_env(config, beta);
        fs::create_dir_all(env.path("work/sub")).unwrap();
        let out = env
            .brg_cmd(&["start"])
            .env("BORGCHART_GATE_WAIT_MS", "1500")
            .output()
            .unwrap();
        assert!(
            !stdout(&out).contains("trusted folder"),
            "{deploy}: {}",
            stdout(&out)
        );
        env.wait_for("alpha idle", |e| e.state_of("alpha") == "idle");
        let trusts: Vec<String> = start_argv(&env, "beta")
            .into_iter()
            .filter(|a| a.starts_with("projects=") && a != "projects={}")
            .collect();
        assert!(trusts.is_empty(), "{deploy}: {trusts:?}");
        let beta = status_of(&env, "beta");
        assert!(beta["trust_accepted"].is_null(), "{deploy}: {beta}");
        assert_ne!(beta["state"], "idle", "{deploy}: {beta}");
    }
}

#[test]
fn locked_member_at_a_prompt_before_its_gate_reports_is_stopped() {
    let env = Env::new();
    fs::create_dir_all(env.path("home/fake-agent")).unwrap();
    fs::write(env.path("home/fake-agent/dialog-first"), "").unwrap();
    let out = env.brg(&["start"]);
    assert_eq!(out.status.code(), Some(1), "{}", stdout(&out));
    assert!(
        stdout(&out)
            .lines()
            .any(|l| l.contains(&format!(" {} ", env.m("alpha")))
                && l.contains("  stopped: it was at a prompt")),
        "{}",
        stdout(&out)
    );
    assert_eq!(env.state_of("alpha"), "stopped");
    assert!(
        !env.events("alpha")
            .iter()
            .any(|e| e["event"] == "dialog-answer")
    );
}

#[test]
fn send_from_must_target_the_senders_parent() {
    let env = Env::new();
    env.up();
    assert!(
        env.brg(&["send", "--from", "beta", "alpha", "note"])
            .status
            .success()
    );
    assert_eq!(
        env.brg(&["send", "--from", "alpha", "beta", "note"])
            .status
            .code(),
        Some(5)
    );
    // A role with no member isn't a sender.
    assert_eq!(
        env.brg(&["send", "--from", "gamma", "beta", "note"])
            .status
            .code(),
        Some(2)
    );
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}
