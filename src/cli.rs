//! Command-line interface for `brg` (also installed as `borgchart`).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::address::{self, Instance, Target};
use crate::runtime;
use crate::validate::validate;

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

#[derive(Subcommand)]
enum Command {
    /// Check a team folder and brg's configuration without starting anything.
    Validate {
        /// The team: its name in the search paths, or a path to its folder
        /// (starting with `/`, `./`, `../` or `~/`).
        team: String,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
        /// Also ask each vendor CLI which MCP connectors are configured. This
        /// starts your MCP servers to health-check them.
        #[arg(long)]
        probe: bool,
    },
    /// Make something new: an instance of a team working in a folder
    /// (`create <team> <folder>`), or a member of a role
    /// (`create <instance>/<role>`). Either is started.
    Create {
        /// `<team>` (a name, or a path starting with `/`, `./`, `../` or
        /// `~/`), or `<instance>/<role>`.
        target: String,
        /// The instance's working folder (required for a team; it must exist).
        folder: Option<String>,
    },
    /// Bring an instance to what should be running, or start one member.
    Start {
        /// `<instance>`, or `<instance>/<member>`.
        target: String,
        /// Point a stopped instance at its team folder's new place.
        #[arg(long)]
        team: Option<PathBuf>,
    },
    /// Stop an instance, or one member. Stopping a stopped one changes nothing.
    Stop {
        /// `<instance>`, or `<instance>/<member>`.
        target: String,
    },
    /// Remove a stopped instance: its messages, notes, state and registration.
    Delete {
        /// The instance.
        instance: String,
        /// Delete even though it holds open work (or its messages can't be
        /// read). Never deletes a running instance.
        #[arg(long)]
        force: bool,
    },
    /// List the secrets a team's plugins need, and whether each is set in
    /// your environment. Prints no values and writes nothing.
    Secret {
        /// The team: its name in the search paths, or a path to its folder.
        team: String,
        /// Print {secrets: [{plugin, name, set}]} as JSON.
        #[arg(long)]
        json: bool,
    },
    /// List the teams in the search paths, each with its instances.
    Ls {
        /// Print the list as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show who is running and what they are doing: one instance, or every one.
    Status {
        /// The instance (default: every instance).
        instance: Option<String>,
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
        /// The member: `<instance>/<member>`.
        member: String,
    },
    /// Show a member's or plugin's recent output.
    Logs {
        /// The member (`<instance>/<member>`), or a plugin process
        /// (`<instance>/_<plugin>`).
        member: String,
        /// How many lines of history.
        #[arg(short = 'n', long, default_value_t = 200)]
        lines: usize,
    },
    /// Paste text into a member and press Enter, as one submission.
    Send {
        /// The member to type into: `<instance>/<member>`.
        member: String,
        /// The text (several words are joined with spaces).
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
        /// Send even when the member's state is unknown.
        #[arg(long)]
        force: bool,
        /// The sending member, by ID (for plugins relaying a member's
        /// message): brg checks that the target is that member's parent.
        #[arg(long)]
        from: Option<String>,
        /// Print {ok, code, message} as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Publish, read and acknowledge bus records.
    Msg {
        /// The instance (outside a member; inside one, only its own).
        #[arg(long, global = true)]
        instance: Option<String>,
        #[command(subcommand)]
        command: crate::bus::cli::MsgCommand,
    },
    /// List records addressed to you (or `--member`/`--external`), oldest first.
    #[command(
        args_conflicts_with_subcommands = true,
        after_long_help = crate::bus::help::inbox()
    )]
    Inbox {
        /// The instance (outside a member; inside one, only its own).
        #[arg(long, global = true)]
        instance: Option<String>,
        #[command(flatten)]
        args: crate::bus::cli::InboxArgs,
    },
    /// Inspect and check the instance's message storage.
    Bus {
        /// The instance (outside a member; inside one, only its own).
        #[arg(long, global = true)]
        instance: Option<String>,
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

/// Print an error and give the failing exit code.
fn fail(e: impl std::fmt::Display) -> ExitCode {
    eprintln!("brg: {e}");
    ExitCode::from(1)
}

macro_rules! or_fail {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => return fail(e),
        }
    };
}

/// An instance and one member or role in it, from `<instance>/<name>` (or,
/// inside a member, a bare name).
fn instance_and_name(arg: &str) -> Result<(Instance, String), String> {
    match address::resolve(arg)? {
        (inst, Target::Member(m) | Target::Role(m)) => Ok((inst, m)),
        (inst, Target::Instance) => Err(format!(
            "`{arg}` is an instance; name a member in it: {}/<member> (its members: {})",
            inst.id,
            address::members_list(&inst)
        )),
    }
}

fn print_outcome(outcome: runtime::UpOutcome) -> ExitCode {
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
        Command::Validate { team, json, probe } => run_validate(&team, json, probe),
        Command::Create { target, folder } => {
            let team_like = crate::team::is_path(&target) || !target.contains('/');
            let result = if team_like {
                match folder {
                    Some(folder) => runtime::create(&target, &folder),
                    None => Err(format!(
                        "give the instance's working folder: brg create {target} <folder>"
                    )),
                }
            } else {
                match address::resolve(&target) {
                    Ok(_) if folder.is_some() => Err(format!(
                        "`{target}` names a role, which takes no folder: its member works in the instance's folder"
                    )),
                    Ok((inst, Target::Role(role))) => runtime::create_member(inst, &role),
                    Ok((inst, Target::Member(m))) => Err(format!(
                        "`create` takes a role, not a member; `{m}` exists already: brg start {}",
                        inst.at(&m)
                    )),
                    Ok((inst, Target::Instance)) => Err(format!(
                        "`create` makes an instance from a team, or a member from `{}/<role>`",
                        inst.id
                    )),
                    Err(e) => Err(e),
                }
            };
            print_outcome(or_fail!(result))
        }
        Command::Start { target, team } => {
            let result = match or_fail!(address::resolve(&target)) {
                (inst, Target::Instance) => runtime::start(inst, None, team.as_deref()),
                (inst, Target::Member(m) | Target::Role(m)) => {
                    runtime::start(inst, Some(&m), team.as_deref())
                }
            };
            print_outcome(or_fail!(result))
        }
        Command::Stop { target } => {
            let result = match or_fail!(address::resolve(&target)) {
                (inst, Target::Instance) => runtime::stop(&inst, None),
                (inst, Target::Member(m) | Target::Role(m)) => runtime::stop(&inst, Some(&m)),
            };
            for m in or_fail!(result) {
                println!("{m}");
            }
            ExitCode::SUCCESS
        }
        Command::Delete { instance, force } => {
            let inst = or_fail!(address::instance(&instance));
            for m in or_fail!(runtime::delete(inst, force)) {
                println!("{m}");
            }
            ExitCode::SUCCESS
        }
        Command::Secret { team, json } => {
            let secrets = or_fail!(runtime::secrets(&team));
            if json {
                let rows: Vec<serde_json::Value> = secrets
                    .iter()
                    .map(|(p, n, set)| serde_json::json!({"plugin": p, "name": n, "set": set}))
                    .collect();
                println!("{}", serde_json::json!({ "secrets": rows }));
            } else if secrets.is_empty() {
                println!("the team's plugins need no secrets");
            } else {
                for (plugin, name, set) in &secrets {
                    println!("{plugin}: {name}  {}", if *set { "set" } else { "not set" });
                }
            }
            ExitCode::SUCCESS
        }
        Command::Ls { json } => {
            let listed = or_fail!(runtime::ls());
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&listed).unwrap_or_default()
                );
            } else {
                print!("{}", runtime::render_ls(&listed));
            }
            ExitCode::SUCCESS
        }
        Command::Status {
            instance,
            json,
            watch,
            interactive,
            interval,
        } => {
            let one = match &instance {
                Some(id) => match or_fail!(address::resolve(id)) {
                    (inst, Target::Instance) => Some(inst),
                    (inst, _) => {
                        return fail(format!(
                            "`status` takes an instance; `{id}` is in instance `{}`",
                            inst.id
                        ));
                    }
                },
                None => None,
            };
            if watch || interactive {
                let Some(inst) = one else {
                    return fail(format!(
                        "name the instance to watch: brg status <instance> -w; {}",
                        address::known()
                    ));
                };
                let result = if interactive {
                    crate::tui::interactive(&inst)
                } else {
                    crate::tui::watch(&inst, std::time::Duration::from_secs(interval.max(1)))
                };
                return match result {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(e) => fail(e),
                };
            }
            let all: Vec<Instance> = match one {
                Some(inst) => vec![inst],
                None => {
                    let registry = or_fail!(crate::registry::load());
                    registry
                        .instances
                        .keys()
                        .filter_map(|id| address::instance(id).ok())
                        .collect()
                }
            };
            let mut code = ExitCode::SUCCESS;
            let mut rows = Vec::new();
            let mut text = String::new();
            for inst in &all {
                match runtime::status(inst) {
                    Ok(r) => {
                        text.push_str(&runtime::render_status(&runtime::header(inst), &r));
                        rows.extend(r);
                    }
                    Err(e) => {
                        eprintln!("brg: {}: {e}", inst.id);
                        code = ExitCode::from(1);
                    }
                }
            }
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&rows).unwrap_or_default()
                );
            } else if all.is_empty() {
                println!("{}", address::known());
            } else {
                print!("{text}");
            }
            code
        }
        Command::Attach { member } => {
            let (inst, name) = or_fail!(instance_and_name(&member));
            match runtime::attach(&inst, &name) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => fail(e),
            }
        }
        Command::Logs { member, lines } => {
            let (inst, name) = or_fail!(instance_and_name(&member));
            let text = or_fail!(runtime::logs(&inst, &name, lines));
            print!("{text}");
            ExitCode::SUCCESS
        }
        Command::Send {
            member,
            text,
            force,
            from,
            json,
        } => {
            let text = text.join(" ");
            let result = instance_and_name(&member)
                .map_err(runtime::SendError::Usage)
                .and_then(|(inst, name)| {
                    runtime::send(&inst, &name, &text, force, from.as_deref())
                });
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
        Command::Msg { instance, command } => crate::bus::cli::msg(instance.as_deref(), command),
        Command::Inbox { instance, args } => {
            crate::bus::cli::inbox(instance.as_deref(), args, started)
        }
        Command::Bus { instance, command } => crate::bus::cli::bus(instance.as_deref(), command),
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

fn run_validate(team: &str, json: bool, probe: bool) -> ExitCode {
    let mut config_report = crate::diag::Report::default();
    let search = crate::config::load(&mut config_report)
        .map(|c| c.teams)
        .unwrap_or_default();
    let dir = match crate::team::find(team, &search) {
        Ok(dir) => dir,
        Err(e) => {
            for diag in &config_report.diags {
                eprintln!("{diag}");
            }
            return fail(e);
        }
    };
    let (mut report, validated) = validate(&dir);
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
