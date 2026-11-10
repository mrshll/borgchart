//! Shell completion (`BRG_COMPLETE=<shell> brg -- <words>`): which names it
//! offers, and that a tab press changes nothing and never waits.
//!
//! Run with `cargo test --features test-support`.
#![cfg(feature = "test-support")]

mod common;

mod instance_env;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use instance_env::*;

/// The candidates brg prints for `words`, the last being the word under
/// the cursor: one per line, without fish's help column.
fn complete(env: &Env, words: &[&str]) -> Vec<String> {
    let mut args = vec!["--", "brg"];
    args.extend_from_slice(words);
    let mut cmd = env.cmd(&args);
    cmd.env("BRG_COMPLETE", "fish");
    let out = cmd.output().unwrap();
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

#[test]
fn completion_offers_instances_kept_members_and_teams() {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    let lead = env.member(&inst, "lead");
    env.ok(&["create", &format!("{inst}/helper")]);
    let helper = env.member(&inst, "helper");
    env.ok(&["create", &format!("{inst}/scout")]);
    let scout = env.member(&inst, "scout");
    env.ok(&["stop", &format!("{inst}/{scout}")]);
    env.ok(&["delete", &format!("{inst}/{scout}")]);

    let targets = complete(&env, &["stop", ""]);
    for wanted in [
        inst.clone(),
        format!("{inst}/{lead}"),
        format!("{inst}/{helper}"),
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
    assert!(
        !instances.iter().any(|i| i.contains(&lead)),
        "a member offered as an instance: {instances:?}"
    );

    assert!(complete(&env, &["create", ""]).contains(&"oncall".to_string()));
    env.ok(&["stop", &inst]);
}

#[test]
fn completion_changes_nothing_and_never_waits() {
    let env = Env::new();
    let inst = env.create("oncall", "work/a");
    env.ok(&["stop", &inst]);
    let state = env.path("state");
    let before = snapshot(&state);

    let started = Instant::now();
    let mut cmd = env.cmd(&["--", "brg", "inbox", "wait", "--instance", ""]);
    cmd.env("BRG_COMPLETE", "fish")
        .env("BORGCHART_WAIT_MIN_MS", "5000");
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "completing an inbox wait waited {:?}",
        started.elapsed()
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains(&inst));

    complete(&env, &["stop", ""]);
    complete(&env, &["attach", ""]);
    complete(&env, &["create", ""]);
    assert_eq!(snapshot(&state), before, "completion changed brg's state");
}
