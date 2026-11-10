use clap::Parser;

#[derive(Parser)]
#[command(
    name = "brg-seat",
    version,
    about = "Check explicit subscription quota before seating"
)]
struct Cli {
    #[command(subcommand)]
    command: brg_seat::Command,
}
fn main() -> std::process::ExitCode {
    brg_seat::run(Cli::parse().command)
}
