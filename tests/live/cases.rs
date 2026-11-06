//! The cases that start real members through real `brg up`, each in a fresh
//! team and folder. Each returns its outcome; none answers a tool approval,
//! and the only screen brg answers is folder trust, through `auto_trust`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::rules::{CHECK_STATUS_TRUST_DETAIL, Outcome, RUN_CLAUDE_TRUST_ON, Who};
use super::world::{self, World, limited, limited_with_stdin};

const POLL: Duration = Duration::from_millis(300);

/// The Claude background-wait member (the fake keys its behaviour on it).
pub const WAITER: &str = "waiter";

/// Commands the background-wait member may run, as session-only
/// `--allowedTools` entries in its deployment's `args`: the wait and the
/// read of its own inbox. Nothing is saved to settings.
pub const WAITER_TOOLS: &str = "Bash(brg inbox wait --background:*),Bash(brg inbox --full:*)";

const READY: &str = "This is an automated test. Reply with the single word READY. Use no tools.";

const WAITER_PROMPT: &str = "This is an automated test of background commands. Follow these steps exactly, and use no other tools.

1. Run this with your Bash tool, with run_in_background set to true: `brg inbox wait --background --timeout 900`
2. End your turn right away with the one word ARMED. Don't wait for, poll or check on the background command.
3. When that background command has finished, run `brg inbox --full` with your Bash tool. One record's body is `nonce <word>`.
4. Reply with one line: REPLY, a space, and that word written backwards, letter by letter. Then stop.";

/// One case's team, started with `brg up`. Dropping it runs `brg down`.
pub struct Team<'w> {
    w: &'w World,
    pub file: PathBuf,
    pub member: String,
    pub session: String,
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
        std::fs::write(
            dir.join("team.yaml"),
            format!(
                "members:\n  {m}:\n    prompt: {m}.md\n    vendor: {v}\n{posture}",
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
            format!("---\nrole: tester\n---\n{}\n", s.instruction),
        )
        .unwrap();
        let args = serde_json::to_string(s.args).unwrap();
        let mut yaml = format!(
            "schema: 1\nauto_trust: {}\nmembers:\n  {}:\n    cwd: {}\n    args: {args}\n",
            s.auto_trust,
            s.member,
            serde_json::to_string(&s.cwd.display().to_string()).unwrap(),
        );
        if s.bus {
            let root = w.scratch.join(w.fresh("bus"));
            yaml.push_str(&format!("bus:\n  root: {}\n", root.display()));
        }
        let file = dir.join("borgchart.yaml");
        std::fs::write(&file, yaml).unwrap();
        let file = file.canonicalize().unwrap();
        let id = borgchart::state::deployment_id(&file);
        Team {
            w,
            session: format!("brg-{id}"),
            file,
            member: s.member.to_string(),
        }
    }

    pub fn brg(&self, args: &[&str], limit: Duration) -> Result<(bool, String), String> {
        let mut cmd = self.w.command(&self.w.brg);
        cmd.args(args).arg("-f").arg(&self.file);
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
        self.brg(&["up"], self.w.limits.up)
            .map_err(|e| format!("`brg up` {e}"))
    }

    /// The member's `brg status --json` row.
    pub fn row(&self) -> Value {
        let Ok((true, text)) = self.brg(&["status", "--json"], self.w.limits.brg) else {
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
            .find(|r| r["name"] == self.member.as_str())
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
        format!("={}:={}", self.session, self.member)
    }

    /// The member's screen, styled (`capture-pane -e`).
    pub fn screen(&self) -> String {
        self.w
            .tmux(&["capture-pane", "-p", "-e", "-t", &self.target()])
    }

    pub fn plain(&self) -> String {
        self.w.tmux(&["capture-pane", "-p", "-t", &self.target()])
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

impl Drop for Team<'_> {
    fn drop(&mut self) {
        let _ = self.brg(&["down"], self.w.limits.brg);
        let _ = self
            .w
            .tmux(&["kill-session", "-t", &format!("={}", self.session)]);
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
    std::thread::sleep(w.limits.hold);
    *status = status_trust(&t);
    let row = t.row();
    t.save(id);
    let still = row["rule"] == "trust-dialog";
    let accepted = !row["trust_accepted"].is_null();
    match (still, accepted) {
        (true, false) => Outcome::Pass(format!(
            "stopped at the trust dialog, unanswered after {} s",
            w.limits.hold.as_secs()
        )),
        _ => Outcome::Fail(format!(
            "after the hold: rule {:?}, trust_accepted {}",
            row["rule"], row["trust_accepted"]
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
    let mut trust_seen = false;
    let mut idle_seen = false;
    let mut blocked = None;
    let up = std::thread::scope(|s| {
        let up = s.spawn(|| t.up());
        // While `up` runs (a locked member keeps it waiting for its gate)
        // and for the start limit after, watch the screen.
        let end = Instant::now() + w.limits.start;
        let mut held_since: Option<Instant> = None;
        loop {
            let screen = t.plain();
            if codex_trust_screen(&screen) {
                if !trust_seen && trust == Some(false) {
                    *status = status_trust(&t);
                }
                trust_seen = true;
                held_since.get_or_insert_with(Instant::now);
                t.save(id);
            }
            let (state, rule) = t.state();
            if state == "idle" && !codex_trust_screen(&screen) {
                idle_seen = true;
            }
            if state == "blocked" {
                blocked = Some(rule);
            }
            let done = up.is_finished();
            let held = held_since.is_some_and(|h| h.elapsed() >= w.limits.hold);
            if done && (idle_seen || held || blocked.is_some() || Instant::now() > end) {
                break;
            }
            if idle_seen && trust == Some(true) && done {
                break;
            }
            std::thread::sleep(POLL);
        }
        up.join().unwrap()
    });
    if !trust_seen && !idle_seen {
        t.save(id);
    }
    let after = w.codex_config_hash();
    let hash = if before == after {
        "config.toml unchanged".to_string()
    } else {
        return Outcome::Fail(format!(
            "the Codex config.toml changed during the case (sha256 {before} → {after}); not restored"
        ));
    };
    let up = match up {
        Ok((_, out)) => out,
        Err(e) => return Outcome::Fail(e),
    };
    if let Some(rule) = blocked {
        return blocked_at(&rule, &repo);
    }
    match trust {
        Some(true) => {
            if trust_seen {
                Outcome::Fail("stopped at the trust screen despite the session trust".into())
            } else if idle_seen {
                Outcome::Pass(format!(
                    "reached its prompt with session-only trust; {hash}"
                ))
            } else {
                Outcome::Fail(format!("never reached its prompt; up said: {}", up.trim()))
            }
        }
        _ => {
            if trust_seen && !idle_seen {
                Outcome::Pass(format!("stopped at the trust screen, unanswered; {hash}"))
            } else if idle_seen {
                Outcome::Blocked(format!(
                    "no trust screen in {}: the folder is already trusted",
                    repo.display()
                ))
            } else {
                Outcome::Fail(format!("no trust screen seen; up said: {}", up.trim()))
            }
        }
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
                && p.argv
                    .windows(3)
                    .any(|w| w == ["inbox", "wait", "--background"])
        })
        .map(|p| p.pid)
}

fn still_runs(pid: u32) -> bool {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .is_ok_and(|c| String::from_utf8_lossy(&c).contains("--background"))
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
    let draft = serde_json::json!({"to": WAITER, "kind": "note", "body": format!("nonce {nonce}")});
    let mut cmd = w.command(&w.brg);
    cmd.args(["msg", "publish", "--external", "live", "--key"])
        .arg(format!("live-{nonce}"))
        .args(["--record", "-", "--json", "-f"])
        .arg(&t.file);
    let published =
        match limited_with_stdin(&mut cmd, w.limits.brg, Some(draft.to_string().as_bytes())) {
            Ok(o) => serde_json::from_slice::<Value>(&o.stdout).unwrap_or(Value::Null),
            Err(e) => return Outcome::Fail(format!("publish {e}")),
        };
    if published["ok"] != true {
        return Outcome::Fail(format!("publish failed: {published}"));
    }
    // brg's own wake must not be what starts the turn.
    if published["attention"] != "none" {
        return Outcome::Blocked(format!(
            "brg arranged attention `{}` for the message; the case needs brg's wake off for this member",
            published["attention"]
        ));
    }

    // 4. A new assistant turn naming the nonce, after the wait exited.
    let end = Instant::now() + w.limits.reply;
    let mut exited: Option<Instant> = None;
    loop {
        let alive = still_runs(wait_pid);
        if !alive {
            exited.get_or_insert_with(Instant::now);
        }
        let screen = t.plain();
        let replied = screen
            .lines()
            .any(|l| l.starts_with("● ") && l.contains(&reply));
        if replied {
            t.save(id);
            if alive {
                return Outcome::Fail(
                    "the reply came while the wait still ran: not caused by it".into(),
                );
            }
            return Outcome::Pass(format!(
                "armed while idle; nonce sent only on the bus (attention none); reply after the wait exited ({} ms later)",
                exited.map(|e| e.elapsed().as_millis()).unwrap_or(0)
            ));
        }
        let (state, rule) = t.state();
        if state == "blocked" {
            t.save(id);
            return blocked_at(&rule, &folder);
        }
        if Instant::now() > end {
            t.save(id);
            return Outcome::Fail(format!(
                "no reply naming this nonce within {} s (wait {})",
                w.limits.reply.as_secs(),
                if exited.is_some() {
                    "exited"
                } else {
                    "still running"
                }
            ));
        }
        std::thread::sleep(POLL);
    }
}
