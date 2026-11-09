//! The status sidebar brg's tmux config adds beside a member window someone
//! views: real clients (attached from panes of an outer test server, which
//! gives them terminals) on a private brg server, fake agents.
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
        let brg = PathBuf::from(env!("CARGO_BIN_EXE_brg"));
        let dir = tempfile::tempdir().unwrap();
        let tmux = common::PrivateTmux::new();
        let socket = tmux.socket("brg-side");
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
        env.write(
            "team/team.yaml",
            "bus: {}\nplugins:\n  pager: {path: plugins/pager, process: [./watch.sh]}\nroles:\n  alpha: {prompt: alpha.md}\n  beta: {prompt: beta.md, vendor: codex, parent: alpha, accept_risk: true}\n  gamma: {prompt: gamma.md, parent: alpha, start: false}\n",
        );
        env.write(
            "team/plugins/pager/watch.sh",
            "#!/bin/sh\nwhile true; do sleep 1; done\n",
        );
        common::write_executable(
            &env.path("team/plugins/pager/watch.sh"),
            "#!/bin/sh\nwhile true; do sleep 1; done\n",
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

    /// Create an instance working in `work` (a folder under the test's).
    fn create(&self, work: &str) -> String {
        let before = self.instances();
        let team = self.path("team").display().to_string();
        fs::create_dir_all(self.path(work)).unwrap();
        let work = self.path(work).display().to_string();
        let out = self.brg(&["create", &team, &work]);
        assert!(out.status.success(), "{out:?}");
        let id = self
            .instances()
            .into_iter()
            .find(|i| !before.contains(i))
            .unwrap();
        self.wait_for("alpha and beta idle", || {
            self.state_of(&id, "alpha") == "idle" && self.state_of(&id, "beta") == "idle"
        });
        id
    }

    fn instances(&self) -> Vec<String> {
        let Ok(text) = fs::read(self.path("state/registry.json")) else {
            return Vec::new();
        };
        let reg: serde_json::Value = serde_json::from_slice(&text).unwrap();
        reg["instances"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
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

    /// Every pane on brg's server.
    fn panes(&self) -> Vec<Pane> {
        self.tmux(&[
            "list-panes",
            "-a",
            "-F",
            "#{session_name}:#{window_name}\t#{pane_id}\t#{@brg-sidebar}\t#{pane_active}\t#{pane_width}\t#{window_width}",
        ])
        .lines()
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            Pane {
                window: f[0].to_string(),
                id: f[1].to_string(),
                sidebar: !f[2].is_empty(),
                active: f[3] == "1",
                width: f[4].parse().unwrap(),
                window_width: f[5].parse().unwrap(),
            }
        })
        .collect()
    }

    /// The windows (`<session>:<window>`) with a sidebar, once per sidebar.
    fn sidebar_windows(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .panes()
            .into_iter()
            .filter(|p| p.sidebar)
            .map(|p| p.window)
            .collect();
        out.sort();
        out
    }

    /// The pane in `window` that isn't a sidebar (there is one).
    fn member_pane(&self, window: &str) -> Pane {
        let panes: Vec<Pane> = self
            .panes()
            .into_iter()
            .filter(|p| p.window == window && !p.sidebar)
            .collect();
        assert_eq!(panes.len(), 1, "{panes:?}");
        panes[0].clone()
    }

    /// What `member` was given, from the fake agent's log.
    fn submissions(&self, member: &str) -> Vec<String> {
        fs::read_to_string(self.path(&format!("home/fake-agent/{member}.jsonl")))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|e| e["event"] == "submit")
            .map(|e| e["text"].as_str().unwrap_or("").to_string())
            .collect()
    }

    /// The windows (`<session>:<window>`) brg's server's clients view.
    fn viewed(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .tmux(&["list-clients", "-F", "#{session_name}:#{window_name}"])
            .lines()
            .map(String::from)
            .collect();
        out.sort();
        out
    }
}

#[derive(Debug, Clone)]
struct Pane {
    window: String,
    id: String,
    sidebar: bool,
    active: bool,
    width: u32,
    window_width: u32,
}

/// Real clients on brg's server, each in a session (named for it) of an
/// outer test server, whose pane is its terminal.
struct Clients<'a> {
    env: &'a Env,
    outer: String,
    socket: String,
}

impl<'a> Clients<'a> {
    fn new(env: &'a Env, outer: &str) -> Self {
        let socket = env.tmux(&["display", "-p", "#{socket_path}"]);
        Clients {
            env,
            outer: env.tmux.socket(outer),
            socket: socket.trim_end().to_string(),
        }
    }

    fn outer(&self, args: &[&str]) -> String {
        let out = self.env.tmux.tmux(&self.outer).args(args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// Attach client `name` to `target` on a `width` x `height` terminal.
    fn attach(&self, name: &str, target: &str, width: u32, height: u32) {
        let (w, h) = (width.to_string(), height.to_string());
        let command = format!(
            "env -u TMUX {} -S '{}' attach -t '{target}'",
            self.env.tmux.guard().display(),
            self.socket
        );
        self.outer(&[
            "new-session",
            "-d",
            "-s",
            name,
            "-x",
            &w,
            "-y",
            &h,
            &command,
        ]);
    }

    fn keys(&self, name: &str, keys: &[&str]) {
        let target = format!("={name}:");
        let mut args = vec!["send-keys", "-t", target.as_str()];
        args.extend(keys);
        self.outer(&args);
    }

    fn resize(&self, name: &str, width: u32, height: u32) {
        let target = format!("={name}:");
        let (w, h) = (width.to_string(), height.to_string());
        self.outer(&["resize-window", "-t", &target, "-x", &w, "-y", &h]);
    }

    /// The terminal client `name` runs on, as brg's server names it.
    fn tty(&self, name: &str) -> String {
        self.outer(&["display", "-p", "-t", &format!("={name}:"), "#{pane_tty}"])
            .trim_end()
            .to_string()
    }
}

/// Nothing more happens for a while: hooks that would act have run.
fn settle() {
    std::thread::sleep(Duration::from_millis(800));
}

#[test]
fn the_sidebar_follows_a_client_between_members_and_goes_when_it_detaches() {
    let env = Env::new();
    let id = env.create("work");
    let session = format!("brg-{id}");
    let at = |w: &str| format!("{session}:{w}");
    let alpha = env.member(&id, "alpha");
    let beta = env.member(&id, "beta");
    let clients = Clients::new(&env, "brg-side-one");

    clients.attach("a", &format!("={session}:={alpha}"), 160, 30);
    env.wait_for("a sidebar beside alpha", || {
        env.sidebar_windows() == [at(&alpha)]
    });
    // 42 wide; the member keeps the focus and the rest of the width.
    let side = env.panes().into_iter().find(|p| p.sidebar).unwrap();
    let member = env.member_pane(&at(&alpha));
    assert_eq!((side.width, side.active), (42, false), "{side:?}");
    assert_eq!(
        (member.width, member.active),
        (160 - 42 - 1, true),
        "{member:?}"
    );
    // It shows the tree, and the narrower member's state still reads.
    env.wait_for("the tree in the sidebar", || {
        env.tmux(&["capture-pane", "-p", "-t", &side.id])
            .contains(&beta)
    });
    env.wait_for("alpha idle", || env.state_of(&id, "alpha") == "idle");

    // It moves with the client to another window.
    env.tmux(&["select-window", "-t", &format!("={session}:={beta}")]);
    env.wait_for("the sidebar beside beta only", || {
        env.sidebar_windows() == [at(&beta)]
    });
    let left = env.member_pane(&at(&alpha));
    assert_eq!(left.width, left.window_width, "{left:?}");
    env.wait_for("beta idle", || env.state_of(&id, "beta") == "idle");

    // A plugin process's window gets none.
    env.tmux(&["select-window", "-t", &format!("={session}:=_pager")]);
    env.wait_for("no sidebar on the plugin's window", || {
        env.viewed() == [at("_pager")] && env.sidebar_windows().is_empty()
    });
    env.tmux(&["select-window", "-t", &format!("={session}:={beta}")]);
    env.wait_for("the sidebar beside beta again", || {
        env.sidebar_windows() == [at(&beta)]
    });

    // prefix S hides it, on every window, until pressed again.
    clients.keys("a", &["C-b", "S"]);
    env.wait_for("no sidebar once hidden", || {
        env.sidebar_windows().is_empty()
    });
    env.tmux(&["select-window", "-t", &format!("={session}:={alpha}")]);
    env.wait_for("the client on alpha", || env.viewed() == [at(&alpha)]);
    settle();
    assert!(env.sidebar_windows().is_empty());
    clients.keys("a", &["C-b", "S"]);
    env.wait_for("the sidebar back beside alpha", || {
        env.sidebar_windows() == [at(&alpha)]
    });

    // Detached, the member has its whole window again.
    clients.keys("a", &["C-b", "d"]);
    env.wait_for("no sidebar after detaching", || {
        env.viewed().is_empty() && env.sidebar_windows().is_empty()
    });
    let back = env.member_pane(&at(&alpha));
    assert_eq!(back.width, back.window_width, "{back:?}");

    // A narrow client gets none, until it is wide enough.
    clients.attach("n", &format!("={session}:={alpha}"), 70, 20);
    env.wait_for("the narrow client", || env.viewed() == [at(&alpha)]);
    settle();
    assert!(env.sidebar_windows().is_empty());
    clients.resize("n", 120, 30);
    env.wait_for("a sidebar once it is wide", || {
        env.sidebar_windows() == [at(&alpha)]
    });
    // A third of the window, below three times the sidebar's width.
    let side = env.panes().into_iter().find(|p| p.sidebar).unwrap();
    assert_eq!(side.width, 40, "{side:?}");

    // A sidebar whose program ends closes; no dead pane is left.
    let pid = env.tmux(&["display", "-p", "-t", &side.id, "#{pane_pid}"]);
    let status = Command::new("kill").arg(pid.trim()).status().unwrap();
    assert!(status.success());
    env.wait_for("the sidebar closed", || {
        env.panes().iter().all(|p| p.id != side.id)
    });
    assert_eq!(
        env.tmux(&["list-panes", "-a", "-F", "#{pane_dead}"])
            .matches('1')
            .count(),
        0
    );
}

#[test]
fn a_viewed_window_keeps_one_sidebar_whoever_else_comes_and_goes() {
    let env = Env::new();
    let one = env.create("work");
    let two = env.create("work2");
    let first = format!("brg-{one}:{}", env.member(&one, "alpha"));
    let second = format!("brg-{two}:{}", env.member(&two, "alpha"));
    let target = |w: &str| {
        let (s, w) = w.split_once(':').unwrap();
        format!("={s}:={w}")
    };
    let clients = Clients::new(&env, "brg-side-two");

    // Two clients on different members: one sidebar each.
    clients.attach("a", &target(&first), 120, 30);
    clients.attach("b", &target(&second), 120, 30);
    let mut both = vec![first.clone(), second.clone()];
    both.sort();
    env.wait_for("a sidebar beside each", || env.sidebar_windows() == both);

    // B joins A's window: one sidebar there, none left behind.
    let b = clients.tty("b");
    env.tmux(&["switch-client", "-c", &b, "-t", &target(&first)]);
    env.wait_for("one sidebar, beside the shared window", || {
        env.sidebar_windows() == [first.clone()]
    });
    // A leaves; B still views it, so it stays.
    clients.keys("a", &["C-b", "d"]);
    env.wait_for("only B attached", || env.viewed() == [first.clone()]);
    settle();
    assert_eq!(env.sidebar_windows(), std::slice::from_ref(&first));
    clients.keys("b", &["C-b", "d"]);
    env.wait_for("no sidebar", || {
        env.viewed().is_empty() && env.sidebar_windows().is_empty()
    });

    // Clients attaching at once still get one.
    for round in 0..3 {
        let (x, y) = (format!("x{round}"), format!("y{round}"));
        clients.attach(&x, &target(&first), 120, 30);
        clients.attach(&y, &target(&first), 120, 30);
        env.wait_for("both attached", || env.viewed().len() == 2);
        env.wait_for("a sidebar", || !env.sidebar_windows().is_empty());
        settle();
        assert_eq!(
            env.sidebar_windows(),
            std::slice::from_ref(&first),
            "round {round}"
        );
        clients.keys(&x, &["C-b", "d"]);
        clients.keys(&y, &["C-b", "d"]);
        env.wait_for("no sidebar", || {
            env.viewed().is_empty() && env.sidebar_windows().is_empty()
        });
    }
}

#[test]
fn with_the_sidebar_focused_brg_still_reads_types_into_and_stops_the_member() {
    let env = Env::new();
    let id = env.create("work");
    let session = format!("brg-{id}");
    let at = |w: &str| format!("{session}:{w}");
    let alpha = env.member(&id, "alpha");
    let beta = env.member(&id, "beta");
    let clients = Clients::new(&env, "brg-side-focus");
    clients.attach("a", &format!("={session}:={alpha}"), 120, 30);
    env.wait_for("a sidebar beside alpha", || {
        env.sidebar_windows() == [at(&alpha)]
    });
    let side = env.panes().into_iter().find(|p| p.sidebar).unwrap();
    env.tmux(&["select-pane", "-t", &side.id]);
    assert!(env.panes().iter().any(|p| p.id == side.id && p.active));

    // The member redraws at its new width; its state reads as before.
    env.wait_for("alpha idle", || env.state_of(&id, "alpha") == "idle");
    let out = env.brg(&[
        "send",
        "--external",
        "test",
        &format!("{id}/{alpha}"),
        "hello",
    ]);
    assert!(out.status.success(), "{out:?}");
    env.wait_for("alpha's submission", || {
        env.submissions(&alpha).iter().any(|t| t.contains("hello"))
    });
    // The sidebar got none of it: it still runs, focused.
    assert!(env.panes().iter().any(|p| p.id == side.id && p.active));

    let out = env.brg(&["stop", &format!("{id}/{alpha}")]);
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains("may still be running"), "{text}");
    assert_eq!(env.state_of(&id, "alpha"), "stopped");
    // The client is on another window now, and a member's has the sidebar.
    env.wait_for("the sidebar where the client went", || {
        let viewed = env.viewed();
        let want: Vec<String> = viewed
            .iter()
            .filter(|w| **w == at(&beta))
            .cloned()
            .collect();
        viewed.len() == 1 && viewed[0] != at(&alpha) && env.sidebar_windows() == want
    });
}
