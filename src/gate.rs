//! Whether the fleet may take on another worker: the pause flag, the worker
//! cap, the plan limit and the fleet budget. Also `orchestral pause` and
//! `orchestral unpause`.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::budget::{self, Estimate};
use crate::limits::{self, Limits};
use crate::outcome::Failure;
use crate::policy::Settings;
use crate::sessions::Agent;
use crate::state::{State, Store};
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
    // Of the windows at the limit, the one that resets last is what spawning
    // waits on. A window with no reset time sorts last.
    let Some(window) = limits
        .windows
        .iter()
        .filter(|window| window.percent >= threshold)
        .max_by_key(|window| (window.resets.is_none(), window.resets.clone()))
    else {
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
        fix: "ask the owner to raise fleet_token_budget in .orchestral/policy.md".into(),
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
    let mut transcripts: Vec<PathBuf> = Vec::new();
    for worker in state.workers.values() {
        let session = agents
            .iter()
            .find(|agent| agent.id.is_some() && agent.id == worker.job_id);
        if let Some(path) = session
            .and_then(|agent| transcript::find(&projects, &worker.worktree, &agent.session_id))
        {
            transcripts.push(path);
        }
    }
    if let Some(orchestrator) = &state.orchestrator
        && let Some(path) = transcript::find(&projects, main, &orchestrator.session_id)
    {
        transcripts.push(path);
    }
    budget::fleet_usage(&transcripts, Some(started))
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
