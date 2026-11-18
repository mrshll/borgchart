//! Bounded gh API calls, pinned to the explicitly configured machine login.
use crate::config::Entry;
use borgchart::{bounded_process, bus::time::format_utc};
use serde_json::Value;
use std::{
    fmt,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant, SystemTime},
};

/// How long a rate-limited answer pauses all polling.
const RATE_LIMIT_PAUSE: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    RateLimited(SystemTime),
    NotAuthenticated,
    NotFound,
    Other(String),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RateLimited(retry) => {
                write!(f, "GitHub rate limited; retrying at {}", format_utc(*retry))
            }
            Self::NotAuthenticated => f.write_str(
                "GitHub is not authenticated; configure the machine account's gh login and restart",
            ),
            Self::NotFound => f.write_str(
                "GitHub item not found or not visible to the machine account; check its access",
            ),
            Self::Other(detail) => f.write_str(detail),
        }
    }
}
impl std::error::Error for Error {}

pub enum Rest<'a> {
    Machine,
    /// A login already checked by the configuration loader.
    Owner(&'a str),
    Issues {
        entry: &'a Entry,
        since: SystemTime,
        page: u32,
    },
}
impl Rest<'_> {
    pub fn path(&self) -> String {
        match self {
            Self::Machine => "user".into(),
            Self::Owner(login) => format!("users/{login}"),
            Self::Issues { entry, since, page } => format!(
                "{}/issues?{}state=all&sort=updated&direction=asc&since={}&per_page=100&page={page}",
                entry.api_path(),
                if entry.is_org() { "filter=all&" } else { "" },
                format_utc(*since)
            ),
        }
    }
}
/// API boundary for identity, discovery and timelines; process tests exercise Gh itself.
pub trait Api {
    fn configure(&mut self, _profile: &Path) {}
    fn get(&self, request: Rest<'_>) -> Result<Value, Error>;
    fn graphql(
        &self,
        query: &str,
        strings: &[(&str, &str)],
        integers: &[(&str, u64)],
    ) -> Result<Value, Error>;
}
#[derive(Default)]
pub struct Gh(pub PathBuf);
impl Gh {
    fn call(&self, args: &[String]) -> Result<Value, Error> {
        if self.0.as_os_str().is_empty() {
            return Err(Error::Other(
                "set gh_config_dir in GITHUB_CONFIG to the machine account's gh configuration"
                    .into(),
            ));
        }
        let mut command = Command::new("gh");
        command
            .env("GH_CONFIG_DIR", &self.0)
            .args(["api", "--include"])
            .args(args);
        for name in [
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "GH_ENTERPRISE_TOKEN",
            "GITHUB_ENTERPRISE_TOKEN",
            "GH_HOST",
            "GH_REPO",
            "GH_DEBUG",
        ] {
            command.env_remove(name);
        }
        let output = bounded_process::run(
            &mut command,
            None,
            bounded_process::Limits {
                deadline: Instant::now() + Duration::from_secs(30),
                stdout_bytes: 8 << 20,
                stderr_bytes: 64 << 10,
            },
        )
        .map_err(|error| {
            Error::Other(match error {
                bounded_process::Error::Deadline => {
                    "gh did not answer within 30 s; check connectivity and retry".into()
                }
                bounded_process::Error::OutputLimit(_) => {
                    "gh output exceeded its limit; reduce the response and retry".into()
                }
                _ => "cannot run gh; check PATH, executable permissions and connectivity".into(),
            })
        })?;
        decode(
            &output.stdout,
            &output.stderr,
            output.status.success(),
            SystemTime::now(),
        )
    }
}
impl Api for Gh {
    fn configure(&mut self, profile: &Path) {
        self.0 = profile.into();
    }
    /// Strings go as `-f`, which gh passes verbatim; only integers use `-F`,
    /// so gh's file and repository substitutions never apply.
    fn graphql(
        &self,
        query: &str,
        strings: &[(&str, &str)],
        integers: &[(&str, u64)],
    ) -> Result<Value, Error> {
        let mut args = vec!["graphql".into(), "-f".into(), format!("query={query}")];
        for (name, value) in strings {
            args.extend(["-f".into(), format!("{name}={value}")]);
        }
        for (name, value) in integers {
            args.extend(["-F".into(), format!("{name}={value}")]);
        }
        self.call(&args)
    }
    fn get(&self, request: Rest<'_>) -> Result<Value, Error> {
        self.call(&["--method".into(), "GET".into(), request.path()])
    }
}

fn decode(stdout: &[u8], stderr: &[u8], success: bool, now: SystemTime) -> Result<Value, Error> {
    let text = String::from_utf8_lossy(stdout).replace("\r\n", "\n");
    let (headers, body) = text.split_once("\n\n").unwrap_or(("", &text));
    let headers = headers.to_ascii_lowercase();
    let status = headers
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok());
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let errors = value["errors"].as_array();
    if status == Some(429)
        || (status == Some(403)
            && (headers.contains("x-ratelimit-remaining: 0") || headers.contains("retry-after:")))
        || errors.is_some_and(|e| e.iter().any(|e| e["type"] == "RATE_LIMITED"))
    {
        return Err(Error::RateLimited(now + RATE_LIMIT_PAUSE));
    }
    // Stderr is used only for classification when gh failed before an HTTP response.
    let unauthenticated = !success && headers.is_empty() && {
        let text = String::from_utf8_lossy(stderr).to_ascii_lowercase();
        text.contains("gh auth login")
            || text.contains("not logged")
            || text.contains("authentication")
    };
    if status == Some(401) || unauthenticated {
        return Err(Error::NotAuthenticated);
    }
    if status == Some(404) || errors.is_some_and(|e| e.iter().any(|e| e["type"] == "NOT_FOUND")) {
        return Err(Error::NotFound);
    }
    if !success || status.is_some_and(|s| s >= 400) || errors.is_some_and(|e| !e.is_empty()) {
        return Err(Error::Other("GitHub API request failed; check machine account permissions and connectivity, then retry".into()));
    }
    if value.is_null() {
        return Err(Error::Other(
            "gh returned invalid JSON; check gh and retry".into(),
        ));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;
    #[test]
    fn api_errors_are_actionable_and_never_expose_response_text() {
        let now = UNIX_EPOCH + Duration::from_secs(100);
        let limited = Error::RateLimited(now + RATE_LIMIT_PAUSE);
        for (response, expected) in [
            (
                "HTTP/2.0 401 Unauthorized\n\n{\"message\":\"private text\"}",
                Error::NotAuthenticated,
            ),
            ("HTTP/2.0 404 Not Found\n\n{}", Error::NotFound),
            (
                "HTTP/2.0 403 Forbidden\r\nX-Ratelimit-Remaining: 0\r\n\r\n{\"message\":\"private text\"}",
                limited.clone(),
            ),
            (
                "HTTP/2.0 403 Forbidden\nRetry-After: 60\n\n{}",
                limited.clone(),
            ),
            (
                "HTTP/2.0 200 OK\n\n{\"errors\":[{\"type\":\"RATE_LIMITED\"}]}",
                limited.clone(),
            ),
        ] {
            let error = decode(response.as_bytes(), b"private text", false, now).unwrap_err();
            assert_eq!(error, expected);
            assert!(!error.to_string().contains("private text"));
        }
        // A plain permission refusal is not a rate limit.
        assert!(matches!(
            decode(
                b"HTTP/2.0 403 Forbidden\nX-Ratelimit-Remaining: 4999\n\n{}",
                b"",
                false,
                now
            ),
            Err(Error::Other(_))
        ));
        assert!(
            decode(b"bad", b"private text", false, now)
                .unwrap_err()
                .to_string()
                .contains("retry")
        );
    }
}
