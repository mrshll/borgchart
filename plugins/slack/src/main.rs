//! Read and watch Slack evidence, promote IDs and make guarded Slack writes.
use borgchart::plugin;
use brg_slack::{
    Error, Result,
    api::{self, Api},
    watch, writes,
};
use clap::{Parser, Subcommand, error::ErrorKind};
use serde_json::{Value, json};

#[derive(Parser)]
#[command(about = "Read and watch Slack evidence; promote IDs and make guarded writes")]
struct Cli {
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Poll the channel and retry promotions
    Watch {
        #[arg(long)]
        once: bool,
    },
    /// Read a top-level post
    Read { ts: String },
    /// Read up to 200 thread messages
    Thread { ts: String },
    /// Send a timestamp and reason to your parent
    Promote { ts: String, reason: String },
    /// Add eyes or done
    React { ts: String, reaction: String },
    /// Reply in a thread
    Reply { thread_ts: String, text: String },
    /// Post or update one status message
    Status { text: String },
    /// Page an explicitly allowed user
    Page { user_id: String, text: String },
}
fn field(message: &Value, key: &str) -> String {
    message[key].as_str().unwrap_or("unknown").to_string()
}
fn show(messages: &[Value]) {
    println!("{}", plugin::UNTRUSTED_HEADER);
    for message in messages.iter().take(200) {
        let mut fields = format!("--- ts={}", field(message, "ts"));
        if let Some(thread) = message["thread_ts"].as_str() {
            fields.push_str(&format!(" thread_ts={thread}"));
        }
        let author = message["user"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| message["bot_id"].as_str())
            .unwrap_or("unknown");
        fields.push_str(&format!(" author={author}"));
        if !message["edited"].is_null() {
            fields.push_str(&format!(" edited={}", field(&message["edited"], "ts")));
        }
        println!("{}", api::one_line(&fields));
        print!(
            "{}",
            plugin::frame(&api::safe(message["text"].as_str().unwrap_or("")))
        );
    }
}
fn read(api: &Api, ts: &str, thread: bool) -> Result<()> {
    api::check_ts(ts)?;
    let mut params = json!({"channel":api.channel()?,"limit":if thread {200} else {1}});
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
        .filter(|m| thread || (m["ts"] == ts && m["thread_ts"].as_str().unwrap_or(ts) == ts))
        .cloned()
        .collect();
    if messages.is_empty() {
        return Err(Error::failure(
            "post not found; use the root timestamp and check channel access",
        ));
    }
    show(&messages);
    Ok(())
}
fn run(action: Action) -> Result<()> {
    let api = Api::default();
    match action {
        Action::Watch { once } => watch::run(&api, once),
        Action::Read { ts } => read(&api, &ts, false),
        Action::Thread { ts } => read(&api, &ts, true),
        Action::Promote { ts, reason } => writes::promote(&ts, &reason),
        Action::React { ts, reaction } => writes::react(&api, &ts, &reaction),
        Action::Reply { thread_ts, text } => writes::reply(&api, &thread_ts, &text),
        Action::Status { text } => writes::status(&api, &text),
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
            eprintln!(
                "{}; run brg-slack --help",
                api::one_line(
                    error
                        .to_string()
                        .lines()
                        .next()
                        .unwrap_or("invalid arguments")
                )
            );
            std::process::exit(2);
        }
    };
    if let Err(error) = run(cli.command) {
        eprintln!("{}", api::one_line(&error.to_string()));
        std::process::exit(error.code());
    }
}
