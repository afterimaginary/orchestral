//! Whether the fleet may take on another worker: the pause flag, the worker
//! cap, the plan limit and the fleet budget. Also `orchestral pause` and
//! `orchestral unpause`.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::budget::{self, Estimate};
use crate::limits::{self, Limits, Window};
use crate::outcome::Failure;
use crate::policy::Settings;
use crate::sessions::Agent;
use crate::state::{State, Store, Worker};
use crate::transcript;

/// Refuses while the fleet is paused or at its worker cap. Cheap enough to
/// check again under the lock, right before a worker is recorded.
pub fn check_capacity(state: &State, settings: &Settings) -> Result<(), Failure> {
    if state.paused {
        return Err(Failure::Refused {
            reason: "the fleet is paused".into(),
            fix: "run `orchestral unpause`".into(),
        });
    }
    if state.workers.len() >= settings.workers as usize {
        return Err(Failure::Refused {
            reason: format!("the fleet is at its cap of {} workers", settings.workers),
            fix: "wait until a worker is cleaned up after its PR merges".into(),
        });
    }
    Ok(())
}

/// Refuses when any plan window is at or past `plan_limit_percent`. Limits
/// that can't be read never refuse; the caller warns instead.
pub fn check_plan_limits(limits: &Limits, settings: &Settings) -> Result<(), Failure> {
    let threshold = settings.plan_limit_percent;
    let Some(window) = limiting_window(limits, threshold) else {
        return Ok(());
    };
    Err(Failure::Refused {
        reason: format!(
            "the {} window is {}% used, at or past plan_limit_percent ({threshold}%)",
            window.name, window.percent
        ),
        fix: match &window.resets {
            Some(resets) => format!(
                "wait until it resets at {}",
                resets.strftime(limits::RESET_FORMAT)
            ),
            None => "wait until it resets".into(),
        },
    })
}

/// Refuses while the supervisor has the fleet wound down at a plan limit and
/// the limits can't be read to check it again.
pub fn check_wound_down(state: &State, limits: &Limits) -> Result<(), Failure> {
    match &state.wound_down {
        Some(wound) if !limits.available => Err(Failure::Refused {
            reason: format!(
                "the fleet is wound down for the {} window, and the plan limits can't be read to check it",
                wound.window
            ),
            fix: "wait for the supervisor to record that the window has reset".into(),
        }),
        _ => Ok(()),
    }
}

/// Of the windows at or past `threshold`, the one that resets last, which is
/// what spawning waits on. A window with no reset time sorts last.
pub fn limiting_window(limits: &Limits, threshold: u8) -> Option<&Window> {
    limits
        .windows
        .iter()
        .filter(|window| window.percent >= threshold)
        .max_by_key(|window| (window.resets.is_none(), window.resets.clone()))
}

/// Refuses when the run has used its `fleet_token_budget`.
pub fn check_budget(used: &Estimate, settings: &Settings) -> Result<(), Failure> {
    if used.estimated_tokens < settings.fleet_token_budget {
        return Ok(());
    }
    Err(Failure::Refused {
        reason: format!(
            "the fleet has used {used}, and fleet_token_budget is {}",
            settings.fleet_token_budget
        ),
        fix: "ask the owner to raise fleet_token_budget in the policy".into(),
    })
}

/// What the run has used: every fleet session's transcript since the run
/// started, finding worker sessions in `agents`. Before a run has started,
/// nothing is read.
pub fn fleet_usage(state: &State, main: &Path, agents: &[Agent]) -> Result<Estimate, Failure> {
    let Some(started) = &state.run_started_at else {
        return Ok(Estimate::default());
    };
    let started = started
        .parse()
        .map_err(|err| Failure::Error(format!("run_started_at {started} is invalid: {err}")))?;
    let Some(projects) = transcript::projects_dir() else {
        return Ok(Estimate::default());
    };
    let mut transcripts: Vec<PathBuf> = state
        .workers
        .values()
        .filter_map(|worker| worker_transcript(&projects, worker, agents))
        .collect();
    if let Some(orchestrator) = &state.orchestrator
        && let Some(path) = transcript::find(&projects, main, &orchestrator.session_id)
    {
        transcripts.push(path);
    }
    budget::fleet_usage(&transcripts, Some(started))
}

/// What worker `name` has used this run since its design proposal started,
/// or `None` when it isn't on one or its transcript can't be found.
pub fn proposal_usage(
    state: &State,
    name: &str,
    agents: &[Agent],
) -> Result<Option<Estimate>, Failure> {
    let (Some(worker), Some(since)) = (state.workers.get(name), state.proposals.get(name)) else {
        return Ok(None);
    };
    let since: jiff::Timestamp = since.parse().map_err(|err| {
        Failure::Error(format!("{name}'s proposal start {since} is invalid: {err}"))
    })?;
    // Only this run counts, as for the fleet's usage.
    let run: Option<jiff::Timestamp> = state
        .run_started_at
        .as_deref()
        .and_then(|started| started.parse().ok());
    let since = run.map_or(since, |run| run.max(since));
    let Some(path) = transcript::projects_dir()
        .and_then(|projects| worker_transcript(&projects, worker, agents))
    else {
        return Ok(None);
    };
    budget::fleet_usage(&[path], Some(since)).map(Some)
}

/// A worker's transcript, through its session in `agents`.
fn worker_transcript(projects: &Path, worker: &Worker, agents: &[Agent]) -> Option<PathBuf> {
    let agent = agents
        .iter()
        .find(|agent| agent.id.is_some() && agent.id == worker.job_id)?;
    transcript::find(projects, &worker.worktree, &agent.session_id)
}

/// Whether the fleet is paused, after `pause` or `unpause`.
#[derive(Debug, Serialize)]
pub struct Paused {
    pub paused: bool,
}

impl fmt::Display for Paused {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.paused {
            write!(
                f,
                "The fleet is paused. Spawns are refused until `orchestral unpause`."
            )
        } else {
            write!(f, "The fleet isn't paused.")
        }
    }
}

/// Sets the pause flag. Changing it records an event; setting it to what it
/// already is doesn't.
pub fn set_paused(cwd: &Path, paused: bool) -> Result<Paused, Failure> {
    let store = Store::for_repo(cwd)?;
    let changed = store.update(|state| std::mem::replace(&mut state.paused, paused) != paused)?;
    if changed {
        let text = if paused {
            "the fleet is paused"
        } else {
            "the fleet is unpaused"
        };
        store.record("pause", text)?;
    }
    Ok(Paused { paused })
}
