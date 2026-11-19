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

fn asked_already(pr: u64) -> Failure {
    Failure::Refused {
        reason: format!("the owner has already been asked to merge PR #{pr}"),
        fix: "leave it on the owner's route".into(),
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
            return Err(asked_already(pr));
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
    // Checked again under the lock, in case the supervisor asked the owner
    // meanwhile.
    let recorded = store.update(|state| {
        if to == MergeRoute::Supervisor && state.asked.contains(&pr) {
            return false;
        }
        state.routes.insert(
            pr,
            Route {
                to,
                reason: reason.to_owned(),
            },
        );
        true
    })?;
    if !recorded {
        return Err(asked_already(pr));
    }
    Ok(Routed {
        pr,
        to,
        reason: reason.to_owned(),
    })
}

/// From the supervisor's minute tick, after the poll: labels each open PR on
/// the owner's route that only needs their approval with `needs-discussion`,
/// records that they were asked, and records an event, once per PR. A PR that
/// was asked about and no longer looks ready, after a push, a failed check or
/// a return to draft, loses the label and is no longer asked about. Forgets
/// PRs that are no longer open. Returns lines for the log.
pub fn ask_owner(store: &Store, gh: &Path, config: &Path) -> Result<Vec<String>, Failure> {
    let Some(prs) = github::open_pr_snapshot(store)? else {
        return Ok(Vec::new());
    };
    let main = main_checkout(store.dir())?;
    let state = store.read()?;
    // What the snapshot shows without asking GitHub again.
    let ready = |number: u64, pr: &github::Pr| {
        !pr.draft
            && pr.ci == Ci::Passed
            && merge::route(&state, number) == MergeRoute::Human
            && state
                .reviews
                .get(&number)
                .is_some_and(|review| review.passed && review.sha == pr.head)
    };
    let candidates: Vec<u64> = prs
        .iter()
        .filter(|&(number, pr)| !state.asked.contains(number) && ready(*number, pr))
        .map(|(number, _)| *number)
        .collect();
    let lapsed: Vec<u64> = prs
        .iter()
        .filter(|&(number, pr)| state.asked.contains(number) && !ready(*number, pr))
        .map(|(number, _)| *number)
        .collect();
    let mut lines = Vec::new();
    let mut asked = Vec::new();
    if !candidates.is_empty() {
        let settings = Policy::load_from(config, &main)?.settings;
        for number in candidates {
            let labelled = merge::read_pr(gh, &main, number).and_then(|pr| {
                if !merge::evaluate(&pr, &state, &settings).waits_on_owner() {
                    return Ok(false);
                }
                // A new repository may not have the label yet. Creating one
                // that exists fails, and the edit below reports anything else.
                let _ = gh_output(gh, &main, &["label", "create", ESCALATION]);
                let n = number.to_string();
                gh_output(gh, &main, &["pr", "edit", &n, "--add-label", ESCALATION])?;
                Ok(true)
            });
            match labelled {
                Ok(true) => asked.push(number),
                Ok(false) => {}
                // Tried again next minute.
                Err(err) => lines.push(format!(
                    "can't ask the owner to merge PR #{number}: {}",
                    crate::supervisor::failure_text(&err)
                )),
            }
        }
    }
    let mut withdrawn = Vec::new();
    for number in lapsed {
        let n = number.to_string();
        match gh_output(gh, &main, &["pr", "edit", &n, "--remove-label", ESCALATION]) {
            Ok(_) => withdrawn.push(number),
            Err(err) => lines.push(format!(
                "can't remove {ESCALATION} from PR #{number}: {}",
                crate::supervisor::failure_text(&err)
            )),
        }
    }
    // A PR routed to the supervisor meanwhile isn't asked about.
    let asked = store.update(|state| {
        state
            .asked
            .retain(|number| prs.contains_key(number) && !withdrawn.contains(number));
        let asked: Vec<u64> = asked
            .into_iter()
            .filter(|&number| merge::route(state, number) == MergeRoute::Human)
            .collect();
        state.asked.extend(asked.iter().copied());
        asked
    })?;
    for number in asked {
        let line = format!(
            "pr #{number}: ready for the owner to merge, and labelled {ESCALATION}: nothing to do until they approve it"
        );
        store.record("pr", &line)?;
        lines.push(line);
    }
    for number in withdrawn {
        lines.push(format!(
            "PR #{number} is no longer ready for the owner to merge, so {ESCALATION} was removed"
        ));
    }
    Ok(lines)
}
