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
    about = "Run teams of coding agents from plain files",
    after_long_help = crate::bus::help::root()
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
        /// Also ask each vendor CLI which MCP connectors are configured. This
        /// starts your MCP servers to health-check them.
        #[arg(long)]
        probe: bool,
    },
    /// Start the team, or one member (including a `start: false` one).
    Up {
        #[command(flatten)]
        target: Target,
        /// Start only this member.
        #[arg(long)]
        member: Option<String>,
    },
    /// Stop the team, or one member.
    Down {
        #[command(flatten)]
        target: Target,
        /// Stop only this member.
        #[arg(long)]
        member: Option<String>,
    },
    /// Show who is running and what they are doing.
    Status {
        #[command(flatten)]
        target: Target,
        /// Print the rows as JSON.
        #[arg(long, conflicts_with_all = ["watch", "interactive"])]
        json: bool,
        /// Refresh the table every few seconds.
        #[arg(short, long, conflicts_with = "interactive")]
        watch: bool,
        /// Seconds between refreshes with --watch.
        #[arg(long, default_value_t = 2)]
        interval: u64,
        /// An interactive screen: j/k to move, Enter to attach, q to quit.
        #[arg(short, long)]
        interactive: bool,
    },
    /// Open a member's live session (detach with your tmux prefix, then d).
    Attach {
        #[command(flatten)]
        target: Target,
        /// The member to attach to.
        member: String,
    },
    /// Show a member's or plugin's recent output.
    Logs {
        #[command(flatten)]
        target: Target,
        /// The member, or a plugin process (`_<plugin>`).
        member: String,
        /// How many lines of history.
        #[arg(short = 'n', long, default_value_t = 200)]
        lines: usize,
    },
    /// Paste text into a member and press Enter, as one submission.
    Send {
        #[command(flatten)]
        target: Target,
        /// The member to type into.
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
        /// Print {ok, code, message} as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Publish, read and acknowledge bus records.
    Msg {
        #[command(flatten)]
        target: Target,
        #[command(subcommand)]
        command: crate::bus::cli::MsgCommand,
    },
    /// List records addressed to you (or `--member`/`--external`), oldest first.
    #[command(
        args_conflicts_with_subcommands = true,
        after_long_help = crate::bus::help::inbox()
    )]
    Inbox {
        #[command(flatten)]
        target: Target,
        #[command(flatten)]
        args: crate::bus::cli::InboxArgs,
    },
    /// List and show tasks folded from the team's messages.
    #[command(after_long_help = crate::bus::help::task())]
    Task {
        #[command(flatten)]
        target: Target,
        #[command(subcommand)]
        command: crate::bus::task_cli::TaskCommand,
    },
    /// Inspect and check the team's message storage.
    Bus {
        #[command(flatten)]
        target: Target,
        #[command(subcommand)]
        command: crate::bus::cli::BusCommand,
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
    let started = std::time::Instant::now();
    let args: Vec<_> = std::env::args_os().collect();
    // Recognize the wait even when clap rejects its flags. Usage errors
    // must back off too, or a broken background command can spin turns.
    // Over-matching (e.g. a file named `wait`) only delays an error, which
    // is harmless. Explicit help and external termination remain immediate.
    let background = args.iter().any(|a| a == "inbox")
        && args.iter().any(|a| a == "wait")
        && args
            .iter()
            .any(|a| a == "--background" || a.as_encoded_bytes().starts_with(b"--background="));
    let help = args.iter().any(|a| a == "--help" || a == "-h");
    let back_off = || {
        if background && !help {
            let minimum = crate::bus::store::background_wait_minimum();
            std::thread::sleep(minimum.saturating_sub(started.elapsed()));
        }
    };
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            if error.use_stderr() {
                back_off();
            }
            error.exit();
        }
    };
    let code = run(cli, started);
    if code != ExitCode::SUCCESS {
        back_off();
    }
    code
}

fn run(cli: Cli, started: std::time::Instant) -> ExitCode {
    match cli.command {
        Command::Validate {
            target,
            json,
            probe,
        } => run_validate(&target_or_exit!(&target), json, probe),
        Command::Up { target, member } => {
            let input = target_or_exit!(&target);
            match runtime::up(&input, member.as_deref()) {
                Ok(outcome) => {
                    print!("{}", outcome.tree);
                    for n in &outcome.notes {
                        println!("{n}");
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
        Command::Status {
            target,
            json,
            watch,
            interval,
            interactive,
        } => {
            let input = target_or_exit!(&target);
            if watch || interactive {
                let result = if interactive {
                    crate::tui::interactive(&input)
                } else {
                    crate::tui::watch(&input, std::time::Duration::from_secs(interval.max(1)))
                };
                return match result {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(e) => {
                        eprintln!("brg: {e}");
                        ExitCode::from(1)
                    }
                };
            }
            match runtime::status(&input) {
                Ok(rows) => {
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&rows).unwrap_or_default()
                        );
                    } else {
                        print!(
                            "{}",
                            runtime::render_status(&runtime::team_name(&input), &rows)
                        );
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
        Command::Msg { target, command } => {
            crate::bus::cli::msg(&target_or_exit!(&target), command)
        }
        Command::Inbox { target, args } => {
            crate::bus::cli::inbox(&target_or_exit!(&target), args, started)
        }
        Command::Task { target, command } => {
            crate::bus::task_cli::task(&target_or_exit!(&target), command)
        }
        Command::Bus { target, command } => {
            crate::bus::cli::bus(&target_or_exit!(&target), command)
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

fn run_validate(input: &Input, json: bool, probe: bool) -> ExitCode {
    let (mut report, validated) = validate(input);
    if probe && let Some(v) = &validated {
        crate::probe::probe(v, &mut report);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// Every visible argument of every visible command says what it is.
    #[test]
    fn every_argument_has_help() {
        fn walk(cmd: &clap::Command, path: &str, missing: &mut Vec<String>) {
            for arg in cmd.get_arguments() {
                let id = arg.get_id().as_str();
                if arg.is_hide_set() || id == "help" || id == "version" {
                    continue;
                }
                if arg.get_help().is_none() && arg.get_long_help().is_none() {
                    missing.push(format!("{path} {id}"));
                }
            }
            for sub in cmd.get_subcommands().filter(|s| !s.is_hide_set()) {
                walk(sub, &format!("{path} {}", sub.get_name()), missing);
            }
        }
        let mut missing = Vec::new();
        walk(&Cli::command(), "brg", &mut missing);
        assert!(missing.is_empty(), "arguments without help: {missing:?}");
    }
}
