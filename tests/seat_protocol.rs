//! Real pipes and process cleanup belong outside the no-process library tier.
use borgchart::bounded_process::{Error, Stream};
use borgchart::seat::{policy::Provider, protocol};
use serde_json::json;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

// Do not fork another fixture while a test has its script open for writing:
// the fork can briefly inherit that descriptor before CLOEXEC takes effect.
static PROCESS_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn protocol_exchange_is_interactive_and_bounded() {
    let _guard = PROCESS_TEST.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let executable = temp.path().join("fake-usage");
    let run = |script: &str, duration| {
        fs::write(&executable, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        protocol::read(Provider::Codex, &executable, Instant::now() + duration)
    };
    let script = r#"
        IFS= read -r init || exit 1
        case "$init" in *initialize*) ;; *) exit 2;; esac
        printf '%s\n' '{"id":1,"result":{}}'
        IFS= read -r initialized || exit 3
        case "$initialized" in *initialized*) ;; *) exit 4;; esac
        IFS= read -r usage || exit 5
        case "$usage" in *account/rateLimits/read*) ;; *) exit 6;; esac
        printf '%s\n' '{"id":2,"result":{"ok":true}}'
        sleep 5
    "#;
    assert_eq!(
        run(script, Duration::from_secs(2)).unwrap(),
        json!({"ok":true})
    );
    assert_eq!(
        run("sleep 5", Duration::from_millis(100)).unwrap_err(),
        Error::Deadline
    );
    assert_eq!(
        run("exit 0", Duration::from_secs(2)).unwrap_err(),
        Error::Io(std::io::ErrorKind::UnexpectedEof)
    );
    assert_eq!(
        run("printf 'DO_NOT_LOG\\n'", Duration::from_secs(2)).unwrap_err(),
        Error::Io(std::io::ErrorKind::InvalidData)
    );
    // Include all notification bytes in the cap, not just matching replies.
    assert_eq!(
        run("yes '{\"method\":\"notice\"}'", Duration::from_secs(8)).unwrap_err(),
        Error::OutputLimit(Stream::Stdout)
    );
    assert_eq!(
        run("yes DO_NOT_LOG >&2", Duration::from_secs(8)).unwrap_err(),
        Error::OutputLimit(Stream::Stderr)
    );
}

#[test]
fn completed_dialogue_kills_descendants_that_hold_pipes() {
    let _guard = PROCESS_TEST.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let executable = temp.path().join("fake-usage");
    let marker = temp.path().join("late-marker");
    let quoted = format!("'{}'", marker.display().to_string().replace('\'', "'\\''"));
    let script = format!(
        r#"#!/bin/sh
        IFS= read -r init
        printf '%s\n' '{{"id":1,"result":{{}}}}'
        IFS= read -r initialized
        IFS= read -r usage
        (sleep 0.6; printf late > {quoted}) &
        printf '%s\n' '{{"id":2,"result":{{"ok":true}}}}'
        sleep 5
    "#
    );
    fs::write(&executable, script).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        protocol::read(
            Provider::Codex,
            &executable,
            Instant::now() + Duration::from_secs(2)
        )
        .unwrap(),
        json!({"ok":true})
    );
    std::thread::sleep(Duration::from_millis(900));
    assert!(
        !marker.exists(),
        "completed protocol left its descendant running"
    );
}
#[test]
fn a_closed_request_pipe_cannot_authorize_an_unsolicited_reply() {
    let _guard = PROCESS_TEST.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let executable = temp.path().join("fake-usage");
    fs::write(
        &executable,
        r#"#!/bin/sh
        IFS= read -r init
        exec 0<&-
        printf '%s\n' '{"id":1,"result":{}}'
        sleep 0.1
        printf '%s\n' '{"id":2,"result":{"ok":true}}'
    "#,
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        protocol::read(
            Provider::Codex,
            &executable,
            Instant::now() + Duration::from_secs(2)
        )
        .unwrap_err(),
        Error::Io(std::io::ErrorKind::BrokenPipe)
    );
}
