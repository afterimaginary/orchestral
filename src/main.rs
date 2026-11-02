use clap::Parser;

/// A small workflow layer for Claude Code.
#[derive(Parser)]
#[command(version, about)]
struct Cli {}

fn main() {
    Cli::parse();
}
