//! Command-line interface for `brg`.

use std::io::IsTerminal;
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

/// The top-level help's headed groups. Every visible command is in one.
const GROUPS: &[(&str, &[&str])] = &[
    ("Team folders", &["templates", "validate", "secrets"]),
    (
        "Instances and members",
        &[
            "create", "start", "stop", "delete", "status", "attach", "logs",
        ],
    ),
    ("Messages", &["msg"]),
    ("Cleanup", &["cleanup", "destroy"]),
];

/// The parser, with its top-level help laid out in `GROUPS`: each command's
/// one-line summary under its heading. Details stay in each command's help.
fn command() -> clap::Command {
    let cmd = <Cli as clap::CommandFactory>::command();
    let header = cmd.get_styles().get_header();
    let literal = cmd.get_styles().get_literal();
    let width = GROUPS
        .iter()
        .flat_map(|(_, names)| names.iter().map(|n| n.len()))
        .max()
        .unwrap_or(0);
    let mut groups = String::new();
    for (heading, names) in GROUPS {
        groups.push_str(&format!("{header}{heading}:{header:#}\n"));
        for name in *names {
            let about = cmd
                .find_subcommand(name)
                .and_then(clap::Command::get_about)
                .map(ToString::to_string)
                .unwrap_or_default();
            let pad = " ".repeat(width - name.len());
            groups.push_str(&format!("  {literal}{name}{literal:#}{pad}  {about}\n"));
        }
        groups.push('\n');
    }
    groups
        .push_str("`brg help <command>` or `brg <command> --help` gives a command's details.\n\n");
    let template = format!(
        "{{before-help}}{{about-with-newline}}\n{{usage-heading}} {{usage}}\n\n{groups}\
         {header}Options:{header:#}\n{{options}}{{after-help}}"
    );
    cmd.help_template(template)
}

#[derive(Subcommand)]
enum Command {
    /// Check a team folder and brg's configuration; starts nothing.
    ///
    /// It also lists the files each agent role's instruction is read from.
    Validate {
        /// The team: its name in the search paths, or a path to its folder
        /// (starting with `/`, `./`, `../` or `~/`).
        #[arg(add = crate::complete::teams())]
        team: String,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
        /// Also ask each vendor CLI which MCP connectors are configured. This
        /// starts your MCP servers to health-check them.
        #[arg(long)]
        probe: bool,
    },
    /// Make an instance of a team, or a member of a role.
    ///
    /// That is, an instance of a team working in a folder
    /// (`create <team> [folder]`, the current folder by default), or a member
    /// of a role (`create <instance>/<role>`, up to its max of kept members).
    /// Team-role copies are created stopped; start them with `brg start`.
    /// Agent members and top-level instances are started. With a bus, a child role's parent role must have a member.
    /// For a role target with a bus, a member caller must be an ancestor,
    /// using its latest running start in its own instance.
    /// Members work in the team's folder. A `worktree: true` role's member or
    /// copy gets its own git worktree from it instead, which brg adds when
    /// issuing it (create, or a full `start` that gives a `start: true` role
    /// a new member), detached at the folder's HEAD or on `--branch`.
    /// With `--repo`, name a clone directly in the tree's folder instead:
    /// fetch origin's default branch and start at its tip. The clone's
    /// checkout stays as it is. Use one dev copy per repository; the root
    /// lead sequences copies.
    /// For a member or an instance, it prints the instance's tree as `status`
    /// lays it out, with a new member's ID on its row, then a line with the
    /// address and folder of each worktree it added; for a copy, one line
    /// with the copy's ID and its folder (its worktree, for a `worktree: true`
    /// role).
    Create {
        /// `<team>` (a name, or a path starting with `/`, `./`, `../` or
        /// `~/`), or `<instance>/<role>`.
        #[arg(add = crate::complete::teams())]
        target: String,
        /// The instance's working folder, for a team: usually the repository
        /// it works on. It must exist. Default: the current folder.
        #[arg(value_hint = clap::ValueHint::DirPath)]
        folder: Option<String>,
        /// For a `worktree: true` role: put the new worktree on this new branch
        /// instead of a detached HEAD.
        #[arg(long)]
        branch: Option<String>,
        /// For a `worktree: true` role: select a real child clone in the
        /// tree's folder and fetch origin's default branch before creating.
        #[arg(long)]
        repo: Option<String>,
        /// For a role: make the new ID `<role>-<name>`, named for its task,
        /// instead of a random suffix: lowercase words joined by hyphens, at
        /// most 24 characters, never a role's name, an ID issued before in the
        /// instance or another ID's suffix.
        #[arg(long)]
        name: Option<String>,
    },
    /// Bring an instance to what should be running, or start one member.
    ///
    /// Restore copies that ran at the last full stop, recursively. Finish
    /// an interrupted cascade with `stop` first. Reuse kept members; never
    /// fill a role to max. A worktree it adds for a new member is named after
    /// the tree. With a bus, a member caller may manage only its subtree,
    /// using its latest running start. Members that start use current files
    /// and configuration.
    Start {
        /// `<instance>`, or `<instance>/<member>`.
        #[arg(add = crate::complete::targets())]
        target: String,
        /// Point a stopped instance at its team folder's new place.
        #[arg(long)]
        team: Option<PathBuf>,
        /// Reopen each member's last vendor session instead of starting a new
        /// conversation. A member started on its own needs a recorded session;
        /// a whole instance starts fresh whoever has none. Restored copies
        /// resume too.
        #[arg(long)]
        resume: bool,
    },
    /// Stop an instance and all its copies deepest first, or one member.
    ///
    /// Every full stop persists `stopping` before the cascade, refusing create/start inside
    /// it until every instance is certainly stopped. Run stop again to
    /// finish an interrupted cascade. A second stop keeps who ran.
    /// If the server is gone, end any surviving recorded process and retry stop.
    /// For other server errors, fix access and retry stop.
    /// With a bus, a member caller may manage only its subtree, using its
    /// latest running start. A copy's roots cannot stop their own copy.
    Stop {
        /// `<instance>`, or `<instance>/<member>`.
        #[arg(add = crate::complete::targets())]
        target: String,
        /// Stop the member's subtree deepest first, including copies. Members
        /// may target descendants, never themselves. Other lifecycle changes
        /// wait until this walk finishes. An instance target already cascades.
        #[arg(short = 'r', long)]
        recursive: bool,
    },
    /// Remove a stopped instance, or a stopped member.
    ///
    /// A deleted member's ID stays issued. With a bus, a member caller may
    /// manage only its subtree, using its latest running start. A copy's roots
    /// cannot delete their own copy.
    /// Unread messages, and a worktree brg added that holds uncommitted or
    /// untracked changes or commits on no remote, need --force; copy history
    /// stays in the tree store.
    Delete {
        /// `<instance>`, or `<instance>/<member>`. A bare role is refused.
        #[arg(add = crate::complete::targets())]
        target: String,
        /// Delete even with unread messages, unreadable message storage, or
        /// worktrees with work in them (those are left as they are; a clean
        /// worktree brg added is always removed). Running members, unknown
        /// liveness and members with kept children or copies are always refused.
        #[arg(long)]
        force: bool,
    },
    /// Stop a whole tree and delete its records, notes and messages.
    ///
    /// It stops the tree first, then deletes every registration, the root's
    /// state folder (all copies' state, the notes and the tree's messages) and
    /// the clean worktrees brg added for members and copies; copy any notes
    /// you need first. Open work never refuses it, and there is no prompt; it
    /// refuses if brg can't confirm every member stopped. Working folders,
    /// checkouts and other worktrees are left as they are, and `brg create`
    /// can start the tree fresh. Run it from your own shell, never inside a
    /// member.
    Destroy {
        /// The tree's root instance (a copy is refused).
        #[arg(add = crate::complete::roots())]
        instance: String,
    },
    /// Remove what brg left behind for deleted members and copies.
    ///
    /// That is the clean git worktrees it added for them, and their leftover
    /// state. A worktree with work in it is listed and kept. Without an instance, also
    /// remove the state of instance IDs brg issued that are no longer
    /// registered. Work folders, kept members' and copies' worktrees, other
    /// worktrees and messages are never touched.
    Cleanup {
        /// Only this instance and its copies (default: every instance).
        #[arg(add = crate::complete::instances())]
        instance: Option<String>,
    },
    /// List the secrets a team's plugins need, and whether each is set.
    ///
    /// It checks your environment, prints no values and writes nothing.
    Secrets {
        /// The team: its name in the search paths, or a path to its folder.
        #[arg(add = crate::complete::teams())]
        team: String,
        /// Print {secrets: [{plugin, name, set}]} as JSON.
        #[arg(long)]
        json: bool,
    },
    /// List available team templates and their instances.
    ///
    /// The teams found in the search paths (`teams:` in brg's configuration),
    /// each with its instances, then instances of teams elsewhere.
    Templates {
        /// Print the list as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show an instance tree, or every top-level one, with nested copies.
    ///
    /// Each member's row shows its ID, vendor and model, state and any unacked
    /// messages. A role with no member shows only its state; its `--json` row
    /// also gives its vendor and model.
    ///
    /// On a terminal, state precedes vendor/model or team. A summary band above
    /// the tree counts working and idle members, rows needing
    /// you and unacked messages. Painted output uses a filled red state label
    /// for attention and a gold ◆ for running teams. Working marks spin in
    /// --watch and --interactive; NO_COLOR suppresses painting and motion.
    Status {
        /// The instance (default: every top-level instance and its copies).
        #[arg(add = crate::complete::instances())]
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
    ///
    /// brg's tmux server runs without your own tmux config unless brg's
    /// configuration sets `tmux.user_config`, and a change to it applies only
    /// once every instance on brg's server has stopped and the server starts
    /// again. To keep your own bindings without a restart, attach from inside
    /// your own tmux. brg's prefix is then tmux's default, Ctrl-b (press it
    /// twice if your own prefix is Ctrl-b too).
    Attach {
        /// The member: `<instance>/<member>`.
        #[arg(add = crate::complete::members())]
        member: String,
    },
    /// Show a member's or plugin's recent output.
    Logs {
        /// The member (`<instance>/<member>`), or a plugin process
        /// (`<instance>/_<plugin>`).
        #[arg(add = crate::complete::members())]
        member: String,
        /// How many lines of history.
        #[arg(short = 'n', long, default_value_t = 200)]
        lines: usize,
    },
    /// Send, list, read, acknowledge, wait for and check messages.
    Msg {
        /// The instance (outside a member; inside one, only its own).
        #[arg(long, global = true, add = crate::complete::instances())]
        instance: Option<String>,
        #[command(subcommand)]
        command: crate::bus::cli::MsgCommand,
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

pub(crate) fn send_target(arg: &str) -> Result<(Instance, String), runtime::SendError> {
    use runtime::SendError::{NotAlive, Usage};
    let (instance, name) = arg
        .rsplit_once('/')
        .map_or((None, arg), |(i, n)| (Some(i), n));
    if instance.is_some_and(|id| !id.split('/').all(crate::names::is_valid_member_id)) {
        return Err(Usage("invalid target instance syntax".into()));
    }
    if name.starts_with("external:") {
        if crate::bus::record::Recipient::parse(name).is_none() {
            return Err(Usage("invalid external recipient".into()));
        }
        let id = instance
            .map(str::to_string)
            .or_else(address::own)
            .ok_or_else(|| Usage("name the target instance".into()))?;
        return Ok((address::instance(&id).map_err(NotAlive)?, name.to_string()));
    }
    if !arg.split('/').all(crate::names::is_valid_member_id) {
        return Err(Usage("invalid target address syntax".into()));
    }
    if instance.is_none() && address::own().is_none() {
        return Err(Usage("name the target instance".into()));
    }
    instance_and_name(arg).map_err(NotAlive)
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
    // First: a completion request must exit before the wait back-off below.
    crate::complete::complete(<Cli as clap::CommandFactory>::command);
    let started = std::time::Instant::now();
    let args: Vec<_> = std::env::args_os().collect();
    // Recognize the wait even when clap rejects its flags. Usage errors
    // must back off too, or a broken wait command can spin turns.
    // Keep the removed `inbox wait` spelling here so old role text cannot spin.
    // Explicit help and external termination remain immediate.
    let mut tokens = args.iter().skip(1);
    let waiting = if tokens
        .next()
        .is_some_and(|arg| arg == "msg" || arg == "inbox")
    {
        loop {
            match tokens.next() {
                Some(arg) if arg == "--instance" => {
                    tokens.next();
                }
                Some(arg) if arg.to_str().is_some_and(|s| s.starts_with("--instance=")) => {}
                Some(arg) => break arg == "wait",
                None => break false,
            }
        }
    } else {
        false
    };
    let help = args.iter().any(|a| a == "--help" || a == "-h");
    let back_off = || {
        if waiting && !help {
            let minimum = crate::bus::store::wait_minimum();
            std::thread::sleep(minimum.saturating_sub(started.elapsed()));
        }
    };
    let mut cmd = command();
    let parsed = cmd
        .try_get_matches_from_mut(args)
        .and_then(|matches| <Cli as clap::FromArgMatches>::from_arg_matches(&matches))
        .map_err(|e| e.format(&mut cmd));
    let cli = match parsed {
        Ok(cli) => cli,
        Err(error) => {
            if error.use_stderr() {
                back_off();
            }
            error.exit();
        }
    };
    let code = run(cli, started);
    // A fenced wait belongs to an ended start and must not linger or retry.
    if code != ExitCode::SUCCESS && code != ExitCode::from(crate::bus::outcome::Code::Fenced.exit())
    {
        back_off();
    }
    code
}

fn run(cli: Cli, started: std::time::Instant) -> ExitCode {
    match cli.command {
        Command::Validate { team, json, probe } => run_validate(&team, json, probe),
        Command::Create {
            target,
            folder,
            branch,
            repo,
            name,
        } => {
            let team_like = crate::team::is_path(&target) || !target.contains('/');
            let result = if team_like && name.is_some() {
                Err(
                    "`--name` names a new member or copy, not an instance: brg create <instance>/<role> --name <name>"
                        .to_string(),
                )
            } else if team_like {
                match branch
                    .as_ref()
                    .map(|_| "--branch")
                    .or(repo.as_ref().map(|_| "--repo"))
                {
                    Some(flag) => Err(format!(
                        "`{flag}` is for a role's new worktree: brg create <instance>/<role> {flag} <name>"
                    )),
                    None => runtime::create(&target, folder.as_deref().unwrap_or(".")),
                }
            } else {
                match address::resolve(&target) {
                    Ok(_) if folder.is_some() => Err(format!(
                        "`{target}` names a role, which takes no folder: its member works in the instance's folder"
                    )),
                    Ok((inst, Target::Role(role))) => runtime::create_member(
                        inst,
                        &role,
                        name.as_deref(),
                        branch.as_deref(),
                        repo.as_deref(),
                    ),
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
        Command::Start {
            target,
            team,
            resume,
        } => {
            let result = match or_fail!(address::resolve(&target)) {
                (inst, Target::Instance) => runtime::start(inst, None, team.as_deref(), resume),
                (inst, Target::Member(m) | Target::Role(m)) => {
                    runtime::start(inst, Some(&m), team.as_deref(), resume)
                }
            };
            print_outcome(or_fail!(result))
        }
        Command::Stop { target, recursive } => {
            let result = match or_fail!(address::resolve(&target)) {
                (inst, Target::Instance) => runtime::stop(&inst, None),
                (inst, Target::Member(m) | Target::Role(m)) if recursive => {
                    runtime::stop_recursive(&inst, &m)
                }
                (inst, Target::Member(m) | Target::Role(m)) => runtime::stop(&inst, Some(&m)),
            };
            for m in or_fail!(result) {
                println!("{m}");
            }
            ExitCode::SUCCESS
        }
        Command::Delete { target, force } => {
            let result = match or_fail!(address::resolve(&target)) {
                (inst, Target::Instance) => runtime::delete(inst, force),
                (inst, Target::Member(m) | Target::Role(m)) => {
                    runtime::delete_member(inst, &m, force)
                }
            };
            for m in or_fail!(result) {
                println!("{m}");
            }
            ExitCode::SUCCESS
        }
        Command::Destroy { instance } => {
            let inst = match or_fail!(address::resolve(&instance)) {
                (inst, Target::Instance) => inst,
                (inst, _) => {
                    return fail(format!(
                        "`destroy` takes a tree's root instance; `{instance}` is in instance `{}`",
                        inst.id
                    ));
                }
            };
            let outcome = or_fail!(runtime::reset(inst, |warning| println!("{warning}")));
            for line in &outcome.lines {
                println!("{line}");
            }
            if outcome.failed {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        Command::Cleanup { instance } => {
            let only = match &instance {
                Some(id) => match or_fail!(address::resolve(id)) {
                    (inst, Target::Instance) => Some(inst),
                    (inst, _) => {
                        return fail(format!(
                            "`cleanup` takes an instance; `{id}` is in instance `{}`",
                            inst.id
                        ));
                    }
                },
                None => None,
            };
            let outcome = or_fail!(crate::cleanup::cleanup(only.as_ref()));
            for line in &outcome.lines {
                println!("{line}");
            }
            if outcome.failed {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        Command::Secrets { team, json } => {
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
        Command::Templates { json } => {
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
                let result = if interactive {
                    crate::tui::interactive(one.as_ref())
                } else {
                    crate::tui::watch(
                        one.as_ref(),
                        std::time::Duration::from_secs(interval.max(1)),
                    )
                };
                return match result {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(e) => fail(e),
                };
            }
            let all: Vec<Instance> = match one {
                Some(inst) => vec![inst],
                None => or_fail!(address::every()),
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
                if std::io::stdout().is_terminal() {
                    println!(
                        "{}\n",
                        runtime::summary_band(&rows, crate::tree::colour_wanted(true))
                    );
                }
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
        Command::Msg { instance, command } => {
            crate::bus::cli::msg(instance.as_deref(), command, started)
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
        let mut out = serde_json::json!({
            "ok": !report.has_errors(),
            "errors": report.errors().collect::<Vec<_>>(),
            "warnings": report.warnings().collect::<Vec<_>>(),
        });
        if let Some(v) = &validated {
            out["roles"] = v
                .team
                .roles
                .iter()
                .map(|(id, role)| {
                    let sources: Vec<String> = role
                        .sources
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect();
                    (id.clone(), serde_json::json!({ "sources": sources }))
                })
                .collect();
        }
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    } else {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let colour = crate::tree::colour_wanted(std::io::stdout().is_terminal());
        for diag in &report.diags {
            let mut diag = diag.clone();
            diag.file = diag
                .file
                .map(|path| runtime::tilde(&path, home.as_deref()).into());
            // Paths inside the message too; a home of `/` would shorten every path.
            if let Some(h) = home.as_deref().filter(|h| h.parent().is_some()) {
                let prefix = format!("{}/", h.display().to_string().trim_end_matches('/'));
                diag.message = diag.message.replace(&prefix, "~/");
            }
            eprintln!("{diag}");
        }
        if !report.diags.is_empty() && validated.is_some() {
            eprintln!();
        }
        if let Some(v) = &validated {
            let warnings = report.warnings().count();
            let verdict = format!(
                "ok: {} role(s), {} plugin(s){}",
                v.team.roles.len(),
                v.team.plugins.len(),
                if warnings > 0 {
                    format!(", {warnings} warning(s)")
                } else {
                    String::new()
                }
            );
            println!("{}", validate_heading(&verdict, colour));
            println!("\n{}", validate_heading("Roles:", colour));
            for (id, role) in &v.team.roles {
                println!("  {}", validate_heading(id, colour));
                print_role_sources(&role.source_tree, 2, home.as_deref(), colour);
            }
        }
    }
    if report.has_errors() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn validate_heading(text: &str, colour: bool) -> String {
    if colour {
        crate::tree::ink_text(text, crate::tree::palette::TEAM, true)
    } else {
        text.to_string()
    }
}

fn print_role_sources(
    sources: &[crate::team::RoleSource],
    depth: usize,
    home: Option<&std::path::Path>,
    colour: bool,
) {
    for source in sources {
        let path = runtime::tilde(&source.file, home);
        let path = if colour {
            crate::tree::ink_text(&path, crate::tree::palette::FADED, false)
        } else {
            path
        };
        println!("{:indent$}{path}", "", indent = depth * 2);
        print_role_sources(&source.includes, depth + 1, home, colour);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// The top-level help lists every visible command under exactly one
    /// heading, each with a summary.
    #[test]
    fn every_command_is_in_one_help_group() {
        let mut listed: Vec<&str> = GROUPS.iter().flat_map(|(_, n)| n.iter().copied()).collect();
        listed.sort_unstable();
        let mut visible: Vec<String> = Cli::command()
            .get_subcommands()
            .filter(|s| !s.is_hide_set())
            .map(|s| s.get_name().to_string())
            .collect();
        visible.sort_unstable();
        assert_eq!(listed, visible);
        let cmd = Cli::command();
        let bare: Vec<_> = listed
            .iter()
            .filter(|n| {
                cmd.find_subcommand(n)
                    .and_then(clap::Command::get_about)
                    .is_none()
            })
            .collect();
        assert!(bare.is_empty(), "commands without a summary: {bare:?}");
    }

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
