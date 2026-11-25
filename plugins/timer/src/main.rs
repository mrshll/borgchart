//! Run checks and schedules, and read evidence and outcomes.
use borgchart::plugin;
use brg_timer::{alarm, config, run, runs, state};
use clap::{
    Parser, Subcommand,
    error::{ContextKind, ContextValue, ErrorKind},
};
use state::{Snapshot, one_line};
use std::path::PathBuf;

const FILE_FIX: &str = "name the schedule file on the process line, for example `process: [brg-timer, run, checks.yaml]`";

#[derive(Parser)]
#[command(about = "Run checks and schedules, and read evidence and outcomes")]
struct Cli {
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Set, show or clear your check-in alarm
    Alarm {
        #[command(subcommand)]
        action: alarm::Action,
    },
    /// Validate checks and schedules without running commands or sending
    Validate { file: PathBuf },
    /// Run checks and schedules, waiting 30 seconds between ticks
    Run {
        file: PathBuf,
        #[arg(long)]
        once: bool,
    },
    /// Read check evidence or the latest scheduled run outcome
    Last { name: String },
    /// Tell the timer this scheduled run is finished
    Done,
    /// Send a check ID and reason (outage, alert, question, other) to your parent
    Promote { name: String, reason: String },
}
fn action(action: Action) -> Result<(), (i32, String)> {
    if let Action::Validate { file } = action {
        let config = config::load(&file).map_err(|e| (1, e))?;
        println!(
            "valid: {} checks, {} schedules",
            config.checks.len(),
            config.schedules.len()
        );
        return Ok(());
    }
    if let Action::Done = action {
        println!("{}", runs::done()?);
        return Ok(());
    }
    let store = plugin::State::from_env().map_err(|e| (1, e.to_string()))?;
    let (name, reason) = match action {
        Action::Validate { .. } | Action::Done => unreachable!(),
        Action::Alarm { action } => return alarm::action(&store, action).map_err(|e| (1, e)),
        Action::Run { file, once } => return run::run(&store, &file, once).map_err(|e| (1, e)),
        Action::Last { name } => (name, None),
        Action::Promote { name, reason } => (name, Some(reason)),
    };
    let snapshot = Snapshot::read(&store);
    if reason.is_none() && snapshot.schedules.get(&name).is_some_and(|s| s.run) {
        let line = snapshot
            .outcomes
            .get(&name)
            .ok_or((2, format!("no outcome yet for scheduled run {name}")))?;
        println!("{}", one_line(line));
        return Ok(());
    }
    let record = snapshot.checks.get(&name).ok_or_else(|| {
        (
            2,
            format!(
                "unknown check {name}; known checks: {}",
                if snapshot.checks.is_empty() {
                    "(none)".into()
                } else {
                    snapshot
                        .checks
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ),
        )
    })?;
    if let Some(reason) = reason {
        if !["outage", "alert", "question", "other"].contains(&reason.as_str()) {
            return Err((
                2,
                "use a known check and reason: outage, alert, question, other".into(),
            ));
        }
        let sender = std::env::var("BORGCHART_MEMBER").unwrap_or_default();
        let target = std::env::var("BORGCHART_PARENT").unwrap_or_default();
        if !config::matches("[a-z][a-z0-9_-]{0,79}", &sender)
            || !config::matches("[a-z][a-z0-9_-]{0,79}(?:/[a-z][a-z0-9_-]{0,79})*", &target)
        {
            return Err((
                2,
                "set BORGCHART_MEMBER and BORGCHART_PARENT to valid member IDs".into(),
            ));
        }
        let text = format!(
            "Timer promote from {sender}: check={name} reason={reason}. Look with brg-timer last {name}."
        );
        match plugin::send("timer", &target, &text, Some(&sender)) {
            Ok(output) => println!("{output}"),
            Err(message) => return Err((1, message)),
        }
    } else {
        println!("{}", plugin::UNTRUSTED_HEADER);
        for (label, value) in [
            ("command", serde_json::to_string(&record.command).unwrap()),
            ("last run", record.last_run.clone()),
            (
                "status/exit",
                format!(
                    "{} / {}",
                    if record.status.is_empty() {
                        "unknown"
                    } else {
                        &record.status
                    },
                    if record.exit_code.is_null() {
                        String::new()
                    } else {
                        record
                            .exit_code
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| record.exit_code.to_string())
                    }
                ),
            ),
            ("last stderr", record.stderr.clone()),
        ] {
            println!("--- {label}: {}", one_line(&value));
        }
        let previous = if record.status == "failing" {
            &record.good_output
        } else {
            &record.previous_output
        };
        for (label, text) in [
            ("current output", &record.output),
            ("previous distinct successful output", previous),
        ] {
            println!("--- {label}");
            print!(
                "{}",
                plugin::frame(
                    &text
                        .split('\n')
                        .map(one_line)
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            );
        }
    }
    Ok(())
}
fn main() {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            print!("{error}");
            return;
        }
        Err(error) => {
            let detail = if error.kind() == ErrorKind::MissingRequiredArgument
                && matches!(error.get(ContextKind::InvalidArg), Some(ContextValue::Strings(args)) if args == &["<FILE>"])
            {
                if std::env::args_os().nth(1).is_some_and(|arg| arg == "run") {
                    if let Ok(store) = plugin::State::from_env() {
                        run::startup_error(&store, FILE_FIX);
                    }
                    FILE_FIX.into()
                } else {
                    "name the schedule file: brg-timer validate <file>".into()
                }
            } else if error.kind() == ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand {
                "choose a command".into()
            } else {
                error
                    .to_string()
                    .lines()
                    .take_while(|l| !l.is_empty() && !l.starts_with("Usage:"))
                    .map(str::trim)
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            eprintln!("{}; run brg-timer --help", one_line(&detail));
            std::process::exit(2);
        }
    };
    if let Err((code, message)) = action(cli.command) {
        eprintln!("{}", one_line(&message));
        std::process::exit(code);
    }
}
