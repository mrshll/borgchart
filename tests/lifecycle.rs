//! Lifecycle: validation before mutation, child starts into a running team,
//! the team tree `up` and `status` print, the team's message storage at
//! `up` (reuse and rollover), incarnations, liveness and `down`. Real tmux
//! on a private per-test socket, with `fake-agent` standing in for `claude`
//! and `codex`.
//!
//! Run with `cargo test --features test-support`.
#![cfg(feature = "test-support")]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use borgchart::bus::mentions_runs;
use serde_json::Value;

const SECRET: &str = "pager-token-not-real-7c2e";

struct Env {
    /// First, so its servers stop before the test's directory goes.
    tmux: common::PrivateTmux,
    dir: tempfile::TempDir,
    socket: String,
}

const TEAM: &str = r#"
plugins:
  pager:
    path: plugins/pager
    env: [PAGER_TOKEN, {PAGER_ROOM: R0000EXAMPLE}]
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

const LEAD: &str = "---\nrole: lead\nmodels:\n  claude: opus high\n---\nYou are the lead.\n";
const HELPER: &str =
    "---\nrole: helper\nmodels:\n  codex: example-model\n---\nYou are the helper.\n";

impl Env {
    /// A two-member team in a folder called `oncall`, its instance working
    /// in `work`; with `bus`, the team has `bus: {}`.
    fn new(bus: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let tmux = common::PrivateTmux::new();
        let socket = tmux.socket("brg-lc");
        let env = Env { tmux, dir, socket };
        let bin = env.path("bin");
        fs::create_dir_all(&bin).unwrap();
        let fake = env!("CARGO_BIN_EXE_fake-agent");
        std::os::unix::fs::symlink(fake, bin.join("claude")).unwrap();
        std::os::unix::fs::symlink(fake, bin.join("codex")).unwrap();
        fs::create_dir_all(env.path("home")).unwrap();
        env.write("oncall/team.yaml", TEAM);
        env.write("oncall/lead.md", LEAD);
        env.write("oncall/helper.md", HELPER);
        env.script(
            "oncall/plugins/pager/pager",
            "#!/bin/sh\necho \"pager $* member=$BORGCHART_MEMBER token_len=${#PAGER_TOKEN}\" >> \"$BORGCHART_STATE_DIR/calls.log\"\necho ok\n",
        );
        if bus {
            env.write("oncall/team.yaml", &format!("bus: {{}}\n{TEAM}"));
        }
        fs::create_dir_all(env.path("work")).unwrap();
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

    fn script(&self, rel: &str, text: &str) {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        common::write_executable(&p, text);
    }

    fn base_path(&self) -> String {
        self.tmux.path_with(&format!(
            "{}:/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin",
            self.path("bin").display()
        ))
    }

    /// A minimal caller PATH: the tmux guard and the system directories.
    fn minimal_path(&self, extra_first: &str) -> String {
        format!("{extra_first}{}", self.tmux.path_with("/usr/bin:/bin"))
    }

    /// `brg` (or another binary) with the usual test environment, run in
    /// the team's folder.
    fn cmd_with(&self, program: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new(program);
        cmd.env_clear()
            .env("PATH", self.base_path())
            .env("HOME", self.path("home"))
            .env("LANG", "C.UTF-8")
            .env("BORGCHART_STATE_HOME", self.path("state"))
            .env("BORGCHART_CONFIG", self.path("config.yaml"))
            .env("PAGER_TOKEN", SECRET)
            .current_dir(self.path("oncall"))
            .args(self.translate(args));
        self.tmux.brg_env(&mut cmd, &self.socket);
        cmd
    }

    /// The fixture's one instance, once created.
    fn instance(&self) -> Option<String> {
        let reg: Value =
            serde_json::from_slice(&fs::read(self.path("state/registry.json")).ok()?).ok()?;
        reg["instances"].as_object()?.keys().next().cloned()
    }

    /// The member of `role` in the instance, once created.
    fn member_id(&self, role: &str) -> Option<String> {
        let inst = self.instance()?;
        let rec: Value = serde_json::from_slice(
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

    /// The instance (it must exist).
    fn inst(&self) -> String {
        self.instance().expect("an instance")
    }

    /// Printed trees with the instance line as `oncall`, and member IDs as
    /// their roles padded to the same width, so they compare exactly.
    fn shown(&self, text: &str) -> String {
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        if let Some(first) = lines.first_mut() {
            *first = "oncall".into();
        }
        let mut out = lines.join("\n") + "\n";
        for role in ["lead", "helper"] {
            if let Some(id) = self.member_id(role) {
                out = out.replace(&id, &format!("{role:<width$}", width = id.len()));
            }
        }
        out
    }

    /// A member's incarnation record, by role.
    fn incarnation_file(&self, role: &str) -> PathBuf {
        self.run_dir()
            .join(format!("runtime/incarnations/{}.json", self.m(role)))
    }

    /// The tests act on the team's one instance, and name members by role
    /// (each role has one member): `start` creates the instance the first
    /// time and starts it after, `start <role>` creates or starts its
    /// member, `stop [<role>]`, `status`, `logs`, `attach` and `send` go to
    /// the instance.
    fn translate(&self, args: &[&str]) -> Vec<String> {
        let inst = self.instance();
        let at = |role: &str| match (&inst, self.member_id(role)) {
            (Some(i), Some(m)) => format!("{i}/{m}"),
            (Some(i), None) => format!("{i}/{role}"),
            (None, _) => role.to_string(),
        };
        let own = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        match args {
            ["start", rest @ ..] if rest.is_empty() || rest[0].starts_with('-') => match &inst {
                None => {
                    let mut v = vec![
                        "create".to_string(),
                        self.path("oncall").display().to_string(),
                        self.path("work").display().to_string(),
                    ];
                    v.extend(own(rest));
                    v
                }
                Some(i) => {
                    let mut v = vec!["start".to_string(), i.clone()];
                    v.extend(own(rest));
                    v
                }
            },
            ["start", role, rest @ ..] => {
                let mut v = match (&inst, self.member_id(role)) {
                    (Some(i), None) => vec!["create".to_string(), format!("{i}/{role}")],
                    _ => vec!["start".to_string(), at(role)],
                };
                v.extend(own(rest));
                v
            }
            ["stop", rest @ ..] if rest.is_empty() || rest[0].starts_with('-') => {
                let mut v = vec!["stop".to_string(), inst.clone().unwrap_or_default()];
                v.extend(own(rest));
                v
            }
            ["stop", role, rest @ ..] => {
                let mut v = vec!["stop".to_string(), at(role)];
                v.extend(own(rest));
                v
            }
            ["status", rest @ ..] => {
                let mut v = vec!["status".to_string()];
                v.extend(inst.clone());
                v.extend(own(rest));
                v
            }
            [cmd @ ("logs" | "attach" | "send"), role, rest @ ..] => {
                let mut v = vec![cmd.to_string(), at(role)];
                v.extend(own(rest));
                v
            }
            _ => own(args),
        }
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
        let out = self.tmux.tmux(&self.socket).args(args).output().unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn events(&self, role: &str) -> Vec<Value> {
        let member = self.member_id(role).unwrap_or_else(|| role.to_string());
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

    fn rows(&self) -> Vec<Value> {
        let out = self.ok(&["status", "--json"]);
        serde_json::from_slice(&out.stdout).unwrap()
    }

    /// A role's member's row (or the role's, with no member), or a plugin's.
    fn row(&self, name: &str) -> Value {
        let rows = self.rows();
        rows.iter()
            .find(|r| r["role"] == name)
            .or_else(|| rows.iter().find(|r| r["name"] == name))
            .cloned()
            .unwrap_or(Value::Null)
    }

    fn state_of(&self, name: &str) -> String {
        self.row(name)["state"].as_str().unwrap_or("").to_string()
    }

    fn up(&self) -> Output {
        let out = self.ok(&["start"]);
        self.wait_for("lead idle", |e| e.state_of("lead") == "idle");
        out
    }

    fn up_helper(&self) {
        self.ok(&["start", "helper"]);
        self.wait_for("helper idle", |e| e.state_of("helper") == "idle");
    }

    /// The instance's message storage.
    fn store(&self) -> PathBuf {
        self.state_dir().join("messages")
    }

    /// The run (internal) the team's storage uses now.
    fn current(&self) -> String {
        fs::read_to_string(self.store().join("current"))
            .unwrap()
            .trim()
            .to_string()
    }

    fn run_dir(&self) -> PathBuf {
        self.store().join(self.current())
    }

    fn state_dir(&self) -> PathBuf {
        self.path(&format!("state/{}", self.instance().expect("an instance")))
    }

    /// Every file under the state home, with its bytes.
    fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut out = BTreeMap::new();
        for root in [self.path("state")] {
            for p in walk(&root) {
                out.insert(p.clone(), fs::read(&p).unwrap_or_default());
            }
        }
        out
    }

    /// A copy of the built brg with extra bytes, so it is another build.
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

/// The last line of `bytes` (after any validation warnings), with its newline.
fn last_line(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    format!("{}\n", text.lines().last().unwrap_or(""))
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// The last column of `member`'s line in a printed tree: its outcome.
fn outcomes(text: &str, member: &str) -> Vec<String> {
    text.lines()
        .filter(|l| {
            l.trim_start_matches([' ', '│', '├', '└', '─'])
                .split_whitespace()
                .next()
                == Some(member)
        })
        .filter_map(|l| {
            let cols: Vec<&str> = l.split("  ").filter(|c| !c.trim().is_empty()).collect();
            (cols.len() > 1).then(|| cols.last().unwrap().trim().to_string())
        })
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
            .brg_cmd(&["start"])
            .env_remove("PAGER_TOKEN")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{}", both(&out));
        assert!(both(&out).contains("pager: PAGER_TOKEN"), "{}", both(&out));
        assert!(
            env.snapshot().is_empty() && !env.path("state").exists(),
            "a refused first start wrote {:?}",
            env.snapshot().keys().collect::<Vec<_>>()
        );

        // A later fresh start, refused the same way, changes nothing either.
        env.up();
        env.ok(&["stop"]);
        let before = env.snapshot();
        let out = env
            .brg_cmd(&["start"])
            .env_remove("PAGER_TOKEN")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{}", both(&out));
        assert_unchanged(&before, &env.snapshot());
        assert_eq!(env.tmux(&["list-sessions"]), "");
    }
}

#[test]
fn up_prints_the_team_as_a_tree() {
    let env = Env::new(true);
    let out = env.up();
    let header = stdout(&out).lines().next().unwrap().to_string();
    assert_eq!(
        header,
        format!(
            "{}  team {} · folder {}",
            env.inst(),
            env.path("oncall").canonicalize().unwrap().display(),
            env.path("work").canonicalize().unwrap().display()
        )
    );
    assert_eq!(
        env.shown(&stdout(&out)),
        "oncall\n\
         └─ lead      claude opus high          started\n   \
         └─ helper on demand (none created)\n"
    );
    let again = env.ok(&["start"]);
    assert_eq!(
        outcomes(&stdout(&again), &env.m("lead")),
        ["already running"]
    );
    let child = env.ok(&["start", "helper"]);
    assert_eq!(
        env.shown(&stdout(&child)),
        "oncall\n\
         └─ lead           claude opus high\n   \
         └─ helper      codex example-model  started\n"
    );
    for out in [&out, &again, &child] {
        assert!(!mentions_runs(&both(out)), "{}", both(out));
    }
}

#[test]
fn status_prints_the_same_tree_with_live_states() {
    let env = Env::new(true);
    env.up();
    let out = env.ok(&["status"]);
    assert_eq!(
        env.shown(&stdout(&out)),
        "oncall\n\
         └─ lead      claude opus high          idle\n   \
         └─ helper on demand (none created)\n"
    );
    let rows = env.rows();
    let names: Vec<&str> = rows.iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert_eq!(names, [env.m("lead").as_str(), "helper"], "tree order");
    assert_eq!(rows[0]["kind"], "member");
    assert_eq!(rows[0]["member"], env.m("lead"));
    assert_eq!(rows[0]["role"], "lead");
    assert_eq!(rows[0]["instance"], env.inst());
    assert_eq!(rows[0]["team"], "oncall");
    assert_eq!(rows[1]["kind"], "role");
    assert_eq!(rows[1]["parent"], env.m("lead"));
    assert_eq!(rows[0]["model"], "opus high");
    assert_eq!(rows[0]["liveness"], "alive");
    assert!(rows[1].get("liveness").is_none(), "{}", rows[1]);
    assert_eq!(rows[1]["state"], "on demand (none created)");
    assert!(rows[1].get("expected_running").is_none(), "{}", rows[1]);
}

#[test]
fn status_without_instances_says_so() {
    let env = Env::new(true);
    let out = env.ok(&["status"]);
    assert!(
        stdout(&out).contains("there are no instances"),
        "{}",
        both(&out)
    );
    let rows: Vec<Value> = serde_json::from_slice(&env.ok(&["status", "--json"]).stdout).unwrap();
    assert!(rows.is_empty());
}

#[test]
fn members_that_ran_and_ended_are_stopped() {
    for bus in [true, false] {
        let env = Env::new(bus);
        env.up();
        env.up_helper();
        env.ok(&["stop", "helper"]);
        let out = env.ok(&["status"]);
        assert_eq!(
            env.shown(&stdout(&out)),
            "oncall\n\
             └─ lead           claude opus high     idle\n   \
             └─ helper      codex example-model  stopped\n",
            "bus: {bus}"
        );
        let helper = env.row("helper");
        assert_eq!(helper["state"], "stopped");
        assert!(helper.get("expected_running").is_none(), "{helper}");

        // While the team runs, a `start: true` member that ended should be
        // running: it stands out.
        env.up_helper();
        env.ok(&["stop", "lead"]);
        let out = env.ok(&["status"]);
        assert_eq!(
            env.shown(&stdout(&out)),
            "oncall\n\
             └─ lead           claude opus high     stopped (should be running)\n   \
             └─ helper      codex example-model  idle\n",
            "bus: {bus}"
        );
        let lead = env.row("lead");
        assert_eq!(lead["state"], "stopped");
        assert_eq!(lead["expected_running"], true);
        assert!(!mentions_runs(&both(&out)), "{}", both(&out));

        // A team stopped on purpose: ended members are plain `stopped`.
        env.ok(&["stop"]);
        let out = env.ok(&["status"]);
        assert_eq!(
            env.shown(&stdout(&out)),
            "oncall\n\
             └─ lead           claude opus high     stopped\n   \
             └─ helper      codex example-model  stopped\n",
            "bus: {bus}"
        );
        let lead = env.row("lead");
        assert_eq!(lead["state"], "stopped");
        assert!(lead.get("expected_running").is_none(), "{lead}");
    }
}

#[test]
fn state_from_an_earlier_brg_reads_as_stopped_not_never_started() {
    let env = Env::new(false);
    env.up();
    env.up_helper();
    env.ok(&["stop", "helper"]);
    let meta_file = env.state_dir().join("start.json");
    let mut meta = read_json(&meta_file);
    // New state records every member's start history explicitly.
    assert_eq!(meta["members"][env.m("lead")]["started"], true);
    assert_eq!(meta["members"][env.m("helper")]["role"], "helper");

    // An earlier brg wrote no `started` field: history unknown.
    for m in meta["members"].as_object_mut().unwrap().values_mut() {
        m.as_object_mut().unwrap().remove("started");
    }
    fs::write(&meta_file, serde_json::to_vec_pretty(&meta).unwrap()).unwrap();
    let helper = env.row("helper");
    assert_eq!(helper["state"], "stopped", "{helper}");
    assert!(helper.get("expected_running").is_none(), "{helper}");

    env.up_helper();
    env.ok(&["stop", "lead"]);
    let out = env.ok(&["status"]);
    assert_eq!(
        env.shown(&stdout(&out)),
        "oncall\n\
         └─ lead           claude opus high     stopped (should be running)\n   \
         └─ helper      codex example-model  idle\n"
    );
    assert_eq!(env.row("lead")["expected_running"], true);
}

/// Start `helper` into the running team from a caller environment built by
/// `caller`, and return helper's environment.
fn child_start_env(caller: impl Fn(&Env, &mut Command)) -> (Env, BTreeMap<String, String>) {
    let env = Env::new(true);
    env.up();
    let lead_env = env.starts("lead")[0]["env"].clone();
    let state = env.state_dir();
    let meta_before = fs::read(state.join("start.json")).unwrap();
    let secrets_before = fs::read(state.join("secrets/pager.json")).unwrap();
    let lead_shim_before =
        fs::read(state.join(format!("members/{}/bin/pager", env.m("lead")))).unwrap();

    let mut cmd = env.brg_cmd(&["start", "helper"]);
    caller(&env, &mut cmd);
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{}", both(&out));
    assert_eq!(
        outcomes(&stdout(&out), &env.m("helper")),
        ["started"],
        "{}",
        both(&out)
    );
    env.wait_for("helper start", |e| !e.starts("helper").is_empty());

    // The new member joins the start record; nothing else changes.
    let mut meta_now = read_json(&state.join("start.json"));
    meta_now["members"]
        .as_object_mut()
        .unwrap()
        .remove(&env.m("helper"));
    assert_eq!(
        meta_now,
        serde_json::from_slice::<Value>(&meta_before).unwrap()
    );
    assert_eq!(
        fs::read(state.join("secrets/pager.json")).unwrap(),
        secrets_before
    );
    assert_eq!(
        fs::read(state.join(format!("members/{}/bin/pager", env.m("lead")))).unwrap(),
        lead_shim_before
    );
    let helper: BTreeMap<String, String> =
        serde_json::from_value(env.starts("helper")[0]["env"].clone()).unwrap();
    let shims = state
        .join(format!("members/{}/bin", env.m("helper")))
        .display()
        .to_string();
    let lead_path = lead_env["PATH"].as_str().unwrap();
    let base_path = lead_path.split_once(':').unwrap().1;
    assert_eq!(helper["PATH"], format!("{shims}:{base_path}"));
    assert_eq!(helper["HOME"], lead_env["HOME"].as_str().unwrap());
    assert_eq!(helper["LANG"], "C.UTF-8");
    assert_eq!(
        helper["TMPDIR"],
        state
            .join(format!("tmp/{}", env.m("helper")))
            .display()
            .to_string()
    );
    assert_eq!(helper["BORGCHART_BIN"], env!("CARGO_BIN_EXE_brg"));
    assert!(!helper.contains_key("PAGER_TOKEN"));
    (env, helper)
}

#[test]
fn child_start_ignores_a_contaminated_caller_env() {
    let (_env, helper) = child_start_env(|env, cmd| {
        cmd.env("PATH", env.minimal_path("/contaminated/bin:"))
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
    let (env, helper) = child_start_env(|env, cmd| {
        // Brg needs tmux on the caller's PATH; nothing else.
        cmd.env("PATH", env.minimal_path(""))
            .env_remove("HOME")
            .env_remove("LANG")
            .env_remove("PAGER_TOKEN");
    });
    assert!(helper["PATH"].contains(&env.path("bin").display().to_string()));
}

#[test]
fn members_run_the_brg_that_started_them() {
    for bus in [false, true] {
        let env = Env::new(bus);
        env.up();
        let built = env!("CARGO_BIN_EXE_brg");
        let lead = &env.starts("lead")[0]["env"];
        // Members find the brg that started them as `$BORGCHART_BIN`.
        assert_eq!(lead["BORGCHART_BIN"], built, "{lead}");
        assert_eq!(lead.get("BORGCHART_RUN").is_some(), bus);
        assert_eq!(lead.get("BORGCHART_INCARNATION").is_some(), bus);
        let state = env.state_dir();
        for name in ["brg", "borgchart"] {
            assert!(
                !state
                    .join(format!("members/{}/bin", env.m("lead")))
                    .join(name)
                    .exists()
            );
        }
        let plugin_shim =
            fs::read_to_string(state.join(format!("members/{}/bin/pager", env.m("lead")))).unwrap();
        assert!(
            plugin_shim.contains(&format!("exec {built} _run")),
            "{plugin_shim}"
        );
        let started = env.tmux(&["list-panes", "-a", "-F", "#{pane_start_command}"]);
        assert!(started.contains(&format!("{built} _exec")), "{started}");
        assert_eq!(read_json(&state.join("start.json"))["brg"], built);
        if bus {
            assert!(!env.run_dir().join("runtime/bin").exists());
        }

        // Another build starts a member into the running team with itself.
        let other = env.other_brg("brg-rebuilt");
        let out = env.cmd_with(&other, &["start", "helper"]).output().unwrap();
        assert!(out.status.success(), "{}", both(&out));
        assert_eq!(outcomes(&stdout(&out), &env.m("helper")), ["started"]);
        let started = env.tmux(&["list-panes", "-a", "-F", "#{pane_start_command}"]);
        assert!(
            started.contains(&format!("{} _exec", other.display())),
            "{started}"
        );
        env.wait_for("helper start", |e| !e.starts("helper").is_empty());
        let helper = &env.starts("helper")[0]["env"];
        assert_eq!(
            helper["BORGCHART_BIN"],
            other.display().to_string(),
            "{helper}"
        );
        // `brg run …` is gone.
        assert_eq!(env.brg(&["run", "show"]).status.code(), Some(2));
    }
}

#[test]
fn a_changed_member_file_refuses_a_child_start() {
    let env = Env::new(true);
    env.up();
    env.up_helper();
    env.ok(&["stop", "helper"]);
    env.write(
        "oncall/helper.md",
        &HELPER.replace("You are the helper.", "You are the helper, edited."),
    );
    let inst = env.inst();
    let refusal = format!(
        "brg: {inst}/helper's role file changed since the instance started; stop it fully and start it again to apply it: brg stop {inst} && brg start {inst}\n"
    );
    let before = env.snapshot();
    let out = env.brg(&["start", "helper"]);
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert_eq!(last_line(&out.stderr), refusal);
    assert_unchanged(&before, &env.snapshot());
    assert_eq!(env.starts("helper").len(), 1);

    // A full start of the running instance refuses the same way.
    let out = env.brg(&["start"]);
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert_eq!(last_line(&out.stderr), refusal);
    assert_unchanged(&before, &env.snapshot());

    // A stopped instance's member start never rolls over: it refuses too.
    env.ok(&["stop"]);
    let before = env.snapshot();
    let out = env.brg(&["start", "helper"]);
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert_eq!(last_line(&out.stderr), refusal);
    assert_unchanged(&before, &env.snapshot());
    assert!(!mentions_runs(&both(&out)));
}

#[test]
fn concurrent_member_creates_create_once() {
    let env = Env::new(true);
    env.up();
    let children: Vec<_> = (0..2)
        .map(|_| {
            env.brg_cmd(&["start", "helper"])
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
    let won: Vec<&Output> = outs.iter().filter(|o| o.status.success()).collect();
    assert_eq!(
        won.len(),
        1,
        "{:?}",
        outs.iter().map(both).collect::<Vec<_>>()
    );
    assert_eq!(outcomes(&stdout(won[0]), &env.m("helper")), ["started"]);
    let lost = outs.iter().find(|o| !o.status.success()).unwrap();
    assert!(
        both(lost).contains("already has its member"),
        "{}",
        both(lost)
    );
    env.wait_for("helper start", |e| !e.starts("helper").is_empty());
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(env.starts("helper").len(), 1);
}

#[test]
fn storage_is_reused_until_the_configuration_changes() {
    let env = Env::new(true);
    env.up();
    assert_eq!(env.current(), "r1");
    let manifest = env.run_dir().join("run.json");
    let first = fs::read(&manifest).unwrap();
    let m = read_json(&manifest);
    assert_eq!(m["run"], "r1");
    assert_eq!(m["schema"], 1);
    assert_eq!(m["roles"]["helper"]["parent"], "lead");
    assert_eq!(m["instance"], env.inst());
    assert!(m.get("linked_from").is_none());
    assert_eq!(env.starts("lead")[0]["env"]["BORGCHART_RUN"], "r1");

    // Unchanged: the same storage.
    env.ok(&["stop"]);
    env.up();
    assert_eq!(env.current(), "r1");
    assert_eq!(fs::read(&manifest).unwrap(), first);
    env.ok(&["stop"]);

    // Changed: new storage, linked to the old, which is left as it was.
    let r1_files: BTreeMap<PathBuf, Vec<u8>> = walk(&env.run_dir())
        .into_iter()
        .map(|p| (p.clone(), fs::read(&p).unwrap()))
        .collect();
    env.write(
        "oncall/lead.md",
        &LEAD.replace("You are the lead.", "You are the lead, edited."),
    );
    let out = env.up();
    assert_eq!(outcomes(&stdout(&out), &env.m("lead")), ["started"]);
    assert!(!mentions_runs(&both(&out)), "{}", both(&out));
    assert_eq!(env.current(), "r2");
    let m2 = read_json(&env.run_dir().join("run.json"));
    assert_eq!(m2["linked_from"]["run"], "r1");
    assert_eq!(m2["linked_from"]["reason"], "config");
    assert_eq!(m2["linked_from"]["root"], env.store().display().to_string());
    assert_eq!(env.starts("lead")[2]["env"]["BORGCHART_RUN"], "r2");
    let r1 = env.store().join("r1");
    assert_eq!(fs::read_to_string(r1.join("retired")).unwrap(), "r2\n");
    for (p, bytes) in &r1_files {
        assert_eq!(&fs::read(p).unwrap(), bytes, "{}", p.display());
    }
    env.ok(&["stop"]);
}

#[test]
fn a_format_change_refuses_until_a_new_instance() {
    let env = Env::new(true);
    env.up();
    env.ok(&["stop"]);
    let run = env.run_dir();
    // As if a future brg had written this storage.
    let mut m = read_json(&run.join("run.json"));
    m["schema"] = 99.into();
    fs::write(run.join("run.json"), serde_json::to_vec(&m).unwrap()).unwrap();
    for n in 1..=2 {
        let p = run.join(format!("bus/records/{n:010}.json"));
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "{}").unwrap();
    }

    // A member start refuses, in instance terms.
    let inst = env.inst();
    let out = env.brg(&["start", "lead"]);
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert!(
        both(&out).contains(&format!(
            "instance `{inst}`'s messages use format 99; this brg uses format 1"
        )),
        "{}",
        both(&out)
    );
    assert!(!mentions_runs(&both(&out)), "{}", both(&out));
    assert_eq!(env.current(), "r1");

    // A full start would move to fresh messages, but brg can't read the old
    // ones to check for open work: it refuses and points to a new instance.
    let before = env.snapshot();
    let out = env.brg(&["start"]);
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert!(
        both(&out).contains("can't be read") && both(&out).contains("brg create"),
        "{}",
        both(&out)
    );
    assert!(!mentions_runs(&both(&out)), "{}", both(&out));
    assert_unchanged(&before, &env.snapshot());
    assert_eq!(env.current(), "r1");
}

#[test]
fn old_storage_rolls_over_at_its_age_limit() {
    let env = Env::new(true);
    env.up();
    env.ok(&["stop"]);
    let manifest = env.run_dir().join("run.json");
    let mut m = read_json(&manifest);
    m["created_at"] = "2000-01-01T00:00:00.000Z".into();
    fs::write(&manifest, serde_json::to_vec(&m).unwrap()).unwrap();
    // A child start never rolls over, whatever the age.
    env.up_helper();
    env.ok(&["stop"]);
    assert_eq!(env.current(), "r1");
    env.up();
    assert_eq!(env.current(), "r2");
    assert_eq!(
        read_json(&env.run_dir().join("run.json"))["linked_from"]["reason"],
        "age"
    );
}

#[test]
fn rollover_needs_every_member_positively_stopped() {
    let env = Env::new(true);
    env.up();
    env.up_helper();
    env.ok(&["stop"]);
    // A start that never recorded its pane: it can't be shown stopped.
    let inc = env.incarnation_file("helper");
    fs::write(&inc, unknown_helper(&env)).unwrap();
    env.write(
        "oncall/lead.md",
        &LEAD.replace("You are the lead.", "You are the lead, edited."),
    );
    let before = env.snapshot();
    let out = env.brg(&["start"]);
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert!(
        both(&out).contains(&format!(
            "brg can't tell that {}/{} has stopped",
            env.inst(),
            env.m("helper")
        )),
        "{}",
        both(&out)
    );
    assert!(!mentions_runs(&both(&out)), "{}", both(&out));
    assert_unchanged(&before, &env.snapshot());
    assert_eq!(env.tmux(&["list-sessions"]), "");
}

#[test]
fn a_lost_current_file_is_not_a_first_start() {
    let env = Env::new(true);
    env.up();
    env.ok(&["stop"]);
    fs::remove_file(env.store().join("current")).unwrap();
    let before = env.snapshot();
    let out = env.brg(&["start"]);
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert!(
        both(&out).contains(&format!(
            "instance `{}` was started before but its messages are missing",
            env.inst()
        )),
        "{}",
        both(&out)
    );
    assert_unchanged(&before, &env.snapshot());
}

#[test]
fn incarnations_are_recorded() {
    let env = Env::new(true);
    env.up();
    let run = env.run_dir();
    let inc = read_json(&env.incarnation_file("lead"));
    let id = inc["incarnation"].as_str().unwrap().to_string();
    let parts: Vec<&str> = id.split('.').collect();
    assert_eq!(parts.len(), 3, "{id}");
    assert_eq!(parts[0], env.m("lead"));
    let lead_env = &env.starts("lead")[0]["env"];
    assert_eq!(lead_env["BORGCHART_INCARNATION"], id.as_str());
    assert_eq!(lead_env["BORGCHART_MEMBER"], env.m("lead"));
    assert!(inc.get("generation").is_none(), "{inc}");
    assert_eq!(inc["vendor_version"], "fake-agent 0.0.0 (claude)");
    let pane = &inc["pane"];
    assert!(pane["pane_id"].as_str().unwrap().starts_with('%'), "{inc}");
    assert!(pane["pane_pid"].as_u64().unwrap() > 0);
    assert_eq!(pane["window"], env.m("lead"));
    let socket = pane["socket"].as_str().unwrap();
    assert!(
        socket.starts_with('/') && socket.ends_with(&format!("/{}", env.socket)),
        "{socket}"
    );
    let listed = env.tmux(&[
        "list-panes",
        "-a",
        "-F",
        "#{pane_id} #{pane_pid} #{window_name}",
    ]);
    assert!(
        listed.contains(&format!(
            "{} {} {}",
            pane["pane_id"].as_str().unwrap(),
            pane["pane_pid"],
            env.m("lead")
        )),
        "{listed}"
    );
    assert!(inc.get("ended_at").is_none());
    assert_eq!(
        read_json(&run.join(format!("runtime/incarnations/{}/{id}.json", env.m("lead")))),
        inc
    );

    env.up_helper();
    let helper = read_json(&env.incarnation_file("helper"));
    assert_eq!(helper["vendor_version"], "fake-agent 0.0.0 (codex)");
    env.ok(&["stop", "helper"]);
    let helper = read_json(&env.incarnation_file("helper"));
    assert_eq!(helper["ended_reason"], "stopped");
    assert!(
        read_json(&env.incarnation_file("lead"))
            .get("ended_at")
            .is_none()
    );

    env.ok(&["stop"]);
    let ended = read_json(&env.incarnation_file("lead"));
    assert_eq!(ended["incarnation"], id.as_str());
    assert_eq!(ended["ended_reason"], "stopped");
    assert!(ended["ended_at"].as_str().unwrap().ends_with('Z'));

    // A new start is a new incarnation; the old one stays in the history.
    env.up();
    let again = read_json(&env.incarnation_file("lead"));
    assert_ne!(again["incarnation"], id.as_str());
    assert!(
        run.join(format!("runtime/incarnations/{}/{id}.json", env.m("lead")))
            .exists()
    );
}

#[test]
fn liveness_is_alive_gone_or_unknown() {
    let env = Env::new(true);
    env.up();
    let live = |rows: &[Value], role: &str| -> String {
        rows.iter().find(|r| r["role"] == role).unwrap()["liveness"]
            .as_str()
            .unwrap_or("none")
            .to_string()
    };
    let json = |cmd: &mut Command| -> Vec<Value> {
        let out = cmd.output().unwrap();
        serde_json::from_slice(&out.stdout).unwrap_or_else(|_| panic!("{}", both(&out)))
    };
    assert_eq!(live(&env.rows(), "lead"), "alive");
    assert_eq!(live(&env.rows(), "helper"), "none");

    // tmux can't be asked: unknown.
    let rows = json(
        env.brg_cmd(&["status", "--json"])
            .env("PATH", env.path("bin")),
    );
    assert_eq!(live(&rows, "lead"), "unknown");

    // A start that never recorded its pane: unknown.
    env.up_helper();
    env.ok(&["stop", "helper"]);
    assert_eq!(live(&env.rows(), "helper"), "gone");
    fs::write(env.incarnation_file("helper"), unknown_helper(&env)).unwrap();
    assert_eq!(live(&env.rows(), "helper"), "unknown");

    // A caller whose tmux environment points elsewhere still asks the
    // recorded server: alive.
    fs::create_dir_all(env.path("empty-tmux")).unwrap();
    let rows = json(
        env.brg_cmd(&["status", "--json"])
            .env("TMUX_TMPDIR", env.path("empty-tmux"))
            .env("BORGCHART_TMUX_SOCKET", "brg-lc-no-such-socket"),
    );
    assert_eq!(live(&rows, "lead"), "alive");

    // The pane went away outside brg, on a server that still answers: gone.
    env.tmux(&["new-session", "-d", "-s", "keep", "sleep 60"]);
    let lead_pane = read_json(&env.incarnation_file("lead"))["pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    env.tmux(&["kill-window", "-t", &lead_pane]);
    assert_eq!(live(&env.rows(), "lead"), "gone");

    // The whole server exited: it can't be asked, so the pane's own process
    // decides. It exits on the hang-up: gone once it has.
    env.up_helper();
    env.ok(&["stop", "helper"]);
    env.up();
    assert_eq!(live(&env.rows(), "lead"), "alive");
    env.tmux(&["kill-server"]);
    let deadline = Instant::now() + Duration::from_secs(10);
    while live(&env.rows(), "lead") != "gone" {
        assert!(
            Instant::now() < deadline,
            "lead still {}",
            live(&env.rows(), "lead")
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // Ended through brg after a confirmed stop: gone.
    env.up();
    assert_eq!(live(&env.rows(), "lead"), "alive");
    env.ok(&["stop"]);
    assert_eq!(live(&env.rows(), "lead"), "gone");
}

/// Is `lead`'s recorded pane still running? Asked of the recorded server.
fn lead_pane_alive(env: &Env) -> bool {
    let inc = read_json(&env.incarnation_file("lead"));
    let pane = inc["pane"]["pane_id"].as_str().unwrap().to_string();
    env.tmux(&["list-panes", "-a", "-F", "#{pane_id} #{pane_dead}"])
        .lines()
        .any(|l| l == format!("{pane} 0"))
}

/// Is a live `lead` window on the team's server, with or without a bus?
fn lead_window_alive(env: &Env) -> bool {
    let want = format!("{} 0", env.m("lead"));
    env.tmux(&["list-panes", "-a", "-F", "#{window_name} #{pane_dead}"])
        .lines()
        .any(|l| l == want)
}

#[test]
fn down_refuses_a_different_tmux_socket() {
    let env = Env::new(true);
    env.up();
    env.up_helper();
    for args in [&["stop"][..], &["stop", "lead"][..]] {
        let before = env.snapshot();
        let out = env
            .brg_cmd(args)
            .env("BORGCHART_TMUX_SOCKET", "brg-lc-no-such-socket")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{args:?}: {}", both(&out));
        assert!(both(&out).contains("tmux socket"), "{}", both(&out));
        assert!(!mentions_runs(&both(&out)), "{}", both(&out));
        assert_unchanged(&before, &env.snapshot());
        assert!(lead_pane_alive(&env));
    }
}

#[test]
fn down_refuses_when_tmux_cannot_be_reached() {
    for bus in [true, false] {
        refuses_when_unreachable(bus);
    }
}

fn refuses_when_unreachable(bus: bool) {
    let env = Env::new(bus);
    env.up();
    for args in [&["stop"][..], &["stop", "lead"][..]] {
        let before = env.snapshot();
        let out = env
            .brg_cmd(args)
            .env("PATH", env.path("bin"))
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{args:?}: {}", both(&out));
        assert!(both(&out).contains("nothing was changed"), "{}", both(&out));
        assert!(!mentions_runs(&both(&out)), "{}", both(&out));
        assert_unchanged(&before, &env.snapshot());
        assert!(lead_window_alive(&env), "bus: {bus}");
    }
    // A redirected tmux environment still stops the recorded server.
    fs::create_dir_all(env.path("empty-tmux")).unwrap();
    let out = env
        .brg_cmd(&["stop"])
        .env("TMUX_TMPDIR", env.path("empty-tmux"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    assert!(both(&out).contains("stopped"), "{}", both(&out));
    assert_eq!(env.tmux(&["list-sessions"]), "");
    // Stopping again is harmless, even though the server has exited.
    let out = env.ok(&["stop"]);
    assert!(both(&out).contains("not running"), "{}", both(&out));
}

#[test]
fn down_on_a_team_that_never_started_says_not_running() {
    for bus in [false, true] {
        let env = Env::new(bus);
        env.write(
            "oncall/team.yaml",
            &TEAM.replace(
                "    prompt: lead.md\n",
                "    prompt: lead.md\n    start: false\n",
            ),
        );
        env.ok(&["start"]);
        assert_eq!(env.tmux(&["list-sessions"]), "", "bus: {bus}");
        let out = env.ok(&["stop"]);
        assert!(
            both(&out).contains("not running"),
            "bus: {bus}: {}",
            both(&out)
        );
        // A role with no member yet isn't a member.
        let err = both(&env.brg(&["stop", "lead"]));
        assert!(err.contains("no member `lead`"), "bus: {bus}: {err}");
    }
}

#[test]
fn an_up_that_cannot_see_the_server_does_not_make_down_say_not_running() {
    for bus in [false, true] {
        let env = Env::new(bus);
        env.write(
            "oncall/team.yaml",
            &TEAM.replace(
                "    prompt: lead.md\n",
                "    prompt: lead.md\n    start: false\n",
            ),
        );
        env.ok(&["start"]);
        env.ok(&["start", "lead"]);
        env.wait_for("lead idle", |e| e.state_of("lead") == "idle");
        // A caller without tmux finds no session, so it starts the
        // instance afresh, with nothing to start.
        let no_tmux = |args: &[&str]| {
            env.brg_cmd(args)
                .env("PATH", env.path("bin"))
                .output()
                .unwrap()
        };
        let out = no_tmux(&["start"]);
        assert!(out.status.success(), "bus: {bus}: {}", both(&out));
        let out = no_tmux(&["stop"]);
        assert_eq!(out.status.code(), Some(1), "bus: {bus}: {}", both(&out));
        assert!(both(&out).contains("nothing was changed"), "{}", both(&out));
        assert!(lead_window_alive(&env), "bus: {bus}");
    }
}

#[test]
fn down_still_refuses_when_only_a_plugin_process_ran() {
    for bus in [false, true] {
        let env = Env::new(bus);
        env.write(
            "oncall/team.yaml",
            &TEAM
                .replace(
                    "    prompt: lead.md\n",
                    "    prompt: lead.md\n    start: false\n",
                )
                .replace(
                    "    path: plugins/pager\n",
                    "    path: plugins/pager\n    process: [./watch]\n",
                ),
        );
        env.script("oncall/plugins/pager/watch", "#!/bin/sh\nexec sleep 300\n");
        env.ok(&["start"]);
        env.wait_for("the plugin's window", |e| {
            e.tmux(&["list-windows", "-a"]).contains("_pager")
        });
        env.tmux(&["kill-server"]);
        let out = env.brg(&["stop"]);
        assert_eq!(out.status.code(), Some(1), "bus: {bus}: {}", both(&out));
        assert!(both(&out).contains("nothing was changed"), "{}", both(&out));
    }
}

/// `pid`'s start time, if it is a running process (not a zombie).
fn started(pid: u32) -> Option<String> {
    let out = Command::new("ps")
        .env("LC_ALL", "C")
        .args(["-o", "stat=", "-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let (stat, start) = text.trim().split_once(' ')?;
    (!stat.starts_with('Z')).then(|| start.trim().to_string())
}

fn running(pid: u32) -> bool {
    started(pid).is_some()
}

/// Processes a test started, killed if the test fails while they run.
struct Strays(Vec<(u32, String)>);

impl Strays {
    fn add(&mut self, pid: u32) {
        self.0.push((pid, started(pid).expect("a running process")));
    }
}

impl Drop for Strays {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }
        for (pid, start) in &self.0 {
            // Only the same process: its PID may have been reused since.
            if started(*pid).as_ref() == Some(start) {
                let _ = Command::new("kill").args(["-9", &pid.to_string()]).output();
            }
        }
    }
}

/// From inside `member`, as a background tool call would: two children,
/// each in a session of its own (so the pane's hangup never reaches them),
/// one ignoring TERM. The tool keeps waiting for them, so they stay in the
/// member's process tree. Returns their PIDs once both are set up.
fn detach_children(env: &Env, member: &str, strays: &mut Strays) -> [u32; 2] {
    let dir = env.path(&format!("detached/{member}"));
    fs::create_dir_all(&dir).unwrap();
    // Each writes its PID once its session and TERM handling are in place,
    // then becomes `sleep`, keeping both.
    let child = |name: &str, deaf: bool| {
        format!(
            "perl -MPOSIX -e 'POSIX::setsid(); {}open(my $f, \">\", \"{d}/{name}.tmp\") or die; print $f $$; close $f; rename(\"{d}/{name}.tmp\", \"{d}/{name}\"); exec @ARGV' sleep 300 </dev/null >/dev/null 2>&1 &",
            if deaf {
                "$SIG{TERM} = \"IGNORE\"; "
            } else {
                ""
            },
            d = dir.display()
        )
    };
    env.script(
        &format!("detached/{member}.sh"),
        &format!(
            "#!/bin/sh\n{}\n{}\nwait\n",
            child("a", false),
            child("b", true)
        ),
    );
    let script = env.path(&format!("detached/{member}.sh"));
    env.ok(&["send", member, &format!("tool sh {}", script.display())]);
    env.wait_for("both children", |_| {
        dir.join("a").exists() && dir.join("b").exists()
    });
    let pid = |f: &str| -> u32 {
        fs::read_to_string(dir.join(f))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    };
    let pids = [pid("a"), pid("b")];
    // Exec'd into `sleep` (its start time doesn't change).
    env.wait_for("both children sleeping", |_| {
        pids.iter().all(|&p| {
            Command::new("ps")
                .args(["-o", "comm=", "-p", &p.to_string()])
                .output()
                .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("sleep"))
        })
    });
    for p in pids {
        strays.add(p);
    }
    pids
}

#[test]
fn down_ends_children_that_left_the_pane() {
    for bus in [false, true] {
        let env = Env::new(bus);
        let mut strays = Strays(Vec::new());
        env.up();
        env.up_helper();
        let lead = detach_children(&env, "lead", &mut strays);
        let helper = detach_children(&env, "helper", &mut strays);
        let lead_id = env.m("lead");
        // Another window that happens to be called `lead`: not lead's.
        let session = env.tmux(&["list-sessions", "-F", "#{session_name}"]);
        let target = format!("={}:", session.trim());
        let namesake: u32 = env
            .tmux(&[
                "new-window",
                "-d",
                "-P",
                "-F",
                "#{pane_pid}",
                "-t",
                &target,
                "-n",
                &lead_id,
                "sleep 300",
            ])
            .trim()
            .parse()
            .unwrap();
        strays.add(namesake);

        env.ok(&["stop", "lead"]);
        for pid in lead {
            assert!(
                !running(pid),
                "bus: {bus}: lead's child {pid} outlived `down --member lead`"
            );
        }
        for pid in helper.into_iter().chain([namesake]) {
            assert!(
                running(pid),
                "bus: {bus}: {pid}, not lead's, was stopped with lead"
            );
        }

        env.ok(&["stop"]);
        for pid in helper {
            assert!(
                !running(pid),
                "bus: {bus}: helper's child {pid} outlived `down`"
            );
        }
    }
}

fn pid_of(child: &std::process::Child) -> String {
    child.id().to_string()
}

#[test]
fn a_stale_gate_waiter_leaves_a_replacement_alone() {
    let env = Env::new(false);
    env.write(
        "oncall/team.yaml",
        &TEAM.replace(
            "    prompt: lead.md\n",
            "    prompt: lead.md\n    posture: locked\n",
        ),
    );
    fs::create_dir_all(env.path("home/fake-agent")).unwrap();
    fs::write(env.path("home/fake-agent/no-hooks"), "").unwrap();
    let started = Instant::now();
    let first = env
        .brg_cmd(&["start"])
        .env("BORGCHART_GATE_WAIT_MS", "4000")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    env.wait_for("first lead start", |e| e.starts("lead").len() == 1);
    // Pause the first `up` while it waits for the gate, outside its locks.
    let lock_path = env.state_dir().join("lifecycle.lock");
    loop {
        Command::new("kill")
            .args(["-STOP", &pid_of(&first)])
            .status()
            .unwrap();
        let lock = fs::File::open(&lock_path).unwrap();
        if lock.try_lock().is_ok() {
            break;
        }
        drop(lock);
        Command::new("kill")
            .args(["-CONT", &pid_of(&first)])
            .status()
            .unwrap();
        std::thread::sleep(Duration::from_millis(50));
    }
    env.ok(&["stop"]);
    let second = env
        .brg_cmd(&["start"])
        .env("BORGCHART_GATE_WAIT_MS", "10000")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    env.wait_for("second lead start", |e| e.starts("lead").len() == 2);
    let target = format!("=brg-{}:={}", env.inst(), env.m("lead"));
    let pane = || env.tmux(&["list-panes", "-t", &target, "-F", "#{pane_id} #{pane_pid}"]);
    let replacement = pane();
    while started.elapsed() < Duration::from_millis(4500) {
        std::thread::sleep(Duration::from_millis(100));
    }
    Command::new("kill")
        .args(["-CONT", &pid_of(&first)])
        .status()
        .unwrap();
    let first = first.wait_with_output().unwrap();
    assert_eq!(
        outcomes(&stdout(&first), &env.m("lead")),
        ["replaced while starting; left alone"],
        "{}",
        both(&first)
    );
    assert_eq!(
        pane(),
        replacement,
        "the stale waiter touched the replacement"
    );
    let second = second.wait_with_output().unwrap();
    assert_eq!(
        outcomes(&stdout(&second), &env.m("lead")),
        ["stopped: its `locked` gate did not report in, so it would be unprotected"],
        "{}",
        both(&second)
    );
}

#[test]
fn api_key_child_starts_take_the_key_from_the_caller() {
    let env = Env::new(true);
    env.write("config.yaml", "schema: 1\nauth: api_key\n");
    env.brg_cmd(&["start"])
        .env("ANTHROPIC_API_KEY", "fake-key-first")
        .output()
        .map(|o| assert!(o.status.success(), "{}", both(&o)))
        .unwrap();
    env.wait_for("lead start", |e| !e.starts("lead").is_empty());
    assert_eq!(
        env.starts("lead")[0]["env"]["ANTHROPIC_API_KEY"],
        "fake-key-first"
    );

    // The key is not frozen: a member start needs it from the caller...
    let before = env.snapshot();
    let out = env.brg(&["start", "helper"]);
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert!(both(&out).contains("OPENAI_API_KEY"), "{}", both(&out));
    assert_unchanged(&before, &env.snapshot());

    // ...and passes the caller's key on, and nothing stores it.
    let out = env
        .brg_cmd(&["start", "helper"])
        .env("OPENAI_API_KEY", "fake-key-second")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    env.wait_for("helper start", |e| !e.starts("helper").is_empty());
    assert_eq!(
        env.starts("helper")[0]["env"]["OPENAI_API_KEY"],
        "fake-key-second"
    );
    assert!(
        env.starts("helper")[0]["env"]
            .get("ANTHROPIC_API_KEY")
            .is_none()
    );
    for (path, bytes) in env.snapshot() {
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains("fake-key-first") && !text.contains("fake-key-second"),
            "a key was stored in {}",
            path.display()
        );
    }
}

#[test]
fn a_plugin_command_may_not_hide_brg() {
    let env = Env::new(false);
    env.write(
        "oncall/team.yaml",
        &TEAM
            .replace(
                "read: pager read <id>",
                "read: pager read <id>\n      run: brg send <id>",
            )
            .replace("plugins: {pager: [read]}", "plugins: {pager: [read, run]}"),
    );
    env.script("oncall/plugins/pager/brg", "#!/bin/sh\necho plugin\n");
    let out = env.brg(&["start"]);
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert!(
        both(&out).contains("would hide the installed `brg` from"),
        "{}",
        both(&out)
    );
    assert!(
        env.instance().is_none(),
        "the refused create was taken back"
    );
}

/// A helper start that never recorded its pane.
fn unknown_helper(env: &Env) -> String {
    let m = env.m("helper");
    format!(
        r#"{{"member":"{m}","incarnation":"{m}.20261105000000000.abcd","started_at":"2026-11-05T00:00:00.000Z"}}"#
    )
}

/// Run a refused full `up` and check it changed nothing and named
/// `oncall/<member>` without run wording.
fn assert_rollover_refused(env: &Env, member: &str) {
    let before = env.snapshot();
    let out = env.brg(&["start"]);
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert!(
        both(&out).contains(&format!(
            "brg can't tell that {}/{} has stopped",
            env.inst(),
            env.m(member)
        )),
        "{}",
        both(&out)
    );
    assert!(!mentions_runs(&both(&out)), "{}", both(&out));
    assert_unchanged(&before, &env.snapshot());
    assert_eq!(env.tmux(&["list-sessions"]), "");
}

#[test]
fn rollover_checks_members_the_new_configuration_dropped() {
    let env = Env::new(true);
    env.up();
    env.up_helper();
    env.ok(&["stop"]);
    fs::write(env.incarnation_file("helper"), unknown_helper(&env)).unwrap();
    // helper is removed from the team (and lead's file changes with it).
    env.write(
        "oncall/team.yaml",
        &format!("bus: {{}}\n{}", TEAM.split("  helper:\n").next().unwrap()),
    );
    assert_rollover_refused(&env, "helper");
}

#[test]
fn a_format_rollover_refuses_unreadable_incarnations() {
    for bad in ["malformed", "unreadable"] {
        let env = Env::new(true);
        env.up();
        env.up_helper();
        env.ok(&["stop"]);
        let run = env.run_dir();
        let mut m = read_json(&run.join("run.json"));
        m["schema"] = 99.into();
        fs::write(run.join("run.json"), serde_json::to_vec(&m).unwrap()).unwrap();
        let inc = env.incarnation_file("helper");
        if bad == "malformed" {
            fs::write(&inc, "not json").unwrap();
        } else {
            // A directory where the record should be: reading it fails.
            fs::remove_file(&inc).unwrap();
            fs::create_dir_all(&inc).unwrap();
        }
        assert_rollover_refused(&env, "helper");
    }
}

#[test]
fn an_interrupted_rollover_is_finished_before_starting() {
    let env = Env::new(true);
    env.up();
    env.up_helper();
    env.ok(&["stop"]);
    env.write(
        "oncall/lead.md",
        &LEAD.replace("You are the lead.", "You are the lead, edited."),
    );
    env.up();
    env.ok(&["stop"]);
    assert_eq!(env.current(), "r2");
    // As if the rollover had stopped after retiring r1, before switching,
    // and the person then reverted the change.
    fs::write(env.store().join("current"), "r1\n").unwrap();
    env.write("oncall/lead.md", LEAD);

    // A child start doesn't finish it; it says how.
    let before = env.snapshot();
    let out = env.brg(&["start", "helper"]);
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert!(
        both(&out).contains(&format!(
            "an earlier start of instance `{}` was interrupted while moving to fresh messages",
            env.inst()
        )),
        "{}",
        both(&out)
    );
    assert!(!mentions_runs(&both(&out)), "{}", both(&out));
    assert_unchanged(&before, &env.snapshot());

    // A full `up` finishes the move, then rolls over for the reverted file.
    let r1: BTreeMap<PathBuf, Vec<u8>> = walk(&env.store().join("r1"))
        .into_iter()
        .map(|p| (p.clone(), fs::read(&p).unwrap()))
        .collect();
    let out = env.up();
    assert!(!mentions_runs(&both(&out)), "{}", both(&out));
    assert_eq!(env.current(), "r3");
    assert_eq!(
        read_json(&env.run_dir().join("run.json"))["linked_from"]["run"],
        "r2"
    );
    assert_eq!(
        fs::read_to_string(env.store().join("r2/retired")).unwrap(),
        "r3\n"
    );
    assert_eq!(env.starts("lead")[2]["env"]["BORGCHART_RUN"], "r3");
    for (p, bytes) in &r1 {
        assert_eq!(&fs::read(p).unwrap(), bytes, "{}", p.display());
    }
}

#[test]
fn retired_storage_with_no_successor_is_refused() {
    let env = Env::new(true);
    env.up();
    env.up_helper();
    env.ok(&["stop"]);
    fs::write(env.run_dir().join("retired"), "r2\n").unwrap();
    for args in [&["start"][..], &["start", "helper"][..]] {
        let before = env.snapshot();
        let out = env.brg(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {}", both(&out));
        assert!(
            both(&out).contains(&format!(
                "an earlier start of instance `{}` was interrupted while moving to fresh messages",
                env.inst()
            )),
            "{}",
            both(&out)
        );
        assert!(!mentions_runs(&both(&out)), "{}", both(&out));
        assert_unchanged(&before, &env.snapshot());
        assert_eq!(env.tmux(&["list-sessions"]), "");
    }
}

#[test]
fn a_lost_manifest_is_damaged_storage() {
    let env = Env::new(true);
    env.up();
    env.up_helper();
    env.ok(&["stop"]);
    fs::remove_file(env.run_dir().join("run.json")).unwrap();
    env.write(
        "oncall/lead.md",
        &LEAD.replace("You are the lead.", "You are the lead, edited."),
    );
    for args in [&["start"][..], &["start", "helper"][..]] {
        let before = env.snapshot();
        let out = env.brg(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {}", both(&out));
        assert!(
            both(&out).contains(&format!(
                "instance `{}` was started before but its messages are missing",
                env.inst()
            )),
            "{}",
            both(&out)
        );
        assert_unchanged(&before, &env.snapshot());
        assert_eq!(env.tmux(&["list-sessions"]), "");
    }
}

#[test]
fn storage_errors_name_the_team_not_its_files() {
    let env = Env::new(true);
    env.up();
    env.ok(&["stop"]);
    let lock = env.run_dir().join("runtime/lifecycle.lock");
    fs::remove_file(&lock).unwrap();
    fs::create_dir(&lock).unwrap();
    let out = env.brg(&["start"]);
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    let text = both(&out);
    assert!(
        text.contains(&format!("instance `{}`", env.inst())),
        "{text}"
    );
    assert!(!mentions_runs(&text), "{text}");
    assert!(
        !text.contains(&env.store().display().to_string()) && !text.contains("lifecycle.lock"),
        "{text}"
    );
}

/// The tmux server sockets in the test's private directory.
fn sockets(env: &Env) -> Vec<PathBuf> {
    use std::os::unix::fs::FileTypeExt;
    walk(env.tmux.dir())
        .into_iter()
        .filter(|p| fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_socket()))
        .collect()
}

#[test]
fn a_running_team_is_reached_by_its_recorded_server() {
    let env = Env::new(true);
    env.up();
    assert_eq!(sockets(&env).len(), 1, "{:?}", sockets(&env));
    // A caller without brg's socket directory setting (a member's own
    // tool process, say) still reaches the team's server, not a new one.
    let caller = |args: &[&str]| {
        let mut cmd = env.brg_cmd(args);
        cmd.env_remove("BORGCHART_TMUX_TMPDIR");
        cmd.output().unwrap()
    };
    let out = caller(&["start"]);
    assert!(out.status.success(), "{}", both(&out));
    assert_eq!(outcomes(&stdout(&out), &env.m("lead")), ["already running"]);
    assert_eq!(sockets(&env).len(), 1, "{:?}", sockets(&env));

    let out = caller(&["status", "--json"]);
    let rows: Vec<Value> =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|_| panic!("{}", both(&out)));
    let lead = rows.iter().find(|r| r["role"] == "lead").unwrap();
    assert_eq!(lead["state"], "idle", "{lead}");
    assert_eq!(lead["liveness"], "alive", "{lead}");

    let out = caller(&["stop"]);
    assert!(out.status.success(), "{}", both(&out));
    assert!(both(&out).contains("stopped"), "{}", both(&out));
    assert_eq!(env.tmux(&["list-sessions"]), "");
    assert_eq!(env.tmux.refused(), "");
}

#[test]
fn up_refuses_a_relative_tmux_directory() {
    let env = Env::new(true);
    let out = env
        .brg_cmd(&["start"])
        .env("BORGCHART_TMUX_TMPDIR", "tmux")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert!(
        both(&out).contains("must be an absolute path"),
        "{}",
        both(&out)
    );
    assert!(env.snapshot().is_empty(), "{:?}", env.snapshot().keys());
    assert!(sockets(&env).is_empty());
}

/// Liveness when the recorded server can't be reached: these start `tmux`,
/// `ps` and a child process, so they run here and not in the fast tier.
mod liveness {
    use std::process::Command;

    use borgchart::bus::layout::RunPaths;
    use borgchart::bus::manifest;
    use borgchart::bus::manifest::{Incarnation, Liveness, PaneIdentity};
    use borgchart::bus::time::utc_now;
    use borgchart::lifecycle::{
        ENDED_STOPPED, begin_incarnation, check_stopped, complete_incarnation, end_incarnation,
        liveness,
    };

    #[test]
    fn liveness_without_a_pane_or_after_ending() {
        let dir = tempfile::tempdir().unwrap();
        let paths = RunPaths::from_root(dir.path());
        let mut inc = Incarnation {
            member: "lead".into(),
            incarnation: "lead.1.abcd".into(),
            started_at: utc_now(),
            vendor_version: None,
            pane: None,
            ended_at: None,
            ended_reason: None,
        };
        begin_incarnation(&paths, &inc).unwrap();
        assert_eq!(liveness(&paths, "lead", "lead.1.abcd"), Liveness::Unknown);
        assert_eq!(liveness(&paths, "lead", "lead.other"), Liveness::Unknown);
        inc.pane = Some(PaneIdentity {
            socket: format!("/nonexistent/brg-no-such-socket-{}", std::process::id()),
            session: "s".into(),
            window: "lead".into(),
            pane_id: "%0".into(),
            pane_pid: 1,
        });
        complete_incarnation(&paths, &inc).unwrap();
        // A server that can't be reached proves nothing.
        assert_eq!(liveness(&paths, "lead", "lead.1.abcd"), Liveness::Unknown);
        assert!(end_incarnation(&paths, "lead", ENDED_STOPPED).unwrap());
        assert!(!end_incarnation(&paths, "lead", ENDED_STOPPED).unwrap());
        assert_eq!(liveness(&paths, "lead", "lead.1.abcd"), Liveness::Gone);
        let stored = manifest::load_incarnation(&paths, "lead").unwrap().unwrap();
        assert_eq!(stored.ended_reason.as_deref(), Some(ENDED_STOPPED));
    }

    #[test]
    fn an_unreachable_server_shows_a_pane_gone_only_once_its_process_exited() {
        let dir = tempfile::tempdir().unwrap();
        let paths = RunPaths::from_root(dir.path());
        // A pane process that ignores the hang-up outlives its tmux server.
        let mut survivor = Command::new("sh")
            .args(["-c", "trap '' HUP; exec sleep 30"])
            .spawn()
            .unwrap();
        let inc = Incarnation {
            member: "lead".into(),
            incarnation: "lead.1.abcd".into(),
            started_at: utc_now(),
            vendor_version: None,
            pane: Some(PaneIdentity {
                socket: dir.path().join("brg-exited").display().to_string(),
                session: "s".into(),
                window: "lead".into(),
                pane_id: "%0".into(),
                pane_pid: survivor.id(),
            }),
            ended_at: None,
            ended_reason: None,
        };
        begin_incarnation(&paths, &inc).unwrap();
        complete_incarnation(&paths, &inc).unwrap();
        assert_eq!(liveness(&paths, "lead", "lead.1.abcd"), Liveness::Unknown);
        assert!(check_stopped(&paths, "t", "brg up").is_err());

        survivor.kill().unwrap();
        survivor.wait().unwrap();
        assert_eq!(liveness(&paths, "lead", "lead.1.abcd"), Liveness::Gone);
        // So a rollover can go ahead.
        check_stopped(&paths, "t", "brg up").unwrap();
    }
}
