//! The team that develops borgchart (`dev-team/`): its example deployment
//! validates, its member files expand and stay generic, and one task round
//! works with `fake-agent` members.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn dev_team() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("dev-team")
}

/// The example deployment with its placeholders filled with paths under
/// `dir`. The bus root stays `~/…`, so `HOME` decides where it lands.
fn fill_example(dir: &Path) -> PathBuf {
    let text = fs::read_to_string(dev_team().join("borgchart.example.yaml")).unwrap();
    assert!(text.contains("root: ~/.local/state/brg-teams"), "{text}");
    let work = dir.join("borgchart-dev");
    for w in ["lead", "impl-a", "impl-b", "review", "notes"] {
        fs::create_dir_all(work.join(w)).unwrap();
    }
    let filled = text
        .replace(
            "/path/to/borgchart/dev-team",
            &dev_team().display().to_string(),
        )
        .replace("/path/to/borgchart-dev", &work.display().to_string());
    assert!(
        !filled
            .lines()
            .any(|l| !l.trim_start().starts_with('#') && l.contains("/path/to")),
        "{filled}"
    );
    let file = dir.join("config/dev-team.yaml");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, filled).unwrap();
    fs::create_dir_all(dir.join("home")).unwrap();
    file
}

fn validate(dir: &Path, file: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_brg"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", dir.join("home"))
        .env("BORGCHART_STATE_HOME", dir.join("state"))
        .args(["validate", "--json", "--file"])
        .arg(file)
        .output()
        .unwrap()
}

#[test]
fn the_example_deployment_validates_with_only_the_expected_warning() {
    let dir = tempfile::tempdir().unwrap();
    let file = fill_example(dir.path());
    let out = validate(dir.path(), &file);
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(out.status.success(), "{report}");
    assert_eq!(report["errors"], serde_json::json!([]), "{report}");
    // The Codex reviewer runs the CLI's default model on purpose.
    let warnings: Vec<(String, String)> = report["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| {
            (
                w["file"].as_str().unwrap_or("").to_string(),
                w["message"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        warnings,
        [(
            dev_team().join("reviewer.md").display().to_string(),
            "no `models.codex` entry; the vendor's default model will be used".to_string()
        )]
    );
}

fn member_files() -> Vec<PathBuf> {
    let team = fs::read_to_string(dev_team().join("team.yaml")).unwrap();
    let mut files: Vec<PathBuf> = team
        .lines()
        .filter_map(|l| l.trim().strip_prefix("prompt: "))
        .map(|p| dev_team().join(p.trim()))
        .collect();
    files.sort();
    files.dedup();
    files
}

#[test]
fn member_files_expand_their_includes() {
    let files = member_files();
    assert_eq!(files.len(), 4, "{files:?}");
    for file in files {
        let text = fs::read_to_string(&file).unwrap();
        let includes: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("{{includes/")?.strip_suffix("}}"))
            .collect();
        assert!(includes.contains(&"rules"), "{}", file.display());
        assert!(includes.contains(&"member-bus") || includes.contains(&"lead-bus"));
        for stem in includes {
            let inc = dev_team().join(format!("includes/{stem}.md"));
            let body = fs::read_to_string(&inc)
                .unwrap_or_else(|e| panic!("{}: {stem}: {e}", file.display()));
            assert!(!body.contains("{{"), "{} nests an include", inc.display());
        }
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

#[test]
fn the_team_files_stay_generic() {
    let mut files = Vec::new();
    walk(&dev_team(), &mut files);
    assert!(files.len() >= 10, "{files:?}");
    // Machine paths, private tooling and people stay in the private
    // deployment file, never here.
    let deny = [
        "herdr",
        "serveserve",
        "mrshll",
        "marsh",
        "passh",
        "agent-gh",
    ];
    let home = regex::Regex::new(r"/(home|Users)/[^/\s]+|/tmp/|/root/").unwrap();
    for file in files {
        let text = fs::read_to_string(&file).unwrap();
        let lower = text.to_lowercase();
        for word in deny {
            assert!(
                !lower.contains(word),
                "{} mentions `{word}`",
                file.display()
            );
        }
        assert!(
            !home.is_match(&text),
            "{} has a machine path: {:?}",
            file.display(),
            home.find(&text).map(|m| m.as_str())
        );
    }
}

/// The lead alone under `fake-agent`, then one implementer task:
/// assign → (start) → accept → result → verify → stop → release, with the
/// lead's background self-wake returning for each reply.
#[cfg(feature = "test-support")]
mod round {
    use super::*;
    use std::time::{Duration, Instant};

    struct Team {
        dir: tempfile::TempDir,
        file: PathBuf,
        socket: String,
    }

    impl Drop for Team {
        fn drop(&mut self) {
            let _ = Command::new("tmux")
                .args(["-L", &self.socket, "kill-server"])
                .output();
        }
    }

    impl Team {
        fn brg(&self, args: &[&str]) -> Output {
            let bin = self.dir.path().join("bin");
            Command::new(env!("CARGO_BIN_EXE_brg"))
                .env_clear()
                .env(
                    "PATH",
                    format!(
                        "{}:/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin",
                        bin.display()
                    ),
                )
                .env("HOME", self.dir.path().join("home"))
                .env("LANG", "C.UTF-8")
                .env("BORGCHART_STATE_HOME", self.dir.path().join("state"))
                .env("BORGCHART_TMUX_SOCKET", &self.socket)
                .args(args)
                .arg("--file")
                .arg(&self.file)
                .output()
                .unwrap()
        }

        fn ok(&self, args: &[&str]) -> String {
            let out = self.brg(args);
            assert!(
                out.status.success(),
                "{args:?}: {}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).to_string()
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.dir.path().join(rel)
        }

        /// Run a shell script inside `member`, as its own tool call.
        fn run_in(&self, member: &str, name: &str, script: &str) {
            let path = self.path(&format!("s/{name}.sh"));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, script).unwrap();
            self.wait_idle(member);
            self.ok(&["send", member, &format!("tool sh {}", path.display())]);
        }

        fn wait_idle(&self, member: &str) {
            self.wait_for(&format!("{member} idle"), || {
                let rows: serde_json::Value =
                    serde_json::from_slice(&self.brg(&["status", "--json"]).stdout)
                        .unwrap_or_default();
                rows.as_array().is_some_and(|rows| {
                    rows.iter()
                        .any(|r| r["name"] == member && r["state"] == "idle")
                })
            });
        }

        fn wait_for(&self, what: &str, f: impl Fn() -> bool) {
            let deadline = Instant::now() + Duration::from_secs(30);
            while Instant::now() < deadline {
                if f() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            panic!("timed out waiting for {what}");
        }

        /// The JSON line a script wrote to `s/<name>.out`, and its exit code.
        fn output(&self, name: &str) -> (serde_json::Value, i32) {
            let path = self.path(&format!("s/{name}.out"));
            self.wait_for(&format!("{name} output"), || {
                fs::read_to_string(&path).is_ok_and(|t| t.contains("exit "))
            });
            let text = fs::read_to_string(&path).unwrap();
            let (json, code) = text.rsplit_once("exit ").unwrap();
            (
                serde_json::from_str(json.trim()).unwrap_or_else(|e| panic!("{name}: {e}: {json}")),
                code.trim().parse().unwrap(),
            )
        }

        /// A background `inbox wait`, the way a Claude member self-wakes.
        fn background_wait(&self, member: &str, name: &str, after: Option<&str>, timeout: u32) {
            let out = self.path(&format!("s/{name}.out"));
            let after = after.map(|a| format!("--after {a} ")).unwrap_or_default();
            self.run_in(
                member,
                name,
                &format!(
                    "( \"$BORGCHART_BIN\" inbox wait {after}--timeout {timeout} --json > {o} 2>&1; echo \"exit $?\" >> {o} ) </dev/null >/dev/null 2>&1 &\n",
                    o = out.display()
                ),
            );
        }

        /// Publish a draft from inside `member`.
        fn publish(&self, member: &str, key: &str, draft: serde_json::Value) -> serde_json::Value {
            let out = self.path(&format!("s/{key}.out"));
            self.run_in(
                member,
                key,
                &format!(
                    "\"$BORGCHART_BIN\" msg publish --key {key} --json > {o} 2>&1 <<'DRAFT'\n{draft}\nDRAFT\necho \"exit $?\" >> {o}\n",
                    o = out.display()
                ),
            );
            let (json, code) = self.output(key);
            assert_eq!(code, 0, "{key}: {json}");
            json
        }

        fn ack(&self, member: &str, id: &str) {
            let name = format!("ack-{member}-{id}");
            let out = self.path(&format!("s/{name}.out"));
            self.run_in(
                member,
                &name,
                &format!(
                    "\"$BORGCHART_BIN\" msg ack {id} --json > {o} 2>&1; echo \"exit $?\" >> {o}\n",
                    o = out.display()
                ),
            );
            assert_eq!(self.output(&name).1, 0);
        }
    }

    fn ids(page: &serde_json::Value) -> Vec<String> {
        page["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn one_task_round_with_fake_agents() {
        let dir = tempfile::tempdir().unwrap();
        let file = fill_example(dir.path());
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        for vendor in ["claude", "codex"] {
            std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_fake-agent"), bin.join(vendor)).unwrap();
        }
        let team = Team {
            socket: format!("brg-dev-team-test-{}", std::process::id()),
            file,
            dir,
        };
        let work = team.path("borgchart-dev");

        let up = team.ok(&["up"]);
        assert!(
            up.lines()
                .any(|l| l.contains("lead") && l.ends_with("started")),
            "{up}"
        );
        assert!(up.contains("not started (start: false)"), "{up}");
        team.wait_idle("lead");

        // The lead's opening prompt is its expanded member file plus its task.
        let log = fs::read_to_string(team.path("home/fake-agent/lead.jsonl")).unwrap();
        let start: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
        let prompt = start["argv"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .as_str()
            .unwrap();
        assert!(
            prompt.starts_with("You are the lead of the team"),
            "{prompt}"
        );
        assert!(prompt.contains("## Waking yourself") && prompt.contains("## Local rules"));
        assert!(!prompt.contains("{{"), "{prompt}");
        assert_eq!(start["env"]["BORGCHART_BIN"], env!("CARGO_BIN_EXE_brg"));
        assert_eq!(start["cwd"], work.join("lead").display().to_string());
        // The bus root `~/…` landed under HOME.
        assert!(team.path("home/.local/state/brg-teams").is_dir());

        // The assignment is waiting before the implementer starts.
        let agreement = work.join("notes/t1-agreement.md");
        fs::write(&agreement, "Task t1: a fictional change.\n").unwrap();
        let sha = borgchart::bus::manifest::sha256_hex(&fs::read(&agreement).unwrap());
        let checkout = work.join("impl-a").display().to_string();
        let assign = team.publish(
            "lead",
            "t1.assign.1",
            serde_json::json!({"to": "implementer-a", "kind": "assign", "task": "t1", "attempt": 1,
                "body": "Make a fictional change.",
                "facts": {"agreement": {"path": agreement, "sha256": sha}, "scope": "tests", "checkout": checkout}}),
        );
        assert_eq!(assign["id"], "m1");

        // The lead has listed nothing yet, so it waits without `--after`.
        team.background_wait("lead", "wait-1", None, 30);
        team.ok(&["up", "--member", "implementer-a"]);
        team.wait_idle("implementer-a");
        let log = fs::read_to_string(team.path("home/fake-agent/implementer-a.jsonl")).unwrap();
        let start: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
        assert_eq!(start["cwd"], checkout);

        // Its first action: read its inbox, then accept.
        let inbox = team.path("s/impl-inbox.out");
        team.run_in(
            "implementer-a",
            "impl-inbox",
            &format!(
                "\"$BORGCHART_BIN\" inbox --json > {o} 2>&1; echo \"exit $?\" >> {o}\n",
                o = inbox.display()
            ),
        );
        assert_eq!(ids(&team.output("impl-inbox").0), ["m1"]);
        team.publish(
            "implementer-a",
            "t1.accept.1",
            serde_json::json!({"to": "lead", "kind": "accept", "task": "t1", "attempt": 1, "reply_to": ["m1"]}),
        );
        let (woke, code) = team.output("wait-1");
        assert_eq!((code, ids(&woke)), (0, vec!["m2".to_string()]));
        team.ack("lead", "m2");

        // With nothing new after the highest ID it has listed, the wait times out.
        team.background_wait("lead", "wait-2", Some("m2"), 1);
        let (timeout, code) = team.output("wait-2");
        assert_eq!(
            (code, &timeout["outcome"]),
            (9, &serde_json::json!("timeout"))
        );

        team.background_wait("lead", "wait-3", Some("m2"), 30);
        let report = work.join("notes/t1-report.md");
        fs::write(&report, "status: complete\naction-needed: none\n").unwrap();
        team.publish(
            "implementer-a",
            "t1.result.1",
            serde_json::json!({"to": "lead", "kind": "result", "task": "t1", "attempt": 1, "reply_to": ["m1"],
                "body": "action-needed: none",
                "artifacts": [{"path": report, "label": "report"}],
                "facts": {"status": "complete", "checks": [{"name": "cargo test", "outcome": "pass"}]}}),
        );
        let (woke, code) = team.output("wait-3");
        assert_eq!((code, ids(&woke)), (0, vec!["m3".to_string()]));
        team.ack("lead", "m3");
        team.ack("implementer-a", "m1");

        team.publish(
            "lead",
            "t1.verify.1",
            serde_json::json!({"to": "implementer-a", "kind": "verify", "task": "t1", "attempt": 1,
                "reply_to": ["m3"], "facts": {"result": "m3", "outcome": "accepted"}}),
        );
        team.ok(&["down", "--member", "implementer-a"]);
        let show = team.ok(&["task", "show", "t1"]);
        assert!(show.contains("(gone, current)"), "{show}");
        team.publish(
            "lead",
            "t1.release.1",
            serde_json::json!({"to": "implementer-a", "kind": "release", "task": "t1", "attempt": 1,
                "facts": {"evidence": "verified m3; implementer-a stopped", "checkout": checkout}}),
        );
        let list = team.ok(&["task", "list"]);
        assert!(list.contains("t1  implementer-a#1  released"), "{list}");

        team.ok(&["down"]);
    }
}
