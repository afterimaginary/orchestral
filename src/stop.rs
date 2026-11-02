//! `orchestral stop` and `orchestral resume`: pausing a worker and picking it
//! up later, with its worktree and conversation intact.

use std::fmt;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::outcome::Failure;
use crate::sessions::{self, Agent};
use crate::spawn::parse_job_id;
use crate::state::{Store, Worker};

/// `claude stop` returns before the session's process exits, and a resume
/// before then starts a copy, so stop waits up to this long.
const EXIT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Serialize)]
pub struct Stopped {
    pub name: String,
    pub was_running: bool,
}

impl fmt::Display for Stopped {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.was_running {
            write!(
                f,
                "Stopped {}. Its worktree and conversation are kept.",
                self.name
            )
        } else {
            write!(f, "{} was already stopped.", self.name)
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Resumed {
    pub name: String,
    pub job_id: String,
}

impl fmt::Display for Resumed {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Resumed {} (job {}). It's idle until it gets a message.",
            self.name, self.job_id
        )
    }
}

pub fn stop(cwd: &Path, name: &str) -> Result<Stopped, Failure> {
    let store = Store::for_repo(cwd)?;
    let (_, agent) = find(&store, name)?;
    if agent.kind.as_deref() != Some("background") {
        return Err(Failure::Refused {
            reason: format!("{name}'s session isn't a background session"),
            fix: "stop it from where it runs".into(),
        });
    }
    let Some(pid) = agent.pid else {
        return Ok(Stopped {
            name: name.into(),
            was_running: false,
        });
    };
    let job_id = agent.id.clone().unwrap_or_default();
    run_claude(&["stop", &job_id])?;
    let deadline = Instant::now() + EXIT_TIMEOUT;
    while sessions::process(pid)?.is_some() {
        if Instant::now() >= deadline {
            return Err(Failure::Error(format!(
                "{name}'s session (pid {pid}) is still running after `claude stop`"
            )));
        }
        thread::sleep(Duration::from_millis(200));
    }
    store.record("worker", &format!("{name} stopped"))?;
    Ok(Stopped {
        name: name.into(),
        was_running: true,
    })
}

pub fn resume(cwd: &Path, name: &str) -> Result<Resumed, Failure> {
    let store = Store::for_repo(cwd)?;
    let (worker, agent) = find(&store, name)?;
    if agent.pid.is_some() {
        return Err(Failure::Refused {
            reason: format!("{name} is still running"),
            fix: format!("send it a message, or run `orchestral stop {name}` first"),
        });
    }
    // Any other flag starts a copy under a new id, and a bare resume keeps
    // the options the worker was started with.
    let output = Command::new("claude")
        .args(["--bg", "--resume", &agent.session_id])
        .current_dir(&worker.worktree)
        .output()
        .map_err(|err| Failure::Error(format!("can't run claude: {err}")))?;
    let printed = String::from_utf8_lossy(&output.stdout);
    let job_id = match parse_job_id(&printed) {
        Some(id) if output.status.success() && Some(&id) == agent.id.as_ref() => id,
        _ => {
            return Err(Failure::Error(format!(
                "claude --bg --resume didn't resume {name}: {}{}",
                printed.trim_end(),
                String::from_utf8_lossy(&output.stderr).trim_end()
            )));
        }
    };
    store.record("worker", &format!("{name} resumed"))?;
    Ok(Resumed {
        name: name.into(),
        job_id,
    })
}

/// The worker named `name` and its session in `claude agents`.
fn find(store: &Store, name: &str) -> Result<(Worker, Agent), Failure> {
    let worker = store
        .read()?
        .workers
        .remove(name)
        .ok_or_else(|| Failure::Error(format!("there's no worker named {name}")))?;
    let Some(job_id) = worker.job_id.clone() else {
        return Err(Failure::Error(format!("{name} never started")));
    };
    let agent = sessions::agents(Path::new("claude"))?
        .into_iter()
        .find(|agent| agent.id.as_deref() == Some(job_id.as_str()))
        .ok_or_else(|| {
            Failure::Error(format!(
                "`claude agents` doesn't list {name}'s session {job_id}"
            ))
        })?;
    Ok((worker, agent))
}

fn run_claude(args: &[&str]) -> Result<(), Failure> {
    let output = Command::new("claude")
        .args(args)
        .output()
        .map_err(|err| Failure::Error(format!("can't run claude: {err}")))?;
    if !output.status.success() {
        return Err(Failure::Error(format!(
            "claude {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim_end()
        )));
    }
    Ok(())
}
