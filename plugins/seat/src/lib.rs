//! Optional quota plugin. Explicit vendor/model only; never launches a member.
pub mod cache;
pub mod context;
pub mod policy;
pub mod protocol;

use borgchart::codex_models;
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
        /// Model name or Codex tier alias, without effort.
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
fn cache_path() -> Option<PathBuf> {
    let root = match std::env::var_os("XDG_CACHE_HOME") {
        Some(root) => PathBuf::from(root),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".cache"),
    };
    let path = root.join("borgchart/seat-v2");
    (path.is_absolute()
        && !path
            .components()
            .any(|part| part == std::path::Component::ParentDir))
    .then_some(path)
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
    let Some(path) = cache_path() else {
        return wait("invalid cache location; use an absolute path without parent components");
    };
    let Ok(context) = context::Context::discover(vendor) else {
        return wait(policy::UNAVAILABLE);
    };
    let reading = cache::snapshot(
        &path,
        vendor,
        refresh,
        Instant::now() + Duration::from_secs(30),
        || context.identity(),
        || {
            let models = if vendor == Provider::Codex {
                let list =
                    codex_models::listed(&context.executable, &borgchart::runtime::base_env())
                        .unwrap_or_default();
                codex_models::TIERS
                    .into_iter()
                    .filter_map(|tier| codex_models::newest(tier, &list).map(str::to_owned))
                    .collect()
            } else {
                Vec::new()
            };
            let raw = protocol::read(
                vendor,
                &context.executable,
                Instant::now() + Duration::from_secs(20),
            )
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidData));
            // Deliberately trust the returned Claude observation without reading
            // profiles. A silently stale vendor reply remains a documented risk.
            (models, raw.map(|value| (value, SystemTime::now())))
        },
    );
    let mut notice = None;
    let mut requested = model.as_str();
    if vendor == Provider::Codex {
        let newest = codex_models::newest(&model, &reading.models);
        if codex_models::alias(&model) {
            let Some(slug) = newest else {
                return wait("cannot resolve Codex tier; pin a full model name and retry");
            };
            requested = slug;
            notice = Some(format!("{model} resolves to {slug}"));
        } else if let Some(slug) = newest {
            notice = Some(format!("newer Codex model available for {model}: {slug}"));
        }
    }
    let assessment = policy::assess(vendor, requested, &reading, SystemTime::now());
    let mut reason = assessment.reason;
    for warning in assessment.warnings {
        reason.push_str("; ");
        reason.push_str(&warning);
    }
    if let Some(retry) = assessment.retry_at {
        reason.push_str("; check again at ");
        reason.push_str(&policy::stamp(retry));
    }
    if let Some(notice) = notice {
        reason.push('\n');
        reason.push_str(&notice);
    }
    if assessment.available {
        println!("go: {reason}");
        ExitCode::SUCCESS
    } else {
        wait(&reason)
    }
}
