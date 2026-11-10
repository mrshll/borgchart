//! Shell completion (`BRG_COMPLETE=<shell> brg -- <words>`): which names it
//! offers, and that a tab press changes nothing and never waits.
//!
//! Run with `cargo test --features test-support`.
#![cfg(feature = "test-support")]

mod common;

mod instance_env;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime};

use instance_env::*;

/// The candidates brg prints for `words`, the last being the word under
/// the cursor: one per line, without fish's help column. A completion must
/// answer within a few seconds; one that waits (on a lock, or the inbox
/// wait's back-off) is killed and fails the test.
fn complete(env: &Env, words: &[&str]) -> Vec<String> {
    let mut args = vec!["--", "brg"];
    args.extend_from_slice(words);
    let mut child = env
        .cmd(&args)
        .env("BRG_COMPLETE", "fish")
        .env("BORGCHART_WAIT_MIN_MS", "5000")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > Duration::from_secs(3) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("completing {words:?} waited over 3 seconds");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "completion of {words:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|line| line.split('\t').next().unwrap_or_default().to_string())
        .collect()
}

/// Every path under `root` with its size and modification time.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, (u64, SystemTime)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                stack.push(entry.path());
            }
            out.insert(entry.path(), (meta.len(), meta.modified().unwrap()));
        }
    }
    out
}

/// The test team plus `dev`, a team role whose copies run the `dev` team.
const DEPARTMENT: &str = r#"
bus: {}
roles:
  lead:
    prompt: lead.md
    cwd: lead
  helper:
    prompt: helper.md
    vendor: codex
    parent: lead
    start: false
    cwd: lead
  scout:
    prompt: scout.md
    start: false
    cwd: lead
  dev:
    team: dev
    parent: lead
    start: false
    cwd: "dev/{member_id}"
"#;

#[test]
fn completion_offers_instances_kept_members_and_teams() {
    let env = Env::new();
    env.write("teams/oncall/team.yaml", DEPARTMENT);
    env.team("teams/dev");
    env.write(
        "teams/dev/team.yaml",
        "bus: {}\nroles:\n  lead: {prompt: lead.md, cwd: repo}\n",
    );
    std::fs::create_dir(env.path("work/a/lead")).unwrap();
    let inst = env.create("oncall", "work/a");
    let lead = env.member(&inst, "lead");
    env.ok(&["create", &format!("{inst}/helper")]);
    let helper = env.member(&inst, "helper");
    env.ok(&["create", &format!("{inst}/scout")]);
    let scout = env.member(&inst, "scout");
    env.ok(&["stop", &format!("{inst}/{scout}")]);
    env.ok(&["delete", &format!("{inst}/{scout}")]);
    let out = env.ok(&["create", &format!("{inst}/dev")]);
    let copy = stdout(&out).split_whitespace().next().unwrap().to_string();
    std::fs::create_dir(env.path(&format!(
        "work/a/dev/{}/repo",
        copy.rsplit('/').next().unwrap()
    )))
    .unwrap();
    env.ok(&["start", &copy]);
    env.wait_for("copy lead", || !env.member(&copy, "lead").is_empty());
    let copy_lead = env.member(&copy, "lead");

    let targets = complete(&env, &["stop", ""]);
    for wanted in [
        inst.clone(),
        format!("{inst}/{lead}"),
        format!("{inst}/{helper}"),
        copy.clone(),
        format!("{copy}/{copy_lead}"),
    ] {
        assert!(targets.contains(&wanted), "{wanted} in {targets:?}");
    }
    assert!(
        !targets.iter().any(|t| t.contains(&scout)),
        "deleted member offered: {targets:?}"
    );

    let members = complete(&env, &["attach", ""]);
    assert!(members.contains(&format!("{inst}/{lead}")), "{members:?}");
    assert!(
        !members.contains(&inst),
        "an instance offered as a member: {members:?}"
    );

    let instances = complete(&env, &["inbox", "--instance", ""]);
    assert!(instances.contains(&inst), "{instances:?}");
    let cleanup = complete(&env, &["cleanup", ""]);
    assert!(
        cleanup.contains(&inst) && cleanup.contains(&copy),
        "{cleanup:?}"
    );
    assert!(
        !instances.iter().any(|i| i.contains(&lead)),
        "a member offered as an instance: {instances:?}"
    );

    assert!(complete(&env, &["create", ""]).contains(&"oncall".to_string()));
    assert_eq!(complete(&env, &["validate", "./te"]), ["./teams/"]);
    env.ok(&["stop", &inst]);
}

#[test]
fn completion_changes_nothing_and_never_waits() {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    env.ok(&["stop", &inst]);
    let state = env.path("state");
    // Hold the registry lock, as a lifecycle change would: a completion
    // that took it would wait here.
    let lock = std::fs::File::open(state.join("registry.lock")).unwrap();
    lock.lock().unwrap();
    let before = snapshot(&state);

    assert!(complete(&env, &["inbox", "wait", "--instance", ""]).contains(&inst));
    complete(&env, &["stop", ""]);
    complete(&env, &["attach", ""]);
    complete(&env, &["create", ""]);
    assert_eq!(snapshot(&state), before, "completion changed brg's state");
    drop(lock);
}
