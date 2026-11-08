//! The guarded wake's input-box matrix against the real CLIs (formerly
//! `tests/qualify.rs`): how `wake` reads and treats their input boxes.
//! Typing without submitting costs nothing; each vendor gets two real
//! submissions of the reminder (vim insert mode, with clients attached and a
//! resize during the attempt; and vim normal mode). Approval prompts are
//! never answered: one that appears is recorded and left.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use borgchart::composer::{self, Reading};
use borgchart::lifecycle::PaneIdentity;
use borgchart::rules;
use borgchart::state::StateDir;
use borgchart::team::Vendor;
use borgchart::tmux::{self, Tmux};
use borgchart::wake::{self, Defer, LaunchFacts, Qualification, REMINDER, Snapshot, WakeResult};

use super::common;

const SESSION: &str = "brg-qualify";
const WINDOW: &str = "member";

/// Where the matrix works: `work` is inside a folder Claude already trusts
/// (Codex gets session-only trust for it); screens and state are kept
/// beside the run's matrix.
pub struct Dirs {
    pub work: PathBuf,
    pub keep: PathBuf,
    /// The whole matrix's limit, for each vendor's watchdog.
    pub limit: Duration,
}

static DIRS: OnceLock<Dirs> = OnceLock::new();

pub fn set_dirs(d: Dirs) {
    let _ = DIRS.set(d);
}

fn dirs() -> &'static Dirs {
    DIRS.get().expect("wake_matrix::set_dirs first")
}

fn work() -> PathBuf {
    dirs().work.clone()
}

fn root() -> PathBuf {
    dirs().keep.clone()
}

fn base_env() -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    // Your own configuration homes, in place (nothing is copied).
    for key in [
        "HOME",
        "USER",
        "LANG",
        "PATH",
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
    ] {
        if let Ok(v) = std::env::var(key) {
            env.insert(key.to_string(), v);
        }
    }
    env.insert("TERM".into(), "xterm-256color".into());
    env.entry("LANG".into()).or_insert_with(|| "C.UTF-8".into());
    env
}

struct Run {
    /// First, so its servers stop before anything else goes.
    private: common::PrivateTmux,
    vendor: Vendor,
    socket: String,
    tmux: Tmux,
    state: StateDir,
    argv: Vec<String>,
    facts: LaunchFacts,
    rows: Vec<[String; 4]>,
    qualified: bool,
}

fn show(r: &WakeResult) -> String {
    match r {
        WakeResult::Submitted => "submitted".into(),
        WakeResult::Deferred(Defer::Busy) => "deferred:busy".into(),
        WakeResult::Deferred(Defer::Draft) => "deferred:draft".into(),
        WakeResult::Deferred(Defer::Locked) => "deferred:locked".into(),
        WakeResult::Deferred(Defer::Uncertain(w)) => format!("deferred:uncertain ({w})"),
        WakeResult::Blocked(rule) => format!("blocked:{rule}"),
        WakeResult::HumanRequired(w) => format!("human-required ({w})"),
        WakeResult::AlertOnly(w) => format!("alert-only ({w})"),
        WakeResult::NotRunning => "not-running".into(),
    }
}

fn show_reading(r: &Reading) -> String {
    match r {
        Reading::Empty => "empty".into(),
        Reading::Draft => "draft".into(),
        Reading::Busy => "busy".into(),
        Reading::Unknown(w) => format!("unknown ({w})"),
    }
}

impl Run {
    fn new(vendor: Vendor, version: Option<String>) -> Run {
        let private = common::PrivateTmux::with_watchdog(dirs().limit);
        let socket = private.socket(&format!("brg-qualify-{}", vendor.as_str()));
        std::fs::create_dir_all(work()).unwrap();
        std::fs::create_dir_all(root().join("screens")).unwrap();
        let conf = root().join(format!("tmux-{}.conf", vendor.as_str()));
        std::fs::write(&conf, tmux::CONFIG).unwrap();
        let mut tmux_env = base_env();
        let path = tmux_env.get("PATH").cloned().unwrap_or_default();
        tmux_env.insert("PATH".into(), private.path_with(&path));
        tmux_env.insert(
            "BORGCHART_TMUX_TMPDIR".into(),
            private.dir().display().to_string(),
        );
        let tmux = Tmux::on_socket(&socket, &conf, tmux_env);
        let argv: Vec<String> = match vendor {
            Vendor::Claude => vec!["claude".into()],
            // The update check would show a startup dialog; brg answers none.
            // Trust for the work folder is for this session only, as
            // `auto_trust` passes it: nothing is written to your config.
            Vendor::Codex => vec![
                "codex".into(),
                "-c".into(),
                "check_for_update_on_startup=false".into(),
                "-c".into(),
                borgchart::launch::codex_trust_config(&work()),
            ],
        };
        let facts = LaunchFacts::measure(vendor, version, &argv, &work(), &base_env());
        Run {
            private,
            vendor,
            state: StateDir::from_root(&root().join(format!("state-{}", vendor.as_str()))),
            socket,
            tmux,
            argv,
            facts,
            rows: Vec::new(),
            qualified: false,
        }
    }

    /// A tmux client for `socket` in the private directory, with the
    /// vendors' environment.
    fn client(&self, socket: &str) -> Command {
        let mut cmd = self.private.tmux(socket);
        cmd.env_clear()
            .envs(base_env())
            .env("TMUX_TMPDIR", self.private.dir());
        cmd
    }

    /// (Re)start the vendor in a fresh pane and wait for its empty input box.
    fn start(&self, cols: u32, rows: u32) {
        let _ = self.tmux.run(&["kill-server"]);
        std::thread::sleep(Duration::from_millis(500));
        let cmd = self
            .argv
            .iter()
            .map(|a| borgchart::state::shell_quote(a))
            .collect::<Vec<_>>()
            .join(" ");
        let cwd = work().display().to_string();
        let (c, r) = (cols.to_string(), rows.to_string());
        let args = [
            "new-session",
            "-d",
            "-s",
            SESSION,
            "-n",
            WINDOW,
            "-x",
            &c,
            "-y",
            &r,
            "-c",
            &cwd,
            &cmd,
        ];
        if let Err(e) = self.tmux.ok(&args) {
            eprintln!("retrying new-session after: {e}");
            std::thread::sleep(Duration::from_secs(1));
            self.tmux.ok(&args).unwrap();
        }
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(30) {
            std::thread::sleep(Duration::from_millis(500));
            if self.reading() == Reading::Empty {
                std::thread::sleep(Duration::from_millis(1500));
                return;
            }
        }
        panic!(
            "{} did not reach an empty input box:\n{}",
            self.vendor.as_str(),
            self.screen()
        );
    }

    fn target(&self) -> String {
        format!("={SESSION}:={WINDOW}")
    }

    fn pane(&self) -> PaneIdentity {
        let out = self
            .tmux
            .ok(&[
                "display-message",
                "-p",
                "-t",
                &self.target(),
                "#{pane_id} #{pane_pid}",
            ])
            .unwrap();
        let (id, pid) = out.trim().split_once(' ').unwrap();
        PaneIdentity {
            socket: self.socket.clone(),
            session: SESSION.into(),
            window: WINDOW.into(),
            pane_id: id.into(),
            pane_pid: pid.parse().unwrap(),
        }
    }

    fn screen(&self) -> String {
        self.tmux
            .ok(&["capture-pane", "-p", "-e", "-t", &self.target()])
            .unwrap_or_default()
    }

    fn snap(&self) -> Snapshot {
        wake::snapshot(&self.tmux, &self.pane()).expect("the pane is gone")
    }

    fn reading(&self) -> Reading {
        let s = self.snap();
        composer::read(self.vendor, &s.screen, Some(s.geometry))
    }

    fn save(&self, case: &str) {
        let path = root()
            .join("screens")
            .join(format!("{}-{case}.ansi", self.vendor.as_str()));
        let _ = std::fs::write(path, self.screen());
    }

    fn keys(&self, args: &[&str]) {
        let target = self.target();
        let mut a = vec!["send-keys", "-t", target.as_str()];
        a.extend_from_slice(args);
        self.tmux.ok(&a).unwrap();
        std::thread::sleep(Duration::from_millis(150));
    }

    fn newline(&self) {
        match self.vendor {
            Vendor::Claude => self.keys(&["S-Enter"]),
            Vendor::Codex => self.keys(&["C-j"]),
        }
    }

    fn attempt(&self) -> (WakeResult, u32) {
        let mut calls = 0;
        let r = wake::attempt(
            &self.tmux,
            &self.state,
            WINDOW,
            &self.pane(),
            self.vendor,
            &self.facts,
            &mut || {
                calls += 1;
                Ok(())
            },
        );
        (r, calls)
    }

    fn record(&mut self, case: &str, want: &str, got: String) {
        self.record_checked(case, want, got, &[]);
    }

    /// Record a row that also fails if any invariant is false.
    fn record_checked(&mut self, case: &str, want: &str, got: String, invariants: &[(&str, bool)]) {
        let got = invariants
            .iter()
            .filter(|(_, held)| !held)
            .fold(got, |g, (what, _)| format!("{g} ({what} violated)"));
        let ok = if evaluate(want, &got, invariants) {
            "ok"
        } else {
            "FAIL"
        };
        println!(
            "| {} | {case} | {want} | {got} | {ok} |",
            self.vendor.as_str()
        );
        self.rows.push([case.into(), want.into(), got, ok.into()]);
    }

    /// An attempt on a draft must defer and leave the draft as it was.
    fn draft_case(&mut self, case: &str) {
        std::thread::sleep(Duration::from_millis(500));
        let before = self.snap();
        let (r, calls) = if self.qualified {
            self.attempt()
        } else {
            let rules = rules::load(self.vendor).unwrap();
            let r = wake::before_paste(self.vendor, &rules, &before);
            (r.unwrap_or(WakeResult::Submitted), 0)
        };
        std::thread::sleep(Duration::from_millis(300));
        let after = self.snap();
        let lines = |s: &Snapshot| {
            composer::view(self.vendor, &s.screen, None)
                .ok()
                .map(|v| v.lines)
        };
        let untouched = lines(&before) == lines(&after)
            && (before.geometry.cursor_x, before.geometry.cursor_y)
                == (after.geometry.cursor_x, after.geometry.cursor_y);
        self.save(case);
        self.record_checked(
            case,
            "deferred:draft",
            show(&r),
            &[
                ("no intent recorded", calls == 0),
                ("input box untouched", untouched),
            ],
        );
    }

    /// A whitespace draft may read as a draft or as uncertain, never as empty.
    fn draft_or_uncertain(&mut self, case: &str) {
        std::thread::sleep(Duration::from_millis(500));
        let rules = rules::load(self.vendor).unwrap();
        let s = self.snap();
        self.save(case);
        let got = match wake::before_paste(self.vendor, &rules, &s) {
            None => "would paste".to_string(),
            Some(r) => show(&r),
        };
        self.record(case, "deferred:draft|deferred:uncertain", got);
    }

    /// Paste text by hand (bracketed), the way the wake does, without Enter.
    fn paste(&self, text: &str) {
        self.tmux
            .run_with_stdin(&["load-buffer", "-b", "qualify", "-"], text.as_bytes())
            .unwrap();
        self.tmux
            .ok(&[
                "paste-buffer",
                "-p",
                "-d",
                "-b",
                "qualify",
                "-t",
                &self.target(),
            ])
            .unwrap();
        std::thread::sleep(Duration::from_millis(800));
    }

    fn enter_case(&mut self, case: &str, want: &str) {
        std::thread::sleep(Duration::from_millis(300));
        let rules = rules::load(self.vendor).unwrap();
        let s = self.snap();
        self.save(case);
        let g = s.geometry;
        let got = match wake::before_enter(self.vendor, &rules, &s) {
            None => format!("would press Enter (cursor {},{})", g.cursor_x, g.cursor_y),
            Some(why) => format!("withheld: {why}"),
        };
        self.record(case, want, got);
    }

    /// Wait for a turn to end: an empty idle box, or a prompt (left alone).
    fn wait_turn(&self) -> String {
        let start = Instant::now();
        let rules = rules::load(self.vendor).unwrap();
        while start.elapsed() < Duration::from_secs(180) {
            std::thread::sleep(Duration::from_secs(2));
            match wake::before_paste(self.vendor, &rules, &self.snap()) {
                None => return "idle".into(),
                Some(WakeResult::Blocked(rule)) => {
                    return format!("blocked:{rule} (left unanswered)");
                }
                Some(_) => {}
            }
        }
        "still busy after 180 s".into()
    }
}

/// The matrix's rows for one vendor, and whether its wake qualified (typed
/// attempts and submissions ran).
pub fn qualify(vendor: Vendor, version: Option<String>, submit: bool) -> (Vec<[String; 4]>, bool) {
    let mut run = Run::new(vendor, version);
    println!(
        "\n{}: version {:?}, editor {:?}, suggestions {:?}, qualification {:?}",
        vendor.as_str(),
        run.facts.vendor_version,
        run.facts.editor,
        run.facts.suggestions,
        wake::qualification(vendor, &run.facts)
    );
    let qualified = wake::qualification(vendor, &run.facts) == Qualification::Qualified;
    run.qualified = qualified;
    let mut facts_row = format!("{:?}", run.facts.vendor_version.as_deref().unwrap_or("?"));
    let _ = write!(facts_row, " editor={:?}", run.facts.editor);
    run.record("launch facts", "", facts_row);
    if !qualified {
        // Unqualified: the adapter must not type, whatever the screen shows.
        run.start(200, 50);
        let (r, _) = run.attempt();
        run.record("unqualified attempt", "alert-only", show(&r));
        // The readings below are still measured, without typing.
    }

    // Empty idle box.
    run.start(200, 50);
    run.save("empty");
    let rules = rules::load(vendor).unwrap();
    let got = match wake::before_paste(vendor, &rules, &run.snap()) {
        None => "would paste".to_string(),
        Some(r) => show(&r),
    };
    run.record("empty idle", "would paste", got);

    // Single-line draft, then vim normal mode with it.
    run.keys(&["-l", "please look at the example widget"]);
    run.draft_case("draft single-line");
    run.keys(&["Escape"]);
    run.draft_case("draft in vim normal mode");

    // Multiline.
    run.start(200, 50);
    run.keys(&["-l", "first line of a note"]);
    run.newline();
    run.keys(&["-l", "second line"]);
    run.draft_case("draft multiline");

    // A draft scrolled out of view: many lines, then blank ones at the end.
    run.start(200, 50);
    for i in 0..45 {
        run.keys(&["-l", &format!("row {i} of a long note")]);
        run.newline();
    }
    for _ in 0..25 {
        run.newline();
    }
    run.draft_case("draft offscreen (blank tail)");

    // Wrapped single line in a narrow pane.
    run.start(60, 50);
    run.keys(&[
        "-l",
        "a fairly long single line draft that will need to wrap across the narrow pane width",
    ]);
    run.draft_case("draft wrapped");

    // History recall.
    run.start(200, 50);
    run.keys(&["Up"]);
    std::thread::sleep(Duration::from_millis(500));
    let got = show_reading(&run.reading());
    run.save("history-up");
    run.record("history recall (Up)", "draft", got.clone());

    // A draft that clears: draft, then (after a person clears it) empty.
    run.start(200, 50);
    run.keys(&["-l", "temporary"]);
    run.draft_case("draft clears: first attempt");
    run.keys(&["C-u"]);
    std::thread::sleep(Duration::from_millis(500));
    let got = match wake::before_paste(vendor, &rules, &run.snap()) {
        None => "would paste".to_string(),
        Some(r) => show(&r),
    };
    run.record("draft clears: after clearing", "would paste", got);

    // Whitespace-only drafts: never read as empty.
    run.start(200, 50);
    run.keys(&["-l", "   "]);
    run.draft_or_uncertain("spaces-only draft (insert)");
    run.start(200, 50);
    run.keys(&["-l", " "]);
    run.keys(&["Escape"]);
    run.draft_or_uncertain("one space, then vim normal mode");

    // The Enter check, with the reminder pasted by hand and never submitted.
    run.start(200, 50);
    run.paste(REMINDER);
    run.enter_case("reminder pasted", "would press Enter");
    run.keys(&["-l", " "]);
    run.enter_case("reminder + a typed space", "withheld");
    run.start(200, 50);
    run.paste(&REMINDER.replace("brg inbox", "brginbox"));
    run.enter_case("reminder with a space removed", "withheld");
    run.start(60, 50);
    run.paste(REMINDER);
    run.enter_case("reminder wrapped at 60 columns", "would press Enter");

    // A persistent draft keeps deferring, with a resize and attached clients.
    run.start(200, 50);
    run.keys(&["-l", "a draft left overnight"]);
    let outer = format!("{}-outer", run.socket);
    let _ = run
        .client(&outer)
        .args([
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-x",
            "150",
            "-y",
            "45",
        ])
        .arg(format!(
            "{} -L {} attach -t ={SESSION}",
            run.private.guard().display(),
            run.socket
        ))
        .status();
    let mut control = run
        .client(&run.socket)
        .args(["-C", "attach", "-t", &format!("={SESSION}")])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    for (i, size) in [("180", "48"), ("120", "40"), ("200", "50")]
        .iter()
        .enumerate()
    {
        let _ = run.tmux.ok(&[
            "resize-window",
            "-t",
            &run.target(),
            "-x",
            size.0,
            "-y",
            size.1,
        ]);
        run.draft_case(&format!("persistent draft #{} (attached, resized)", i + 1));
    }
    let clients = run
        .tmux
        .ok(&["list-clients", "-F", "#{client_control_mode}"])
        .unwrap_or_default();
    run.record(
        "clients attached during attempts",
        "2",
        clients.lines().count().to_string(),
    );

    // Too small a pane.
    let _ = run.tmux.ok(&[
        "resize-window",
        "-t",
        &run.target(),
        "-x",
        "100",
        "-y",
        "12",
    ]);
    run.keys(&["C-u"]);
    std::thread::sleep(Duration::from_millis(800));
    run.save("small");
    let r = if run.qualified {
        run.attempt().0
    } else {
        wake::before_paste(vendor, &rules, &run.snap()).unwrap_or(WakeResult::Submitted)
    };
    run.record("pane 100x12", "deferred:uncertain", show(&r));
    let _ = control.kill();
    let _ = control.wait();
    let _ = run.client(&outer).arg("kill-server").output();

    if !qualified || !submit {
        run.record("submissions", "", "skipped".into());
        return (std::mem::take(&mut run.rows), qualified);
    }

    // Submission 1: insert mode, clients attached, resize during the attempt.
    run.start(200, 50);
    let _ = run
        .client(&outer)
        .args([
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-x",
            "150",
            "-y",
            "45",
        ])
        .arg(format!(
            "{} -L {} attach -t ={SESSION}",
            run.private.guard().display(),
            run.socket
        ))
        .status();
    let mut control = run
        .client(&run.socket)
        .args(["-C", "attach", "-t", &format!("={SESSION}")])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let target = run.target();
    let (r, _) = std::thread::scope(|scope| {
        let run = &run;
        scope.spawn(|| {
            for (x, y) in [("190", "50"), ("200", "48"), ("200", "50")] {
                std::thread::sleep(Duration::from_millis(150));
                let _ = run
                    .client(&run.socket)
                    .args(["resize-window", "-t", &target, "-x", x, "-y", y])
                    .output();
            }
        });
        run.attempt()
    });
    // A redraw mid-resize can make the box unreadable: that defers, safely.
    run.record(
        "attempt during a resize",
        "submitted|deferred:uncertain",
        show(&r),
    );
    let mut submitted = r == WakeResult::Submitted;
    if !submitted {
        std::thread::sleep(Duration::from_secs(1));
        let (r, _) = run.attempt();
        run.record(
            "submit: vim insert, 2 clients attached",
            "submitted",
            show(&r),
        );
        submitted = r == WakeResult::Submitted;
    }
    if submitted {
        std::thread::sleep(Duration::from_millis(800));
        run.save("working");
        let (busy, calls) = run.attempt();
        run.record(
            "attempt during the turn",
            "deferred:busy",
            format!(
                "{}{}",
                show(&busy),
                if calls > 0 { " (intent recorded!)" } else { "" }
            ),
        );
    }
    let end = run.wait_turn();
    run.save("after-turn-1");
    run.record("turn 1 ended", "idle", end);
    let _ = control.kill();
    let _ = control.wait();
    let _ = run.client(&outer).arg("kill-server").output();

    // Submission 2: vim normal mode.
    run.start(200, 50);
    run.keys(&["Escape"]);
    std::thread::sleep(Duration::from_millis(500));
    let mode = composer::view(vendor, &run.screen(), None)
        .ok()
        .and_then(|v| v.vim);
    run.record("normal mode shown", "Some(Normal)", format!("{mode:?}"));
    let (r, _) = run.attempt();
    // Claude can't tell an empty box from one space in normal mode, so it defers.
    let want = match vendor {
        Vendor::Claude => "deferred:uncertain",
        Vendor::Codex => "submitted",
    };
    run.record("submit: vim normal", want, show(&r));
    let end = run.wait_turn();
    run.save("after-turn-2");
    run.record("turn 2 ended", "idle", end);
    (std::mem::take(&mut run.rows), qualified)
}

/// The rows as a Markdown table.
pub fn table(vendor: Vendor, rows: &[[String; 4]]) -> String {
    let mut md =
        String::from("| vendor | case | expected | got | ok |\n| --- | --- | --- | --- | --- |\n");
    for row in rows {
        let _ = writeln!(
            md,
            "| {} | {} | {} | {} | {} |",
            vendor.as_str(),
            row[0],
            row[1],
            row[2],
            row[3]
        );
    }
    md
}

/// A row passes when the result has one of the `|`-separated prefixes and
/// every invariant held, whatever the result text says.
fn evaluate(want: &str, got: &str, invariants: &[(&str, bool)]) -> bool {
    want.split('|').any(|w| got.starts_with(w)) && invariants.iter().all(|(_, held)| *held)
}

#[test]
fn rows_fail_on_any_broken_invariant() {
    assert!(evaluate("deferred:draft", "deferred:draft", &[("a", true)]));
    assert!(evaluate(
        "submitted|deferred:uncertain",
        "deferred:uncertain (x)",
        &[]
    ));
    assert!(!evaluate("deferred:draft", "deferred:busy", &[]));
    assert!(!evaluate(
        "deferred:draft",
        "deferred:draft",
        &[("no intent recorded", false)]
    ));
    assert!(!evaluate(
        "deferred:draft",
        "deferred:draft",
        &[("no intent recorded", true), ("input box untouched", false)]
    ));
}
