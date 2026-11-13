//! The `locked` gate: a pre-tool-use hook shared by Claude and Codex.
//!
//! The vendor runs `brg _gate pre <state> <member>` before every tool call and
//! passes the call as JSON on stdin. The gate allows a call only when it is a
//! shell call to one of the member's granted command prefixes followed by plain
//! arguments, or one of the member's listed connector tools. Everything else is
//! denied. Any internal failure also denies, because both vendors treat hook
//! failures other than exit code 2 as non-blocking.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::process::ExitCode;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::names::is_plain_word;
use crate::state::{StateDir, write_private};

/// What one member may do, written at a start.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GateRules {
    /// Granted command prefixes, e.g. `["slackbot", "read"]`.
    pub prefixes: Vec<Vec<String>>,
    /// Command name → the shim's absolute path, which is also accepted.
    pub shims: BTreeMap<String, String>,
    /// Connector tool names the vendor uses, e.g. `mcp__Example__read`.
    pub mcp_tools: Vec<String>,
}

/// Split a command into words the gate accepts, or `None` if any part of it
/// is something a shell would expand, redirect, chain or substitute.
pub fn plain_words(command: &str) -> Option<Vec<String>> {
    if command.contains('\n') || command.contains('\r') || command.contains('\t') {
        return None;
    }
    let bytes: Vec<char> = command.chars().collect();
    let mut words = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == ' ' {
            i += 1;
            continue;
        }
        let (word, next) = match bytes[i] {
            '\'' => {
                let end = bytes[i + 1..].iter().position(|&c| c == '\'')? + i + 1;
                (bytes[i + 1..end].iter().collect::<String>(), end + 1)
            }
            '"' => {
                let end = bytes[i + 1..].iter().position(|&c| c == '"')? + i + 1;
                let inner: String = bytes[i + 1..end].iter().collect();
                if inner.chars().any(|c| "$`\\!".contains(c)) {
                    return None;
                }
                (inner, end + 1)
            }
            _ => {
                let end = bytes[i..]
                    .iter()
                    .position(|&c| c == ' ')
                    .map(|p| p + i)
                    .unwrap_or(bytes.len());
                let word: String = bytes[i..end].iter().collect();
                if !is_plain_word(&word) {
                    return None;
                }
                (word, end)
            }
        };
        // A quoted word must be followed by a space or the end: no `'a''b'` splicing.
        if next < bytes.len() && bytes[next] != ' ' {
            return None;
        }
        words.push(word);
        i = next;
    }
    Some(words)
}

/// The decision for one tool call.
pub fn decide(
    rules: &GateRules,
    tool_name: &str,
    tool_input: &serde_json::Value,
) -> Result<(), String> {
    if rules.mcp_tools.iter().any(|t| t == tool_name) {
        return Ok(());
    }
    if tool_name != "Bash" {
        return Err(format!("`{tool_name}` is not allowed for this member"));
    }
    let command = match tool_input.get("command") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => parts
            .iter()
            .map(|p| p.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()
            .ok_or("unreadable command")?
            .join(" "),
        _ => return Err("no command".into()),
    };
    let mut words = plain_words(command.trim()).ok_or(
        "only granted commands with plain arguments are allowed (no shell operators, expansions or redirects)",
    )?;
    if let Some(first) = words.first().cloned()
        && let Some((name, _)) = rules.shims.iter().find(|(_, path)| **path == first)
    {
        words[0] = name.clone();
    }
    let granted = rules
        .prefixes
        .iter()
        .any(|prefix| words.len() >= prefix.len() && words[..prefix.len()] == prefix[..]);
    if granted {
        Ok(())
    } else {
        Err("not a granted command".into())
    }
}

#[derive(Deserialize)]
struct HookInput {
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    tool_input: serde_json::Value,
}

#[derive(Deserialize)]
struct SessionInput {
    #[serde(default)]
    session_id: String,
}

/// Whether `id` can be handed back to the vendor's resume as one argument.
pub fn usable_session_id(id: &str) -> bool {
    id.len() <= 128
        && id.starts_with(|c: char| c.is_ascii_alphanumeric())
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Record the session a SessionStart hook reports, for `brg start --resume`.
fn record_session(state: &StateDir, member: &str) -> Result<(), String> {
    let mut raw = String::new();
    std::io::stdin()
        .take(1024 * 1024)
        .read_to_string(&mut raw)
        .map_err(|e| format!("cannot read hook input: {e}"))?;
    let input: SessionInput =
        serde_json::from_str(&raw).map_err(|e| format!("bad hook input: {e}"))?;
    if !usable_session_id(&input.session_id) {
        return Err("the hook input has no usable session_id".into());
    }
    write_private(
        &state.member_session(member),
        format!("{}\n", input.session_id).as_bytes(),
    )
    .map_err(|e| format!("cannot record session: {e}"))
}

/// `brg _gate pre|post|start|session <state> <member>`. `start` is a `locked`
/// member's SessionStart hook: it marks the gate active and records the
/// session. `session` is every other member's, and only records the session.
pub fn hook_main(event: &str, state_root: &Path, member: &str) -> ExitCode {
    let state = StateDir::from_root(state_root);
    match event {
        "post" => {
            if let Err(e) = post_send(&state, member) {
                let note = state.member_dir(member).join("send-note");
                let _ = write_private(&note, e.to_string().as_bytes());
            }
            ExitCode::SUCCESS
        }
        "start" => {
            let stamp = format!("{}\n", std::process::id());
            if let Err(e) = write_private(&state.gate_marker(member), stamp.as_bytes()) {
                eprintln!("borgchart gate: cannot record start: {e}");
                return ExitCode::from(2);
            }
            // The gate is active either way; only resuming needs the session.
            if let Err(e) = record_session(&state, member) {
                eprintln!("borgchart: {e}; `brg start --resume` can't reopen this session");
            }
            ExitCode::SUCCESS
        }
        "session" => match record_session(&state, member) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                // 1, not 2: never block the session over it.
                eprintln!("borgchart: {e}; `brg start --resume` can't reopen this session");
                ExitCode::from(1)
            }
        },
        "pre" => {
            let verdict = (|| -> Result<(), String> {
                let text = std::fs::read_to_string(state.gate_file(member))
                    .map_err(|e| format!("cannot read gate rules: {e}"))?;
                let rules: GateRules =
                    serde_json::from_str(&text).map_err(|e| format!("bad gate rules: {e}"))?;
                let mut raw = String::new();
                std::io::stdin()
                    .take(1024 * 1024)
                    .read_to_string(&mut raw)
                    .map_err(|e| format!("cannot read hook input: {e}"))?;
                let input: HookInput =
                    serde_json::from_str(&raw).map_err(|e| format!("bad hook input: {e}"))?;
                decide(&rules, &input.tool_name, &input.tool_input)
            })();
            match verdict {
                Ok(()) => ExitCode::SUCCESS,
                Err(why) => {
                    eprintln!("borgchart locked: {why}");
                    ExitCode::from(2)
                }
            }
        }
        other => {
            eprintln!("borgchart gate: unknown event `{other}`");
            ExitCode::from(2)
        }
    }
}

fn post_send(state: &StateDir, member: &str) -> Result<(), Box<dyn std::error::Error>> {
    use crate::bounded_process::{Limits, run};
    use std::time::{Duration, Instant};
    use std::{fs, os::unix::fs::OpenOptionsExt, process::Command};
    if std::env::var("BORGCHART_MEMBER").as_deref() != Ok(member) {
        return Ok(());
    }
    let queue = state.member_tmp(member).join("send-queue");
    if !fs::symlink_metadata(&queue).is_ok_and(|m| m.is_dir()) {
        return Ok(());
    }
    let meta = crate::state::read_meta(state)?.ok_or("no running team")?;
    let actor = meta.members.get(member).ok_or("unknown sender")?;
    let parent = actor.parent.as_deref().ok_or("sender has no parent")?;
    let full_parent = if parent.contains('/') {
        parent.into()
    } else {
        format!("{}/{parent}", state.id)
    };
    let mut requests = fs::read_dir(queue)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect::<Vec<_>>();
    requests.sort();
    let deadline = Instant::now() + Duration::from_secs(60);
    for path in requests.into_iter().take(8) {
        if Instant::now() >= deadline {
            break;
        }
        let sending = path.with_extension("sending");
        if fs::rename(&path, &sending).is_err() {
            continue;
        }
        let flags = rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK;
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(flags.bits() as i32)
            .open(&sending)?;
        let request: Value = serde_json::from_reader(file.take(65536)).unwrap_or_default();
        let target = request["target"].as_str().unwrap_or("");
        let text = request["text"].as_str().unwrap_or("");
        if request["sender"] != member
            || (target != parent && target != full_parent)
            || text.is_empty()
        {
            let note = state.member_dir(member).join("send-note");
            write_private(&note, b"Refused queued send: wrong sender or target.\n")?;
            fs::remove_file(sending)?;
            continue;
        }
        let mut command = Command::new(&meta.brg);
        command.args(["msg", "send", "--json", target, text]);
        let limits = Limits {
            deadline: deadline.min(Instant::now() + Duration::from_secs(30)),
            stdout_bytes: 65536,
            stderr_bytes: 65536,
        };
        if let Ok(output) = run(&mut command, None, limits) {
            let result: Value = serde_json::from_slice(&output.stdout).unwrap_or_default();
            if output.status.success() || result["id"].is_string() {
                fs::remove_file(sending)?;
                continue;
            }
            if result["ok"] == false {
                fs::rename(sending, path)?;
                break;
            }
        }
        return Err("Queued send outcome uncertain; inspect .sending files in the member's tmp/send-queue and the bus history before retrying.\n".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rules() -> GateRules {
        GateRules {
            prefixes: vec![
                vec!["slackbot".into(), "read".into()],
                vec!["slackbot".into(), "escalate".into()],
            ],
            shims: BTreeMap::from([("slackbot".into(), "/state/m/bin/slackbot".into())]),
            mcp_tools: vec!["mcp__Example__read".into()],
        }
    }

    fn bash(cmd: &str) -> Result<(), String> {
        decide(&rules(), "Bash", &json!({ "command": cmd }))
    }

    #[test]
    fn session_ids_are_single_plain_arguments() {
        assert!(usable_session_id("01a1c796-56be-7392-a43a-152e7e335777"));
        assert!(usable_session_id("a5a64568-a729-488f-b7fd-b3e7674ddd18"));
        for bad in ["", "-x", "--last", "a b", "a/b", "a;b", &"a".repeat(129)] {
            assert!(!usable_session_id(bad), "{bad}");
        }
    }

    #[test]
    fn allows_granted_commands_with_plain_arguments() {
        assert!(bash("slackbot read 1234.5678").is_ok());
        assert!(bash("slackbot read 'with space'").is_ok());
        assert!(bash("slackbot read \"it's fine\"").is_ok());
        assert!(bash("/state/m/bin/slackbot escalate 1 high outage").is_ok());
        assert!(decide(&rules(), "mcp__Example__read", &json!({})).is_ok());
    }

    #[test]
    fn denies_the_spike_list() {
        let tricks = [
            "slackbot read 1; touch x",
            "slackbot read 1 && touch x",
            "slackbot read 1 || touch x",
            "slackbot read 1 | tee x",
            "slackbot read 1 & touch x",
            "slackbot read $(touch x)",
            "slackbot read `touch x`",
            "slackbot read <(touch x)",
            "slackbot read 1 >(touch x)",
            "slackbot read \"$HOME\"",
            "slackbot read ${PATH}",
            "slackbot read ~",
            "slackbot read *",
            "slackbot read 1 > x",
            "slackbot read 1 <<< x",
            "X=1 slackbot read 1",
            "'slack''bot' read 1",
            "/other/bin/slackbot read 1",
            "slackbot reply 1 hi",
            "touch x",
            "env",
            "slackbot read 1 #; touch x",
            "slackbot read 1 \\; touch x",
            "slackbot read 1\ntouch x",
            "slackbot read \"a\\\"b\"",
            "slackbot read 'unterminated",
            "slackbot",
        ];
        for t in tricks {
            assert!(bash(t).is_err(), "should deny: {t}");
        }
    }

    #[test]
    fn denies_other_tools() {
        for tool in [
            "apply_patch",
            "Write",
            "Read",
            "webrun",
            "mcp__other__x",
            "",
        ] {
            assert!(decide(&rules(), tool, &json!({})).is_err(), "{tool}");
        }
    }

    #[test]
    fn denies_missing_command() {
        assert!(decide(&rules(), "Bash", &json!({})).is_err());
    }
}
