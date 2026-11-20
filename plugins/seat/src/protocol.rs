//! Subscription usage requests through the vendor CLI.
use super::policy::Provider;
use borgchart::bounded_process::{Error, Limits};
pub use borgchart::vendor_protocol::MAX_OUTPUT;
use borgchart::vendor_protocol::{Query, exchange};
use serde_json::Value;
use std::io;
use std::process::Command;

/// Authentication stays in the vendor CLI. No arbitrary parent overrides or
/// member identity reach this process, and no response/error text is logged.
pub fn read(
    provider: Provider,
    executable: &std::path::Path,
    deadline: std::time::Instant,
) -> Result<Value, Error> {
    if !executable.is_absolute() {
        return Err(Error::Io(io::ErrorKind::InvalidInput));
    }
    let mut command = Command::new(executable);
    match provider {
        Provider::Codex => {
            command.args(["app-server", "--stdio"]);
        }
        Provider::Claude => {
            command.args([
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--no-session-persistence",
                "--setting-sources",
                "",
                "--settings",
                "{\"disableAllHooks\":true}",
                "--strict-mcp-config",
                "--mcp-config",
                "{\"mcpServers\":{}}",
            ]);
        }
    }
    command
        .env_clear()
        .envs(
            borgchart::runtime::base_env()
                .into_iter()
                .filter(|(key, _)| !key.starts_with("BORGCHART_")),
        )
        .current_dir(std::env::temp_dir());
    exchange(
        match provider {
            Provider::Codex => Query::CodexUsage,
            Provider::Claude => Query::ClaudeUsage,
        },
        &mut command,
        Limits {
            deadline,
            stdout_bytes: MAX_OUTPUT,
            stderr_bytes: MAX_OUTPUT,
        },
    )
}
