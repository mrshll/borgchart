//! Run change-gated checks and read their evidence.
use borgchart::plugin;
use brg_checks::{config, run, state};
use clap::{Parser, Subcommand, error::ErrorKind};
use state::{Snapshot, one_line};

#[derive(Parser)]
#[command(about = "Run change-gated checks and read their evidence")]
struct Cli {
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Run checks, waiting 30 seconds between ticks
    Run {
        #[arg(long)]
        once: bool,
    },
    /// Read the latest and previous evidence without waiting for a check
    Last { name: String },
    /// Send a check ID and reason (outage, alert, question, other) to your parent
    Promote { name: String, reason: String },
}
fn action(action: Action) -> Result<(), (i32, String)> {
    let store = plugin::State::from_env().map_err(|e| (1, e.to_string()))?;
    let (name, reason) = match action {
        Action::Run { once } => return run::run(&store, once).map_err(|e| (1, e)),
        Action::Last { name } => (name, None),
        Action::Promote { name, reason } => (name, Some(reason)),
    };
    let snapshot = Snapshot::read(&store);
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
            "Checks promote from {sender}: check={name} reason={reason}. Look with brg-checks last {name}."
        );
        match plugin::send("checks", &target, &text, Some(&sender)) {
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
            let detail = if error.kind() == ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand {
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
            eprintln!("{}; run brg-checks --help", one_line(&detail));
            std::process::exit(2);
        }
    };
    if let Err((code, message)) = action(cli.command) {
        eprintln!("{}", one_line(&message));
        std::process::exit(code);
    }
}
