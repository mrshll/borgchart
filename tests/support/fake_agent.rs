//! A stand-in for the Claude and Codex CLIs, for brg's runtime tests.
//!
//! Run as `claude` or `codex` (by symlink). It draws just enough of each
//! vendor's screen for brg's state rules, turns on bracketed paste, runs the
//! gate hooks brg installs, and logs everything it receives to
//! `$HOME/fake-agent/<member>.jsonl`.
//!
//! Submissions it understands: `work <secs>`, `dialog`, `mute`, `crash <code>`,
//! `tool <shell command>`. Anything else is just logged.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

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
    Mute,
    /// Claude's folder-trust screen; `trust_choice` 0 = "No, exit", 1 = "Yes".
    Trust,
}

struct Agent {
    vendor: Vendor,
    log: PathBuf,
    pre_hooks: Vec<String>,
    mode: Mode,
    work_until: Option<Instant>,
    lines: Vec<String>,
    trust_choice: usize,
    start_hooks: Vec<String>,
}

fn log_event(path: &PathBuf, value: serde_json::Value) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{value}");
    }
}

fn run_hook(command: &str, input: &serde_json::Value) -> (i32, String) {
    let child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        return (-1, "spawn failed".into());
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.to_string().as_bytes());
    }
    match child.wait_with_output() {
        Ok(out) => (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ),
        Err(_) => (-1, "wait failed".into()),
    }
}

/// Hook commands brg installed: from Claude's `--settings` file or Codex's `-c hooks=…`.
fn hooks(vendor: Vendor, args: &[String]) -> (Vec<String>, Vec<String>) {
    let mut pre = Vec::new();
    let mut start = Vec::new();
    match vendor {
        Vendor::Claude => {
            if let Some(i) = args.iter().position(|a| a == "--settings")
                && let Some(file) = args.get(i + 1)
                && let Ok(text) = std::fs::read_to_string(file)
                && let Ok(v) = serde_json::from_str::<serde_json::Value>(&text)
            {
                let commands = |event: &str| -> Vec<String> {
                    v["hooks"][event]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
                        .filter_map(|h| h["command"].as_str().map(str::to_string))
                        .collect()
                };
                pre = commands("PreToolUse");
                start = commands("SessionStart");
            }
        }
        Vendor::Codex => {
            for arg in args.iter().filter(|a| a.starts_with("hooks=")) {
                for (event, out) in [("PreToolUse", &mut pre), ("SessionStart", &mut start)] {
                    let pattern = format!(
                        r#"{event}=\[\{{matcher="\*",hooks=\[\{{type="command",command=("(?:[^"\\]|\\.)*")"#
                    );
                    if let Some(c) = regex::Regex::new(&pattern).unwrap().captures(arg)
                        && let Ok(command) = serde_json::from_str::<String>(&c[1])
                    {
                        out.push(command);
                    }
                }
            }
        }
    }
    (pre, start)
}

impl Agent {
    fn draw(&self) {
        let (title, body) = match (self.vendor, self.mode) {
            (_, Mode::Mute) => ("plain".to_string(), vec!["$ ".to_string()]),
            (_, Mode::Trust) => (
                String::new(),
                vec![
                    "─".repeat(60),
                    " Accessing workspace:".into(),
                    "".into(),
                    format!(
                        " {}",
                        std::env::current_dir()
                            .map(|d| d.display().to_string())
                            .unwrap_or_default()
                    ),
                    "".into(),
                    " Quick safety check: Is this a project you created or one you trust?".into(),
                    "".into(),
                    format!(
                        " {}No, exit",
                        if self.trust_choice == 0 { "❯ " } else { "  " }
                    ),
                    format!(
                        " {}Yes, I trust this folder",
                        if self.trust_choice == 1 { "❯ " } else { "  " }
                    ),
                    "".into(),
                    " Enter to confirm · Esc to cancel".into(),
                ],
            ),
            (Vendor::Claude, Mode::Idle) => (
                "\u{2733} fake".to_string(),
                vec![
                    "─".repeat(60),
                    "❯ ".into(),
                    "─".repeat(60),
                    "  ⏵⏵ don't ask on (shift+tab to cycle)".into(),
                ],
            ),
            (Vendor::Claude, Mode::Working) => (
                "\u{25D0} fake".to_string(),
                vec![
                    "✻ Working… (1s)".into(),
                    "─".repeat(60),
                    "❯ ".into(),
                    "─".repeat(60),
                    "  ⏵⏵ don't ask on (shift+tab to cycle) · esc to interrupt".into(),
                ],
            ),
            (Vendor::Claude, Mode::Dialog) => (
                "\u{2733} fake".to_string(),
                vec![
                    " Bash command".into(),
                    " This command requires approval".into(),
                    " Do you want to proceed?".into(),
                    " ❯ 1. Yes".into(),
                    "   2. No".into(),
                    " Esc to cancel · Tab to amend".into(),
                ],
            ),
            (Vendor::Codex, Mode::Idle) => (
                "fake".to_string(),
                vec![
                    "› Ask Codex to do anything".into(),
                    "".into(),
                    "  ? for shortcuts".into(),
                ],
            ),
            (Vendor::Codex, Mode::Working) => (
                "\u{2838} fake".to_string(),
                vec![
                    "• Working (1s • esc to interrupt)".into(),
                    "› Ask Codex to do anything".into(),
                    "  ? for shortcuts".into(),
                ],
            ),
            (Vendor::Codex, Mode::Dialog) => (
                "[ . ] Action Required | fake".to_string(),
                vec![
                    "  Would you like to run the following command?".into(),
                    "› 1. Yes, proceed (y)".into(),
                    "  Press enter to confirm or esc to cancel".into(),
                ],
            ),
        };
        let mut out = String::from("\x1b[2J\x1b[H");
        out.push_str(&format!("\x1b]0;{title}\x07"));
        let tail: Vec<&String> = self.lines.iter().rev().take(20).collect();
        for line in tail.into_iter().rev() {
            out.push_str(line);
            out.push_str("\r\n");
        }
        for line in body {
            out.push_str(&line);
            out.push_str("\r\n");
        }
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(out.as_bytes());
        let _ = stdout.flush();
    }

    fn submit(&mut self, text: &str) {
        log_event(
            &self.log,
            serde_json::json!({"event": "submit", "text": text}),
        );
        self.lines.push(format!("> {}", text.replace('\n', " / ")));
        let first = text.lines().next().unwrap_or("").trim();
        let (word, rest) = first.split_once(' ').unwrap_or((first, ""));
        match word {
            "work" => {
                let secs: u64 = rest.trim().parse().unwrap_or(2);
                self.mode = Mode::Working;
                self.work_until = Some(Instant::now() + Duration::from_secs(secs));
            }
            "dialog" => self.mode = Mode::Dialog,
            "mute" => self.mode = Mode::Mute,
            "crash" => {
                let code: i32 = rest.trim().parse().unwrap_or(1);
                log_event(
                    &self.log,
                    serde_json::json!({"event": "crash", "code": code}),
                );
                restore_terminal();
                std::process::exit(code);
            }
            "tool" => {
                let command = rest.to_string();
                let input = serde_json::json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": command}});
                let mut allowed = true;
                let mut reason = String::new();
                for hook in &self.pre_hooks {
                    let (code, stderr) = run_hook(hook, &input);
                    if code == 2 {
                        allowed = false;
                        reason = stderr;
                    }
                }
                let mut output = String::new();
                if allowed && let Ok(out) = Command::new("sh").arg("-c").arg(&command).output() {
                    output = String::from_utf8_lossy(&out.stdout).trim().to_string();
                }
                log_event(
                    &self.log,
                    serde_json::json!({"event": "tool", "command": command, "allowed": allowed, "reason": reason, "output": output}),
                );
                self.lines.push(format!(
                    "tool {}: {}",
                    if allowed { "ran" } else { "blocked" },
                    command
                ));
            }
            _ => {}
        }
        self.draw();
    }
}

fn run_start_hooks(log: &PathBuf, hooks: &[String]) {
    for hook in hooks {
        let (code, _) = run_hook(
            hook,
            &serde_json::json!({"hook_event_name": "SessionStart", "source": "startup"}),
        );
        log_event(
            log,
            serde_json::json!({"event": "session-start-hook", "code": code}),
        );
    }
}

fn restore_terminal() {
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(b"\x1b[?2004l");
    let _ = stdout.flush();
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let name = PathBuf::from(&argv[0])
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let vendor = if name.contains("codex") {
        Vendor::Codex
    } else {
        Vendor::Claude
    };
    let args = &argv[1..];
    if args.iter().any(|a| a == "--version") {
        println!("fake-agent 0.0.0 ({name})");
        return;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let member = std::env::var("BORGCHART_MEMBER").unwrap_or_else(|_| "unknown".into());
    let log = PathBuf::from(&home)
        .join("fake-agent")
        .join(format!("{member}.jsonl"));
    let env: BTreeMap<String, String> = std::env::vars().collect();
    log_event(
        &log,
        serde_json::json!({"event": "start", "argv": argv, "env": env, "cwd": std::env::current_dir().ok()}),
    );

    let (pre_hooks, start_hooks) = hooks(vendor, args);
    let skip_hooks = PathBuf::from(&home)
        .join("fake-agent")
        .join("no-hooks")
        .exists();
    let trust_first = vendor == Vendor::Claude
        && PathBuf::from(&home)
            .join("fake-agent")
            .join("trust")
            .exists();
    let start_hooks = if skip_hooks { Vec::new() } else { start_hooks };
    if !trust_first {
        run_start_hooks(&log, &start_hooks);
    }

    // Raw mode and bracketed paste, like the real CLIs.
    let stdin_fd = rustix::stdio::stdin();
    if let Ok(mut termios) = rustix::termios::tcgetattr(stdin_fd) {
        termios.make_raw();
        let _ =
            rustix::termios::tcsetattr(stdin_fd, rustix::termios::OptionalActions::Now, &termios);
    }
    print!("\x1b[?2004h");

    let mut agent = Agent {
        vendor,
        log,
        pre_hooks: if skip_hooks { Vec::new() } else { pre_hooks },
        mode: if trust_first { Mode::Trust } else { Mode::Idle },
        work_until: None,
        lines: Vec::new(),
        trust_choice: 0,
        start_hooks,
    };
    agent.draw();

    let (tx, rx) = mpsc::channel::<u8>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 1024];
        while let Ok(n) = stdin.read(&mut buf) {
            if n == 0 {
                break;
            }
            for &b in &buf[..n] {
                if tx.send(b).is_err() {
                    return;
                }
            }
        }
    });

    let mut pending: Vec<u8> = Vec::new();
    let mut in_paste = false;
    let mut text = String::new();
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(b) => pending.push(b),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if let Some(until) = agent.work_until
            && Instant::now() >= until
        {
            agent.work_until = None;
            agent.mode = Mode::Idle;
            agent.draw();
        }
        // Consume complete tokens from `pending`.
        loop {
            if pending.is_empty() {
                break;
            }
            if pending.starts_with(b"\x1b[200~") {
                in_paste = true;
                pending.drain(..6);
                continue;
            }
            if pending.starts_with(b"\x1b[201~") {
                in_paste = false;
                pending.drain(..6);
                continue;
            }
            if pending.starts_with(b"\x1b[B") || pending.starts_with(b"\x1b[A") {
                let down = pending[2] == b'B';
                pending.drain(..3);
                if agent.mode == Mode::Trust {
                    agent.trust_choice = if down { 1 } else { 0 };
                    agent.draw();
                }
                continue;
            }
            if pending[0] == 0x1b
                && pending.len() < 6
                && (b"\x1b[200~".starts_with(&pending) || b"\x1b[201~".starts_with(&pending))
            {
                break; // wait for the rest of a paste marker
            }
            let b = pending.remove(0);
            if in_paste {
                // Terminals (and tmux) paste newlines as CR; real CLIs read them as line breaks.
                text.push(if b == b'\r' { '\n' } else { b as char });
                continue;
            }
            match b {
                b'\r' | b'\n' if agent.mode == Mode::Trust => {
                    if agent.trust_choice == 1 {
                        log_event(&agent.log, serde_json::json!({"event": "trust-accepted"}));
                        agent.mode = Mode::Idle;
                        run_start_hooks(&agent.log, &agent.start_hooks);
                        agent.draw();
                    } else {
                        log_event(&agent.log, serde_json::json!({"event": "trust-declined"}));
                        restore_terminal();
                        std::process::exit(1);
                    }
                }
                b'\r' | b'\n' => {
                    if agent.mode == Mode::Dialog {
                        log_event(
                            &agent.log,
                            serde_json::json!({"event": "dialog-answer", "answer": "yes"}),
                        );
                        agent.mode = Mode::Idle;
                        agent.draw();
                    } else {
                        let submitted = std::mem::take(&mut text);
                        let decoded = String::from_utf8_lossy(
                            &submitted.chars().map(|c| c as u8).collect::<Vec<u8>>(),
                        )
                        .to_string();
                        agent.submit(&decoded);
                    }
                }
                0x1b => {
                    if agent.mode == Mode::Dialog {
                        log_event(
                            &agent.log,
                            serde_json::json!({"event": "dialog-answer", "answer": "cancel"}),
                        );
                        agent.mode = Mode::Idle;
                        agent.draw();
                    }
                }
                0x03 => {
                    restore_terminal();
                    return;
                }
                _ => text.push(b as char),
            }
        }
    }
    restore_terminal();
}
