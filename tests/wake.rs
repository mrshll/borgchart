//! Message delivery: recorded input-box screens and real sends against the
//! `fake-composer` stand-in on a private tmux socket.
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
use borgchart::wake::{self, REMINDER, Snapshot};

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
        assert_eq!(composer::read(claude(name), geo), want, "{name}");
    }
    for (name, geo) in [
        ("small", at(100, 2, 9)),
        // Without the cursor an empty box can't be told from typed spaces.
        ("empty", None),
        // Vim normal mode: one typed space leaves the cursor where it was.
        ("empty-normal", at(200, 2, 47)),
    ] {
        assert!(
            matches!(composer::read(claude(name), geo), Reading::Unknown(_)),
            "{name}"
        );
    }
}

#[test]
fn claude_rgb_busy_prompts_remain_readable_and_idle_stays_empty() {
    let busy = screen!("claude-2.1.325-busy-rgb.ansi");
    let idle = screen!("claude-2.1.325-idle.ansi");
    let other = busy.replace("38;2;153;153;153", "38;2;12;34;56");
    for (capture, expected) in [
        (busy, Reading::Busy),
        (idle, Reading::Empty),
        (&other, Reading::Busy),
    ] {
        let row = capture
            .lines()
            .enumerate()
            .filter(|(_, line)| line.contains('❯'))
            .map(|(row, _)| row)
            .last()
            .unwrap();
        assert_eq!(
            composer::view(capture, at(200, 2, row)).unwrap().reading(),
            expected
        );
    }
}

#[test]
fn dim_placeholders_do_not_block_an_empty_composer() {
    // Recorded nightly retry screen; the example variant uses the same SGR
    // styling recorded in the nightly's wake-matrix failure.
    let retry = screen!("claude-composer-retry.ansi");
    let example = retry.replace(
        "Once fixed, resend your message",
        "Try \"how do I log an error?\"",
    );
    for capture in [retry, example.as_str()] {
        assert_eq!(composer::read(capture, at(200, 2, 47)), Reading::Empty);
    }
}

#[test]
fn placeholder_recognition_never_hides_typed_or_ambiguous_drafts() {
    let retry = screen!("claude-composer-retry.ansi");
    let example = retry.replace(
        "Once fixed, resend your message",
        "Try \"how do I log an error?\"",
    );
    for capture in [retry, example.as_str()] {
        let typed = capture.replace("\x1b[2m", "");
        assert_eq!(composer::read(&typed, at(200, 2, 47)), Reading::Draft);
        assert_eq!(composer::read(capture, at(200, 3, 47)), Reading::Draft);
        assert_ne!(composer::read(capture, None), Reading::Empty);
        let mixed = capture
            .replace("fixed", "\x1b[22mfixed\x1b[2m")
            .replace("how", "\x1b[22mhow\x1b[2m");
        assert_eq!(composer::read(&mixed, at(200, 2, 47)), Reading::Draft);
        let busy = capture.replace('❯', "\x1b[2m❯");
        assert_eq!(composer::read(&busy, at(200, 2, 47)), Reading::Busy);
        let multiline = capture.replace("\x1b[0m\n", "\x1b[0m\n  draft\n");
        assert_eq!(composer::read(&multiline, at(200, 2, 47)), Reading::Draft);
    }
    let unknown = retry.replace("Once fixed, resend your message", "suggested draft");
    assert_eq!(composer::read(&unknown, at(200, 2, 47)), Reading::Draft);
}

#[test]
fn whitespace_only_drafts_are_never_empty() {
    // Claude: three typed spaces look like nothing but move the cursor.
    let spaces = edited(claude("reminder"), "   ");
    assert_eq!(composer::read(&spaces, at(200, 5, 47)), Reading::Draft);
    assert!(!matches!(composer::read(&spaces, None), Reading::Empty));
    let s = edited(claude("reminder"), " ");
    assert_eq!(composer::read(&s, at(200, 3, 47)), Reading::Draft);
}

#[test]
fn vim_modes_are_read() {
    let mode = |s| composer::view(s, None).unwrap().vim;
    assert_eq!(mode(claude("empty")), Some(VimMode::Insert));
    assert_eq!(mode(claude("empty-normal")), Some(VimMode::Normal));
}

#[test]
fn only_the_exact_reminder_passes_the_enter_check() {
    let holds = |s: &str, g| composer::view(s, g).unwrap().holds(REMINDER);
    assert!(holds(claude("reminder"), at(200, 81, 47)));
    // Wrapped by Claude in a 60-column pane: `next` could not have fitted.
    assert!(holds(claude("reminder-wrapped"), at(60, 22, 47)));
    // A trailing typed space only shows in the cursor.
    assert!(!holds(claude("reminder"), at(200, 82, 47)));
    // Without geometry nothing passes.
    assert!(
        !composer::view(claude("reminder"), None)
            .unwrap()
            .holds(REMINDER)
    );
    for name in ["reminder-extra", "draft", "empty", "working", "multiline"] {
        assert!(!holds(claude(name), at(200, 87, 47)), "claude {name}");
    }
}

#[test]
fn claude_narrow_paste_verifies_exact_payload() {
    for (capture, width, cursor_x) in [
        // The 60-column case guards the width fix; 80 columns also cover the negatives.
        (screen!("claude-composer-reminder-60.ansi"), 60, 27),
        (screen!("claude-composer-reminder-80.ansi"), 80, 8),
    ] {
        let snapshot = snap("✳ Fictional session", capture, at(width, cursor_x, 47));
        let view = composer::view(capture, Some(snapshot.geometry)).unwrap();
        assert!(view.holds_submission(REMINDER));

        for (from, to) in [("new records", "new  records"), ("msg list", "msglist")] {
            let changed = capture.replace(from, to);
            let snapshot = snap("✳ Fictional session", &changed, at(width, cursor_x, 47));
            assert!(
                !composer::view(&changed, Some(snapshot.geometry))
                    .unwrap()
                    .holds_submission(REMINDER)
            );
        }
        for moved in [cursor_x - 1, cursor_x + 1] {
            let snapshot = snap("✳ Fictional session", capture, at(width, moved, 47));
            assert!(
                !composer::view(capture, Some(snapshot.geometry))
                    .unwrap()
                    .holds_submission(REMINDER)
            );
        }
    }
}

#[test]
fn claude_hard_break_before_an_exact_fit_word_is_not_a_soft_wrap() {
    let line = "a".repeat(51);
    let capture = screen!("claude-composer-reminder-60.ansi")
        .replace(
            "brg: new records in your inbox. Run `brg msg list` at",
            &line,
        )
        .replace("your next stopping point.", "word");
    let view = composer::view(&capture, at(60, 6, 47)).unwrap();
    let text = format!("{line} word");
    assert!(!view.holds(&text));
    assert!(!view.holds_submission(&text));
}

#[test]
fn claude_long_word_wrap_does_not_withhold_valid_delivery() {
    for (capture, width, length) in [
        (screen!("claude-composer-long-word-60.ansi"), 60, 58),
        (screen!("claude-composer-long-word-80.ansi"), 80, 78),
    ] {
        let view = composer::view(capture, at(width, 4, 47)).unwrap();
        assert!(view.holds_submission(&"a".repeat(length)));
        assert!(!view.holds_submission(&"a".repeat(length + 1)));
    }
}

#[test]
fn changed_reminders_fail_the_enter_check() {
    let joined = REMINDER.replace("brg msg list", "brginbox");
    let doubled = REMINDER.replace("Run ", "Run  ");
    let leading = format!(" {REMINDER}");
    for text in [&joined, &doubled, &leading] {
        let s = edited(claude("reminder"), text);
        let g = at(200, 2 + text.chars().count(), 47);
        assert!(!composer::view(&s, g).unwrap().holds(REMINDER), "{text}");
    }
    // A hard newline after `Run`, where the rest would have fitted.
    let (head, tail) = REMINDER.split_once("Run ").unwrap();
    let broken = format!("{head}Run\n  {tail}");
    let s = edited(claude("reminder"), &broken);
    let g = at(200, 2 + tail.chars().count(), 48);
    let v = composer::view(&s, g).unwrap();
    assert_eq!(v.lines.len(), 2, "{:?}", v.lines);
    assert!(!v.holds(REMINDER));
    // The real wrapped fixture read in a wider pane: that break was typed.
    assert!(
        !composer::view(claude("reminder-wrapped"), at(200, 22, 47))
            .unwrap()
            .holds(REMINDER)
    );
}

#[test]
fn a_newline_inside_a_word_with_space_to_fit_withholds_enter() {
    let (head, tail) = REMINDER.split_once("inbox").unwrap();
    let s = edited(claude("reminder"), &format!("{head}in\n  box{tail}"));
    let g = at(200, 2 + format!("box{tail}").chars().count(), 48);
    let v = composer::view(&s, g).unwrap();
    assert_eq!(v.lines.len(), 2, "{:?}", v.lines);
    assert!(!v.holds_submission(REMINDER));
}

#[test]
fn blocked_rule_ids_are_shared_with_status() {
    // Delivery and status read the same dialog rules.
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
    ];
    for (vendor, title, screen, rule) in cases {
        let rules = rules::load(vendor).unwrap();
        assert_eq!(rules.read(title, screen).rule.as_deref(), Some(rule));
    }
}

#[test]
fn only_a_seen_empty_box_confirms_a_submission() {
    let ok = |s: &str, g| wake::after_enter(Some(&snap("t", s, g)));
    assert!(ok(claude("empty"), at(200, 2, 47)).is_ok());
    assert!(ok(claude("working"), at(200, 2, 47)).is_ok());
    assert!(ok(claude("reminder"), at(200, 81, 47)).is_err());
    assert!(ok(claude("multiline"), None).is_err());
    assert!(ok(claude("small"), at(100, 2, 9)).is_err());
    assert!(ok(screen!("claude-permission.txt"), None).is_err());
    assert!(wake::after_enter(None).is_err());
    // An empty box under copy mode doesn't show the Enter reached the member.
    let mut s = snap("t", claude("empty"), at(200, 2, 47));
    s.in_mode = true;
    assert!(wake::after_enter(Some(&s)).is_err());
}

// ---- sends against the fake composer ----

struct Env {
    /// First, so its server stops before the test's directory goes.
    private: common::PrivateTmux,
    dir: tempfile::TempDir,
    socket: String,
    tmux: Tmux,
    state: StateDir,
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
            "brg-state",
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
                "=brg-state:=m",
                "#{pane_id} #{pane_pid}",
            ])
            .unwrap();
        let (id, pid) = out.trim().split_once(' ').unwrap();
        PaneIdentity {
            socket: self
                .tmux
                .ok(&[
                    "display-message",
                    "-p",
                    "-t",
                    "=brg-state:=m",
                    "#{socket_path}",
                ])
                .unwrap()
                .trim()
                .into(),
            session: "brg-state".into(),
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

    fn show_capture(&self, capture: &str) {
        fs::write(self.dir.path().join("fake/capture"), capture).unwrap();
        self.control("mode=capture");
    }

    fn keys(&self, args: &[&str]) {
        let mut a = vec!["send-keys", "-t", "=brg-state:=m"];
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
            .ok(&["capture-pane", "-p", "-t", "=brg-state:=m"])
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

    fn submissions(&self) -> Vec<String> {
        self.events("submit")
            .iter()
            .map(|e| e["text"].as_str().unwrap().to_string())
            .collect()
    }
}

#[test]
fn a_different_pane_is_not_running() {
    let e = Env::new(Vendor::Claude);
    let mut pane = e.pane();
    pane.pane_pid += 1;
    assert!(wake::snapshot(&e.tmux, &pane).is_none());
    let mut pane = e.pane();
    pane.pane_id = "%999".into();
    assert!(wake::snapshot(&e.tmux, &pane).is_none());
    assert!(e.events("paste").is_empty());
}

#[test]
fn a_pane_is_found_by_its_socket_path() {
    let e = Env::new(Vendor::Claude);
    let pane = e.pane();
    assert!(pane.socket.starts_with('/'));
    let other = e.tmux.with_socket("brg-send-no-such-server");
    assert!(wake::snapshot(&other, &pane).is_none());
    assert!(wake::snapshot(&other.with_socket(&pane.socket), &pane).is_some());
    assert!(e.events("paste").is_empty());
}

impl Env {
    fn send_setup(&self, vendor: Vendor, locked: bool, interactive: bool) {
        use serde_json::json;
        let write = |path: &std::path::Path, value: serde_json::Value| {
            borgchart::state::write_private(path, &serde_json::to_vec(&value).unwrap()).unwrap();
        };
        let folder = self.dir.path();
        fs::write(
            folder.join("template.yaml"),
            format!(
                "roles:\n  worker: {{vendor: {}, prompt: worker.md}}\n",
                vendor.as_str()
            ),
        )
        .unwrap();
        fs::write(
            folder.join("worker.md"),
            "---\nmodels: {claude: haiku, codex: gpt-6.2-astra}\n---\nDo the task.\n",
        )
        .unwrap();
        write(
            &folder.join("registry.json"),
            json!({"schema":2,"teams":{"state":{
            "template":"sample","template_dir":folder,"folder":folder,"created_at":"2026-11-09T00:00:00Z"
        }},"issued":{}}),
        );
        write(
            &self.state.instance_file(),
            json!({"schema":2,"id":"state","template":"sample",
            "template_dir":folder,"folder":folder,"created_at":"2026-11-09T00:00:00Z","members":{"m":{"role":"worker"}}}),
        );
        write(
            &self.state.meta_file(),
            json!({"template_dir":folder,"folder":folder,
            "brg":env!("CARGO_BIN_EXE_brg"),
            "config":{"version":1,"digest":"00","inputs":{},"template_dirs":{}},
            "socket":"brg-wake-test","windows_started":true,
            "base_env":{"PATH":format!("{}:{}", folder.join("fakebin").display(), self.private.path_with(&std::env::var("PATH").unwrap_or_default())),"HOME":folder,"QUEUE_RECIPIENT_MARKER":"recipient-only"},"plugins":{},"members":{"m":{
                "role":"worker","vendor":vendor.as_str(),"posture":if locked {"locked"} else {"vendor"},
                "interactive":interactive,"started":true,"parent":null,"vendor_version":null
            }}}),
        );
        write(
            &self.state.current_incarnation("m"),
            json!({"member":"m","incarnation":"m-1",
            "started_at":"2026-11-09T00:00:00Z","pane":self.pane()}),
        );
        borgchart::bus::layout::StorePaths::new(&self.state)
            .initialize(&self.state)
            .unwrap();
        if locked {
            fs::write(self.state.gate_marker("m"), "active").unwrap();
        }
    }

    fn brg(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = common::command(env!("CARGO_BIN_EXE_brg"));
        cmd.env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.dir.path().join("fakebin").display(),
                    self.private
                        .path_with(&std::env::var("PATH").unwrap_or_default())
                ),
            )
            .env("HOME", self.dir.path())
            .env("QUEUE_SENDER_MARKER", "sender-only")
            .env("BORGCHART_STATE_HOME", self.dir.path())
            .env("BORGCHART_CONFIG", self.dir.path().join("unused.yaml"))
            .args(args);
        self.private.brg_env(&mut cmd, &self.socket);
        cmd.output().unwrap()
    }

    fn status_row(&self) -> serde_json::Value {
        let out = self.brg(&["status", "state", "--json"]);
        assert!(out.status.success(), "{out:?}");
        let rows: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
        rows.into_iter().find(|r| r["member"] == "m").unwrap()
    }

    fn send_message(&self, text: &str, extra: &[&str]) -> (i32, serde_json::Value) {
        let mut args = vec![
            "msg",
            "send",
            "state/m",
            text,
            "--external",
            "test",
            "--json",
        ];
        args.extend_from_slice(extra);
        let out = self.brg(&args);
        let value = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| panic!("{out:?}"));
        (out.status.code().unwrap(), value)
    }
}

#[test]
fn unified_send_types_into_working_composers_and_keeps_a_record() {
    let vendor = Vendor::Claude;
    let e = Env::new(vendor);
    e.send_setup(vendor, false, false);
    e.control("mode=working");
    let (code, out) = e.send_message("hello", &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out["delivery"], "typed", "{out}");
    let submits = e.events("submit");
    assert_eq!(submits.len(), 1, "{submits:?}");
    let body = submits[0]["text"].as_str().unwrap();
    assert!(body.contains("[brg m1 from external:test]"), "{body}");
    assert!(body.contains("hello"), "{body}");
    assert!(body.contains("brg msg ack m1"), "{body}");
    let bus = borgchart::bus::store::open(&e.state).unwrap();
    let snap = borgchart::bus::store::snapshot(&bus).unwrap();
    assert_eq!(snap.records[&1].body, "hello");
    assert!(snap.records[&1].ack_required);
    assert!(snap.acks.is_empty());
}

#[test]
fn claude_wrapped_typed_send_is_submitted_without_uncertainty() {
    let e = Env::new(Vendor::Claude);
    e.send_setup(Vendor::Claude, false, false);
    let text = "x".repeat(160);
    let (code, out) = e.send_message(&text, &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out["delivery"], "typed", "{out}");
    let submissions = e.submissions();
    assert_eq!(submissions.len(), 1);
    assert!(submissions[0].contains(&text));
}

#[test]
fn unified_send_preserves_drafts_and_skips_interactive_messages() {
    for (interactive, text, draft) in [
        (true, "hello".to_string(), false),
        (false, "hello".to_string(), true),
    ] {
        let e = Env::new(Vendor::Claude);
        e.send_setup(Vendor::Claude, false, interactive);
        if draft {
            e.keys(&["draft"]);
        }
        let (code, out) = e.send_message(&text, &["--allow-unknown"]);
        assert_eq!(code, 0, "{out}");
        assert_eq!(
            out["delivery"],
            if draft { "blocked" } else { "stored" },
            "{out}"
        );
        assert!(e.events("paste").is_empty());
        assert!(e.events("submit").is_empty());
        if draft {
            assert!(e.screen().contains("draft"));
        }
    }
}

#[test]
fn claude_long_message_is_typed_as_a_pointer_and_submitted() {
    for body in ["one\ntwo\nthree\nfour\nfive\nsix".into(), "x".repeat(801)] {
        let e = Env::new(Vendor::Claude);
        e.send_setup(Vendor::Claude, false, false);
        e.control("fold=on");
        let (code, out) = e.send_message(&body, &[]);
        assert_eq!(code, 0, "{out}");
        assert_eq!(out["delivery"], "typed", "{out}");
        let submitted = e.submissions();
        assert_eq!(submitted.len(), 1);
        assert_eq!(submitted[0].lines().count(), 1);
        assert!(submitted[0].contains("msg read m1"), "{submitted:?}");
        let after = wake::snapshot(&e.tmux, &e.pane()).unwrap();
        assert!(
            composer::view(&after.screen, Some(after.geometry))
                .unwrap()
                .lines
                .iter()
                .all(String::is_empty)
        );
        let bus = borgchart::bus::store::open(&e.state).unwrap();
        assert_eq!(
            borgchart::bus::store::snapshot(&bus).unwrap().records[&1].body,
            body
        );
    }
}

#[test]
fn claude_short_message_is_still_typed_in_full() {
    let e = Env::new(Vendor::Claude);
    e.send_setup(Vendor::Claude, false, false);
    e.control("fold=on");
    let header = "[brg m1 from external:test]\n";
    let footer = "\n[Ignore duplicate delivery if already acked; otherwise brg msg ack m1 when taking responsibility.]";
    let body = "x".repeat(800 - header.len() - footer.len());
    let (code, out) = e.send_message(&body, &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out["delivery"], "typed", "{out}");
    let submitted = e.submissions();
    assert_eq!(submitted.len(), 1);
    assert_eq!(submitted[0], format!("{header}{body}{footer}"));
    assert_eq!(submitted[0].len(), 800);
}

#[test]
fn locked_pane_shrinking_after_preflight_does_not_receive_an_unusable_pointer() {
    let e = Env::new(Vendor::Claude);
    e.send_setup(Vendor::Claude, true, false);
    let gate = e.dir.path().join("publish-pause");
    let mut cmd = common::command(env!("CARGO_BIN_EXE_brg"));
    cmd.env_clear()
        .env(
            "PATH",
            e.private
                .path_with(&std::env::var("PATH").unwrap_or_default()),
        )
        .env("HOME", e.dir.path())
        .env("BORGCHART_STATE_HOME", e.dir.path())
        .env("BORGCHART_CONFIG", e.dir.path().join("unused.yaml"))
        .env("BORGCHART_TEST_PUBLISH_PAUSE", &gate)
        .args([
            "msg",
            "send",
            "state/m",
            &"x".repeat(600),
            "--external",
            "test",
            "--json",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    e.private.brg_env(&mut cmd, &e.socket);
    let child = cmd.spawn().unwrap();
    e.wait_for("the completed preflight", |_| {
        gate.with_extension("waiting").exists()
    });
    e.tmux
        .ok(&["resize-window", "-t", "=brg-state:=m", "-x", "60"])
        .unwrap();
    e.wait_for("the smaller composer", |e| {
        wake::snapshot(&e.tmux, &e.pane()).is_some_and(|s| {
            s.geometry.width == 60
                && composer::view(&s.screen, Some(s.geometry))
                    .is_ok_and(|v| v.reading() == Reading::Empty)
        })
    });
    fs::write(&gate, b"").unwrap();
    let out = child.wait_with_output().unwrap();
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(5), "{value}");
    assert_eq!(value["delivery"], "recorded-not-delivered", "{value}");
    assert!(value["id"].is_string(), "{value}");
    assert!(e.events("paste").is_empty());
    assert!(e.events("enter").is_empty());
}

#[test]
fn claude_queued_placeholder_stays_stored_without_a_blocked_alert_or_paste() {
    let e = Env::new(Vendor::Claude);
    e.send_setup(Vendor::Claude, false, false);
    let placeholder = "Press up to edit queued messages";
    e.keys(&[placeholder]);
    e.wait_for("the queued placeholder", |e| {
        e.screen().contains(placeholder)
    });
    let (code, out) = e.send_message("task", &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out["delivery"], "stored", "{out}");
    assert!(e.events("paste").is_empty());
    assert!(e.events("enter").is_empty());
    let status = e.status_row();
    assert_eq!(status["delivery"]["outcome"], "stored", "{status}");
    assert!(e.screen().contains(placeholder));
}

#[test]
fn claude_empty_normal_mode_stays_stored_without_claiming_a_draft() {
    let e = Env::new(Vendor::Claude);
    e.send_setup(Vendor::Claude, false, false);
    e.keys(&["Escape"]);
    e.wait_for("normal mode", |e| !e.screen().contains("-- INSERT --"));
    let (code, out) = e.send_message("task", &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out["delivery"], "stored", "{out}");
    assert!(e.events("paste").is_empty());
    assert!(e.events("enter").is_empty());
    assert_eq!(e.status_row()["delivery"]["outcome"], "stored");
}

#[test]
fn claude_acked_block_does_not_hide_the_next_delivery_episode() {
    let e = Env::new(Vendor::Claude);
    e.send_setup(Vendor::Claude, false, false);
    e.control("mode=dialog");
    let (code, first) = e.send_message("first", &[]);
    assert_eq!(code, 0, "{first}");
    assert_eq!(first["delivery"], "blocked", "{first}");
    assert_eq!(e.status_row()["delivery"]["id"], first["id"]);
    e.control("mode=idle");
    let bus = borgchart::bus::store::open(&e.state).unwrap();
    let actor = borgchart::bus::context::Caller::Member {
        member: "m".into(),
        incarnation: "m-1".into(),
    };
    borgchart::bus::store::ack(&bus, &actor, &["m1".into()]).unwrap();
    assert!(e.status_row().get("delivery").is_none());
    e.keys(&["draft"]);
    e.wait_for("the new draft", |e| e.screen().contains("draft"));
    let (code, second) = e.send_message("second", &[]);
    assert_eq!(code, 0, "{second}");
    assert_eq!(second["delivery"], "blocked", "{second}");
    let status = e.status_row();
    assert_eq!(status["delivery"]["id"], second["id"], "{status}");
    assert_eq!(status["delivery"]["reason"], second["reason"], "{status}");
}

#[test]
fn claude_draft_makes_later_sends_blocked_and_status_keeps_the_first_cause() {
    let e = Env::new(Vendor::Claude);
    e.send_setup(Vendor::Claude, false, false);
    e.control("on_paste=append: leftover");
    let (code, first) = e.send_message("first", &[]);
    assert_eq!(code, 0, "{first}");
    assert_eq!(first["delivery"], "uncertain", "{first}");
    let first_status = e.status_row()["delivery"].clone();
    assert_eq!(first_status["id"], first["id"]);
    assert!(first_status["at"].is_string());
    for text in ["second", "third"] {
        let (code, later) = e.send_message(text, &[]);
        assert_eq!(code, 0, "{later}");
        assert_eq!(later["delivery"], "blocked", "{later}");
        assert_eq!(e.status_row()["delivery"], first_status);
    }
    assert_eq!(e.events("paste").len(), 1);
    assert!(e.events("submit").is_empty());
    let bus = borgchart::bus::store::open(&e.state).unwrap();
    assert_eq!(
        borgchart::bus::store::snapshot(&bus).unwrap().records.len(),
        3
    );
    e.control("on_paste=none");
    e.keys(&["C-u"]);
    e.wait_for("the draft to clear", |e| !e.events("clear").is_empty());
    assert_eq!(e.send_message("resume", &[]).1["delivery"], "typed");
    assert!(e.status_row().get("delivery").is_none());
}

#[test]
fn claude_dialog_and_copy_mode_block_delivery_without_typing() {
    for mode in ["dialog", "copy"] {
        let e = Env::new(Vendor::Claude);
        e.send_setup(Vendor::Claude, false, false);
        if mode == "dialog" {
            e.control("mode=dialog");
        } else {
            e.tmux.ok(&["copy-mode", "-t", "=brg-state:=m"]).unwrap();
        }
        let (code, out) = e.send_message("hello", &[]);
        assert_eq!(code, 0, "{out}");
        assert_eq!(out["delivery"], "blocked", "{out}");
        assert!(e.events("paste").is_empty());
        let first_status = e.status_row()["delivery"].clone();
        assert_eq!(first_status["id"], out["id"]);
        assert_eq!(e.send_message("next", &[]).1["delivery"], "blocked");
        assert_eq!(e.status_row()["delivery"], first_status);
    }
}

#[test]
fn claude_idle_draft_with_unacked_work_shows_attach_hint() {
    let e = Env::new(Vendor::Claude);
    e.send_setup(Vendor::Claude, false, false);
    e.keys(&["draft"]);
    e.wait_for("the draft", |e| e.screen().contains("draft"));
    assert!(
        !e.status_row()["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("brg attach state/m")
    );
    e.send_message("task", &[]);
    // Status must diagnose the box itself, even without a delivery alert.
    fs::remove_file(e.state.member_dir("m").join("delivery.json")).unwrap();
    let row = e.status_row();
    assert_eq!(row["state"], "idle", "{row}");
    let detail = row["detail"].as_str().unwrap();
    assert!(detail.contains("brg attach state/m"), "{detail}");
}

#[test]
fn claude_dialog_status_shows_its_rule_and_attach_hint() {
    let e = Env::new(Vendor::Claude);
    e.send_setup(Vendor::Claude, false, false);
    e.control("mode=dialog");
    let row = e.status_row();
    assert_eq!(row["state"], "blocked", "{row}");
    let detail = row["detail"].as_str().unwrap();
    assert!(detail.contains(row["rule"].as_str().unwrap()), "{detail}");
    assert!(detail.contains("brg attach state/m"), "{detail}");
}

impl Env {
    /// A stand-in `codex` on the sender's PATH that records its arguments
    /// and then runs `then` (a shell snippet: `exit 0`, `exit 3`, `exec sleep 60`).
    fn fake_codex(&self, then: &str) {
        let bin = self.dir.path().join("fakebin");
        fs::create_dir_all(&bin).unwrap();
        let log = self.dir.path().join("codex-args");
        let script = bin.join("codex");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf 'call\\n' >> '{}'\nfor a in \"$@\"; do printf '%s\\0' \"$a\"; done > '{}'\n{then}\n",
                log.with_extension("calls").display(),
                log.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The arguments the stand-in `codex` was called with, if it was.
    fn codex_args(&self) -> Option<Vec<String>> {
        let raw = fs::read_to_string(self.dir.path().join("codex-args")).ok()?;
        Some(raw.split_terminator('\0').map(String::from).collect())
    }

    /// The vendor session the member's start reported, as its hook would.
    fn record_session(&self, id: &str) {
        borgchart::state::write_private(
            &self.state.member_session("m"),
            format!("{id}\n").as_bytes(),
        )
        .unwrap();
    }
}

/// Every Codex posture queues the next turn without reading or changing a draft.
#[test]
fn codex_recipients_queue_without_touching_a_draft_in_every_posture() {
    for (locked, interactive) in [(false, true), (false, false), (true, false)] {
        let e = Env::new(Vendor::Codex);
        e.send_setup(Vendor::Codex, locked, interactive);
        e.fake_codex("echo 'Queued message 1 for thread 2.'; exit 0");
        e.record_session("01a1d300-f48d-7c70-a58a-d5fbea1edd20");
        e.keys(&["draft"]);
        let (code, out) = e.send_message("hello", &[]);
        assert_eq!(code, 0, "{out}");
        assert_eq!(out["delivery"], "queued", "{out}");
        let args = e.codex_args().expect("codex queue was called");
        assert_eq!(
            args[..4],
            [
                "queue",
                "--thread",
                "01a1d300-f48d-7c70-a58a-d5fbea1edd20",
                "--message"
            ]
        );
        assert_eq!(args.len(), 5, "{args:?}");
        assert!(args[4].contains("\nhello"), "{args:?}");
        assert_eq!(args[4].contains("brg msg ack m1"), !locked, "{args:?}");
        // The person's draft is untouched and nothing was pasted.
        assert!(e.events("paste").is_empty());
        assert!(e.screen().contains("draft"));
    }
}

/// Without a session for this start, or when `codex queue` fails,
/// the message stays stored and the send still succeeds.
#[test]
fn codex_messages_without_a_current_session_or_queue_success_stay_stored() {
    for case in ["no session", "stale session", "unreadable start", "fails"] {
        let e = Env::new(Vendor::Codex);
        e.send_setup(Vendor::Codex, false, true);
        e.fake_codex(match case {
            "fails" => "exit 3",
            "hangs" => "exec sleep 60",
            _ => "exit 0",
        });
        match case {
            "no session" => {}
            "stale session" => {
                e.record_session("01a1d300-f48d-7c70-a58a-d5fbea1edd20");
                // A later start that hasn't reported its session yet.
                let path = e.state.current_incarnation("m");
                let text = fs::read_to_string(&path).unwrap();
                let later = text.replace("2026-11-09T00:00:00Z", "2099-01-01T00:00:00Z");
                assert_ne!(text, later);
                fs::write(&path, later).unwrap();
            }
            "unreadable start" => {
                e.record_session("01a1d300-f48d-7c70-a58a-d5fbea1edd20");
                let path = e.state.current_incarnation("m");
                let text = fs::read_to_string(&path).unwrap();
                fs::write(&path, text.replace("2026-11-09T00:00:00Z", "unreadable")).unwrap();
            }
            _ => e.record_session("01a1d300-f48d-7c70-a58a-d5fbea1edd20"),
        }
        let started = Instant::now();
        let (code, out) = e.send_message("hello", &[]);
        assert_eq!(code, 0, "{case}: {out}");
        assert_eq!(out["delivery"], "stored", "{case}: {out}");
        assert!(!out["reason"].as_str().unwrap().is_empty(), "{case}: {out}");
        assert!(started.elapsed() < Duration::from_secs(30), "{case}");
        if case.contains("session") || case.contains("start") {
            assert!(e.codex_args().is_none(), "{case}");
        }
        assert!(e.events("paste").is_empty(), "{case}");
        let bus = borgchart::bus::store::open(&e.state).unwrap();
        let snap = borgchart::bus::store::snapshot(&bus).unwrap();
        assert_eq!(snap.records[&1].body, "hello", "{case}");
    }
}

/// A member stopped or started again while the record was being stored
/// gets nothing queued on its old session: the message stays stored for
/// whichever start reads its inbox next.
#[test]
fn queue_delivery_rechecks_the_start_after_the_record_is_stored() {
    for case in ["stopped", "started again"] {
        let e = Env::new(Vendor::Codex);
        e.send_setup(Vendor::Codex, false, true);
        e.fake_codex("exit 0");
        e.record_session("01a1d300-f48d-7c70-a58a-d5fbea1edd20");
        let gate = e.dir.path().join("publish-pause");
        let mut cmd = common::command(env!("CARGO_BIN_EXE_brg"));
        cmd.env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    e.dir.path().join("fakebin").display(),
                    e.private
                        .path_with(&std::env::var("PATH").unwrap_or_default())
                ),
            )
            .env("HOME", e.dir.path())
            .env("BORGCHART_STATE_HOME", e.dir.path())
            .env("BORGCHART_CONFIG", e.dir.path().join("unused.yaml"))
            .env("BORGCHART_TEST_PUBLISH_PAUSE", &gate)
            .args([
                "msg",
                "send",
                "state/m",
                "hello",
                "--external",
                "test",
                "--json",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        e.private.brg_env(&mut cmd, &e.socket);
        let child = cmd.spawn().unwrap();
        e.wait_for("send to read the start", |_| {
            gate.with_extension("waiting").exists()
        });
        if case == "stopped" {
            borgchart::lifecycle::end_incarnation(&e.state, "m", "stopped").unwrap();
        } else {
            let mut inc = borgchart::lifecycle::load_incarnation(&e.state, "m")
                .unwrap()
                .unwrap();
            inc.incarnation = "m-2".into();
            inc.started_at = borgchart::bus::time::utc_now();
            inc.pane = None;
            borgchart::lifecycle::begin_incarnation(&e.state, &inc).unwrap();
        }
        fs::write(&gate, b"").unwrap();
        let out = child.wait_with_output().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(out.status.code(), Some(0), "{case}: {value}");
        assert_eq!(value["delivery"], "stored", "{case}: {value}");
        assert!(e.codex_args().is_none(), "{case}");
    }
}

#[test]
fn unified_send_rechecks_dialogs_after_paste_and_reports_locked_uncertainty() {
    for locked in [false, true] {
        let e = Env::new(Vendor::Claude);
        e.send_setup(Vendor::Claude, locked, false);
        e.control("on_paste=dialog");
        let (code, out) = e.send_message("hello", &[]);
        assert_eq!(code, if locked { 5 } else { 0 }, "{out}");
        assert_eq!(out["id"], "m1");
        assert_eq!(out["delivery"], "uncertain", "{out}");
        assert!(e.events("submit").is_empty());
        let bus = borgchart::bus::store::open(&e.state).unwrap();
        let snap = borgchart::bus::store::snapshot(&bus).unwrap();
        assert_eq!(snap.records[&1].ack_required, !locked);
        assert_eq!(
            borgchart::bus::store::progress(&bus, &snap)["state/m"].unread,
            usize::from(!locked)
        );
        if locked {
            let actor = borgchart::bus::context::Caller::Member {
                member: "m".into(),
                incarnation: "m-1".into(),
            };
            let ack = borgchart::bus::store::ack(&bus, &actor, &["m1".into()]).unwrap();
            assert_eq!(ack[0].1, borgchart::bus::store::AckOutcome::NotRequired);
            assert!(!bus.paths.ack("m1").exists());
            assert!(
                borgchart::lifecycle::open_work(&e.state)
                    .unwrap()
                    .items
                    .is_empty()
            );
        }
    }
}

#[test]
fn locked_dialog_is_refused_before_recording_even_with_allow_unknown() {
    let e = Env::new(Vendor::Claude);
    e.send_setup(Vendor::Claude, true, false);
    e.control("mode=dialog");
    let (code, out) = e.send_message("hello", &["--allow-unknown"]);
    assert_eq!(code, 4, "{out}");
    assert!(out.get("id").is_none());
    let bus = borgchart::bus::store::open(&e.state).unwrap();
    assert!(
        borgchart::bus::store::snapshot(&bus)
            .unwrap()
            .records
            .is_empty()
    );
    assert!(e.events("paste").is_empty());
}

#[test]
fn locked_delivery_has_no_ack_hint_and_stays_no_ack_after_posture_change() {
    let e = Env::new(Vendor::Claude);
    e.send_setup(Vendor::Claude, true, false);
    let (code, out) = e.send_message("hello", &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out["delivery"], "typed", "{out}");
    let submissions = e.events("submit");
    let text = submissions[0]["text"].as_str().unwrap();
    assert!(text.contains("ack not required"), "{text}");
    assert!(!text.contains("brg msg ack"), "{text}");
    e.send_setup(Vendor::Claude, false, false);
    let bus = borgchart::bus::store::open(&e.state).unwrap();
    let snap = borgchart::bus::store::snapshot(&bus).unwrap();
    assert!(!snap.records[&1].ack_required);
    assert_eq!(
        borgchart::bus::store::progress(&bus, &snap)["state/m"].unread,
        0
    );
    let wrong_actor = borgchart::bus::context::Caller::External {
        name: "test".into(),
    };
    assert!(borgchart::bus::store::ack(&bus, &wrong_actor, &["m1".into()]).is_err());
}

#[test]
fn enter_without_observed_submission_is_uncertain_and_never_retried() {
    let e = Env::new(Vendor::Claude);
    e.send_setup(Vendor::Claude, true, false);
    e.control("enter=ignore");
    let (code, out) = e.send_message("hello", &[]);
    assert_eq!(code, 5, "{out}");
    assert_eq!(out["delivery"], "uncertain", "{out}");
    assert_eq!(out["id"], "m1");
    assert_eq!(e.events("enter").len(), 1);
    assert!(e.events("submit").is_empty());
}

#[test]
fn locked_oversize_refuses_before_commit_or_paste() {
    let e = Env::new(Vendor::Claude);
    e.send_setup(Vendor::Claude, true, false);
    let (code, out) = e.send_message("one\ntwo\nthree", &[]);
    assert_eq!(code, 5, "{out}");
    assert!(out.get("id").is_none());
    assert!(e.events("paste").is_empty());
    let bus = borgchart::bus::store::open(&e.state).unwrap();
    assert!(
        borgchart::bus::store::snapshot(&bus)
            .unwrap()
            .records
            .is_empty()
    );
}

#[test]
fn an_ack_file_for_a_locked_record_is_corrupt_not_consumption() {
    let e = Env::new(Vendor::Claude);
    e.send_setup(Vendor::Claude, true, false);
    assert_eq!(e.send_message("hello", &[]).0, 0);
    let bus = borgchart::bus::store::open(&e.state).unwrap();
    let actor = borgchart::bus::context::Caller::Member {
        member: "m".into(),
        incarnation: "m-1".into(),
    };
    assert_eq!(
        borgchart::bus::store::ack(&bus, &actor, &["m1".into()]).unwrap()[0].1,
        borgchart::bus::store::AckOutcome::NotRequired
    );
    assert!(
        !bus.paths.acks_dir().exists()
            || fs::read_dir(bus.paths.acks_dir()).unwrap().next().is_none()
    );
    borgchart::state::write_private(
        &bus.paths.ack("m1"),
        &serde_json::to_vec(&serde_json::json!({
            "schema":4,"id":"m1","at":"2026-11-09T00:00:00Z",
            "by":{"type":"member","member":"state/m","incarnation":"m-1"}
        }))
        .unwrap(),
    )
    .unwrap();
    let snap = borgchart::bus::store::snapshot(&bus).unwrap();
    assert!(!snap.healthy());
    assert!(
        snap.problems
            .iter()
            .any(|p| p.contains("does not require acknowledgement"))
    );
    assert!(!snap.is_acked(1));
}

#[test]
fn cross_edge_sends_type_and_preflight_locked_recipients_in_their_own_team() {
    use serde_json::{Value, json};
    for (sender, recipient, locked, sibling) in [
        ("state/copy/lead", "state/m", false, false),
        ("state/copy/lead", "state/m", true, false),
        ("state/m", "state/copy/lead", false, false),
        ("state/copy/scout", "state/copy/lead", false, true),
    ] {
        let e = Env::new(Vendor::Claude);
        e.send_setup(Vendor::Claude, locked, false);
        let write = |path: &std::path::Path, value: Value| {
            borgchart::state::write_private(path, &serde_json::to_vec(&value).unwrap()).unwrap();
        };
        let read = |path: &std::path::Path| -> Value {
            serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
        };
        let registry_path = e.dir.path().join("registry.json");
        let mut registry = read(&registry_path);
        registry["teams"]["state/copy"] = registry["teams"]["state"].clone();
        write(&registry_path, registry);
        let mut root = read(&e.state.instance_file());
        root["child_teams"] = json!({"copy": {"role": "dev"}});
        write(&e.state.instance_file(), root.clone());
        let child = e.state.root.join("copy");
        root["id"] = json!("state/copy");
        root["parent"] = json!("state/m");
        root["members"] = json!({"lead": {"role": "lead"}, "scout": {"role": "scout"}});
        root["child_teams"] = json!({});
        write(&child.join("team.json"), root);
        let mut meta = read(&e.state.meta_file());
        let mut member = meta["members"]["m"].clone();
        member["parent"] = json!("state/m");
        member["posture"] = json!("vendor");
        meta["members"] = json!({"lead": member, "scout": member});
        write(&child.join("start.json"), meta);
        for member in ["lead", "scout"] {
            write(
                &child.join(format!("members/{member}/incarnation.json")),
                json!({
                    "member": member,
                    "incarnation": format!("{member}-1"),
                    "started_at": "2026-11-09T00:00:00Z"
                }),
            );
        }
        if recipient == "state/copy/lead" {
            let mut parent_incarnation = read(&e.state.current_incarnation("m"));
            let pane = parent_incarnation
                .as_object_mut()
                .unwrap()
                .remove("pane")
                .unwrap();
            write(&e.state.current_incarnation("m"), parent_incarnation);
            let path = child.join("members/lead/incarnation.json");
            let mut child_incarnation = read(&path);
            child_incarnation["pane"] = pane;
            write(&path, child_incarnation);
        }
        let send = || {
            let (team, member) = sender.rsplit_once('/').unwrap();
            let mut cmd = common::command(env!("CARGO_BIN_EXE_brg"));
            cmd.env_clear()
                .env(
                    "PATH",
                    e.private
                        .path_with(&std::env::var("PATH").unwrap_or_default()),
                )
                .env("HOME", e.dir.path())
                .env("BORGCHART_STATE_HOME", e.dir.path())
                .env("BORGCHART_TEAM", team)
                .env("BORGCHART_MEMBER", member)
                .env("BORGCHART_INCARNATION", format!("{member}-1"))
                .args(["msg", "send", recipient, "Routing check", "--json"]);
            e.private.brg_env(&mut cmd, &e.socket);
            cmd.output().unwrap()
        };
        if locked {
            e.keys(&["draft"]);
            let refused = send();
            assert_eq!(refused.status.code(), Some(5), "{refused:?}");
            let snap =
                borgchart::bus::store::snapshot(&borgchart::bus::store::open(&e.state).unwrap())
                    .unwrap();
            assert!(
                snap.records.is_empty(),
                "locked preflight must precede publication"
            );
            e.keys(&["C-u"]);
        }
        let out = send();
        let result: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(out.status.success(), "{result}");
        assert_eq!(result["delivery"], "typed", "{result}");
        let submissions = e.submissions();
        assert_eq!(submissions.len(), 1);
        assert!(submissions[0].contains(&format!("[brg m1 from {sender}")));
        assert_eq!(
            submissions[0].contains("not your parent"),
            sibling,
            "{sender} -> {recipient}: {}",
            submissions[0]
        );
        let snap = borgchart::bus::store::snapshot(&borgchart::bus::store::open(&e.state).unwrap())
            .unwrap();
        assert_eq!(snap.records[&1].ack_required, !locked);
    }
}

#[test]
fn locked_codex_missing_readiness_refuses_without_recording() {
    for case in ["no session", "inactive gate", "stopped", "dead pane"] {
        let e = Env::new(Vendor::Codex);
        e.send_setup(Vendor::Codex, true, false);
        e.fake_codex("exit 0");
        if case != "no session" {
            e.record_session("01a1d300-f48d-7c70-a58a-d5fbea1edd20");
        }
        if case == "inactive gate" {
            fs::remove_file(e.state.gate_marker("m")).unwrap();
        }
        if case == "stopped" {
            borgchart::lifecycle::end_incarnation(&e.state, "m", "stopped").unwrap();
        }
        if case == "dead pane" {
            e.tmux.ok(&["kill-pane", "-t", "=brg-state:=m"]).unwrap();
        }
        let (code, out) = e.send_message("hello", &["--allow-unknown"]);
        assert_ne!(code, 0, "{case}: {out}");
        if case == "dead pane" {
            assert_eq!(code, 3, "{case}: {out}");
        }
        assert!(out.get("id").is_none(), "{case}: {out}");
        let bus = borgchart::bus::store::open(&e.state).unwrap();
        assert!(
            borgchart::bus::store::snapshot(&bus)
                .unwrap()
                .records
                .is_empty(),
            "{case}"
        );
        assert!(e.codex_args().is_none(), "{case}");
        assert!(e.events("paste").is_empty(), "{case}");
    }
}

#[test]
fn codex_queue_failure_preserves_the_record_and_reports_locked_failure() {
    for locked in [false, true] {
        let e = Env::new(Vendor::Codex);
        e.send_setup(Vendor::Codex, locked, false);
        e.fake_codex("exit 3");
        e.record_session("01a1d300-f48d-7c70-a58a-d5fbea1edd20");
        let (code, out) = e.send_message("hello", &[]);
        assert_eq!(code, if locked { 5 } else { 0 }, "{out}");
        assert_eq!(
            out["delivery"],
            if locked {
                "recorded-not-delivered"
            } else {
                "stored"
            },
            "{out}"
        );
        assert_eq!(out["id"], "m1");
        let bus = borgchart::bus::store::open(&e.state).unwrap();
        assert_eq!(
            borgchart::bus::store::snapshot(&bus).unwrap().records[&1].body,
            "hello"
        );
    }
}

#[test]
fn codex_queue_timeout_is_uncertain_and_never_retried() {
    for locked in [false, true] {
        let e = Env::new(Vendor::Codex);
        e.send_setup(Vendor::Codex, locked, !locked);
        e.fake_codex("exec sleep 60");
        e.record_session("01a1d300-f48d-7c70-a58a-d5fbea1edd20");
        let start = Instant::now();
        let (code, out) = e.send_message("hello", &[]);
        assert_eq!(code, if locked { 5 } else { 0 }, "{out}");
        assert_eq!(out["delivery"], "uncertain", "{out}");
        assert!(start.elapsed() < Duration::from_secs(20));
        assert!(e.codex_args().is_some());
        assert_eq!(
            fs::read_to_string(e.dir.path().join("codex-args.calls"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        assert!(e.events("paste").is_empty());
    }
}

#[test]
fn codex_queue_uses_the_recipients_environment() {
    let e = Env::new(Vendor::Codex);
    e.send_setup(Vendor::Codex, false, true);
    e.fake_codex(
        "test \"$QUEUE_RECIPIENT_MARKER\" = recipient-only && test -z \"${QUEUE_SENDER_MARKER+x}\"",
    );
    e.record_session("01a1d300-f48d-7c70-a58a-d5fbea1edd20");
    let (code, out) = e.send_message("hello", &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out["delivery"], "queued", "{out}");
}

#[test]
fn codex_queue_waits_for_a_young_starts_session() {
    for locked in [false, true] {
        let e = Env::new(Vendor::Codex);
        e.send_setup(Vendor::Codex, locked, false);
        e.fake_codex("exit 0");
        let mut inc = borgchart::lifecycle::load_incarnation(&e.state, "m")
            .unwrap()
            .unwrap();
        inc.started_at = borgchart::bus::time::utc_now();
        borgchart::lifecycle::begin_incarnation(&e.state, &inc).unwrap();
        std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(Duration::from_millis(500));
                e.record_session("01a1d300-f48d-7c70-a58a-d5fbea1edd20");
            });
            let (code, out) = e.send_message("hello", &[]);
            assert_eq!(code, 0, "{out}");
            assert_eq!(out["delivery"], "queued", "{out}");
        });
    }
}

#[test]
fn codex_queue_accepts_a_full_body_without_the_typing_budget() {
    for locked in [false, true] {
        let e = Env::new(Vendor::Codex);
        e.send_setup(Vendor::Codex, locked, false);
        e.fake_codex("exit 0");
        e.record_session("01a1d300-f48d-7c70-a58a-d5fbea1edd20");
        let body = "x".repeat(16 * 1024);
        let (code, out) = e.send_message(&body, &[]);
        assert_eq!(code, 0, "{out}");
        assert_eq!(out["delivery"], "queued", "{out}");
        assert!(e.codex_args().unwrap()[4].contains(&body));
        assert!(e.events("paste").is_empty());
    }
}

#[test]
fn codex_queue_never_targets_a_dead_pane_or_a_start_replaced_during_session_wait() {
    for case in ["dead pane", "stopped during wait", "replaced during wait"] {
        let e = Env::new(Vendor::Codex);
        e.send_setup(Vendor::Codex, false, false);
        e.fake_codex("exit 0");
        let mut inc = borgchart::lifecycle::load_incarnation(&e.state, "m")
            .unwrap()
            .unwrap();
        inc.started_at = borgchart::bus::time::utc_now();
        borgchart::lifecycle::begin_incarnation(&e.state, &inc).unwrap();
        if case == "dead pane" {
            e.record_session("01a1d300-f48d-7c70-a58a-d5fbea1edd20");
            e.tmux.ok(&["kill-pane", "-t", "=brg-state:=m"]).unwrap();
        }
        std::thread::scope(|s| {
            if case != "dead pane" {
                s.spawn(|| {
                    std::thread::sleep(Duration::from_millis(250));
                    if case == "stopped during wait" {
                        borgchart::lifecycle::end_incarnation(&e.state, "m", "stopped").unwrap();
                    } else {
                        inc.incarnation = "m-2".into();
                        borgchart::lifecycle::begin_incarnation(&e.state, &inc).unwrap();
                    }
                    e.record_session("01a1d300-f48d-7c70-a58a-d5fbea1edd20");
                });
            }
            let (code, out) = e.send_message("hello", &[]);
            assert_eq!(code, 0, "{case}: {out}");
            assert_eq!(out["delivery"], "stored", "{case}: {out}");
        });
        assert!(e.codex_args().is_none(), "{case}");
        assert!(e.events("paste").is_empty(), "{case}");
    }
}

#[test]
fn codex_queue_delivers_through_dialog_and_copy_mode_without_answering_them() {
    for locked in [false, true] {
        for mode in ["dialog", "copy"] {
            let e = Env::new(Vendor::Codex);
            e.send_setup(Vendor::Codex, locked, false);
            e.fake_codex("exit 0");
            e.record_session("01a1d300-f48d-7c70-a58a-d5fbea1edd20");
            if mode == "dialog" {
                e.control("mode=dialog");
            } else {
                e.tmux.ok(&["copy-mode", "-t", "=brg-state:=m"]).unwrap();
            }
            let (code, out) = e.send_message("hello", &["--allow-unknown"]);
            assert_eq!(code, 0, "{locked} {mode}: {out}");
            assert_eq!(
                out["delivery"],
                if mode == "dialog" {
                    "blocked"
                } else {
                    "queued"
                },
                "{locked} {mode}: {out}"
            );
            assert!(e.events("paste").is_empty());
            assert!(e.events("enter").is_empty());
        }
    }
}

#[test]
fn codex_blocked_pane_queues_and_reports_blocked() {
    let e = Env::new(Vendor::Codex);
    e.send_setup(Vendor::Codex, false, false);
    e.fake_codex("exit 0");
    e.record_session("01a1d300-f48d-7c70-a58a-d5fbea1edd20");
    e.control("mode=dialog");
    let (code, out) = e.send_message("task", &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out["delivery"], "blocked", "{out}");
    assert_eq!(
        fs::read_to_string(e.dir.path().join("codex-args.calls"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert!(e.codex_args().unwrap()[4].contains("task"));
    let row = e.status_row();
    assert_eq!(row["state"], "blocked", "{row}");
    assert!(
        row["detail"]
            .as_str()
            .unwrap()
            .contains("brg attach state/m")
    );
    assert_eq!(row["delivery"]["id"], out["id"]);
    assert!(e.events("enter").is_empty());
    assert!(e.events("paste").is_empty());
    e.control("mode=idle");
    assert_eq!(e.send_message("next", &[]).1["delivery"], "queued");
    assert!(e.status_row().get("delivery").is_none());
    e.control("after_enter=garbage");
    e.keys(&["Enter"]);
    e.wait_for("the unreadable screen", |e| e.screen().contains('$'));
    assert_eq!(
        e.send_message("unreadable pane", &[]).1["delivery"],
        "queued"
    );
}

#[test]
fn codex_question_panel_reports_why_queued_messages_wait() {
    for capture in [
        screen!("codex-0.169.1-question-multiple.txt"),
        screen!("codex-0.169.1-question.txt"),
        screen!("codex-0.169.1-question-narrow.txt"),
        screen!("codex-0.169.1-question-notes.txt"),
    ] {
        let e = Env::new(Vendor::Codex);
        e.send_setup(Vendor::Codex, false, false);
        e.fake_codex("exit 0");
        e.record_session("01a1d300-f48d-7c70-a58a-d5fbea1edd20");
        e.show_capture(capture);

        let row = e.status_row();
        let (code, send) = e.send_message("task", &[]);
        assert!(
            row["state"] == "blocked" && code == 0 && send["delivery"] == "blocked",
            "{capture}\nstatus: {row}\nsend: {send}"
        );
        for hint in [&row["detail"], &send["reason"]] {
            let hint = hint.as_str().unwrap();
            assert!(
                hint.contains("question-prompt") && hint.contains("brg attach state/m"),
                "{hint}"
            );
        }
        assert!(e.codex_args().unwrap()[4].contains("task"));
        assert!(e.events("enter").is_empty());
        assert!(e.events("paste").is_empty());
    }
}

#[test]
fn codex_quoted_question_panel_does_not_block_member_or_messages() {
    let e = Env::new(Vendor::Codex);
    e.send_setup(Vendor::Codex, false, false);
    e.fake_codex("exit 0");
    e.record_session("01a1d300-f48d-7c70-a58a-d5fbea1edd20");
    e.show_capture(screen!("codex-question-quoted.txt"));

    let row = e.status_row();
    assert_eq!(row["state"], "idle", "{row}");
    let (code, send) = e.send_message("task", &[]);
    assert_eq!(code, 0, "{send}");
    assert_eq!(send["delivery"], "queued", "{send}");
}
