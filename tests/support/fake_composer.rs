//! A stand-in for the Claude and Codex input boxes, for the guarded wake
//! tests (`tests/wake.rs`).
//!
//! Run as `fake-composer claude|codex` with `FAKE_COMPOSER_DIR` set. It draws
//! the bottom of each vendor's screen the way the real CLIs do (styles
//! included: Claude's grey prompt while busy, Codex's dim placeholder), shows
//! what was typed in its input box, and logs every paste, key, Enter and
//! submission to `$FAKE_COMPOSER_DIR/log.jsonl`.
//!
//! `$FAKE_COMPOSER_DIR/control` holds `key=value` lines, re-read while it
//! runs: `mode=idle|working|dialog`, `enter=submit|ignore`,
//! `on_paste=none|dialog|append:<text>` (a person typing during the paste),
//! `after_enter=garbage` (an unreadable screen after Enter).

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq)]
enum Vendor {
    Claude,
    Codex,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Idle,
    Working,
    Dialog,
    /// Something unreadable instead of the input box.
    Garbage,
}

struct Fake {
    vendor: Vendor,
    dir: PathBuf,
    mode: Mode,
    insert: bool,
    buffer: String,
    enter_submits: bool,
    on_paste: String,
    after_enter: String,
    control: String,
    history: Vec<String>,
    width: usize,
    /// Events from input being handled, written once the screen shows it,
    /// so a test that sees an event also sees what it drew.
    held: std::cell::RefCell<Option<Vec<serde_json::Value>>>,
}

impl Fake {
    fn log(&self, value: serde_json::Value) {
        if let Some(held) = self.held.borrow_mut().as_mut() {
            held.push(value);
            return;
        }
        self.write_log(&value);
    }

    /// Hold events until `release`.
    fn hold(&self) {
        self.held.borrow_mut().get_or_insert_with(Vec::new);
    }

    /// Write the held events; call after drawing.
    fn release(&self) {
        let held = self.held.borrow_mut().take().unwrap_or_default();
        for value in held {
            self.write_log(&value);
        }
    }

    fn write_log(&self, value: &serde_json::Value) {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("log.jsonl"))
        {
            let _ = writeln!(f, "{value}");
        }
    }

    fn reload(&mut self) {
        let text = std::fs::read_to_string(self.dir.join("control")).unwrap_or_default();
        if text == self.control {
            return;
        }
        self.control = text.clone();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            match k {
                "mode" => {
                    self.mode = match v {
                        "working" => Mode::Working,
                        "dialog" => Mode::Dialog,
                        _ => Mode::Idle,
                    }
                }
                "enter" => self.enter_submits = v != "ignore",
                "on_paste" => self.on_paste = v.to_string(),
                "after_enter" => self.after_enter = v.to_string(),
                _ => {}
            }
        }
        self.log(serde_json::json!({"event": "control", "text": text}));
        self.draw();
    }

    /// The input box's lines, wrapped to the pane width.
    fn box_lines(&self) -> Vec<String> {
        let w = self.width.saturating_sub(4).max(10);
        let mut out = Vec::new();
        for line in self.buffer.split('\n') {
            let chars: Vec<char> = line.chars().collect();
            if chars.is_empty() {
                out.push(String::new());
            }
            for chunk in chars.chunks(w) {
                out.push(chunk.iter().collect());
            }
        }
        // Like the real boxes: at most 20 lines, scrolled to the end.
        let skip = out.len().saturating_sub(20);
        out.split_off(skip)
    }

    fn draw(&self) {
        let rule = format!("\x1b[38;5;244m{}\x1b[39m", "─".repeat(self.width.max(20)));
        let mut lines: Vec<String> = self.history.iter().map(|h| format!("> {h}")).collect();
        lines.push(String::new());
        let title;
        // Where the cursor goes: the end of the input box, as the real CLIs do.
        let mut cursor = (0, 0);
        match (self.vendor, self.mode) {
            (_, Mode::Garbage) => {
                title = "fake";
                lines.push("$ ".into());
            }
            (Vendor::Claude, Mode::Dialog) => {
                title = "\u{2733} fake";
                lines.extend(
                    [
                        " Bash command",
                        " This command requires approval",
                        " Do you want to proceed?",
                        " ❯ 1. Yes",
                        "   2. No",
                        " Esc to cancel · Tab to amend",
                    ]
                    .map(String::from),
                );
            }
            (Vendor::Codex, Mode::Dialog) => {
                title = "[ . ] Action Required | fake";
                lines.extend(
                    [
                        "  Would you like to run the following command?",
                        "› 1. Yes, proceed (y)",
                        "",
                        "  Press enter to confirm or esc to cancel",
                    ]
                    .map(String::from),
                );
            }
            (Vendor::Claude, mode) => {
                // The real title stays idle during a turn; the prompt turns grey.
                title = "\u{2733} fake";
                // The real banner starts at column 0; Claude's idle rule needs
                // such a line as the nearest one above the box.
                lines.insert(0, "▝▜█ Fake Claude v0.0.0".into());
                let marker = if mode == Mode::Working {
                    lines.push("\x1b[38;5;174m✶ Working… (1s)\x1b[39m".into());
                    "\x1b[38;5;246m❯\u{a0}\x1b[39m"
                } else {
                    "❯\u{a0}"
                };
                lines.push(rule.clone());
                for (i, l) in self.box_lines().iter().enumerate() {
                    cursor = (lines.len(), 2 + l.chars().count());
                    lines.push(if i == 0 {
                        format!("{marker}{l}")
                    } else {
                        format!("  {l}")
                    });
                }
                lines.push(rule);
                lines.push(if self.insert {
                    "  \x1b[38;5;246m-- INSERT --\x1b[39m ⏵⏵ auto mode on".into()
                } else {
                    "  ⏵⏵ auto mode on".into()
                });
            }
            (Vendor::Codex, mode) => {
                title = if mode == Mode::Working {
                    "\u{2838} fake"
                } else {
                    "fake"
                };
                if mode == Mode::Working {
                    lines.push("• Working (1s • esc to interrupt)".into());
                    lines.push(String::new());
                }
                if self.buffer.is_empty() {
                    cursor = (lines.len(), 2);
                    lines.push("\x1b[1m›\x1b[0m \x1b[2mAsk Codex to do anything\x1b[0m".into());
                } else {
                    for (i, l) in self.box_lines().iter().enumerate() {
                        cursor = (lines.len(), 2 + l.chars().count());
                        lines.push(if i == 0 {
                            format!("\x1b[1m›\x1b[0m {l}")
                        } else {
                            format!("  {l}")
                        });
                    }
                }
                lines.push(String::new());
                lines.push(format!(
                    "  example-model · ~/example   {}",
                    if self.insert {
                        "Vim: Insert"
                    } else {
                        "Vim: Normal"
                    }
                ));
                if self.buffer.is_empty() {
                    lines.push("  ? for shortcuts".into());
                }
            }
        }
        let mut out = format!("\x1b[2J\x1b[H\x1b]0;{title}\x07");
        out.push_str(&lines.join("\r\n"));
        // Vim normal mode puts the cursor on the last character.
        let col = if self.insert {
            cursor.1
        } else {
            cursor.1.saturating_sub(1).max(2)
        };
        out.push_str(&format!("\x1b[{};{}H", cursor.0 + 1, col + 1));
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(out.as_bytes());
        let _ = stdout.flush();
    }

    fn paste(&mut self, text: &str) {
        self.log(serde_json::json!({"event": "paste", "text": text}));
        self.buffer.push_str(text);
        if self.on_paste == "dialog" {
            self.mode = Mode::Dialog;
        } else if let Some(extra) = self.on_paste.strip_prefix("append:") {
            self.buffer.push_str(extra);
        }
    }

    fn enter(&mut self) {
        self.log(serde_json::json!({"event": "enter"}));
        if self.mode == Mode::Dialog {
            self.log(serde_json::json!({"event": "dialog-answered"}));
        } else if self.enter_submits {
            let text = std::mem::take(&mut self.buffer);
            self.log(serde_json::json!({"event": "submit", "text": text}));
            self.history.push(text);
        }
        if self.after_enter == "garbage" {
            self.mode = Mode::Garbage;
        }
    }
}

fn width() -> usize {
    rustix::termios::tcgetwinsize(rustix::stdio::stdout())
        .map(|w| w.ws_col as usize)
        .unwrap_or(80)
}

fn main() {
    let vendor = match std::env::args().nth(1).as_deref() {
        Some("codex") => Vendor::Codex,
        _ => Vendor::Claude,
    };
    let dir = PathBuf::from(std::env::var("FAKE_COMPOSER_DIR").expect("FAKE_COMPOSER_DIR"));
    let stdin_fd = rustix::stdio::stdin();
    if let Ok(mut termios) = rustix::termios::tcgetattr(stdin_fd) {
        termios.make_raw();
        let _ =
            rustix::termios::tcsetattr(stdin_fd, rustix::termios::OptionalActions::Now, &termios);
    }
    print!("\x1b[?2004h");
    let mut fake = Fake {
        vendor,
        dir,
        mode: Mode::Idle,
        insert: true,
        buffer: String::new(),
        enter_submits: true,
        on_paste: String::new(),
        after_enter: String::new(),
        control: String::new(),
        history: Vec::new(),
        width: width(),
        held: Default::default(),
    };
    fake.draw();
    fake.log(serde_json::json!({"event": "ready"}));

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 4096];
        while let Ok(n) = stdin.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                return;
            }
        }
    });

    let mut pending: Vec<u8> = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(bytes) => pending.extend(bytes),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        fake.reload();
        let w = width();
        if w != fake.width {
            fake.width = w;
            fake.draw();
        }
        if pending.is_empty() {
            continue;
        }
        fake.hold();
        while !pending.is_empty() {
            if pending.starts_with(b"\x1b[200~") {
                let Some(end) = pending.windows(6).position(|w| w == b"\x1b[201~") else {
                    break; // the rest of the paste is still coming
                };
                let text = String::from_utf8_lossy(&pending[6..end]).replace('\r', "\n");
                pending.drain(..end + 6);
                fake.paste(&text);
                continue;
            }
            if pending[0] == 0x1b && pending.len() > 1 && pending[1] == b'[' {
                // Some other key sequence: logged, otherwise ignored.
                let n = pending
                    .iter()
                    .skip(2)
                    .position(|b| (0x40..=0x7e).contains(b))
                    .map(|p| p + 3)
                    .unwrap_or(pending.len());
                let seq: Vec<u8> = pending.drain(..n).collect();
                fake.log(serde_json::json!({"event": "key", "seq": format!("{seq:?}")}));
                continue;
            }
            let b = pending.remove(0);
            match b {
                b'\r' => fake.enter(),
                b'\n' => {
                    fake.log(serde_json::json!({"event": "key", "key": "newline"}));
                    fake.buffer.push('\n');
                }
                0x15 => {
                    fake.log(serde_json::json!({"event": "clear"}));
                    fake.buffer.clear();
                }
                0x1b => {
                    fake.log(serde_json::json!({"event": "key", "key": "escape"}));
                    fake.insert = false;
                }
                b'i' if !fake.insert => {
                    fake.log(serde_json::json!({"event": "key", "key": "i"}));
                    fake.insert = true;
                }
                _ => {
                    fake.log(serde_json::json!({"event": "key", "byte": b}));
                    fake.buffer.push(b as char);
                }
            }
        }
        fake.draw();
        fake.release();
    }
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(b"\x1b[?2004l");
}
