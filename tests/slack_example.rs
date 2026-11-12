//! End-to-end round trip for `examples/slack-devops` with a fake Slack API and
//! `fake-agent` members: message → watcher → triager → escalate → lead → reply.
//!
//! Needs tmux and uv. Run with `cargo test --features test-support`.
#![cfg(feature = "test-support")]

mod common;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TOKEN: &str = "xoxb-not-a-real-token-9a2d";

#[derive(Default)]
struct Slack {
    messages: Vec<serde_json::Value>,
    posted: Vec<serde_json::Value>,
}

fn serve(listener: TcpListener, slack: Arc<Mutex<Slack>>) {
    for stream in listener.incoming().flatten() {
        let slack = slack.clone();
        std::thread::spawn(move || {
            let _ = handle(stream, &slack);
        });
    }
}

fn handle(mut stream: TcpStream, slack: &Mutex<Slack>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut length = 0usize;
    let mut auth = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            length = v.trim().parse().unwrap_or(0);
        }
        if lower.starts_with("authorization:") {
            auth = line["authorization:".len()..].trim().to_string();
        }
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .to_string();
    let (route, query) = path.split_once('?').unwrap_or((&path, ""));
    let method = route.rsplit('/').next().unwrap_or("");
    let param = |key: &str| -> Option<String> {
        query.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (k == key).then(|| v.replace("%2E", ".").replace("%2e", "."))
        })
    };
    let reply = if auth != format!("Bearer {TOKEN}") {
        serde_json::json!({"ok": false, "error": "invalid_auth"})
    } else {
        let mut s = slack.lock().unwrap();
        match method {
            "auth.test" => serde_json::json!({"ok": true, "user_id": "U0BOT"}),
            "conversations.history" => {
                let mut msgs: Vec<serde_json::Value> = s.messages.clone();
                if let Some(oldest) = param("oldest").and_then(|o| o.parse::<f64>().ok()) {
                    msgs.retain(|m| m["ts"].as_str().unwrap().parse::<f64>().unwrap() > oldest);
                }
                if let Some(latest) = param("latest") {
                    msgs.retain(|m| m["ts"] == latest.as_str());
                }
                serde_json::json!({"ok": true, "messages": msgs})
            }
            "chat.postMessage" => {
                s.posted
                    .push(serde_json::from_slice(&body).unwrap_or_default());
                serde_json::json!({"ok": true, "ts": "1"})
            }
            _ => serde_json::json!({"ok": false, "error": "unknown_method"}),
        }
    };
    let text = reply.to_string();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
        text.len()
    )?;
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if name == "__pycache__" || name == "tests" || name == ".pytest_cache" {
            continue;
        }
        if path.is_dir() {
            copy_dir(&path, &to.join(&name));
        } else {
            fs::copy(&path, to.join(&name)).unwrap();
        }
    }
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

struct Run {
    /// First, so its server stops before the test's directory goes.
    tmux: crate::common::PrivateTmux,
    dir: tempfile::TempDir,
    socket: String,
    path: String,
}

impl Run {
    fn brg(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_brg"));
        cmd.env_clear();
        self.tmux
            .brg_env(&mut cmd, &self.socket)
            .env("PATH", &self.path)
            .env("HOME", self.dir.path().join("home"))
            .env("LANG", "C.UTF-8")
            .env("BORGCHART_STATE_HOME", self.dir.path().join("state"))
            .env("BORGCHART_CONFIG", self.dir.path().join("config.yaml"))
            .env("BORGCHART_GATE_WAIT_MS", "15000")
            .env("SLACK_BOT_TOKEN", TOKEN)
            .current_dir(self.dir.path().join("team"))
            .args(self.translate(args))
            .output()
            .unwrap()
    }

    fn read(&self, rel: &str) -> Option<serde_json::Value> {
        serde_json::from_slice(&fs::read(self.dir.path().join(rel)).ok()?).ok()
    }

    /// The example's one instance, once created.
    fn instance(&self) -> Option<String> {
        let reg = self.read("state/registry.json")?;
        reg["instances"].as_object()?.keys().next().cloned()
    }

    /// The member of `role`, or the name as it is (a plugin process).
    fn member(&self, role: &str) -> String {
        let found = self.instance().and_then(|inst| {
            let rec = self.read(&format!("state/{inst}/instance.json"))?;
            rec["members"]
                .as_object()?
                .iter()
                .find(|(_, m)| m["role"] == role)
                .map(|(id, _)| id.clone())
        });
        found.unwrap_or_else(|| role.to_string())
    }

    /// `start` creates the instance (working in `work`); `stop`, `status`,
    /// `msg send` and `logs` go to it, with members named by role.
    fn translate(&self, args: &[&str]) -> Vec<String> {
        let inst = self.instance().unwrap_or_default();
        let mut v: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        match args {
            ["start"] => {
                v = vec![
                    "create".into(),
                    self.dir.path().join("team").display().to_string(),
                    self.dir.path().join("work").display().to_string(),
                ]
            }
            ["stop"] => v.push(inst),
            ["status", ..] => v.insert(1, inst),
            ["logs", name, ..] => v[1] = format!("{inst}/{}", self.member(name)),
            ["msg", "send", name, ..] => v[2] = format!("{inst}/{}", self.member(name)),
            _ => {}
        }
        v
    }

    fn submissions(&self, member: &str) -> Vec<String> {
        fs::read_to_string(
            self.dir
                .path()
                .join(format!("home/fake-agent/{}.jsonl", self.member(member))),
        )
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|e| e["event"] == "submit")
        .map(|e| e["text"].as_str().unwrap_or("").to_string())
        .collect()
    }

    fn wait<F: Fn() -> bool>(&self, what: &str, f: F) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if f() {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let status = String::from_utf8_lossy(&self.brg(&["status"]).stdout).to_string();
        let logs = String::from_utf8_lossy(&self.brg(&["logs", "slackbot", "-n", "40"]).stdout)
            .to_string();
        panic!(
            "timed out waiting for {what}\nstatus:\n{status}\nwatcher:\n{logs}\ntriager: {:?}\nlead: {:?}",
            self.submissions("triager"),
            self.submissions("lead")
        );
    }
}

#[test]
fn slack_round_trip_through_the_example_team() {
    let Some(uv) = which("uv") else {
        eprintln!("skipping: uv is not on PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/slack-devops");
    let team = dir.path().join("team");
    copy_dir(&example, &team);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 5;
    let urgent = format!("{now}.000100");
    let chatter = format!("{}.000200", now + 1);
    let slack = Arc::new(Mutex::new(Slack {
        messages: vec![
            serde_json::json!({"ts": urgent, "user": "U0ALICE", "text": "deploys are failing on main"}),
            serde_json::json!({"ts": chatter, "user": "U0BOB", "text": "lunch?"}),
        ],
        posted: Vec::new(),
    }));
    {
        let slack = slack.clone();
        std::thread::spawn(move || serve(listener, slack));
    }

    // Point the plugin at the fake API (tests only), and poll quickly.
    let team_yaml = fs::read_to_string(team.join("template.yaml")).unwrap();
    let channel = "      SLACK_CHANNEL_ID: C0000EXAMPLE";
    assert!(team_yaml.contains(channel), "{team_yaml}");
    let team_yaml = team_yaml.replacen(
        channel,
        &format!(
            "      SLACK_CHANNEL_ID: C0000EXAMPLE\n      SLACK_API_BASE: http://127.0.0.1:{port}/api\n      SLACK_POLL_SECONDS: '1'\n      UV_CACHE_DIR: {}",
            dir.path().join("uv-cache").display()
        ),
        1,
    );
    fs::write(team.join("template.yaml"), team_yaml).unwrap();
    fs::create_dir_all(dir.path().join("work")).unwrap();

    let bin = dir.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_fake-agent"), bin.join("claude")).unwrap();
    fs::create_dir_all(dir.path().join("home")).unwrap();
    let tmux = crate::common::PrivateTmux::new();
    let path = tmux.path_with(&format!(
        "{}:{}:/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin",
        bin.display(),
        uv.parent().unwrap().display()
    ));
    let run = Run {
        socket: tmux.socket("brg-slack-test"),
        tmux,
        dir,
        path,
    };

    let up = run.brg(&["start"]);
    assert!(
        up.status.success(),
        "{}{}",
        String::from_utf8_lossy(&up.stdout),
        String::from_utf8_lossy(&up.stderr)
    );

    // The watcher hands both new messages to the triager, as IDs only.
    run.wait("triager notified", || run.submissions("triager").len() >= 2);
    let notes = run.submissions("triager");
    assert!(
        notes.iter().any(|n| n.contains(&format!("ts={urgent}"))),
        "{notes:?}"
    );
    assert!(
        notes
            .iter()
            .all(|n| !n.contains("deploys") && !n.contains("lunch")),
        "{notes:?}"
    );

    // The triager escalates the urgent one (a real triager would decide this).
    let cmd = format!("tool slackbot escalate {urgent} high deploy");
    assert!(
        run.brg(&["msg", "send", "triager", &cmd, "--external", "test"])
            .status
            .success()
    );
    run.wait("lead escalation", || {
        run.submissions("lead").iter().any(|s| {
            s.contains(&format!(
                "Escalation from {}: message ts={urgent}, severity=high, reason=deploy",
                run.member("triager")
            ))
        })
    });

    // The lead replies in the thread.
    let cmd = format!("tool slackbot reply {urgent} 'Looking into the failing deploys now'");
    assert!(
        run.brg(&["msg", "send", "lead", &cmd, "--external", "test"])
            .status
            .success()
    );
    run.wait("reply posted", || !slack.lock().unwrap().posted.is_empty());
    let posted = slack.lock().unwrap().posted[0].clone();
    assert_eq!(posted["channel"], "C0000EXAMPLE");
    assert_eq!(posted["thread_ts"], urgent.as_str());
    assert_eq!(posted["text"], "Looking into the failing deploys now");

    // The triager can't reply: `reply` isn't granted to it.
    let cmd = format!("tool slackbot reply {urgent} 'sneaky'");
    assert!(
        run.brg(&["msg", "send", "triager", &cmd, "--external", "test"])
            .status
            .success()
    );
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(slack.lock().unwrap().posted.len(), 1);

    let status: serde_json::Value =
        serde_json::from_slice(&run.brg(&["status", "--json"]).stdout).unwrap();
    let watcher = status
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "slackbot")
        .unwrap()
        .clone();
    assert_eq!(watcher["state"], "alive", "{watcher}");
    assert!(run.brg(&["stop"]).status.success());
}
