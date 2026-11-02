//! The merge check: every condition a PR must meet before it merges, at its
//! head commit. `orchestral merge-check` prints it and never merges.
//!
//! A PR goes to the owner unless it's routed to the supervisor. Either way it
//! must be open, ready, on `main`, free of conflicts, not held, with passing
//! CI, a passing review and every finding decided. The owner's route also
//! needs their approval of the head commit. The supervisor's needs
//! `supervisor_merges` on, and no change to `.orchestral/` or `.github/`,
//! which are the owner's to review.

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::findings;
use crate::github::{self, Ci, GhCheck, gh_json, gh_output};
use crate::outcome::Failure;
use crate::policy::{Policy, Settings};
use crate::spawn::main_checkout;
use crate::state::{MergeRoute, State, Store};

/// Paths a supervisor merge may not change.
const HARD_CHECKED: [&str; 2] = [".orchestral/", ".github/"];

/// What the check reads about a PR from GitHub.
#[derive(Debug, Clone)]
pub struct Pr {
    pub number: u64,
    /// `OPEN`, `CLOSED` or `MERGED`.
    pub state: String,
    pub draft: bool,
    pub base: String,
    /// `MERGEABLE`, `CONFLICTING` or `UNKNOWN`.
    pub mergeable: String,
    pub head: String,
    pub body: String,
    pub ci: Ci,
    /// The paths the PR changes.
    pub files: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Condition {
    pub name: &'static str,
    pub passed: bool,
    /// Why it failed. Empty when it passed.
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct MergeCheck {
    pub pr: u64,
    pub head: String,
    pub route: MergeRoute,
    pub conditions: Vec<Condition>,
}

impl MergeCheck {
    pub fn mergeable(&self) -> bool {
        self.conditions.iter().all(|condition| condition.passed)
    }

    fn failed(&self) -> usize {
        self.conditions.iter().filter(|c| !c.passed).count()
    }
}

impl fmt::Display for MergeCheck {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let route = match self.route {
            MergeRoute::Human => "the owner",
            MergeRoute::Supervisor => "the supervisor",
        };
        write!(
            f,
            "PR #{} at {}, merged by {route}:",
            self.pr,
            short(&self.head)
        )?;
        for condition in &self.conditions {
            match condition.passed {
                true => write!(f, "\n  pass  {}", condition.name)?,
                false => write!(f, "\n  FAIL  {}: {}", condition.name, condition.detail)?,
            }
        }
        match self.failed() {
            0 => write!(f, "\nIt can merge."),
            n => write!(
                f,
                "\nIt can't merge yet: {n} condition{} failed.",
                if n == 1 { "" } else { "s" }
            ),
        }
    }
}

/// Every condition for `pr`, from the state and the policy's settings.
pub fn evaluate(pr: &Pr, state: &State, settings: &Settings) -> MergeCheck {
    let mut conditions = Vec::new();
    let mut add = |name, failure: Option<String>| {
        conditions.push(Condition {
            name,
            passed: failure.is_none(),
            detail: failure.unwrap_or_default(),
        })
    };
    add(
        "not paused",
        state.paused.then(|| "the fleet is paused".into()),
    );
    add(
        "open",
        (pr.state != "OPEN").then(|| format!("it's {}", pr.state.to_lowercase())),
    );
    add("not a draft", pr.draft.then(|| "it's a draft".into()));
    add(
        "targets main",
        (pr.base != "main").then(|| format!("it targets {}", pr.base)),
    );
    add(
        "no merge conflict",
        match pr.mergeable.as_str() {
            "MERGEABLE" => None,
            "CONFLICTING" => Some("it conflicts with main".into()),
            _ => Some("GitHub hasn't worked out yet whether it conflicts".into()),
        },
    );
    add(
        "not held",
        state
            .holds
            .contains(&pr.number)
            .then(|| "the owner put it on hold".into()),
    );
    add(
        "CI passed",
        match pr.ci {
            Ci::Passed => None,
            Ci::None => Some("it has no checks".into()),
            Ci::Pending => Some("checks are still running".into()),
            Ci::Failed => Some("a check failed".into()),
        },
    );
    add(
        "review passed",
        match state.reviews.get(&pr.number) {
            Some(review) if review.sha == pr.head && review.passed => None,
            Some(review) if review.sha == pr.head => Some("the review failed".into()),
            _ => Some("no review has passed at the head commit".into()),
        },
    );
    add(
        "findings decided",
        findings::check(&pr.body)
            .err()
            .map(|problem| problem.to_string()),
    );
    let route = state
        .routes
        .get(&pr.number)
        .map_or(MergeRoute::Human, |route| route.to);
    match route {
        MergeRoute::Human => add(
            "approved by the owner",
            match state.approvals.get(&pr.number) {
                Some(sha) if *sha == pr.head => None,
                Some(_) => Some("the approval was for an earlier commit".into()),
                None => Some("the owner hasn't approved it".into()),
            },
        ),
        MergeRoute::Supervisor => {
            add(
                "supervisor merges allowed",
                (!settings.supervisor_merges)
                    .then(|| "the policy's supervisor_merges is off".into()),
            );
            let hard: Vec<&str> = pr
                .files
                .iter()
                .filter(|path| HARD_CHECKED.iter().any(|dir| path.starts_with(dir)))
                .map(String::as_str)
                .collect();
            add(
                "no change to .orchestral/ or .github/",
                (!hard.is_empty()).then(|| format!("it changes {}", hard.join(", "))),
            );
        }
    }
    MergeCheck {
        pr: pr.number,
        head: pr.head.clone(),
        route,
        conditions,
    }
}

/// `orchestral merge-check <pr>`.
pub fn check(cwd: &Path, number: u64) -> Result<MergeCheck, Failure> {
    let store = Store::for_repo(cwd)?;
    let main = main_checkout(store.dir())?;
    let settings = Policy::load(cwd)?.settings;
    let pr = read_pr(Path::new("gh"), &main, number)?;
    Ok(evaluate(&pr, &store.read()?, &settings))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrView {
    number: u64,
    state: String,
    is_draft: bool,
    base_ref_name: String,
    mergeable: String,
    head_ref_oid: String,
    body: String,
    #[serde(default)]
    status_check_rollup: Vec<GhCheck>,
}

/// Reads a PR, and the paths it changes, with `gh`.
pub fn read_pr(gh: &Path, main: &Path, number: u64) -> Result<Pr, Failure> {
    let view: PrView = gh_json(
        gh,
        main,
        &[
            "pr",
            "view",
            &number.to_string(),
            "--json",
            "number,state,isDraft,baseRefName,mergeable,headRefOid,body,statusCheckRollup",
        ],
    )?;
    let files = gh_output(
        gh,
        main,
        &["pr", "diff", &number.to_string(), "--name-only"],
    )?;
    Ok(Pr {
        number: view.number,
        state: view.state,
        draft: view.is_draft,
        base: view.base_ref_name,
        mergeable: view.mergeable,
        head: view.head_ref_oid,
        body: view.body,
        ci: github::ci(&view.status_check_rollup),
        files: String::from_utf8_lossy(&files)
            .lines()
            .map(str::to_owned)
            .collect(),
    })
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}
