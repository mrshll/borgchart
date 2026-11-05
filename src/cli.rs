//! Command-line interface for `brg` (also installed as `borgchart`).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::deploy::DEPLOY_FILE;
use crate::team::TEAM_FILE;
use crate::validate::{Input, validate};

#[derive(Parser)]
#[command(
    name = "brg",
    version,
    about = "Run teams of coding agents from plain files"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check the team and deployment without starting anything.
    Validate {
        /// Deployment file (default: ./borgchart.yaml, or ./team.yaml on its own).
        #[arg(short, long, conflicts_with = "team")]
        file: Option<PathBuf>,
        /// Validate a team directory on its own, without deployment values.
        #[arg(long)]
        team: Option<PathBuf>,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
}

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Validate { file, team, json } => run_validate(file, team, json),
    }
}

fn run_validate(file: Option<PathBuf>, team: Option<PathBuf>, json: bool) -> ExitCode {
    let input = match (file, team) {
        (Some(file), _) => Input::Deployment(file),
        (None, Some(team)) => Input::Team(team),
        (None, None) if PathBuf::from(DEPLOY_FILE).exists() => {
            Input::Deployment(PathBuf::from(DEPLOY_FILE))
        }
        (None, None) if PathBuf::from(TEAM_FILE).exists() => Input::Team(PathBuf::from(".")),
        (None, None) => {
            eprintln!("brg: no {DEPLOY_FILE} or {TEAM_FILE} here; pass --file or --team");
            return ExitCode::from(2);
        }
    };
    let (report, validated) = validate(&input);
    if json {
        let out = serde_json::json!({
            "ok": !report.has_errors(),
            "errors": report.errors().collect::<Vec<_>>(),
            "warnings": report.warnings().collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    } else {
        for diag in &report.diags {
            eprintln!("{diag}");
        }
        if let Some(v) = &validated {
            let warnings = report.warnings().count();
            println!(
                "ok: {} member(s), {} plugin(s){}",
                v.team.members.len(),
                v.team.plugins.len(),
                if warnings > 0 {
                    format!(", {warnings} warning(s)")
                } else {
                    String::new()
                }
            );
        }
    }
    if report.has_errors() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
