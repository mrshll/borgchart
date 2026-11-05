//! Real-vendor qualification for the guarded wake. Ignored by default.
//!
//! Starts the real `claude` and `codex` with your own configuration on a
//! private tmux socket and checks how `wake` reads and treats their input
//! boxes. Typing without submitting costs nothing; each vendor gets two real
//! submissions of the reminder (vim insert mode, with clients attached and a
//! resize during the attempt; and vim normal mode). Approval prompts are never
//! answered: one that appears is recorded and left.
//!
//! ```text
//! BORGCHART_QUALIFY=1 cargo test --locked --test qualify -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `BORGCHART_QUALIFY_VENDORS=claude` (or `codex`) limits the run;
//! `BORGCHART_QUALIFY_SUBMIT=0` skips the submissions. The vendors run in
//! `target/qualify/work`, which must be inside a folder both CLIs already
//! trust (no trust prompt is answered). The matrix is printed and written to
//! `target/qualify/matrix.md`; raw screens go to `target/qualify/screens/`
//! (they show your real screen: sanitize before keeping any as fixtures).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use borgchart::bus::manifest::PaneIdentity;
use borgchart::composer::{self, Reading};
use borgchart::rules;
use borgchart::state::StateDir;
use borgchart::team::Vendor;
use borgchart::tmux::{self, Tmux};
use borgchart::wake::{self, Defer, LaunchFacts, Qualification, WakeResult};

const SESSION: &str = "brg-qualify";
const WINDOW: &str = "member";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/qualify")
}

fn base_env() -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for key in ["HOME", "USER", "LANG", "PATH"] {
        if let Ok(v) = std::env::var(key) {
            env.insert(key.to_string(), v);
        }
    }
    env.insert("TERM".into(), "xterm-256color".into());
    env.entry("LANG".into()).or_insert_with(|| "C.UTF-8".into());
    env
}

struct Run {
    vendor: Vendor,
    socket: String,
    tmux: Tmux,
    state: StateDir,
    argv: Vec<String>,
    facts: LaunchFacts,
    rows: Vec<[String; 4]>,
    qualified: bool,
}

impl Drop for Run {
    fn drop(&mut self) {
        let _ = self.tmux.run(&["kill-server"]);
        let _ = Command::new("tmux")
            .args(["-L", &format!("{}-outer", self.socket), "kill-server"])
            .output();
    }
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
    fn new(vendor: Vendor) -> Run {
        let socket = format!("brg-qualify-{}-{}", vendor.as_str(), std::process::id());
        std::fs::create_dir_all(root().join("work")).unwrap();
        std::fs::create_dir_all(root().join("screens")).unwrap();
        let conf = root().join(format!("tmux-{}.conf", vendor.as_str()));
        std::fs::write(&conf, tmux::CONFIG).unwrap();
        let tmux = Tmux::on_socket(&socket, &conf, base_env());
        let argv: Vec<String> = match vendor {
            Vendor::Claude => vec!["claude".into()],
            // The update check would show a startup dialog; brg answers none.
            Vendor::Codex => ["codex", "-c", "check_for_update_on_startup=false"]
                .map(String::from)
                .to_vec(),
        };
        let version = Command::new(vendor.as_str())
            .arg("--version")
            .env_clear()
            .envs(base_env())
            .stderr(Stdio::null())
            .output()
            .ok()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string()
            });
        let facts = LaunchFacts::measure(vendor, version, &argv, &root().join("work"), &base_env());
        Run {
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
        let cwd = root().join("work").display().to_string();
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

    fn title(&self) -> String {
        self.tmux
            .ok(&[
                "display-message",
                "-p",
                "-t",
                &self.target(),
                "#{pane_title}",
            ])
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    fn reading(&self) -> Reading {
        composer::read(self.vendor, &self.screen())
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
        let ok = if want.split('|').any(|w| got.starts_with(w)) {
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
        let before = self.screen();
        let (r, calls) = if self.qualified {
            self.attempt()
        } else {
            let rules = rules::load(self.vendor).unwrap();
            let r = wake::before_paste(self.vendor, &rules, &self.title(), &before);
            (r.unwrap_or(WakeResult::Submitted), 0)
        };
        std::thread::sleep(Duration::from_millis(300));
        let untouched = composer::view(self.vendor, &before).ok().map(|v| v.lines)
            == composer::view(self.vendor, &self.screen())
                .ok()
                .map(|v| v.lines);
        self.save(case);
        let got = format!(
            "{}{}{}",
            show(&r),
            if calls > 0 { " (intent recorded!)" } else { "" },
            if untouched { "" } else { " (screen changed!)" }
        );
        self.record(case, "deferred:draft", got);
    }

    /// Wait for a turn to end: an empty idle box, or a prompt (left alone).
    fn wait_turn(&self) -> String {
        let start = Instant::now();
        let rules = rules::load(self.vendor).unwrap();
        while start.elapsed() < Duration::from_secs(180) {
            std::thread::sleep(Duration::from_secs(2));
            match wake::before_paste(self.vendor, &rules, &self.title(), &self.screen()) {
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

fn vendors() -> Vec<Vendor> {
    let only = std::env::var("BORGCHART_QUALIFY_VENDORS").unwrap_or_default();
    [Vendor::Claude, Vendor::Codex]
        .into_iter()
        .filter(|v| only.is_empty() || only.split(',').any(|o| o == v.as_str()))
        .collect()
}

fn qualify(vendor: Vendor) -> Vec<[String; 4]> {
    let mut run = Run::new(vendor);
    let submit = std::env::var("BORGCHART_QUALIFY_SUBMIT").as_deref() != Ok("0");
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
    let got = match wake::before_paste(vendor, &rules, &run.title(), &run.screen()) {
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
    let got = match wake::before_paste(vendor, &rules, &run.title(), &run.screen()) {
        None => "would paste".to_string(),
        Some(r) => show(&r),
    };
    run.record("draft clears: after clearing", "would paste", got);

    // A persistent draft keeps deferring, with a resize and attached clients.
    run.start(200, 50);
    run.keys(&["-l", "a draft left overnight"]);
    let outer = format!("{}-outer", run.socket);
    let _ = Command::new("tmux")
        .args([
            "-L",
            &outer,
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-x",
            "150",
            "-y",
            "45",
        ])
        .arg(format!("tmux -L {} attach -t ={SESSION}", run.socket))
        .env_clear()
        .envs(base_env())
        .status();
    let mut control = Command::new("tmux")
        .args([
            "-L",
            &run.socket,
            "-C",
            "attach",
            "-t",
            &format!("={SESSION}"),
        ])
        .env_clear()
        .envs(base_env())
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
        let s = run.screen();
        wake::before_paste(vendor, &rules, &run.title(), &s).unwrap_or(WakeResult::Submitted)
    };
    run.record("pane 100x12", "deferred:uncertain", show(&r));
    let _ = control.kill();
    let _ = control.wait();
    let _ = Command::new("tmux")
        .args(["-L", &outer, "kill-server"])
        .output();

    if !qualified || !submit {
        run.record("submissions", "", "skipped".into());
        return std::mem::take(&mut run.rows);
    }

    // Submission 1: insert mode, clients attached, resize during the attempt.
    run.start(200, 50);
    let _ = Command::new("tmux")
        .args([
            "-L",
            &outer,
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-x",
            "150",
            "-y",
            "45",
        ])
        .arg(format!("tmux -L {} attach -t ={SESSION}", run.socket))
        .env_clear()
        .envs(base_env())
        .status();
    let mut control = Command::new("tmux")
        .args([
            "-L",
            &run.socket,
            "-C",
            "attach",
            "-t",
            &format!("={SESSION}"),
        ])
        .env_clear()
        .envs(base_env())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let socket = run.socket.clone();
    let target = run.target();
    let resizer = std::thread::spawn(move || {
        for (x, y) in [("190", "50"), ("200", "48"), ("200", "50")] {
            std::thread::sleep(Duration::from_millis(150));
            let _ = Command::new("tmux")
                .args([
                    "-L",
                    &socket,
                    "resize-window",
                    "-t",
                    &target,
                    "-x",
                    x,
                    "-y",
                    y,
                ])
                .output();
        }
    });
    let (r, _) = run.attempt();
    let _ = resizer.join();
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
    let _ = Command::new("tmux")
        .args(["-L", &outer, "kill-server"])
        .output();

    // Submission 2: vim normal mode.
    run.start(200, 50);
    run.keys(&["Escape"]);
    std::thread::sleep(Duration::from_millis(500));
    let mode = composer::view(vendor, &run.screen())
        .ok()
        .and_then(|v| v.vim);
    run.record("normal mode shown", "Some(Normal)", format!("{mode:?}"));
    let (r, _) = run.attempt();
    run.record("submit: vim normal", "submitted", show(&r));
    let end = run.wait_turn();
    run.save("after-turn-2");
    run.record("turn 2 ended", "idle", end);
    std::mem::take(&mut run.rows)
}

#[test]
#[ignore = "launches the real vendor CLIs; set BORGCHART_QUALIFY=1"]
fn qualification_matrix() {
    if std::env::var("BORGCHART_QUALIFY").as_deref() != Ok("1") {
        eprintln!("set BORGCHART_QUALIFY=1 to run");
        return;
    }
    let mut md =
        String::from("| vendor | case | expected | got | ok |\n| --- | --- | --- | --- | --- |\n");
    let mut failed = 0;
    println!("| vendor | case | expected | got | ok |");
    for v in vendors() {
        for row in qualify(v) {
            if row[3] == "FAIL" {
                failed += 1;
            }
            let _ = writeln!(
                md,
                "| {} | {} | {} | {} | {} |",
                v.as_str(),
                row[0],
                row[1],
                row[2],
                row[3]
            );
        }
    }
    std::fs::write(root().join("matrix.md"), &md).unwrap();
    assert_eq!(failed, 0, "qualification failures:\n{md}");
}
