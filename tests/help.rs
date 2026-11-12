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
    for name in ["send", "list", "read", "ack", "wait", "integrity-check"] {
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
        for name in ["send", "inbox", "bus"] {
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
        vec!["msg", "check"],
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
        assert!(path.exists(), "missing shipped surface: {}", path.display());
        if path.is_dir() {
            for entry in std::fs::read_dir(path).unwrap() {
                check(&entry.unwrap().path());
            }
        } else if path.starts_with(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs"))
            && matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("html" | "js")
            )
        {
            // The website owns its generated surface separately.
        } else if let Ok(text) = std::fs::read_to_string(path) {
            for removed in [
                "inbox wait",
                "bus check",
                "msg check",
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
                "team folder",
                "team's files",
                "team.yaml",
                "start --team",
                "teams/",
                "{{teams/",
                "BORGCHART_INSTANCE",
                "--instance",
            ] {
                let word = |c: char| c.is_alphanumeric() || c == '_';
                let found = text.match_indices(removed).any(|(start, _)| {
                    !text[..start].chars().next_back().is_some_and(word)
                        && (removed.ends_with('/')
                            || !text[start + removed.len()..]
                                .chars()
                                .next()
                                .is_some_and(word))
                });
                assert!(!found, "{} names {removed}", path.display());
            }
            let install = regex::Regex::new(r"cargo install[^\n`]*\sborgchart\b").unwrap();
            let old_config = regex::Regex::new(r"(?m)^teams:").unwrap();
            assert!(
                !old_config.is_match(&text),
                "{} names teams:",
                path.display()
            );
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
    check(&root.join("templates"));
    check(&root.join("examples"));
    check(&root.join("docs"));
    check(&root.join("README.md"));
    check(&root.join("CONTRIBUTING.md"));
    check(&root.join("plugins"));
    check(&root.join("rules"));
}

#[test]
fn running_team_help_does_not_teach_retired_instance_or_copy_words() {
    let retired = regex::Regex::new(r"(?i)\b(instances?|cop(?:y|ies)|team-only)\b").unwrap();
    let copy_mode = regex::Regex::new(r"(?i)\bcopy mode\b").unwrap();
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
        let words = copy_mode.replace_all(&text, "");
        assert!(!retired.is_match(&words), "{args:?}: {text}");
        if args.first().is_some_and(|command| command == "msg") {
            // Bus after-help paragraphs are hand-wrapped; clap owns the tables/options.
            for line in text.lines().filter(|line| !line.starts_with(' ')) {
                assert!(line.chars().count() <= 80, "{args:?}: {line}");
            }
        }
    }
}

#[test]
fn shipped_prose_does_not_teach_retired_running_team_vocabulary() {
    fn check(root: &std::path::Path, path: &std::path::Path, retired: &regex::Regex) {
        if path.is_dir() {
            for entry in std::fs::read_dir(path).unwrap() {
                check(root, &entry.unwrap().path(), retired);
            }
            return;
        }
        let relative = path.strip_prefix(root).unwrap().to_str().unwrap();
        if (relative.starts_with("examples/") || relative.starts_with("plugins/"))
            && !matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("md" | "rst" | "txt" | "adoc")
            )
        {
            return;
        }
        let Ok(mut text) = std::fs::read_to_string(path) else {
            return;
        };
        // These name tmux's input mode, never a child team.
        for (file, phrase) in [
            ("docs/cli.md", "Copy mode"),
            ("docs/cli.md", "copy mode"),
            ("docs/format.md", "tmux copy mode"),
            ("docs/bus.md", "copy mode"),
        ] {
            if relative == file {
                text = text.replace(phrase, "");
            }
        }
        assert!(
            !retired.is_match(&text),
            "{relative} still teaches retired vocabulary"
        );
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let retired = regex::Regex::new(r"(?i)\b(instances?|cop(?:y|ies)|team-only)\b").unwrap();
    for dir in ["templates", "examples", "plugins", "rules"] {
        check(root, &root.join(dir), &retired);
    }
    for entry in std::fs::read_dir(root.join("docs")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "md") {
            check(root, &path, &retired);
        }
    }
    for file in ["README.md", "CONTRIBUTING.md"] {
        check(root, &root.join(file), &retired);
    }
}
