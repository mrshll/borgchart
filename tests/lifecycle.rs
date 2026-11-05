//! Lifecycle and runtime binding: validation before mutation, child starts
//! into a running deployment, the pinned runtime, run identity, generations,
//! incarnations, liveness and rebinds. Real tmux on a private per-test
//! socket, with `fake-agent` standing in for `claude` and `codex`.
//!
//! Run with `cargo test --features test-support`.
#![cfg(feature = "test-support")]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;
use sha2::{Digest, Sha256};

const SECRET: &str = "pager-token-not-real-7c2e";

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Env {
    dir: tempfile::TempDir,
    socket: String,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-L", &self.socket, "kill-server"])
            .output();
    }
}

const TEAM: &str = r#"
plugins:
  pager:
    path: plugins/pager
    env: [PAGER_TOKEN, PAGER_ROOM]
    actions:
      read: pager read <id>
members:
  lead:
    prompt: lead.md
    plugins: {pager: [read]}
  helper:
    prompt: helper.md
    vendor: codex
    parent: lead
    start: false
"#;

impl Env {
    /// A two-member team; with `bus`, a deployment with `bus: {root, run: run-1}`.
    fn new(bus: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = format!(
            "brg-lc-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        );
        let env = Env { dir, socket };
        let bin = env.path("bin");
        fs::create_dir_all(&bin).unwrap();
        let fake = env!("CARGO_BIN_EXE_fake-agent");
        std::os::unix::fs::symlink(fake, bin.join("claude")).unwrap();
        std::os::unix::fs::symlink(fake, bin.join("codex")).unwrap();
        fs::create_dir_all(env.path("home")).unwrap();
        env.write("team/team.yaml", TEAM);
        env.write("team/lead.md", "---\nrole: lead\n---\nYou are the lead.\n");
        env.write(
            "team/helper.md",
            "---\nrole: helper\n---\nYou are the helper.\n",
        );
        env.script(
            "team/plugins/pager/pager",
            "#!/bin/sh\necho \"pager $* member=$BORGCHART_MEMBER token_len=${#PAGER_TOKEN}\" >> \"$BORGCHART_STATE_DIR/calls.log\"\necho ok\n",
        );
        env.write_deploy(if bus { Some("run-1") } else { None }, None);
        env
    }

    fn write_deploy(&self, run: Option<&str>, continues: Option<&str>) {
        let mut text =
            "schema: 1\nplugins:\n  pager:\n    env: {PAGER_ROOM: R0000EXAMPLE}\n".to_string();
        if let Some(run) = run {
            text.push_str(&format!(
                "bus:\n  root: {}\n  run: {run}\n",
                self.path("runs").display()
            ));
            if let Some(prev) = continues {
                text.push_str(&format!("  continues: {prev}\n"));
            }
        }
        self.write("team/borgchart.yaml", &text);
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

    fn base_path(&self) -> String {
        format!(
            "{}:/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin",
            self.path("bin").display()
        )
    }

    /// `brg` (or another binary) with the usual test environment.
    fn cmd_with(&self, program: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new(program);
        cmd.env_clear()
            .env("PATH", self.base_path())
            .env("HOME", self.path("home"))
            .env("LANG", "C.UTF-8")
            .env("BORGCHART_STATE_HOME", self.path("state"))
            .env("BORGCHART_TMUX_SOCKET", &self.socket)
            .env("PAGER_TOKEN", SECRET)
            .current_dir(self.path("team"))
            .args(args);
        cmd
    }

    fn brg_cmd(&self, args: &[&str]) -> Command {
        self.cmd_with(Path::new(env!("CARGO_BIN_EXE_brg")), args)
    }

    fn brg(&self, args: &[&str]) -> Output {
        self.brg_cmd(args).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> Output {
        let out = self.brg(args);
        assert!(out.status.success(), "brg {args:?} failed:\n{}", both(&out));
        out
    }

    fn tmux(&self, args: &[&str]) -> String {
        let out = Command::new("tmux")
            .arg("-L")
            .arg(&self.socket)
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn events(&self, member: &str) -> Vec<Value> {
        fs::read_to_string(self.path(&format!("home/fake-agent/{member}.jsonl")))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn starts(&self, member: &str) -> Vec<Value> {
        self.events(member)
            .into_iter()
            .filter(|e| e["event"] == "start")
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
        panic!(
            "timed out waiting for {what}\nwindows: {}",
            self.tmux(&["list-windows", "-a"])
        );
    }

    fn state_of(&self, name: &str) -> String {
        let out = self.ok(&["status", "--json"]);
        let rows: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
        rows.iter()
            .find(|r| r["name"] == name)
            .map(|r| r["state"].as_str().unwrap_or("").to_string())
            .unwrap_or_default()
    }

    fn up(&self) {
        self.ok(&["up"]);
        self.wait_for("lead idle", |e| e.state_of("lead") == "idle");
    }

    fn up_helper(&self) {
        self.ok(&["up", "--member", "helper"]);
        self.wait_for("helper idle", |e| e.state_of("helper") == "idle");
    }

    fn run_dir(&self, run: &str) -> PathBuf {
        self.path("runs").join(run)
    }

    fn show(&self) -> Value {
        let out = self.ok(&["run", "show", "--json"]);
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn state_dir(&self) -> PathBuf {
        fs::read_dir(self.path("state"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.is_dir())
            .expect("a deployment state dir")
    }

    /// Every file under the state home and the bus root, with its bytes.
    fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut out = BTreeMap::new();
        for root in [self.path("state"), self.path("runs")] {
            for p in walk(&root) {
                out.insert(p.clone(), fs::read(&p).unwrap_or_default());
            }
        }
        out
    }

    /// A copy of the built brg with extra bytes, so its hash differs.
    fn other_brg(&self, name: &str) -> PathBuf {
        let path = self.path(name);
        let mut bytes = fs::read(env!("CARGO_BIN_EXE_brg")).unwrap();
        bytes.extend_from_slice(format!("\nnot the same build: {name}\n").as_bytes());
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
}

fn both(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn sha(path: &Path) -> String {
    Sha256::digest(fs::read(path).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
        .unwrap()
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

fn assert_unchanged(before: &BTreeMap<PathBuf, Vec<u8>>, after: &BTreeMap<PathBuf, Vec<u8>>) {
    let added: Vec<_> = after.keys().filter(|k| !before.contains_key(*k)).collect();
    let removed: Vec<_> = before.keys().filter(|k| !after.contains_key(*k)).collect();
    let changed: Vec<_> = before
        .iter()
        .filter(|(k, v)| after.get(*k).is_some_and(|a| a != *v))
        .map(|(k, _)| k)
        .collect();
    assert!(
        added.is_empty() && removed.is_empty() && changed.is_empty(),
        "files changed: added {added:?}, removed {removed:?}, changed {changed:?}"
    );
}

#[test]
fn missing_values_write_nothing() {
    for bus in [false, true] {
        let env = Env::new(bus);
        let out = env
            .brg_cmd(&["up"])
            .env_remove("PAGER_TOKEN")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{}", both(&out));
        assert!(both(&out).contains("pager: PAGER_TOKEN"), "{}", both(&out));
        assert!(
            env.snapshot().is_empty() && !env.path("state").exists() && !env.path("runs").exists(),
            "a refused first start wrote {:?}",
            env.snapshot().keys().collect::<Vec<_>>()
        );

        // A later fresh start, refused the same way, changes nothing either.
        env.up();
        env.ok(&["down"]);
        let before = env.snapshot();
        let out = env
            .brg_cmd(&["up"])
            .env_remove("PAGER_TOKEN")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{}", both(&out));
        assert_unchanged(&before, &env.snapshot());
        assert_eq!(env.tmux(&["list-sessions"]), "");
    }
}

/// Start `helper` into the running team from a caller environment built by
/// `caller`, and return helper's environment.
fn child_start_env(caller: impl Fn(&mut Command)) -> (Env, BTreeMap<String, String>) {
    let env = Env::new(true);
    env.up();
    let lead_env = env.starts("lead")[0]["env"].clone();
    let state = env.state_dir();
    let meta_before = fs::read(state.join("deployment.json")).unwrap();
    let secrets_before = fs::read(state.join("secrets/pager.json")).unwrap();
    let lead_shim_before = fs::read(state.join("members/lead/bin/pager")).unwrap();

    let mut cmd = env.brg_cmd(&["up", "--member", "helper"]);
    caller(&mut cmd);
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{}", both(&out));
    assert!(both(&out).contains("helper: started"), "{}", both(&out));
    env.wait_for("helper start", |e| !e.starts("helper").is_empty());

    assert_eq!(
        fs::read(state.join("deployment.json")).unwrap(),
        meta_before
    );
    assert_eq!(
        fs::read(state.join("secrets/pager.json")).unwrap(),
        secrets_before
    );
    assert_eq!(
        fs::read(state.join("members/lead/bin/pager")).unwrap(),
        lead_shim_before
    );
    let helper: BTreeMap<String, String> =
        serde_json::from_value(env.starts("helper")[0]["env"].clone()).unwrap();
    let shims = state.join("members/helper/bin").display().to_string();
    let lead_path = lead_env["PATH"].as_str().unwrap();
    let base_path = lead_path.split_once(':').unwrap().1;
    assert_eq!(helper["PATH"], format!("{shims}:{base_path}"));
    assert_eq!(helper["HOME"], lead_env["HOME"].as_str().unwrap());
    assert_eq!(helper["LANG"], "C.UTF-8");
    assert_eq!(
        helper["TMPDIR"],
        state.join("tmp/helper").display().to_string()
    );
    assert_eq!(
        helper["BORGCHART_BIN"],
        lead_env["BORGCHART_BIN"].as_str().unwrap()
    );
    assert!(!helper.contains_key("PAGER_TOKEN"));
    (env, helper)
}

#[test]
fn child_start_ignores_a_contaminated_caller_env() {
    let (_env, helper) = child_start_env(|cmd| {
        cmd.env("PATH", "/contaminated/bin:/usr/bin:/bin")
            .env("TMPDIR", "/tmp/contaminated")
            .env("LANG", "xx_XX.UTF-8")
            .env("LEAKY_SETTING", "from-the-caller")
            .env("PAGER_TOKEN", "a-different-token");
    });
    assert!(!helper["PATH"].contains("contaminated"), "{helper:?}");
    assert!(!helper.contains_key("LEAKY_SETTING"));
    assert!(
        !helper
            .values()
            .any(|v| v.contains("contaminated") || v == "a-different-token")
    );
}

#[test]
fn child_start_ignores_a_scrubbed_caller_env() {
    let (env, helper) = child_start_env(|cmd| {
        cmd.env_remove("PATH")
            .env_remove("HOME")
            .env_remove("LANG")
            .env_remove("PAGER_TOKEN");
    });
    assert!(helper["PATH"].contains(&env.path("bin").display().to_string()));
    // The plugin still has the values recorded at `up`.
    let shim = env.state_dir().join("members/helper/bin/brg");
    assert!(shim.exists());
}

#[test]
fn a_different_binary_is_refused_and_changes_nothing() {
    for bus in [false, true] {
        let env = Env::new(bus);
        env.up();
        let other = env.other_brg("brg-rebuilt");
        let before = env.snapshot();
        let out = env
            .cmd_with(&other, &["up", "--member", "helper"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{}", both(&out));
        assert!(
            both(&out).contains("not the binary the running deployment started with"),
            "{}",
            both(&out)
        );
        assert!(both(&out).contains("up --member helper"), "{}", both(&out));
        assert_unchanged(&before, &env.snapshot());
        assert!(env.starts("helper").is_empty());
    }
}

#[test]
fn a_changed_member_file_is_refused_and_changes_nothing() {
    let env = Env::new(true);
    env.up();
    env.write(
        "team/helper.md",
        "---\nrole: helper\n---\nYou are the helper, edited.\n",
    );
    let before = env.snapshot();
    let out = env.brg(&["up", "--member", "helper"]);
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert!(
        both(&out).contains("members/helper/prompt"),
        "{}",
        both(&out)
    );
    assert!(both(&out).contains("new run"), "{}", both(&out));
    assert_unchanged(&before, &env.snapshot());
    assert!(env.starts("helper").is_empty());
}

#[test]
fn concurrent_child_starts_start_once() {
    let env = Env::new(true);
    env.up();
    let children: Vec<_> = (0..2)
        .map(|_| {
            env.brg_cmd(&["up", "--member", "helper"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let outs: Vec<Output> = children
        .into_iter()
        .map(|c| c.wait_with_output().unwrap())
        .collect();
    for out in &outs {
        assert!(out.status.success(), "{}", both(out));
    }
    let text: String = outs.iter().map(both).collect();
    assert_eq!(text.matches("helper: started").count(), 1, "{text}");
    assert_eq!(text.matches("helper: already running").count(), 1, "{text}");
    env.wait_for("helper start", |e| !e.starts("helper").is_empty());
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(env.starts("helper").len(), 1);
}

#[test]
fn members_run_the_pinned_binary() {
    let env = Env::new(true);
    env.up();
    let built = Path::new(env!("CARGO_BIN_EXE_brg"));
    let digest = sha(built);
    let pinned = env
        .run_dir("run-1")
        .join(format!("runtime/bin/{digest}/brg"));
    assert_eq!(sha(&pinned), digest);
    assert_eq!(
        fs::metadata(&pinned).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let lead = &env.starts("lead")[0]["env"];
    assert_eq!(lead["BORGCHART_BIN"], pinned.display().to_string());
    let state = env.state_dir();
    for shim in ["brg", "borgchart"] {
        let text = fs::read_to_string(state.join("members/lead/bin").join(shim)).unwrap();
        assert!(
            text.contains(&format!("exec {} \"$@\"", pinned.display())),
            "{text}"
        );
    }
    let plugin_shim = fs::read_to_string(state.join("members/lead/bin/pager")).unwrap();
    assert!(
        plugin_shim.contains(&format!("exec {} _run", pinned.display())),
        "{plugin_shim}"
    );
    let started = env.tmux(&["list-panes", "-a", "-F", "#{pane_start_command}"]);
    assert!(
        started.contains(&format!("{} _exec", pinned.display())),
        "{started}"
    );
    assert_eq!(
        read_json(&state.join("deployment.json"))["brg"],
        pinned.display().to_string()
    );

    // Inside the member, `brg` is the shim, and it runs.
    for cmd in ["tool command -v brg", "tool brg --version"] {
        env.ok(&["send", "lead", cmd]);
    }
    env.wait_for("tool calls", |e| {
        e.events("lead")
            .iter()
            .filter(|x| x["event"] == "tool")
            .count()
            >= 2
    });
    let tools: Vec<Value> = env
        .events("lead")
        .into_iter()
        .filter(|x| x["event"] == "tool")
        .collect();
    assert_eq!(
        tools[0]["output"],
        state.join("members/lead/bin/brg").display().to_string()
    );
    assert!(
        tools[1]["output"].as_str().unwrap().starts_with("brg "),
        "{}",
        tools[1]
    );

    // Rebuilding the original changes nothing the deployment runs.
    let show = env.show();
    assert_eq!(show["pinned"], pinned.display().to_string());
    assert!(
        show["stop"]
            .as_str()
            .unwrap()
            .contains(&format!("{} --file", pinned.display()))
    );
}

#[test]
fn without_a_bus_members_run_the_starting_binary() {
    let env = Env::new(false);
    env.up();
    let lead = &env.starts("lead")[0]["env"];
    assert_eq!(lead["BORGCHART_BIN"], env!("CARGO_BIN_EXE_brg"));
    assert!(lead.get("BORGCHART_RUN").is_none());
    assert!(lead.get("BORGCHART_INCARNATION").is_none());
    let shim = fs::read_to_string(env.state_dir().join("members/lead/bin/brg")).unwrap();
    assert!(shim.contains(env!("CARGO_BIN_EXE_brg")), "{shim}");
    let out = env.brg(&["run", "show", "--json"]);
    assert_eq!(out.status.code(), Some(3), "{}", both(&out));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["outcome"], "not-configured");
}

#[test]
fn the_run_manifest_is_created_once_and_frozen() {
    let env = Env::new(true);
    env.up();
    let manifest = env.run_dir("run-1").join("run.json");
    let first = fs::read(&manifest).unwrap();
    let m = read_json(&manifest);
    assert_eq!(m["run"], "run-1");
    assert_eq!(m["schema"], 1);
    assert_eq!(m["members"]["helper"]["parent"], "lead");
    assert!(m.get("linked_from").is_none());
    env.ok(&["down"]);
    env.up();
    assert_eq!(
        fs::read(&manifest).unwrap(),
        first,
        "run.json was rewritten"
    );
    env.ok(&["down"]);

    env.write(
        "team/lead.md",
        "---\nrole: lead\n---\nYou are the lead, edited.\n",
    );
    let before = env.snapshot();
    let out = env.brg(&["up"]);
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert!(both(&out).contains("start a new run"), "{}", both(&out));
    assert!(both(&out).contains("members/lead/prompt"), "{}", both(&out));
    assert_unchanged(&before, &env.snapshot());
    env.write("team/lead.md", "---\nrole: lead\n---\nYou are the lead.\n");

    // Another deployment can't use the same run.
    let team2 = env.path("team2");
    fs::create_dir_all(&team2).unwrap();
    for f in ["team.yaml", "lead.md", "helper.md", "borgchart.yaml"] {
        fs::copy(env.path("team").join(f), team2.join(f)).unwrap();
    }
    fs::create_dir_all(team2.join("plugins/pager")).unwrap();
    fs::copy(
        env.path("team/plugins/pager/pager"),
        team2.join("plugins/pager/pager"),
    )
    .unwrap();
    let before = fs::read(&manifest).unwrap();
    let out = env.brg_cmd(&["up"]).current_dir(&team2).output().unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert!(
        both(&out).contains("belongs to deployment"),
        "{}",
        both(&out)
    );
    assert_eq!(fs::read(&manifest).unwrap(), before);
}

#[test]
fn generations_and_incarnations_are_recorded() {
    let env = Env::new(true);
    env.up();
    let run = env.run_dir("run-1");
    let g = read_json(&run.join("runtime/generations/000001.json"));
    assert_eq!(g["n"], 1);
    assert_eq!(g["reason"], "start");
    assert_eq!(g["binary"], env!("CARGO_BIN_EXE_brg"));
    assert_eq!(g["socket"], env.socket.as_str());
    assert_eq!(g["state_dir"], env.state_dir().display().to_string());

    let inc = read_json(&run.join("runtime/incarnations/lead.json"));
    let id = inc["incarnation"].as_str().unwrap().to_string();
    assert!(id.starts_with("lead.1."), "{id}");
    let lead_env = &env.starts("lead")[0]["env"];
    assert_eq!(lead_env["BORGCHART_INCARNATION"], id.as_str());
    assert_eq!(lead_env["BORGCHART_RUN"], "run-1");
    assert_eq!(lead_env["BORGCHART_MEMBER"], "lead");
    assert_eq!(inc["generation"], 1);
    assert_eq!(inc["vendor_version"], "fake-agent 0.0.0 (claude)");
    let pane = &inc["pane"];
    assert!(pane["pane_id"].as_str().unwrap().starts_with('%'), "{inc}");
    assert!(pane["pane_pid"].as_u64().unwrap() > 0);
    assert_eq!(pane["window"], "lead");
    assert_eq!(pane["socket"], env.socket.as_str());
    let listed = env.tmux(&[
        "list-panes",
        "-a",
        "-F",
        "#{pane_id} #{pane_pid} #{window_name}",
    ]);
    assert!(
        listed.contains(&format!(
            "{} {} lead",
            pane["pane_id"].as_str().unwrap(),
            pane["pane_pid"]
        )),
        "{listed}"
    );
    assert!(inc.get("ended_at").is_none());
    assert_eq!(
        read_json(&run.join(format!("runtime/incarnations/lead/{id}.json"))),
        inc
    );

    env.up_helper();
    let helper = read_json(&run.join("runtime/incarnations/helper.json"));
    assert_eq!(helper["vendor_version"], "fake-agent 0.0.0 (codex)");
    env.ok(&["down", "--member", "helper"]);
    let helper = read_json(&run.join("runtime/incarnations/helper.json"));
    assert_eq!(helper["ended_reason"], "stopped");
    assert!(
        read_json(&run.join("runtime/incarnations/lead.json"))
            .get("ended_at")
            .is_none()
    );

    env.ok(&["down"]);
    let ended = read_json(&run.join("runtime/incarnations/lead.json"));
    assert_eq!(ended["incarnation"], id.as_str());
    assert_eq!(ended["ended_reason"], "stopped");
    assert!(ended["ended_at"].as_str().unwrap().ends_with('Z'));

    // A new start is a new incarnation; the old one stays in the history.
    env.up();
    let again = read_json(&run.join("runtime/incarnations/lead.json"));
    assert_ne!(again["incarnation"], id.as_str());
    assert!(
        run.join(format!("runtime/incarnations/lead/{id}.json"))
            .exists()
    );
}

#[test]
fn liveness_is_alive_gone_or_unknown() {
    let env = Env::new(true);
    env.up();
    let live = |show: &Value, member: &str| -> String {
        show["members"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["member"] == member)
            .unwrap()["liveness"]
            .as_str()
            .unwrap_or("none")
            .to_string()
    };
    let show = env.show();
    assert_eq!(live(&show, "lead"), "alive");
    assert_eq!(live(&show, "helper"), "none");
    let text = String::from_utf8_lossy(&env.ok(&["run", "show"]).stdout).to_string();
    assert!(text.contains("lead  lead.1."), "{text}");
    assert!(text.contains("alive"), "{text}");
    assert!(text.contains("stop with: "), "{text}");

    // tmux can't be asked: unknown.
    let out = env
        .brg_cmd(&["run", "show", "--json"])
        .env("PATH", env.path("bin"))
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(live(&v, "lead"), "unknown", "{}", both(&out));

    // A start that never recorded its pane: unknown.
    let run = env.run_dir("run-1");
    fs::write(
        run.join("runtime/incarnations/helper.json"),
        r#"{"member":"helper","incarnation":"helper.1.20261105000000000.abcd","generation":1,"started_at":"2026-11-05T00:00:00.000Z"}"#,
    )
    .unwrap();
    assert_eq!(live(&env.show(), "helper"), "unknown");

    // The pane went away outside brg: gone.
    env.tmux(&["kill-window", "-t", "lead"]);
    assert_eq!(live(&env.show(), "lead"), "gone");

    // Ended through brg: gone.
    env.up();
    assert_eq!(live(&env.show(), "lead"), "alive");
    env.ok(&["down"]);
    assert_eq!(live(&env.show(), "lead"), "gone");
}

#[test]
fn rebind_is_refused_while_running_or_busy() {
    let env = Env::new(true);
    let out = env.brg(&["run", "rebind", "--reason", "early", "--json"]);
    assert_eq!(out.status.code(), Some(3), "{}", both(&out));

    env.up();
    let out = env.brg(&["run", "rebind", "--reason", "test", "--json"]);
    assert_eq!(out.status.code(), Some(4), "{}", both(&out));
    assert!(both(&out).contains("running"), "{}", both(&out));
    env.ok(&["down"]);

    let run = env.run_dir("run-1");
    let inflight = run.join("attention/inflight/lead.json");
    fs::create_dir_all(inflight.parent().unwrap()).unwrap();
    fs::write(&inflight, "{}").unwrap();
    let out = env.brg(&["run", "rebind", "--reason", "test"]);
    assert_eq!(out.status.code(), Some(4), "{}", both(&out));
    assert!(both(&out).contains("in flight"), "{}", both(&out));
    fs::remove_file(&inflight).unwrap();

    let held = fs::File::open(run.join("runtime/generation.lock")).unwrap();
    held.lock_shared().unwrap();
    let out = env.brg(&["run", "rebind", "--reason", "test"]);
    assert_eq!(out.status.code(), Some(4), "{}", both(&out));
    assert!(both(&out).contains("generation.lock"), "{}", both(&out));
    drop(held);

    let out = env.brg(&["run", "rebind", "--reason", "test", "--json"]);
    assert!(out.status.success(), "{}", both(&out));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["outcome"], "unchanged");
    assert!(!run.join("runtime/generations/000002.json").exists());

    let out = env.brg(&["run", "rebind", "--reason", ""]);
    assert_eq!(out.status.code(), Some(2), "{}", both(&out));
}

#[test]
fn upgrade_rebind_and_rollback_keep_records_and_pins() {
    let env = Env::new(true);
    env.up();
    env.ok(&["down"]);
    let run = env.run_dir("run-1");
    // Records other pieces own; a rebind must leave them alone.
    let fixtures = [
        ("bus/records/0000000001.json", r#"{"fixture":"record one"}"#),
        ("bus/acks/m1.json", r#"{"fixture":"ack"}"#),
        ("attention/alerts/a1.json", r#"{"fixture":"alert"}"#),
    ];
    for (rel, text) in fixtures {
        let p = run.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, text).unwrap();
    }
    let original = Path::new(env!("CARGO_BIN_EXE_brg")).to_path_buf();
    let original_pin = run.join(format!("runtime/bin/{}/brg", sha(&original)));
    let upgraded = env.other_brg("brg-upgraded");
    let upgraded_pin = run.join(format!("runtime/bin/{}/brg", sha(&upgraded)));

    // The upgraded binary can't start the run until it is rebound.
    let out = env.cmd_with(&upgraded, &["up"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert!(both(&out).contains("rebind needed"), "{}", both(&out));
    assert_eq!(env.tmux(&["list-sessions"]), "");

    let out = env
        .cmd_with(
            &upgraded,
            &["run", "rebind", "--reason", "upgrade", "--json"],
        )
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["outcome"], "rebound");
    let g2 = read_json(&run.join("runtime/generations/000002.json"));
    assert_eq!(g2["previous"], 1);
    assert_eq!(g2["reason"], "upgrade");
    assert_eq!(g2["pinned"], upgraded_pin.display().to_string());
    assert_eq!(sha(&upgraded_pin), sha(&upgraded));

    // Now the old binary is the one that needs a rebind; the new one starts.
    let out = env.brg(&["up"]);
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert!(both(&out).contains("rebind needed"), "{}", both(&out));
    let out = env.cmd_with(&upgraded, &["up"]).output().unwrap();
    assert!(out.status.success(), "{}", both(&out));
    env.wait_for("lead start", |e| e.starts("lead").len() >= 2);
    let lead = &env.starts("lead")[1]["env"];
    assert_eq!(lead["BORGCHART_BIN"], upgraded_pin.display().to_string());
    assert!(
        lead["BORGCHART_INCARNATION"]
            .as_str()
            .unwrap()
            .starts_with("lead.2.")
    );
    let out = env.cmd_with(&upgraded, &["down"]).output().unwrap();
    assert!(out.status.success(), "{}", both(&out));

    // Roll back: rebind with the original binary.
    env.ok(&["run", "rebind", "--reason", "rollback"]);
    let g3 = read_json(&run.join("runtime/generations/000003.json"));
    assert_eq!(g3["previous"], 2);
    assert_eq!(g3["pinned"], original_pin.display().to_string());
    env.up();
    let lead = &env.starts("lead")[2]["env"];
    assert_eq!(lead["BORGCHART_BIN"], original_pin.display().to_string());
    env.ok(&["down"]);

    for (rel, text) in fixtures {
        assert_eq!(fs::read_to_string(run.join(rel)).unwrap(), text, "{rel}");
    }
    assert_eq!(sha(&original_pin), sha(&original));
    assert_eq!(sha(&upgraded_pin), sha(&upgraded));
    let gens = env.show()["generations"].as_array().unwrap().len();
    assert_eq!(gens, 3);
    // The original install is untouched.
    assert_eq!(
        fs::read(&original).unwrap(),
        fs::read(&original_pin).unwrap()
    );
}

#[test]
fn a_new_run_can_continue_an_earlier_one() {
    let env = Env::new(true);
    env.up();
    env.ok(&["down"]);

    env.write_deploy(Some("run-2"), Some("run-0"));
    let out = env.brg(&["up"]);
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert!(both(&out).contains("continues: run-0"), "{}", both(&out));
    assert!(!env.run_dir("run-2").exists());

    env.write_deploy(Some("run-2"), Some("run-1"));
    env.up();
    let m = read_json(&env.run_dir("run-2").join("run.json"));
    assert_eq!(m["linked_from"]["run"], "run-1");
    assert_eq!(m["linked_from"]["reason"], "continues");
    assert_eq!(
        m["linked_from"]["root"],
        env.path("runs").display().to_string()
    );
    assert!(
        env.run_dir("run-2")
            .join("runtime/generations/000001.json")
            .exists()
    );
    // The earlier run is left as it was.
    assert!(
        read_json(&env.run_dir("run-1").join("runtime/incarnations/lead.json"))
            .get("ended_at")
            .is_some()
    );
}
