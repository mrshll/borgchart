//! Guarded wake tests: the input-box reader against screens recorded from
//! the real CLIs (sanitized), and `wake::attempt` against the `fake-composer`
//! stand-in on a private tmux socket.
//!
//! Run with `cargo test --features test-support --test wake`.
#![cfg(feature = "test-support")]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::time::{Duration, Instant};

use borgchart::composer::{self, Geometry, Reading, VimMode};
use borgchart::lifecycle::PaneIdentity;
use borgchart::rules;
use borgchart::state::StateDir;
use borgchart::team::Vendor;
use borgchart::tmux::Tmux;
use borgchart::wake::{self, Defer, LaunchFacts, REMINDER, Snapshot, WakeResult};

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
        "reminder-wrapped" => screen!("codex-composer-reminder-wrapped.ansi"),
        "working" => screen!("codex-composer-working.ansi"),
        _ => unreachable!(),
    }
}

/// Where the real CLI left the cursor on these screens (recorded with them).
fn at(width: usize, cursor_x: usize, cursor_y: usize) -> Option<Geometry> {
    Some(Geometry {
        width,
        cursor_x,
        cursor_y,
    })
}

fn snap(title: &str, screen: &str, geometry: Option<Geometry>) -> Snapshot {
    Snapshot {
        title: title.into(),
        screen: screen.into(),
        geometry: geometry.unwrap_or(Geometry {
            width: 200,
            cursor_x: 0,
            cursor_y: 0,
        }),
        in_mode: false,
    }
}

/// A fixture with its first REMINDER replaced, the cursor after it.
fn edited(screen: &str, with: &str) -> String {
    assert!(screen.contains(REMINDER));
    screen.replacen(REMINDER, with, 1)
}

#[test]
fn claude_input_box_readings() {
    for (name, geo, want) in [
        ("empty", at(200, 2, 47), Reading::Empty),
        ("draft", None, Reading::Draft),
        ("draft-normal", None, Reading::Draft),
        ("multiline", None, Reading::Draft),
        ("offscreen", None, Reading::Draft),
        ("wrapped", None, Reading::Draft),
        ("history-up", None, Reading::Draft),
        ("reminder", None, Reading::Draft),
        ("working", at(200, 2, 47), Reading::Busy),
    ] {
        assert_eq!(
            composer::read(Vendor::Claude, claude(name), geo),
            want,
            "{name}"
        );
    }
    for (name, geo) in [
        ("small", at(100, 2, 9)),
        // Without the cursor an empty box can't be told from typed spaces.
        ("empty", None),
        // Vim normal mode: one typed space leaves the cursor where it was.
        ("empty-normal", at(200, 2, 47)),
    ] {
        assert!(
            matches!(
                composer::read(Vendor::Claude, claude(name), geo),
                Reading::Unknown(_)
            ),
            "{name}"
        );
    }
}

#[test]
fn whitespace_only_drafts_are_never_empty() {
    // Claude: three typed spaces look like nothing but move the cursor.
    let spaces = edited(claude("reminder"), "   ");
    assert_eq!(
        composer::read(Vendor::Claude, &spaces, at(200, 5, 47)),
        Reading::Draft
    );
    assert!(!matches!(
        composer::read(Vendor::Claude, &spaces, None),
        Reading::Empty
    ));
    // Codex: typed spaces remove the placeholder.
    let spaces = edited(codex("reminder"), "   ");
    assert_eq!(
        composer::read(Vendor::Codex, &spaces, at(200, 5, 46)),
        Reading::Draft
    );
    let c = rules::load(Vendor::Claude).unwrap();
    let s = edited(claude("reminder"), " ");
    assert_eq!(
        wake::before_paste(
            Vendor::Claude,
            &c,
            &snap("✳ Claude Code", &s, at(200, 3, 47))
        ),
        Some(WakeResult::Deferred(Defer::Draft))
    );
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
        assert_eq!(
            composer::read(Vendor::Codex, codex(name), at(200, 2, 46)),
            want,
            "{name}"
        );
    }
}

#[test]
fn vim_modes_are_read() {
    let mode = |v, s| composer::view(v, s, None).unwrap().vim;
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
    let holds = |v, s: &str, g| composer::view(v, s, g).unwrap().holds(REMINDER);
    assert!(holds(Vendor::Claude, claude("reminder"), at(200, 78, 47)));
    // Wrapped by Claude in a 60-column pane: `next` could not have fitted.
    assert!(holds(
        Vendor::Claude,
        claude("reminder-wrapped"),
        at(60, 22, 47)
    ));
    assert!(holds(Vendor::Codex, codex("reminder"), at(200, 78, 46)));
    assert!(holds(
        Vendor::Codex,
        codex("reminder-wrapped"),
        at(60, 22, 46)
    ));
    // A trailing typed space only shows in the cursor.
    assert!(!holds(Vendor::Claude, claude("reminder"), at(200, 79, 47)));
    assert!(!holds(Vendor::Codex, codex("reminder"), at(200, 79, 46)));
    // Without geometry nothing passes.
    assert!(
        !composer::view(Vendor::Codex, codex("reminder"), None)
            .unwrap()
            .holds(REMINDER)
    );
    for name in ["reminder-extra", "draft", "empty", "history-up"] {
        assert!(
            !holds(Vendor::Codex, codex(name), at(200, 84, 46)),
            "codex {name}"
        );
    }
    for name in ["reminder-extra", "draft", "empty", "working", "multiline"] {
        assert!(
            !holds(Vendor::Claude, claude(name), at(200, 84, 47)),
            "claude {name}"
        );
    }
}

#[test]
fn changed_reminders_fail_the_enter_check() {
    let joined = REMINDER.replace("brg inbox", "brginbox");
    let doubled = REMINDER.replace("Run ", "Run  ");
    let leading = format!(" {REMINDER}");
    for (vendor, screen, row) in [
        (Vendor::Claude, claude("reminder"), 47),
        (Vendor::Codex, codex("reminder"), 46),
    ] {
        for text in [&joined, &doubled, &leading] {
            let s = edited(screen, text);
            let g = at(200, 2 + text.chars().count(), row);
            assert!(
                !composer::view(vendor, &s, g).unwrap().holds(REMINDER),
                "{vendor:?}: {text}"
            );
        }
    }
    // A hard newline after `Run`, where the rest would have fitted.
    let (head, tail) = REMINDER.split_once("Run ").unwrap();
    let broken = format!("{head}Run\n  {tail}");
    let s = edited(claude("reminder"), &broken);
    let g = at(200, 2 + tail.chars().count(), 48);
    let v = composer::view(Vendor::Claude, &s, g).unwrap();
    assert_eq!(v.lines.len(), 2, "{:?}", v.lines);
    assert!(!v.holds(REMINDER));
    // The real wrapped fixture read in a wider pane: that break was typed.
    assert!(
        !composer::view(Vendor::Claude, claude("reminder-wrapped"), at(200, 22, 47))
            .unwrap()
            .holds(REMINDER)
    );
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
            wake::before_paste(vendor, &rules, &snap(title, screen, None)),
            Some(WakeResult::Blocked(rule.into()))
        );
    }
}

#[test]
fn pre_paste_decisions_on_real_screens() {
    let c = rules::load(Vendor::Claude).unwrap();
    let x = rules::load(Vendor::Codex).unwrap();
    let t = "✳ Claude Code";
    let paste = |v, r, s: Snapshot| wake::before_paste(v, r, &s);
    assert_eq!(
        paste(Vendor::Claude, &c, snap(t, claude("empty"), at(200, 2, 47))),
        None
    );
    assert_eq!(
        paste(Vendor::Claude, &c, snap(t, claude("draft"), None)),
        Some(WakeResult::Deferred(Defer::Draft))
    );
    // The title stays idle while a turn runs; the grey prompt says busy.
    assert_eq!(
        paste(
            Vendor::Claude,
            &c,
            snap(t, claude("working"), at(200, 2, 47))
        ),
        Some(WakeResult::Deferred(Defer::Busy))
    );
    // Copy mode over an empty box: keys would go to tmux, not the member.
    let mut s = snap(t, claude("empty"), at(200, 2, 47));
    s.in_mode = true;
    assert!(matches!(
        wake::before_paste(Vendor::Claude, &c, &s),
        Some(WakeResult::Deferred(Defer::Uncertain(_)))
    ));
    let t = "example";
    assert_eq!(
        paste(Vendor::Codex, &x, snap(t, codex("empty"), at(200, 2, 46))),
        None
    );
    // A Codex draft hides the idle footer, so the state is unknown; the draft
    // is still reported as a draft.
    assert_eq!(
        paste(Vendor::Codex, &x, snap(t, codex("draft"), None)),
        Some(WakeResult::Deferred(Defer::Draft))
    );
    assert_eq!(
        wake::before_enter(
            Vendor::Codex,
            &x,
            &snap(t, codex("reminder"), at(200, 78, 46))
        ),
        None
    );
    assert!(
        wake::before_enter(
            Vendor::Codex,
            &x,
            &snap(t, codex("reminder-extra"), at(200, 84, 46))
        )
        .is_some()
    );
    // The reminder is in the box, but copy mode would catch the Enter.
    let mut s = snap(t, codex("reminder"), at(200, 78, 46));
    s.in_mode = true;
    assert!(wake::before_enter(Vendor::Codex, &x, &s).is_some());
    // The title can lag; the working line alone (either bullet) says busy.
    for title in ["\u{2838} example", t] {
        assert_eq!(
            paste(
                Vendor::Codex,
                &x,
                snap(title, codex("working"), at(200, 2, 46))
            ),
            Some(WakeResult::Deferred(Defer::Busy)),
            "{title}"
        );
    }
}

#[test]
fn only_a_seen_empty_box_confirms_a_submission() {
    let ok = |v, s: &str, g| wake::after_enter(v, Some(&snap("t", s, g)));
    assert!(ok(Vendor::Claude, claude("empty"), at(200, 2, 47)).is_ok());
    assert!(ok(Vendor::Claude, claude("working"), at(200, 2, 47)).is_ok());
    assert!(ok(Vendor::Codex, codex("working"), at(200, 2, 46)).is_ok());
    assert!(ok(Vendor::Claude, claude("reminder"), at(200, 78, 47)).is_err());
    assert!(ok(Vendor::Claude, claude("multiline"), None).is_err());
    assert!(ok(Vendor::Claude, claude("small"), at(100, 2, 9)).is_err());
    assert!(ok(Vendor::Claude, screen!("claude-permission.txt"), None).is_err());
    assert!(wake::after_enter(Vendor::Codex, None).is_err());
    // An empty box under copy mode doesn't show the Enter reached the member.
    let mut s = snap("t", claude("empty"), at(200, 2, 47));
    s.in_mode = true;
    assert!(wake::after_enter(Vendor::Claude, Some(&s)).is_err());
}

// ---- attempts against the fake composer ----

struct Env {
    /// First, so its server stops before the test's directory goes.
    private: common::PrivateTmux,
    dir: tempfile::TempDir,
    socket: String,
    tmux: Tmux,
    state: StateDir,
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
        let private = common::PrivateTmux::new();
        let socket = private.socket("brg-wake");
        let conf = dir.path().join("tmux.conf");
        fs::write(&conf, "set -g remain-on-exit on\n").unwrap();
        let env = BTreeMap::from([
            // The guard, then wherever this test found tmux (e.g.
            // /opt/homebrew/bin on macOS).
            (
                "PATH".to_string(),
                private.path_with(&std::env::var("PATH").unwrap_or_default()),
            ),
            (
                "BORGCHART_TMUX_TMPDIR".to_string(),
                private.dir().display().to_string(),
            ),
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
            private,
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

    /// The fake's screen as tmux shows it.
    fn screen(&self) -> String {
        self.tmux
            .ok(&["capture-pane", "-p", "-t", "=s:=m"])
            .unwrap_or_default()
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
        // On screen, not just read by the fake: the wake reads the screen.
        e.wait_for("the draft", |e| e.screen().contains("half a thought"));
        assert_eq!(e.attempt(v), (WakeResult::Deferred(Defer::Draft), 0));
        assert_eq!(e.attempt(v), (WakeResult::Deferred(Defer::Draft), 0));
        assert!(e.events("paste").is_empty());
        // The person clears it; the next attempt goes ahead.
        e.keys(&["C-u"]);
        e.wait_for("the clear", |e| {
            !e.events("clear").is_empty() && !e.screen().contains("half a thought")
        });
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
    let mut client = e
        .private
        .tmux(&e.socket)
        .args(["-C", "attach", "-t", "s"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    e.tmux
        .ok(&["resize-window", "-t", "=s:=m", "-x", "90", "-y", "30"])
        .unwrap();
    // Let the program redraw at the new size (mid-redraw reads defer).
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(e.attempt(Vendor::Claude), (WakeResult::Submitted, 1));
    let _ = client.kill();
    let _ = client.wait();
}

#[test]
fn an_unreadable_screen_after_enter_is_human_required() {
    for v in VENDORS {
        let e = Env::new(v);
        e.control("after_enter=garbage");
        let (r, _) = e.attempt(v);
        assert!(matches!(r, WakeResult::HumanRequired(_)), "{v:?}: {r:?}");
        assert_eq!(e.events("enter").len(), 1);
    }
}

#[test]
fn a_pane_is_found_by_its_socket_path() {
    let e = Env::new(Vendor::Codex);
    let path = e
        .tmux
        .ok(&["display-message", "-p", "-t", "=s:=m", "#{socket_path}"])
        .unwrap();
    let mut p = e.pane();
    p.socket = path.trim().to_string();
    assert!(p.socket.starts_with('/'));
    // The client given names another server; the pane's own socket wins.
    let other = e.tmux.with_socket("brg-wake-no-such-server");
    let r = wake::attempt(
        &other,
        &e.state,
        "m",
        &p,
        Vendor::Codex,
        &qualified(Vendor::Codex),
        &mut || Ok(()),
    );
    assert_eq!(r, WakeResult::Submitted);
    assert_eq!(e.submissions(), vec![REMINDER.to_string()]);
}
