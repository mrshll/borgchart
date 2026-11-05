//! Guarded wake tests: the input-box reader against screens recorded from
//! the real CLIs (sanitized), and `wake::attempt` against the `fake-composer`
//! stand-in on a private tmux socket.
//!
//! Run with `cargo test --features test-support --test wake`.
#![cfg(feature = "test-support")]

use std::collections::BTreeMap;
use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use borgchart::bus::manifest::PaneIdentity;
use borgchart::composer::{self, Reading, VimMode};
use borgchart::rules;
use borgchart::state::StateDir;
use borgchart::team::Vendor;
use borgchart::tmux::Tmux;
use borgchart::wake::{self, Defer, LaunchFacts, REMINDER, WakeResult};

macro_rules! screen {
    ($name:literal) => {
        include_str!(concat!("fixtures/screens/", $name))
    };
}

fn claude(name: &str) -> &'static str {
    match name {
        "empty" => screen!("claude-composer-empty.ansi"),
        "empty-normal" => screen!("claude-composer-empty-normal.ansi"),
        "draft" => screen!("claude-composer-draft.ansi"),
        "draft-normal" => screen!("claude-composer-draft-normal.ansi"),
        "multiline" => screen!("claude-composer-multiline.ansi"),
        "offscreen" => screen!("claude-composer-offscreen.ansi"),
        "wrapped" => screen!("claude-composer-wrapped.ansi"),
        "history-up" => screen!("claude-composer-history-up.ansi"),
        "reminder" => screen!("claude-composer-reminder.ansi"),
        "reminder-extra" => screen!("claude-composer-reminder-extra.ansi"),
        "reminder-wrapped" => screen!("claude-composer-reminder-wrapped.ansi"),
        "working" => screen!("claude-composer-working.ansi"),
        "small" => screen!("claude-composer-small.ansi"),
        _ => unreachable!(),
    }
}

fn codex(name: &str) -> &'static str {
    match name {
        "empty" => screen!("codex-composer-empty.ansi"),
        "empty-normal" => screen!("codex-composer-empty-normal.ansi"),
        "draft" => screen!("codex-composer-draft.ansi"),
        "draft-normal" => screen!("codex-composer-draft-normal.ansi"),
        "multiline" => screen!("codex-composer-multiline.ansi"),
        "offscreen" => screen!("codex-composer-offscreen.ansi"),
        "wrapped" => screen!("codex-composer-wrapped.ansi"),
        "history-up" => screen!("codex-composer-history-up.ansi"),
        "reminder" => screen!("codex-composer-reminder.ansi"),
        "reminder-extra" => screen!("codex-composer-reminder-extra.ansi"),
        "working" => screen!("codex-composer-working.ansi"),
        _ => unreachable!(),
    }
}

#[test]
fn claude_input_box_readings() {
    for (name, want) in [
        ("empty", Reading::Empty),
        ("empty-normal", Reading::Empty),
        ("draft", Reading::Draft),
        ("draft-normal", Reading::Draft),
        ("multiline", Reading::Draft),
        ("offscreen", Reading::Draft),
        ("wrapped", Reading::Draft),
        ("history-up", Reading::Draft),
        ("reminder", Reading::Draft),
        ("working", Reading::Busy),
    ] {
        assert_eq!(composer::read(Vendor::Claude, claude(name)), want, "{name}");
    }
    assert!(matches!(
        composer::read(Vendor::Claude, claude("small")),
        Reading::Unknown(_)
    ));
}

#[test]
fn codex_input_box_readings() {
    for (name, want) in [
        ("empty", Reading::Empty),
        ("empty-normal", Reading::Empty),
        ("draft", Reading::Draft),
        ("draft-normal", Reading::Draft),
        ("multiline", Reading::Draft),
        ("offscreen", Reading::Draft),
        ("wrapped", Reading::Draft),
        ("history-up", Reading::Draft),
        ("reminder", Reading::Draft),
    ] {
        assert_eq!(composer::read(Vendor::Codex, codex(name)), want, "{name}");
    }
}

#[test]
fn vim_modes_are_read() {
    let mode = |v, s| composer::view(v, s).unwrap().vim;
    assert_eq!(mode(Vendor::Claude, claude("empty")), Some(VimMode::Insert));
    assert_eq!(
        mode(Vendor::Claude, claude("empty-normal")),
        Some(VimMode::Normal)
    );
    assert_eq!(mode(Vendor::Codex, codex("empty")), Some(VimMode::Insert));
    assert_eq!(
        mode(Vendor::Codex, codex("draft-normal")),
        Some(VimMode::Normal)
    );
}

#[test]
fn only_the_exact_reminder_passes_the_enter_check() {
    let holds = |v, s| composer::view(v, s).unwrap().holds(REMINDER);
    assert!(holds(Vendor::Claude, claude("reminder")));
    assert!(holds(Vendor::Claude, claude("reminder-wrapped")));
    assert!(holds(Vendor::Codex, codex("reminder")));
    for name in ["reminder-extra", "draft", "empty", "history-up"] {
        assert!(!holds(Vendor::Codex, codex(name)), "codex {name}");
    }
    for name in ["reminder-extra", "draft", "empty", "working", "multiline"] {
        assert!(!holds(Vendor::Claude, claude(name)), "claude {name}");
    }
}

#[test]
fn blocked_rule_ids_are_shared_with_status() {
    // `status` reads these screens with the same rules; the wake must report
    // the same rule ID and type nothing.
    let cases = [
        (
            Vendor::Claude,
            "✳ Slackbot read 5",
            screen!("claude-permission.txt"),
            "permission-dialog",
        ),
        (
            Vendor::Claude,
            "",
            screen!("claude-trust.txt"),
            "trust-dialog",
        ),
        (
            Vendor::Codex,
            "[ . ] Action Required | Run netbot | m2",
            screen!("codex-approval.txt"),
            "approval-title",
        ),
    ];
    for (vendor, title, screen, rule) in cases {
        let rules = rules::load(vendor).unwrap();
        assert_eq!(rules.read(title, screen).rule.as_deref(), Some(rule));
        assert_eq!(
            wake::before_paste(vendor, &rules, title, screen),
            Some(WakeResult::Blocked(rule.into()))
        );
    }
}

#[test]
fn pre_paste_decisions_on_real_screens() {
    let c = rules::load(Vendor::Claude).unwrap();
    let x = rules::load(Vendor::Codex).unwrap();
    let t = "✳ Claude Code";
    assert_eq!(
        wake::before_paste(Vendor::Claude, &c, t, claude("empty")),
        None
    );
    assert_eq!(
        wake::before_paste(Vendor::Claude, &c, t, claude("draft")),
        Some(WakeResult::Deferred(Defer::Draft))
    );
    // The title stays idle while a turn runs; the grey prompt says busy.
    assert_eq!(
        wake::before_paste(Vendor::Claude, &c, t, claude("working")),
        Some(WakeResult::Deferred(Defer::Busy))
    );
    let t = "example";
    assert_eq!(
        wake::before_paste(Vendor::Codex, &x, t, codex("empty")),
        None
    );
    // A Codex draft hides the idle footer, so the state is unknown; the draft
    // is still reported as a draft.
    assert_eq!(
        wake::before_paste(Vendor::Codex, &x, t, codex("draft")),
        Some(WakeResult::Deferred(Defer::Draft))
    );
    assert_eq!(
        wake::before_enter(Vendor::Codex, &x, t, codex("reminder")),
        None
    );
    assert!(wake::before_enter(Vendor::Codex, &x, t, codex("reminder-extra")).is_some());
    // The title can lag; the working line alone (either bullet) says busy.
    for title in ["\u{2838} example", t] {
        assert_eq!(
            wake::before_paste(Vendor::Codex, &x, title, codex("working")),
            Some(WakeResult::Deferred(Defer::Busy)),
            "{title}"
        );
    }
}

// ---- attempts against the fake composer ----

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Env {
    dir: tempfile::TempDir,
    socket: String,
    tmux: Tmux,
    state: StateDir,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-L", &self.socket, "kill-server"])
            .output();
    }
}

fn qualified(vendor: Vendor) -> LaunchFacts {
    LaunchFacts {
        vendor_version: Some(match vendor {
            Vendor::Claude => "2.1.318 (Claude Code)".into(),
            Vendor::Codex => "codex-cli 0.165.0".into(),
        }),
        editor: Some("vim".into()),
        suggestions: (vendor == Vendor::Claude).then_some(false),
        ..LaunchFacts::default()
    }
}

impl Env {
    /// Start the fake composer for `vendor` in session `s`, window `m`.
    fn new(vendor: Vendor) -> Env {
        let dir = tempfile::tempdir().unwrap();
        let socket = format!(
            "brg-wake-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        );
        let conf = dir.path().join("tmux.conf");
        fs::write(&conf, "set -g remain-on-exit on\n").unwrap();
        let env = BTreeMap::from([
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ("LANG".to_string(), "C.UTF-8".to_string()),
            ("TERM".to_string(), "xterm-256color".to_string()),
        ]);
        let tmux = Tmux::on_socket(&socket, &conf, env);
        let ctl = dir.path().join("fake");
        fs::create_dir_all(&ctl).unwrap();
        let cmd = format!(
            "FAKE_COMPOSER_DIR={} {} {}",
            ctl.display(),
            env!("CARGO_BIN_EXE_fake-composer"),
            vendor.as_str()
        );
        tmux.ok(&[
            "new-session",
            "-d",
            "-s",
            "s",
            "-n",
            "m",
            "-x",
            "120",
            "-y",
            "40",
            &cmd,
        ])
        .unwrap();
        let e = Env {
            state: StateDir::from_root(&dir.path().join("state")),
            dir,
            socket,
            tmux,
        };
        e.wait_for("the fake to draw", |e| {
            e.log().iter().any(|l| l["event"] == "ready")
        });
        e
    }

    fn pane(&self) -> PaneIdentity {
        let out = self
            .tmux
            .ok(&[
                "display-message",
                "-p",
                "-t",
                "=s:=m",
                "#{pane_id} #{pane_pid}",
            ])
            .unwrap();
        let (id, pid) = out.trim().split_once(' ').unwrap();
        PaneIdentity {
            socket: self.socket.clone(),
            session: "s".into(),
            window: "m".into(),
            pane_id: id.into(),
            pane_pid: pid.parse().unwrap(),
        }
    }

    fn control(&self, text: &str) {
        let path = self.dir.path().join("fake/control");
        fs::write(path.with_extension("tmp"), text).unwrap();
        fs::rename(path.with_extension("tmp"), path).unwrap();
        let want = text.to_string();
        self.wait_for("the fake to take its control file", |e| {
            e.log()
                .iter()
                .any(|l| l["event"] == "control" && l["text"] == want.as_str())
        });
    }

    fn keys(&self, args: &[&str]) {
        let mut a = vec!["send-keys", "-t", "=s:=m"];
        a.extend_from_slice(args);
        self.tmux.ok(&a).unwrap();
    }

    fn log(&self) -> Vec<serde_json::Value> {
        fs::read_to_string(self.dir.path().join("fake/log.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn events(&self, kind: &str) -> Vec<serde_json::Value> {
        self.log()
            .into_iter()
            .filter(|l| l["event"] == kind)
            .collect()
    }

    fn wait_for(&self, what: &str, f: impl Fn(&Env) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if f(self) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("timed out waiting for {what}; log: {:?}", self.log());
    }

    fn attempt(&self, vendor: Vendor) -> (WakeResult, usize) {
        let mut calls = 0;
        let r = wake::attempt(
            &self.tmux,
            &self.state,
            "m",
            &self.pane(),
            vendor,
            &qualified(vendor),
            &mut || {
                calls += 1;
                Ok(())
            },
        );
        (r, calls)
    }

    fn submissions(&self) -> Vec<String> {
        self.events("submit")
            .iter()
            .map(|e| e["text"].as_str().unwrap().to_string())
            .collect()
    }
}

const VENDORS: [Vendor; 2] = [Vendor::Claude, Vendor::Codex];

#[test]
fn empty_idle_member_gets_exactly_one_reminder() {
    for v in VENDORS {
        let e = Env::new(v);
        assert_eq!(e.attempt(v), (WakeResult::Submitted, 1), "{v:?}");
        assert_eq!(e.submissions(), vec![REMINDER.to_string()]);
        assert_eq!(e.events("paste").len(), 1);
    }
}

#[test]
fn drafts_defer_without_typing_and_resume_once_cleared() {
    for v in VENDORS {
        let e = Env::new(v);
        e.keys(&["-l", "half a thought"]);
        e.wait_for("the draft", |e| !e.events("key").is_empty());
        assert_eq!(e.attempt(v), (WakeResult::Deferred(Defer::Draft), 0));
        assert_eq!(e.attempt(v), (WakeResult::Deferred(Defer::Draft), 0));
        assert!(e.events("paste").is_empty());
        // The person clears it; the next attempt goes ahead.
        e.keys(&["C-u"]);
        e.wait_for("the clear", |e| !e.events("clear").is_empty());
        assert_eq!(e.attempt(v), (WakeResult::Submitted, 1), "{v:?}");
        assert_eq!(e.submissions(), vec![REMINDER.to_string()]);
    }
}

#[test]
fn multiline_and_scrolled_drafts_defer() {
    for v in VENDORS {
        let e = Env::new(v);
        e.keys(&["-l", "first"]);
        for _ in 0..60 {
            e.keys(&["C-j"]);
        }
        e.wait_for("the newlines", |e| e.events("key").len() >= 61);
        assert_eq!(
            e.attempt(v),
            (WakeResult::Deferred(Defer::Draft), 0),
            "{v:?}"
        );
        assert!(e.events("paste").is_empty());
    }
}

#[test]
fn busy_and_blocked_members_get_nothing() {
    for v in VENDORS {
        let e = Env::new(v);
        e.control("mode=working");
        assert_eq!(e.attempt(v), (WakeResult::Deferred(Defer::Busy), 0));
        e.control("mode=dialog");
        let (r, calls) = e.attempt(v);
        assert!(matches!(r, WakeResult::Blocked(_)), "{v:?}: {r:?}");
        assert_eq!(calls, 0);
        assert!(e.events("paste").is_empty());
        assert!(e.events("enter").is_empty());
    }
}

#[test]
fn a_dialog_after_the_paste_withholds_enter() {
    for v in VENDORS {
        let e = Env::new(v);
        e.control("on_paste=dialog");
        let (r, calls) = e.attempt(v);
        assert!(matches!(r, WakeResult::HumanRequired(_)), "{v:?}: {r:?}");
        assert_eq!(calls, 1);
        assert_eq!(e.events("paste").len(), 1);
        assert!(e.events("enter").is_empty(), "{v:?}");
    }
}

#[test]
fn typing_during_the_paste_withholds_enter() {
    for v in VENDORS {
        let e = Env::new(v);
        e.control("on_paste=append: and more");
        let (r, _) = e.attempt(v);
        assert!(matches!(r, WakeResult::HumanRequired(_)), "{v:?}: {r:?}");
        assert!(e.events("enter").is_empty());
        // Nothing was cleared: the text is still there for the person.
        assert!(e.events("clear").is_empty());
    }
}

#[test]
fn an_enter_that_does_not_submit_is_human_required() {
    let e = Env::new(Vendor::Claude);
    e.control("enter=ignore");
    let (r, _) = e.attempt(Vendor::Claude);
    assert!(matches!(r, WakeResult::HumanRequired(_)), "{r:?}");
    assert_eq!(e.events("enter").len(), 1);
    assert!(e.submissions().is_empty());
}

#[test]
fn a_different_pane_is_not_running() {
    let e = Env::new(Vendor::Claude);
    let mut p = e.pane();
    p.pane_pid += 1;
    let r = wake::attempt(
        &e.tmux,
        &e.state,
        "m",
        &p,
        Vendor::Claude,
        &qualified(Vendor::Claude),
        &mut || Ok(()),
    );
    assert_eq!(r, WakeResult::NotRunning);
    let mut p = e.pane();
    p.pane_id = "%999".into();
    let r = wake::attempt(
        &e.tmux,
        &e.state,
        "m",
        &p,
        Vendor::Claude,
        &qualified(Vendor::Claude),
        &mut || Ok(()),
    );
    assert_eq!(r, WakeResult::NotRunning);
    assert!(e.events("paste").is_empty());
}

#[test]
fn a_held_send_lock_defers() {
    let e = Env::new(Vendor::Claude);
    let lock = borgchart::state::open_lock(&e.state.lock_file("m")).unwrap();
    lock.lock().unwrap();
    assert_eq!(
        e.attempt(Vendor::Claude),
        (WakeResult::Deferred(Defer::Locked), 0)
    );
    drop(lock);
    assert_eq!(e.attempt(Vendor::Claude), (WakeResult::Submitted, 1));
}

#[test]
fn a_failing_intent_record_types_nothing() {
    let e = Env::new(Vendor::Codex);
    let r = wake::attempt(
        &e.tmux,
        &e.state,
        "m",
        &e.pane(),
        Vendor::Codex,
        &qualified(Vendor::Codex),
        &mut || Err(std::io::Error::other("disk full")),
    );
    assert!(
        matches!(r, WakeResult::Deferred(Defer::Uncertain(_))),
        "{r:?}"
    );
    assert!(e.events("paste").is_empty());
}

#[test]
fn unqualified_members_get_alerts_only() {
    let e = Env::new(Vendor::Claude);
    let mut facts = qualified(Vendor::Claude);
    facts.vendor_version = Some("9.9.9 (Claude Code)".into());
    let r = wake::attempt(
        &e.tmux,
        &e.state,
        "m",
        &e.pane(),
        Vendor::Claude,
        &facts,
        &mut || Ok(()),
    );
    assert!(matches!(r, WakeResult::AlertOnly(_)), "{r:?}");
    assert!(e.log().iter().all(|l| l["event"] != "paste"));
}

#[test]
fn an_attached_client_and_a_resize_do_not_matter() {
    let e = Env::new(Vendor::Claude);
    // A control-mode client stays attached for the attempt.
    let mut client = Command::new("tmux")
        .args(["-L", &e.socket, "-C", "attach", "-t", "s"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    e.tmux
        .ok(&["resize-window", "-t", "=s:=m", "-x", "90", "-y", "30"])
        .unwrap();
    assert_eq!(e.attempt(Vendor::Claude), (WakeResult::Submitted, 1));
    let _ = client.kill();
    let _ = client.wait();
}
