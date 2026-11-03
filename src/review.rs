//! `orchestral review`: the verdict of a PR's review on one commit, recorded
//! by the author after its own review subagent, or by a separate reviewer.
//!
//! A verdict holds only for the commit it names. After two failed rounds,
//! on two different commits, the PR's issue gets `blocked` and the fleet
//! moves on.

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::github::{BLOCKED, gh_json, gh_output};
use crate::outcome::Failure;
use crate::spawn::main_checkout;
use crate::state::Store;

/// Failed review rounds before the issue is blocked.
pub const ROUNDS: usize = 2;

#[derive(Debug, Serialize)]
pub struct Reviewed {
    pub pr: u64,
    pub sha: String,
    pub passed: bool,
    /// Failed rounds so far, this one included.
    pub failed_rounds: usize,
    /// Whether this verdict blocked the issue.
    pub blocked: bool,
}

impl fmt::Display for Reviewed {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let verdict = if self.passed { "passed" } else { "failed" };
        write!(
            f,
            "Recorded: PR #{} {verdict} review at {}.",
            self.pr,
            short(&self.sha)
        )?;
        if self.blocked {
            write!(
                f,
                " That was round {ROUNDS} of {ROUNDS}, so its issue is now blocked."
            )?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrView {
    state: String,
    head_ref_oid: String,
    head_ref_name: String,
}

pub fn record(cwd: &Path, pr: u64, passed: bool, sha: &str) -> Result<Reviewed, Failure> {
    let store = Store::for_repo(cwd)?;
    let main = main_checkout(store.dir())?;
    let gh = Path::new("gh");
    let view: PrView = gh_json(
        gh,
        &main,
        &[
            "pr",
            "view",
            &pr.to_string(),
            "--json",
            "state,headRefOid,headRefName",
        ],
    )?;
    if view.state != "OPEN" {
        return Err(Failure::Refused {
            reason: format!("PR #{pr} is {}", view.state.to_lowercase()),
            fix: "only an open PR can be reviewed".into(),
        });
    }
    let head = view.head_ref_oid;
    if sha.len() < 7 || !head.starts_with(sha) {
        return Err(Failure::Refused {
            reason: format!("PR #{pr}'s head is {}, not {sha}", short(&head)),
            fix: format!("review {} and record the verdict for it", short(&head)),
        });
    }
    let state = store.read()?;
    let reviewer = state
        .workers
        .iter()
        .find(|(_, worker)| worker.review_of == Some(pr))
        .map(|(name, worker)| (name.clone(), worker.issue));
    let author = state
        .workers
        .iter()
        .find(|(_, worker)| worker.branch == view.head_ref_name && worker.review_of.is_none())
        .map(|(name, worker)| (name.clone(), worker.issue));
    // Authors review their own PRs (ADR-0011); a separate reviewer, when
    // there is one, gives the verdict instead.
    let Some(issue) = reviewer
        .as_ref()
        .or(author.as_ref())
        .map(|(_, issue)| *issue)
    else {
        return Err(Failure::Refused {
            reason: format!("PR #{pr} has no worker in this fleet"),
            fix: "only the PR's author or its reviewer records a verdict".into(),
        });
    };
    let reviewer = reviewer.map(|(name, _)| name);
    let author = author.map(|(name, _)| name);

    // A second verdict on the same commit isn't another round.
    let mut failed = state.reviews.get(&pr).cloned().unwrap_or_default().failed;
    if !passed && !failed.contains(&head) {
        failed.push(head.clone());
    }
    let rounds = failed.len();
    let blocked = !passed && rounds >= ROUNDS;
    // Labelled before anything is recorded, so a failure here leaves the
    // verdict to be recorded again.
    if blocked {
        // A new repository may not have the label yet. Creating one that
        // exists fails, and the edit below reports anything else.
        let _ = gh_output(gh, &main, &["label", "create", BLOCKED]);
        gh_output(
            gh,
            &main,
            &["issue", "edit", &issue.to_string(), "--add-label", BLOCKED],
        )?;
    }
    store.update(|state| {
        let review = state.reviews.entry(pr).or_default();
        review.sha = head.clone();
        review.passed = passed;
        review.failed = failed;
    })?;

    let fixer = author.as_deref().unwrap_or("its author");
    let subject = match &author {
        Some(worker) => format!("pr #{pr} (worker {worker})"),
        None => format!("pr #{pr}"),
    };
    let at = short(&head);
    let line = match (&reviewer, passed, blocked) {
        (Some(reviewer), true, _) => {
            format!("{subject}: review passed at {at}: clean up {reviewer}")
        }
        (Some(reviewer), false, true) => format!(
            "{subject}: review failed at {at} (round {rounds} of {ROUNDS}): issue #{issue} is now blocked, so clean up {reviewer} and {fixer}, and move on"
        ),
        (Some(reviewer), false, false) => format!(
            "{subject}: review failed at {at} (round {rounds} of {ROUNDS}): clean up {reviewer}, and have {fixer} fix it"
        ),
        (None, true, _) => format!("{subject}: review passed at {at}: decide its findings"),
        (None, false, true) => format!(
            "{subject}: review failed at {at} (round {rounds} of {ROUNDS}): issue #{issue} is now blocked, so clean up {fixer} and move on"
        ),
        (None, false, false) => format!(
            "{subject}: review failed at {at} (round {rounds} of {ROUNDS}): {fixer} is fixing it"
        ),
    };
    store.record("pr", &line)?;
    Ok(Reviewed {
        pr,
        sha: head,
        passed,
        failed_rounds: rounds,
        blocked,
    })
}

pub(crate) fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}
