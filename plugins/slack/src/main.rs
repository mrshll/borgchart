//! Read and watch Slack evidence, promote IDs and make guarded Slack writes.
use borgchart::plugin;
use brg_slack::{
    Error, Result,
    api::{self, Api},
    mentions, watch, writes,
};
use clap::{Parser, Subcommand, error::ErrorKind};
use serde_json::json;

#[derive(Parser)]
#[command(about = "Read and watch Slack evidence; promote IDs and make guarded writes")]
struct Cli {
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Poll the channel and send ID notices
    Watch {
        #[arg(long)]
        once: bool,
    },
    /// Listen for allow-listed leading mentions over Socket Mode
    Mentions {
        #[arg(long)]
        once: bool,
    },
    /// Read a top-level post
    Read { ts: String },
    /// Read up to 200 thread messages by timestamp or request ID
    Thread { ts: String },
    /// Send a timestamp and reason to your parent
    Promote { ts: String, reason: String },
    /// Add eyes or done by timestamp or request ID
    React { ts: String, reaction: String },
    /// Reply in a thread by timestamp or request ID
    Reply { thread_ts: String, text: String },
    /// Page an explicitly allowed user in the configured channel
    Page { user_id: String, text: String },
}
fn read(api: &Api, ts: &str, thread: bool) -> Result<()> {
    let (channel, ts) = if thread {
        let request = mentions::resolve(api, ts)?;
        (request.channel, request.thread)
    } else {
        api::check_ts(ts)?;
        (api.channel()?, ts.to_owned())
    };
    let mut params = json!({"channel":channel,"limit":if thread {200} else {1}});
    if thread {
        params["ts"] = json!(ts);
    } else {
        params["latest"] = json!(ts);
        params["inclusive"] = json!("true");
    }
    let data = api.call(
        if thread {
            "conversations.replies"
        } else {
            "conversations.history"
        },
        params,
        false,
    )?;
    let messages: Vec<_> = data["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| thread || (m["ts"] == ts && m["thread_ts"].as_str().unwrap_or(&ts) == ts))
        .cloned()
        .collect();
    if messages.is_empty() {
        return Err(Error::failure(
            "post not found; use the root timestamp and check channel access",
        ));
    }
    print!(
        "{}\n{}",
        plugin::UNTRUSTED_HEADER,
        api::render(&messages, usize::MAX)
    );
    Ok(())
}
fn run(action: Action) -> Result<()> {
    let api = Api::default();
    match action {
        Action::Mentions { once } => mentions::run(&api, once),
        Action::Watch { once } => watch::run(&api, once),
        Action::Read { ts } => read(&api, &ts, false),
        Action::Thread { ts } => read(&api, &ts, true),
        Action::Promote { ts, reason } => writes::promote(&ts, &reason),
        Action::React { ts, reaction } => writes::react(&api, &ts, &reaction),
        Action::Reply { thread_ts, text } => writes::reply(&api, &thread_ts, &text),
        Action::Page { user_id, text } => writes::page(&api, &user_id, &text),
    }
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
                    .take_while(|line| !line.is_empty() && !line.starts_with("Usage:"))
                    .map(str::trim)
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            eprintln!("{}; run brg-slack --help", api::one_line(&detail));
            std::process::exit(2);
        }
    };
    if let Err(error) = run(cli.command) {
        eprintln!("{}", api::one_line(&error.to_string()));
        std::process::exit(error.code());
    }
}
