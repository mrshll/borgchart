//! The live tier's result rules: case IDs, outcomes, the run's mode and
//! vendor selection, the verdict, and the matrix written as cases finish.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The vendors a case starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Who {
    Claude,
    Codex,
}

impl Who {
    pub fn as_str(self) -> &'static str {
        match self {
            Who::Claude => "claude",
            Who::Codex => "codex",
        }
    }
}

/// Every case of a complete run, by ID. A stable rebuild qualifies only on
/// a complete run where each of these is PASS.
pub const REQUIRED: &[(&str, Who)] = &[
    ("claude-trust-off", Who::Claude),
    ("claude-trust-status", Who::Claude),
    ("claude-trust-on", Who::Claude),
    ("codex-trust-on", Who::Codex),
    ("codex-trust-off", Who::Codex),
    ("codex-trust-status", Who::Codex),
    ("codex-locked-trust", Who::Codex),
    ("claude-env-colour", Who::Claude),
    ("codex-env-colour", Who::Codex),
    ("claude-background-wait", Who::Claude),
    ("codex-queue", Who::Codex),
    ("claude-wake-matrix", Who::Claude),
    ("codex-wake-matrix", Who::Codex),
];

/// The Claude `auto_trust`-on case answers Claude's trust dialog, which
/// saves trust to your Claude config, so it doesn't run. Flip this to run
/// it.
pub const RUN_CLAUDE_TRUST_ON: bool = false;

/// With `auto_trust: false`, `brg status` should report the member waiting
/// at folder trust with a `brg attach` hint (`*-trust-status`). brg doesn't
/// say so yet (#54); flip this once it does.
pub const CHECK_STATUS_TRUST_DETAIL: bool = false;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pass(String),
    Fail(String),
    Skip(String),
    Blocked(String),
}

impl Outcome {
    pub fn label(&self) -> &'static str {
        match self {
            Outcome::Pass(_) => "PASS",
            Outcome::Fail(_) => "FAIL",
            Outcome::Skip(_) => "SKIP",
            Outcome::Blocked(_) => "BLOCKED",
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            Outcome::Pass(d) | Outcome::Fail(d) | Outcome::Skip(d) | Outcome::Blocked(d) => d,
        }
    }

    /// A SKIP must say why; one that doesn't is a FAIL.
    pub fn checked(self) -> Outcome {
        match self {
            Outcome::Skip(r) if r.trim().is_empty() => {
                Outcome::Fail("SKIP without a reason".into())
            }
            o => o,
        }
    }
}

/// How the run goes, from `BORGCHART_LIVE` and `BORGCHART_LIVE_DRY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Off,
    Live,
    /// Owned fake CLIs only. Never qualifies anything.
    Dry,
}

pub fn mode(live: Option<&str>, dry: Option<&str>) -> Mode {
    if dry == Some("1") {
        Mode::Dry
    } else if live == Some("1") {
        Mode::Live
    } else {
        Mode::Off
    }
}

/// `BORGCHART_LIVE_VENDORS`: unset means both; otherwise a comma list of
/// `claude` and `codex`. Empty or unknown names are refused.
pub fn select(value: Option<&str>) -> Result<(Vec<Who>, bool), String> {
    let Some(value) = value else {
        return Ok((vec![Who::Claude, Who::Codex], false));
    };
    let mut out = Vec::new();
    for name in value.split(',').map(str::trim) {
        let who = match name {
            "claude" => Who::Claude,
            "codex" => Who::Codex,
            "" => return Err(format!("empty vendor in BORGCHART_LIVE_VENDORS={value:?}")),
            other => {
                return Err(format!(
                    "unknown vendor {other:?} in BORGCHART_LIVE_VENDORS"
                ));
            }
        };
        if !out.contains(&who) {
            out.push(who);
        }
    }
    out.sort();
    let partial = out.len() < 2;
    Ok((out, partial))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// Selected required cases with no result.
    pub missing: Vec<String>,
    pub failed: Vec<String>,
    pub blocked: Vec<String>,
    /// Selected required cases that are SKIP.
    pub skipped: Vec<String>,
    pub partial: bool,
    pub dry: bool,
}

impl Verdict {
    /// Whether the run fails (non-zero exit).
    pub fn fails(&self) -> bool {
        !self.missing.is_empty() || !self.failed.is_empty() || !self.blocked.is_empty()
    }

    /// Whether this run qualifies a stable rebuild.
    pub fn qualifies(&self) -> bool {
        !self.fails() && self.skipped.is_empty() && !self.partial && !self.dry
    }

    pub fn summary(&self) -> String {
        let mut s = String::new();
        if self.dry {
            s.push_str("DRY run (owned fake CLIs): qualifies nothing. ");
        }
        if self.partial {
            s.push_str("Partial run: qualifies nothing. ");
        }
        for (what, list) in [
            ("incomplete, no result for", &self.missing),
            ("FAIL", &self.failed),
            ("BLOCKED", &self.blocked),
            ("not qualified (SKIP)", &self.skipped),
        ] {
            if !list.is_empty() {
                let _ = write!(s, "{what}: {}. ", list.join(", "));
            }
        }
        if self.qualifies() {
            s.push_str("Every required case passed: this run qualifies a stable rebuild.");
        } else if !self.fails() && !self.dry && !self.partial {
            s.push_str("No failures, but the run doesn't qualify the items above.");
        }
        s.trim_end().to_string()
    }
}

pub fn verdict(
    selected: &[Who],
    results: &BTreeMap<String, Outcome>,
    partial: bool,
    dry: bool,
) -> Verdict {
    let mut v = Verdict {
        missing: Vec::new(),
        failed: Vec::new(),
        blocked: Vec::new(),
        skipped: Vec::new(),
        partial,
        dry,
    };
    for (id, who) in REQUIRED {
        if !selected.contains(who) {
            continue;
        }
        match results.get(*id).cloned().map(Outcome::checked) {
            None => v.missing.push(id.to_string()),
            Some(Outcome::Pass(_)) => {}
            Some(Outcome::Fail(_)) => v.failed.push(id.to_string()),
            Some(Outcome::Blocked(_)) => v.blocked.push(id.to_string()),
            Some(Outcome::Skip(_)) => v.skipped.push(id.to_string()),
        }
    }
    // A result for a case that isn't listed is a harness bug.
    for id in results.keys() {
        if !REQUIRED.iter().any(|(r, _)| r == id) {
            v.failed.push(format!("{id} (not a listed case)"));
        }
    }
    v
}

/// `target/live/<run>/matrix.md` (or the dry run's own directory), written
/// row by row as cases finish, so a killed run keeps what it had.
pub struct Matrix {
    pub path: PathBuf,
}

impl Matrix {
    pub fn create(dir: &Path, header: &str) -> Matrix {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("matrix.md");
        std::fs::write(
            &path,
            format!("{header}\n\n| case | result | detail |\n| --- | --- | --- |\n"),
        )
        .unwrap();
        Matrix { path }
    }

    fn append(&self, text: &str) {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    pub fn row(&self, id: &str, outcome: &Outcome) {
        let detail = outcome.detail().replace('|', "\\|").replace('\n', " ");
        let line = format!("| {id} | {} | {detail} |\n", outcome.label());
        print!("{line}");
        self.append(&line);
    }

    pub fn finish(&self, v: &Verdict) {
        let line = format!("\n**Verdict:** {}\n", v.summary());
        println!("{line}");
        self.append(&line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(o: Outcome) -> BTreeMap<String, Outcome> {
        REQUIRED
            .iter()
            .map(|(id, _)| (id.to_string(), o.clone()))
            .collect()
    }

    const BOTH: &[Who] = &[Who::Claude, Who::Codex];

    #[test]
    fn only_a_complete_live_run_of_passes_qualifies() {
        let v = verdict(BOTH, &all(Outcome::Pass("".into())), false, false);
        assert!(v.qualifies() && !v.fails(), "{v:?}");
        assert!(!verdict(BOTH, &all(Outcome::Pass("".into())), false, true).qualifies());
        assert!(!verdict(BOTH, &all(Outcome::Pass("".into())), true, false).qualifies());
    }

    #[test]
    fn a_missing_case_makes_the_run_incomplete_and_fail() {
        let mut r = all(Outcome::Pass("".into()));
        r.remove("codex-trust-off");
        let v = verdict(BOTH, &r, false, false);
        assert_eq!(v.missing, ["codex-trust-off"]);
        assert!(v.fails() && !v.qualifies());
        // Not selected is not missing.
        let mut r = all(Outcome::Pass("".into()));
        r.retain(|id, _| id.starts_with("claude"));
        assert!(!verdict(&[Who::Claude], &r, true, false).fails());
    }

    #[test]
    fn skip_needs_a_reason_and_never_qualifies() {
        assert_eq!(
            Outcome::Skip(" ".into()).checked(),
            Outcome::Fail("SKIP without a reason".into())
        );
        let mut r = all(Outcome::Pass("".into()));
        r.insert("codex-queue".into(), Outcome::Skip("".into()));
        assert!(verdict(BOTH, &r, false, false).fails());
        r.insert("codex-queue".into(), Outcome::Skip("not until #20".into()));
        let v = verdict(BOTH, &r, false, false);
        assert!(!v.fails() && !v.qualifies(), "{v:?}");
        assert!(v.summary().contains("codex-queue"), "{}", v.summary());
    }

    #[test]
    fn fail_and_blocked_fail_the_run() {
        for o in [Outcome::Fail("x".into()), Outcome::Blocked("x".into())] {
            let mut r = all(Outcome::Pass("".into()));
            r.insert("claude-env-colour".into(), o);
            assert!(verdict(BOTH, &r, false, false).fails());
        }
        let mut r = all(Outcome::Pass("".into()));
        r.insert("made-up".into(), Outcome::Pass("".into()));
        assert!(verdict(BOTH, &r, false, false).fails());
    }

    #[test]
    fn vendor_selection_refuses_empty_or_unknown_and_marks_partial() {
        assert_eq!(select(None), Ok((BOTH.to_vec(), false)));
        assert_eq!(select(Some("codex,claude")), Ok((BOTH.to_vec(), false)));
        assert_eq!(select(Some("claude")), Ok((vec![Who::Claude], true)));
        assert!(select(Some("")).is_err());
        assert!(select(Some("claude,")).is_err());
        assert!(select(Some("gemini")).is_err());
    }

    #[test]
    fn dry_wins_over_live() {
        assert_eq!(mode(Some("1"), Some("1")), Mode::Dry);
        assert_eq!(mode(Some("1"), None), Mode::Live);
        assert_eq!(mode(None, None), Mode::Off);
        assert_eq!(mode(Some("yes"), None), Mode::Off);
    }
}
