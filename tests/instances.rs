//! Instances and the four verbs: `create`, `start`, `stop`, `delete`, with
//! `ls`, `status` and addressing. Teams are found by name in the search
//! paths of a test configuration (`BORGCHART_CONFIG`) or by path. Real tmux
//! on a private per-test socket, with `fake-agent` standing in for `claude`
//! and `codex`.
//!
//! Run with `cargo test --features test-support`.
#![cfg(feature = "test-support")]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct Env {
    /// First, so its servers stop before the test's directory goes.
    tmux: common::PrivateTmux,
    dir: tempfile::TempDir,
    socket: String,
}

/// `lead` starts with the instance; `helper` (its child) and `scout` (a
/// second root) are created on demand.
const TEAM: &str = r#"
bus: {}
members:
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

const ALPHABET: &str = "23456789abcdefghjkmnpqrstuvwxyz";

fn well_formed(id: &str, base: &str) -> bool {
    id.strip_prefix(base)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|s| s.len() == 4 && s.chars().all(|c| ALPHABET.contains(c)))
}

impl Env {
    fn new() -> Self {
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
    fn team(&self, rel: &str) {
        self.write(&format!("{rel}/team.yaml"), TEAM);
        for (role, model) in [
            ("lead", "claude: opus high"),
            ("helper", "codex: example-model"),
            ("scout", "claude: haiku"),
        ] {
            self.write(
                &format!("{rel}/{role}.md"),
                &format!("---\nrole: {role}\nmodels: {{{model}}}\n---\nYou are the {role}.\n"),
            );
        }
    }

    /// The configuration: the test's search path, plus `extra`.
    fn config(&self, extra: &str) {
        self.write(
            "config.yaml",
            &format!(
                "schema: 1\nteams: [{}]\n{extra}",
                self.path("teams").display()
            ),
        );
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn cmd(&self, args: &[&str]) -> Command {
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

    fn brg(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> Output {
        let out = self.brg(args);
        assert!(out.status.success(), "brg {args:?} failed:\n{}", both(&out));
        out
    }

    fn fails(&self, args: &[&str]) -> String {
        let out = self.brg(args);
        assert!(
            !out.status.success(),
            "brg {args:?} succeeded:\n{}",
            both(&out)
        );
        both(&out)
    }

    /// `brg create <team> <folder>`; the new instance's ID.
    fn create(&self, team: &str, folder: &str) -> String {
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

    fn rows(&self, inst: &str) -> Vec<Value> {
        let out = self.ok(&["status", inst, "--json"]);
        serde_json::from_slice(&out.stdout).unwrap()
    }

    /// The row of `role`'s member (or of the role, with none).
    fn row(&self, inst: &str, role: &str) -> Value {
        self.rows(inst)
            .into_iter()
            .find(|r| r["role"] == role)
            .unwrap_or(Value::Null)
    }

    fn member(&self, inst: &str, role: &str) -> String {
        self.row(inst, role)["member"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    fn state_of(&self, inst: &str, role: &str) -> String {
        self.row(inst, role)["state"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    fn starts(&self, member: &str) -> Vec<Value> {
        fs::read_to_string(self.path(&format!("home/fake-agent/{member}.jsonl")))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|e| e["event"] == "start")
            .collect()
    }

    fn wait_for(&self, what: &str, f: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if f() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("timed out waiting for {what}");
    }

    fn tmux(&self, args: &[&str]) -> String {
        let out = self.tmux.tmux(&self.socket).args(args).output().unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// Publish `record` into `inst` as the outside caller `ops`.
    fn publish(&self, inst: &str, key: &str, record: &Value) -> Output {
        let mut child = self
            .cmd(&[
                "msg",
                "publish",
                "--key",
                key,
                "--external",
                "ops",
                "--instance",
                inst,
                "--json",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(record.to_string().as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    /// An open task assigned to `member` (a root, so `ops` is its authority).
    fn assign(&self, inst: &str, member: &str) {
        self.write("agreement.md", "Fix the parser.\n");
        let sha = {
            use sha2::{Digest, Sha256};
            format!("{:x}", Sha256::digest(b"Fix the parser.\n"))
        };
        let out = self.publish(
            inst,
            &format!("assign-{member}"),
            &json!({"to": member, "kind": "assign", "task": "t1", "attempt": 1,
            "facts": {
                "agreement": {"path": self.path("agreement.md"), "sha256": sha},
                "scope": "the parser",
                "provenance": "asked by the owner",
                "checkout": self.path("work/a"),
            }}),
        );
        assert!(out.status.success(), "assign failed:\n{}", both(&out));
    }

    fn registry(&self) -> Value {
        serde_json::from_slice(&fs::read(self.path("state/registry.json")).unwrap()).unwrap()
    }
}

fn both(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn script(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

// --- 1. IDs ---

#[test]
fn create_twice_makes_two_instances_with_suffixed_members() {
    let env = Env::new();
    let a = env.create("oncall", "work/a");
    let b = env.create("oncall", "work/b");
    assert_ne!(a, b);
    let lead_a = env.member(&a, "lead");
    let lead_b = env.member(&b, "lead");
    assert!(well_formed(&lead_a, "lead"), "{lead_a}");
    assert!(well_formed(&lead_b, "lead"), "{lead_b}");
    // The member's own ID reaches it, with its instance and role.
    let start = &env.starts(&lead_a)[0];
    assert_eq!(start["env"]["BORGCHART_MEMBER"], lead_a);
    assert_eq!(start["env"]["BORGCHART_INSTANCE"], a);
    assert_eq!(start["env"]["BORGCHART_ROLE"], "lead");
    assert!(
        start["env"]["BORGCHART_NOTES"]
            .as_str()
            .unwrap()
            .ends_with(&format!("{a}/notes"))
    );
    assert!(start["env"].get("BORGCHART_FILE").is_none());
    assert!(start["env"].get("BORGCHART_DEPLOYMENT").is_none());
    assert_eq!(
        start["cwd"].as_str().unwrap(),
        env.path("work/a")
            .canonicalize()
            .unwrap()
            .display()
            .to_string()
    );
    // A role's name where a member is expected is refused, listing it.
    let err = env.fails(&["stop", &format!("{a}/lead")]);
    assert!(err.contains("is a role") && err.contains(&lead_a), "{err}");
    let err = env.fails(&["logs", &format!("{a}/lead")]);
    assert!(err.contains(&lead_a), "{err}");
    assert_eq!(env.state_of(&a, "lead"), "idle", "nothing was stopped");
    // The tmux session is `brg-<instance>`.
    assert!(
        env.tmux(&["list-sessions", "-F", "#{session_name}"])
            .contains(&format!("brg-{a}"))
    );
    env.ok(&["stop", &a]);
    env.ok(&["stop", &b]);
}

#[test]
fn a_deleted_instances_suffix_is_never_issued_again() {
    let env = Env::new();
    let seeded = |args: &[&str]| {
        let out = env
            .cmd(args)
            .env("BORGCHART_SUFFIX_SEED", "7")
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", both(&out));
        out
    };
    let folder = env.path("work/a").display().to_string();
    let first = stdout(&seeded(&["create", "oncall", &folder]))
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    env.ok(&["stop", &first]);
    env.ok(&["delete", &first]);
    let second = stdout(&seeded(&["create", "oncall", &folder]))
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    assert!(well_formed(&first, "oncall") && well_formed(&second, "oncall"));
    assert_ne!(first, second, "the same seed draws the same suffix first");
    let issued = env.registry()["issued"]["oncall"].clone();
    assert_eq!(issued.as_array().unwrap().len(), 2, "{issued}");
    env.ok(&["stop", &second]);
}

// --- 2. Finding teams ---

#[test]
fn teams_are_found_by_name_or_by_path() {
    let env = Env::new();
    // By path, from the current folder and absolute.
    let out = env.ok(&["validate", "./teams/oncall"]);
    assert!(stdout(&out).starts_with("ok:"), "{}", both(&out));
    env.ok(&["validate", &env.path("teams/oncall").display().to_string()]);
    env.ok(&["validate", "oncall"]);
    // A path without `./` is refused with the hint.
    let err = env.fails(&["create", "teams/oncall", "work/a"]);
    assert!(err.contains("./teams/oncall"), "{err}");
    let err = env.fails(&["validate", "teams/oncall"]);
    assert!(err.contains("./teams/oncall"), "{err}");
    // A name in two search paths is refused, listing both.
    env.team("more/oncall");
    env.write(
        "config.yaml",
        &format!(
            "schema: 1\nteams: [{}, {}]\n",
            env.path("teams").display(),
            env.path("more").display()
        ),
    );
    let err = env.fails(&["create", "oncall", "work/a"]);
    assert!(
        err.contains(
            &env.path("teams/oncall")
                .canonicalize()
                .unwrap()
                .display()
                .to_string()
        ) && err.contains(
            &env.path("more/oncall")
                .canonicalize()
                .unwrap()
                .display()
                .to_string()
        ),
        "{err}"
    );
    // Two folders with the same name both run, by path.
    let a = stdout(&env.ok(&["create", "./teams/oncall", "work/a"]))
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    let b = stdout(&env.ok(&["create", "./more/oncall", "work/b"]))
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    assert!(well_formed(&a, "oncall") && well_formed(&b, "oncall") && a != b);
    let ls: Value = serde_json::from_slice(&env.ok(&["ls", "--json"]).stdout).unwrap();
    let teams: Vec<&Value> = ls.as_array().unwrap().iter().collect();
    assert_eq!(teams.len(), 2, "{ls}");
    for t in &teams {
        assert_eq!(t["instances"].as_array().unwrap().len(), 1, "{ls}");
    }
    env.ok(&["stop", &a]);
    env.ok(&["stop", &b]);
}

#[test]
fn create_needs_an_existing_folder() {
    let env = Env::new();
    let err = env.fails(&["create", "oncall"]);
    assert!(err.contains("folder"), "{err}");
    let err = env.fails(&["create", "oncall", "work/missing"]);
    assert!(err.contains("does not exist"), "{err}");
    assert!(!env.path("state/registry.json").exists() || env.registry()["instances"] == json!({}));
}

// --- 3, 4, 10. start, stop and create of a member ---

#[test]
fn start_brings_back_what_should_run_and_leaves_running_members_alone() {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    let lead = env.member(&inst, "lead");
    env.ok(&["create", &format!("{inst}/helper")]);
    let helper = env.member(&inst, "helper");
    assert!(well_formed(&helper, "helper"), "{helper}");
    env.wait_for("helper idle", || env.state_of(&inst, "helper") == "idle");
    // Running: only a crashed member is started again.
    env.tmux(&["kill-window", "-t", &format!("=brg-{inst}:={lead}")]);
    let out = env.ok(&["start", &inst]);
    assert!(stdout(&out).contains("already running"), "{}", both(&out));
    env.wait_for("lead idle", || env.state_of(&inst, "lead") == "idle");
    assert_eq!(env.starts(&lead).len(), 2);
    assert_eq!(env.starts(&helper).len(), 1);
    // Stopped: the members running at the stop come back. A second stop
    // keeps the first's record.
    env.ok(&["stop", &inst]);
    let out = env.ok(&["stop", &inst]);
    assert!(stdout(&out).contains("not running"), "{}", both(&out));
    env.ok(&["start", &inst]);
    env.wait_for("helper idle", || env.state_of(&inst, "helper") == "idle");
    assert_eq!(env.starts(&helper).len(), 2);
    assert_eq!(env.starts(&lead).len(), 3);
    env.ok(&["stop", &inst]);
}

#[test]
fn start_brings_back_members_holding_work() {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    env.ok(&["create", &format!("{inst}/scout")]);
    let scout = env.member(&inst, "scout");
    env.wait_for("scout idle", || env.state_of(&inst, "scout") == "idle");
    env.ok(&["stop", &format!("{inst}/{scout}")]);
    env.assign(&inst, &scout);
    env.ok(&["stop", &inst]);
    env.ok(&["start", &inst]);
    env.wait_for("scout idle", || env.state_of(&inst, "scout") == "idle");
    assert_eq!(env.starts(&scout).len(), 2, "scout holds a task");
    env.ok(&["stop", &inst]);
}

#[test]
fn stop_on_a_stopped_instance_or_member_is_a_no_op() {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    let lead = env.member(&inst, "lead");
    env.ok(&["stop", &format!("{inst}/{lead}")]);
    let out = env.ok(&["stop", &format!("{inst}/{lead}")]);
    assert!(stdout(&out).contains("not running"), "{}", both(&out));
    env.ok(&["stop", &inst]);
    env.ok(&["stop", &inst]);
}

#[test]
fn create_of_an_existing_member_is_refused_and_start_resumes_it() {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    env.ok(&["create", &format!("{inst}/helper")]);
    let helper = env.member(&inst, "helper");
    let err = env.fails(&["create", &format!("{inst}/helper")]);
    assert!(err.contains(&format!("brg start {inst}/{helper}")), "{err}");
    env.ok(&["stop", &format!("{inst}/{helper}")]);
    let err = env.fails(&["create", &format!("{inst}/helper")]);
    assert!(err.contains(&format!("brg start {inst}/{helper}")), "{err}");
    let out = env.ok(&["start", &format!("{inst}/{helper}")]);
    assert!(stdout(&out).contains("started"), "{}", both(&out));
    env.wait_for("helper idle", || env.state_of(&inst, "helper") == "idle");
    assert_eq!(env.member(&inst, "helper"), helper, "the same member");
    assert_eq!(env.starts(&helper).len(), 2);
    let out = env.ok(&["start", &format!("{inst}/{helper}")]);
    assert!(stdout(&out).contains("already running"), "{}", both(&out));
    env.ok(&["stop", &inst]);
}

// --- 5. delete ---

#[test]
fn delete_needs_a_stop_and_force_for_open_work_and_keeps_the_folder() {
    let env = Env::new();
    fs::write(env.path("work/a/keep.txt"), "mine\n").unwrap();
    let inst = env.create("oncall", "work/a");
    let lead = env.member(&inst, "lead");
    let err = env.fails(&["delete", &inst]);
    assert!(
        err.contains("running") && err.contains(&format!("brg stop {inst}")),
        "{err}"
    );
    env.assign(&inst, &lead);
    env.ok(&["stop", &inst]);
    let err = env.fails(&["delete", &inst]);
    assert!(err.contains("open work") && err.contains("t1"), "{err}");
    assert!(env.path(&format!("state/{inst}")).is_dir());
    env.ok(&["delete", &inst, "--force"]);
    assert!(!env.path(&format!("state/{inst}")).exists());
    assert!(env.registry()["instances"].get(&inst).is_none());
    assert_eq!(
        fs::read_to_string(env.path("work/a/keep.txt")).unwrap(),
        "mine\n"
    );
    let err = env.fails(&["status", &inst]);
    assert!(err.contains("no instance"), "{err}");
}

// --- 8. Addressing ---

#[test]
fn addresses_name_the_instance_in_full_outside_a_member() {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    let lead = env.member(&inst, "lead");
    // Outside a member, a member needs its instance, even with one running.
    let err = env.fails(&["logs", &lead]);
    assert!(err.contains(&format!("<instance>/{lead}")), "{err}");
    // A team's name where an instance is expected.
    let err = env.fails(&["status", "oncall"]);
    assert!(err.contains("is a team") && err.contains(&inst), "{err}");
    // Inside a member, the instance comes from BORGCHART_INSTANCE.
    let out = env
        .cmd(&["logs", &lead])
        .env("BORGCHART_INSTANCE", &inst)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    // Bus commands take `--instance`, required outside a member.
    let err = env.fails(&["inbox", "--external", "ops"]);
    assert!(err.contains("--instance") && err.contains(&inst), "{err}");
    env.ok(&["inbox", "--external", "ops", "--instance", &inst]);
    env.ok(&["bus", "inspect", "--instance", &inst]);
    // An instance isn't a recipient; nor is a role.
    for to in [inst.as_str(), "lead"] {
        let out = env.publish(
            &inst,
            &format!("to-{to}"),
            &json!({"to": to, "kind": "note", "body": "hi"}),
        );
        assert!(!out.status.success(), "{}", both(&out));
        assert!(both(&out).contains(&lead), "{}", both(&out));
    }
    // A role with no member shows as on demand.
    assert_eq!(env.state_of(&inst, "helper"), "on demand (none created)");
    let text = stdout(&env.ok(&["status", &inst]));
    assert!(text.lines().next().unwrap().starts_with(&inst), "{text}");
    assert!(
        text.lines()
            .any(|l| l.contains("helper") && l.contains("on demand (none created)")),
        "{text}"
    );
    // Bare `status` shows every instance.
    let b = env.create("oncall", "work/b");
    let all = stdout(&env.ok(&["status"]));
    assert!(all.contains(&inst) && all.contains(&b), "{all}");
    env.ok(&["stop", &inst]);
    env.ok(&["stop", &b]);
}

// --- 9. Removed names ---

#[test]
fn removed_commands_and_files_are_gone() {
    let env = Env::new();
    for args in [
        vec!["up"],
        vec!["down"],
        vec!["status", "--file", "x"],
        vec!["create", "--team", "x"],
        vec!["stop", "x", "--team", "y"],
        vec!["status", "--team", "x"],
    ] {
        let out = env.brg(&args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", both(&out));
        let text = both(&out);
        assert!(
            text.contains("unrecognized subcommand") || text.contains("unexpected argument"),
            "{args:?}: {text}"
        );
    }
    // A `borgchart.yaml` in the team folder is not read.
    env.write("teams/oncall/borgchart.yaml", "this: [is not read\n");
    env.ok(&["validate", "oncall"]);
    let inst = env.create("oncall", "work/a");
    env.ok(&["stop", &inst]);
}

// --- 14. Configuration ---

#[test]
fn auth_comes_from_the_configuration_and_a_missing_file_means_login() {
    let env = Env::new();
    let sentinel = "sk-not-a-real-key-2b7d";
    // No configuration file: the defaults, so no key even though the shell has one.
    let out = env
        .cmd(&[
            "create",
            "./teams/oncall",
            &env.path("work/a").display().to_string(),
        ])
        .env("BORGCHART_CONFIG", env.path("missing.yaml"))
        .env("ANTHROPIC_API_KEY", sentinel)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    let a = stdout(&out).split_whitespace().next().unwrap().to_string();
    env.wait_for("lead idle", || env.state_of(&a, "lead") == "idle");
    let lead = env.member(&a, "lead");
    assert!(
        env.starts(&lead)[0]["env"]
            .get("ANTHROPIC_API_KEY")
            .is_none()
    );
    // `auth: api_key` from the configuration passes it.
    env.config("auth: api_key\n");
    let out = env
        .cmd(&[
            "create",
            "oncall",
            &env.path("work/b").display().to_string(),
        ])
        .env("ANTHROPIC_API_KEY", sentinel)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    assert!(!both(&out).contains(sentinel));
    let b = stdout(&out).split_whitespace().next().unwrap().to_string();
    env.wait_for("lead idle", || env.state_of(&b, "lead") == "idle");
    let lead_b = env.member(&b, "lead");
    assert_eq!(env.starts(&lead_b)[0]["env"]["ANTHROPIC_API_KEY"], sentinel);
    // A changed `auth` refuses a member start into the running instance,
    // and applies after a full stop.
    env.config("");
    let err = env.fails(&["create", &format!("{b}/scout")]);
    assert!(
        err.contains("`auth`") && err.contains(&format!("brg stop {b}")),
        "{err}"
    );
    env.ok(&["stop", &b]);
    env.ok(&["start", &b]);
    env.wait_for("lead idle", || env.state_of(&b, "lead") == "idle");
    assert!(
        env.starts(&lead_b)[1]["env"]
            .get("ANTHROPIC_API_KEY")
            .is_none()
    );
    env.ok(&["stop", &a]);
    env.ok(&["stop", &b]);
}

#[test]
fn auto_trust_comes_from_the_configuration() {
    let env = Env::new();
    fs::create_dir_all(env.path("home/fake-agent")).unwrap();
    fs::write(env.path("home/fake-agent/trust"), "").unwrap();
    let folder = env.path("work/a").display().to_string();
    // The default leaves a `vendor` member's trust dialog for a person.
    let out = env.ok(&["create", "oncall", &folder]);
    assert!(
        !stdout(&out).contains("accepted folder trust"),
        "{}",
        both(&out)
    );
    let a = stdout(&out).split_whitespace().next().unwrap().to_string();
    let lead = env.member(&a, "lead");
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !fs::read_to_string(env.path(&format!("home/fake-agent/{lead}.jsonl")))
            .unwrap_or_default()
            .contains("trust-accepted")
    );
    env.ok(&["stop", &a]);
    env.config("auto_trust: true\n");
    let out = env.ok(&[
        "create",
        "oncall",
        &env.path("work/b").display().to_string(),
    ]);
    assert!(
        stdout(&out).contains("accepted folder trust for"),
        "{}",
        both(&out)
    );
    let b = stdout(&out).split_whitespace().next().unwrap().to_string();
    env.ok(&["stop", &b]);
}

#[test]
fn tmux_user_config_comes_from_the_configuration_and_a_change_is_reported() {
    let env = Env::new();
    env.write("my-tmux.conf", "set -g @my-option loaded\n");
    env.config(&format!(
        "tmux: {{user_config: {}}}\n",
        env.path("my-tmux.conf").display()
    ));
    let a = env.create("oncall", "work/a");
    assert_eq!(
        env.tmux(&["show-options", "-gqv", "@my-option"]).trim(),
        "loaded"
    );
    env.config("");
    let out = env.ok(&[
        "create",
        "oncall",
        &env.path("work/b").display().to_string(),
    ]);
    assert!(
        stdout(&out).contains("takes effect only once"),
        "{}",
        both(&out)
    );
    let b = stdout(&out).split_whitespace().next().unwrap().to_string();
    env.ok(&["stop", &a]);
    env.ok(&["stop", &b]);
}

#[test]
fn plain_plugin_values_come_from_the_team() {
    let env = Env::new();
    env.write(
        "teams/oncall/team.yaml",
        &TEAM.replace(
            "members:\n  lead:\n    prompt: lead.md\n",
            "plugins:\n  pager:\n    path: plugins/pager\n    env:\n      PAGER_ROOM: R0000EXAMPLE\n      PAGER_TOKEN:\n    actions:\n      read: pager read <id>\nmembers:\n  lead:\n    prompt: lead.md\n    plugins: {pager: [read]}\n",
        ),
    );
    fs::create_dir_all(env.path("teams/oncall/plugins/pager")).unwrap();
    script(
        &env.path("teams/oncall/plugins/pager/pager"),
        "#!/bin/sh\necho \"room=$PAGER_ROOM token_len=${#PAGER_TOKEN} instance=$BORGCHART_INSTANCE\" >> \"$BORGCHART_STATE_DIR/calls.log\"\n",
    );
    // The secret must come from the environment.
    let err = env.fails(&[
        "create",
        "oncall",
        &env.path("work/a").display().to_string(),
    ]);
    assert!(
        err.contains("PAGER_TOKEN") && !err.contains("PAGER_ROOM"),
        "{err}"
    );
    let out = env
        .cmd(&[
            "create",
            "oncall",
            &env.path("work/a").display().to_string(),
        ])
        .env("PAGER_TOKEN", "tok-not-real")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", both(&out));
    let inst = stdout(&out).split_whitespace().next().unwrap().to_string();
    env.wait_for("lead idle", || env.state_of(&inst, "lead") == "idle");
    let lead = env.member(&inst, "lead");
    let shim = env.path(&format!("state/{inst}/members/{lead}/bin/pager"));
    let run = Command::new(&shim)
        .args(["read", "1"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", both(&run));
    let log =
        fs::read_to_string(env.path(&format!("state/{inst}/plugins/pager/calls.log"))).unwrap();
    assert_eq!(
        log.trim(),
        format!("room=R0000EXAMPLE token_len=12 instance={inst}")
    );
    env.ok(&["stop", &inst]);
}

#[test]
fn a_roles_args_and_cwd_come_from_the_team() {
    let env = Env::new();
    env.write(
        "teams/oncall/team.yaml",
        &TEAM.replace(
            "  lead:\n    prompt: lead.md\n",
            "  lead:\n    prompt: lead.md\n    cwd: sub\n    args: [--example-flag]\n",
        ),
    );
    fs::create_dir_all(env.path("work/a/sub")).unwrap();
    let inst = env.create("oncall", "work/a");
    let lead = env.member(&inst, "lead");
    let start = &env.starts(&lead)[0];
    assert_eq!(
        start["cwd"].as_str().unwrap(),
        env.path("work/a/sub")
            .canonicalize()
            .unwrap()
            .display()
            .to_string()
    );
    assert!(
        start["argv"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a == "--example-flag"),
        "{start}"
    );
    env.ok(&["stop", &inst]);
    // A `cwd` outside the folder is refused by validation.
    env.write(
        "teams/oncall/team.yaml",
        &TEAM.replace(
            "  lead:\n    prompt: lead.md\n",
            "  lead:\n    prompt: lead.md\n    cwd: ../x\n",
        ),
    );
    let err = env.fails(&["validate", "oncall"]);
    assert!(err.contains("inside the instance's folder"), "{err}");
}
