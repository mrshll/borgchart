//! `brg … --help` is how agents in a team learn the bus, so it must not
//! drift from the message command surface.

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
    let text = help(&["msg", "send"]);
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
}

#[test]
fn msg_exposes_all_message_commands() {
    let text = help(&["msg"]);
    for name in ["send", "list", "read", "ack", "wait", "integrity-check"] {
        assert!(
            text.lines()
                .any(|line| line.split_whitespace().next() == Some(name)),
            "missing {name}: {text}"
        );
    }
}

#[test]
fn top_level_help_groups_one_line_summaries_and_leaves_details_to_commands() {
    for flag in ["-h", "--help"] {
        let out = Command::new(env!("CARGO_BIN_EXE_brg"))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .arg(flag)
            .output()
            .unwrap();
        let text = String::from_utf8(out.stdout).unwrap();
        for heading in [
            "Team templates:",
            "Teams and members:",
            "Messages:",
            "Cleanup:",
        ] {
            assert!(text.lines().any(|l| l == heading), "{flag}: {text}");
        }
        let commands: Vec<_> = text
            .lines()
            .filter_map(|line| line.strip_prefix("  "))
            .filter_map(|line| line.split_whitespace().next())
            .collect();
        assert_eq!(
            commands.iter().filter(|&&name| name == "msg").count(),
            1,
            "{text}"
        );
        for name in ["templates", "secrets", "destroy"] {
            assert!(commands.contains(&name), "missing {name}: {text}");
        }
        assert!(!commands.contains(&"send"), "{text}");
        let long: Vec<_> = text.lines().filter(|l| l.chars().count() > 80).collect();
        assert!(long.is_empty(), "{flag}: lines over 80 columns: {long:?}");
        assert!(!text.contains("--repo"), "{flag}: details leaked: {text}");
    }
    assert!(help(&["create"]).contains("With `--repo`, name a clone"));
}

#[test]
fn template_secrets_and_destroy_commands_accept_help() {
    for name in ["templates", "secrets", "destroy"] {
        let out = Command::new(env!("CARGO_BIN_EXE_brg"))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .args([name, "--help"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{name}: {out:?}");
    }
}

#[test]
fn message_help_prose_fits_eighty_columns() {
    let command = regex::Regex::new(r"^  ([a-z][a-z-]*)\s{2,}\S").unwrap();
    let mut pending: Vec<Vec<String>> = vec![vec![]];
    while let Some(args) = pending.pop() {
        let text = help(&args.iter().map(String::as_str).collect::<Vec<_>>());
        for line in text.lines() {
            // `help` forwards to these pages and has no `--help` of its own.
            if let Some(found) = command.captures(line)
                && &found[1] != "help"
            {
                let mut child = args.clone();
                child.push(found[1].to_string());
                pending.push(child);
            }
        }
        if args.first().is_some_and(|command| command == "msg") {
            // Bus after-help paragraphs are hand-wrapped; clap owns the tables/options.
            for line in text.lines().filter(|line| !line.starts_with(' ')) {
                assert!(line.chars().count() <= 80, "{args:?}: {line}");
            }
        }
    }
}
