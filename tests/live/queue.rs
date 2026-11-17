//! Codex next-turn delivery, including a locked tool's real post-tool send.
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::cases::{Spec, Team};
use super::rules::{Outcome, Who};
use super::world::{self, World, limited_with_stdin};

const PROMPT: &str = "This is an automated queue test. End this opening turn with READY. For each later message containing `nonce WORD`, reply with the literal prefix REPLY- immediately followed by WORD copied verbatim. If a message has several nonce WORD lines, reply with one line per word, in input order. If the message also says HOLD, first spend a turn working: vendor members run `sleep 5`; locked members run their granted promote action with timestamp 1700000000.000001 and reason question. For a message containing `promote TIMESTAMP`, run your granted promote action with that timestamp and reason question, then reply SENT. For an incoming `Slack promote` message, reply with the literal prefix PROMOTED- immediately followed by the timestamp after ts= copied verbatim. Do not read Slack or run other tools. End each turn after the requested reply.";

fn state_dir(w: &World, t: &Team<'_>) -> borgchart::state::StateDir {
    borgchart::state::StateDir::from_root(
        &PathBuf::from(&w.env["BORGCHART_STATE_HOME"]).join(t.instance()),
    )
}

fn screen(w: &World, t: &Team<'_>, member: &str) -> String {
    w.tmux(&[
        "capture-pane",
        "-p",
        "-t",
        &format!("={}:={member}", t.session()),
    ])
}

fn wait_for(
    w: &World,
    t: &Team<'_>,
    member: &str,
    what: &str,
    ready: impl Fn() -> bool,
) -> Result<(), Outcome> {
    let end = Instant::now() + w.limits.reply;
    loop {
        if ready() {
            return Ok(());
        }
        let text = screen(w, t, member);
        let vendor = borgchart::rules::load(borgchart::team::Vendor::Codex).unwrap();
        if vendor.read("", &text).state == borgchart::rules::State::Blocked {
            return Err(Outcome::Blocked(format!(
                "{what}: a prompt was left unanswered"
            )));
        }
        if Instant::now() >= end {
            t.save(&format!("codex-queue-{member}-failed"));
            return Err(Outcome::Fail(format!(
                "{member}: timed out waiting for {what}; screen: {text}"
            )));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn send(w: &World, t: &Team<'_>, body: &str, delivery: &str) -> Result<Value, Outcome> {
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
    let out =
        limited_with_stdin(&mut cmd, w.limits.brg, Some(body.as_bytes())).map_err(Outcome::Fail)?;
    let result: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    if !out.status.success() || result["delivery"] != delivery {
        return Err(Outcome::Fail(format!(
            "send expected {delivery}: {result}; {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(result)
}

fn reply(w: &World, t: &Team<'_>, nonce: &str) -> Result<(), Outcome> {
    let want = format!("REPLY-{nonce}");
    wait_for(w, t, &t.member(), &want, || {
        screen(w, t, &t.member())
            .lines()
            .scan(false, |assistant, l| {
                if l.starts_with("• ") {
                    *assistant = true;
                }
                if l.starts_with("› ") {
                    *assistant = false;
                }
                Some(*assistant && l.trim_start_matches("• ").trim() == want)
            })
            .any(|matched| matched)
    })
}

fn plugin_parent(w: &World, t: &Team<'_>) -> Result<String, Outcome> {
    let (ok, text) = t
        .brg(&["status", &t.instance(), "--json"], w.limits.brg)
        .map_err(Outcome::Fail)?;
    let rows: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if !ok {
        return Err(Outcome::Fail(text));
    }
    rows.as_array()
        .into_iter()
        .flatten()
        .find(|r| r["role"] == "queue-parent")
        .and_then(|r| r["member"].as_str())
        .map(String::from)
        .ok_or_else(|| Outcome::Fail(format!("no queue parent: {rows}")))
}

fn posture(w: &World, locked: bool) -> Result<(), Outcome> {
    let folder = w.trusted.join(w.fresh("queue-work"));
    std::fs::create_dir_all(&folder).unwrap();
    let t = Team::new(
        w,
        "codex-queue",
        &Spec {
            who: Who::Codex,
            member: if locked {
                "queue-locked"
            } else {
                "queue-vendor"
            },
            locked,
            cwd: &folder,
            auto_trust: true,
            args: &[],
            instruction: PROMPT,
            bus: true,
        },
    );
    let model = std::env::var("BORGCHART_LIVE_CODEX_MODEL").ok();
    let model = serde_json::to_string(super::rules::codex_model(model.as_deref())).unwrap();
    let prompt_path = t.dir.join(if locked {
        "queue-locked.md"
    } else {
        "queue-vendor.md"
    });
    std::fs::write(
        prompt_path,
        format!("---\nmodels: {{codex: {model}}}\n---\n{PROMPT}\n"),
    )
    .unwrap();
    if locked {
        let path = t.dir.join("template.yaml");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("plugins:\n  pager:\n    path: plugins/pager\n    actions:\n      promote: brg-slack promote <ts> <reason>\n{text}    parent: queue-parent\n    plugins: {{pager: [promote]}}\n  queue-parent:\n    prompt: queue-parent.md\n    vendor: codex\n")).unwrap();
        std::fs::write(
            t.dir.join("queue-parent.md"),
            format!("---\nmodels: {{codex: {model}}}\n---\n{PROMPT}\n"),
        )
        .unwrap();
        let plugin = w.brg.with_file_name("brg-slack");
        if !plugin.is_file() {
            return Err(Outcome::Fail(
                "build workspace binaries before the live queue case".into(),
            ));
        }
        let bin = t.dir.join("plugins/pager/brg-slack");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::copy(plugin, bin).unwrap();
    }
    // Send during startup, as soon as the recorded pane (and locked gate)
    // permits it, before waiting for create or the opening turn to finish.
    let early = world::nonce();
    std::thread::scope(|s| -> Result<(), Outcome> {
        let up = s.spawn(|| t.up());
        wait_for(w, &t, &t.member(), "start readiness", || {
            let state = state_dir(w, &t);
            borgchart::lifecycle::load_incarnation(&state, &t.member())
                .ok()
                .flatten()
                .is_some_and(|i| i.pane.is_some())
                && (!locked || state.gate_marker(&t.member()).exists())
        })?;
        send(w, &t, &format!("nonce {early}"), "queued")?;
        let (ok, text) = up.join().unwrap().map_err(Outcome::Fail)?;
        if !ok {
            return Err(Outcome::Fail(text));
        }
        Ok(())
    })?;
    reply(w, &t, &early)?;
    wait_for(w, &t, &t.member(), "idle before nonce", || {
        t.state().0 == "idle"
    })?;
    let idle = world::nonce();
    send(w, &t, &format!("nonce {idle}"), "queued")?;
    reply(w, &t, &idle)?;

    let hold = world::nonce();
    send(w, &t, &format!("HOLD nonce {hold}"), "queued")?;
    wait_for(w, &t, &t.member(), "a running turn", || {
        t.state().0 == "working"
    })?;
    let middle = world::nonce();
    send(w, &t, &format!("nonce {middle}"), "queued")?;
    reply(w, &t, &middle)?;
    // The earlier turn's response must precede the queued turn's response.
    let text = screen(w, &t, &t.member());
    let before = text.find(&format!("REPLY-{hold}"));
    let after = text.find(&format!("REPLY-{middle}"));
    if !matches!((before, after), (Some(a), Some(b)) if a < b) {
        return Err(Outcome::Fail(format!(
            "mid-turn queue did not run after the earlier turn: {text}"
        )));
    }

    let large = [world::nonce(), world::nonce(), world::nonce()];
    let markers = large
        .iter()
        .map(|n| format!("\nnonce {n}\n"))
        .collect::<Vec<_>>();
    let padding = 16 * 1024 - markers.iter().map(String::len).sum::<usize>();
    let body = format!(
        "{}{}{}{}{}",
        markers[0],
        "x".repeat(padding / 2),
        markers[1],
        "x".repeat(padding - padding / 2),
        markers[2]
    );
    send(w, &t, &body, "queued")?;
    for nonce in &large {
        reply(w, &t, nonce)?;
    }

    if locked {
        let parent = plugin_parent(w, &t)?;
        let timestamp = format!("1700000000.{:06}", std::process::id() % 1_000_000);
        send(w, &t, &format!("promote {timestamp}"), "queued")?;
        let want = format!("PROMOTED-{timestamp}");
        wait_for(
            w,
            &t,
            &parent,
            "the parent's queued tool-hook reply",
            || screen(w, &t, &parent).contains(&want),
        )?;
        let state = state_dir(w, &t);
        let bus = borgchart::bus::store::open(&state).map_err(|e| Outcome::Fail(e.to_string()))?;
        let messages =
            borgchart::bus::store::snapshot(&bus).map_err(|e| Outcome::Fail(e.to_string()))?;
        if !messages.records.values().any(|r| {
            r.from == format!("{}/{}", t.instance(), t.member()) && r.body.contains(&timestamp)
        }) {
            return Err(Outcome::Fail(
                "no committed promotion authored by the locked child".into(),
            ));
        }
    }

    let state = state_dir(w, &t);
    let session = std::fs::read_to_string(state.member_session(&t.member()))
        .map_err(|e| Outcome::Fail(e.to_string()))?;
    let (ok, text) = t
        .brg(&["stop", &t.instance()], w.limits.brg)
        .map_err(Outcome::Fail)?;
    if !ok {
        return Err(Outcome::Fail(text));
    }
    let stopped = world::nonce();
    if !locked {
        send(w, &t, &format!("nonce {stopped}"), "stored")?;
    }
    let (ok, text) = t
        .brg(&["start", &t.instance(), "--resume"], w.limits.up)
        .map_err(Outcome::Fail)?;
    if !ok {
        return Err(Outcome::Fail(text));
    }
    if !locked {
        reply(w, &t, &stopped)?;
    }
    let resumed = world::nonce();
    send(w, &t, &format!("nonce {resumed}"), "queued")?;
    reply(w, &t, &resumed)?;
    if std::fs::read_to_string(state.member_session(&t.member()))
        .ok()
        .as_deref()
        != Some(session.as_str())
    {
        return Err(Outcome::Fail(
            "resume did not record the same session".into(),
        ));
    }
    t.save(if locked {
        "codex-queue-locked"
    } else {
        "codex-queue-vendor"
    });
    Ok(())
}

pub fn run(w: &World) -> Outcome {
    for locked in [false, true] {
        if let Err(outcome) = posture(w, locked) {
            return outcome;
        }
    }
    Outcome::Pass("vendor and locked: startup, idle, mid-turn ordering, 16 KiB, resume; locked PostToolUse promotion reached a non-interactive Codex parent as a queued turn".into())
}
