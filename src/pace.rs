//! Winding the fleet down at a plan limit or the budget, and back up after
//! a plan window resets. The supervisor calls these from its ticks; each
//! records one event when the fleet's pace changes, and nothing otherwise.

use jiff::Timestamp;

use crate::budget::Estimate;
use crate::gate;
use crate::limits::{self, Limits, Window};
use crate::outcome::Failure;
use crate::state::{Store, Usage, WoundDown};

/// Compares the plan windows with `plan_limit_percent`. Past it, the fleet
/// winds down: spawns are refused, and running workers finish their turn.
/// It resumes once a read shows that window below the limit, or, when no
/// read shows it, once the window's reset time has passed.
///
/// The event is recorded before the state, so a crash between the two
/// repeats it rather than losing it.
pub fn plan_limits(
    store: &Store,
    limits: &Limits,
    threshold: u8,
    now: Timestamp,
) -> Result<Option<String>, Failure> {
    let over = gate::limiting_window(limits, threshold);
    let wound_down = store.read()?.wound_down;
    let (line, next) = match (&wound_down, over) {
        (None, None) => return Ok(None),
        (None, Some(window)) => {
            let until = match &window.resets {
                Some(resets) => format!(
                    "until it resets at {}",
                    resets.strftime(limits::RESET_FORMAT)
                ),
                None => "until it resets".into(),
            };
            let line = format!(
                "limits: the {} window is {}% used, past plan_limit_percent ({threshold}%): start nothing new {until}, and let workers finish their turn",
                window.name, window.percent
            );
            (Some(line), Some(wound(window)))
        }
        // Still wound down, perhaps now for another window.
        (Some(_), Some(window)) => (None, Some(wound(window))),
        (Some(wound), None) => {
            let read_below = limits
                .windows
                .iter()
                .any(|window| window.name == wound.window && window.percent < threshold);
            let reset_passed = wound
                .resets_at
                .as_deref()
                .and_then(|at| at.parse::<Timestamp>().ok())
                .is_some_and(|at| at <= now);
            if !read_below && !reset_passed {
                return Ok(None);
            }
            let line = format!(
                "limits: the {} window has reset: start ready work again",
                wound.window
            );
            (Some(line), None)
        }
    };
    if next == wound_down {
        return Ok(None);
    }
    if let Some(line) = &line {
        store.record("limits", line)?;
    }
    store.update(|state| state.wound_down = next)?;
    Ok(line)
}

fn wound(window: &Window) -> WoundDown {
    WoundDown {
        window: window.name.clone(),
        resets_at: window
            .resets
            .as_ref()
            .map(|resets| resets.timestamp().to_string()),
    }
}

/// Saves the run's usage, and the first time in a run that it reaches
/// `budget`, records that the fleet stops for the owner. Spawns are already
/// refused past the budget, so this only tells the orchestrator, once.
pub fn budget(
    store: &Store,
    used: &Estimate,
    budget: u64,
    now: Timestamp,
) -> Result<Option<String>, Failure> {
    let reached = used.estimated_tokens >= budget;
    // Once a run: the total can fall back below the budget when a finished
    // worker is cleaned up, and the owner has already been asked.
    let first = reached && !store.read()?.budget_reached;
    let line = first.then(|| {
        format!(
            "budget: {used} of {budget} used this run: start nothing new, let workers finish their turn, and ask the owner how to continue"
        )
    });
    if let Some(line) = &line {
        store.record("budget", line)?;
    }
    store.update(|state| {
        state.usage = Some(Usage {
            estimated_tokens: used.estimated_tokens,
            at: now.to_string(),
        });
        state.budget_reached |= reached;
    })?;
    Ok(line)
}
