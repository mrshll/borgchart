//! Poll and read GitHub as the configured machine account.
use borgchart::plugin;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Poll and read GitHub as the configured machine account")]
struct Cli {
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Read an owner action, checking authorship and framing its text
    Read { id: String },
    /// Poll every 60 seconds; wait while GITHUB_CONFIG is missing
    Run {
        /// Poll once and exit
        #[arg(long)]
        once: bool,
    },
}
fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Action::Read { id } => brg_github::read::read(&id).map(|text| print!("{text}")),
        Action::Run { once } => plugin::State::from_env()
            .map_err(|_| "set BORGCHART_STATE_DIR to a private plugin state directory".to_string())
            .and_then(|store| brg_github::run::run(&store, once))
            .map_err(|error| (1, error)),
    };
    if let Err((code, error)) = result {
        eprintln!("{}", plugin::escape(&error).replace('\n', " "));
        std::process::exit(code);
    }
}
