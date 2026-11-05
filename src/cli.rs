//! Command-line interface for `brg` (also installed as `borgchart`).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use crate::deploy::DEPLOY_FILE;
use crate::runtime;
use crate::state::ENV_FILE;
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

#[derive(Args, Clone)]
struct Target {
    /// Deployment file (default: $BORGCHART_FILE, ./borgchart.yaml, or ./team.yaml on its own).
    #[arg(short, long, conflicts_with = "team", global = true)]
    file: Option<PathBuf>,
    /// Use a team directory on its own, without deployment values.
    #[arg(long, global = true)]
    team: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Check the team and deployment without starting anything.
    Validate {
        #[command(flatten)]
        target: Target,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Start the team, or one member (including a `start: false` one).
    Up {
        #[command(flatten)]
        target: Target,
        #[arg(long)]
        member: Option<String>,
    },
    /// Stop the team, or one member.
    Down {
        #[command(flatten)]
        target: Target,
        #[arg(long)]
        member: Option<String>,
    },
    /// Show who is running and what they are doing.
    Status {
        #[command(flatten)]
        target: Target,
        #[arg(long)]
        json: bool,
    },
    /// Open a member's live session (detach with your tmux prefix, then d).
    Attach {
        #[command(flatten)]
        target: Target,
        member: String,
    },
    /// Show a member's or plugin's recent output.
    Logs {
        #[command(flatten)]
        target: Target,
        member: String,
        /// How many lines of history.
        #[arg(short = 'n', long, default_value_t = 200)]
        lines: usize,
    },
    /// Paste text into a member and press Enter, as one submission.
    Send {
        #[command(flatten)]
        target: Target,
        member: String,
        /// The text (several words are joined with spaces).
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
        /// Send even when the member's state is unknown.
        #[arg(long)]
        force: bool,
        /// The sending member (for plugins relaying a member's message):
        /// brg checks that the target is that member's parent.
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        json: bool,
    },
    #[command(name = "_exec", hide = true)]
    Exec { state: PathBuf, window: String },
    #[command(name = "_run", hide = true)]
    Run {
        state: PathBuf,
        member: String,
        plugin: String,
        command: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    #[command(name = "_gate", hide = true)]
    Gate {
        event: String,
        state: PathBuf,
        member: String,
    },
}

fn resolve(target: &Target) -> Result<Input, ExitCode> {
    let input = match (&target.file, &target.team) {
        (Some(file), _) => Input::Deployment(file.clone()),
        (None, Some(team)) => Input::Team(team.clone()),
        (None, None) => match std::env::var_os(ENV_FILE) {
            Some(file) => Input::Deployment(PathBuf::from(file)),
            None if PathBuf::from(DEPLOY_FILE).exists() => {
                Input::Deployment(PathBuf::from(DEPLOY_FILE))
            }
            None if PathBuf::from(TEAM_FILE).exists() => Input::Team(PathBuf::from(".")),
            None => {
                eprintln!("brg: no {DEPLOY_FILE} or {TEAM_FILE} here; pass --file or --team");
                return Err(ExitCode::from(2));
            }
        },
    };
    Ok(input)
}

macro_rules! target_or_exit {
    ($t:expr) => {
        match resolve($t) {
            Ok(input) => input,
            Err(code) => return code,
        }
    };
}

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Validate { target, json } => run_validate(&target_or_exit!(&target), json),
        Command::Up { target, member } => {
            let input = target_or_exit!(&target);
            match runtime::up(&input, member.as_deref()) {
                Ok(outcome) => {
                    for m in &outcome.messages {
                        println!("{m}");
                    }
                    if outcome.failed {
                        ExitCode::from(1)
                    } else {
                        ExitCode::SUCCESS
                    }
                }
                Err(e) => {
                    eprintln!("brg: {e}");
                    ExitCode::from(1)
                }
            }
        }
        Command::Down { target, member } => {
            let input = target_or_exit!(&target);
            match runtime::down(&input, member.as_deref()) {
                Ok(messages) => {
                    for m in messages {
                        println!("{m}");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("brg: {e}");
                    ExitCode::from(1)
                }
            }
        }
        Command::Status { target, json } => {
            let input = target_or_exit!(&target);
            match runtime::status(&input) {
                Ok(rows) => {
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&rows).unwrap_or_default()
                        );
                    } else {
                        print!("{}", runtime::render_table(&rows));
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("brg: {e}");
                    ExitCode::from(1)
                }
            }
        }
        Command::Attach { target, member } => {
            let input = target_or_exit!(&target);
            match runtime::attach(&input, &member) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("brg: {e}");
                    ExitCode::from(1)
                }
            }
        }
        Command::Logs {
            target,
            member,
            lines,
        } => {
            let input = target_or_exit!(&target);
            match runtime::logs(&input, &member, lines) {
                Ok(text) => {
                    print!("{text}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("brg: {e}");
                    ExitCode::from(1)
                }
            }
        }
        Command::Send {
            target,
            member,
            text,
            force,
            from,
            json,
        } => {
            let input = target_or_exit!(&target);
            let text = text.join(" ");
            let result = runtime::send(&input, &member, &text, force, from.as_deref());
            if json {
                let out = match &result {
                    Ok(m) => serde_json::json!({"ok": true, "code": 0, "message": m}),
                    Err(e) => {
                        serde_json::json!({"ok": false, "code": e.code(), "message": e.message()})
                    }
                };
                println!("{out}");
            }
            match result {
                Ok(m) => {
                    if !json {
                        println!("{m}");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    if !json {
                        eprintln!("brg: {}", e.message());
                    }
                    ExitCode::from(e.code())
                }
            }
        }
        Command::Exec { state, window } => crate::exec::exec_main(&state, &window),
        Command::Run {
            state,
            member,
            plugin,
            command,
            args,
        } => crate::exec::run_main(&state, &member, &plugin, &command, &args),
        Command::Gate {
            event,
            state,
            member,
        } => crate::gate::hook_main(&event, &state, &member),
    }
}

fn run_validate(input: &Input, json: bool) -> ExitCode {
    let (report, validated) = validate(input);
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
