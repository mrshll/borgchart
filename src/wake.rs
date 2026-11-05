//! The guarded wake: type one fixed reminder into an idle member, or don't.
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

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::bus::manifest::PaneIdentity;
use crate::composer::{self, Reading};
use crate::rules::{self, State};
use crate::state::{self, StateDir};
use crate::team::Vendor;
use crate::tmux::Tmux;

/// The only text the wake ever types. It carries no payload.
pub const REMINDER: &str =
    "brg: new records in your inbox. Run `brg inbox` at your next stopping point.";

/// How long to wait for the member's send lock.
const LOCK_WAIT: Duration = Duration::from_secs(2);
/// How long the pasted reminder has to show up in the input box.
const RENDER_WAIT: Duration = Duration::from_secs(3);
/// How long the input box has to clear after Enter.
const SUBMIT_WAIT: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_millis(100);

/// Vendor, version and settings combinations the wake has been qualified
/// against with real CLIs (`tests/qualify.rs`). Anything else gets alerts only.
struct Qualified {
    vendor: Vendor,
    /// The first line of `<cli> --version`, exactly.
    version: &'static str,
    /// The input mode the CLI was set to.
    editor: &'static str,
    /// Claude's `promptSuggestionEnabled` (`None`: not applicable).
    suggestions: Option<bool>,
}

const QUALIFIED: &[Qualified] = &[
    Qualified {
        vendor: Vendor::Claude,
        version: "2.1.318 (Claude Code)",
        editor: "vim",
    suggestions: Some(false),
    },
    Qualified {
        vendor: Vendor::Codex,
        version: "codex-cli 0.165.0",
        editor: "vim",
        suggestions: None,
    },
];

/// What qualification needs to know about one member's start.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchFacts {
    /// The first line of `<cli> --version`, measured at the member's start.
    pub vendor_version: Option<String>,
    /// The member's launch argv.
    pub argv: Vec<String>,
    /// The input mode the vendor will use: `vim` or `normal` (`None`: unknown).
    pub editor: Option<String>,
    /// Claude's `promptSuggestionEnabled` (`None`: unknown or not applicable).
    pub suggestions: Option<bool>,
    /// Settings files that were read, for diagnosis.
    pub sources: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Qualification {
    Qualified,
    Unqualified(String),
}

/// Whether the guarded wake may type into this member at all.
pub fn qualification(vendor: Vendor, launch: &LaunchFacts) -> Qualification {
    let Some(version) = launch.vendor_version.as_deref() else {
        return Qualification::Unqualified(format!("{} version unknown", vendor.as_str()));
    };
    let Some(editor) = launch.editor.as_deref() else {
        return Qualification::Unqualified(format!(
            "{}'s input mode is unknown",
            vendor.as_str()
        ));
    };
    let candidates: Vec<&Qualified> = QUALIFIED
        .iter()
        .filter(|q| q.vendor == vendor && q.version == version)
        .collect();
    if candidates.is_empty() {
        return Qualification::Unqualified(format!(
            "{} `{version}` has not been qualified for typed reminders",
            vendor.as_str()
        ));
    }
    if !candidates.iter().any(|q| q.editor == editor) {
        return Qualification::Unqualified(format!(
            "{} `{version}` in `{editor}` input mode has not been qualified",
            vendor.as_str()
        ));
    }
    if !candidates
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

fn read_json(path: &Path, sources: &mut Vec<PathBuf>) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    sources.push(path.to_path_buf());
    serde_json::from_str(&text).ok()
}

/// `[section] key = value` from a TOML file, for the few plain keys needed
/// (no TOML dependency: tables written inline or dotted read as unknown).
fn toml_bool(text: &str, section: &str, key: &str) -> Option<bool> {
    let mut current = String::new();
    let mut found = None;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = name.trim().to_string();
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        let dotted = format!("{section}.{key}");
        if (current == section && k == key) || (current.is_empty() && k == dotted) {
            found = match v {
                "true" => Some(true),
                "false" => Some(false),
                _ => return None,
            };
        }
        if current.is_empty() && k == section {
            // `tui = { … }`: not read here.
            return None;
        }
    }
    found
}

impl LaunchFacts {
    /// Measure a member's launch facts: its argv plus the settings files the
    /// vendor will read from `cwd` and the member's environment (`HOME`,
    /// `CODEX_HOME`).
    pub fn measure(
        vendor: Vendor,
        vendor_version: Option<String>,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> LaunchFacts {
        let home = env.get("HOME").map(PathBuf::from);
        let mut facts = LaunchFacts {
            vendor_version,
            argv: argv.to_vec(),
            ..LaunchFacts::default()
        };
        match vendor {
            Vendor::Claude => {
                // Lowest precedence first; later files win.
                let mut files: Vec<PathBuf> = Vec::new();
                if let Some(h) = &home {
                    files.push(h.join(".claude.json"));
                    files.push(h.join(".claude/settings.json"));
                }
                files.push(cwd.join(".claude/settings.json"));
                files.push(cwd.join(".claude/settings.local.json"));
                let mut values: Vec<serde_json::Value> = files
                    .iter()
                    .filter_map(|f| read_json(f, &mut facts.sources))
                    .collect();
                if let Some(i) = argv.iter().position(|a| a == "--settings")
                    && let Some(arg) = argv.get(i + 1)
                {
                    let v = if arg.trim_start().starts_with('{') {
                        serde_json::from_str(arg).ok()
                    } else {
                        read_json(Path::new(arg), &mut facts.sources)
                    };
                    match v {
                        Some(v) => values.push(v),
                        // Unreadable settings could say anything.
                        None => return facts,
                    }
                }
                values.extend(read_json(
                    Path::new("/etc/claude-code/managed-settings.json"),
                    &mut facts.sources,
                ));
                // The CLI default when nothing sets them.
                let mut editor = Some("normal".to_string());
                let mut suggestions = Some(true);
                for v in &values {
                    if let Some(e) = v.get("editorMode") {
                        editor = e.as_str().map(str::to_string);
                    }
                    if let Some(s) = v.get("promptSuggestionEnabled") {
                        suggestions = s.as_bool();
                    }
                }
                facts.editor = editor;
                facts.suggestions = suggestions;
            }
            Vendor::Codex => {
                if argv.iter().any(|a| a == "-p" || a == "--profile") {
                    return facts;
                }
                let codex_home = env
                    .get("CODEX_HOME")
                    .map(PathBuf::from)
                    .or_else(|| home.as_ref().map(|h| h.join(".codex")));
                let mut vim = Some(false);
                for file in codex_home
                    .map(|h| h.join("config.toml"))
                    .into_iter()
                    .chain([cwd.join(".codex/config.toml")])
                {
                    if let Ok(text) = std::fs::read_to_string(&file) {
                        facts.sources.push(file.clone());
                        if text.contains("vim_mode_default") {
                            vim = toml_bool(&text, "tui", "vim_mode_default");
                        }
                    }
                }
                for pair in argv.windows(2) {
                    if (pair[0] == "-c" || pair[0] == "--config")
                        && pair[1].trim_start().starts_with("tui")
                    {
                        vim = match pair[1].replace(' ', "").as_str() {
                            "tui.vim_mode_default=true" => Some(true),
                            "tui.vim_mode_default=false" => Some(false),
                            _ => None,
                        };
                    }
                }
                facts.editor = vim.map(|v| if v { "vim" } else { "normal" }.to_string());
            }
        }
        facts
    }
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

/// One snapshot of the pane: identity, title and styled screen.
struct Snapshot {
    title: String,
    screen: String,
}

fn snapshot(tmux: &Tmux, expect: &PaneIdentity) -> Option<Snapshot> {
    let format = "#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{session_name}\t#{window_name}\t#{pane_title}";
    let out = tmux
        .ok(&["display-message", "-p", "-t", &expect.pane_id, format])
        .ok()?;
    let line = out.lines().next()?;
    let f: Vec<&str> = line.splitn(6, '\t').collect();
    if f.len() < 6
        || f[0] != expect.pane_id
        || f[1] != expect.pane_pid.to_string()
        || f[2] != "0"
        || f[3] != expect.session
        || f[4] != expect.window
    {
        return None;
    }
    let screen = tmux
        .ok(&["capture-pane", "-p", "-e", "-t", &expect.pane_id])
        .ok()?;
    Some(Snapshot {
        title: f[5].to_string(),
        screen,
    })
}

/// The pre-paste decision for one screen: `None` means type the reminder.
pub fn before_paste(
    vendor: Vendor,
    rules: &rules::RuleSet,
    title: &str,
    screen: &str,
) -> Option<WakeResult> {
    let lines = composer::parse(screen);
    let reading = rules.read(title, &composer::plain(&lines));
    match reading.state {
        State::Blocked => {
            return Some(WakeResult::Blocked(
                reading.rule.unwrap_or_else(|| "blocked".into()),
            ));
        }
        State::Working => return Some(WakeResult::Deferred(Defer::Busy)),
        State::Idle | State::Unknown => {}
    }
    match composer::read(vendor, screen) {
        Reading::Busy => Some(WakeResult::Deferred(Defer::Busy)),
        Reading::Draft => Some(WakeResult::Deferred(Defer::Draft)),
        Reading::Unknown(why) => Some(WakeResult::Deferred(Defer::Uncertain(why))),
        Reading::Empty if reading.state == State::Unknown => Some(WakeResult::Deferred(
            Defer::Uncertain("no state rule matched".into()),
        )),
        Reading::Empty => None,
    }
}

/// Why Enter may not be pressed on this screen, or `None` if it may.
pub fn before_enter(
    vendor: Vendor,
    rules: &rules::RuleSet,
    title: &str,
    screen: &str,
) -> Option<String> {
    let lines = composer::parse(screen);
    let reading = rules.read(title, &composer::plain(&lines));
    if reading.state == State::Blocked {
        return Some(format!(
            "a prompt appeared after the paste ({}); Enter was not pressed",
            reading.rule.unwrap_or_default()
        ));
    }
    match composer::view(vendor, screen) {
        Ok(v) if v.holds(REMINDER) => None,
        Ok(v) if v.busy => Some("a turn started after the paste; Enter was not pressed".into()),
        Ok(_) => Some(
            "the input box doesn't hold exactly the reminder; Enter was not pressed".into(),
        ),
        Err(why) => Some(format!(
            "the input box can't be read after the paste ({why}); Enter was not pressed"
        )),
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
/// `before_input` runs after every pre-paste check has passed and before the
/// first byte is typed; if it fails nothing is typed. After it has run the
/// result is `Submitted` or `HumanRequired`, except `Deferred(Uncertain)`
/// when tmux refused the reminder before anything reached the pane.
pub fn attempt(
    tmux: &Tmux,
    state: &StateDir,
    member: &str,
    expect: &PaneIdentity,
    vendor: Vendor,
    launch: &LaunchFacts,
    before_input: &mut dyn FnMut() -> std::io::Result<()>,
) -> WakeResult {
    if let Qualification::Unqualified(why) = qualification(vendor, launch) {
        return WakeResult::AlertOnly(why);
    }
    if tmux.socket() != expect.socket {
        return WakeResult::NotRunning;
    }
    if let Some(meta) = state::read_meta(state)
        && meta.members.get(member).is_some_and(|m| m.posture == "locked")
        && !state.gate_marker(member).exists()
    {
        return WakeResult::Deferred(Defer::Uncertain(
            "the member's `locked` gate is not active".into(),
        ));
    }
    let rules = match rules::load(vendor) {
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
    if let Some(result) = before_paste(vendor, &rules, &snap.title, &snap.screen) {
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
    if let Err(e) = tmux.run_with_stdin(&["load-buffer", "-b", &buffer, "-"], REMINDER.as_bytes())
    {
        return WakeResult::Deferred(Defer::Uncertain(format!("tmux refused the reminder: {e}")));
    }
    if let Err(e) = tmux.ok(&["paste-buffer", "-p", "-d", "-b", &buffer, "-t", &expect.pane_id]) {
        let _ = tmux.run(&["delete-buffer", "-b", &buffer]);
        return WakeResult::HumanRequired(format!("pasting the reminder failed: {e}"));
    }

    // Wait for the reminder to show, then check the screen once more.
    let start = Instant::now();
    let why = loop {
        std::thread::sleep(POLL);
        let Some(snap) = snapshot(tmux, expect) else {
            break "the pane changed or exited after the paste".to_string();
        };
        match before_enter(vendor, &rules, &snap.title, &snap.screen) {
            None => {
                if let Err(e) = tmux.ok(&["send-keys", "-t", &expect.pane_id, "Enter"]) {
                    return WakeResult::HumanRequired(format!(
                        "the reminder was pasted but Enter failed: {e}"
                    ));
                }
                break String::new();
            }
            Some(why) if start.elapsed() >= RENDER_WAIT => break why,
            Some(_) => {}
        }
    };
    if !why.is_empty() {
        return WakeResult::HumanRequired(why);
    }

    // Enter was pressed: the reminder should leave the input box.
    let start = Instant::now();
    loop {
        std::thread::sleep(POLL);
        let held = snapshot(tmux, expect)
            .and_then(|s| composer::view(vendor, &s.screen).ok())
            .is_some_and(|v| v.holds(REMINDER));
        if !held {
            drop(lock);
            return WakeResult::Submitted;
        }
        if start.elapsed() >= SUBMIT_WAIT {
            return WakeResult::HumanRequired(
                "Enter was pressed but the reminder is still in the input box".into(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            qualification(
                Vendor::Claude,
                &facts("2.1.318 (Claude Code)", "vim", Some(false))
            ),
            Qualification::Qualified
        );
        for f in [
            facts("2.1.319 (Claude Code)", "vim", Some(false)),
            facts("2.1.318 (Claude Code)", "normal", Some(false)),
            facts("2.1.318 (Claude Code)", "vim", Some(true)),
            facts("2.1.318 (Claude Code)", "vim", None),
            LaunchFacts::default(),
        ] {
            assert!(matches!(
                qualification(Vendor::Claude, &f),
                Qualification::Unqualified(_)
            ));
        }
    }

    #[test]
    fn toml_reading_is_narrow() {
        assert_eq!(
            toml_bool("[tui]\nvim_mode_default = true\n", "tui", "vim_mode_default"),
            Some(true)
        );
        assert_eq!(
            toml_bool("tui.vim_mode_default = false\n", "tui", "vim_mode_default"),
            Some(false)
        );
        assert_eq!(
            toml_bool("tui = { vim_mode_default = true }\n", "tui", "vim_mode_default"),
            None
        );
        assert_eq!(
            toml_bool("[other]\nvim_mode_default = true\n", "tui", "vim_mode_default"),
            None
        );
    }

    #[test]
    fn launch_facts_follow_settings_precedence() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let cwd = dir.path().join("work");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::create_dir_all(cwd.join(".claude")).unwrap();
        std::fs::write(
            home.join(".claude/settings.json"),
            r#"{"editorMode":"vim","promptSuggestionEnabled":false}"#,
        )
        .unwrap();
        let env = BTreeMap::from([("HOME".to_string(), home.display().to_string())]);
        let argv = vec!["claude".to_string()];
        let f = LaunchFacts::measure(Vendor::Claude, None, &argv, &cwd, &env);
        assert_eq!(f.editor.as_deref(), Some("vim"));
        assert_eq!(f.suggestions, Some(false));
        std::fs::write(
            cwd.join(".claude/settings.local.json"),
            r#"{"editorMode":"normal"}"#,
        )
        .unwrap();
        let f = LaunchFacts::measure(Vendor::Claude, None, &argv, &cwd, &env);
        assert_eq!(f.editor.as_deref(), Some("normal"));
        let argv = vec![
            "claude".to_string(),
            "--settings".into(),
            "/nonexistent/settings.json".into(),
        ];
        let f = LaunchFacts::measure(Vendor::Claude, None, &argv, &cwd, &env);
        assert_eq!(f.editor, None);
    }

    #[test]
    fn codex_launch_facts() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(
            home.join(".codex/config.toml"),
            "model = \"m\"\n[tui]\nvim_mode_default = true\n",
        )
        .unwrap();
        let env = BTreeMap::from([("HOME".to_string(), home.display().to_string())]);
        let argv = vec!["codex".to_string()];
        let f = LaunchFacts::measure(Vendor::Codex, None, &argv, dir.path(), &env);
        assert_eq!(f.editor.as_deref(), Some("vim"));
        let argv: Vec<String> = ["codex", "-c", "tui.vim_mode_default=false"]
            .map(String::from)
            .to_vec();
        let f = LaunchFacts::measure(Vendor::Codex, None, &argv, dir.path(), &env);
        assert_eq!(f.editor.as_deref(), Some("normal"));
        let argv: Vec<String> = ["codex", "--profile", "x"].map(String::from).to_vec();
        let f = LaunchFacts::measure(Vendor::Codex, None, &argv, dir.path(), &env);
        assert_eq!(f.editor, None);
    }
}
