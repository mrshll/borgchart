//! Poll GitHub as the configured machine account.
use borgchart::plugin;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Poll GitHub as the configured machine account")]
struct Cli {
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Poll configured repositories every 60 seconds
    Run {
        /// Poll once and exit
        #[arg(long)]
        once: bool,
    },
}
fn main() {
    let cli = Cli::parse();
    let result = plugin::State::from_env()
        .map_err(|_| "set BORGCHART_STATE_DIR to a private plugin state directory".to_string())
        .and_then(|store| match cli.command {
            Action::Run { once } => brg_github::run::run(&store, once),
        });
    if let Err(error) = result {
        eprintln!("{}", plugin::escape(&error).replace('\n', " "));
        std::process::exit(1);
    }
}
