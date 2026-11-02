use std::process::ExitCode;

use clap::{Parser, Subcommand};

use orchestral::outcome::{self, Failure};
use orchestral::{limits, spawn};

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
    /// Start a worker on an issue, in its own worktree.
    Spawn {
        /// Lowercase letters, digits and dashes. Also the branch name.
        name: String,
        /// The issue the worker implements.
        #[arg(long)]
        issue: u64,
        /// Overrides the policy's default model.
        #[arg(long)]
        model: Option<String>,
        /// Overrides the policy's default effort.
        #[arg(long)]
        effort: Option<String>,
        /// The worker's first message.
        #[arg(long)]
        prompt: String,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Hooks that Claude Code runs. Not for direct use.
    #[command(hide = true)]
    Hook {
        #[command(subcommand)]
        hook: Hook,
    },
}

#[derive(Subcommand)]
enum Hook {
    /// Checks a worker's Bash commands before they run.
    WorkerGuard,
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
        Command::Spawn {
            name,
            issue,
            model,
            effort,
            prompt,
            json,
        } => {
            let cwd = std::env::current_dir().map_err(|err| {
                Failure::Error(format!("can't read the current directory: {err}"))
            })?;
            let request = spawn::Request {
                name,
                issue,
                model,
                effort,
                prompt,
            };
            outcome::print(&spawn::spawn(&cwd, request)?, json)
        }
        // Allows everything until the worker's denies exist.
        Command::Hook {
            hook: Hook::WorkerGuard,
        } => Ok(()),
    }
}
