mod limits;
mod outcome;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use outcome::Failure;

/// A small workflow layer for Claude Code.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show the plan's usage windows as percent used and reset time.
    Limits {
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            // clap exits 2 on a usage error, which here means a refusal.
            let _ = err.print();
            return if err.use_stderr() {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            };
        }
    };
    outcome::finish(run(cli.command))
}

fn run(command: Command) -> Result<(), Failure> {
    match command {
        Command::Limits { json } => outcome::print(&limits::read(), json),
    }
}
