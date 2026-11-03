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
use crate::policy::{MergeMethod, Policy, Settings};
use crate::review::short;
use crate::spawn::main_checkout;
use crate::state::{MergeRoute, State, Store};

/// Paths a supervisor merge may not change.
pub(crate) const HARD_CHECKED: [&str; 2] = [".orchestral/", ".github/"];

/// The owner's route's own condition.
const APPROVED: &str = "approved by the owner";

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
    /// `None` when no check succeeded, even if none failed.
    pub ci: Ci,
    /// The paths the PR changes, with the old path of a rename. Read only
    /// for the supervisor route.
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

    /// Whether only the owner's approval is missing, so the PR waits on them.
    pub fn waits_on_owner(&self) -> bool {
        self.route == MergeRoute::Human
            && self
                .conditions
                .iter()
                .all(|condition| condition.passed || condition.name == APPROVED)
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
            Ci::None => Some("no check has succeeded".into()),
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
    let route = route(state, pr.number);
    match route {
        MergeRoute::Human => add(
            APPROVED,
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

/// A PR's route. With no record, it's the owner's.
pub fn route(state: &State, pr: u64) -> MergeRoute {
    state
        .routes
        .get(&pr)
        .map_or(MergeRoute::Human, |route| route.to)
}

/// From the supervisor's minute tick, after the poll: merges each open PR
/// whose merge check passes, at the head commit it passed for (ADR-0003).
/// The snapshot rules out PRs that can't pass before GitHub is asked about
/// them. A refused merge is checked again from scratch next minute. Returns
/// lines for the log.
pub fn merge_ready(store: &Store, gh: &Path) -> Result<Vec<String>, Failure> {
    let Some(prs) = github::open_pr_snapshot(store)? else {
        return Ok(Vec::new());
    };
    let state = store.read()?;
    let main = main_checkout(store.dir())?;
    let settings = Policy::load(&main)?.settings;
    let candidates: Vec<(u64, github::Pr)> = prs
        .into_iter()
        .filter(|(number, pr)| {
            let reviewed = state
                .reviews
                .get(number)
                .is_some_and(|review| review.passed && review.sha == pr.head);
            let approved = state.approvals.get(number) == Some(&pr.head);
            let supervised =
                settings.supervisor_merges && route(&state, *number) == MergeRoute::Supervisor;
            !state.paused
                && !state.holds.contains(number)
                && !pr.draft
                && pr.ci == Ci::Passed
                && reviewed
                && (approved || supervised)
        })
        .collect();
    let mut lines = Vec::new();
    for (number, snapshot_pr) in candidates {
        match merge_one(store, gh, &main, &settings, number) {
            Ok(None) => {}
            Ok(Some(head)) => {
                let worker = state
                    .workers
                    .iter()
                    .find(|(_, worker)| worker.branch == snapshot_pr.branch)
                    .map(|(name, _)| name.as_str());
                let subject = github::pr_subject(number, &snapshot_pr, worker);
                let merged = format!("merged by the supervisor at {}", short(&head));
                let line = github::merged_line(&subject, &merged, worker);
                // It merged, so a failure here doesn't hold up the others.
                if let Err(err) = store
                    .record("pr", &line)
                    .and_then(|_| github::forget_pr(store, number))
                {
                    lines.push(format!(
                        "PR #{number} merged, but recording it failed: {}",
                        crate::supervisor::failure_text(&err)
                    ));
                }
                lines.push(line);
            }
            Err(err) => lines.push(format!(
                "can't merge PR #{number}: {}",
                crate::supervisor::failure_text(&err)
            )),
        }
    }
    Ok(lines)
}

/// Runs the merge check on one PR and merges it if it passes. Returns the
/// head commit it merged.
fn merge_one(
    store: &Store,
    gh: &Path,
    main: &Path,
    settings: &Settings,
    number: u64,
) -> Result<Option<String>, Failure> {
    // Read again, so a hold or pause from a moment ago counts.
    let state = store.read()?;
    let mut pr = read_pr(gh, main, number)?;
    if route(&state, number) == MergeRoute::Supervisor {
        pr.files = changed_files(gh, main, number)?;
    }
    if !evaluate(&pr, &state, settings).mergeable() {
        return Ok(None);
    }
    let method = match settings.merge_method {
        MergeMethod::Squash => "--squash",
        MergeMethod::Merge => "--merge",
        MergeMethod::Rebase => "--rebase",
    };
    gh_output(
        gh,
        main,
        &[
            "pr",
            "merge",
            &number.to_string(),
            method,
            "--match-head-commit",
            &pr.head,
        ],
    )?;
    Ok(Some(pr.head))
}

/// `orchestral merge-check <pr>`.
pub fn check(cwd: &Path, number: u64) -> Result<MergeCheck, Failure> {
    let store = Store::for_repo(cwd)?;
    let main = main_checkout(store.dir())?;
    let settings = Policy::load(cwd)?.settings;
    let state = store.read()?;
    let gh = Path::new("gh");
    let mut pr = read_pr(gh, &main, number)?;
    if route(&state, number) == MergeRoute::Supervisor {
        pr.files = changed_files(gh, &main, number)?;
    }
    Ok(evaluate(&pr, &state, &settings))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrView {
    state: String,
    is_draft: bool,
    base_ref_name: String,
    mergeable: String,
    head_ref_oid: String,
    body: String,
    #[serde(default)]
    status_check_rollup: Vec<GhCheck>,
}

/// Reads a PR with `gh`, without the paths it changes.
pub fn read_pr(gh: &Path, main: &Path, number: u64) -> Result<Pr, Failure> {
    let view: PrView = gh_json(
        gh,
        main,
        &[
            "pr",
            "view",
            &number.to_string(),
            "--json",
            "state,isDraft,baseRefName,mergeable,headRefOid,body,statusCheckRollup",
        ],
    )?;
    let ci = match github::ci(&view.status_check_rollup) {
        Ci::Passed if !github::succeeded(&view.status_check_rollup) => Ci::None,
        ci => ci,
    };
    Ok(Pr {
        number,
        state: view.state,
        draft: view.is_draft,
        base: view.base_ref_name,
        mergeable: view.mergeable,
        head: view.head_ref_oid,
        body: view.body,
        ci,
        files: Vec::new(),
    })
}

/// The paths a PR changes, and the old path of each rename, so moving a
/// file out of a checked directory still counts as changing it.
pub(crate) fn changed_files(gh: &Path, main: &Path, number: u64) -> Result<Vec<String>, Failure> {
    let endpoint = format!("repos/{{owner}}/{{repo}}/pulls/{number}/files");
    let output = gh_output(
        gh,
        main,
        &[
            "api",
            &endpoint,
            "--paginate",
            "--jq",
            ".[] | .filename, (.previous_filename // empty)",
        ],
    )?;
    Ok(String::from_utf8_lossy(&output)
        .lines()
        .map(str::to_owned)
        .collect())
}
