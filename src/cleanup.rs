//! `orchestral cleanup`: removing a finished worker, its worktree, branch
//! and session.
//!
//! A worker is finished when its PR merged or closed, or failed its last
//! review round. Git can't tell, since a squash merge leaves the branch's own
//! commits unmerged.

use std::fmt;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::git::git;
use crate::outcome::Failure;
use crate::review::ROUNDS;
use crate::sessions;
use crate::spawn::main_checkout;
use crate::state::{Store, Worker};

/// `claude rm` is refused while the session's process still holds its lock,
/// which can outlast `claude stop`, so it's retried for this long.
const RM_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Serialize)]
pub struct CleanedUp {
    pub name: String,
    /// The merged or closed PR that finished the worker, if it had one.
    pub pr: Option<u64>,
    /// What was there to remove, of `session`, `worktree` and `branch`.
    pub removed: Vec<&'static str>,
}

impl fmt::Display for CleanedUp {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.removed.as_slice() {
            [] => write!(f, "Removed {} from the fleet.", self.name),
            removed => write!(f, "Removed {} and its {}.", self.name, removed.join(", ")),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pr {
    number: u64,
    state: String,
    head_ref_oid: String,
    is_cross_repository: bool,
}

pub fn cleanup(cwd: &Path, name: &str, force: bool) -> Result<CleanedUp, Failure> {
    let store = Store::for_repo(cwd)?;
    let mut state = store.read()?;
    let worker = state
        .workers
        .remove(name)
        .ok_or_else(|| Failure::Error(format!("there's no worker named {name}")))?;
    let main = main_checkout(store.dir())?;
    let blocked = |pr: u64| {
        state
            .reviews
            .get(&pr)
            .is_some_and(|review| review.failed.len() >= ROUNDS)
    };
    let pr = finished_pr(&main, name, &worker, force, blocked)?;

    let agent = match &worker.job_id {
        Some(job_id) => sessions::agents(Path::new("claude"))?
            .into_iter()
            .find(|agent| agent.id.as_deref() == Some(job_id.as_str())),
        None => None,
    };
    if let Some(agent) = &agent
        && agent.kind.as_deref() != Some("background")
    {
        return Err(Failure::Refused {
            reason: format!("{name}'s session isn't a background session"),
            fix: "manage it from where it runs".into(),
        });
    }
    // Stopped before the work is checked, so nothing lands after the check.
    if agent.as_ref().is_some_and(|agent| agent.pid.is_some()) {
        crate::stop::stop(cwd, name)?;
    }
    // A worktree whose directory is gone would hold its branch.
    git(&main, &["worktree", "prune"])?;
    let branch = format!("refs/heads/{}", worker.branch);
    let has_branch = git(&main, &["rev-parse", "--verify", "--quiet", &branch]).is_ok();
    if !force {
        check_saved(&main, name, &worker, has_branch, pr.as_ref())?;
    }

    let mut removed = Vec::new();
    if worker.worktree.exists() {
        let path = worker.worktree.display().to_string();
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(&path);
        git(&main, &args)?;
        removed.push("worktree");
    }
    if has_branch {
        git(&main, &["branch", "-D", &worker.branch])?;
        removed.push("branch");
    }
    if let (Some(job_id), Some(_)) = (&worker.job_id, &agent) {
        remove_session(job_id)?;
        removed.insert(0, "session");
    }

    store.update(|state| {
        state.workers.remove(name);
        state.proposals.remove(name);
        state.observed.remove(name);
    })?;
    store.record("worker", &format!("{name} cleaned up"))?;
    Ok(CleanedUp {
        name: name.into(),
        pr: pr.map(|pr| pr.number),
        removed,
    })
}

/// The worker's finished PR from this repository, if it has one: an open one
/// that failed its last review round, or else a merged or closed one,
/// preferring a merged one. Refuses while one is open otherwise, unless
/// `force`.
fn finished_pr(
    main: &Path,
    name: &str,
    worker: &Worker,
    force: bool,
    blocked: impl Fn(u64) -> bool,
) -> Result<Option<Pr>, Failure> {
    let output = Command::new("gh")
        .args([
            "pr",
            "list",
            "--head",
            &worker.branch,
            "--state",
            "all",
            "--json",
            "number,state,headRefOid,isCrossRepository",
        ])
        .current_dir(main)
        .output()
        .map_err(|err| Failure::Error(format!("can't run gh: {err}")))?;
    if !output.status.success() {
        return Err(Failure::Error(format!(
            "gh pr list: {}",
            String::from_utf8_lossy(&output.stderr).trim_end()
        )));
    }
    let prs: Vec<Pr> = serde_json::from_slice(&output.stdout)
        .map_err(|err| Failure::Error(format!("gh pr list printed something unexpected: {err}")))?;
    // A fork's PR can share the branch name.
    let mut prs: Vec<Pr> = prs
        .into_iter()
        .filter(|pr| !pr.is_cross_repository)
        .collect();
    if let Some(open) = prs.iter().position(|pr| pr.state == "OPEN")
        && blocked(prs[open].number)
    {
        return Ok(Some(prs.swap_remove(open)));
    }
    if let Some(open) = prs.iter().find(|pr| pr.state == "OPEN")
        && !force
    {
        return Err(Failure::Refused {
            reason: format!("{name}'s PR #{} is still open", open.number),
            fix: format!("wait until it merges or closes, or stop {name} instead"),
        });
    }
    // gh lists the newest first.
    prs.retain(|pr| pr.state != "OPEN");
    let merged = prs.iter().position(|pr| pr.state == "MERGED");
    Ok(match merged {
        Some(index) => Some(prs.swap_remove(index)),
        None => prs.into_iter().next(),
    })
}

/// Refuses when the worktree has changes, or the branch has commits its PR
/// doesn't have. A worker with no PR, such as a reviewer, may have no
/// commits of its own: its branch must not have moved from `origin/main`,
/// where it started.
fn check_saved(
    main: &Path,
    name: &str,
    worker: &Worker,
    has_branch: bool,
    pr: Option<&Pr>,
) -> Result<(), Failure> {
    let refuse = |what: &str| Failure::Refused {
        reason: format!("{name}'s worktree has {what}"),
        fix: format!("run `orchestral cleanup {name} --force` to discard it"),
    };
    if worker.worktree.exists()
        && !git(&worker.worktree, &["status", "--porcelain"])?
            .trim()
            .is_empty()
    {
        return Err(refuse("uncommitted changes"));
    }
    if !has_branch {
        return Ok(());
    }
    let branch = format!("refs/heads/{}", worker.branch);
    match pr {
        // The PR's head holds everything that was pushed, whatever the merge
        // did to the branch.
        Some(pr) => {
            let head = format!("{}^{{commit}}", pr.head_ref_oid);
            if git(main, &["cat-file", "-e", &head]).is_err() {
                git(
                    main,
                    &["fetch", "-q", "origin", &format!("pull/{}/head", pr.number)],
                )?;
            }
            if !is_ancestor(main, &branch, &pr.head_ref_oid)? {
                return Err(refuse("commits its PR doesn't have"));
            }
        }
        None => {
            if !is_ancestor(main, &branch, "origin/main")? {
                return Err(refuse("commits and no merged or closed PR"));
            }
        }
    }
    Ok(())
}

/// Whether `commit` is in `of`'s history. Any failure but a plain "no" is
/// an error.
fn is_ancestor(dir: &Path, commit: &str, of: &str) -> Result<bool, Failure> {
    let status = Command::new("git")
        .args(["merge-base", "--is-ancestor", commit, of])
        .current_dir(dir)
        .status()
        .map_err(|err| Failure::Error(format!("can't run git: {err}")))?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(Failure::Error(format!(
            "git merge-base --is-ancestor {commit} {of} failed"
        ))),
    }
}

/// `claude rm`, retried while the stopped session still holds its lock.
fn remove_session(job_id: &str) -> Result<(), Failure> {
    let deadline = Instant::now() + RM_TIMEOUT;
    loop {
        let output = Command::new("claude")
            .args(["rm", job_id])
            .output()
            .map_err(|err| Failure::Error(format!("can't run claude: {err}")))?;
        if output.status.success() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Failure::Error(format!(
                "claude rm {job_id}: {}",
                String::from_utf8_lossy(&output.stderr).trim_end()
            )));
        }
        thread::sleep(Duration::from_millis(500));
    }
}
