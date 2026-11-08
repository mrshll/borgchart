//! A development team (`dev-team/`) and its design panel
//! (`dev-design-team/`): the README's deployment validates, the member
//! files expand and stay generic, one task round works with `fake-agent`
//! members, and the lead can seat the design panel.

mod common;

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

/// The ```yaml blocks of the dev team's README: the configuration a
/// reader copies.
fn readme_configs() -> Vec<String> {
    let text = fs::read_to_string(dev_team().join("README.md")).unwrap();
    let mut blocks = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if line.trim() == "```yaml" {
            let block: Vec<&str> = lines.by_ref().take_while(|l| l.trim() != "```").collect();
            blocks.push(block.join("\n") + "\n");
        }
    }
    blocks
}

/// A pinned copy of both teams in `dir/teams`, as the README sets them up,
/// with a README configuration block pointing there (`dir/config.yaml`).
/// The instance works in `dir/work/repo`; the panel in `dir/work/design`.
fn setup(dir: &Path, text: &str) -> PathBuf {
    for w in ["repo", "design"] {
        fs::create_dir_all(dir.join("work").join(w)).unwrap();
    }
    for team in TEAMS {
        copy_dir(&team_dir(team), &dir.join("teams").join(team));
    }
    let filled = text.replace("/path/to/teams", &dir.join("teams").display().to_string());
    assert!(!filled.contains("/path/to"), "{filled}");
    let file = dir.join("config.yaml");
    fs::write(&file, filled).unwrap();
    fs::create_dir_all(dir.join("home")).unwrap();
    file
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap().flatten() {
        let target = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &target);
        } else {
            fs::copy(e.path(), &target).unwrap();
        }
    }
}

/// A member file's standing instruction: its text after the front matter.
fn member_body(file: &Path) -> String {
    let text = fs::read_to_string(file).unwrap();
    text.splitn(3, "---\n").nth(2).unwrap().to_string()
}

fn validate(dir: &Path, config: &Path, team: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_brg"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", dir.join("home"))
        .env("BORGCHART_STATE_HOME", dir.join("state"))
        .env("BORGCHART_CONFIG", config)
        .args(["validate", "--json", team])
        .output()
        .unwrap()
}

/// What a reader copies from the README works as written: both teams
/// validate by name with no errors and no warnings.
#[test]
fn the_readme_configuration_validates_cleanly() {
    let blocks = readme_configs();
    assert!(!blocks.is_empty());
    for (i, text) in blocks.iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let config = setup(dir.path(), text);
        for team in TEAMS {
            let out = validate(dir.path(), &config, team);
            let report: serde_json::Value = serde_json::from_slice(&out.stdout)
                .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&out.stderr)));
            assert!(out.status.success(), "block {i} {team}: {report}");
            assert_eq!(
                report["errors"],
                serde_json::json!([]),
                "block {i} {team}: {report}"
            );
            assert_eq!(
                report["warnings"],
                serde_json::json!([]),
                "block {i} {team}: {report}"
            );
        }
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
/// member file belongs in that file. Every dev-team member, the lead
/// included, gets the shared base.
#[test]
fn includes_resolve_and_are_shared() {
    let files = member_files("dev-team");
    assert_eq!(files.len(), 5, "{files:?}");
    let mut uses: std::collections::BTreeMap<String, usize> = Default::default();
    for file in &files {
        let includes = includes_of(file);
        assert!(includes.contains(&"base".into()), "{}", file.display());
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

/// Claude Code refuses foreground sleep and poll loops, and Codex starts no
/// turn when a background command exits. So wherever a Claude member's
/// instructions sleep, they do it in a background command, and every wait
/// a member of either vendor may run says how that vendor runs it.
#[test]
fn members_wait_the_way_their_vendor_allows() {
    // Member files run as Claude by team.yaml, plus the shared base (Claude
    // helpers use it too) and the designer (one designer is Claude).
    let mut files = vec![
        dev_team().join("includes/base.md"),
        team_dir("dev-design-team").join("designer.md"),
    ];
    let team = fs::read_to_string(dev_team().join("team.yaml")).unwrap();
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
        // A line belongs to the nearest bullet or step that starts at or
        // before it; an indented command line belongs to its item.
        let mut item = "";
        for line in text.lines() {
            let t = line.trim_start();
            let starts_item = !line.starts_with(' ')
                || t.starts_with("- ")
                || t.split_once(". ")
                    .is_some_and(|(n, _)| n.chars().all(|c| c.is_ascii_digit()));
            if starts_item {
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
    // The lead's design-panel wait, the monitor's watch and the Claude
    // designer's wait for answers, at least.
    assert!(sleeps >= 3, "{sleeps}");

    // A helper waiting for its answer, and a designer waiting for the
    // lead's answers, do it the way their vendor allows.
    let base = fs::read_to_string(dev_team().join("includes/base.md")).unwrap();
    assert!(
        base.contains("**Codex:** run it in the **foreground**"),
        "{base}"
    );
    assert!(
        base.contains("**Claude:** run it with your Bash tool in **background** mode"),
        "{base}"
    );
    let designer = fs::read_to_string(team_dir("dev-design-team").join("designer.md")).unwrap();
    assert!(
        designer.contains("**Claude:** start, with your Bash tool in **background** mode"),
        "{designer}"
    );
    assert!(
        designer.contains("**Codex:** run the same loop in the **foreground**, at most 10 minutes"),
        "{designer}"
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
    // Only the owner's handle is allowed, only in `dev-team/`, and only as
    // `@<handle>`: other forms and other denied words stay denied.
    assert!(denied_word("mrshll-agent").is_some());

    let mut files = Vec::new();
    for team in TEAMS {
        let before = files.len();
        walk(&team_dir(team), &mut files);
        assert!(files.len() - before >= 2, "{team}: {files:?}");
    }
    // Machine paths, private tooling and people stay in the private
    // deployment file, never here.
    let home = regex::Regex::new(r"/(home|Users)/[^/\s]+|/tmp/|/root/").unwrap();
    // The one exception: `dev-team/` names the repository's owner and
    // reviewer by his public handle, exactly as `@<handle>`.
    let owner = regex::Regex::new(r"@mrshll([^A-Za-z0-9_-]|$)").unwrap();
    let dev_team = team_dir("dev-team");
    for file in files {
        let text = fs::read_to_string(&file).unwrap();
        let text = if file.starts_with(&dev_team) {
            owner.replace_all(&text, "$1").to_string()
        } else {
            text
        };
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
        /// First, so its servers stop before the test's directory goes.
        tmux: crate::common::PrivateTmux,
        dir: tempfile::TempDir,
        config: PathBuf,
        socket: String,
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
                    self.tmux.path_with(&format!(
                        "{}:/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin",
                        bin.display()
                    )),
                )
                .env("HOME", self.dir.path().join("home"))
                .env("LANG", "C.UTF-8")
                .env("BORGCHART_STATE_HOME", self.dir.path().join("state"))
                .env("BORGCHART_CONFIG", &self.config)
                .args(args);
            self.tmux.brg_env(&mut cmd, &self.socket);
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

        fn read(&self, rel: &str) -> Option<serde_json::Value> {
            serde_json::from_slice(&fs::read(self.path(rel)).ok()?).ok()
        }

        /// The instance of `team`, once created.
        fn instance_of(&self, team: &str) -> Option<String> {
            let reg = self.read("state/registry.json")?;
            reg["instances"]
                .as_object()?
                .iter()
                .find(|(_, e)| e["team"] == team)
                .map(|(id, _)| id.clone())
        }

        /// The dev team's instance.
        fn inst(&self) -> String {
            self.instance_of("dev-team")
                .expect("the dev team's instance")
        }

        /// The member of `role` in `instance`.
        fn member_in(&self, instance: &str, role: &str) -> String {
            let rec = self
                .read(&format!("state/{instance}/instance.json"))
                .expect("its record");
            rec["members"]
                .as_object()
                .unwrap()
                .iter()
                .find(|(_, m)| m["role"] == role)
                .map(|(id, _)| id.clone())
                .unwrap_or_else(|| panic!("no member of `{role}` in {instance}"))
        }

        /// The dev team's member of `role`.
        fn m(&self, role: &str) -> String {
            self.member_in(&self.inst(), role)
        }

        /// `role`'s member, addressed in full.
        fn at(&self, role: &str) -> String {
            format!("{}/{}", self.inst(), self.m(role))
        }

        /// `role`'s member's fake-agent log.
        fn log(&self, role: &str) -> String {
            fs::read_to_string(self.path(&format!("home/fake-agent/{}.jsonl", self.m(role))))
                .unwrap()
        }

        /// Run a shell script inside `role`'s member, as its own tool call.
        fn run_in(&self, role: &str, name: &str, script: &str) {
            let path = self.path(&format!("s/{name}.sh"));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, script).unwrap();
            self.wait_idle(role);
            self.ok(&[
                "send",
                &self.at(role),
                &format!("tool sh {}", path.display()),
            ]);
        }

        fn wait_idle(&self, role: &str) {
            self.wait_for(&format!("{role} idle"), || {
                let rows: serde_json::Value =
                    serde_json::from_slice(&self.brg(&["status", &self.inst(), "--json"]).stdout)
                        .unwrap_or_default();
                rows.as_array().is_some_and(|rows| {
                    rows.iter()
                        .any(|r| r["role"] == role && r["kind"] == "member" && r["state"] == "idle")
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
        fn background_wait(&self, role: &str, name: &str, after: Option<&str>, timeout: u32) {
            let out = self.path(&format!("s/{name}.out"));
            let after = after.map(|a| format!("--after {a} ")).unwrap_or_default();
            self.run_in(
                role,
                name,
                &format!(
                    "( \"$BORGCHART_BIN\" inbox wait {after}--timeout {timeout} --json > {o} 2>&1; echo \"exit $?\" >> {o} ) </dev/null >/dev/null 2>&1 &\n",
                    o = out.display()
                ),
            );
        }

        /// Publish a draft from inside `role`'s member.
        fn publish(&self, role: &str, key: &str, draft: serde_json::Value) -> serde_json::Value {
            let json = self.publish_as(role, key, key, draft);
            assert_eq!(json["outcome"], "committed", "{key}: {json}");
            json
        }

        /// Publish from inside `role`'s member, its output kept under `name`
        /// (so a retry of the same key gets its own output file).
        fn publish_as(
            &self,
            role: &str,
            name: &str,
            key: &str,
            draft: serde_json::Value,
        ) -> serde_json::Value {
            let out = self.path(&format!("s/{name}.out"));
            self.run_in(
                role,
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

        fn ack(&self, role: &str, id: &str) {
            let name = format!("ack-{role}-{id}");
            let out = self.path(&format!("s/{name}.out"));
            self.run_in(
                role,
                &name,
                &format!(
                    "\"$BORGCHART_BIN\" msg ack {id} --json > {o} 2>&1; echo \"exit $?\" >> {o}\n",
                    o = out.display()
                ),
            );
            assert_eq!(self.output(&name).1, 0);
        }

        /// Create the dev team's instance in the repo checkout.
        fn create(&self) -> String {
            let out = self.ok(&[
                "create",
                "dev-team",
                &self.path("work/repo").display().to_string(),
            ]);
            self.wait_idle("lead");
            out
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
        let config = setup(dir.path(), &readme_configs()[0]);
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        for vendor in ["claude", "codex"] {
            std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_fake-agent"), bin.join(vendor)).unwrap();
        }
        let tmux = crate::common::PrivateTmux::new();
        Team {
            socket: tmux.socket(&format!("brg-dev-team-{tag}")),
            tmux,
            config,
            dir,
        }
    }

    #[test]
    #[ignore = "rewritten in impl-4"]
    fn one_task_round_with_fake_agents() {
        let team = new_team("round");
        // Resolved, as brg records paths (macOS: /var is /private/var).
        let repo = team.path("work/repo").canonicalize().unwrap();

        let up = team.create();
        let lead = team.m("lead");
        assert!(
            up.lines()
                .any(|l| l.contains(&lead) && l.ends_with("started")),
            "{up}"
        );
        assert!(
            up.contains("implementer   on demand (none created)")
                || up.contains("on demand (none created)"),
            "{up}"
        );

        // The lead's opening prompt is its expanded role file.
        let start: serde_json::Value =
            serde_json::from_str(team.log("lead").lines().next().unwrap()).unwrap();
        let prompt = start["argv"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .as_str()
            .unwrap();
        let lead_body = member_body(&dev_team().join("orchestrator.md"));
        let base = fs::read_to_string(dev_team().join("includes/base.md")).unwrap();
        let opening = lead_body.lines().find(|l| !l.trim().is_empty()).unwrap();
        assert!(prompt.starts_with(opening.trim()), "{prompt}");
        assert!(prompt.contains(base.lines().next().unwrap()), "{prompt}");
        assert!(!prompt.contains("{{"), "{prompt}");
        assert_eq!(start["env"]["BORGCHART_BIN"], env!("CARGO_BIN_EXE_brg"));
        assert_eq!(start["cwd"], repo.display().to_string());
        let notes = PathBuf::from(start["env"]["BORGCHART_NOTES"].as_str().unwrap());
        assert!(notes.is_dir(), "{notes:?}");

        // The implementer exists, stopped, and its assignment waits for it.
        team.ok(&["create", &format!("{}/implementer", team.inst())]);
        team.wait_idle("implementer");
        let implementer = team.m("implementer");
        team.ok(&["stop", &team.at("implementer")]);
        let agreement = notes.join("t1-agreement.md");
        fs::write(&agreement, "Task t1: a fictional change.\n").unwrap();
        let sha = borgchart::bus::manifest::sha256_hex(&fs::read(&agreement).unwrap());
        let checkout = repo.display().to_string();
        let assign = team.publish(
            "lead",
            "t1.assign.1.1",
            serde_json::json!({"to": implementer, "kind": "assign", "task": "t1", "attempt": 1,
                "body": "Make a fictional change.",
                "facts": {"agreement": {"path": agreement, "sha256": sha}, "scope": "tests", "checkout": checkout}}),
        );
        assert_eq!(assign["id"], "m1");

        // The lead has listed nothing yet, so it waits without `--after`.
        team.background_wait("lead", "wait-1", None, 30);
        team.ok(&["start", &team.at("implementer")]);
        team.wait_idle("implementer");
        let log = team.log("implementer");
        // Its latest start: the one after the assignment.
        let start: serde_json::Value = log
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .rfind(|e| e["event"] == "start")
            .unwrap();
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
            serde_json::json!({"to": lead, "kind": "accept", "task": "t1", "attempt": 1, "reply_to": ["m1"]}),
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
            serde_json::json!({"to": lead, "kind": "question", "task": "t1", "attempt": 1,
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
            serde_json::json!({"to": implementer, "kind": "decision", "task": "t1", "attempt": 1,
                "reply_to": ["m4"], "body": "Fictional answer."}),
        );
        let (answered, code) = team.output("impl-answer");
        assert_eq!((code, ids(&answered)), (0, vec!["m5".to_string()]));
        team.ack("implementer", "m5");

        team.background_wait("lead", "wait-3", Some("m5"), 30);
        let report = notes.join("t1-report.md");
        fs::write(&report, "status: complete\naction-needed: none\n").unwrap();
        team.publish(
            "implementer",
            "t1.result.1.1",
            serde_json::json!({"to": lead, "kind": "result", "task": "t1", "attempt": 1, "reply_to": ["m1"],
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
            serde_json::json!({"to": implementer, "kind": "verify", "task": "t1", "attempt": 1,
                "reply_to": ["m6"], "facts": {"result": "m6", "outcome": "accepted"}}),
        );
        team.ok(&["stop", &team.at("implementer")]);
        let inst = team.inst();
        let show = team.ok(&["task", "show", "t1", "--instance", &inst]);
        assert!(show.contains("(gone, current)"), "{show}");
        team.publish(
            "lead",
            "t1.release.1.1",
            serde_json::json!({"to": implementer, "kind": "release", "task": "t1", "attempt": 1,
                "facts": {"evidence": "verified m6; implementer stopped", "checkout": checkout}}),
        );
        let list = team.ok(&["task", "list", "--instance", &inst]);
        assert!(
            list.contains(&format!("t1  {implementer}#1  released")),
            "{list}"
        );

        team.ok(&["stop", &inst]);
    }

    /// The lead seats the design panel as a separate instance, from inside
    /// the lead: `create` from the panel's team in the design checkout, then
    /// `stop` and `delete` by the instance's ID.
    #[test]
    fn the_lead_seats_the_design_panel_as_an_instance() {
        let team = new_team("design");
        team.create();

        let run = |name: &str, args: &str| {
            let out = team.path(&format!("s/{name}.out"));
            team.run_in(
                "lead",
                name,
                &format!(
                    "\"$BORGCHART_BIN\" {args} > {o} 2>&1; echo \"exit $?\" >> {o}\n",
                    o = out.display()
                ),
            );
            team.wait_for(&format!("{name} output"), || {
                fs::read_to_string(&out).is_ok_and(|t| t.contains("exit "))
            });
            fs::read_to_string(&out).unwrap()
        };
        let up = run(
            "panel-up",
            &format!(
                "create dev-design-team {}",
                team.path("work/design").display()
            ),
        );
        assert!(up.ends_with("exit 0\n"), "{up}");
        let panel = up.split_whitespace().next().unwrap().to_string();
        assert!(panel.starts_with("dev-design-team-"), "{up}");
        for designer in ["designer-claude", "designer-codex"] {
            let member = team.member_in(&panel, designer);
            assert!(
                up.lines()
                    .any(|l| l.contains(&member) && l.ends_with("started")),
                "{up}"
            );
            let log = team.path(&format!("home/fake-agent/{member}.jsonl"));
            team.wait_for(designer, || log.exists());
            let start: serde_json::Value =
                serde_json::from_str(fs::read_to_string(&log).unwrap().lines().next().unwrap())
                    .unwrap();
            let argv = start["argv"].to_string();
            assert!(argv.contains("You are a designer"), "{argv}");
            assert_eq!(start["env"]["BORGCHART_MEMBER"], member.as_str());
            assert_eq!(start["env"]["BORGCHART_INSTANCE"], panel.as_str());
        }

        // The file exchange: each designer writes its questions and its
        // proposal under the names designer.md gives, and the lead's
        // documented wait finds them.
        let designer_rule =
            fs::read_to_string(team_dir("dev-design-team").join("designer.md")).unwrap();
        for name in ["questions", "proposal"] {
            assert!(
                designer_rule.contains(&format!("`{name}-$BORGCHART_ROLE.md`")),
                "designer.md names its {name} file by role"
            );
        }
        let notes = team
            .path("state")
            .join(&panel)
            .join("notes")
            .canonicalize()
            .unwrap();
        for designer in ["designer-claude", "designer-codex"] {
            let member = team.member_in(&panel, designer);
            let script = team.path(&format!("s/{designer}.sh"));
            fs::create_dir_all(script.parent().unwrap()).unwrap();
            fs::write(
                &script,
                "for f in questions proposal; do echo none > \"$BORGCHART_NOTES/.tmp-$f\"; mv \"$BORGCHART_NOTES/.tmp-$f\" \"$BORGCHART_NOTES/$f-$BORGCHART_ROLE.md\"; done\n",
            )
            .unwrap();
            team.wait_for(&format!("{designer} idle"), || {
                let rows: serde_json::Value =
                    serde_json::from_slice(&team.brg(&["status", &panel, "--json"]).stdout)
                        .unwrap_or_default();
                rows.as_array().is_some_and(|r| {
                    r.iter()
                        .any(|r| r["member"] == member.as_str() && r["state"] == "idle")
                })
            });
            team.ok(&[
                "send",
                &format!("{panel}/{member}"),
                &format!("tool sh {}", script.display()),
            ]);
        }
        let rule = fs::read_to_string(dev_team().join("orchestrator.md")).unwrap();
        let wait = rule
            .lines()
            .find(|l| l.contains("until {") && l.contains("questions-designer-claude.md"))
            .unwrap()
            .trim()
            .trim_matches('`')
            .replace("<deadline seconds>", "20")
            .replace("<dir>", &notes.display().to_string());
        // Once the designers have written (four files), the documented
        // wait must end at once: its first check finds them.
        team.wait_for("the designers' files", || {
            fs::read_dir(&notes).map(|d| d.count()).unwrap_or(0) >= 4
        });
        for (name, wait) in [
            ("questions", wait.clone()),
            ("proposal", wait.replace("questions-", "proposal-")),
        ] {
            let started = std::time::Instant::now();
            let out = Command::new("sh").args(["-c", &wait]).output().unwrap();
            let listed = String::from_utf8_lossy(&out.stdout).to_string();
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "{name}: waited out: {listed}"
            );
            for designer in ["designer-claude", "designer-codex"] {
                assert!(
                    listed.contains(&format!("{name}-{designer}.md")),
                    "{name}: {listed}"
                );
            }
        }

        let down = run("panel-down", &format!("stop {panel}"));
        assert!(down.ends_with("exit 0\n"), "{down}");
        let rows: serde_json::Value =
            serde_json::from_slice(&team.brg(&["status", &panel, "--json"]).stdout).unwrap();
        for row in rows.as_array().unwrap() {
            assert_eq!(row["state"], "stopped", "{rows}");
        }
        let gone = run("panel-delete", &format!("delete {panel}"));
        assert!(gone.ends_with("exit 0\n"), "{gone}");
        assert!(team.instance_of("dev-design-team").is_none());
        // The dev team still runs.
        team.wait_idle("lead");
        team.ok(&["stop", &team.inst()]);
    }

    /// Run brg with exactly the environment the lead was started with, as a
    /// tool process the lead left running would.
    fn lead_cmd(team: &Team) -> Command {
        lead_cmd_at(team, 0)
    }

    /// As [`lead_cmd`], with the environment of the lead's `n`th start.
    fn lead_cmd_at(team: &Team, n: usize) -> Command {
        let log = team.log("lead");
        let start: serde_json::Value = log
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|e| e["event"] == "start")
            .nth(n)
            .unwrap();
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

    /// Self-waking uses a background wait with no cursor. The foreground
    /// wait still allows a re-list when storage restarts under a cursor;
    /// a stopped member's session cannot recover that way.
    #[test]
    fn fenced_waits_stop_the_loop_except_after_a_restart() {
        let rule = fs::read_to_string(dev_team().join("orchestrator.md")).unwrap();
        let wake = rule
            .split("## Waking yourself")
            .nth(1)
            .unwrap()
            .split("## Talking to the owner")
            .next()
            .unwrap();
        assert!(wake.contains("inbox wait --background --json"), "{wake}");
        assert!(wake.contains("start no more waits"), "{wake}");

        let team = new_team("fenced");
        team.create();
        let inst = team.inst();
        let lead = team.m("lead");
        let note = team.path("note.json");
        fs::write(
            &note,
            format!(r#"{{"to": "{lead}", "kind": "note", "body": "Fictional note."}}"#),
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
            "--instance",
            &inst,
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
        team.ok(&["stop", &inst]);
        let (stale, code) = json(&waiting.join().unwrap());
        assert_eq!(
            (code, &stale["outcome"]),
            (Some(8), &"fenced".into()),
            "{stale}"
        );
        let message = stale["message"].as_str().unwrap();
        assert!(
            message.contains(&format!("{lead} was stopped")),
            "{message}"
        );
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
        // (Open work isn't carried to fresh messages yet, so the note is
        // read first.)
        let owner = |args: &[&str]| {
            let mut all = vec!["inbox"];
            all.extend_from_slice(args);
            all.extend_from_slice(&["--external", "owner", "--json", "--instance", &inst]);
            all.iter().map(|s| s.to_string()).collect::<Vec<_>>()
        };
        team.ok(&["start", &inst]);
        team.wait_idle("lead");
        let ack = lead_cmd_at(&team, 1)
            .args(["msg", "ack", "m1", "--json"])
            .output()
            .unwrap();
        assert!(
            ack.status.success(),
            "{}",
            String::from_utf8_lossy(&ack.stdout)
        );
        let args = owner(&["wait", "--after", "m1", "--timeout", "30"]);
        let waiter = {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let mut cmd = team.cmd(&args);
            std::thread::spawn(move || cmd.output().unwrap())
        };
        std::thread::sleep(Duration::from_millis(500));
        team.ok(&["stop", &inst]);
        let file = team.path("teams/dev-team/team.yaml");
        let mut text = fs::read_to_string(&file).unwrap();
        text.push_str("# changed\n");
        fs::write(&file, text).unwrap();
        team.ok(&["start", &inst]);
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
        team.ok(&["stop", &inst]);
    }
}
