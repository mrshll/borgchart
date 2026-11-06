//! The fast tier's guard (`.githooks/no-processes`): a fast test that starts
//! tmux or another process must fail the tier, even when the test itself
//! tolerated the failure.

mod common;

use std::path::PathBuf;
use std::process::Output;

fn guarded(script: &str) -> Output {
    let guard = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".githooks/no-processes");
    common::command(guard)
        .args(["/bin/sh", "-c", script])
        .output()
        .unwrap()
}

#[test]
fn a_tolerated_call_to_a_refused_program_fails_the_tier() {
    // Catches: a stand-in that only refuses, so a test that ignores the
    // error (as liveness does with an unreachable server) passes the tier.
    for program in ["tmux", "ps", "sh", "sleep", "claude", "codex"] {
        let out = guarded(&format!("{program} -V >/dev/null 2>&1 || true; exit 0"));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{program}: {stderr}");
        assert!(stderr.contains(&format!("\n{program} -V\n")), "{stderr}");
    }
}

#[test]
fn a_command_that_starts_nothing_keeps_its_own_exit_status() {
    // Catches: a guard that always passes, or that hides the tests' failure.
    assert_eq!(guarded("exit 0").status.code(), Some(0));
    assert_eq!(guarded("exit 3").status.code(), Some(3));
}
