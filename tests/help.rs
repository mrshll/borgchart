//! `brg … --help` is how agents in a team learn the bus, so it must not
//! drift from the send/record command surface.

use std::process::Command;

fn help(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_brg"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .args(args)
        .arg("--help")
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}: {:?}", out);
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn send_help_describes_identity_references_and_committed_delivery() {
    let text = help(&["send"]);
    for word in [
        "--external",
        "--reply-to",
        "--from",
        "--json",
        "id",
        "delivery",
        "reason",
    ] {
        assert!(text.contains(word), "send help omits {word}: {text}");
    }
    assert!(text.contains("outside author"), "{text}");
    assert!(text.contains("fenced identity"), "{text}");
    assert!(!text.contains("--record"), "old draft API remains: {text}");
}

#[test]
fn msg_keeps_read_and_ack_but_has_no_publish_command() {
    let text = help(&["msg"]);
    assert!(text.contains("read") && text.contains("ack"), "{text}");
    assert!(!text.contains("publish"), "{text}");
    let out = Command::new(env!("CARGO_BIN_EXE_brg"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .args(["msg", "publish", "--help"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "removed command still accepted: {out:?}"
    );
}
