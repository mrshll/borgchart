//! Reading a member's state from its screen.
//!
//! Each vendor has a small rule file (bundled, or overridden in
//! `$XDG_CONFIG_HOME/borgchart/rules/<vendor>.yaml`). A rule matches the
//! terminal title and/or the bottom lines of the visible screen; the
//! highest-priority match wins, and no match means `unknown`.

use std::path::PathBuf;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::team::Vendor;
use crate::yaml;

const CLAUDE_RULES: &str = include_str!("../rules/claude.yaml");
const CODEX_RULES: &str = include_str!("../rules/codex.yaml");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Idle,
    Working,
    Blocked,
    Unknown,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Working => "working",
            State::Blocked => "blocked",
            State::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleFileRaw {
    vendor: Vendor,
    activity: Option<ActivityRaw>,
    rules: Vec<RuleRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivityRaw {
    title: String,
    #[serde(default)]
    none: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleRaw {
    id: String,
    state: State,
    priority: i32,
    title: Option<String>,
    bottom: Option<usize>,
    #[serde(default)]
    all: Vec<String>,
    #[serde(default)]
    any: Vec<String>,
    #[serde(default)]
    none: Vec<String>,
}

#[derive(Debug)]
pub struct Rule {
    pub id: String,
    pub state: State,
    pub priority: i32,
    title: Option<Regex>,
    bottom: usize,
    all: Vec<Regex>,
    any: Vec<Regex>,
    none: Vec<Regex>,
}

#[derive(Debug)]
pub struct RuleSet {
    pub vendor: Vendor,
    pub source: String,
    rules: Vec<Rule>,
    /// Where the terminal title says what the session is about: its first
    /// capture group, unless that matches one of the second's patterns.
    activity: Option<(Regex, Vec<Regex>)>,
}

/// The longest activity `status` shows, in characters.
const ACTIVITY_MAX: usize = 60;

/// The result of reading one screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reading {
    pub state: State,
    /// The rule that decided, if any.
    pub rule: Option<String>,
}

fn compile(pattern: &str, id: &str) -> Result<Regex, String> {
    Regex::new(&format!("(?m){pattern}")).map_err(|e| format!("rule `{id}`: bad pattern: {e}"))
}

pub fn parse(text: &str, source: &str) -> Result<RuleSet, String> {
    let raw: RuleFileRaw = yaml::parse(text).map_err(|e| format!("{source}: {e}"))?;
    let mut rules = Vec::new();
    for r in raw.rules {
        if r.title.is_none() && r.all.is_empty() && r.any.is_empty() {
            return Err(format!(
                "{source}: rule `{}` needs `title`, `all` or `any`",
                r.id
            ));
        }
        if (!r.all.is_empty() || !r.any.is_empty() || !r.none.is_empty()) && r.bottom.is_none() {
            return Err(format!(
                "{source}: rule `{}` matches lines, so it needs `bottom`",
                r.id
            ));
        }
        let compile_all = |v: &[String]| {
            v.iter()
                .map(|p| compile(p, &r.id))
                .collect::<Result<Vec<_>, _>>()
        };
        rules.push(Rule {
            title: r.title.as_deref().map(|t| compile(t, &r.id)).transpose()?,
            bottom: r.bottom.unwrap_or(0),
            all: compile_all(&r.all)?,
            any: compile_all(&r.any)?,
            none: compile_all(&r.none)?,
            id: r.id,
            state: r.state,
            priority: r.priority,
        });
    }
    rules.sort_by_key(|r| std::cmp::Reverse(r.priority));
    let activity = raw
        .activity
        .map(|a| {
            let title = compile(&a.title, "activity")?;
            if title.captures_len() < 2 {
                return Err("`activity.title` needs a capture group".to_string());
            }
            let none = a
                .none
                .iter()
                .map(|p| compile(p, "activity"))
                .collect::<Result<Vec<_>, _>>()?;
            Ok((title, none))
        })
        .transpose()
        .map_err(|e| format!("{source}: {e}"))?;
    Ok(RuleSet {
        activity,
        vendor: raw.vendor,
        source: source.to_string(),
        rules,
    })
}

fn override_path(vendor: Vendor) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(
        base.join("borgchart/rules")
            .join(format!("{}.yaml", vendor.as_str())),
    )
}

/// The rules for a vendor: your override file if present, else the bundled one.
pub fn load(vendor: Vendor) -> Result<RuleSet, String> {
    if let Some(path) = override_path(vendor)
        && path.is_file()
    {
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        return parse(&text, &path.display().to_string());
    }
    match vendor {
        Vendor::Claude => parse(CLAUDE_RULES, "bundled claude rules"),
        Vendor::Codex => parse(CODEX_RULES, "bundled codex rules"),
    }
}

impl RuleSet {
    /// What the vendor's terminal title says the session is about, if its
    /// rules know where to find that: one line, no control characters, at
    /// most [`ACTIVITY_MAX`] characters. The agent's own words, so advisory.
    pub fn activity(&self, title: &str) -> Option<String> {
        let (pattern, none) = self.activity.as_ref()?;
        let text: String = pattern
            .captures(title)?
            .get(1)?
            .as_str()
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        let text = text.trim();
        if text.is_empty() || none.iter().any(|r| r.is_match(text)) {
            return None;
        }
        Some(if text.chars().count() > ACTIVITY_MAX {
            let cut: String = text.chars().take(ACTIVITY_MAX - 1).collect();
            format!("{}…", cut.trim_end())
        } else {
            text.to_string()
        })
    }

    pub fn read(&self, title: &str, screen: &str) -> Reading {
        let lines: Vec<&str> = screen.lines().collect();
        let end = lines
            .iter()
            .rposition(|l| !l.trim().is_empty())
            .map(|i| i + 1)
            .unwrap_or(0);
        for rule in &self.rules {
            if let Some(t) = &rule.title
                && !t.is_match(title)
            {
                continue;
            }
            if rule.bottom > 0 {
                let start = end.saturating_sub(rule.bottom);
                let region = lines[start..end].join("\n");
                if !rule.all.iter().all(|r| r.is_match(&region)) {
                    continue;
                }
                if !rule.any.is_empty() && !rule.any.iter().any(|r| r.is_match(&region)) {
                    continue;
                }
                if rule.none.iter().any(|r| r.is_match(&region)) {
                    continue;
                }
            }
            return Reading {
                state: rule.state,
                rule: Some(rule.id.clone()),
            };
        }
        Reading {
            state: State::Unknown,
            rule: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE_IDLE: &str = include_str!("../tests/fixtures/screens/claude-idle.txt");
    const CLAUDE_WORKING: &str = include_str!("../tests/fixtures/screens/claude-working.txt");
    const CLAUDE_PERMISSION: &str = include_str!("../tests/fixtures/screens/claude-permission.txt");
    const CLAUDE_TRUST: &str = include_str!("../tests/fixtures/screens/claude-trust.txt");
    const CLAUDE_SPOOF: &str = include_str!("../tests/fixtures/screens/claude-spoof.txt");
    const CODEX_IDLE: &str = include_str!("../tests/fixtures/screens/codex-idle.txt");
    const CODEX_WORKING: &str = include_str!("../tests/fixtures/screens/codex-working.txt");
    const CODEX_APPROVAL: &str = include_str!("../tests/fixtures/screens/codex-approval.txt");

    fn claude() -> RuleSet {
        parse(CLAUDE_RULES, "claude").unwrap()
    }
    fn codex() -> RuleSet {
        parse(CODEX_RULES, "codex").unwrap()
    }

    #[test]
    fn activity_comes_from_the_title() {
        let r = claude();
        for title in ["✳ Review open GitHub issues", "⠐ Review open GitHub issues"] {
            assert_eq!(
                r.activity(title).as_deref(),
                Some("Review open GitHub issues")
            );
        }
        // No topic yet, no title, or another program's title.
        for title in ["✳ Claude Code", "", "✳ ", "bash"] {
            assert_eq!(r.activity(title), None, "{title:?}");
        }
        let long = format!("✳ {}", "word ".repeat(40));
        let cut = r.activity(&long).unwrap();
        assert_eq!(cut.chars().count(), ACTIVITY_MAX);
        assert!(cut.ends_with('…'));
        assert_eq!(r.activity("✳ a\u{1b}[31mb").as_deref(), Some("a[31mb"));
        // Codex's title names the folder, not the work.
        assert_eq!(codex().activity("⠸ example"), None);
        assert!(
            parse("vendor: claude\nactivity: {title: '^x'}\nrules: []", "t")
                .unwrap_err()
                .contains("capture group")
        );
    }

    #[test]
    fn claude_screens() {
        let r = claude();
        assert_eq!(
            r.read("✳ Test agent standby", CLAUDE_IDLE).state,
            State::Idle
        );
        assert_eq!(
            r.read("◐ Test agent standby", CLAUDE_WORKING).state,
            State::Working
        );
        // The permission dialog keeps the idle title; the dialog must win.
        assert_eq!(
            r.read("✳ Slackbot read 5", CLAUDE_PERMISSION).state,
            State::Blocked
        );
        assert_eq!(r.read("", CLAUDE_TRUST).state, State::Blocked);
    }

    /// Real Claude Code 2.1.318 screens across settings. In this version the
    /// title keeps `✳` during a turn, and the footer only sometimes says
    /// `esc to interrupt`, so a turn must be read from the spinner line.
    #[test]
    fn claude_screens_across_configurations() {
        let fixtures = |name: &str| {
            std::fs::read_to_string(format!(
                "{}/tests/fixtures/screens/{name}",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap()
        };
        let title = "✳ English number words one to one hundred twenty";
        let cases = [
            // Default (manual) mode, vim off, 120 columns.
            ("claude-working-manual.txt", State::Working),
            ("claude-working-manual-start.txt", State::Working),
            // Vim insert mode, 80 columns: no `esc to interrupt`.
            ("claude-working-vim-insert-80.txt", State::Working),
            // Vim normal mode, plan mode, a live thinking line above.
            ("claude-working-vim-normal-plan.txt", State::Working),
            // Auto mode with vim insert: no `esc to interrupt`, a tool running.
            ("claude-working-auto-tool.txt", State::Working),
            // Auto mode, vim insert and a background agent, as first reported.
            ("claude-working-auto-agent.txt", State::Working),
            // Reduced motion, custom spinner verb, accept-edits mode, 80 columns.
            ("claude-working-reduced-motion-80.txt", State::Working),
            // Auto mode, vim insert, 80x24, a 4-line and a 7-line draft (Claude
            // caps the box height and scrolls inside it), and two pastes.
            ("claude-working-composer-short-80.txt", State::Working),
            ("claude-working-composer-tall-80.txt", State::Working),
            ("claude-working-paste-80.txt", State::Working),
            // The first frame of a turn, while a prompt hook runs.
            ("claude-working-hook-plan.txt", State::Working),
            ("claude-idle-fresh.txt", State::Idle),
            ("claude-idle-composer-tall-80.txt", State::Idle),
            // Esc while thinking: the turn is dropped and the prompt restored.
            ("claude-idle-interrupted-restored.txt", State::Idle),
            ("claude-idle-vim-insert-80.txt", State::Idle),
            ("claude-idle-plan.txt", State::Idle),
            ("claude-idle-auto.txt", State::Idle),
            ("claude-idle-reduced-motion-80.txt", State::Idle),
        ];
        let r = claude();
        for (name, want) in cases {
            let reading = r.read(title, &fixtures(name));
            assert_eq!(reading.state, want, "{name}");
            if want == State::Working {
                assert_eq!(reading.rule.as_deref(), Some("working-spinner"), "{name}");
            }
        }
    }

    #[test]
    fn claude_text_ending_in_an_ellipsis_is_not_a_spinner() {
        let rule = "─".repeat(40);
        let screens = [
            // A finished turn: the `done` line sits between reply and box.
            format!(
                "● Let me check…\n\n✻ Worked for 3s · done 9:00 AM\n\n{rule}\n❯ \n{rule}\n  ? for shortcuts\n"
            ),
            // A draft in the input box that ends in `…`.
            format!(
                "✻ Worked for 3s · done 9:00 AM\n\n{rule}\n❯ wait for it…\n{rule}\n  ? for shortcuts\n"
            ),
            // A quoted spinner inside a tool result is indented.
            format!(
                "● Read(notes.txt)\n  ⎿  ✶ Thinking… (3s · ↓ 9 tokens)\n\n{rule}\n❯ \n{rule}\n  ? for shortcuts\n"
            ),
        ];
        for screen in &screens {
            assert_eq!(
                claude().read("✳ Example", screen).state,
                State::Idle,
                "{screen}"
            );
        }
    }

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/screens/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    /// The screen with `n` more draft lines in its input box.
    fn grow_composer(screen: &str, n: usize) -> String {
        let lines: Vec<&str> = screen.lines().collect();
        let at = lines.iter().rposition(|l| l.starts_with('❯')).unwrap();
        let mut out: Vec<String> = lines[..=at].iter().map(|l| l.to_string()).collect();
        out.extend((0..n).map(|i| format!("  continued draft {i}")));
        out.extend(lines[at + 1..].iter().map(|l| l.to_string()));
        out.join("\n") + "\n"
    }

    /// Only the last `rows` non-blank-trimmed lines, as a shorter pane shows.
    fn clip(screen: &str, rows: usize) -> String {
        let lines: Vec<&str> = screen.trim_end().lines().collect();
        lines[lines.len().saturating_sub(rows)..].join("\n") + "\n"
    }

    #[test]
    fn claude_footer_text_in_a_draft_is_not_the_footer() {
        let rule = "─".repeat(80);
        let screen = format!(
            "✻ Worked for 3s · done 9:00 AM\n\n{rule}\n❯ Explain this UI text:\n  esc to interrupt · example\n{rule}\n  -- INSERT -- ⏸ manual mode on\n"
        );
        assert_eq!(claude().read("✳ Example", &screen).state, State::Idle);
    }

    #[test]
    fn claude_real_footers_without_a_spinner_read_working() {
        // Real footers with items after `esc to interrupt`; the spinner line
        // is removed so only the footer can decide.
        let manual = fixture("claude-working-manual.txt");
        let footer = "  ⏸ manual mode on · esc to interrupt · ← 1 agent";
        assert!(manual.contains(footer));
        let screens = [
            manual.clone(),
            manual.replace(
                footer,
                "  ⏸ manual mode on · PR #42 · esc to interrupt · ← 1 agent",
            ),
            manual.replace(
                footer,
                "  ⏵⏵ accept edits on (shift+tab to cycle) · esc to interrupt",
            ),
            fixture("claude-working-vim-normal-plan.txt"),
            fixture("claude-working-reduced-motion-80.txt"),
        ];
        for screen in screens {
            let screen: String = screen
                .lines()
                .map(|l| {
                    if l.contains('…') && !l.starts_with(' ') {
                        ""
                    } else {
                        l
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            let reading = claude().read("✳ Example", &screen);
            assert_eq!(reading.state, State::Working, "{screen}");
            assert_eq!(reading.rule.as_deref(), Some("working-footer"), "{screen}");
        }
    }

    /// `screen` with the composer box's top rule ending in a session's
    /// name, as Claude Code draws it for a named session.
    fn named_box(screen: &str, name: &str) -> String {
        let lines: Vec<&str> = screen.lines().collect();
        let top = lines
            .iter()
            .rposition(|l| l.starts_with('❯'))
            .and_then(|i| i.checked_sub(1))
            .expect("a composer box");
        let width = lines[top].chars().count();
        let label = format!(" {name} ─");
        let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        out[top] = format!("{}{label}", "─".repeat(width - label.chars().count()));
        out.join("\n") + "\n"
    }

    #[test]
    fn claude_named_sessions_read_like_unnamed_ones() {
        // A named session's title is its name, with the idle or a spinner glyph.
        let idle = named_box(&fixture("claude-idle-vim-insert-80.txt"), "demo-notes");
        assert!(idle.contains("─ demo-notes ─\n❯"), "{idle}");
        let reading = claude().read("✳ demo-notes", &idle);
        assert_eq!(reading.state, State::Idle);
        assert_eq!(reading.rule.as_deref(), Some("idle-composer"));
        // A reduced-motion `● Verb…` with no timer stays ambiguous.
        let early = named_box(
            &fixture("claude-working-reduced-motion-80.txt")
                .replace(
                    "● Pondering deeply… (1s · ↓ 2 tokens)",
                    "● Pondering deeply…",
                )
                .replace(" · esc to interrupt", ""),
            "demo-notes",
        );
        assert_eq!(claude().read("✳ demo-notes", &early).state, State::Unknown);
        // Working reads from the screen, not only the title: a spinner, and
        // a footer with no spinner line.
        let spinner = named_box(&fixture("claude-working-manual.txt"), "demo-notes");
        let reading = claude().read("✳ demo-notes", &spinner);
        assert_eq!(reading.state, State::Working);
        assert_eq!(reading.rule.as_deref(), Some("working-spinner"));
        let footer: String = spinner
            .lines()
            .filter(|l| !(l.contains('…') && !l.starts_with(' ')))
            .map(|l| format!("{l}\n"))
            .collect();
        let reading = claude().read("✳ demo-notes", &footer);
        assert_eq!(reading.state, State::Working);
        assert_eq!(reading.rule.as_deref(), Some("working-footer"));
        // A label is not a licence for any text: the rule still starts and
        // ends in dashes.
        let broken = idle.replace("─ demo-notes ─\n❯", "─ demo-notes\n❯");
        assert_eq!(claude().read("✳ demo-notes", &broken).state, State::Unknown);
    }

    #[test]
    fn claude_tall_composers() {
        // Vim insert + auto mode, 120 and 80 columns; vim insert idle.
        for (name, want) in [
            ("claude-working-auto-agent.txt", State::Working),
            ("claude-working-composer-short-80.txt", State::Working),
            ("claude-idle-vim-insert-80.txt", State::Idle),
        ] {
            let short = fixture(name);
            let tall = grow_composer(&short, 40);
            assert_eq!(
                claude().read("✳ Example", &short).state,
                want,
                "{name} short"
            );
            // The whole screen, as on a taller pane.
            assert_eq!(claude().read("✳ Example", &tall).state, want, "{name} tall");
            // A 40-row pane: the spinner/status line above the box is cut
            // off, then the box's top rule too. Neither is proof of idle.
            let rows_from_top_rule = tall.trim_end().lines().count()
                - tall.lines().position(|l| l.starts_with('─')).unwrap();
            for rows in [rows_from_top_rule, rows_from_top_rule - 1] {
                assert_eq!(
                    claude().read("✳ Example", &clip(&tall, rows)).state,
                    State::Unknown,
                    "{name} clipped to {rows}"
                );
            }
        }
    }

    #[test]
    fn claude_reduced_motion_dot_needs_more_than_an_ellipsis() {
        let rule = "─".repeat(80);
        // A reply line that ends in `…`, then an interrupt, and no done line.
        let interrupted = format!(
            "❯ Check the logs\n\n● Let me check…\n  ⎿  Interrupted · What should Claude do instead?\n\n{rule}\n❯ \n{rule}\n  ⏸ manual mode on\n"
        );
        assert_eq!(
            claude().read("✳ Example", &interrupted).state,
            State::Unknown
        );
        // The first reduced-motion frame (no timer yet) without the footer
        // hint is ambiguous too.
        let early = fixture("claude-working-reduced-motion-80.txt")
            .replace(
                "● Pondering deeply… (1s · ↓ 2 tokens)",
                "● Pondering deeply…",
            )
            .replace(" · esc to interrupt", "");
        assert_eq!(claude().read("✳ Example", &early).state, State::Unknown);
    }

    #[test]
    fn claude_message_text_cannot_fake_a_dialog() {
        // A Slack message quoting dialog text, shown inside a tool result.
        assert_eq!(claude().read("✳ Triage", CLAUDE_SPOOF).state, State::Idle);
    }

    #[test]
    fn codex_screens() {
        let r = codex();
        assert_eq!(
            r.read("Wait for instructions | m2", CODEX_IDLE).state,
            State::Idle
        );
        assert_eq!(
            r.read("⠸ Wait for instructions | m2", CODEX_WORKING).state,
            State::Working
        );
        assert_eq!(
            r.read("[ . ] Action Required | Run netbot | m2", CODEX_APPROVAL)
                .state,
            State::Blocked
        );
    }

    #[test]
    fn codex_update_dialog_is_blocked() {
        // Recorded from codex-cli 0.165.0 at startup; the title is the host name.
        let screen = include_str!("../tests/fixtures/screens/codex-update.txt");
        let reading = codex().read("example-host", screen);
        assert_eq!(reading.state, State::Blocked);
        assert_eq!(reading.rule.as_deref(), Some("update-dialog"));
        // Each marker is needed.
        for marker in [
            "  Update available · 0.165.0 → 0.166.0",
            "› 1. Update now",
            "  enter continue · esc skip",
        ] {
            assert!(screen.contains(marker), "{marker}");
            let without = screen.replace(marker, "");
            assert_ne!(
                codex().read("example-host", &without).state,
                State::Blocked,
                "without {marker}"
            );
        }
    }

    #[test]
    fn codex_trust_screen_is_blocked() {
        // Recorded from codex-cli 0.165.0 at startup in an untrusted Git
        // repository, at 120 and 80 columns; the title is the host name.
        let wide = include_str!("../tests/fixtures/screens/codex-trust.txt");
        let narrow = include_str!("../tests/fixtures/screens/codex-trust-80.txt");
        // After Down, "Quit" is highlighted: still the same screen.
        let quit = narrow
            .replace("› 1. Trust and continue", "  1. Trust and continue")
            .replace("  2. Quit", "› 2. Quit");
        for screen in [wide, narrow, quit.as_str()] {
            let reading = codex().read("example-host", screen);
            assert_eq!(reading.rule.as_deref(), Some("trust-dialog"), "{screen}");
            assert_eq!(reading.state, State::Blocked);
        }
        // Each marker is needed.
        for marker in [
            "  Folder access",
            "› 1. Trust and continue",
            "  2. Quit",
            "  enter continue · esc quit",
        ] {
            assert!(narrow.contains(marker), "{marker}");
            let without = narrow.replace(marker, "");
            assert_ne!(
                codex().read("example-host", &without).rule.as_deref(),
                Some("trust-dialog"),
                "without {marker}"
            );
        }
        // The same text quoted in the transcript, above the prompt.
        let idle = include_str!("../tests/fixtures/screens/codex-idle.txt");
        let quoted = format!("{narrow}{idle}");
        assert_ne!(
            codex().read("example-host", &quoted).rule.as_deref(),
            Some("trust-dialog")
        );
    }

    #[test]
    fn quoted_update_text_above_the_composer_is_not_the_dialog() {
        let screen = include_str!("../tests/fixtures/screens/codex-update-quoted.txt");
        assert_eq!(
            codex().read("Wait for instructions", screen).state,
            State::Idle
        );
    }

    #[test]
    fn nothing_matching_is_unknown() {
        assert_eq!(claude().read("bash", "$ ls\n").state, State::Unknown);
    }

    #[test]
    fn rules_need_evidence() {
        assert!(
            parse(
                "vendor: claude\nrules:\n  - {id: x, state: idle, priority: 1}\n",
                "t"
            )
            .is_err()
        );
        assert!(
            parse(
                "vendor: claude\nrules:\n  - {id: x, state: idle, priority: 1, any: [a]}\n",
                "t"
            )
            .is_err()
        );
    }
}
