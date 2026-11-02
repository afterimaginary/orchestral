//! Winding the fleet down at a plan limit or the budget, and back up after
//! a plan window resets. The supervisor calls these from its ticks; each
//! records one event when the fleet's pace changes, and nothing otherwise.

use jiff::Timestamp;

use crate::budget::Estimate;
use crate::gate;
use crate::limits::{self, Limits};
use crate::outcome::Failure;
use crate::state::{Store, Usage, WoundDown};

/// Compares the plan windows with `plan_limit_percent`. Past it, the fleet
/// winds down: spawns are refused, and running workers finish their turn.
/// It resumes once a read shows the window below the limit, or, when the
/// limits can't be read, once the window's reset time has passed.
pub fn plan_limits(
    store: &Store,
    limits: &Limits,
    threshold: u8,
    now: Timestamp,
) -> Result<Option<String>, Failure> {
    let over = gate::limiting_window(limits, threshold);
    let line = store.update(|state| match (&state.wound_down, over) {
        (None, Some(window)) => {
            state.wound_down = Some(WoundDown {
                window: window.name.clone(),
                resets_at: window
                    .resets
                    .as_ref()
                    .map(|resets| resets.timestamp().to_string()),
            });
            let until = match &window.resets {
                Some(resets) => format!(
                    "until it resets at {}",
                    resets.strftime(limits::RESET_FORMAT)
                ),
                None => "until it resets".into(),
            };
            Some(format!(
                "limits: the {} window is {}% used, past plan_limit_percent ({threshold}%): start nothing new {until}, and let workers finish their turn",
                window.name, window.percent
            ))
        }
        (Some(wound), None) => {
            let reset = if limits.available {
                true
            } else {
                // Unreadable limits only end a wind-down by the clock.
                wound
                    .resets_at
                    .as_deref()
                    .and_then(|at| at.parse::<Timestamp>().ok())
                    .is_some_and(|at| at <= now)
            };
            if !reset {
                return None;
            }
            let line = format!(
                "limits: the {} window has reset: start ready work again",
                wound.window
            );
            state.wound_down = None;
            Some(line)
        }
        _ => None,
    })?;
    if let Some(line) = &line {
        store.record("limits", line)?;
    }
    Ok(line)
}

/// Saves the run's usage, and the first time it reaches `budget`, records
/// that the fleet stops for the owner. Spawns are already refused past the
/// budget, so this only tells the orchestrator once.
pub fn budget(
    store: &Store,
    used: &Estimate,
    budget: u64,
    now: Timestamp,
) -> Result<Option<String>, Failure> {
    let reached = used.estimated_tokens >= budget;
    let first = store.update(|state| {
        state.usage = Some(Usage {
            estimated_tokens: used.estimated_tokens,
            at: now.to_string(),
        });
        let first = reached && !state.budget_reached;
        state.budget_reached = reached;
        first
    })?;
    if !first {
        return Ok(None);
    }
    let line = format!(
        "budget: {used} of {budget} used this run: start nothing new, let workers finish their turn, and ask the owner how to continue"
    );
    store.record("budget", &line)?;
    Ok(Some(line))
}
