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
    rules: Vec<RuleRaw>,
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
}

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
    Ok(RuleSet {
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
