//! The guarded Claude wake: type one fixed reminder into an idle member, or don't.
//!
//! [`attempt`] pastes [`REMINDER`] and presses Enter only after every check
//! passes, and stops short (leaving whatever is on screen alone) as soon as
//! one doesn't. It never clears input, forces, answers a dialog or retries.
//! The caller (the attention worker) owns episodes, budgets and deadlines;
//! this module only makes one attempt and says what happened.
//!
//! Before pasting, with the member's send lock held:
//! 1. the pane is the incarnation's own (`pane_id`, `pane_pid`, session,
//!    window, not dead), else `NotRunning`;
//! 2. the state rules don't see a prompt (`Blocked(rule)`) or a running turn
//!    (`Busy`);
//! 3. the input box reads as empty ([`crate::composer`]); a draft defers as
//!    `Draft`, anything unreadable as `Uncertain`;
//! 4. the caller's `before_input` succeeds (it records the attempt).
//!
//! Before Enter: the same pane, no prompt, and the input box holds exactly the
//! reminder (wrapping allowed). Otherwise `HumanRequired` and no Enter. A
//! person typing between that last check and Enter is the remaining race.
//! Remote prompts arrive over Claude Code's own connection, not the terminal,
//! so they shouldn't disturb the guarded wake's input-box checks; at worst a
//! reminder queues behind a remote turn.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::composer::{self, Geometry, Reading};
use crate::lifecycle::PaneIdentity;
use crate::rules::{self, State};
use crate::state::{self, StateDir};
use crate::team::Vendor;
use crate::tmux::{self, Tmux};

/// The only text the wake ever types. It carries no payload.
pub const REMINDER: &str =
    "brg: new records in your inbox. Run `brg msg list` at your next stopping point.";

/// How long to wait for the member's send lock.
const LOCK_WAIT: Duration = Duration::from_secs(2);
/// How long the pasted reminder has to show up in the input box.
const RENDER_WAIT: Duration = Duration::from_secs(3);
/// How long the input box has to clear after Enter.
const SUBMIT_WAIT: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_millis(100);

/// Claude settings qualified against the real CLI (the live tier,
/// `tests/live.rs`). Anything else gets alerts only.
struct Qualified {
    /// The input mode the CLI was set to.
    editor: &'static str,
    /// Claude's qualified `promptSuggestionEnabled` setting.
    suggestions: Option<bool>,
}

const QUALIFIED: &[Qualified] = &[Qualified {
    editor: "vim",
    suggestions: Some(false),
}];

/// What qualification needs to know about one member's start.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchFacts {
    /// The first line of `<cli> --version`, measured at the member's start.
    pub vendor_version: Option<String>,
    /// The member's launch argv.
    pub argv: Vec<String>,
    /// The input mode the vendor will use: `vim` or `normal` (`None`: unknown).
    pub editor: Option<String>,
    /// Claude's `promptSuggestionEnabled` (`None`: unknown).
    pub suggestions: Option<bool>,
    /// Settings files that were read, for diagnosis.
    pub sources: Vec<PathBuf>,
    /// Why the effective settings are unknown (a file that exists but can't
    /// be read or parsed, an unsupported option), if they are.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Qualification {
    Qualified,
    Unqualified(String),
}

/// Whether the measured CLI version is in the shared verified-version list.
pub fn verified(
    rules: &crate::watcher::methods::WakeRules,
    vendor: Vendor,
    version: Option<&str>,
) -> bool {
    let version = version.map(|v| match vendor {
        Vendor::Claude => v.strip_suffix(" (Claude Code)").unwrap_or(v),
        Vendor::Codex => v.strip_prefix("codex-cli ").unwrap_or(v),
    });
    rules.find(vendor, version).is_some()
}

/// Whether the guarded wake may type into this member at all.
pub fn qualification(launch: &LaunchFacts) -> Qualification {
    let vendor = Vendor::Claude;
    if let Some(problem) = &launch.problem {
        return Qualification::Unqualified(format!(
            "{}'s settings are unknown: {problem}",
            vendor.as_str()
        ));
    }
    let Some(version) = launch.vendor_version.as_deref() else {
        return Qualification::Unqualified(format!("{} version unknown", vendor.as_str()));
    };
    let Some(editor) = launch.editor.as_deref() else {
        return Qualification::Unqualified(format!("{}'s input mode is unknown", vendor.as_str()));
    };
    if !crate::watcher::methods::load().is_ok_and(|rules| verified(&rules, vendor, Some(version))) {
        return Qualification::Unqualified(format!(
            "{} `{version}` has not been qualified for typed reminders",
            vendor.as_str()
        ));
    }
    if !QUALIFIED.iter().any(|q| q.editor == editor) {
        return Qualification::Unqualified(format!(
            "{} `{version}` in `{editor}` input mode has not been qualified",
            vendor.as_str()
        ));
    }
    if !QUALIFIED
        .iter()
        .any(|q| q.editor == editor && q.suggestions == launch.suggestions)
    {
        return Qualification::Unqualified(format!(
            "{} `{version}` with prompt suggestions {} has not been qualified",
            vendor.as_str(),
            match launch.suggestions {
                Some(true) => "on",
                Some(false) => "off",
                None => "unknown",
            }
        ));
    }
    Qualification::Qualified
}

/// A settings file's text: `Ok(None)` only when it doesn't exist.
fn read_text(path: &Path, sources: &mut Vec<PathBuf>) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            sources.push(path.to_path_buf());
            Ok(Some(text))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

fn read_json(path: &Path, sources: &mut Vec<PathBuf>) -> Result<Option<serde_json::Value>, String> {
    let Some(text) = read_text(path, sources)? else {
        return Ok(None);
    };
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(v) if v.is_object() => Ok(Some(v)),
        Ok(_) => Err(format!("{} is not a JSON object", path.display())),
        Err(e) => Err(format!("{} is not valid JSON: {e}", path.display())),
    }
}

impl LaunchFacts {
    /// Measure Claude's launch facts: its argv plus the settings files read
    /// from `cwd` and the member's environment (`HOME`). A missing file counts
    /// as unset; an unreadable or malformed file leaves a `problem`.
    pub fn measure(
        vendor_version: Option<String>,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> LaunchFacts {
        let mut facts = LaunchFacts {
            vendor_version,
            argv: argv.to_vec(),
            ..LaunchFacts::default()
        };
        let result = claude_settings(&mut facts, argv, cwd, env);
        if let Err(problem) = result {
            facts.editor = None;
            facts.suggestions = None;
            facts.problem = Some(problem);
        }
        facts
    }
}

fn claude_settings(
    facts: &mut LaunchFacts,
    argv: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    // Lowest precedence first; later files win.
    let mut files: Vec<PathBuf> = Vec::new();
    let home = env.get("HOME").ok_or("HOME is not set")?;
    let home = Path::new(home);
    files.push(home.join(".claude.json"));
    files.push(home.join(".claude/settings.json"));
    files.push(cwd.join(".claude/settings.json"));
    files.push(cwd.join(".claude/settings.local.json"));
    let mut values: Vec<serde_json::Value> = Vec::new();
    for f in &files {
        values.extend(read_json(f, &mut facts.sources)?);
    }
    let settings: Vec<&String> = argv
        .iter()
        .enumerate()
        .filter(|(i, a)| *a == "--settings" && *i + 1 < argv.len())
        .map(|(i, _)| &argv[i + 1])
        .collect();
    if argv.iter().any(|a| a.starts_with("--settings=")) {
        return Err("`--settings=…` is not followed".into());
    }
    for arg in settings {
        if arg.trim_start().starts_with('{') {
            let v: serde_json::Value = serde_json::from_str(arg)
                .map_err(|e| format!("the --settings argument is not valid JSON: {e}"))?;
            values.push(v);
        } else {
            let v = read_json(Path::new(arg), &mut facts.sources)?
                .ok_or_else(|| format!("the --settings file {arg} does not exist"))?;
            values.push(v);
        }
    }
    values.extend(read_json(
        Path::new("/etc/claude-code/managed-settings.json"),
        &mut facts.sources,
    )?);
    // The CLI defaults when nothing sets them.
    let mut editor = Some("normal".to_string());
    let mut suggestions = Some(true);
    for v in &values {
        if let Some(e) = v.get("editorMode") {
            editor = Some(
                e.as_str()
                    .ok_or("`editorMode` is not a string")?
                    .to_string(),
            );
        }
        if let Some(s) = v.get("promptSuggestionEnabled") {
            suggestions = Some(
                s.as_bool()
                    .ok_or("`promptSuggestionEnabled` is not a boolean")?,
            );
        }
    }
    facts.editor = editor;
    facts.suggestions = suggestions;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Defer {
    /// A turn is running.
    Busy,
    /// Something is in the input box.
    Draft,
    /// The screen couldn't be read with confidence.
    Uncertain(String),
    /// Another sender holds the member's send lock.
    Locked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeResult {
    /// The reminder was pasted, Enter pressed, and the input box cleared.
    Submitted,
    /// Nothing was typed; the attempt may be made again later.
    Deferred(Defer),
    /// The member is at a prompt (the state rule's ID); nothing was typed.
    Blocked(String),
    /// Something may have been typed and Enter was withheld, or the outcome
    /// is unknown. A person has to look; never retried automatically.
    HumanRequired(String),
    /// Not qualified for typed reminders; nothing was typed.
    AlertOnly(String),
    /// The incarnation's pane is gone, dead or another process.
    NotRunning,
}

/// One snapshot of the pane: title, styled screen and geometry.
pub struct Snapshot {
    pub title: String,
    pub screen: String,
    pub geometry: Geometry,
    /// tmux copy mode (or another mode) is showing over the program.
    pub in_mode: bool,
}

/// A tmux client for the server the incarnation's pane lives on: an
/// absolute socket path is used as is (`-S`), a bare name as `-L`.
fn server(tmux: &Tmux, expect: &PaneIdentity) -> Tmux {
    tmux.with_socket(&expect.socket)
}

/// Read the pane if it is still the incarnation's own.
pub fn snapshot(tmux: &Tmux, expect: &PaneIdentity) -> Option<Snapshot> {
    let format = "#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{session_name}\t#{window_name}\t#{pane_in_mode}\t#{pane_width}\t#{cursor_x}\t#{cursor_y}\t#{pane_title}";
    let out = tmux
        .ok(&["display-message", "-p", "-t", &expect.pane_id, format])
        .ok()?;
    let line = out.lines().next()?;
    let f: Vec<&str> = line.splitn(10, '\t').collect();
    if f.len() < 10
        || f[0] != expect.pane_id
        || f[1] != expect.pane_pid.to_string()
        || f[2] != "0"
        || f[3] != expect.session
        || f[4] != expect.window
    {
        return None;
    }
    let geometry = Geometry {
        width: f[6].parse().ok()?,
        cursor_x: f[7].parse().ok()?,
        cursor_y: f[8].parse().ok()?,
    };
    let screen = tmux
        .ok(&["capture-pane", "-p", "-e", "-t", &expect.pane_id])
        .ok()?;
    Some(Snapshot {
        title: f[9].to_string(),
        screen,
        geometry,
        in_mode: tmux::in_mode(f[5]),
    })
}

/// The pre-paste decision for one snapshot: `None` means type the reminder.
pub fn before_paste(rules: &rules::RuleSet, snap: &Snapshot) -> Option<WakeResult> {
    let lines = composer::parse(&snap.screen);
    let reading = rules.read(&snap.title, &composer::plain(&lines));
    match reading.state {
        State::Blocked => {
            return Some(WakeResult::Blocked(
                reading.rule.unwrap_or_else(|| "blocked".into()),
            ));
        }
        State::Working => return Some(WakeResult::Deferred(Defer::Busy)),
        State::Idle | State::Unknown => {}
    }
    if snap.in_mode {
        return Some(WakeResult::Deferred(Defer::Uncertain(tmux::IN_MODE.into())));
    }
    match composer::read(&snap.screen, Some(snap.geometry)) {
        Reading::Busy => Some(WakeResult::Deferred(Defer::Busy)),
        Reading::Draft => Some(WakeResult::Deferred(Defer::Draft)),
        Reading::Unknown(why) => Some(WakeResult::Deferred(Defer::Uncertain(why))),
        Reading::Empty if reading.state == State::Unknown => Some(WakeResult::Deferred(
            Defer::Uncertain("no state rule matched".into()),
        )),
        Reading::Empty => None,
    }
}

/// Why Enter may not be pressed on this snapshot, or `None` if it may.
pub fn before_enter(rules: &rules::RuleSet, snap: &Snapshot) -> Option<String> {
    let lines = composer::parse(&snap.screen);
    let reading = rules.read(&snap.title, &composer::plain(&lines));
    if reading.state == State::Blocked {
        return Some(format!(
            "a prompt appeared after the paste ({}); Enter was not pressed",
            reading.rule.unwrap_or_default()
        ));
    }
    if snap.in_mode {
        return Some(format!("{}; Enter was not pressed", tmux::IN_MODE));
    }
    match composer::view(&snap.screen, Some(snap.geometry)) {
        Ok(v) if v.holds(REMINDER) => None,
        Ok(v) if v.busy => Some("a turn started after the paste; Enter was not pressed".into()),
        Ok(_) => {
            Some("the input box doesn't hold exactly the reminder; Enter was not pressed".into())
        }
        Err(why) => Some(format!(
            "the input box can't be read after the paste ({why}); Enter was not pressed"
        )),
    }
}

/// After Enter: `Ok` once the input box is empty or shows Claude's queued-message
/// placeholder (a turn may be running); otherwise why the outcome isn't known.
pub fn after_enter(snap: Option<&Snapshot>) -> Result<(), String> {
    let snap = snap.ok_or("the pane changed or exited after Enter")?;
    if snap.in_mode {
        return Err(format!("{} after Enter", tmux::IN_MODE));
    }
    let view = composer::view(&snap.screen, Some(snap.geometry))
        .map_err(|why| format!("the input box can't be read after Enter ({why})"))?;
    if view.lines.iter().all(|l| l.trim().is_empty()) || view.shows_queued_placeholder() {
        Ok(())
    } else if view.holds(REMINDER) {
        Err("Enter was pressed but the reminder is still in the input box".into())
    } else {
        Err("the input box isn't empty after Enter".into())
    }
}

fn take_lock(state: &StateDir, member: &str) -> Result<File, WakeResult> {
    let file = state::open_lock(&state.lock_file(member)).map_err(|e| {
        WakeResult::Deferred(Defer::Uncertain(format!("cannot open the send lock: {e}")))
    })?;
    let start = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if start.elapsed() < LOCK_WAIT => {
                std::thread::sleep(POLL)
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(WakeResult::Deferred(Defer::Locked));
            }
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(WakeResult::Deferred(Defer::Uncertain(format!(
                    "cannot lock: {e}"
                ))));
            }
        }
    }
}

/// Make one guarded wake attempt on `member`'s incarnation pane `expect`.
///
/// `tmux` supplies the environment and config; the pane is addressed on the
/// server `expect.socket` names. `before_input` runs after every pre-paste
/// check has passed and before the first byte is typed; if it fails nothing
/// is typed. After it has run the result is `Submitted` or `HumanRequired`,
/// except `Deferred(Uncertain)` when tmux refused the reminder before
/// anything reached the pane.
pub fn attempt(
    tmux: &Tmux,
    state: &StateDir,
    member: &str,
    expect: &PaneIdentity,
    launch: &LaunchFacts,
    before_input: &mut dyn FnMut() -> std::io::Result<()>,
) -> WakeResult {
    if let Qualification::Unqualified(why) = qualification(launch) {
        return WakeResult::AlertOnly(why);
    }
    let tmux = &server(tmux, expect);
    let meta = match state::read_meta(state) {
        Ok(meta) => meta,
        Err(e) => return WakeResult::Deferred(Defer::Uncertain(e)),
    };
    if let Some(meta) = meta
        && meta
            .members
            .get(member)
            .is_some_and(|m| m.posture == "locked")
        && !state.gate_marker(member).exists()
    {
        return WakeResult::Deferred(Defer::Uncertain(
            "the member's restricted-access gate is not active".into(),
        ));
    }
    let rules = match rules::load(Vendor::Claude) {
        Ok(r) => r,
        Err(e) => return WakeResult::Deferred(Defer::Uncertain(e)),
    };
    let lock = match take_lock(state, member) {
        Ok(l) => l,
        Err(r) => return r,
    };
    let Some(snap) = snapshot(tmux, expect) else {
        return WakeResult::NotRunning;
    };
    if let Some(result) = before_paste(&rules, &snap) {
        return result;
    }
    if let Err(e) = before_input() {
        return WakeResult::Deferred(Defer::Uncertain(format!(
            "the attempt could not be recorded: {e}"
        )));
    }

    let buffer = format!(
        "brg-wake-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    if let Err(e) = tmux.run_with_stdin(&["load-buffer", "-b", &buffer, "-"], REMINDER.as_bytes()) {
        return WakeResult::Deferred(Defer::Uncertain(format!("tmux refused the reminder: {e}")));
    }
    if let Err(e) = tmux.ok(&[
        "paste-buffer",
        "-p",
        "-d",
        "-b",
        &buffer,
        "-t",
        &expect.pane_id,
    ]) {
        let _ = tmux.run(&["delete-buffer", "-b", &buffer]);
        return WakeResult::HumanRequired(format!("pasting the reminder failed: {e}"));
    }

    // Wait for the reminder to show, then check the screen once more.
    let start = Instant::now();
    loop {
        std::thread::sleep(POLL);
        let why = match snapshot(tmux, expect) {
            None => "the pane changed or exited after the paste".to_string(),
            Some(snap) => match before_enter(&rules, &snap) {
                None => break,
                Some(why) => why,
            },
        };
        if start.elapsed() >= RENDER_WAIT {
            return WakeResult::HumanRequired(why);
        }
    }
    if let Err(e) = tmux.ok(&["send-keys", "-t", &expect.pane_id, "Enter"]) {
        return WakeResult::HumanRequired(format!("the reminder was pasted but Enter failed: {e}"));
    }

    // Only an empty input box, seen, confirms the submission.
    let start = Instant::now();
    loop {
        std::thread::sleep(POLL);
        match after_enter(snapshot(tmux, expect).as_ref()) {
            Ok(()) => {
                drop(lock);
                return WakeResult::Submitted;
            }
            Err(why) if start.elapsed() >= SUBMIT_WAIT => return WakeResult::HumanRequired(why),
            Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_queued_placeholder_confirms_submission_without_accepting_a_draft() {
        let screen =
            include_str!("../tests/fixtures/screens/claude-queued-placeholder-assembled.txt");
        let placeholder = "Press up to edit queued messages";
        let snapshot = |screen: String| Snapshot {
            title: "✳ Example".into(),
            screen,
            geometry: composer::Geometry {
                width: 80,
                cursor_x: 2,
                cursor_y: 35,
            },
            in_mode: false,
        };
        for screen in [
            screen.to_string(),
            screen.replace(placeholder, &format!("\x1b[2m{placeholder}\x1b[0m")),
        ] {
            assert_eq!(after_enter(Some(&snapshot(screen))), Ok(()));
        }
        for draft in [
            "a draft",
            "Press up to edit queued messages extra",
            "Press up to edit queued messages\n  more text",
        ] {
            assert!(after_enter(Some(&snapshot(screen.replace(placeholder, draft)))).is_err());
        }
    }

    fn facts(version: &str, editor: &str, suggestions: Option<bool>) -> LaunchFacts {
        LaunchFacts {
            vendor_version: Some(version.into()),
            editor: Some(editor.into()),
            suggestions,
            ..LaunchFacts::default()
        }
    }

    #[test]
    fn only_qualified_combinations_qualify() {
        assert_eq!(
            qualification(&facts("2.1.318 (Claude Code)", "vim", Some(false))),
            Qualification::Qualified
        );
        for f in [
            facts("2.2.0 (Claude Code)", "vim", Some(false)),
            facts("2.1.318 (Claude Code)", "normal", Some(false)),
            facts("2.1.318 (Claude Code)", "vim", Some(true)),
            facts("2.1.318 (Claude Code)", "vim", None),
            LaunchFacts::default(),
        ] {
            assert!(matches!(qualification(&f), Qualification::Unqualified(_)));
        }
    }

    fn claude_env(dir: &Path) -> (PathBuf, PathBuf, BTreeMap<String, String>) {
        let home = dir.join("home");
        let cwd = dir.join("work");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::create_dir_all(cwd.join(".claude")).unwrap();
        std::fs::write(
            home.join(".claude/settings.json"),
            r#"{"editorMode":"vim","promptSuggestionEnabled":false}"#,
        )
        .unwrap();
        let env = BTreeMap::from([("HOME".to_string(), home.display().to_string())]);
        (home, cwd, env)
    }

    fn qualified_claude(f: LaunchFacts) -> bool {
        let f = LaunchFacts {
            vendor_version: Some("2.1.318 (Claude Code)".into()),
            ..f
        };
        qualification(&f) == Qualification::Qualified
    }

    #[test]
    fn launch_facts_follow_settings_precedence() {
        let dir = tempfile::tempdir().unwrap();
        let (_, cwd, env) = claude_env(dir.path());
        let argv = vec!["claude".to_string()];
        let f = LaunchFacts::measure(None, &argv, &cwd, &env);
        assert_eq!(f.editor.as_deref(), Some("vim"));
        assert_eq!(f.suggestions, Some(false));
        assert!(qualified_claude(f));
        std::fs::write(
            cwd.join(".claude/settings.local.json"),
            r#"{"editorMode":"normal"}"#,
        )
        .unwrap();
        let f = LaunchFacts::measure(None, &argv, &cwd, &env);
        assert_eq!(f.editor.as_deref(), Some("normal"));
        let argv = vec![
            "claude".to_string(),
            "--settings".into(),
            "/nonexistent/settings.json".into(),
        ];
        let f = LaunchFacts::measure(None, &argv, &cwd, &env);
        assert_eq!(f.editor, None);
        assert!(!qualified_claude(f));
    }

    #[test]
    fn unreadable_or_malformed_claude_settings_are_unqualified() {
        let argv = vec!["claude".to_string()];
        type Make<'a> = &'a dyn Fn(&Path);
        let bad: [(&str, Make); 4] = [
            ("a directory where a file should be", &|p| {
                std::fs::create_dir_all(p).unwrap()
            }),
            ("malformed JSON", &|p| {
                std::fs::write(p, "{not json").unwrap()
            }),
            ("not an object", &|p| std::fs::write(p, "[1]").unwrap()),
            ("a wrongly typed value", &|p| {
                std::fs::write(p, r#"{"editorMode": 1}"#).unwrap()
            }),
        ];
        for (what, make) in bad {
            for rel in [".claude/settings.json", ".claude/settings.local.json"] {
                let dir = tempfile::tempdir().unwrap();
                let (_, cwd, env) = claude_env(dir.path());
                make(&cwd.join(rel));
                let f = LaunchFacts::measure(None, &argv, &cwd, &env);
                assert!(f.problem.is_some(), "{what} at {rel}");
                assert!(!qualified_claude(f), "{what} at {rel}");
            }
        }
    }
}
