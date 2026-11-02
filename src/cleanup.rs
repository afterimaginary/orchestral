//! `orchestral cleanup`: removing a finished worker, its worktree, branch
//! and session.
//!
//! A worker is finished when its PR merged or closed. Git can't tell, since
//! a squash merge leaves the branch's own commits unmerged.

use std::fmt;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::outcome::Failure;
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
}

impl fmt::Display for CleanedUp {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Removed {}: its session, worktree and local branch.",
            self.name
        )
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pr {
    number: u64,
    state: String,
    head_ref_oid: String,
}

pub fn cleanup(cwd: &Path, name: &str, force: bool) -> Result<CleanedUp, Failure> {
    let store = Store::for_repo(cwd)?;
    let worker = store
        .read()?
        .workers
        .remove(name)
        .ok_or_else(|| Failure::Error(format!("there's no worker named {name}")))?;
    let main = main_checkout(store.dir())?;
    let pr = finished_pr(&main, name, &worker, force)?;
    if worker.worktree.exists() && !force {
        check_saved(name, &worker, pr.as_ref())?;
    }

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
    if agent.as_ref().is_some_and(|agent| agent.pid.is_some()) {
        crate::stop::stop(cwd, name)?;
    }
    if worker.worktree.exists() {
        let path = worker.worktree.display().to_string();
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(&path);
        git(&main, &args)?;
    }
    if git(
        &main,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{}", worker.branch),
        ],
    )
    .is_ok()
    {
        git(&main, &["branch", "-D", &worker.branch])?;
    }
    if let (Some(job_id), Some(_)) = (&worker.job_id, &agent) {
        remove_session(job_id)?;
    }

    store.update(|state| {
        state.workers.remove(name);
        state.observed.remove(name);
    })?;
    store.record("worker", &format!("{name} cleaned up"))?;
    Ok(CleanedUp {
        name: name.into(),
        pr: pr.map(|pr| pr.number),
    })
}

/// The worker's merged or closed PR, if it has one. Refuses while one is
/// open, unless `force`.
fn finished_pr(
    main: &Path,
    name: &str,
    worker: &Worker,
    force: bool,
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
            "number,state,headRefOid",
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
    if let Some(open) = prs.iter().find(|pr| pr.state == "OPEN")
        && !force
    {
        return Err(Failure::Refused {
            reason: format!("{name}'s PR #{} is still open", open.number),
            fix: format!("wait until it merges or closes, or stop {name} instead"),
        });
    }
    Ok(prs.into_iter().find(|pr| pr.state != "OPEN"))
}

/// Refuses when the worktree has changes or commits its PR doesn't have. A
/// worker with no PR, such as a reviewer, may have no commits of its own.
fn check_saved(name: &str, worker: &Worker, pr: Option<&Pr>) -> Result<(), Failure> {
    let refuse = |what: &str| Failure::Refused {
        reason: format!("{name}'s worktree has {what}"),
        fix: format!("run `orchestral cleanup {name} --force` to discard it"),
    };
    if !git(&worker.worktree, &["status", "--porcelain"])?
        .trim()
        .is_empty()
    {
        return Err(refuse("uncommitted changes"));
    }
    match pr {
        // The PR's head holds everything that was pushed, whatever the merge
        // did to the branch.
        Some(pr) => {
            let args = ["merge-base", "--is-ancestor", "HEAD", &pr.head_ref_oid];
            if git(&worker.worktree, &args).is_err() {
                return Err(refuse("commits its PR doesn't have"));
            }
        }
        None => {
            // Workers start from origin/main.
            let own = git(
                &worker.worktree,
                &["rev-list", "HEAD", "--not", "origin/main"],
            )?;
            if !own.trim().is_empty() {
                return Err(refuse("commits and no merged or closed PR"));
            }
        }
    }
    Ok(())
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

fn git(dir: &Path, args: &[&str]) -> Result<String, Failure> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|err| Failure::Error(format!("can't run git: {err}")))?;
    if !output.status.success() {
        return Err(Failure::Error(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim_end()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
