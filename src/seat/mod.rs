//! Built-in quota check. Explicit vendor/model only; never launches a member.
pub mod cache;
pub mod context;
pub mod policy;
pub mod protocol;

use clap::Subcommand;
use policy::Provider;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime};

#[derive(Subcommand)]
pub enum Command {
    /// Check subscription quota for exactly this vendor/model; go=0, wait=3.
    Route {
        /// Subscription vendor: claude or codex.
        #[arg(value_parser = parse_provider)]
        vendor: Provider,
        /// Explicit model name without effort; no fallback model is selected.
        #[arg(value_parser = parse_model)]
        model: String,
        /// Refresh a successful reading; failure backoff still applies.
        #[arg(long)]
        refresh: bool,
    },
}
fn parse_provider(s: &str) -> Result<Provider, String> {
    match s {
        "claude" => Ok(Provider::Claude),
        "codex" => Ok(Provider::Codex),
        _ => Err("expected claude or codex".into()),
    }
}
fn parse_model(s: &str) -> Result<String, String> {
    if s.is_empty()
        || s.len() > 128
        || !s.as_bytes()[0].is_ascii_alphanumeric()
        || !s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err("model must be one explicit model identifier".into());
    }
    Ok(s.to_owned())
}
fn cache_path(config: &crate::config::Config) -> Option<PathBuf> {
    if let Some(path) = &config.seat_cache_dir {
        return std::path::absolute(path).ok();
    }
    let root = match std::env::var_os("XDG_CACHE_HOME") {
        Some(root) => PathBuf::from(root),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".cache"),
    };
    root.is_absolute().then(|| root.join("borgchart/seat-v2"))
}
fn wait(reason: &str) -> ExitCode {
    println!("wait: {reason}");
    ExitCode::from(3)
}
pub fn run(command: Command) -> ExitCode {
    let Command::Route {
        vendor,
        model,
        refresh,
    } = command;
    let mut report = crate::diag::Report::default();
    let Some(config) = crate::config::load(&mut report) else {
        return wait("invalid brg configuration");
    };
    if config.auth == crate::config::Auth::ApiKey {
        return wait("subscription quota cannot authorize API-key spending");
    }
    let Some(path) = cache_path(&config) else {
        return wait(policy::UNAVAILABLE);
    };
    let Ok(context) = context::Context::discover(vendor) else {
        return wait(policy::UNAVAILABLE);
    };
    let reading = cache::snapshot(
        &path,
        vendor,
        refresh,
        Instant::now() + Duration::from_secs(22),
        || context.identity(),
        || {
            let raw = protocol::read(
                vendor,
                &context.executable,
                Instant::now() + Duration::from_secs(20),
            )
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidData))?;
            // Deliberately trust the returned Claude observation without reading
            // profiles. A silently stale vendor reply remains a documented risk.
            Ok((raw, SystemTime::now()))
        },
    );
    let assessment = policy::assess(vendor, &model, &reading, SystemTime::now());
    let mut reason = assessment.reason;
    for warning in assessment.warnings {
        reason.push_str("; ");
        reason.push_str(&warning);
    }
    if let Some(retry) = assessment.retry_at {
        reason.push_str("; check again at ");
        reason.push_str(&policy::stamp(retry));
    }
    if assessment.available {
        println!("go: {reason}");
        ExitCode::SUCCESS
    } else {
        wait(&reason)
    }
}
