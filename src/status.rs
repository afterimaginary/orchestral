//! `orchestral status`: one view of the fleet, as text for people and as
//! JSON for any later interface.
//!
//! A part that can't be read, such as GitHub or `claude agents`, is left
//! empty with a warning, so the rest still shows.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use jiff::Timestamp;
use serde::Serialize;
use serde_json::Value;

use crate::budget::Estimate;
use crate::gate;
use crate::limits::{self, Limits};
use crate::observe::Observed;
use crate::outcome::Failure;
use crate::policy::Policy;
use crate::preview;
use crate::sessions::{self, Activity, Agent};
use crate::spawn::main_checkout;
use crate::state::{State, Store, Want};
use crate::transcript::{self, Context};

#[derive(Debug, Serialize)]
pub struct Status {
    pub paused: bool,
    pub workers: Vec<WorkerStatus>,
    /// The policy's `workers`.
    pub worker_cap: u32,
    pub limits: Limits,
    pub budget: Budget,
    /// The latest review verdict on each open PR that has one.
    pub reviews: Vec<ReviewStatus>,
    /// The previews that are running.
    pub previews: Vec<PreviewStatus>,
    /// Parts that couldn't be read, and why.
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct WorkerStatus {
    pub name: String,
    pub issue: u64,
    /// The open PR from the worker's branch.
    pub pr: Option<u64>,
    /// Whether that PR is a draft.
    pub draft: bool,
    pub session_name: String,
    /// `running`, `stopped`, or `unknown` when `claude agents` doesn't list
    /// the session. `lost` when the supervisor found it can't be resumed.
    pub session: &'static str,
    /// `working`, `waiting`, `done`, `failed` or `unknown`.
    pub activity: &'static str,
    /// What the session waits for, such as `input needed`.
    pub waiting_for: Option<String>,
    pub context: Option<Context>,
    /// What the next message will cost: the context re-read from the cache
    /// when it's warm, or written to it again when it's cold.
    pub wake_cost: Option<Estimate>,
    /// Whether it's still on its issue's design proposal.
    pub proposing: bool,
    /// What its proposal has used so far, when it's on one and that's known.
    pub proposal_used: Option<Estimate>,
}

#[derive(Debug, Serialize)]
pub struct ReviewStatus {
    pub pr: u64,
    /// The head commit the verdict is for.
    pub sha: String,
    pub passed: bool,
}

#[derive(Debug, Serialize)]
pub struct PreviewStatus {
    pub pr: u64,
    pub branch: String,
    pub url: String,
}

#[derive(Debug, Serialize)]
pub struct Budget {
    /// Null when the usage couldn't be read.
    pub used: Option<Estimate>,
    pub fleet_token_budget: u64,
    pub run_started_at: Option<String>,
    /// What design proposals used this run: the finished ones and the open
    /// ones so far. Null when an open one's usage couldn't be read.
    pub proposals: Option<Estimate>,
}

pub fn status(cwd: &Path) -> Result<Status, Failure> {
    let store = Store::for_repo(cwd)?;
    let main = main_checkout(store.dir())?;
    let policy = Policy::load(cwd)?;
    let state = store.read()?;
    let mut warnings = Vec::new();

    let agents = sessions::agents(Path::new("claude"))
        .map_err(|err| warnings.push(format!("sessions are unknown: {}", message(&err))));
    let prs = open_prs()
        .map_err(|err| warnings.push(format!("PRs are unknown: {err}")))
        .unwrap_or_default();
    // Without the sessions, worker usage can't be found, so it's unknown
    // rather than undercounted.
    let used = match &agents {
        Ok(agents) => gate::fleet_usage(&state, &main, agents)
            .map_err(|err| warnings.push(format!("budget usage is unknown: {}", message(&err))))
            .ok(),
        Err(()) if state.run_started_at.is_none() => Some(Estimate::default()),
        Err(()) => None,
    };
    let agents = agents.unwrap_or_default();
    let limits = limits::read();
    let workers = workers(&state, &agents, &prs, Timestamp::now(), &mut warnings);
    let proposals = workers
        .iter()
        .filter(|worker| worker.proposing)
        .try_fold(state.proposal_tokens, |total, worker| {
            Some(total + worker.proposal_used.as_ref()?.estimated_tokens)
        })
        .map(|estimated_tokens| Estimate { estimated_tokens });

    Ok(Status {
        paused: state.paused,
        workers,
        worker_cap: policy.settings.workers,
        limits,
        budget: Budget {
            used,
            fleet_token_budget: policy.settings.fleet_token_budget,
            run_started_at: state.run_started_at.clone(),
            proposals,
        },
        reviews: state
            .reviews
            .iter()
            .filter(|(pr, review)| {
                !review.sha.is_empty() && prs.iter().any(|(_, number, _)| number == *pr)
            })
            .map(|(pr, review)| ReviewStatus {
                pr: *pr,
                sha: review.sha.clone(),
                passed: review.passed,
            })
            .collect(),
        previews: state
            .previews
            .iter()
            .filter(|(_, preview)| preview.want == Want::Running && preview.failure.is_none())
            .filter_map(|(pr, preview)| {
                Some(PreviewStatus {
                    pr: *pr,
                    branch: preview.branch.clone(),
                    url: preview::link(preview.server.as_ref()?),
                })
            })
            .collect(),
        warnings,
    })
}

fn workers(
    state: &State,
    agents: &[Agent],
    prs: &[(String, u64, bool)],
    now: Timestamp,
    warnings: &mut Vec<String>,
) -> Vec<WorkerStatus> {
    let projects = transcript::projects_dir();
    let organization = organization();
    state
        .workers
        .iter()
        .map(|(name, worker)| {
            let agent = agents
                .iter()
                .find(|agent| agent.id.is_some() && agent.id == worker.job_id);
            let context = agent.and_then(|agent| {
                let path =
                    transcript::find(projects.as_deref()?, &worker.worktree, &agent.session_id)?;
                transcript::read(&path, now, organization.as_deref())
                    .map_err(|err| {
                        warnings.push(format!("{name}'s context is unknown: {}", message(&err)))
                    })
                    .ok()
            });
            let pr = prs.iter().find(|(branch, _, _)| *branch == worker.branch);
            let (activity, waiting_for) = match agent.map(Agent::activity) {
                Some(Activity::Working) => ("working", None),
                Some(Activity::Waiting(reason)) => ("waiting", Some(reason)),
                Some(Activity::Done) => ("done", None),
                Some(Activity::Failed) => ("failed", None),
                Some(Activity::Unknown) | None => ("unknown", None),
            };
            WorkerStatus {
                name: name.clone(),
                issue: worker.issue,
                pr: pr.map(|(_, number, _)| *number),
                draft: pr.is_some_and(|(_, _, draft)| *draft),
                session_name: worker.session_name.clone(),
                session: match agent {
                    _ if state.observed.get(name) == Some(&Observed::Lost) => "lost",
                    Some(agent) if agent.pid.is_some() => "running",
                    Some(_) => "stopped",
                    None => "unknown",
                },
                activity,
                waiting_for,
                wake_cost: context.as_ref().map(wake_cost),
                context,
                proposing: state.proposals.contains_key(name),
                proposal_used: gate::proposal_usage(state, name, agents)
                    .map_err(|err| {
                        warnings.push(format!(
                            "{name}'s proposal usage is unknown: {}",
                            message(&err)
                        ))
                    })
                    .ok()
                    .flatten(),
            }
        })
        .collect()
}

/// The next request sends the whole context. Warm, it's read from the cache
/// at 0.1; cold, it's written again at the rate of the cache lifetime the
/// session last used, the weights of [the budget](crate::budget).
pub fn wake_cost(context: &Context) -> Estimate {
    let weight = match (context.warm, context.cache_ttl) {
        (true, _) => 0.1,
        (false, Some(3600)) => 2.0,
        (false, _) => 1.25,
    };
    Estimate {
        estimated_tokens: (context.tokens as f64 * weight).round() as u64,
    }
}

/// Each open PR's branch, number, and whether it's a draft.
fn open_prs() -> Result<Vec<(String, u64, bool)>, String> {
    let output = Command::new("gh")
        .args([
            "pr",
            "list",
            "--state",
            "open",
            "--limit",
            "200",
            "--json",
            "number,headRefName,isDraft",
        ])
        .output()
        .map_err(|err| format!("can't run gh: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "gh pr list failed: {}",
            String::from_utf8_lossy(&output.stderr).trim_end()
        ));
    }
    let prs: Vec<Value> = serde_json::from_slice(&output.stdout)
        .map_err(|err| format!("unexpected `gh pr list` output: {err}"))?;
    Ok(prs
        .iter()
        .filter_map(|pr| {
            Some((
                pr["headRefName"].as_str()?.to_string(),
                pr["number"].as_u64()?,
                pr["isDraft"].as_bool().unwrap_or(false),
            ))
        })
        .collect())
}

/// The account's `oauthAccount.organizationType`, which decides some context
/// windows.
pub(crate) fn organization() -> Option<String> {
    let path = match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) => PathBuf::from(dir).join(".claude.json"),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".claude.json"),
    };
    let config: Value = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
    Some(
        config["oauthAccount"]["organizationType"]
            .as_str()?
            .to_string(),
    )
}

fn message(failure: &Failure) -> String {
    match failure {
        Failure::Error(message) => message.clone(),
        Failure::Refused { reason, .. } => reason.clone(),
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(
            f,
            "Fleet: {}",
            if self.paused { "paused" } else { "not paused" }
        )?;
        match (&self.budget.used, &self.budget.run_started_at) {
            (_, None) => writeln!(
                f,
                "Budget: no run has started; {} tokens per run",
                self.budget.fleet_token_budget
            )?,
            (Some(used), Some(_)) => writeln!(
                f,
                "Budget: {used} of {} used this run",
                self.budget.fleet_token_budget
            )?,
            (None, Some(_)) => writeln!(
                f,
                "Budget: usage unknown, of {}",
                self.budget.fleet_token_budget
            )?,
        }
        match &self.budget.proposals {
            Some(used) if used.estimated_tokens > 0 => {
                writeln!(f, "Design proposals: {used} this run")?
            }
            Some(_) => {}
            None => writeln!(f, "Design proposals: usage unknown")?,
        }
        if self.limits.available {
            writeln!(f, "Plan limits:")?;
            for line in self.limits.to_string().lines() {
                writeln!(f, "  {line}")?;
            }
        } else {
            writeln!(f, "{}", self.limits)?;
        }
        write!(f, "Workers: {} of {}", self.workers.len(), self.worker_cap)?;
        for worker in &self.workers {
            write!(f, "\n  {}", worker)?;
        }
        if !self.reviews.is_empty() {
            write!(f, "\nReviews:")?;
        }
        for review in &self.reviews {
            write!(
                f,
                "\n  PR #{} {} at {}",
                review.pr,
                if review.passed { "passed" } else { "failed" },
                &review.sha[..review.sha.len().min(7)]
            )?;
        }
        if !self.previews.is_empty() {
            write!(f, "\nPreviews:")?;
        }
        for preview in &self.previews {
            write!(
                f,
                "\n  PR #{} ({}) at {}",
                preview.pr, preview.branch, preview.url
            )?;
        }
        for warning in &self.warnings {
            write!(f, "\nwarning: {warning}")?;
        }
        Ok(())
    }
}

impl fmt::Display for WorkerStatus {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} · issue #{}", self.name, self.issue)?;
        if let Some(pr) = self.pr {
            write!(f, " · PR #{pr}")?;
            if self.draft {
                write!(f, " (draft)")?;
            }
        }
        if self.proposing {
            match &self.proposal_used {
                Some(used) => write!(f, " · proposal, {used}")?,
                None => write!(f, " · proposal")?,
            }
        }
        write!(f, " · {}", self.session)?;
        match &self.waiting_for {
            Some(reason) => write!(f, ", waiting: {reason}")?,
            None if self.activity != "unknown" => write!(f, ", {}", self.activity)?,
            None => {}
        }
        if let Some(context) = &self.context {
            write!(f, " · context {} tokens", context.tokens)?;
            if let Some(percent) = context.percent {
                write!(f, " ({percent:.0}%)")?;
            }
            write!(f, " · cache {}", if context.warm { "warm" } else { "cold" })?;
        }
        if let Some(cost) = &self.wake_cost {
            write!(f, " · wake {cost}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(tokens: u64, warm: bool, cache_ttl: Option<u64>) -> Context {
        Context {
            tokens,
            model: None,
            identity: None,
            window: None,
            percent: None,
            last_request_at: None,
            compactions: Vec::new(),
            cache_ttl,
            warm,
        }
    }

    #[test]
    fn waking_reads_a_warm_cache_and_rewrites_a_cold_one() {
        let cost = |c: &Context| wake_cost(c).estimated_tokens;
        assert_eq!(cost(&context(100_000, true, Some(3600))), 10_000);
        assert_eq!(cost(&context(100_000, false, Some(3600))), 200_000);
        assert_eq!(cost(&context(100_000, false, Some(300))), 125_000);
        assert_eq!(cost(&context(100_000, false, None)), 125_000);
    }
}
