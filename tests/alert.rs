//! Alert action tests: a fake plugin script records its argv, environment
//! and working directory, so the tests can check exactly what an alert
//! action receives.
//!
//! The library-level tests build a deployment's state by hand; the CLI tests
//! run a real `brg up` (with `start: false` members, so no tmux windows).

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use borgchart::alert::{self, AlertOutcome, AlertRequest, Limits, Reason};
use borgchart::bus::layout::RunPaths;
use borgchart::deploy::{AlertConfig, BusConfig};
use borgchart::state::{MemberMeta, Meta, PluginMeta, StateDir};

const NOTIFY_TOKEN: &str = "notify-not-a-real-token-77aa";
const SLACK_SECRET: &str = "xoxb-not-a-real-token-41f3";

/// A script that writes its argv, environment and cwd next to itself.
fn recorder(out: &Path) -> String {
    format!(
        "#!/bin/sh\nout={}\nprintf '%s\\n' \"$0\" \"$@\" > \"$out/argv\"\nenv > \"$out/env\"\npwd > \"$out/cwd\"\necho handed-off\n",
        out.display()
    )
}

fn write_script(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn read_env(path: &Path) -> BTreeMap<String, String> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn read_lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

// ---- library-level -------------------------------------------------------

struct Fixture {
    dir: tempfile::TempDir,
    state: StateDir,
    meta: Meta,
    bus: BusConfig,
}

impl Fixture {
    /// A plugin `notify` with action `notify alert --quiet <id> <member> <reason> <pending>`.
    fn new(script: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let state = StateDir::from_root(&dir.path().join("state/example-1234"));
        let plugin_dir = dir.path().join("plugins/notify");
        write_script(&plugin_dir.join("notify"), script);
        let mut meta = Meta {
            source: dir.path().join("borgchart.yaml"),
            brg: PathBuf::from("/opt/example/brg"),
            base_env: BTreeMap::from([
                ("PATH".to_string(), "/usr/bin:/bin".to_string()),
                ("LANG".to_string(), "C.UTF-8".to_string()),
            ]),
            ..Meta::default()
        };
        meta.members.insert(
            "lead".into(),
            MemberMeta {
                vendor: "claude".into(),
                posture: "vendor".into(),
                ..MemberMeta::default()
            },
        );
        meta.plugins.insert(
            "notify".into(),
            PluginMeta {
                dir: Some(plugin_dir),
                has_process: false,
                actions: BTreeMap::from([(
                    "alert".to_string(),
                    vec!["notify".into(), "alert".into(), "--quiet".into()],
                )]),
            },
        );
        let fx = Fixture {
            state,
            meta,
            bus: BusConfig {
                root: dir.path().join("runs"),
                run: "example-run-1".into(),
                alert: Some(AlertConfig {
                    plugin: "notify".into(),
                    action: "alert".into(),
                }),
                continues: None,
                warn_age_days: 7,
                max_age_days: 14,
            },
            dir,
        };
        fx.write_values("notify", &[("NOTIFY_TOKEN", NOTIFY_TOKEN)]);
        fx
    }

    fn out(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    fn write_values(&self, plugin: &str, values: &[(&str, &str)]) {
        let env: BTreeMap<_, _> = values.iter().copied().collect();
        let path = self.state.plugin_env_file(plugin);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, serde_json::json!({ "env": env }).to_string()).unwrap();
    }

    fn invoke(&self, limits: &Limits) -> alert::Invocation {
        alert::invoke(&self.state, &self.meta, &self.bus, &request(), limits)
    }
}

fn request() -> AlertRequest {
    AlertRequest {
        id: "a-example-1".into(),
        member: "lead".into(),
        reason: Reason::Blocked,
        pending: 3,
    }
}

#[test]
fn argv_is_exactly_the_prefix_and_fixed_arguments() {
    let fx = Fixture::new("");
    write_script(
        &fx.meta.plugins["notify"]
            .dir
            .clone()
            .unwrap()
            .join("notify"),
        &recorder(&fx.out()),
    );
    let inv = fx.invoke(&Limits::default());
    assert_eq!(inv.outcome, AlertOutcome::Invoked, "{inv:?}");
    let plugin_dir = fx.meta.plugins["notify"].dir.clone().unwrap();
    assert_eq!(
        read_lines(&fx.out().join("argv")),
        vec![
            plugin_dir.join("notify").display().to_string(),
            "alert".into(),
            "--quiet".into(),
            "a-example-1".into(),
            "lead".into(),
            "blocked".into(),
            "3".into(),
        ]
    );
    assert_eq!(
        fs::read_to_string(fx.out().join("cwd")).unwrap().trim(),
        plugin_dir.canonicalize().unwrap().display().to_string()
    );
    assert_eq!(inv.output.trim(), "handed-off");
}

#[test]
fn env_is_built_from_scratch() {
    let fx = Fixture::new("");
    write_script(
        &fx.meta.plugins["notify"]
            .dir
            .clone()
            .unwrap()
            .join("notify"),
        &recorder(&fx.out()),
    );
    // Another plugin's secret is never read, and a values file can't smuggle
    // in member identity.
    fx.write_values("slack", &[("SLACK_BOT_TOKEN", SLACK_SECRET)]);
    fx.write_values(
        "notify",
        &[
            ("NOTIFY_TOKEN", NOTIFY_TOKEN),
            ("BORGCHART_MEMBER", "lead"),
            ("BORGCHART_ACTOR", "member"),
        ],
    );
    let inv = fx.invoke(&Limits::default());
    assert_eq!(inv.outcome, AlertOutcome::Invoked, "{inv:?}");
    let env = read_env(&fx.out().join("env"));
    assert_eq!(env["BORGCHART_ACTOR"], "bus");
    assert_eq!(env["BORGCHART_RUN"], "example-run-1");
    assert_eq!(env["BORGCHART_DEPLOYMENT"], "example-1234");
    assert_eq!(env["BORGCHART_BIN"], "/opt/example/brg");
    assert_eq!(env["BORGCHART_FILE"], fx.meta.source.display().to_string());
    assert_eq!(
        env["BORGCHART_STATE_DIR"],
        fx.state.plugin_dir("notify").display().to_string()
    );
    assert_eq!(env["NOTIFY_TOKEN"], NOTIFY_TOKEN);
    assert_eq!(env["PATH"], "/usr/bin:/bin");
    assert!(!env.contains_key("BORGCHART_MEMBER"), "{env:?}");
    assert!(!env.contains_key("BORGCHART_PARENT"));
    assert!(!env.contains_key("SLACK_BOT_TOKEN"));
    let text = fs::read_to_string(fx.out().join("env")).unwrap();
    assert!(!text.contains(SLACK_SECRET));
}

#[test]
fn missing_configuration_is_unavailable() {
    let unavailable = |fx: &Fixture| match fx.invoke(&Limits::default()).outcome {
        AlertOutcome::Unavailable(why) => why,
        other => panic!("expected unavailable, got {other:?}"),
    };

    let mut fx = Fixture::new("#!/bin/sh\nexit 0\n");
    fx.bus.alert = None;
    assert!(unavailable(&fx).contains("bus.alert"));

    let fx = Fixture::new("#!/bin/sh\nexit 0\n");
    fs::remove_file(fx.state.plugin_env_file("notify")).unwrap();
    assert!(unavailable(&fx).contains("values"));

    let mut fx = Fixture::new("#!/bin/sh\nexit 0\n");
    fx.meta.plugins.clear();
    assert!(unavailable(&fx).contains("notify"));

    let mut fx = Fixture::new("#!/bin/sh\nexit 0\n");
    fx.meta.plugins.get_mut("notify").unwrap().dir = None;
    assert!(unavailable(&fx).contains("PATH"));

    let mut fx = Fixture::new("#!/bin/sh\nexit 0\n");
    fx.meta.plugins.get_mut("notify").unwrap().actions.clear();
    assert!(unavailable(&fx).contains("alert"));
}

#[test]
fn absent_program_is_unavailable() {
    let fx = Fixture::new("#!/bin/sh\nexit 0\n");
    fs::remove_file(
        fx.meta.plugins["notify"]
            .dir
            .clone()
            .unwrap()
            .join("notify"),
    )
    .unwrap();
    assert!(matches!(
        fx.invoke(&Limits::default()).outcome,
        AlertOutcome::Unavailable(_)
    ));
}

#[test]
fn invalid_request_runs_nothing() {
    let fx = Fixture::new("");
    write_script(
        &fx.meta.plugins["notify"]
            .dir
            .clone()
            .unwrap()
            .join("notify"),
        &recorder(&fx.out()),
    );
    for (id, member) in [
        ("a b", "lead"),
        ("A1", "lead"),
        ("ok-1", "Lead; rm"),
        ("ok-1", ""),
    ] {
        let req = AlertRequest {
            id: id.into(),
            member: member.into(),
            reason: Reason::Test,
            pending: 0,
        };
        let inv = alert::invoke(&fx.state, &fx.meta, &fx.bus, &req, &Limits::default());
        assert!(
            matches!(inv.outcome, AlertOutcome::Unavailable(_)),
            "{id} {member}"
        );
    }
    assert!(!fx.out().join("argv").exists());
}

#[test]
fn non_zero_exit_is_failed_with_output() {
    let fx = Fixture::new("#!/bin/sh\necho 'no route to notifier' >&2\nexit 7\n");
    let inv = fx.invoke(&Limits::default());
    assert_eq!(inv.outcome, AlertOutcome::Failed { status: Some(7) });
    assert!(inv.output.contains("no route to notifier"), "{inv:?}");
}

#[test]
fn timeout_kills_the_process_group() {
    let fx = Fixture::new("");
    write_script(
        &fx.meta.plugins["notify"]
            .dir
            .clone()
            .unwrap()
            .join("notify"),
        &format!(
            "#!/bin/sh\nsleep 30 &\necho $! > {}/child\nsleep 30\n",
            fx.out().display()
        ),
    );
    let limits = Limits {
        timeout: Duration::from_millis(500),
        ..Limits::default()
    };
    let start = Instant::now();
    let inv = fx.invoke(&limits);
    assert_eq!(inv.outcome, AlertOutcome::TimedOut);
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    let child: i32 = fs::read_to_string(fx.out().join("child"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // The background child was in the same group and is gone (or a zombie
    // about to be reaped by init).
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let status = fs::read_to_string(format!("/proc/{child}/stat")).unwrap_or_default();
        let state = status.split(") ").nth(1).and_then(|s| s.chars().next());
        if status.is_empty() || state == Some('Z') {
            break;
        }
        assert!(Instant::now() < deadline, "child {child} still running");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn output_is_capped_to_its_tail() {
    let fx = Fixture::new(
        "#!/bin/sh\ni=0\nwhile [ $i -lt 4000 ]; do echo \"line $i of noisy output\"; i=$((i+1)); done\necho last-line\n",
    );
    let limits = Limits {
        max_output: 256,
        ..Limits::default()
    };
    let inv = fx.invoke(&limits);
    assert_eq!(inv.outcome, AlertOutcome::Invoked);
    assert!(inv.output.len() <= 256, "{}", inv.output.len());
    assert!(
        inv.output.trim_end().ends_with("last-line"),
        "{}",
        inv.output
    );
}

#[test]
fn defaults_are_bounded() {
    let limits = Limits::default();
    assert_eq!(limits.timeout, Duration::from_secs(10));
    assert_eq!(limits.max_output, 4096);
    assert_eq!(Reason::HumanRequired.as_str(), "human-required");
    assert_eq!(Reason::WorkerUnavailable.as_str(), "worker-unavailable");
}

// ---- store ---------------------------------------------------------------

fn run_paths(dir: &Path) -> RunPaths {
    let bus = BusConfig {
        root: dir.join("runs"),
        run: "example-run-1".into(),
        alert: None,
        continues: None,
        warn_age_days: 7,
        max_age_days: 14,
    };
    borgchart::bus::layout::ensure_run_dir(&bus).unwrap()
}

#[test]
fn store_records_invocations_and_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let paths = run_paths(dir.path());
    let opened = alert::store::open(&paths, &request()).unwrap();
    assert_eq!(opened.state, alert::store::State::Open);
    assert_eq!(opened.invocation_count, 0);
    // Opening again with the same request is a no-op.
    let again = alert::store::open(&paths, &request()).unwrap();
    assert_eq!(again.opened_at, opened.opened_at);
    // A different request under the same ID is refused.
    let mut other = request();
    other.member = "helper".into();
    assert!(alert::store::open(&paths, &other).is_err());

    let inv = alert::Invocation {
        outcome: AlertOutcome::Failed { status: Some(1) },
        output: "x".repeat(10),
        elapsed_ms: 5,
    };
    for _ in 0..13 {
        alert::store::record_invocation(&paths, "a-example-1", &inv).unwrap();
    }
    let listed = alert::store::list(&paths).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].invocation_count, 13);
    assert_eq!(listed[0].invocations.len(), alert::store::MAX_INVOCATIONS);
    assert_eq!(listed[0].invocations[0].outcome, "failed");
    assert_eq!(listed[0].invocations[0].status, Some(1));

    let resolved = alert::store::resolve(&paths, "a-example-1", "operator").unwrap();
    assert_eq!(resolved.state, alert::store::State::Resolved);
    assert_eq!(resolved.resolved_by.as_deref(), Some("operator"));
    assert!(resolved.resolved_at.is_some());
    assert!(alert::store::resolve(&paths, "a-missing", "operator").is_err());
    let mode = fs::metadata(paths.alert("a-example-1"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

// ---- CLI -----------------------------------------------------------------

struct Deploy {
    dir: tempfile::TempDir,
}

impl Deploy {
    fn new(alert: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let d = Deploy { dir };
        d.write(
            "team/team.yaml",
            r#"
plugins:
  notify:
    path: plugins/notify
    env: [NOTIFY_TOKEN, NOTIFY_ROOM]
    actions:
      alert: notify alert --quiet <id> <member> <reason> <pending>
  slack:
    path: plugins/slack
    env: [SLACK_BOT_TOKEN]
    actions:
      post: slack post <text>
members:
  lead:
    prompt: lead.md
    start: false
  helper:
    prompt: helper.md
    parent: lead
    start: false
"#,
        );
        d.write("team/lead.md", "---\nrole: lead\n---\nYou lead.\n");
        d.write("team/helper.md", "---\nrole: helper\n---\nYou help.\n");
        write_script(
            &d.path("team/plugins/notify/notify"),
            &recorder(&d.path("out")),
        );
        write_script(&d.path("team/plugins/slack/slack"), "#!/bin/sh\nexit 0\n");
        fs::create_dir_all(d.path("out")).unwrap();
        fs::create_dir_all(d.path("home")).unwrap();
        d.deploy_file("R0000EXAMPLE", alert);
        d
    }

    fn deploy_file(&self, room: &str, alert: bool) {
        let alert_line = if alert {
            "  alert: {plugin: notify, action: alert}\n"
        } else {
            ""
        };
        self.write(
            "team/borgchart.yaml",
            &format!(
                "schema: 1\nplugins:\n  notify:\n    env: {{NOTIFY_ROOM: {room}}}\nbus:\n  root: {}\n  run: example-run-1\n{alert_line}",
                self.path("runs").display()
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

    fn brg_cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_brg"));
        cmd.env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.path("home"))
            .env("LANG", "C.UTF-8")
            .env("BORGCHART_STATE_HOME", self.path("state"))
            .env("BORGCHART_TMUX_SOCKET", "brg-alert-test-unused")
            .current_dir(self.path("team"))
            .args(args);
        cmd
    }

    fn up(&self) {
        let out = self
            .brg_cmd(&["up"])
            .env("NOTIFY_TOKEN", NOTIFY_TOKEN)
            .env("SLACK_BOT_TOKEN", SLACK_SECRET)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn json(&self, args: &[&str]) -> (i32, serde_json::Value) {
        let out = self.brg_cmd(args).output().unwrap();
        (code(&out), parse(&out))
    }
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

fn parse(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "not JSON ({e}): {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

#[test]
fn cli_test_alert_isolates_the_action_from_the_caller() {
    let d = Deploy::new(true);
    d.up();
    // After `up`, the caller's environment and the deployment file change.
    d.deploy_file("R9999CHANGED", true);
    let out = d
        .brg_cmd(&["bus", "alert", "test", "--member", "lead", "--json"])
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", d.path("evil").display()),
        )
        .env("NOTIFY_TOKEN", "changed-in-caller")
        .env("SLACK_BOT_TOKEN", "caller-slack-token")
        .env("BORGCHART_MEMBER", "lead")
        .env("BORGCHART_PARENT", "lead")
        .env("BORGCHART_INCARNATION", "i-1")
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let json = parse(&out);
    assert_eq!(json["outcome"], "invoked", "{json}");
    let id = json["alert"]["id"].as_str().unwrap().to_string();

    let env = read_env(&d.path("out/env"));
    assert_eq!(env["BORGCHART_ACTOR"], "bus");
    assert_eq!(env["BORGCHART_RUN"], "example-run-1");
    assert_eq!(env["NOTIFY_TOKEN"], NOTIFY_TOKEN);
    assert_eq!(env["NOTIFY_ROOM"], "R0000EXAMPLE");
    for name in [
        "SLACK_BOT_TOKEN",
        "BORGCHART_MEMBER",
        "BORGCHART_PARENT",
        "BORGCHART_INCARNATION",
    ] {
        assert!(!env.contains_key(name), "{name} leaked: {env:?}");
    }
    assert!(!env["PATH"].contains("evil"), "{}", env["PATH"]);
    let text = fs::read_to_string(d.path("out/env")).unwrap();
    assert!(!text.contains(SLACK_SECRET) && !text.contains("caller-slack-token"));

    let argv = read_lines(&d.path("out/argv"));
    assert_eq!(argv[1..], ["alert", "--quiet", &id, "lead", "test", "0"]);
}

#[test]
fn cli_alert_persists_until_resolved() {
    let d = Deploy::new(true);
    d.up();
    let (rc, json) = d.json(&["bus", "alert", "test", "--json"]);
    assert_eq!(rc, 0, "{json}");
    let id = json["alert"]["id"].as_str().unwrap().to_string();
    // The team's root is the default member.
    assert_eq!(json["alert"]["member"], "lead");

    let (rc, listed) = d.json(&["bus", "alerts", "--json"]);
    assert_eq!(rc, 0);
    let alerts = listed["alerts"].as_array().unwrap();
    assert_eq!(alerts.len(), 1, "{listed}");
    assert_eq!(alerts[0]["id"], id.as_str());
    assert_eq!(alerts[0]["state"], "open");
    assert_eq!(alerts[0]["reason"], "test");
    assert_eq!(alerts[0]["invocation_count"], 1);
    assert_eq!(alerts[0]["invocations"][0]["outcome"], "invoked");
    assert_eq!(
        alerts[0]["invocations"][0]["output_tail"]
            .as_str()
            .unwrap()
            .trim(),
        "handed-off"
    );

    let (rc, resolved) = d.json(&["bus", "alert", "resolve", &id, "--json"]);
    assert_eq!(rc, 0, "{resolved}");
    assert_eq!(resolved["alert"]["state"], "resolved");
    let (_, listed) = d.json(&["bus", "alerts", "--json"]);
    assert_eq!(listed["alerts"].as_array().unwrap().len(), 0);
    let (_, listed) = d.json(&["bus", "alerts", "--all", "--json"]);
    assert_eq!(listed["alerts"][0]["resolved_by"], "operator");

    let (rc, missing) = d.json(&["bus", "alert", "resolve", "a-nope", "--json"]);
    assert_eq!(rc, 10);
    assert_eq!(missing["outcome"], "not-found");

    let out = d.brg_cmd(&["bus", "alerts"]).output().unwrap();
    assert_eq!(code(&out), 0);
}

#[test]
fn cli_failing_action_still_records_an_open_alert() {
    let d = Deploy::new(true);
    write_script(
        &d.path("team/plugins/notify/notify"),
        "#!/bin/sh\necho 'notifier down' >&2\nexit 3\n",
    );
    d.up();
    let (rc, json) = d.json(&["bus", "alert", "test", "--member", "helper", "--json"]);
    assert_eq!(rc, 1, "{json}");
    assert_eq!(json["outcome"], "alert-failed");
    assert_eq!(json["alert"]["state"], "open");
    assert_eq!(json["alert"]["invocations"][0]["status"], 3);
    let (_, listed) = d.json(&["bus", "alerts", "--json"]);
    assert_eq!(listed["alerts"][0]["member"], "helper");
}

#[test]
fn cli_without_an_alert_action_is_attention_unavailable() {
    let d = Deploy::new(false);
    d.up();
    let (rc, json) = d.json(&["bus", "alert", "test", "--json"]);
    assert_eq!(rc, 3, "{json}");
    assert_eq!(json["outcome"], "attention-unavailable");
    assert!(!d.path("out/argv").exists());
    let (_, listed) = d.json(&["bus", "alerts", "--json"]);
    assert_eq!(listed["alerts"].as_array().unwrap().len(), 0);
}

#[test]
fn cli_refusals() {
    let d = Deploy::new(true);
    // Not started yet.
    let (rc, json) = d.json(&["bus", "alert", "test", "--json"]);
    assert_eq!(rc, 3, "{json}");
    assert_eq!(json["outcome"], "not-configured");
    d.up();
    let (rc, json) = d.json(&["bus", "alert", "test", "--member", "nobody", "--json"]);
    assert_eq!(rc, 5, "{json}");
    assert_eq!(json["outcome"], "invalid");
    // `down` removes the plugin values: the action is unavailable, not a crash.
    let out = d.brg_cmd(&["down"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (rc, json) = d.json(&["bus", "alert", "test", "--json"]);
    assert_eq!(rc, 3, "{json}");
    assert_eq!(json["outcome"], "attention-unavailable");
    assert_eq!(json["alert"]["invocations"][0]["outcome"], "unavailable");
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

#[test]
fn desktop_alert_example_logs_the_alert() {
    let dir = tempfile::tempdir().unwrap();
    let team = dir.path().join("desktop-alert");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/desktop-alert"),
        &team,
    );
    // A PATH without notify-send, so the test never pops a notification.
    let bin = dir.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink("/bin/date", bin.join("date")).unwrap();
    fs::create_dir_all(dir.path().join("home")).unwrap();
    let brg = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_brg"))
            .env_clear()
            .env("PATH", &bin)
            .env("HOME", dir.path().join("home"))
            .env("BORGCHART_STATE_HOME", dir.path().join("state"))
            .env("BORGCHART_TMUX_SOCKET", "brg-alert-test-unused")
            .current_dir(&team)
            .args(args)
            .output()
            .unwrap()
    };
    let out = brg(&["validate"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = brg(&["up"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = brg(&["bus", "alert", "test", "--json"]);
    let json = parse(&out);
    assert_eq!(json["outcome"], "invoked", "{json}");
    let id = json["alert"]["id"].as_str().unwrap();
    let states: Vec<_> = fs::read_dir(dir.path().join("state"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("plugins/notify").is_dir())
        .collect();
    assert_eq!(states.len(), 1);
    let log = fs::read_to_string(states[0].path().join("plugins/notify/alerts.log")).unwrap();
    assert!(
        log.contains(&format!(
            "run=desktop-alert-1 id={id} member=lead reason=test pending=0"
        )),
        "{log}"
    );
}

#[test]
fn output_never_keeps_the_plugins_values() {
    let fx = Fixture::new("#!/bin/sh\necho \"token is $NOTIFY_TOKEN\"\nexit 1\n");
    let inv = fx.invoke(&Limits::default());
    assert_eq!(inv.outcome, AlertOutcome::Failed { status: Some(1) });
    assert!(!inv.output.contains(NOTIFY_TOKEN), "{}", inv.output);
    assert!(inv.output.contains("token is [redacted]"), "{}", inv.output);
}

#[test]
fn list_refuses_a_tampered_alert_file() {
    let dir = tempfile::tempdir().unwrap();
    let paths = run_paths(dir.path());
    alert::store::open(&paths, &request()).unwrap();
    let path = paths.alert("a-example-1");
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["member"] = "lead\u{1b}]0;owned\u{7}".into();
    fs::write(&path, value.to_string()).unwrap();
    let err = alert::store::list(&paths).unwrap_err();
    assert_eq!(err.code, borgchart::bus::outcome::Code::Corrupt, "{err}");

    // An ID that doesn't match its file name is refused too.
    value["member"] = "lead".into();
    value["id"] = "a-other".into();
    fs::write(&path, value.to_string()).unwrap();
    assert!(alert::store::list(&paths).is_err());
}

#[test]
fn cli_list_refuses_a_run_directory_others_can_write() {
    let d = Deploy::new(true);
    d.up();
    let (rc, _) = d.json(&["bus", "alert", "test", "--json"]);
    assert_eq!(rc, 0);
    let run = d.path("runs/example-run-1");
    fs::set_permissions(&run, fs::Permissions::from_mode(0o777)).unwrap();
    let (rc, json) = d.json(&["bus", "alerts", "--json"]);
    assert_ne!(rc, 0, "{json}");
    fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn overlapping_values_are_masked_together() {
    let fx = Fixture::new("#!/bin/sh\necho \"x-${A_VALUE}ijkl-y\"\n");
    // Two values that overlap in the output: masking one first must not
    // leave the rest of the other visible.
    fx.write_values("notify", &[("A_VALUE", "abcdefgh"), ("Z_LONG", "efghijkl")]);
    let inv = fx.invoke(&Limits::default());
    assert_eq!(inv.outcome, AlertOutcome::Invoked, "{inv:?}");
    for part in ["abcd", "efgh", "ijkl"] {
        assert!(!inv.output.contains(part), "{}", inv.output);
    }
    assert!(inv.output.starts_with("x-[redacted]"), "{}", inv.output);
    assert!(inv.output.trim_end().ends_with("-y"), "{}", inv.output);
}

#[test]
fn corrupt_alert_messages_never_echo_file_contents() {
    let dir = tempfile::tempdir().unwrap();
    let paths = run_paths(dir.path());
    alert::store::open(&paths, &request()).unwrap();
    let path = paths.alert("a-example-1");
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["id"] = "x\u{1b}[2J".into();
    fs::write(&path, value.to_string()).unwrap();
    let err = alert::store::list(&paths).unwrap_err();
    assert!(!err.message.contains('\u{1b}'), "{:?}", err.message);
    fs::remove_file(&path).unwrap();

    // A planted file name is refused without being printed.
    fs::write(paths.alerts_dir().join("evil\u{1b}[2J.json"), "{}").unwrap();
    let err = alert::store::list(&paths).unwrap_err();
    assert_eq!(err.code, borgchart::bus::outcome::Code::Corrupt);
    assert!(!err.message.contains('\u{1b}'), "{:?}", err.message);
}
