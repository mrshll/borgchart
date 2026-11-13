//! `brg validate --probe`: ask each vendor CLI which MCP servers are configured,
//! and warn about connectors a member expects but the vendor doesn't list.
//!
//! Claude listing health-checks (and so starts) MCP servers. Codex listing does
//! not start stdio servers, but contacts HTTP servers to check sign-in status;
//! each unresponsive HTTP server can add a few seconds. It is also used at start
//! to disable explicitly empty connectors.
//! Neither checks which account a connector acts as, or what it may do.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;
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
pub fn parse_codex_list(text: &str) -> Result<BTreeSet<String>, String> {
    #[derive(serde::Deserialize)]
    struct Server {
        name: String,
    }
    serde_json::from_str::<Vec<Server>>(text)
        .map(|servers| servers.into_iter().map(|s| s.name).collect())
        .map_err(|_| "`codex mcp list --json` returned invalid server names".to_string())
}

pub(crate) fn list(
    vendor: Vendor,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    extra_args: &[String],
) -> Result<BTreeSet<String>, String> {
    let mut cmd = Command::new(vendor.as_str());
    cmd.args(["mcp", "list"]);
    if vendor == Vendor::Codex {
        cmd.arg("--json");
    }
    cmd.args(extra_args).env_clear().envs(env).current_dir(cwd);
    let out = crate::bounded_process::run(
        &mut cmd,
        None,
        crate::bounded_process::Limits {
            deadline: Instant::now() + PROBE_TIMEOUT,
            stdout_bytes: 1024 * 1024,
            stderr_bytes: 64 * 1024,
        },
    )
    .map_err(|e| format!("cannot run `{} mcp list`: {e}", vendor.as_str()))?;
    if !out.status.success() {
        return Err(format!("`{} mcp list` failed", vendor.as_str()));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    match vendor {
        Vendor::Claude => Ok(parse_claude_list(&text)),
        Vendor::Codex => parse_codex_list(&text),
    }
}

pub fn probe(v: &Validated, report: &mut Report) {
    let team_file = v.team.dir.join(crate::team::TEAM_FILE);
    for (id, member) in &v.team.roles {
        if member.connectors.is_empty() {
            continue;
        }
        // The team's folder isn't known here: ask from the template folder.
        let cwd = v.team.dir.clone();
        match list(member.vendor, &cwd, &crate::runtime::base_env(), &[]) {
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
        let names = parse_codex_list(text).unwrap();
        assert_eq!(
            names,
            BTreeSet::from(["example".to_string(), "other".to_string()])
        );
        assert!(parse_codex_list("not json").is_err());
    }
}
