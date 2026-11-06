//! The team that develops borgchart (`dev-team/`) and its design panel
//! (`dev-design-team/`): their example deployments validate, their member
//! files expand and stay generic, one task round works with `fake-agent`
//! members, and the lead can seat the design panel.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TEAMS: [&str; 2] = ["dev-team", "dev-design-team"];

fn team_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(name)
}

fn dev_team() -> PathBuf {
    team_dir("dev-team")
}

/// The dev team's example deployment, filled in under `dir`.
fn fill_example(dir: &Path) -> PathBuf {
    fill(dir, "dev-team")
}

/// `team`'s example deployment with its placeholders filled with paths
/// under `dir`, written to `dir/config/<team>.yaml`. The bus root stays
/// `~/…`, so `HOME` decides where it lands.
fn fill(dir: &Path, team: &str) -> PathBuf {
    let text = fs::read_to_string(team_dir(team).join("borgchart.example.yaml")).unwrap();
    if team == "dev-team" {
        assert!(text.contains("root: ~/.local/state/brg-teams"), "{text}");
    }
    let work = dir.join("borgchart-dev");
    for w in ["lead", "impl", "review", "design", "notes/design/current"] {
        fs::create_dir_all(work.join(w)).unwrap();
    }
    let filled = text
        .replace(
            &format!("/path/to/borgchart/{team} "),
            &format!("{} ", team_dir(team).display()),
        )
        .replace("/path/to/borgchart-dev", &work.display().to_string())
        .replace("/path/to/config", &dir.join("config").display().to_string());
    assert!(
        !filled
            .lines()
            .any(|l| !l.trim_start().starts_with('#') && l.contains("/path/to")),
        "{filled}"
    );
    let file = dir.join(format!("config/{team}.yaml"));
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
fn both_example_deployments_validate_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    for team in TEAMS {
        let file = fill(dir.path(), team);
        let out = validate(dir.path(), &file);
        let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(out.status.success(), "{team}: {report}");
        assert_eq!(report["errors"], serde_json::json!([]), "{team}: {report}");
        assert_eq!(
            report["warnings"],
            serde_json::json!([]),
            "{team}: {report}"
        );
    }
}

/// Each team's member files, by the `prompt:` lines of its `team.yaml`.
fn member_files(team: &str) -> Vec<PathBuf> {
    let text = fs::read_to_string(team_dir(team).join("team.yaml")).unwrap();
    let mut files: Vec<PathBuf> = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("prompt: "))
        .map(|p| team_dir(team).join(p.trim()))
        .collect();
    files.sort();
    files.dedup();
    files
}

fn includes_of(file: &Path) -> Vec<String> {
    fs::read_to_string(file)
        .unwrap()
        .lines()
        .filter_map(|l| l.strip_prefix("{{includes/")?.strip_suffix("}}"))
        .map(str::to_string)
        .collect()
}

/// Includes resolve, don't nest, and each is shared: text used by one
/// member file belongs in that file.
#[test]
fn includes_resolve_and_are_shared() {
    let files = member_files("dev-team");
    assert_eq!(files.len(), 5, "{files:?}");
    let mut uses: std::collections::BTreeMap<String, usize> = Default::default();
    for file in &files {
        let includes = includes_of(file);
        assert!(includes.contains(&"rules".into()), "{}", file.display());
        let is_lead = file.ends_with("orchestrator.md");
        assert_eq!(
            includes.contains(&"member-bus".into()),
            !is_lead,
            "{}",
            file.display()
        );
        for stem in includes {
            let inc = dev_team().join(format!("includes/{stem}.md"));
            let body = fs::read_to_string(&inc)
                .unwrap_or_else(|e| panic!("{}: {stem}: {e}", file.display()));
            assert!(!body.contains("{{"), "{} nests an include", inc.display());
            *uses.entry(stem).or_default() += 1;
        }
    }
    let mut present: Vec<String> = fs::read_dir(dev_team().join("includes"))
        .unwrap()
        .map(|e| {
            let name = e.unwrap().file_name().to_string_lossy().to_string();
            name.strip_suffix(".md").unwrap_or(&name).to_string()
        })
        .collect();
    present.sort();
    assert_eq!(
        present,
        uses.keys().cloned().collect::<Vec<_>>(),
        "{uses:?}"
    );
    for (stem, n) in &uses {
        assert!(*n >= 2, "includes/{stem}.md is used by {n} member file");
    }
    // The design panel's one member file stands alone.
    for file in member_files("dev-design-team") {
        assert_eq!(includes_of(&file), Vec::<String>::new());
    }
    assert!(!team_dir("dev-design-team").join("includes").exists());
}

/// Claude Code refuses foreground sleep and poll loops, so wherever a
/// Claude member's instructions sleep, they do it in a background command.
#[test]
fn claude_members_sleep_only_in_background_commands() {
    // Member files whose `vendor:` in team.yaml is claude, plus the helpers'
    // shared include (Claude helpers use it too).
    let team = fs::read_to_string(dev_team().join("team.yaml")).unwrap();
    let mut files = vec![dev_team().join("includes/member-bus.md")];
    let mut prompt = None;
    for line in team.lines().map(str::trim) {
        if let Some(p) = line.strip_prefix("prompt: ") {
            prompt = Some(p.to_string());
        }
        if line.starts_with("vendor: claude") {
            files.push(dev_team().join(prompt.take().unwrap()));
        }
    }
    assert!(files.iter().any(|f| f.ends_with("monitor.md")), "{files:?}");
    assert!(
        files.iter().any(|f| f.ends_with("orchestrator.md")),
        "{files:?}"
    );
    let mut sleeps = 0;
    for file in &files {
        let text = fs::read_to_string(file).unwrap();
        // A command line indented under a bullet or step belongs to it.
        let mut item = "";
        for line in text.lines() {
            if !line.starts_with(' ') {
                item = line;
            }
            if line.contains("sleep") {
                sleeps += 1;
                // Either it forbids sleeping, or its item says to run the
                // command in the Bash tool's background mode.
                let forbids = line.to_lowercase().contains("never sleep");
                assert!(
                    forbids || item.contains("**background** mode"),
                    "{}: a sleep outside a background command: {line}",
                    file.display()
                );
            }
        }
    }
    // The lead's design-panel wait and the monitor's watch, at least.
    assert!(sleeps >= 2, "{sleeps}");

    // A helper waiting for its answer does it the way its vendor allows.
    let bus = fs::read_to_string(dev_team().join("includes/member-bus.md")).unwrap();
    assert!(
        bus.contains("**Codex:** run it in the **foreground**"),
        "{bus}"
    );
    assert!(
        bus.contains("**Claude:** run it with your Bash tool in **background** mode"),
        "{bus}"
    );
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

/// SHA-256 of private words (a person's name and handle, a host, private
/// tools and their wrappers), kept as hashes so this public test doesn't
/// name them. Matched against every word, split on non-alphanumerics and
/// also keeping `-`.
const DENIED: [&str; 6] = [
    "78193ef266c1e3c2ce4ea2a86d7fc87e8c52799653faaac8536533a1c9300f82",
    "ffcc96318a5d172bf3bcb94007800f1efcfc56d7e030a30cd1feaa44605fb127",
    "428e626f22d0273bb2741c8747418eaff1828bcd397fe7d00f3ec30d4519d95d",
    "1bd5e42b105faceb0b4de779ebc3c72142dd8777d363a49e70c6b37d8c5fafb3",
    "ecb4780d51076af0ad6855abe157e8465ddfe2ed00b591515f2e0ebca5247b04",
    "96573e904a3798a4d9e6a0dff1b51f2c7ac36a475b28f71ed20cf8d3685f8189",
];

fn denied_word(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let words = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .chain(lower.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')));
    words
        .filter(|w| !w.is_empty())
        .find(|w| DENIED.contains(&borgchart::bus::manifest::sha256_hex(w.as_bytes()).as_str()))
        .map(str::to_string)
}

#[test]
fn the_team_files_stay_generic() {
    assert!(denied_word("the owner merges").is_none());

    let mut files = Vec::new();
    for team in TEAMS {
        let before = files.len();
        walk(&team_dir(team), &mut files);
        assert!(files.len() - before >= 3, "{team}: {files:?}");
    }
    // Machine paths, private tooling and people stay in the private
    // deployment file, never here.
    let home = regex::Regex::new(r"/(home|Users)/[^/\s]+|/tmp/|/root/").unwrap();
    for file in files {
        let text = fs::read_to_string(&file).unwrap();
        assert_eq!(denied_word(&text), None, "{}", file.display());
        assert!(
            !home.is_match(&text),
            "{} has a machine path: {:?}",
            file.display(),
            home.find(&text).map(|m| m.as_str())
        );
    }
}

/// The lead alone under `fake-agent`, then one task for the (Codex)
/// implementer: assign → (start) → accept → questions → result → verify →
/// stop → release, with the lead's background self-wake returning for each
/// reply.
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
            self.cmd(args).output().unwrap()
        }

        fn cmd(&self, args: &[&str]) -> Command {
            let bin = self.dir.path().join("bin");
            let mut cmd = Command::new(env!("CARGO_BIN_EXE_brg"));
            cmd.env_clear()
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
                .arg(&self.file);
            cmd
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
            let json = self.publish_as(member, key, key, draft);
            assert_eq!(json["outcome"], "committed", "{key}: {json}");
            json
        }

        /// Publish from inside `member`, its output kept under `name` (so a
        /// retry of the same key gets its own output file).
        fn publish_as(
            &self,
            member: &str,
            name: &str,
            key: &str,
            draft: serde_json::Value,
        ) -> serde_json::Value {
            let out = self.path(&format!("s/{name}.out"));
            self.run_in(
                member,
                name,
                &format!(
                    "\"$BORGCHART_BIN\" msg publish --key {key} --json > {o} 2>&1 <<'DRAFT'\n{draft}\nDRAFT\necho \"exit $?\" >> {o}\n",
                    o = out.display()
                ),
            );
            let (json, code) = self.output(name);
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

    fn new_team(tag: &str) -> Team {
        let dir = tempfile::tempdir().unwrap();
        let file = fill_example(dir.path());
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        for vendor in ["claude", "codex"] {
            std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_fake-agent"), bin.join(vendor)).unwrap();
        }
        Team {
            socket: format!("brg-dev-team-{tag}-{}", std::process::id()),
            file,
            dir,
        }
    }

    #[test]
    fn one_task_round_with_fake_agents() {
        let team = new_team("round");
        // Resolved, as brg records paths (macOS: /var is /private/var).
        let work = team.path("borgchart-dev").canonicalize().unwrap();

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
        let checkout = work.join("impl").display().to_string();
        let assign = team.publish(
            "lead",
            "t1.assign.1.1",
            serde_json::json!({"to": "implementer", "kind": "assign", "task": "t1", "attempt": 1,
                "body": "Make a fictional change.",
                "facts": {"agreement": {"path": agreement, "sha256": sha}, "scope": "tests", "checkout": checkout}}),
        );
        assert_eq!(assign["id"], "m1");

        // The lead has listed nothing yet, so it waits without `--after`.
        team.background_wait("lead", "wait-1", None, 30);
        team.ok(&["up", "--member", "implementer"]);
        team.wait_idle("implementer");
        let log = fs::read_to_string(team.path("home/fake-agent/implementer.jsonl")).unwrap();
        let start: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
        assert_eq!(start["cwd"], checkout);
        // The implementer is Codex, on the model its file names.
        let argv = start["argv"].to_string();
        assert!(
            argv.starts_with("[\"codex") && argv.contains("gpt-6.1-sol"),
            "{argv}"
        );

        // Its first action: read its inbox, then accept.
        let inbox = team.path("s/impl-inbox.out");
        team.run_in(
            "implementer",
            "impl-inbox",
            &format!(
                "\"$BORGCHART_BIN\" inbox --json > {o} 2>&1; echo \"exit $?\" >> {o}\n",
                o = inbox.display()
            ),
        );
        assert_eq!(ids(&team.output("impl-inbox").0), ["m1"]);
        team.publish(
            "implementer",
            "t1.accept.1.1",
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

        // Two questions in one attempt: each its own key (`.<n>` counts that
        // kind within the attempt); an identical retry is a duplicate.
        let question = |n: u32| {
            serde_json::json!({"to": "lead", "kind": "question", "task": "t1", "attempt": 1,
                "body": format!("Fictional question {n}?")})
        };
        let q1 = team.publish("implementer", "t1.question.1.1", question(1));
        let q2 = team.publish("implementer", "t1.question.1.2", question(2));
        assert_eq!((&q1["id"], &q2["id"]), (&"m3".into(), &"m4".into()));
        let retry = team.publish_as(
            "implementer",
            "t1.question.1.2-retry",
            "t1.question.1.2",
            question(2),
        );
        assert_eq!(
            (&retry["outcome"], &retry["id"]),
            (&"duplicate".into(), &"m4".into())
        );
        team.ack("lead", "m3");
        team.ack("lead", "m4");

        // A Codex helper waits for its answer in the foreground, inside its
        // own turn: it never leaves a wait running in the background.
        let answer = team.path("s/impl-answer.out");
        team.run_in(
            "implementer",
            "impl-answer",
            &format!(
                "\"$BORGCHART_BIN\" inbox wait --after m1 --timeout 30 --json > {o} 2>&1; echo \"exit $?\" >> {o}\n",
                o = answer.display()
            ),
        );
        team.publish(
            "lead",
            "t1.decision.1.1",
            serde_json::json!({"to": "implementer", "kind": "decision", "task": "t1", "attempt": 1,
                "reply_to": ["m4"], "body": "Fictional answer."}),
        );
        let (answered, code) = team.output("impl-answer");
        assert_eq!((code, ids(&answered)), (0, vec!["m5".to_string()]));
        team.ack("implementer", "m5");

        team.background_wait("lead", "wait-3", Some("m5"), 30);
        let report = work.join("notes/t1-report.md");
        fs::write(&report, "status: complete\naction-needed: none\n").unwrap();
        team.publish(
            "implementer",
            "t1.result.1.1",
            serde_json::json!({"to": "lead", "kind": "result", "task": "t1", "attempt": 1, "reply_to": ["m1"],
                "body": "action-needed: none",
                "artifacts": [{"path": report, "label": "report"}],
                "facts": {"status": "complete", "checks": [{"name": "cargo test", "outcome": "pass"}]}}),
        );
        let (woke, code) = team.output("wait-3");
        assert_eq!((code, ids(&woke)), (0, vec!["m6".to_string()]));
        team.ack("lead", "m6");
        team.ack("implementer", "m1");

        team.publish(
            "lead",
            "t1.verify.1.1",
            serde_json::json!({"to": "implementer", "kind": "verify", "task": "t1", "attempt": 1,
                "reply_to": ["m6"], "facts": {"result": "m6", "outcome": "accepted"}}),
        );
        team.ok(&["down", "--member", "implementer"]);
        let show = team.ok(&["task", "show", "t1"]);
        assert!(show.contains("(gone, current)"), "{show}");
        team.publish(
            "lead",
            "t1.release.1.1",
            serde_json::json!({"to": "implementer", "kind": "release", "task": "t1", "attempt": 1,
                "facts": {"evidence": "verified m6; implementer stopped", "checkout": checkout}}),
        );
        let list = team.ok(&["task", "list"]);
        assert!(list.contains("t1  implementer#1  released"), "{list}");

        team.ok(&["down"]);
    }

    /// The lead seats the design panel the way orchestrator.md says: `up`
    /// and `down` of the panel's own deployment, by `--file`, from inside
    /// the lead. Each designer's opening prompt names the brief.
    #[test]
    fn the_lead_seats_the_design_panel_by_file() {
        let team = new_team("design");
        let panel = fill(team.dir.path(), "dev-design-team");
        let brief = team.path("borgchart-dev/notes/design/current/brief.md");
        team.ok(&["up"]);
        team.wait_idle("lead");

        let run = |name: &str, args: &str| {
            let out = team.path(&format!("s/{name}.out"));
            team.run_in(
                "lead",
                name,
                &format!(
                    "\"$BORGCHART_BIN\" {args} --file {f} > {o} 2>&1; echo \"exit $?\" >> {o}\n",
                    f = panel.display(),
                    o = out.display()
                ),
            );
            team.wait_for(&format!("{name} output"), || {
                fs::read_to_string(&out).is_ok_and(|t| t.contains("exit "))
            });
            fs::read_to_string(&out).unwrap()
        };
        let up = run("panel-up", "up");
        assert!(up.ends_with("exit 0\n"), "{up}");
        for designer in ["designer-claude", "designer-codex"] {
            assert!(
                up.lines()
                    .any(|l| l.contains(designer) && l.ends_with("started")),
                "{up}"
            );
            let log = team.path(&format!("home/fake-agent/{designer}.jsonl"));
            team.wait_for(designer, || log.exists());
            let start: serde_json::Value =
                serde_json::from_str(fs::read_to_string(&log).unwrap().lines().next().unwrap())
                    .unwrap();
            let argv = start["argv"].to_string();
            assert!(argv.contains(&brief.display().to_string()), "{argv}");
            assert!(argv.contains("You are a designer"), "{argv}");
            assert_eq!(start["env"]["BORGCHART_MEMBER"], designer);
        }

        let down = run("panel-down", "down");
        assert!(down.ends_with("exit 0\n"), "{down}");
        // The panel is down; the dev team still runs.
        let status = Command::new(env!("CARGO_BIN_EXE_brg"))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", team.path("home"))
            .env("BORGCHART_STATE_HOME", team.path("state"))
            .env("BORGCHART_TMUX_SOCKET", &team.socket)
            .args(["status", "--json", "--file"])
            .arg(&panel)
            .output()
            .unwrap();
        let rows: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
        for row in rows.as_array().unwrap() {
            assert_eq!(row["state"], "stopped", "{rows}");
        }
        team.wait_idle("lead");
        team.ok(&["down"]);
    }

    /// Run brg with exactly the environment the lead was started with, as a
    /// tool process the lead left running would.
    fn lead_cmd(team: &Team) -> Command {
        let log = fs::read_to_string(team.path("home/fake-agent/lead.jsonl")).unwrap();
        let start: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
        let env: std::collections::BTreeMap<String, String> =
            serde_json::from_value(start["env"].clone()).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_brg"));
        cmd.env_clear().envs(env);
        cmd
    }

    fn as_lead(team: &Team, args: &[&str]) -> Output {
        lead_cmd(team).args(args).output().unwrap()
    }

    fn json(out: &Output) -> (serde_json::Value, Option<i32>) {
        (
            serde_json::from_slice(&out.stdout).unwrap_or_default(),
            out.status.code(),
        )
    }

    /// `fenced` from a member's own wait means its session is over: listing
    /// again can't help. Only "… list the inbox again" (the team was
    /// restarted under a cursor) is worth one re-list, and self-wake.md says
    /// exactly that.
    #[test]
    fn fenced_waits_stop_the_loop_except_after_a_restart() {
        let rule = fs::read_to_string(dev_team().join("orchestrator.md")).unwrap();
        assert!(rule.contains("list the inbox again"), "{rule}");

        let team = new_team("fenced");
        team.ok(&["up"]);
        team.wait_idle("lead");
        let note = team.path("note.json");
        fs::write(
            &note,
            r#"{"to": "lead", "kind": "note", "body": "Fictional note."}"#,
        )
        .unwrap();
        let note = note.display().to_string();
        team.ok(&[
            "msg",
            "publish",
            "--external",
            "owner",
            "--key",
            "n1",
            "--record",
            &note,
        ]);
        assert_eq!(
            json(&as_lead(&team, &["inbox", "--json"])).0["records"][0]["id"],
            "m1"
        );

        // The lead is stopped while a wait of its session runs.
        let waiting = {
            let mut cmd = lead_cmd(&team);
            cmd.args([
                "inbox",
                "wait",
                "--after",
                "m1",
                "--timeout",
                "30",
                "--json",
            ]);
            std::thread::spawn(move || cmd.output().unwrap())
        };
        std::thread::sleep(Duration::from_millis(500));
        team.ok(&["down"]);
        let (stale, code) = json(&waiting.join().unwrap());
        assert_eq!(
            (code, &stale["outcome"]),
            (Some(8), &"fenced".into()),
            "{stale}"
        );
        let message = stale["message"].as_str().unwrap();
        assert!(message.contains("lead was stopped"), "{message}");
        assert!(!message.contains("list the inbox again"), "{message}");
        // Listing again from that session is fenced the same way.
        let (relist, code) = json(&as_lead(&team, &["inbox", "--json"]));
        assert_eq!(code, Some(8), "{relist}");
        assert!(
            !relist["message"]
                .as_str()
                .unwrap()
                .contains("list the inbox again")
        );

        // A restart with changed files under a waiting cursor is the one case
        // that says "list the inbox again", and there listing again works.
        let owner = |args: &[&str]| {
            let mut all = vec!["inbox"];
            all.extend_from_slice(args);
            all.extend_from_slice(&["--external", "owner", "--json"]);
            all.iter().map(|s| s.to_string()).collect::<Vec<_>>()
        };
        team.ok(&["up"]);
        team.wait_idle("lead");
        let args = owner(&["wait", "--after", "m1", "--timeout", "30"]);
        let waiter = {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let mut cmd = team.cmd(&args);
            std::thread::spawn(move || cmd.output().unwrap())
        };
        std::thread::sleep(Duration::from_millis(500));
        team.ok(&["down"]);
        let mut text = fs::read_to_string(&team.file).unwrap();
        text.push_str("# changed\n");
        fs::write(&team.file, text).unwrap();
        team.ok(&["up"]);
        let (restarted, code) = json(&waiter.join().unwrap());
        assert_eq!(code, Some(8), "{restarted}");
        assert!(
            restarted["message"]
                .as_str()
                .unwrap()
                .ends_with("list the inbox again"),
            "{restarted}"
        );
        let relist = owner(&[]);
        let relist: Vec<&str> = relist.iter().map(String::as_str).collect();
        team.ok(&relist);
        team.ok(&["down"]);
    }
}
