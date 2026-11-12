//! The sibling definitions under `templates/`: the engineering department,
//! dev team and design panel: the catalog guide's deployment
//! validate, the member files expand and stay generic, one message round
//! works with `fake-agent` members, the department lead seats a panel, and the
//! department lead starts a dev team.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TEAMS: [&str; 2] = ["dev-team", "dev-design-team"];

/// The installable catalog: sibling teams and one shared asset folder.
fn catalog() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("templates")
}

fn team_dir(name: &str) -> PathBuf {
    catalog().join(name)
}

fn department() -> PathBuf {
    team_dir("engineering")
}

fn shared() -> PathBuf {
    catalog().join("shared")
}

fn dev_team() -> PathBuf {
    team_dir("dev-team")
}

/// The ```yaml blocks of CONTRIBUTING.md: the configuration a
/// reader copies.
fn readme_configs() -> Vec<String> {
    let text = fs::read_to_string(catalog().join("../CONTRIBUTING.md")).unwrap();
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

/// The guide's commands that start with `prefix`, as a reader copies them
/// (each inside backticks).
fn readme_commands(prefix: &str) -> Vec<String> {
    let text = fs::read_to_string(catalog().join("../CONTRIBUTING.md")).unwrap();
    text.split('`')
        .skip(1)
        .step_by(2)
        .filter(|command| command.starts_with(prefix))
        .map(str::to_string)
        .collect()
}

/// As CONTRIBUTING.md sets it up: a pinned copy of the complete template catalog
/// in `dir/pinned/templates`, where brg resolves names; and its configuration
/// in `dir/config.yaml`. The team's folder is `dir/work`, a checkout
/// with one baseline commit pushed to a local remote.
fn setup(dir: &Path, text: &str) -> PathBuf {
    let origin = dir.join("origin.git").display().to_string();
    let work = dir.join("work");
    git_in(dir, &["init", "-q", "--bare", &origin]);
    git_in(dir, &["clone", "-q", &origin, &work.display().to_string()]);
    git_in(
        &work,
        &[
            "-c",
            "user.name=Fictional Dev",
            "-c",
            "user.email=dev@example.com",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "baseline",
        ],
    );
    git_in(&work, &["push", "-q", "origin", "HEAD"]);
    copy_dir(&catalog(), &dir.join("pinned/templates"));
    let file = dir.join("pinned/config.yaml");
    fs::write(&file, text).unwrap();
    fs::create_dir_all(dir.join("home")).unwrap();
    fs::create_dir_all(dir.join("pinned/bin")).unwrap();
    let plugin = Path::new(env!("CARGO_BIN_EXE_brg")).with_file_name("brg-seat");
    assert!(
        plugin.is_file(),
        "build workspace binaries before department tests"
    );
    std::os::unix::fs::symlink(&plugin, dir.join("pinned/bin/brg-seat")).unwrap();
    file
}

/// Run git in `dir` with no user or system configuration; its output.
fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = common::command("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
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
        .current_dir(dir.join("pinned"))
        .env_clear()
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", dir.join("pinned/bin").display()),
        )
        .env("HOME", dir.join("home"))
        .env("BORGCHART_STATE_HOME", dir.join("state"))
        .env("BORGCHART_CONFIG", config)
        .args(["validate", "--json", team])
        .output()
        .unwrap()
}

/// What a reader copies from the guide works as written: the teams
/// validate by its catalog paths, with no errors and no warnings.
#[test]
fn the_readme_configuration_validates_cleanly() {
    let blocks = readme_configs();
    assert!(!blocks.is_empty());
    let teams: Vec<String> = readme_commands("brg validate ")
        .iter()
        .flat_map(|c| c.split(' ').skip(2).map(str::to_string).collect::<Vec<_>>())
        .collect();
    assert_eq!(teams.len(), 3, "{teams:?}");
    for (i, text) in blocks.iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let config = setup(dir.path(), text);
        for team in &teams {
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
/// The department validates from its folder, with the guide's
/// configuration, cleanly.
#[test]
fn the_parent_template_validates_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let config = setup(dir.path(), &readme_configs()[0]);
    let out = validate(dir.path(), &config, "engineering");
    let report: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&out.stderr)));
    assert!(out.status.success(), "{report}");
    assert_eq!(report["errors"], serde_json::json!([]), "{report}");
    assert_eq!(report["warnings"], serde_json::json!([]), "{report}");
}

/// Each team's member files, by the `prompt:` lines of its `template.yaml`.
fn member_files(team: &str) -> Vec<PathBuf> {
    let text = fs::read_to_string(team_dir(team).join("template.yaml")).unwrap();
    let mut files: Vec<PathBuf> = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("prompt: "))
        .map(|p| team_dir(team).join(p.trim()))
        .collect();
    files.sort();
    files.dedup();
    files
}

/// The includes a member file takes, directly or through the other
/// template's role file it takes with `{{templates/<template>/<role>}}`.
fn includes_of(file: &Path) -> Vec<String> {
    fs::read_to_string(file)
        .unwrap()
        .lines()
        .flat_map(|l| {
            if let Some(stem) = l
                .strip_prefix("{{includes/")
                .and_then(|l| l.strip_suffix("}}"))
            {
                return vec![stem.to_string()];
            }
            match l
                .strip_prefix("{{templates/")
                .and_then(|l| l.strip_suffix("}}"))
            {
                Some(role) => includes_of(&catalog().join(format!("{role}.md"))),
                None => Vec::new(),
            }
        })
        .collect()
}
/// Includes resolve, don't nest, and each is shared: text used by one
/// member file belongs in that file. Every dev-team member, the lead
/// included, gets the common rules.
#[test]
fn includes_resolve_and_are_shared() {
    let mut files = member_files("dev-team");
    assert_eq!(files.len(), 5, "{files:?}");
    files.extend(member_files("engineering"));
    let mut uses: std::collections::BTreeMap<String, usize> = Default::default();
    for file in &files {
        let includes = includes_of(file);
        assert!(includes.contains(&"common".into()), "{}", file.display());
        for stem in includes {
            let inc = shared().join(format!("includes/{stem}.md"));
            let body = fs::read_to_string(&inc)
                .unwrap_or_else(|e| panic!("{}: {stem}: {e}", file.display()));
            assert!(!body.contains("{{"), "{} nests an include", inc.display());
            *uses.entry(stem).or_default() += 1;
        }
    }
    let mut present: Vec<String> = fs::read_dir(shared().join("includes"))
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

/// Shared GitHub-write procedures reach only the department/dev leads and
/// local committers, including when the dev team runs standalone.
#[test]
fn git_rules_reach_only_the_roles_that_write() {
    let git = fs::read_to_string(shared().join("includes/git.md")).unwrap();
    for folder in [department(), dev_team(), team_dir("dev-design-team")] {
        let config = borgchart::config::Config {
            teams: vec![catalog()],
            ..Default::default()
        };
        let (report, validated) = borgchart::validate::validate_with(&folder, config);
        let team = validated.unwrap_or_else(|| panic!("{report:?}")).team;
        for (id, role) in &team.roles {
            assert_eq!(
                role.instruction.contains(git.trim()),
                id == "lead" || id == "implementer",
                "{} {id}",
                folder.display()
            );
        }
    }
}

/// Claude Code refuses foreground sleep and poll loops, and Codex starts no
/// turn when a background command exits. So wherever a Claude member's
/// instructions sleep, they do it in a background command, and every wait
/// a member of either vendor may run says how that vendor runs it.
#[test]
fn members_wait_the_way_their_vendor_allows() {
    // Member files run as Claude by template.yaml, plus the helper rules (Claude
    // helpers use them too) and the designer (one designer is Claude).
    let mut files = vec![
        shared().join("includes/helper.md"),
        team_dir("dev-design-team").join("designer.md"),
    ];
    let team = fs::read_to_string(dev_team().join("template.yaml")).unwrap();
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
    files.push(shared().join("includes/monitor.md"));
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
    let base = fs::read_to_string(shared().join("includes/helper.md")).unwrap();
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
        .find(|w| DENIED.contains(&borgchart::config_snapshot::sha256_hex(w.as_bytes()).as_str()))
        .map(str::to_string)
}

/// Words that belong to this repository, not to team files: its name, and
/// its own rules (its build wrapper, checks and language), which live in its
/// `AGENTS.md`.
const PROJECT_WORDS: [&str; 7] = [
    "borgchart",
    "borgchart-engineering",
    "cargo",
    "cargo-slot",
    "githooks",
    "clippy",
    "rust",
];

#[test]
fn the_team_files_stay_generic() {
    assert!(denied_word("the owner merges").is_none());
    // No owner's handle, machine account or GitHub wrapper either: those
    // come from the owner's own instructions, so any owner can run the teams.
    assert!(denied_word("mrshll-agent").is_some());

    // Every sibling definition and shared asset in the catalog.
    let mut files = Vec::new();
    walk(&catalog(), &mut files);
    assert!(files.contains(&department().join("lead.md")), "{files:?}");
    for team in TEAMS {
        let held = files.iter().filter(|f| f.starts_with(team_dir(team)));
        assert!(held.count() >= 2, "{team}: {files:?}");
    }
    // Machine paths and private names never belong here.
    let home = regex::Regex::new(r"/(home|Users)/[^/\s]+|/tmp/|/root/").unwrap();
    let word = regex::Regex::new(r"[A-Za-z0-9_-]+").unwrap();
    for file in files {
        let text = fs::read_to_string(&file).unwrap();
        assert_eq!(denied_word(&text), None, "{}", file.display());
        let project = word
            .find_iter(&text.to_lowercase())
            .map(|m| m.as_str().to_string())
            .find(|w| PROJECT_WORDS.contains(&w.as_str()));
        assert_eq!(project, None, "{} names a project rule", file.display());
        assert!(
            !home.is_match(&text),
            "{} has a machine path: {:?}",
            file.display(),
            home.find(&text).map(|m| m.as_str())
        );
    }
}

/// One text for both vendors: `CLAUDE.md` only imports `AGENTS.md` (which
/// Codex reads), so no rule is Claude-only; and `AGENTS.md` holds the rules
/// the shared includes defer to. Whether each vendor loads it is the live
/// tier's concern.
#[test]
fn claude_md_only_imports_agents_md() {
    let root = catalog().join("..");
    let claude = fs::read_to_string(root.join("CLAUDE.md")).unwrap();
    assert_eq!(claude.trim(), "@AGENTS.md");
    let agents = fs::read_to_string(root.join("AGENTS.md")).unwrap();
    // What the shared includes defer to: the build wrapper, the checks and
    // the public-repository rule.
    for rule in ["tools/cargo-slot", ".githooks/checks", "fictional names"] {
        assert!(agents.contains(rule), "AGENTS.md lacks {rule}");
    }
}

/// The role files' commands perform a brief/report exchange with fake agents,
/// then the lead stops the helper and checks its unread count.
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
            cmd.current_dir(self.path("pinned"))
                .env_clear()
                .env(
                    "PATH",
                    self.tmux.path_with(&format!(
                        "{}:{}:/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin",
                        self.path("pinned/bin").display(),
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

        /// The team from template `team`, once created.
        fn instance_of(&self, team: &str) -> Option<String> {
            let reg = self.read("state/registry.json")?;
            reg["teams"]
                .as_object()?
                .iter()
                .find(|(_, e)| e["template"] == team)
                .map(|(id, _)| id.clone())
        }

        /// The team made from `dev-team`.
        fn inst(&self) -> String {
            self.instance_of("dev-team")
                .expect("the team made from `dev-team`")
        }

        /// The member of `role` in `team`.
        fn member_in(&self, instance: &str, role: &str) -> String {
            let rec = self
                .read(&format!("state/{instance}/team.json"))
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

        /// Execute fixture tools with the member's captured, fenced environment.
        /// Fixture setup must not depend on typing into an interactive lead.
        fn run_tool(&self, member: &str, path: &Path) {
            let log =
                fs::read_to_string(self.path(&format!("home/fake-agent/{member}.jsonl"))).unwrap();
            let start: serde_json::Value = log
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .rfind(|event| event["event"] == "start")
                .unwrap();
            let env: std::collections::BTreeMap<String, String> =
                serde_json::from_value(start["env"].clone()).unwrap();
            let status = Command::new("/bin/sh")
                .arg(path)
                .env_clear()
                .envs(env)
                .current_dir(start["cwd"].as_str().unwrap())
                .status()
                .unwrap();
            assert!(status.success(), "fixture tool: {status}");
        }

        fn run_in(&self, role: &str, name: &str, script: &str) {
            let path = self.path(&format!("s/{name}.sh"));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, script).unwrap();
            self.wait_idle(role);
            self.run_tool(&self.m(role), &path);
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

        /// Execute a command as one of the member's tool calls; Claude's
        /// wait is a background job, while other commands finish in the
        /// tool call.
        fn tool(&self, role: &str, name: &str, script: &str, background: bool) {
            let out = self.path(&format!("s/{name}.out"));
            let script = format!(
                "( {script}\n) > {o} 2>&1; echo \"exit $?\" >> {o}\n",
                o = out.display()
            );
            let script = if background {
                format!("( {script} ) </dev/null >/dev/null 2>&1 &\n")
            } else {
                script
            };
            self.run_in(role, name, &script);
        }

        /// Create the team made from `dev-team` in `work/`, by name, from the
        /// pinned copy.
        fn create(&self) -> String {
            let folder = self.path("work").display().to_string();
            let out = self.ok(&["create", "dev-team", &folder]);
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

    /// Extract executable examples, not copies of their argv in the test.
    fn documented(text: &str, prefix: &str) -> String {
        for block in text.split("```sh\n").skip(1) {
            let script = block.split("```").next().unwrap().trim();
            if script.starts_with(prefix) {
                return script.to_string();
            }
        }
        text.split('`')
            .skip(1)
            .step_by(2)
            .find(|command| command.starts_with(prefix))
            .unwrap_or_else(|| panic!("no documented command starting {prefix}"))
            .to_string()
    }

    #[test]
    fn one_round_with_fake_agents() {
        let team = new_team("round");
        // Resolved, as brg records paths (macOS: /var is /private/var).
        let repo = team.path("work").canonicalize().unwrap();

        let up = team.create();
        let lead = team.m("lead");
        assert!(
            up.lines()
                .any(|l| l.contains(&lead) && l.ends_with("started")),
            "{up}"
        );
        assert!(
            up.lines()
                .any(|line| line.contains("implementer")
                    && line.contains("on demand (none created)")),
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
        let base = fs::read_to_string(shared().join("includes/common.md")).unwrap();
        let opening = lead_body.lines().find(|l| !l.trim().is_empty()).unwrap();
        assert!(prompt.starts_with(opening.trim()), "{prompt}");
        assert!(prompt.contains(base.lines().next().unwrap()), "{prompt}");
        assert!(!prompt.contains("{{"), "{prompt}");
        assert_eq!(start["env"]["BORGCHART_BIN"], env!("CARGO_BIN_EXE_brg"));
        assert_eq!(start["cwd"], repo.display().to_string());
        let meta = team
            .read(&format!("state/{}/start.json", team.inst()))
            .unwrap();
        assert_eq!(meta["members"][&lead]["interactive"], true);
        let notes = PathBuf::from(start["env"]["BORGCHART_NOTES"].as_str().unwrap());
        assert!(notes.is_dir(), "{notes:?}");

        // A new role must exist before it can receive messages. Bootstrap
        // it once, stop it, then send its first brief before the work start.
        team.ok(&["create", &format!("{}/implementer", team.inst())]);
        team.wait_idle("implementer");
        let implementer = team.m("implementer");
        let copy_folder = team
            .path("state-worktrees")
            .join(format!("{}.{implementer}", team.inst()))
            .canonicalize()
            .unwrap();
        assert!(
            copy_folder.join(".git").is_file(),
            "{}",
            copy_folder.display()
        );
        team.ok(&["stop", &team.at("implementer")]);
        let brief = notes.join("change-brief.md");
        let report = notes.join("change-report.md");
        let brief_tmp = notes.join("change-brief.md.tmp");
        fs::write(&brief_tmp, format!("Task: a fictional change.\nScope: tests only.\nNon-goals: product changes.\nChecks: fake round.\nBranch: example/change.\nCheckout: {}\nReport: {}\nDeadline: 2026-11-09T00:00:00Z\n", copy_folder.display(), report.display())).unwrap();
        fs::rename(brief_tmp, &brief).unwrap();
        // The brief's message is the one the lead's file gives.
        let message = documented(&lead_body, "Read <brief path>")
            .replace("<brief path>", &brief.display().to_string());
        let assign = format!(
            "\"$BORGCHART_BIN\" msg send {implementer} --json - <<'MESSAGE'\n{message}\nMESSAGE"
        );
        team.tool("lead", "brief", &assign, false);
        let (sent, code) = team.output("brief");
        assert_eq!(code, 0, "{sent}");
        assert_eq!(sent["id"], "m1");

        // The lead arms its documented wait before the helper replies.
        let wait = documented(&lead_body, "\"$BORGCHART_BIN\" msg wait --full");
        team.tool(
            "lead",
            "reply-wait",
            &format!("BORGCHART_WAIT_MIN_MS=200 BORGCHART_WAIT_TIMEOUT_MS=5000 {wait} --json"),
            true,
        );
        let start_command = format!("\"$BORGCHART_BIN\" start {}/{implementer}", team.inst());
        team.tool("lead", "start-impl", &start_command, false);
        team.wait_idle("implementer");
        let log = team.log("implementer");
        let start: serde_json::Value = log
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .rfind(|e| e["event"] == "start")
            .unwrap();
        assert_eq!(start["cwd"], copy_folder.display().to_string());
        let argv = start["argv"].to_string();
        assert!(
            argv.starts_with("[\"codex") && argv.contains("gpt-6.1-sol"),
            "{argv}"
        );

        // Its first tool action is a full inbox read.
        let helper = fs::read_to_string(shared().join("includes/helper.md")).unwrap();
        let inbox = "\"$BORGCHART_BIN\" msg list --full --json";
        team.tool("implementer", "impl-inbox", inbox, false);
        let (inbox, code) = team.output("impl-inbox");
        assert_eq!(code, 0, "{inbox}");
        assert_eq!(ids(&inbox), ["m1"]);
        assert!(
            inbox["records"][0]["body"]
                .as_str()
                .unwrap()
                .contains(&brief.display().to_string())
        );
        let ack = "\"$BORGCHART_BIN\" msg ack <id> --json";
        team.tool(
            "implementer",
            "take-brief",
            &ack.replace("<id>", "m1"),
            false,
        );
        assert_eq!(team.output("take-brief").1, 0);

        // Report by sibling rename, then a reply naming it and action-needed.
        team.run_in(
            "implementer",
            "write-report",
            &format!(
                "printf 'status: complete\\naction-needed: none\\n' > {p}.tmp\nmv {p}.tmp {p}\n",
                p = report.display()
            ),
        );
        team.wait_for("the report", || report.exists());
        // The reply's first line is the one helper.md gives.
        let first = documented(&helper, "Report: <report path>")
            .replace("<report path>", &report.display().to_string());
        let reply = format!(
            "\"$BORGCHART_BIN\" msg send {lead} --reply-to m1 --json - <<'MESSAGE'\n{first}\naction-needed: none\nMESSAGE"
        );
        team.tool("implementer", "report-reply", &reply, false);
        let (sent, code) = team.output("report-reply");
        assert_eq!(code, 0, "{sent}");
        assert_eq!(sent["id"], "m2");
        let (woke, code) = team.output("reply-wait");
        assert_eq!((code, ids(&woke)), (0, vec!["m2".to_string()]));
        assert_eq!(woke["records"][0]["reply_to"], serde_json::json!(["m1"]));
        assert!(
            woke["records"][0]["body"]
                .as_str()
                .unwrap()
                .contains(&report.display().to_string())
        );
        assert!(
            woke["records"][0]["body"]
                .as_str()
                .unwrap()
                .contains("action-needed: none")
        );
        assert_eq!(
            fs::read_to_string(report).unwrap(),
            "status: complete\naction-needed: none\n"
        );
        team.tool("lead", "take-report", &ack.replace("<id>", "m2"), false);
        assert_eq!(team.output("take-report").1, 0);
        let stop = format!("\"$BORGCHART_BIN\" stop {}/{implementer}", team.inst());
        team.tool("lead", "stop-impl", &stop, false);
        team.tool(
            "lead",
            "stopped-status",
            "\"$BORGCHART_BIN\" status --json",
            false,
        );
        let (rows, code) = team.output("stopped-status");
        assert_eq!(code, 0, "{rows}");
        let helper = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["member"] == implementer)
            .unwrap();
        assert_eq!(helper["state"], "stopped", "{rows}");
        assert_eq!(helper["unacked"], 0, "{rows}");
        team.ok(&["stop", &team.inst()]);
    }

    /// The department lead seats a design panel as a nested child team with the steps
    /// lead.md gives, from inside the lead: `create` of its
    /// `design` role makes the child team stopped in the lead's own checkout, the
    /// lead checks seating for each designer, starts the child team, exchanges
    /// files in the child team's notes folder, then stops and deletes the child team.
    #[test]
    fn the_lead_seats_the_design_panel_as_a_nested_child_team() {
        let team = new_team("design");
        let repo = team.path("work");
        let baseline = git_in(&repo, &["rev-parse", "HEAD"]);
        team.ok(&[
            "create",
            "engineering",
            &team.path("work").display().to_string(),
        ]);
        let inst = team
            .instance_of("engineering")
            .expect("the department's team");
        let lead = team.member_in(&inst, "lead");
        let rule = member_body(&department().join("lead.md"));
        let lead_idle = || {
            team.wait_for("lead idle", || {
                serde_json::from_slice::<Vec<serde_json::Value>>(
                    &team.brg(&["status", &inst, "--json"]).stdout,
                )
                .unwrap_or_default()
                .iter()
                .any(|r| r["member"] == lead.as_str() && r["state"] == "idle")
            });
        };

        // A command, run inside the lead: its output and exit code.
        let run = |name: &str, script: &str| {
            let out = team.path(&format!("s/{name}.out"));
            let path = team.path(&format!("s/{name}.sh"));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(
                &path,
                format!(
                    "( {script}\n) > {o} 2>&1; echo \"exit $?\" >> {o}\n",
                    o = out.display()
                ),
            )
            .unwrap();
            lead_idle();
            team.run_tool(&lead, &path);
            team.wait_for(&format!("{name} output"), || {
                fs::read_to_string(&out).is_ok_and(|t| t.contains("exit "))
            });
            let text = fs::read_to_string(&out).unwrap();
            let (text, code) = text.rsplit_once("exit ").unwrap();
            (text.to_string(), code.trim().parse::<i32>().unwrap())
        };

        let (up, code) = run(
            "panel-create",
            "\"$BORGCHART_BIN\" create \"$BORGCHART_TEAM/design\"",
        );
        assert_eq!(code, 0, "{up}");
        let panel = up.split_whitespace().next().unwrap().to_string();
        let copy = panel
            .strip_prefix(&format!("{inst}/"))
            .unwrap_or_else(|| panic!("{up}"))
            .to_string();
        assert!(copy.starts_with("design-"), "{up}");
        // The panel reads the lead's checkout: no folder of its own.
        let folder = repo.canonicalize().unwrap();
        assert!(up.contains(&folder.display().to_string()), "{up}");
        let rows = |instance: &str| -> Vec<serde_json::Value> {
            serde_json::from_slice(&team.brg(&["status", instance, "--json"]).stdout)
                .unwrap_or_default()
        };
        assert!(
            rows(&panel).iter().all(|r| r["member"].is_null()),
            "nothing starts at create"
        );

        let fill = |command: String| {
            command
                .replace("<folder>", &team.path("work").display().to_string())
                .replace("<child team folder>", &folder.display().to_string())
                .replace("<child-team-id>", &panel)
                .replace("<child-team>", &copy)
                .replace("<baseline>", &baseline)
        };
        let work = folder.clone();
        assert_eq!(git_in(&work, &["rev-parse", "HEAD"]), baseline);

        // Seating: each designer's vendor and model, from the child team's status,
        // checked through the granted seating plugin and the isolated fake vendor protocols.
        let (listed, code) = run(
            "panel-roles",
            &fill(documented(
                &rule,
                "\"$BORGCHART_BIN\" status <child-team-id> --json",
            )),
        );
        assert_eq!(code, 0, "{listed}");
        let listed: Vec<serde_json::Value> = serde_json::from_str(&listed).unwrap();
        for designer in ["designer-claude", "designer-codex"] {
            let row = listed
                .iter()
                .find(|r| r["role"] == designer)
                .unwrap_or_else(|| panic!("{listed:?}"));
            let vendor = row["vendor"].as_str().unwrap();
            let model = row["model"].as_str().unwrap();
            let route = documented(&rule, "brg-seat route <vendor> <model>")
                .replace("<vendor>", vendor)
                .replace("<model>", model);
            let (answer, code) = run(&format!("seat-{designer}"), &route);
            assert_eq!((code, answer.trim()), (0, "go: included quota available"));
            let events =
                fs::read_to_string(team.path(&format!("home/fake-usage/{vendor}.jsonl"))).unwrap();
            assert!(
                events
                    .lines()
                    .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                    .any(|event| event["event"] == "request")
            );
        }

        let (started, code) = run("panel-start", &format!("\"$BORGCHART_BIN\" start {panel}"));
        assert_eq!(code, 0, "{started}");
        let work = work.canonicalize().unwrap();
        let mut notes = None;
        for designer in ["designer-claude", "designer-codex"] {
            team.wait_for(designer, || {
                rows(&panel)
                    .iter()
                    .any(|r| r["role"] == designer && r["state"] == "idle")
            });
            let member = team.member_in(&panel, designer);
            let log = team.path(&format!("home/fake-agent/{member}.jsonl"));
            let start: serde_json::Value =
                serde_json::from_str(fs::read_to_string(&log).unwrap().lines().next().unwrap())
                    .unwrap();
            assert!(start["argv"].to_string().contains("You are a designer"));
            assert_eq!(start["cwd"], work.display().to_string());
            assert_eq!(start["env"]["BORGCHART_TEAM"], panel.as_str());
            assert_eq!(start["env"]["BORGCHART_PARENT"], format!("{inst}/{lead}"));
            notes = Some(PathBuf::from(
                start["env"]["BORGCHART_NOTES"].as_str().unwrap(),
            ));
        }
        let notes = notes.unwrap();

        // The design directory, as the lead works it out, is the child team's
        // notes folder.
        let (dir, code) = run(
            "panel-notes",
            &format!(
                "echo {}",
                fill(documented(&rule, "\"$(dirname \"$BORGCHART_NOTES\")/"))
            ),
        );
        assert_eq!(code, 0, "{dir}");
        assert_eq!(
            Path::new(dir.trim()).canonicalize().unwrap(),
            notes.canonicalize().unwrap()
        );

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
        for designer in ["designer-claude", "designer-codex"] {
            let member = team.member_in(&panel, designer);
            let script = team.path(&format!("s/{designer}.sh"));
            fs::write(
                &script,
                "for f in questions proposal; do echo none > \"$BORGCHART_NOTES/.tmp-$f\"; mv \"$BORGCHART_NOTES/.tmp-$f\" \"$BORGCHART_NOTES/$f-$BORGCHART_ROLE.md\"; done\n",
            )
            .unwrap();
            team.run_tool(&member, &script);
        }
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

        for (name, command) in [("panel-stop", "stop"), ("panel-delete", "delete")] {
            let (out, code) = run(name, &format!("\"$BORGCHART_BIN\" {command} {panel}"));
            assert_eq!(code, 0, "{name}: {out}");
            if name == "panel-stop" {
                for row in rows(&panel) {
                    assert_eq!(row["state"], "stopped", "{row}");
                }
            }
        }
        assert!(work.join(".git").is_dir(), "the lead's checkout stays");
        let registry = team.read("state/registry.json").unwrap();
        assert!(registry["teams"].get(&panel).is_none(), "{registry}");
        // The department still runs.
        lead_idle();
        team.ok(&["stop", &inst]);
    }

    /// The fake agent's first record of a start, once it is fully written:
    /// the log can exist before its first line is complete.
    fn first_start(team: &Team, what: &str, log: &Path) -> serde_json::Value {
        let first = || {
            let text = fs::read_to_string(log).ok()?;
            serde_json::from_str::<serde_json::Value>(text.lines().next()?).ok()
        };
        team.wait_for(what, || first().is_some());
        first().unwrap()
    }

    /// The opening prompt of `member` in `team`: its expanded role file.
    fn prompt_of(team: &Team, instance: &str, member: &str) -> String {
        let log = team.path(&format!("home/fake-agent/{member}.jsonl"));
        let start = first_start(team, &format!("{member} started"), &log);
        assert_eq!(start["env"]["BORGCHART_TEAM"], instance);
        start["argv"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .as_str()
            .unwrap()
            .to_string()
    }

    /// The department's own steps, run from inside its lead as lead.md
    /// gives them: `create` of `dev` makes a stopped child team, the
    /// child team gets its own worktree on a new branch, the lead checks seating
    /// and starts it, and the dev lead's parent is the department lead.
    /// Every launched member gets the common rules; only the dev lead gets
    /// the dev-only ones.
    #[test]
    fn the_parent_team_lead_starts_a_dev_child_team_with_its_documented_steps() {
        let team = new_team("dept");
        let checkout = team.path("work");

        team.ok(&[
            "create",
            &team
                .path("pinned/templates/engineering")
                .display()
                .to_string(),
            &team.path("work").display().to_string(),
        ]);
        let dept = team
            .instance_of("engineering")
            .expect("the department's team");
        let dept_lead = team.member_in(&dept, "lead");
        let meta = team.read(&format!("state/{dept}/start.json")).unwrap();
        assert_eq!(meta["members"][&dept_lead]["interactive"], true);
        let log = team.path(&format!("home/fake-agent/{dept_lead}.jsonl"));
        let start = first_start(&team, "the department lead", &log);
        assert_eq!(
            start["cwd"],
            checkout.canonicalize().unwrap().display().to_string()
        );
        let rows = |instance: &str| -> Vec<serde_json::Value> {
            serde_json::from_slice(&team.brg(&["status", instance, "--json"]).stdout)
                .unwrap_or_default()
        };
        let idle = |instance: &str, member: &str| {
            team.wait_for(&format!("{member} idle"), || {
                rows(instance)
                    .iter()
                    .any(|r| r["member"] == member && r["state"] == "idle")
            });
        };
        let rule = member_body(&department().join("lead.md"));
        // A command, run inside the department lead.
        let run_in = |instance: &str, member: &str, name: &str, script: &str| {
            let out = team.path(&format!("s/{name}.out"));
            let path = team.path(&format!("s/{name}.sh"));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(
                &path,
                format!(
                    "( {script}\n) > {o} 2>&1; echo \"exit $?\" >> {o}\n",
                    o = out.display()
                ),
            )
            .unwrap();
            idle(instance, member);
            team.run_tool(member, &path);
            team.wait_for(&format!("{name} output"), || {
                fs::read_to_string(&out).is_ok_and(|t| t.contains("exit "))
            });
            let text = fs::read_to_string(&out).unwrap();
            let (text, code) = text.rsplit_once("exit ").unwrap();
            (text.to_string(), code.trim().parse::<i32>().unwrap())
        };

        let run = |name: &str, script: &str| run_in(&dept, &dept_lead, name, script);

        let (up, code) = run(
            "dev-create",
            "\"$BORGCHART_BIN\" create \"$BORGCHART_TEAM/dev\" --branch example/work",
        );
        assert_eq!(code, 0, "{up}");
        let copy_id = up.split_whitespace().next().unwrap().to_string();
        let copy = copy_id
            .strip_prefix(&format!("{dept}/"))
            .unwrap_or_else(|| panic!("{up}"))
            .to_string();
        assert!(copy.starts_with("dev-"), "{up}");
        let folder = team.path("state-worktrees").join(copy_id.replace('/', "."));
        assert!(up.contains(&folder.display().to_string()), "{up}");
        assert_eq!(
            git_in(&folder, &["branch", "--show-current"]),
            "example/work"
        );
        assert!(
            rows(&copy_id).iter().all(|r| r["member"].is_null()),
            "nothing starts at create"
        );

        let fill = |command: String| {
            command
                .replace("<folder>", &team.path("work").display().to_string())
                .replace("<child team folder>", &folder.display().to_string())
                .replace("<child-team-id>", &copy_id)
                .replace("<child-team>", &copy)
                .replace("<branch>", "example/work")
        };
        let lead_row = rows(&copy_id)
            .into_iter()
            .find(|r| r["role"] == "lead")
            .expect("the child team's lead role");
        let route = documented(&rule, "brg-seat route <vendor> <model>")
            .replace("<vendor>", lead_row["vendor"].as_str().unwrap())
            .replace("<model>", lead_row["model"].as_str().unwrap());
        let (answer, code) = run("dev-seat", &route);
        assert_eq!((code, answer.trim()), (0, "go: included quota available"));

        let (started, code) = run("dev-start", &format!("\"$BORGCHART_BIN\" start {copy_id}"));
        assert_eq!(code, 0, "{started}");
        let dev_lead = team.member_in(&copy_id, "lead");
        let child_meta = team.read(&format!("state/{copy_id}/start.json")).unwrap();
        let parent_shim = team.path(&format!("state/{dept}/members/{dept_lead}/bin"));
        assert!(
            std::env::split_paths(child_meta["base_env"]["PATH"].as_str().unwrap())
                .all(|entry| entry != parent_shim),
            "child captured its parent's identity-fenced plugin shims"
        );
        let dev_rule = member_body(&dev_team().join("orchestrator.md"));
        let nested_route = documented(&dev_rule, "brg-seat route <vendor> <model>")
            .replace("<vendor>", "codex")
            .replace("<model>", "astra");
        let (answer, code) = run_in(&copy_id, &dev_lead, "nested-dev-seat", &nested_route);
        assert_eq!((code, answer.trim()), (0, "go: included quota available"));

        let log = team.path(&format!("home/fake-agent/{dev_lead}.jsonl"));
        let start = first_start(&team, "the dev lead", &log);
        assert_eq!(
            start["cwd"],
            folder.canonicalize().unwrap().display().to_string()
        );
        assert_eq!(
            start["env"]["BORGCHART_PARENT"],
            format!("{dept}/{dept_lead}")
        );
        let (notes, code) = run(
            "dev-notes",
            &format!(
                "echo {}",
                fill(documented(&rule, "\"$(dirname \"$BORGCHART_NOTES\")/"))
            ),
        );
        assert_eq!(code, 0, "{notes}");
        assert_eq!(
            Path::new(notes.trim()).canonicalize().unwrap(),
            Path::new(start["env"]["BORGCHART_NOTES"].as_str().unwrap())
                .canonicalize()
                .unwrap()
        );

        // Researchers and monitors are ordinary members; `create` starts one.
        for role in ["researcher", "monitor"] {
            let (made, code) = run(
                &format!("{role}-create"),
                &format!("\"$BORGCHART_BIN\" create \"$BORGCHART_TEAM/{role}\""),
            );
            assert_eq!(code, 0, "{made}");
        }

        let common = fs::read_to_string(shared().join("includes/common.md")).unwrap();
        let dev = fs::read_to_string(shared().join("includes/dev.md")).unwrap();
        let dev_rules: Vec<&str> = dev.split("\n\n").map(str::trim).collect();
        for (instance, role, dev_only) in [
            (&dept, "lead", false),
            (&dept, "researcher", false),
            (&dept, "monitor", false),
            (&copy_id, "lead", true),
        ] {
            let member = team.member_in(instance, role);
            let prompt = prompt_of(&team, instance, &member);
            assert!(
                prompt.contains(common.trim()),
                "{instance} {role}: {prompt}"
            );
            for paragraph in &dev_rules {
                assert_eq!(
                    prompt.contains(paragraph),
                    dev_only,
                    "{instance} {role}: {paragraph}"
                );
            }
            assert!(!prompt.contains("{{"), "{instance} {role}: {prompt}");
        }
        team.ok(&["stop", &dept]);
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

    /// A stopped or replaced member stops re-arming the one wait.
    #[test]
    fn fenced_waits_stop_the_loop_for_replaced_or_stopped_members() {
        let rule = fs::read_to_string(dev_team().join("orchestrator.md")).unwrap();
        let wake = rule
            .split("## Watch")
            .nth(1)
            .unwrap()
            .split("## When your team has a parent")
            .next()
            .unwrap();
        assert!(wake.contains("msg wait --full"), "{wake}");
        assert!(wake.contains("stop waiting"), "{wake}");

        let team = new_team("fenced");
        team.create();
        let inst = team.inst();
        let lead = team.m("lead");
        team.ok(&[
            "msg",
            "send",
            "--external",
            "owner",
            &format!("{inst}/{lead}"),
            "Fictional note.",
        ]);
        assert_eq!(
            json(&as_lead(&team, &["msg", "list", "--json"])).0["records"][0]["id"],
            "m1"
        );

        // The lead is stopped while a wait of its session runs.
        let waiting = {
            let mut cmd = lead_cmd(&team);
            cmd.env("BORGCHART_WAIT_MIN_MS", "1000")
                .env("BORGCHART_WAIT_TIMEOUT_MS", "3000");
            cmd.args(["msg", "wait", "--timeout", "60", "--json"]);
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
        let (relist, code) = json(&as_lead(&team, &["msg", "list", "--json"]));
        assert_eq!(code, Some(8), "{relist}");
        assert!(
            !relist["message"]
                .as_str()
                .unwrap()
                .contains("list the inbox again")
        );
    }
}

#[test]
fn pinned_sibling_catalog_validates_without_the_source_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let config = setup(dir.path(), &readme_configs()[0]);
    for name in ["engineering", "dev-team", "dev-design-team"] {
        let output = validate(dir.path(), &config, name);
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let found = borgchart::team::list(&[dir.path().join("pinned/templates")]);
    assert_eq!(found.len(), 3);
    assert!(!found.iter().any(|(name, _)| name == "shared"));
}
