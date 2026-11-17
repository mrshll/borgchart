//! Bounded gh API calls, pinned to the explicitly configured machine login.
use crate::config::{Entry, valid_login};
use borgchart::{bounded_process, bus::time::format_utc};
use serde_json::Value;
use std::{
    fmt,
    process::Command,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

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
            Self::RateLimited(reset) => write!(
                f,
                "GitHub rate limited until {}; wait for polling to resume",
                format_utc(*reset)
            ),
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
    Owner(&'a str),
    Issues {
        entry: &'a Entry,
        since: SystemTime,
        page: u32,
    },
}
impl Rest<'_> {
    pub fn path(&self) -> Result<String, Error> {
        match self {
            Self::Machine => Ok("user".into()),
            Self::Owner(login) if valid_login(login) => Ok(format!("users/{login}")),
            Self::Owner(_) => Err(Error::Other(
                "set GITHUB_OWNER to a valid GitHub login".into(),
            )),
            Self::Issues { entry, since, page } if *page > 0 => Ok(format!(
                "{}/issues?{}state=all&sort=updated&direction=asc&since={}&per_page=100&page={page}",
                entry.api_path(),
                if entry.is_org() { "filter=all&" } else { "" },
                format_utc(*since)
            )),
            _ => Err(Error::Other("use a positive GitHub page number".into())),
        }
    }
}
/// A narrow seam for discovery and identity checks; process tests exercise Gh itself.
pub trait Api {
    fn get(&self, request: Rest<'_>) -> Result<Value, Error>;
}
pub struct Gh;
impl Gh {
    fn call(&self, args: &[String]) -> Result<Value, Error> {
        if std::env::var_os("GH_CONFIG_DIR").is_none_or(|s| s.is_empty()) {
            return Err(Error::Other(
                "set GH_CONFIG_DIR to the machine account's gh configuration and restart".into(),
            ));
        }
        let mut command = Command::new("gh");
        command.args(["api", "--include"]).args(args);
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
    pub fn graphql(&self, query: &str, variables: &[(&str, &str)]) -> Result<Value, Error> {
        let mut args = vec!["graphql".into(), "-f".into(), format!("query={query}")];
        for (name, value) in variables {
            if value.starts_with('@')
                || ["{owner}", "{repo}", "{branch}"]
                    .iter()
                    .any(|pattern| value.contains(pattern))
            {
                return Err(Error::Other("pass literal GraphQL values; gh file and repository substitutions are not allowed".into()));
            }
            if name.is_empty()
                || !name.bytes().enumerate().all(|(i, b)| {
                    b.is_ascii_alphabetic() || b == b'_' || (i > 0 && b.is_ascii_digit())
                })
            {
                return Err(Error::Other(
                    "use GraphQL variable names containing letters, digits and underscores".into(),
                ));
            }
            args.extend(["-F".into(), format!("{name}={value}")]);
        }
        self.call(&args)
    }
}
impl Api for Gh {
    fn get(&self, request: Rest<'_>) -> Result<Value, Error> {
        self.call(&["--method".into(), "GET".into(), request.path()?])
    }
}

fn decode(stdout: &[u8], stderr: &[u8], success: bool, now: SystemTime) -> Result<Value, Error> {
    let text = String::from_utf8_lossy(stdout);
    let normalized = text.replace("\r\n", "\n");
    let (headers, body) = normalized.split_once("\n\n").unwrap_or(("", &normalized));
    let status = headers
        .lines()
        .next()
        .and_then(|s| s.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok());
    let header = |name: &str| {
        headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.trim())
    };
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let graphql_limited = value["errors"].as_array().is_some_and(|errors| {
        errors
            .iter()
            .any(|e| e["type"] == "RATE_LIMITED" || e["extensions"]["type"] == "RATE_LIMITED")
    });
    if status == Some(429)
        || (status == Some(403)
            && (header("x-ratelimit-remaining") == Some("0") || header("retry-after").is_some()))
        || graphql_limited
    {
        let reset = header("x-ratelimit-reset")
            .and_then(|s| s.parse().ok())
            .and_then(|seconds| UNIX_EPOCH.checked_add(Duration::from_secs(seconds)))
            .or_else(|| {
                value["data"]["rateLimit"]["resetAt"]
                    .as_str()
                    .and_then(borgchart::bus::time::parse_utc)
            })
            .unwrap_or(
                now + Duration::from_secs(
                    header("retry-after")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(60),
                ),
            );
        return Err(Error::RateLimited(reset.max(now + Duration::from_secs(1))));
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
    if status == Some(404) {
        return Err(Error::NotFound);
    }
    if !success
        || status.is_some_and(|s| s >= 400)
        || value["errors"]
            .as_array()
            .is_some_and(|errors| !errors.is_empty())
    {
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
    #[test]
    fn api_errors_are_actionable_and_never_expose_response_text() {
        let now = UNIX_EPOCH + Duration::from_secs(100);
        for (response, expected) in [
            (
                "HTTP/2.0 401 Unauthorized\n\n{\"message\":\"private text\"}",
                Error::NotAuthenticated,
            ),
            ("HTTP/2.0 404 Not Found\n\n{}", Error::NotFound),
            (
                "HTTP/2.0 403 Forbidden\nX-Ratelimit-Remaining: 0\nX-Ratelimit-Reset: 200\n\n{}",
                Error::RateLimited(UNIX_EPOCH + Duration::from_secs(200)),
            ),
            (
                "HTTP/2.0 200 OK\n\n{\"errors\":[{\"type\":\"RATE_LIMITED\"}],\"data\":{\"rateLimit\":{\"resetAt\":\"1970-01-01T00:03:20Z\"}}}",
                Error::RateLimited(UNIX_EPOCH + Duration::from_secs(200)),
            ),
        ] {
            let error = decode(response.as_bytes(), b"private text", false, now).unwrap_err();
            assert_eq!(error, expected);
            assert!(!error.to_string().contains("private text"));
        }
        assert!(
            decode(b"bad", b"private text", false, now)
                .unwrap_err()
                .to_string()
                .contains("retry")
        );
    }
}
