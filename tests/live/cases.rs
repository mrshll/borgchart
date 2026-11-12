//! The cases that start real members through real `brg create`, each in a
//! fresh team and folder. Each returns its outcome; none answers a tool approval,
//! and the only screen brg answers is folder trust, through `auto_trust`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use borgchart::rules::State;

use super::rules::{CHECK_STATUS_TRUST_DETAIL, Outcome, RUN_CLAUDE_TRUST_ON, Who};
use super::world::{self, World, limited, limited_with_stdin};

const POLL: Duration = Duration::from_millis(300);
/// Between looks while a trust screen must stay.
const HOLD_POLL: Duration = Duration::from_millis(100);
/// The pause between looks after the nonce is sent (each look takes a few
/// ms more); the result reports the gaps seen.
const FINE_POLL: Duration = Duration::from_millis(15);

/// The Claude background-wait member (the fake keys its behaviour on it).
pub const WAITER: &str = "waiter";

/// Commands the background-wait member may run, as session-only
/// `--allowedTools` entries in its role's `args`: the wait and the
/// read of its own inbox. Nothing is saved to settings.
pub const WAITER_TOOLS: &str = "Bash(brg msg wait:*),Bash(brg msg list --full:*)";

const READY: &str = "This is an automated test. Reply with the single word READY. Use no tools.";

const WAITER_PROMPT: &str = "This is an automated test of background commands. Follow these steps exactly, and use no other tools.

1. Run this with your Bash tool, with run_in_background set to true: `brg msg wait --timeout 900`
2. End your turn right away with the one word ARMED. Don't wait for, poll or check on the background command.
3. When that background command has finished, run `brg msg list --full` with your Bash tool. One record's body is `nonce <word>`.
4. Reply with one line: REPLY, a space, and that word written backwards, letter by letter. Then stop.";

/// One case's team, started with `brg create`. Dropping it runs `brg stop`.
pub struct Team<'w> {
    w: &'w World,
    rules: borgchart::rules::RuleSet,
    /// The team folder.
    pub dir: PathBuf,
    /// brg's configuration for this case (`auto_trust`).
    pub config: PathBuf,
    /// The instance's working folder.
    cwd: PathBuf,
    /// The role; its member's ID is learned once `create` registers it.
    role: String,
    ids: std::sync::OnceLock<(String, String)>,
}

pub struct Spec<'a> {
    pub who: Who,
    pub member: &'a str,
    pub locked: bool,
    pub cwd: &'a Path,
    pub auto_trust: bool,
    pub args: &'a [&'a str],
    pub instruction: &'a str,
    pub bus: bool,
}

impl<'w> Team<'w> {
    pub fn new(w: &'w World, case: &str, s: &Spec) -> Team<'w> {
        let dir = w.scratch.join(w.fresh(case));
        std::fs::create_dir_all(&dir).unwrap();
        let args = serde_json::to_string(s.args).unwrap();
        std::fs::write(
            dir.join("template.yaml"),
            format!(
                "{bus}roles:\n  {m}:\n    prompt: {m}.md\n    vendor: {v}\n    args: {args}\n{posture}",
                bus = if s.bus { "bus: {}\n" } else { "" },
                m = s.member,
                v = s.who.as_str(),
                posture = if s.locked {
                    "    posture: locked\n"
                } else {
                    ""
                },
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join(format!("{}.md", s.member)),
            format!("---\n{{}}\n---\n{}\n", s.instruction),
        )
        .unwrap();
        let dir = dir.canonicalize().unwrap();
        let config = dir.with_extension("yaml");
        std::fs::write(
            &config,
            format!("schema: 1\ntemplates: []\nauto_trust: {}\n", s.auto_trust),
        )
        .unwrap();
        let vendor = match s.who {
            Who::Claude => borgchart::team::Vendor::Claude,
            Who::Codex => borgchart::team::Vendor::Codex,
        };
        Team {
            w,
            rules: borgchart::rules::load(vendor).unwrap(),
            dir,
            config,
            cwd: s.cwd.to_path_buf(),
            role: s.member.to_string(),
            ids: std::sync::OnceLock::new(),
        }
    }

    /// The instance and member, from brg's registry once `create` has
    /// registered them (it does so before starting anything).
    fn ids(&self) -> Option<(String, String)> {
        if let Some(ids) = self.ids.get() {
            return Some(ids.clone());
        }
        let state = PathBuf::from(self.w.env.get("BORGCHART_STATE_HOME")?);
        let read =
            |p: PathBuf| -> Option<Value> { serde_json::from_slice(&std::fs::read(p).ok()?).ok() };
        let registry = read(state.join("registry.json"))?;
        let dir = self.dir.display().to_string();
        let instance = registry["teams"]
            .as_object()?
            .iter()
            .find(|(_, e)| e["template_dir"] == dir.as_str())
            .map(|(id, _)| id.clone())?;
        let record = read(state.join(&instance).join("team.json"))?;
        let member = record["members"]
            .as_object()?
            .iter()
            .find(|(_, m)| m["role"] == self.role.as_str())
            .map(|(id, _)| id.clone())?;
        let _ = self.ids.set((instance, member));
        self.ids.get().cloned()
    }

    /// The instance's ID (empty until it is registered).
    pub fn instance(&self) -> String {
        self.ids().map(|i| i.0).unwrap_or_default()
    }

    /// The member's ID (empty until it is registered).
    pub fn member(&self) -> String {
        self.ids().map(|i| i.1).unwrap_or_default()
    }

    pub fn session(&self) -> String {
        format!("brg-{}", self.instance())
    }

    /// `brg` with the world's environment and this case's configuration.
    pub fn brg_command(&self) -> std::process::Command {
        let mut cmd = self.w.command(&self.w.brg);
        cmd.env("BORGCHART_CONFIG", &self.config);
        cmd
    }

    pub fn brg(&self, args: &[&str], limit: Duration) -> Result<(bool, String), String> {
        let mut cmd = self.brg_command();
        cmd.args(args);
        let out = limited(&mut cmd, limit)?;
        Ok((
            out.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        ))
    }

    pub fn up(&self) -> Result<(bool, String), String> {
        let dir = self.dir.display().to_string();
        let cwd = self.cwd.display().to_string();
        self.brg(&["create", &dir, &cwd], self.w.limits.up)
            .map_err(|e| format!("`brg create` {e}"))
    }

    /// The member's `brg status --json` row.
    pub fn row(&self) -> Value {
        let instance = self.instance();
        if instance.is_empty() {
            return Value::Null;
        }
        let Ok((true, text)) = self.brg(&["status", &instance, "--json"], self.w.limits.brg) else {
            return Value::Null;
        };
        // stdout and stderr are joined; the JSON is the first line that parses.
        let rows: Value = text
            .lines()
            .find_map(|l| serde_json::from_str(l).ok())
            .or_else(|| serde_json::from_str(&text).ok())
            .unwrap_or(Value::Null);
        rows.as_array()
            .into_iter()
            .flatten()
            .find(|r| r["role"] == self.role.as_str() && r["kind"] == "member")
            .cloned()
            .unwrap_or(Value::Null)
    }

    pub fn state(&self) -> (String, String) {
        let r = self.row();
        (
            r["state"].as_str().unwrap_or("").to_string(),
            r["rule"].as_str().unwrap_or("").to_string(),
        )
    }

    fn target(&self) -> String {
        format!("={}:={}", self.session(), self.member())
    }

    /// The member's screen, styled (`capture-pane -e`).
    pub fn screen(&self) -> String {
        self.w
            .tmux(&["capture-pane", "-p", "-e", "-t", &self.target()])
    }

    pub fn pane_pid(&self) -> Option<u32> {
        self.w
            .tmux(&["display-message", "-p", "-t", &self.target(), "#{pane_pid}"])
            .trim()
            .parse()
            .ok()
    }

    pub fn save(&self, name: &str) {
        let text = self.screen();
        if !text.is_empty() {
            let _ = std::fs::write(
                self.w.evidence.join("screens").join(format!("{name}.ansi")),
                text,
            );
        }
    }

    /// Wait for one of: idle, a prompt (blocked), or the limit.
    pub fn settle(&self) -> (String, String) {
        let end = Instant::now() + self.w.limits.start;
        loop {
            let (state, rule) = self.state();
            if state == "idle" || state == "blocked" || Instant::now() > end {
                return (state, rule);
            }
            std::thread::sleep(POLL);
        }
    }
}

/// One observation of the member's pane, from a single tmux call.
pub struct Look {
    /// The window or pane is gone.
    pub gone: bool,
    /// The member exited (the pane stays, dead, showing its last screen).
    pub dead: bool,
    pub title: String,
    pub screen: String,
}

impl Team<'_> {
    pub fn look(&self) -> Look {
        let t = self.target();
        let out = self.w.tmux(&[
            "display-message",
            "-p",
            "-t",
            &t,
            "#{pane_dead}\t#{pane_title}",
            ";",
            "capture-pane",
            "-p",
            "-t",
            &t,
        ]);
        let Some((head, screen)) = out.split_once('\n') else {
            return Look {
                gone: true,
                dead: false,
                title: String::new(),
                screen: String::new(),
            };
        };
        let (dead, title) = head.split_once('\t').unwrap_or((head, ""));
        Look {
            gone: false,
            dead: dead == "1",
            title: title.to_string(),
            screen: screen.to_string(),
        }
    }

    /// The state rules `status` uses, on one look.
    pub fn read(&self, look: &Look) -> (State, String) {
        let r = self.rules.read(&look.title, &look.screen);
        (r.state, r.rule.unwrap_or_default())
    }

    /// The trust screen, kept: on every poll through the hold and on the
    /// final look, with the member's pane alive. `Err` says how it left.
    pub fn hold_trust(&self, at_trust: impl Fn(&Self, &Look) -> bool) -> Result<u32, String> {
        let end = Instant::now() + self.w.limits.hold;
        let mut polls = 0;
        loop {
            let last = Instant::now() >= end;
            let look = self.look();
            polls += 1;
            if look.gone {
                return Err(format!("its window was gone at poll {polls}"));
            }
            if look.dead {
                self.save(&format!("{}-left-trust", self.role));
                return Err(format!("the member exited at poll {polls}"));
            }
            if !at_trust(self, &look) {
                self.save(&format!("{}-left-trust", self.role));
                return Err(format!("another screen at poll {polls}"));
            }
            if last {
                return Ok(polls);
            }
            std::thread::sleep(HOLD_POLL);
        }
    }
}

impl Drop for Team<'_> {
    fn drop(&mut self) {
        let instance = self.instance();
        if !instance.is_empty() {
            let _ = self.brg(&["stop", &instance], self.w.limits.brg);
            let _ = self
                .w
                .tmux(&["kill-session", "-t", &format!("={}", self.session())]);
        }
    }
}

fn fresh_dir(parent: &Path, w: &World, what: &str) -> PathBuf {
    let d = parent.join(w.fresh(what));
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

/// A fresh Git repository under the temp directory: no trusted parent.
fn fresh_repo(w: &World, what: &str) -> PathBuf {
    let d = fresh_dir(&w.scratch, w, what);
    let mut git = w.command("git");
    git.args(["-c", "init.defaultBranch=main", "init", "-q"])
        .current_dir(&d)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    let _ = limited(&mut git, w.limits.brg);
    d
}

fn codex_trust_screen(screen: &str) -> bool {
    let s = screen.to_lowercase();
    [
        "trust this folder",
        "trust and continue",
        "trust this directory",
        "trust the files in this",
    ]
    .iter()
    .any(|t| s.contains(t))
}

/// A member stopped at a prompt: folder trust in a folder that should be
/// trusted, or anything else (a tool approval). Nothing is answered.
fn blocked_at(rule: &str, folder: &Path) -> Outcome {
    if rule == "trust-dialog" {
        return Outcome::Blocked(format!(
            "Claude doesn't trust the work folder {}; run from a checkout inside a folder you trust",
            folder.display()
        ));
    }
    Outcome::Blocked(format!("stopped at a prompt (`{rule}`); left unanswered"))
}

/// While the member waits at folder trust: `brg status` says so, with a
/// `brg attach` hint (`*-trust-status`).
fn status_trust(t: &Team) -> Outcome {
    if !CHECK_STATUS_TRUST_DETAIL {
        return Outcome::Skip("status trust detail: pending #54".into());
    }
    let r = t.row();
    let text = format!("{} {}", r["state"], r["detail"]);
    if text.to_lowercase().contains("trust") && text.contains("brg attach") {
        Outcome::Pass(format!("status: {text}"))
    } else {
        Outcome::Fail(format!("status doesn't report the trust wait: {text}"))
    }
}

/// The `*-trust-status` result when the case never reached the screen.
fn status_unreached() -> Outcome {
    if CHECK_STATUS_TRUST_DETAIL {
        Outcome::Blocked("the case didn't reach the trust screen".into())
    } else {
        Outcome::Skip("status trust detail: pending #54".into())
    }
}

/// `claude-trust-off` with `claude-trust-status`, or `claude-trust-on`.
pub fn claude_trust(w: &World, on: bool) -> Vec<(String, Outcome)> {
    let id = if on {
        "claude-trust-on"
    } else {
        "claude-trust-off"
    };
    let mut status = status_unreached();
    let main = claude_trust_case(w, on, id, &mut status);
    let mut out = vec![(id.to_string(), main)];
    if !on {
        out.push(("claude-trust-status".into(), status));
    }
    out
}

fn claude_trust_case(w: &World, on: bool, id: &str, status: &mut Outcome) -> Outcome {
    if on && !RUN_CLAUDE_TRUST_ON {
        return Outcome::Skip("would save trust to your Claude config".into());
    }
    let folder = fresh_dir(&w.scratch, w, "untrusted");
    let t = Team::new(
        w,
        id,
        &Spec {
            who: Who::Claude,
            member: "truster",
            locked: false,
            cwd: &folder,
            auto_trust: on,
            args: &[],
            instruction: READY,
            bus: false,
        },
    );
    let (_, out) = match t.up() {
        Ok(o) => o,
        Err(e) => return Outcome::Fail(e),
    };
    if on {
        let (state, rule) = t.settle();
        t.save(id);
        return match state.as_str() {
            "idle" if out.contains("accepted folder trust for") => {
                Outcome::Pass("brg answered the trust dialog; idle".into())
            }
            "blocked" => blocked_at(&rule, &folder),
            _ => Outcome::Fail(format!("state `{state}` after up: {}", out.trim())),
        };
    }
    // brg leaves the dialog alone with `auto_trust: false`; status reads it.
    let (state, rule) = t.settle();
    if state != "blocked" || rule != "trust-dialog" {
        t.save(id);
        if state == "idle" || state == "working" {
            return Outcome::Blocked(format!(
                "no trust dialog in {}: the folder is already trusted",
                folder.display()
            ));
        }
        return Outcome::Fail(format!(
            "no trust dialog (state `{state}` {rule}); up said: {}",
            out.trim()
        ));
    }
    *status = status_trust(&t);
    t.save(id);
    let held = t.hold_trust(|t, look| t.read(look).1 == "trust-dialog");
    let row = t.row();
    match held {
        Err(e) => Outcome::Fail(format!("left the trust screen: {e}")),
        Ok(_) if !row["trust_accepted"].is_null() => {
            Outcome::Fail(format!("trust was accepted: {}", row["trust_accepted"]))
        }
        Ok(polls) => Outcome::Pass(format!(
            "stopped at the trust dialog, kept unanswered on all {polls} looks over {} s",
            w.limits.hold.as_secs()
        )),
    }
}

/// `trust`: `auto_trust` for a `vendor` member, or `None` for a `locked`
/// member with `auto_trust: true`.
pub fn codex_trust(w: &World, trust: Option<bool>) -> Vec<(String, Outcome)> {
    let id = match trust {
        Some(true) => "codex-trust-on",
        Some(false) => "codex-trust-off",
        None => "codex-locked-trust",
    };
    let mut status = status_unreached();
    let main = codex_trust_case(w, trust, id, &mut status);
    let mut out = vec![(id.to_string(), main)];
    if trust == Some(false) {
        out.push(("codex-trust-status".into(), status));
    }
    out
}

fn codex_trust_case(w: &World, trust: Option<bool>, id: &str, status: &mut Outcome) -> Outcome {
    let repo = fresh_repo(w, "repo");
    let before = w.codex_config_hash();
    let t = Team::new(
        w,
        id,
        &Spec {
            who: Who::Codex,
            member: "truster",
            locked: trust.is_none(),
            cwd: &repo,
            auto_trust: trust != Some(false),
            args: &[],
            instruction: READY,
            bus: false,
        },
    );
    enum Seen {
        Trust,
        Idle,
        Blocked(String),
        Nothing(String),
    }
    let (seen, held, up) = std::thread::scope(|s| {
        let up = s.spawn(|| t.up());
        // While `up` runs (a locked member keeps it waiting for its gate):
        // the first look that shows the trust screen, idle or a prompt.
        let end = Instant::now() + w.limits.start;
        let seen = loop {
            let look = t.look();
            let (state, rule) = t.read(&look);
            if !look.gone && !look.dead && codex_trust_screen(&look.screen) {
                break Seen::Trust;
            }
            match state {
                State::Idle => break Seen::Idle,
                State::Blocked => break Seen::Blocked(rule),
                _ if Instant::now() > end => break Seen::Nothing(format!("{state:?}")),
                _ => {}
            }
            std::thread::sleep(HOLD_POLL);
        };
        t.save(id);
        // Then the screen must stay, through the whole hold. For a locked
        // member this runs before brg's gate deadline stops its window.
        let held = matches!(seen, Seen::Trust).then(|| {
            if trust == Some(false) {
                *status = status_trust(&t);
            }
            t.hold_trust(|_, look| codex_trust_screen(&look.screen))
        });
        (seen, held, up.join().unwrap())
    });
    // A `locked` member is left at the screen once `up` is done: `up`
    // succeeds and says so, and the screen is still there afterwards.
    let left_waiting = || -> Result<(), String> {
        match &up {
            Ok((true, text)) if text.contains("waiting at Codex's folder-trust prompt") => {}
            Ok((_, text)) => return Err(format!("`up` didn't leave it waiting: {}", text.trim())),
            Err(e) => return Err(e.clone()),
        }
        let look = t.look();
        if look.gone || look.dead || !codex_trust_screen(&look.screen) {
            return Err("the trust screen was gone after `up`".into());
        }
        Ok(())
    };
    let waiting = match (trust, &seen) {
        (None, Seen::Trust) => left_waiting(),
        _ => Ok(()),
    };
    let outcome = match (trust, seen, held) {
        (_, Seen::Blocked(rule), _) => blocked_at(&rule, &repo),
        (_, _, _) if up.is_err() => Outcome::Fail(up.clone().unwrap_err()),
        (None, Seen::Trust, Some(Ok(_))) if waiting.is_err() => {
            Outcome::Fail(waiting.clone().unwrap_err())
        }
        (Some(true), Seen::Idle, _) => {
            Outcome::Pass("reached its prompt with session-only trust".into())
        }
        (Some(true), Seen::Trust, _) => {
            Outcome::Fail("stopped at the trust screen despite the session trust".into())
        }
        (_, Seen::Trust, Some(Ok(polls))) => Outcome::Pass(format!(
            "stopped at the trust screen, kept unanswered on all {polls} looks over {} s",
            w.limits.hold.as_secs()
        )),
        (_, Seen::Trust, Some(Err(e))) => Outcome::Fail(format!("left the trust screen: {e}")),
        (_, Seen::Idle, _) => Outcome::Blocked(format!(
            "no trust screen in {}: the folder is already trusted",
            repo.display()
        )),
        (_, Seen::Nothing(state), _) => Outcome::Fail(format!(
            "neither its prompt nor a trust screen (state {state}); up said: {}",
            up.map(|(_, o)| o).unwrap_or_default().trim()
        )),
        (_, Seen::Trust, None) => unreachable!(),
    };
    // The config is hashed again only after the team is down, so a write
    // at shutdown counts too.
    drop(t);
    let after = w.codex_config_hash();
    if before != after {
        return Outcome::Fail(format!(
            "the Codex config.toml changed during the case (sha256 {before} → {after}); not restored"
        ));
    }
    match outcome {
        Outcome::Pass(d) => Outcome::Pass(format!("{d}; config.toml unchanged after teardown")),
        o => o,
    }
}

pub fn env_colour(w: &World, who: Who) -> Outcome {
    let id = format!("{}-env-colour", who.as_str());
    if !world::has_proc() {
        return Outcome::Skip("reads the vendor's environment from /proc (Linux only)".into());
    }
    let folder = fresh_dir(&w.trusted, w, "env");
    let t = Team::new(
        w,
        &id,
        &Spec {
            who,
            member: "painter",
            locked: false,
            cwd: &folder,
            auto_trust: who == Who::Codex,
            args: &[],
            instruction: READY,
            bus: false,
        },
    );
    if let Err(e) = t.up() {
        return Outcome::Fail(e);
    }
    let (state, rule) = t.settle();
    t.save(&id);
    if state == "blocked" {
        return blocked_at(&rule, &folder);
    }
    if state != "idle" {
        return Outcome::Fail(format!("state `{state}` after the start limit"));
    }
    let Some(pane) = t.pane_pid() else {
        return Outcome::Fail("no pane".into());
    };
    let procs = world::vendor_procs(pane, who);
    if procs.is_empty() {
        return Outcome::Fail(format!("no {} process under pane {pane}", who.as_str()));
    }
    let term = w.tmux(&["show-options", "-gv", "default-terminal"]);
    let term = term.trim();
    for p in &procs {
        let env = world::environ(p.pid);
        let (t_, c) = (
            env.get("TERM").map(String::as_str).unwrap_or("<unset>"),
            env.get("COLORTERM")
                .map(String::as_str)
                .unwrap_or("<unset>"),
        );
        if t_ != term || c != "truecolor" {
            return Outcome::Fail(format!(
                "pid {}: TERM={t_} COLORTERM={c}; want TERM={term} COLORTERM=truecolor",
                p.pid
            ));
        }
    }
    let rgb = regex::Regex::new(r"\x1b\[[0-9;]*[34]8;2;").unwrap();
    if rgb.is_match(&t.screen()) {
        Outcome::Pass(format!(
            "TERM={term} COLORTERM=truecolor in {} process(es); RGB colour on screen",
            procs.len()
        ))
    } else {
        Outcome::Fail("the right environment, but no RGB colour SGR on screen".into())
    }
}

/// The background-wait process in the member's tree, if one is running.
fn armed_wait(pane: u32) -> Option<u32> {
    world::tree(pane)
        .into_iter()
        .find(|p| {
            p.argv
                .first()
                .and_then(|a| Path::new(a).file_name())
                .is_some_and(|n| n == "brg")
                && p.argv.windows(2).any(|w| w == ["msg", "wait"])
        })
        .map(|p| p.pid)
}

fn still_runs(pid: u32) -> bool {
    std::fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|c| {
        c.split(|&byte| byte == 0)
            .collect::<Vec<_>>()
            .windows(2)
            .any(|words| words == [b"msg".as_slice(), b"wait".as_slice()])
    })
}

pub fn background_wait(w: &World) -> Outcome {
    let id = "claude-background-wait";
    if !world::has_proc() {
        return Outcome::Skip("reads the member's process tree from /proc (Linux only)".into());
    }
    let folder = fresh_dir(&w.trusted, w, "wait");
    let t = Team::new(
        w,
        id,
        &Spec {
            who: Who::Claude,
            member: WAITER,
            locked: false,
            cwd: &folder,
            auto_trust: false,
            args: &["--allowedTools", WAITER_TOOLS],
            instruction: WAITER_PROMPT,
            bus: true,
        },
    );
    match t.up() {
        Ok((true, _)) => {}
        Ok((false, out)) => return Outcome::Fail(format!("up failed: {}", out.trim())),
        Err(e) => return Outcome::Fail(e),
    }
    let Some(pane) = t.pane_pid() else {
        return Outcome::Fail("no pane".into());
    };
    // The session-only allow rule reached the vendor's own process.
    let procs = world::vendor_procs(pane, Who::Claude);
    if !procs.iter().any(|p| {
        p.argv
            .windows(2)
            .any(|a| a[0] == "--allowedTools" && a[1] == WAITER_TOOLS)
    }) {
        return Outcome::Fail("`--allowedTools` didn't reach the Claude process".into());
    }

    // 1–2. Armed: the wait runs in the member's tree, and the member is idle.
    let end = Instant::now() + w.limits.arm;
    let mut armed_busy = false;
    let mut confirm = 0;
    let wait_pid = loop {
        let (state, rule) = t.state();
        if state == "blocked" {
            t.save(id);
            return blocked_at(&rule, &folder);
        }
        let armed = armed_wait(pane);
        if armed.is_some() && state != "idle" {
            armed_busy = true;
        }
        match armed {
            Some(pid) if state == "idle" => {
                confirm += 1;
                if confirm >= 2 {
                    break pid;
                }
            }
            _ => confirm = 0,
        }
        if Instant::now() > end {
            t.save(id);
            return Outcome::Fail(if armed_busy {
                "the wait ran only during a turn, never with the member idle (a foreground wait?)"
                    .into()
            } else {
                "the background wait was never armed".into()
            });
        }
        std::thread::sleep(POLL);
    };
    t.save(&format!("{id}-armed"));

    // 3. Only now: a fresh nonce, sent only as a bus message.
    let nonce = world::nonce();
    let reply = format!("REPLY {}", world::reversed(&nonce));
    let body = format!("nonce {nonce}");
    let mut cmd = t.brg_command();
    cmd.args([
        "msg",
        "send",
        &format!("{}/{}", t.instance(), t.member()),
        "-",
        "--external",
        "live",
        "--json",
    ]);
    let published = match limited_with_stdin(&mut cmd, w.limits.brg, Some(body.as_bytes())) {
        Ok(o) => serde_json::from_slice::<Value>(&o.stdout).unwrap_or(Value::Null),
        Err(e) => return Outcome::Fail(format!("send {e}")),
    };
    if published["ok"] != true {
        return Outcome::Fail(format!("publish failed: {published}"));
    }
    // 4. A new assistant turn naming the nonce, started after the wait
    // exited. Each poll checks the wait before and after one look of the
    // pane, so a look is known to be from while it ran (alive after) or
    // after it ended (gone before); a poll where it ended in between counts
    // only against a PASS.
    let start = Instant::now();
    let end = start + w.limits.reply;
    let mut polls = 0u32;
    let mut last = start;
    let mut max_gap = Duration::ZERO;
    let mut exit: Option<(u32, Instant)> = None;
    let mut turn: Option<Instant> = None;
    let ms = |d: Duration| d.as_millis();
    loop {
        let now = Instant::now();
        if polls > 0 {
            max_gap = max_gap.max(now - last);
        }
        last = now;
        polls += 1;
        let alive_before = still_runs(wait_pid);
        let look = t.look();
        let alive_after = still_runs(wait_pid);
        let (state, rule) = t.read(&look);
        let replied = look
            .screen
            .lines()
            .any(|l| l.starts_with("● ") && l.contains(&reply));
        let resolution = || {
            format!(
                "{polls} polls, gaps up to {} ms (pause {} ms)",
                ms(max_gap),
                ms(FINE_POLL)
            )
        };
        if state == State::Blocked {
            t.save(id);
            return blocked_at(&rule, &folder);
        }
        if look.gone || look.dead {
            t.save(id);
            return Outcome::Fail(format!("the member exited at poll {polls}"));
        }
        match exit {
            None if alive_before => {
                if replied {
                    t.save(id);
                    return Outcome::Fail(format!(
                        "the reply came while the wait still ran: not caused by it ({})",
                        resolution()
                    ));
                }
                if alive_after && state != State::Idle {
                    t.save(id);
                    return Outcome::Fail(format!(
                        "turn started before the wait exited (state {state:?} at poll {polls}; {})",
                        resolution()
                    ));
                }
            }
            None => {
                exit = Some((polls, now));
                if replied {
                    t.save(id);
                    return Outcome::Fail(format!(
                        "the reply was already there on the first look after the wait ended: \
                         its order against the exit is unknown ({})",
                        resolution()
                    ));
                }
                if state == State::Working {
                    turn = Some(now);
                }
            }
            Some((exit_poll, exit_at)) => {
                if state == State::Working {
                    turn.get_or_insert(now);
                }
                if replied {
                    t.save(id);
                    let Some(turn_at) = turn else {
                        return Outcome::Fail(format!(
                            "the reply came with no new turn seen after the wait ended ({})",
                            resolution()
                        ));
                    };
                    return Outcome::Pass(format!(
                        "armed while idle; nonce sent only on the bus; idle while the wait ran; \
                         wait gone at poll {exit_poll}, new turn seen {} ms later, reply {} ms later ({})",
                        ms(turn_at - exit_at),
                        ms(now - exit_at),
                        resolution()
                    ));
                }
            }
        }
        if Instant::now() > end {
            t.save(id);
            return Outcome::Fail(format!(
                "no reply naming this nonce within {} s (wait {}; {})",
                w.limits.reply.as_secs(),
                if exit.is_some() {
                    "exited"
                } else {
                    "still running"
                },
                resolution()
            ));
        }
        std::thread::sleep(FINE_POLL);
    }
}
