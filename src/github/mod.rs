//! GitHub notifications as a pointer-only external bus producer.
pub mod api;
pub mod engine;
pub mod mentions;
pub mod model;
pub mod qualify;
pub mod reactions;
pub mod send;
use clap::{Args, Subcommand};
use model::{Config, State, now_ms, utc};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    process::ExitCode,
    time::{Duration, Instant},
};
#[derive(Args)]
pub struct Cli {
    /// Local account and policy JSON; otherwise GITHUB_INTAKE_CONFIG.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Action,
}
#[derive(Subcommand)]
pub enum Action {
    /// Explicitly initialize once-only state, refusing any existing state.
    Init {
        /// Earliest UTC source time to admit (RFC3339).
        #[arg(long)]
        since: String,
    },
    /// Show local health and counts without contacting GitHub.
    Status,
    /// Retry a held notification feed, or a held REPOSITORY_ID:NUMBER.
    Retry {
        /// Release this held REPOSITORY_ID:NUMBER instead of the feed.
        #[arg(long)]
        item: Option<String>,
    },
    /// Run one serialized, bounded poll and delivery cycle.
    Tick,
    /// Run bounded ticks as a brg-managed plugin process.
    Watch,
}
fn summary(s: &State, dropped: usize) -> Value {
    let held = !s.feed.holds.is_empty() || s.row["hold"].as_object().is_some_and(|m| !m.is_empty());
    let ok = !held && s.fault.as_deref().is_none_or(|s| s == "rate_limit");
    json!({"at":utc(),"ok":ok,"detail":s.fault.as_deref().unwrap_or(if held{"held"}else{"poll complete"}),"retrying":s.attempts.len(),"dropped":dropped,"pending":s.row["pending"].as_object().map_or(0,|m|m.len()),"held_items":s.row["dirty"].as_object().map_or(0,|m|m.values().filter(|v|v["held"]==true).count()),"due_ms":s.feed.due_ms.max(s.backoff_ms).max(s.reactions.read_retry_ms)})
}
pub fn tick(cfg: &Config, target: &send::Target) -> model::Result<Value> {
    let _lock = match model::lock(cfg) {
        Ok(l) => l,
        Err(e) if e == "busy" => {
            return Ok(
                json!({"at":utc(),"ok":true,"detail":"another tick is running","dropped":0}),
            );
        }
        Err(e) => return Err(e),
    };
    let mut s = model::load(cfg)?;
    if cfg.settings.mode == "off" {
        return Ok(json!({"at":utc(),"ok":false,"detail":"disabled","dropped":0}));
    }
    if now_ms()
        < s.backoff_ms
            .max(s.reactions.read_retry_ms)
            .max(s.feed.due_ms)
    {
        return Ok(summary(&s, 0));
    }
    let deadline = Instant::now() + Duration::from_secs(cfg.settings.max_seconds);
    let mut api = api::Api::new(cfg, deadline);
    s.fault = None;
    let mut committed = Vec::new();
    let mut dropped = 0;
    let work = (|| -> api::Result<()> {
        engine::verify_account(&mut api)?;
        engine::collect(&mut api, &mut s)?;
        if !s.feed.holds.contains("state_capacity") {
            let result = send::produce(&mut api, &mut s, target)?;
            committed = result.0;
            dropped = result.1;
        }
        Ok(())
    })();
    if let Err(e) = work {
        s.fault = Some(e.kind);
        s.backoff_ms = s.backoff_ms.max(e.retry_ms);
    }
    s.backoff_ms = s.backoff_ms.max(api.retry_ms);
    s.feed.due_ms = s.feed.due_ms.max(now_ms() + api.poll_seconds * 1000);
    s.last_tick = Some(utc());
    engine::persist(cfg, &mut s).map_err(|e| e.kind)?;
    if let Err(e) = reactions::send(&mut api, &mut s, &committed) {
        eprintln!("github-intake: reaction {}", e.kind);
    }
    Ok(summary(&s, dropped))
}
fn health(result: &Value) -> model::Result<()> {
    let path = PathBuf::from(std::env::var_os("BORGCHART_STATE_DIR").ok_or("health_directory")?);
    if !path.is_absolute() {
        return Err("health_directory".into());
    }
    let data = serde_json::to_vec(result).map_err(|_| "health_encode")?;
    crate::bus::fsx::replace(&path.join("health.json"), &data).map_err(|_| "health_write".into())
}
fn execute(cli: Cli) -> model::Result<Value> {
    let path = cli
        .config
        .or_else(|| std::env::var_os("GITHUB_INTAKE_CONFIG").map(PathBuf::from))
        .ok_or("config_path")?;
    if matches!(cli.command, Action::Watch) {
        // Binding is fixed at plugin start, just like brg's delivers_to env.
        let target = send::Target::environment()?;
        loop {
            let mut delay = 300;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let cfg = Config::read(&path)?;
                delay = cfg.settings.poll_seconds;
                tick(&cfg, &target)
            }))
            .unwrap_or_else(|_| Err("internal".into()));
            let result = result
                .unwrap_or_else(|kind| json!({"at":utc(),"ok":false,"detail":kind,"dropped":0}));
            if let Err(e) = health(&result) {
                eprintln!("github-intake: {e}");
            }
            if result["ok"] != true {
                eprintln!(
                    "github-intake: {}",
                    result["detail"].as_str().unwrap_or("internal")
                );
            }
            let wait = result["due_ms"]
                .as_u64()
                .unwrap_or(0)
                .saturating_sub(now_ms());
            std::thread::sleep(Duration::from_millis((delay * 1000).max(wait)));
        }
    }
    let cfg = Config::read(&path)?;
    match cli.command {
        Action::Init { since } => {
            model::initialize(&cfg, &since)?;
            Ok(json!({"ok":true,"detail":"initialized"}))
        }
        Action::Status => Ok(summary(&model::load(&cfg)?, 0)),
        Action::Retry { item } => {
            model::retry(&cfg, item.as_deref())?;
            Ok(json!({"ok":true,"detail":"retry scheduled"}))
        }
        Action::Tick => tick(&cfg, &send::Target::environment()?),
        Action::Watch => unreachable!(),
    }
}
pub fn run(cli: Cli) -> ExitCode {
    match execute(cli) {
        Ok(v) => {
            println!("{v}");
            if v["ok"] == true {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(e) => {
            eprintln!("github-intake: {e}");
            ExitCode::from(1)
        }
    }
}
