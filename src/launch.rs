//! Building what a member launches with: its opening prompt, its vendor CLI
//! arguments, its plugin shims and, in `locked` posture, its gate.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::deploy::MemberDeploy;
use crate::gate::GateRules;
use crate::state::{StateDir, shell_quote};
use crate::team::{Grant, Member, Plugin, Posture, Team, Vendor};

/// One granted command line as the member is told about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Granted {
    pub plugin: String,
    pub command: String,
    /// The enforced prefix, e.g. `slackbot read`.
    pub prefix: Vec<String>,
    /// The full action line, e.g. `slackbot read <ts>`.
    pub line: String,
}

/// Every command line a member is granted, in a stable order.
pub fn granted(team: &Team, member: &Member) -> Vec<Granted> {
    let mut out = Vec::new();
    for (plugin_name, grant) in &member.grants {
        let Some(plugin) = team.plugins.get(plugin_name) else {
            continue;
        };
        let names: Vec<&String> = match grant {
            Grant::All => plugin.actions.keys().collect(),
            Grant::Actions(names) => names.iter().collect(),
        };
        for name in names {
            if let Some(action) = plugin.actions.get(name) {
                out.push(Granted {
                    plugin: plugin_name.clone(),
                    command: action.command().to_string(),
                    prefix: action.prefix.clone(),
                    line: action.line.clone(),
                });
            }
        }
        if matches!(grant, Grant::All)
            && plugin.actions.is_empty()
            && let Some(command) = &plugin.command
        {
            out.push(Granted {
                plugin: plugin_name.clone(),
                command: command.clone(),
                prefix: vec![command.clone()],
                line: format!("{command} --help"),
            });
        }
    }
    out
}

/// Distinct (plugin, command) pairs that need a shim.
pub fn shim_commands(granted: &[Granted]) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = granted
        .iter()
        .map(|g| (g.plugin.clone(), g.command.clone()))
        .collect();
    pairs.sort();
    pairs.dedup();
    pairs
}

/// The shim script for one command: it hands off to `brg _run`, which holds
/// the plugin's secrets and the member's identity.
pub fn shim_script(
    brg: &Path,
    state: &StateDir,
    member: &str,
    plugin: &str,
    command: &str,
) -> String {
    format!(
        "#!/bin/sh\n# Written by brg for member {member}. Runs plugin {plugin}'s `{command}`.\nexec {} _run {} {} {} {} \"$@\"\n",
        shell_quote(&brg.display().to_string()),
        shell_quote(&state.root.display().to_string()),
        shell_quote(member),
        shell_quote(plugin),
        shell_quote(command),
    )
}

/// The member's opening prompt: its standing instruction, the deployment's
/// extra task, and the commands it may run (shims by absolute path).
pub fn opening_prompt(
    member: &Member,
    deploy: Option<&MemberDeploy>,
    granted: &[Granted],
    shim_dir: &Path,
    posture: Posture,
    help: &BTreeMap<String, String>,
) -> String {
    let mut prompt = member.instruction.trim_end().to_string();
    if let Some(task) = deploy.and_then(|d| d.task.as_deref()) {
        prompt.push_str("\n\n");
        prompt.push_str(task.trim_end());
    }
    if !granted.is_empty() {
        prompt.push_str("\n\n## Commands you can run\n\n");
        for g in granted {
            let shim = shim_dir.join(&g.command);
            let rest = g.line.strip_prefix(&g.command).unwrap_or(&g.line);
            prompt.push_str(&format!("- `{}{}`\n", shim.display(), rest));
            if let Some(text) = help.get(&g.command)
                && g.line.ends_with("--help")
            {
                prompt.push_str(&format!("\n```\n{}\n```\n", text.trim_end()));
            }
        }
        if posture == Posture::Locked {
            prompt.push_str(
                "\nOnly these commands are allowed. Give each argument as a plain word or a quoted string; shell operators, variables and redirects are refused.\n",
            );
        }
    }
    prompt
}

/// MCP tool names the vendor uses for a connector's tools.
pub fn mcp_tool_names(vendor: Vendor, member: &Member) -> Vec<String> {
    let mut names = Vec::new();
    for (server, tools) in &member.connectors.servers {
        let server = match vendor {
            // Claude normalizes server names: anything but letters, digits, `_` and `-` becomes `_`.
            Vendor::Claude => server
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect::<String>(),
            Vendor::Codex => server.clone(),
        };
        for tool in tools {
            names.push(format!("mcp__{server}__{tool}"));
        }
    }
    names
}

pub fn gate_rules(granted: &[Granted], shim_dir: &Path, mcp_tools: Vec<String>) -> GateRules {
    let mut prefixes: Vec<Vec<String>> = granted.iter().map(|g| g.prefix.clone()).collect();
    prefixes.sort();
    prefixes.dedup();
    let shims = granted
        .iter()
        .map(|g| {
            (
                g.command.clone(),
                shim_dir.join(&g.command).display().to_string(),
            )
        })
        .collect();
    GateRules {
        prefixes,
        shims,
        mcp_tools,
    }
}

fn hook_command(brg: &Path, event: &str, state: &StateDir, member: &str) -> String {
    format!(
        "{} _gate {event} {} {}",
        shell_quote(&brg.display().to_string()),
        shell_quote(&state.root.display().to_string()),
        shell_quote(member)
    )
}

/// Claude `--settings` JSON that installs the gate.
pub fn claude_settings(brg: &Path, state: &StateDir, member: &str) -> serde_json::Value {
    serde_json::json!({
        "hooks": {
            "PreToolUse": [{"matcher": "*", "hooks": [{"type": "command", "command": hook_command(brg, "pre", state, member)}]}],
            "SessionStart": [{"matcher": "*", "hooks": [{"type": "command", "command": hook_command(brg, "start", state, member)}]}],
        }
    })
}

/// Codex's trust hash for a session-flag hook: SHA-256 of the normalized hook
/// identity as compact JSON with sorted keys (matching Codex's own hashing).
fn codex_hook_hash(event_label: &str, command: &str) -> String {
    let identity = serde_json::json!({
        "event_name": event_label,
        "matcher": "*",
        "hooks": [{"type": "command", "command": command, "timeout": 600, "async": false}],
    });
    // serde_json's Map is a BTreeMap without the preserve_order feature, so keys are sorted.
    let text = serde_json::to_string(&identity).unwrap_or_default();
    let digest = Sha256::digest(text.as_bytes());
    format!(
        "sha256:{}",
        digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

fn toml_string(value: &str) -> String {
    // TOML basic strings accept JSON string escapes for these characters.
    serde_json::to_string(value).unwrap_or_default()
}

/// The `-c hooks=…` value for Codex: the gate hooks plus their trust state,
/// for this session only.
pub fn codex_hook_config(brg: &Path, state: &StateDir, member: &str) -> String {
    let pre = hook_command(brg, "pre", state, member);
    let start = hook_command(brg, "start", state, member);
    let key = |label: &str| toml_string(&format!("/<session-flags>/config.toml:{label}:0:0"));
    format!(
        "hooks={{PreToolUse=[{{matcher=\"*\",hooks=[{{type=\"command\",command={pre_q}}}]}}],SessionStart=[{{matcher=\"*\",hooks=[{{type=\"command\",command={start_q}}}]}}],state={{{pre_key}={{trusted_hash={pre_hash}}},{start_key}={{trusted_hash={start_hash}}}}}}}",
        pre_q = toml_string(&pre),
        start_q = toml_string(&start),
        pre_key = key("pre_tool_use"),
        start_key = key("session_start"),
        pre_hash = toml_string(&codex_hook_hash("pre_tool_use", &pre)),
        start_hash = toml_string(&codex_hook_hash("session_start", &start)),
    )
}

/// What a sandboxed member's plugin commands may write: each granted plugin's
/// own state directory, the send-lock directory and its own temp directory. Never the directories
/// holding gate rules, shims, launch files or plugin secrets.
pub fn writable_roots(state: &StateDir, member: &str, granted: &[Granted]) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = granted
        .iter()
        .map(|g| state.plugin_dir(&g.plugin))
        .collect();
    roots.push(state.root.join("locks"));
    roots.push(state.member_tmp(member));
    roots.sort();
    roots.dedup();
    roots
}

/// Everything needed to start one member.
pub struct LaunchSpec {
    pub argv: Vec<String>,
    pub prompt: String,
    pub gate: Option<GateRules>,
    pub claude_settings: Option<serde_json::Value>,
}

#[allow(clippy::too_many_arguments)]
pub fn member_launch(
    brg: &Path,
    state: &StateDir,
    team: &Team,
    member: &Member,
    deploy: Option<&MemberDeploy>,
    posture: Posture,
    help: &BTreeMap<String, String>,
    settings_path: &Path,
) -> LaunchSpec {
    let granted = granted(team, member);
    let shim_dir = state.member_bin(&member.id);
    let prompt = opening_prompt(member, deploy, &granted, &shim_dir, posture, help);
    let mcp_tools = mcp_tool_names(member.vendor, member);
    let extra_args = deploy.map(|d| d.args.clone()).unwrap_or_default();
    let mut argv: Vec<String> = Vec::new();
    let mut gate = None;
    let mut settings = None;
    match member.vendor {
        Vendor::Claude => {
            argv.push("claude".into());
            if let Some(model) = &member.model {
                argv.extend(["--model".into(), model.name.clone()]);
                if let Some(effort) = &model.effort {
                    argv.extend(["--effort".into(), effort.clone()]);
                }
            }
            if posture == Posture::Locked {
                let rules = gate_rules(&granted, &shim_dir, mcp_tools.clone());
                let mut allowed: Vec<String> = Vec::new();
                for prefix in &rules.prefixes {
                    allowed.push(format!("Bash({}:*)", prefix.join(" ")));
                    let mut abs = prefix.clone();
                    if let Some(path) = rules.shims.get(&prefix[0]) {
                        abs[0] = path.clone();
                        allowed.push(format!("Bash({}:*)", abs.join(" ")));
                    }
                }
                allowed.extend(mcp_tools.iter().cloned());
                argv.extend([
                    "--restricted".into(),
                    "--tools".into(),
                    "Bash".into(),
                    "--permission-mode".into(),
                    "dontAsk".into(),
                ]);
                if !allowed.is_empty() {
                    argv.extend(["--allowedTools".into(), allowed.join(",")]);
                }
                if member.connectors.is_empty() {
                    argv.push("--strict-mcp-config".into());
                }
                argv.extend(["--settings".into(), settings_path.display().to_string()]);
                settings = Some(claude_settings(brg, state, &member.id));
                gate = Some(rules);
            }
            argv.extend(extra_args);
            argv.push("--".into());
            argv.push(prompt.clone());
        }
        Vendor::Codex => {
            argv.push("codex".into());
            if let Some(model) = &member.model {
                argv.extend(["-m".into(), model.name.clone()]);
                if let Some(effort) = &model.effort {
                    argv.extend([
                        "-c".into(),
                        format!("model_reasoning_effort={}", toml_string(effort)),
                    ]);
                }
            }
            if posture == Posture::Locked {
                argv.extend([
                    "-a".into(),
                    "never".into(),
                    "-s".into(),
                    "workspace-write".into(),
                    "-c".into(),
                    "sandbox_workspace_write.network_access=true".into(),
                    "-c".into(),
                    format!(
                        "sandbox_workspace_write.writable_roots=[{}]",
                        writable_roots(state, &member.id, &granted)
                            .iter()
                            .map(|p| toml_string(&p.display().to_string()))
                            .collect::<Vec<_>>()
                            .join(",")
                    ),
                    "-c".into(),
                    "allow_login_shell=false".into(),
                    // `/tmp` and $TMPDIR would otherwise be writable; the member
                    // gets its own TMPDIR inside the roots above instead.
                    "-c".into(),
                    "sandbox_workspace_write.exclude_slash_tmp=true".into(),
                    "-c".into(),
                    "sandbox_workspace_write.exclude_tmpdir_env_var=true".into(),
                    "-c".into(),
                    codex_hook_config(brg, state, &member.id),
                ]);
                gate = Some(gate_rules(&granted, &shim_dir, mcp_tools));
            }
            argv.extend(extra_args);
            argv.push(prompt.clone());
        }
    }
    LaunchSpec {
        argv,
        prompt,
        gate,
        claude_settings: settings,
    }
}

/// The program and arguments for a plugin's process.
pub fn process_argv(
    plugin: &Plugin,
    plugin_dir: Option<&Path>,
) -> Option<(Vec<String>, Option<PathBuf>)> {
    let process = plugin.process.as_ref()?;
    let mut argv = process.clone();
    if let Some(dir) = plugin_dir {
        let program = argv[0].strip_prefix("./").unwrap_or(&argv[0]).to_string();
        argv[0] = dir.join(program).display().to_string();
    }
    Some((argv, plugin_dir.map(Path::to_path_buf)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_hash_matches_spike_recipe() {
        // The spike computed this hash in Python for the same identity and
        // Codex accepted it; keep the two in step.
        let h = codex_hook_hash("pre_tool_use", "x");
        let expected_json = r#"{"event_name":"pre_tool_use","hooks":[{"async":false,"command":"x","timeout":600,"type":"command"}],"matcher":"*"}"#;
        let digest = Sha256::digest(expected_json.as_bytes());
        let want = format!(
            "sha256:{}",
            digest
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        assert_eq!(h, want);
    }

    #[test]
    fn codex_hook_config_is_one_inline_table() {
        let state = StateDir::from_root(Path::new("/s/dep-1"));
        let v = codex_hook_config(Path::new("/bin/brg"), &state, "triager");
        assert!(v.starts_with("hooks={PreToolUse=["));
        assert!(
            v.contains(r#""/<session-flags>/config.toml:pre_tool_use:0:0"={trusted_hash="sha256:"#)
        );
        assert!(v.contains("_gate start /s/dep-1 triager"));
    }
}
