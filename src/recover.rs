//! What a restarted supervisor checks before its first tick: each worker in
//! the state against the session records and the worktrees, so a worker
//! that can't come back is reported rather than waited on.

use std::path::Path;

use jiff::{SignedDuration, Timestamp};

use crate::observe::Observed;
use crate::outcome::Failure;
use crate::sessions::{self, Agent};
use crate::state::{State, Store};

/// A worker recorded this long ago without a launched session never will be.
const LAUNCH_GRACE: SignedDuration = SignedDuration::from_secs(120);

/// The workers that can't be resumed, with why. A worker already reported as
/// lost isn't reported again.
pub fn lost(state: &State, agents: &[Agent], now: Timestamp) -> Vec<(String, &'static str)> {
    let mut lost = Vec::new();
    for (name, worker) in &state.workers {
        if state.observed.get(name) == Some(&Observed::Lost) {
            continue;
        }
        let reason = if !worker.worktree.exists() {
            "its worktree is gone"
        } else if let Some(job_id) = &worker.job_id {
            if agents
                .iter()
                .any(|agent| agent.id.as_deref() == Some(job_id.as_str()))
            {
                continue;
            }
            "Claude Code no longer has its session"
        } else {
            let created = worker.created_at.parse::<Timestamp>().ok();
            if created.is_none_or(|created| now.duration_since(created) < LAUNCH_GRACE) {
                continue;
            }
            "its session never launched"
        };
        lost.push((name.clone(), reason));
    }
    lost
}

/// Records one event for each lost worker, then marks it lost, so it's
/// reported once.
pub fn reconcile(store: &Store) -> Result<Vec<String>, Failure> {
    let state = store.read()?;
    let agents = sessions::agents(Path::new("claude"))?;
    let lost = lost(&state, &agents, Timestamp::now());
    let mut lines = Vec::new();
    for (name, reason) in &lost {
        let line = format!(
            "{name} is lost: {reason}, so it can't be resumed: clean it up with `orchestral cleanup {name}`, and start its issue again"
        );
        store.record("worker", &line)?;
        lines.push(line);
    }
    store.update(|state| {
        for (name, _) in lost {
            if state.workers.contains_key(&name) {
                state.observed.insert(name, Observed::Lost);
            }
        }
    })?;
    Ok(lines)
}
