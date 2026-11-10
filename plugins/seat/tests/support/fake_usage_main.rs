#[path = "../../../../tests/support/fake_usage.rs"]
mod fake_usage;
fn main() {
    if !fake_usage::run(&std::env::args().skip(1).collect::<Vec<_>>()) {
        std::process::exit(2);
    }
}
