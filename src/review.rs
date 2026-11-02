//! `orchestral review`: a reviewer's verdict on one commit of a PR.
//!
//! A verdict holds only for the commit it names. After two failed rounds,
//! on two different commits, the PR's issue gets `blocked` and the fleet
//! moves on.

use std::fmt;
use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::outcome::Failure;
use crate::spawn::main_checkout;
use crate::state::{Review, Store};

/// Failed review rounds before the issue is blocked.
pub const ROUNDS: usize = 2;

/// The label for an issue the fleet has given up on until the owner looks.
pub const BLOCKED: &str = "blocked";

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
    let view: PrView = gh_json(
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
    let Some(issue) = state
        .workers
        .values()
        .find(|worker| worker.review_of == Some(pr))
        .map(|worker| worker.issue)
    else {
        return Err(Failure::Refused {
            reason: format!("PR #{pr} has no reviewer in this fleet"),
            fix: format!("start one with `orchestral spawn <name> --role reviewer --pr {pr}`"),
        });
    };
    let author = state
        .workers
        .iter()
        .find(|(_, worker)| worker.branch == view.head_ref_name && worker.review_of.is_none())
        .map(|(name, _)| name.clone());

    let review = store.update(|state| {
        let review = state.reviews.entry(pr).or_insert_with(|| Review {
            sha: head.clone(),
            passed,
            failed: Vec::new(),
        });
        review.sha = head.clone();
        review.passed = passed;
        // A second verdict on the same commit isn't another round.
        if !passed && !review.failed.contains(&head) {
            review.failed.push(head.clone());
        }
        review.clone()
    })?;
    let rounds = review.failed.len();
    let blocked = !passed && rounds >= ROUNDS;
    if blocked {
        gh(
            &main,
            &["issue", "edit", &issue.to_string(), "--add-label", BLOCKED],
        )?;
    }

    let fixer = author.as_deref().unwrap_or("its author");
    let subject = match &author {
        Some(worker) => format!("pr #{pr} (worker {worker})"),
        None => format!("pr #{pr}"),
    };
    let at = short(&head);
    let line = if passed {
        format!("{subject}: review passed at {at}: no action needed")
    } else if blocked {
        format!(
            "{subject}: review failed at {at} (round {rounds} of {ROUNDS}): issue #{issue} is now blocked, so stop {fixer} and move on"
        )
    } else {
        format!(
            "{subject}: review failed at {at} (round {rounds} of {ROUNDS}): have {fixer} fix it"
        )
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

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

fn gh(main: &Path, args: &[&str]) -> Result<Vec<u8>, Failure> {
    let output = Command::new("gh")
        .args(args)
        .current_dir(main)
        .output()
        .map_err(|err| Failure::Error(format!("can't run gh: {err}")))?;
    if !output.status.success() {
        return Err(Failure::Error(format!(
            "gh {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim_end()
        )));
    }
    Ok(output.stdout)
}

fn gh_json<T: serde::de::DeserializeOwned>(main: &Path, args: &[&str]) -> Result<T, Failure> {
    let stdout = gh(main, args)?;
    serde_json::from_slice(&stdout).map_err(|err| {
        Failure::Error(format!(
            "gh {} printed something unexpected: {err}",
            args.join(" ")
        ))
    })
}
