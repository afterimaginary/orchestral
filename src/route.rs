//! `orchestral route`: who merges a PR, the owner or the supervisor, and
//! asking the owner once a PR on their route only needs their approval.
//!
//! A PR without a route is the owner's. Only the orchestrator routes. Once the
//! owner has been asked to merge a PR, it stays theirs.

use std::fmt;
use std::path::Path;

use serde::Serialize;

use crate::github::{self, Ci, ESCALATION, gh_json, gh_output};
use crate::merge::{self, HARD_CHECKED};
use crate::outcome::Failure;
use crate::policy::Policy;
use crate::spawn::main_checkout;
use crate::state::{MergeRoute, Route, Store};

#[derive(Debug, Serialize)]
pub struct Routed {
    pub pr: u64,
    pub to: MergeRoute,
    pub reason: String,
}

impl fmt::Display for Routed {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let who = match self.to {
            MergeRoute::Human => "the owner merges it",
            MergeRoute::Supervisor => "the supervisor may merge it",
        };
        write!(f, "Routed PR #{}: {who} ({}).", self.pr, self.reason)
    }
}

#[derive(serde::Deserialize)]
struct PrState {
    state: String,
}

/// `orchestral route <pr> human|supervisor --reason <text>`.
pub fn route(cwd: &Path, pr: u64, to: MergeRoute, reason: &str) -> Result<Routed, Failure> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(Failure::Refused {
            reason: "the reason is empty".into(),
            fix: "say why with --reason".into(),
        });
    }
    let store = Store::for_repo(cwd)?;
    let main = main_checkout(store.dir())?;
    let state = store.read()?;
    let session = std::env::var("CLAUDE_CODE_SESSION_ID").unwrap_or_default();
    let orchestrator = state.orchestrator.as_ref().map(|o| o.session_id.as_str());
    if session.is_empty() || orchestrator != Some(session.as_str()) {
        return Err(Failure::Refused {
            reason: "only the orchestrator routes PRs".into(),
            fix: "run it from the session that holds the claim".into(),
        });
    }
    let gh = Path::new("gh");
    let view: PrState = gh_json(
        gh,
        &main,
        &["pr", "view", &pr.to_string(), "--json", "state"],
    )?;
    if view.state != "OPEN" {
        return Err(Failure::Refused {
            reason: format!("PR #{pr} is {}", view.state.to_lowercase()),
            fix: "only an open PR can be routed".into(),
        });
    }
    if to == MergeRoute::Supervisor {
        if state.asked.contains(&pr) {
            return Err(Failure::Refused {
                reason: format!("the owner has already been asked to merge PR #{pr}"),
                fix: "leave it on the owner's route".into(),
            });
        }
        if !Policy::load(cwd)?.settings.supervisor_merges {
            return Err(Failure::Refused {
                reason: "the policy's supervisor_merges is off".into(),
                fix: "route it to the owner".into(),
            });
        }
        let hard: Vec<String> = merge::changed_files(gh, &main, pr)?
            .into_iter()
            .filter(|path| HARD_CHECKED.iter().any(|dir| path.starts_with(dir)))
            .collect();
        if !hard.is_empty() {
            return Err(Failure::Refused {
                reason: format!(
                    "PR #{pr} changes {}, which are the owner's to review",
                    hard.join(", ")
                ),
                fix: "route it to the owner".into(),
            });
        }
    }
    store.update(|state| {
        state.routes.insert(
            pr,
            Route {
                to,
                reason: reason.to_owned(),
            },
        );
    })?;
    Ok(Routed {
        pr,
        to,
        reason: reason.to_owned(),
    })
}

/// From the supervisor's minute tick, after the poll: labels each open PR on
/// the owner's route that only needs their approval with `needs-discussion`,
/// records that they were asked, and records an event, once per PR. Forgets
/// PRs that are no longer open. Returns lines for the log.
pub fn ask_owner(store: &Store, gh: &Path) -> Result<Vec<String>, Failure> {
    let Some(prs) = github::open_pr_snapshot(store)? else {
        return Ok(Vec::new());
    };
    let main = main_checkout(store.dir())?;
    let state = store.read()?;
    let candidates: Vec<u64> = prs
        .iter()
        .filter(|(number, pr)| {
            !pr.draft
                && pr.ci == Ci::Passed
                && !state.asked.contains(number)
                && merge::route(&state, **number) == MergeRoute::Human
                && state
                    .reviews
                    .get(number)
                    .is_some_and(|review| review.passed && review.sha == pr.head)
        })
        .map(|(number, _)| *number)
        .collect();
    let mut asked = Vec::new();
    if !candidates.is_empty() {
        let settings = Policy::load(&main)?.settings;
        for number in candidates {
            let pr = merge::read_pr(gh, &main, number)?;
            if !merge::evaluate(&pr, &state, &settings).waits_on_owner() {
                continue;
            }
            // A new repository may not have the label yet. Creating one that
            // exists fails, and the edit below reports anything else.
            let _ = gh_output(gh, &main, &["label", "create", ESCALATION]);
            gh_output(
                gh,
                &main,
                &["pr", "edit", &number.to_string(), "--add-label", ESCALATION],
            )?;
            asked.push(number);
        }
    }
    let mut lines = Vec::new();
    store.update(|state| {
        state.asked.retain(|number| prs.contains_key(number));
        state.asked.extend(asked.iter().copied());
    })?;
    for number in asked {
        let line = format!(
            "pr #{number}: ready for the owner to merge, and labelled {ESCALATION}: nothing to do until they approve it"
        );
        store.record("pr", &line)?;
        lines.push(line);
    }
    Ok(lines)
}
