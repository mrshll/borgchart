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
    assert!(!text.contains("--record"), "old draft API remains: {text}");
}

#[test]
fn msg_exposes_all_message_commands_without_publish() {
    let text = help(&["msg"]);
    for name in ["send", "list", "read", "ack", "wait", "check"] {
        assert!(
            text.lines()
                .any(|line| line.split_whitespace().next() == Some(name)),
            "missing {name}: {text}"
        );
    }
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
            "Team folders:",
            "Instances and members:",
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
        for name in ["send", "inbox", "bus", "ls", "secret", "reset"] {
            assert!(!commands.contains(&name), "leftover {name}: {text}");
        }
        let long: Vec<_> = text.lines().filter(|l| l.chars().count() > 80).collect();
        assert!(long.is_empty(), "{flag}: lines over 80 columns: {long:?}");
        assert!(!text.contains("--repo"), "{flag}: details leaked: {text}");
    }
    assert!(help(&["create"]).contains("With `--repo`, name a clone"));
}

#[test]
fn renamed_commands_parse_and_removed_spellings_are_refused() {
    for (name, code) in [
        ("ls", 2),
        ("secret", 2),
        ("reset", 2),
        ("templates", 0),
        ("secrets", 0),
        ("destroy", 0),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_brg"))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .args([name, "--help"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(code), "{name}: {out:?}");
    }
}

#[test]
fn removed_message_commands_and_force_flag_are_refused() {
    for args in [
        vec!["send", "someone", "hello"],
        vec!["inbox"],
        vec!["inbox", "wait"],
        vec!["bus", "check"],
        vec!["msg", "send", "someone", "hello", "--force"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_brg"))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .args(&args)
            // Help would succeed if a removed spelling were still accepted.
            .arg("--help")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
    }
}

#[test]
fn shipped_roles_and_docs_do_not_instruct_removed_commands() {
    fn check(path: &std::path::Path) {
        if path.is_dir() {
            for entry in std::fs::read_dir(path).unwrap() {
                check(&entry.unwrap().path());
            }
        } else if let Ok(text) = std::fs::read_to_string(path) {
            for removed in [
                "inbox wait",
                "bus check",
                "\"$BORGCHART_BIN\" send",
                "\"$BORGCHART_BIN\" inbox",
                "brg inbox",
                "brg send",
                "brg bus",
                "brg ls",
                "brg secret",
                "brg reset",
                "\"$BORGCHART_BIN\" ls",
                "\"$BORGCHART_BIN\" secret",
                "\"$BORGCHART_BIN\" reset",
            ] {
                let word = |c: char| c.is_alphanumeric() || c == '_';
                let found = text.match_indices(removed).any(|(start, _)| {
                    !text[..start].chars().next_back().is_some_and(word)
                        && !text[start + removed.len()..]
                            .chars()
                            .next()
                            .is_some_and(word)
                });
                assert!(!found, "{} names {removed}", path.display());
            }
            let install = regex::Regex::new(r"cargo install[^\n`]*\sborgchart\b").unwrap();
            let mut code_block = false;
            for line in text.lines() {
                if line.trim_start().starts_with("```") {
                    code_block = !code_block;
                }
                let runs = line.contains("`borgchart ")
                    || (code_block && line.trim_start().starts_with("borgchart "));
                let installs = install.is_match(line) && !line.contains("--bin brg");
                assert!(
                    !runs && !installs,
                    "{} runs or installs the removed binary: {line}",
                    path.display()
                );
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    check(&root.join("teams"));
    check(&root.join("examples"));
    check(&root.join("docs"));
    check(&root.join("README.md"));
    check(&root.join("CONTRIBUTING.md"));
    check(&root.join("plugins"));
    check(&root.join("rules"));
}
