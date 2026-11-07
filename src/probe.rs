//! `brg validate --probe`: ask each vendor CLI which MCP servers are configured,
//! and warn about connectors a member expects but the vendor doesn't list.
//!
//! Listing health-checks (and so starts) your MCP servers, which is why this
//! only runs when asked. It can't tell whether a connector is signed in, which
//! account it acts as, or what it may do.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PROBE_TIMEOUT: Duration = Duration::from_secs(60);

use crate::diag::Report;
use crate::team::Vendor;
use crate::validate::Validated;

/// Server names from `claude mcp list` (text): lines like `name: target - ✓ Connected`.
pub fn parse_claude_list(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter(|l| l.contains(" - "))
        .filter_map(|l| l.split_once(": ").map(|(name, _)| name.trim().to_string()))
        .filter(|n| !n.is_empty())
        .collect()
}

/// Server names from `codex mcp list --json`.
pub fn parse_codex_list(text: &str) -> BTreeSet<String> {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|s| s.get("name").and_then(|n| n.as_str()).map(str::to_string))
        .collect()
}

fn list(vendor: Vendor, cwd: &Path) -> Result<BTreeSet<String>, String> {
    let mut cmd = Command::new(vendor.as_str());
    cmd.args(["mcp", "list"]);
    if vendor == Vendor::Codex {
        cmd.arg("--json");
    }
    // The small base environment only: plugin secrets in your shell don't
    // reach your MCP servers.
    let mut child = cmd
        .env_clear()
        .envs(crate::runtime::base_env())
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run `{} mcp list`: {e}", vendor.as_str()))?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
            break;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "`{} mcp list` took over {}s; skipped",
                vendor.as_str(),
                PROBE_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(match vendor {
        Vendor::Claude => parse_claude_list(&text),
        Vendor::Codex => parse_codex_list(&text),
    })
}

pub fn probe(v: &Validated, report: &mut Report) {
    let team_file = v.team.dir.join(crate::team::TEAM_FILE);
    for (id, member) in &v.team.members {
        if member.connectors.is_empty() {
            continue;
        }
        // The instance's folder isn't known here: ask from the team folder.
        let cwd = v.team.dir.clone();
        match list(member.vendor, &cwd) {
            Ok(names) => {
                for server in member.connectors.servers.keys() {
                    if !names.contains(server) {
                        report.warning(
                            Some(&team_file),
                            format!(
                                "member `{id}`: connector `{server}` is not in `{} mcp list` (run in {})",
                                member.vendor.as_str(),
                                cwd.display()
                            ),
                        );
                    }
                }
            }
            Err(e) => report.warning(Some(&team_file), format!("member `{id}`: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_list() {
        let text = "Checking MCP server health...\n\nexample-docs: npx -y example-docs-mcp@1.2.3 - ✓ Connected\nclaude.ai Example Mail: https://mail.example.invalid/mcp - ✓ Connected\nbroken: ./server - ✗ Failed to connect\n";
        let names = parse_claude_list(text);
        assert!(names.contains("example-docs"));
        assert!(names.contains("claude.ai Example Mail"));
        assert!(names.contains("broken"));
        assert!(!names.iter().any(|n| n.starts_with("Checking")));
    }

    #[test]
    fn codex_list() {
        let text = r#"[{"name":"example","enabled":true,"auth_status":"unsupported","transport":{}},{"name":"other","enabled":false}]"#;
        let names = parse_codex_list(text);
        assert_eq!(
            names,
            BTreeSet::from(["example".to_string(), "other".to_string()])
        );
        assert!(parse_codex_list("not json").is_empty());
    }
}
