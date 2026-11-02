use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};

use orchestral::outcome::{self, Failure};
use orchestral::{
    claim, cleanup, compact, gate, guard, limits, merge, preview, proposal, review, setup, spawn,
    status, stop, supervisor, watch,
};

/// A small workflow layer for Claude Code.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum Role {
    Worker,
    Reviewer,
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
        /// The issue the worker implements, or the reviewed PR is for.
        #[arg(long)]
        issue: u64,
        /// A worker implements the issue; a reviewer reviews `--pr`.
        #[arg(long, value_enum, default_value_t = Role::Worker)]
        role: Role,
        /// The PR a reviewer reviews.
        #[arg(long, required_if_eq("role", "reviewer"))]
        pr: Option<u64>,
        /// The worker starts on its issue's design proposal, whose token
        /// spend `status` shows until `orchestral proposal-done <name>`.
        #[arg(long)]
        proposal: bool,
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
    /// Record that a worker's design proposal is over, because the owner chose
    /// a design or rejected it, keeping what it spent for `status`.
    ProposalDone {
        name: String,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Show which merge conditions a PR meets at its head commit. Never merges.
    MergeCheck {
        pr: u64,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Ask for a PR's preview, which the supervisor runs in its worker's
    /// worktree.
    Try {
        pr: u64,
        /// Stop the PR's preview instead.
        #[arg(long)]
        stop: bool,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// List the PR previews and their links.
    Previews {
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Record a reviewer's verdict on a PR's head commit.
    Review {
        pr: u64,
        /// The PR is fine to merge.
        #[arg(long, conflicts_with = "fail", required_unless_present = "fail")]
        pass: bool,
        /// The author must fix something first.
        #[arg(long)]
        fail: bool,
        /// The head commit the verdict is for.
        #[arg(long)]
        sha: String,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Stop a worker, keeping its worktree and conversation.
    Stop {
        name: String,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Remove a worker whose PR merged or closed: its session, worktree and
    /// local branch.
    Cleanup {
        name: String,
        /// Remove it anyway, discarding unpushed or uncommitted work.
        #[arg(long)]
        force: bool,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Resume a stopped worker where it left off.
    Resume {
        name: String,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Make this Claude Code session the repository's orchestrator.
    Claim {
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Print fleet events while this session holds the claim. Run by the
    /// plugin monitor in every session; never exits by itself.
    Watch,
    /// Print the SendMessage address of a worker, or of `orchestrator`.
    Resolve {
        name: String,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Show the fleet: workers, plan limits, budget and pause.
    Status {
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Start the supervisor in the background, unless it's running.
    Up {
        /// Hours the supervisor stays up after the last worker closes.
        #[arg(long, default_value_t = 8.0)]
        keepalive_hours: f64,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// The supervisor process that `up` starts. Not for direct use.
    #[command(hide = true)]
    Supervise {
        #[arg(long)]
        keepalive_hours: f64,
    },
    /// Refuse new spawns until `unpause`.
    Pause {
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Allow spawns again after `pause`.
    Unpause {
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Set up this repository: policy template, pre-push hook, checks.
    Init {
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Check tools, auth, trust and the policy before a run.
    Doctor {
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Hooks that Claude Code and git run. Not for direct use.
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
    /// Saves the fleet's status before the orchestrator compacts.
    PreCompact,
    /// Puts the instructions and status back after the orchestrator compacts.
    SessionCompact,
    /// Rejects a worker's push to main, as a git pre-push hook.
    PrePush {
        /// The remote's name and URL, which git passes.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        _remote: Vec<String>,
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
        Command::Up {
            keepalive_hours,
            json,
        } => outcome::print(&supervisor::up(&current_dir()?, keepalive_hours)?, json),
        Command::Supervise { keepalive_hours } => {
            supervisor::supervise(&current_dir()?, keepalive_hours)
        }
        Command::Status { json } => outcome::print(&status::status(&current_dir()?)?, json),
        Command::Pause { json } => outcome::print(&gate::set_paused(&current_dir()?, true)?, json),
        Command::Unpause { json } => {
            outcome::print(&gate::set_paused(&current_dir()?, false)?, json)
        }
        Command::Spawn {
            name,
            issue,
            model,
            effort,
            prompt,
            role,
            pr,
            proposal,
            json,
        } => {
            let cwd = current_dir()?;
            if proposal && matches!(role, Role::Reviewer) {
                return Err(Failure::Refused {
                    reason: "--proposal is only for a worker".into(),
                    fix: "drop --proposal".into(),
                });
            }
            let review_of = match role {
                Role::Worker if pr.is_some() => {
                    return Err(Failure::Refused {
                        reason: "--pr is only for `--role reviewer`".into(),
                        fix: "drop --pr, or add `--role reviewer`".into(),
                    });
                }
                Role::Worker => None,
                Role::Reviewer => pr,
            };
            let request = spawn::Request {
                name,
                issue,
                model,
                effort,
                prompt,
                review_of,
                proposal,
            };
            outcome::print(&spawn::spawn(&cwd, request)?, json)
        }
        Command::Try { pr, stop, json } => match stop {
            true => outcome::print(&preview::stop(&current_dir()?, pr)?, json),
            false => outcome::print(&preview::try_pr(&current_dir()?, pr)?, json),
        },
        Command::Previews { json } => outcome::print(&preview::list(&current_dir()?)?, json),
        Command::ProposalDone { name, json } => {
            outcome::print(&proposal::done(&current_dir()?, &name)?, json)
        }
        Command::MergeCheck { pr, json } => {
            let check = merge::check(&current_dir()?, pr)?;
            outcome::print(&check, json)?;
            match check.mergeable() {
                true => Ok(()),
                false => Err(Failure::Refused {
                    reason: format!("PR #{pr} can't merge yet"),
                    fix: "meet the failed conditions above".into(),
                }),
            }
        }
        Command::Review {
            pr,
            pass,
            fail: _,
            sha,
            json,
        } => outcome::print(&review::record(&current_dir()?, pr, pass, &sha)?, json),
        Command::Stop { name, json } => outcome::print(&stop::stop(&current_dir()?, &name)?, json),
        Command::Cleanup { name, force, json } => {
            outcome::print(&cleanup::cleanup(&current_dir()?, &name, force)?, json)
        }
        Command::Resume { name, json } => {
            outcome::print(&stop::resume(&current_dir()?, &name)?, json)
        }
        Command::Claim { json } => outcome::print(&claim::claim(&current_dir()?)?, json),
        Command::Watch => watch::watch(&current_dir()?),
        Command::Resolve { name, json } => {
            outcome::print(&claim::resolve(&current_dir()?, &name)?, json)
        }
        Command::Init { json } => outcome::print(&setup::init(&current_dir()?)?, json),
        Command::Doctor { json } => {
            let report = setup::doctor(&current_dir()?);
            outcome::print(&report, json)?;
            match report.failed() {
                0 => Ok(()),
                1 => Err(Failure::Error("1 check failed".into())),
                n => Err(Failure::Error(format!("{n} checks failed"))),
            }
        }
        Command::Hook { hook } => match hook {
            Hook::WorkerGuard => guard::worker_guard(),
            Hook::PreCompact => {
                compact::pre_compact();
                Ok(())
            }
            Hook::SessionCompact => {
                compact::session_compact();
                Ok(())
            }
            Hook::PrePush { .. } => guard::pre_push(),
        },
    }
}

fn current_dir() -> Result<std::path::PathBuf, Failure> {
    std::env::current_dir()
        .map_err(|err| Failure::Error(format!("can't read the current directory: {err}")))
}
