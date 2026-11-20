//! `orchestral claim` and `orchestral resolve`: one orchestrator per
//! repository, and checked addresses for messaging fleet members.

use std::fmt;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::outcome::Failure;
use crate::sessions::{self, Record};
use crate::state::{Orchestrator, Store};
use crate::watch;

/// What `resolve` takes to mean the claimed orchestrator.
pub const ORCHESTRATOR: &str = "orchestrator";

#[derive(Debug, Serialize)]
pub struct Claimed {
    pub session_id: String,
    /// The holder whose claim was taken over, when it had ended.
    pub replaced: Option<String>,
    /// Problems that don't stop the claim but need fixing before workers
    /// start, such as no `watch` running for this session.
    pub warnings: Vec<String>,
}

impl fmt::Display for Claimed {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Session {} is the orchestrator", self.session_id)?;
        if let Some(old) = &self.replaced {
            write!(f, ", taking over from ended session {old}")?;
        }
        write!(f, ".")?;
        for warning in &self.warnings {
            write!(f, "\nwarning: {warning}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct Address {
    pub name: String,
    pub session_id: String,
    /// The `to` for SendMessage, and the `from` its messages carry.
    pub address: String,
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.address)
    }
}

pub fn claim(cwd: &Path) -> Result<Claimed, Failure> {
    let session_id = std::env::var("CLAUDE_CODE_SESSION_ID").unwrap_or_default();
    if session_id.is_empty() {
        return Err(Failure::Error(
            "CLAUDE_CODE_SESSION_ID isn't set; run claim from the Claude Code session that will orchestrate".into(),
        ));
    }
    let store = Store::for_repo(cwd)?;
    let records = live_records()?;
    let mut already_held = false;
    let mut claimed = store.update(|state| {
        let replaced = match &state.orchestrator {
            Some(holder) if holder.session_id == session_id => {
                already_held = true;
                None
            }
            Some(holder) if records.iter().any(|r| r.session_id == holder.session_id) => {
                return Err(Failure::Refused {
                    reason: format!("live session {} holds the claim", holder.session_id),
                    fix: "orchestrate from that session, or end it first".into(),
                });
            }
            Some(holder) => Some(holder.session_id.clone()),
            None => None,
        };
        state.orchestrator = Some(Orchestrator {
            session_id: session_id.clone(),
            claimed_at: jiff::Timestamp::now().to_string(),
        });
        Ok(Claimed {
            session_id: session_id.clone(),
            replaced,
            warnings: Vec::new(),
        })
    })??;
    if !already_held {
        store.record(
            "orchestrator",
            &format!("session {session_id} claimed the repository"),
        )?;
    }
    if !watch_running(&store, &session_id) {
        claimed.warnings.push(no_monitor_warning());
    }
    Ok(claimed)
}

/// Whether a `watch` for `session_id` is running. A watch beats only once
/// the store exists, and one may have just started, so it waits a couple of
/// polls for a beat before saying no.
fn watch_running(store: &Store, session_id: &str) -> bool {
    let wait = watch::POLL * 2;
    let start = Instant::now();
    loop {
        if watch::is_running(store, session_id) {
            return true;
        }
        if start.elapsed() >= wait {
            return false;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Events reach the orchestrator only through `watch`, so without one this
/// session would never hear from its workers.
fn no_monitor_warning() -> String {
    let watch = std::env::current_exe()
        .map(|exe| format!("`{} watch`", exe.display()))
        .unwrap_or_else(|_| "`orchestral watch` from the plugin's `bin`".into());
    format!(
        "no event monitor is running for this session, so fleet events won't reach it; \
         start {watch} as a persistent background monitor, or restart the session \
         so the plugin's `orchestral-events` monitor starts, then claim again"
    )
}

/// The address of a worker, or of the orchestrator for `orchestrator`.
pub fn resolve(cwd: &Path, name: &str) -> Result<Address, Failure> {
    let state = Store::for_repo(cwd)?.read()?;
    let live = live_records()?;
    let (matches, worktree): (Vec<&Record>, _) = if name == ORCHESTRATOR {
        let holder = state.orchestrator.ok_or_else(|| Failure::Refused {
            reason: "no session has claimed this repository".into(),
            fix: "run `orchestral claim` from the orchestrator's session".into(),
        })?;
        let matches = live
            .iter()
            .filter(|r| r.session_id == holder.session_id)
            .collect();
        (matches, None)
    } else {
        let worker = state
            .workers
            .get(name)
            .ok_or_else(|| Failure::Error(format!("there's no worker named {name}")))?;
        let matches = live
            .iter()
            .filter(|r| r.name.as_deref() == Some(worker.session_name.as_str()))
            .collect();
        (matches, Some(worker.worktree.clone()))
    };

    let record = match matches.as_slice() {
        [] => {
            return Err(Failure::Refused {
                reason: format!("{name} has no live session"),
                fix: format!("resume or restart {name} before messaging it"),
            });
        }
        [record] => *record,
        _ => {
            return Err(Failure::Refused {
                reason: format!("{} live sessions match {name}", matches.len()),
                fix: "stop the extra sessions so one remains".into(),
            });
        }
    };
    if let Some(worktree) = worktree
        && !same_dir(&record.cwd, &worktree)
    {
        return Err(Failure::Refused {
            reason: format!(
                "{name}'s session runs in {}, not its worktree {}",
                record.cwd.display(),
                worktree.display()
            ),
            fix: format!("stop that session and resume {name}"),
        });
    }
    let socket = record
        .messaging_socket_path
        .as_ref()
        .ok_or_else(|| Failure::Refused {
            reason: format!("{name}'s session has no messaging socket"),
            fix: "restart it with a Claude Code version that supports messaging".into(),
        })?;
    Ok(Address {
        name: name.into(),
        session_id: record.session_id.clone(),
        address: format!("uds:{}", socket.display()),
    })
}

/// Session records whose process is alive.
/// Whether a live session has `session_id`.
pub(crate) fn is_live(session_id: &str) -> Result<bool, Failure> {
    Ok(live_records()?
        .iter()
        .any(|record| record.session_id == session_id))
}

fn live_records() -> Result<Vec<Record>, Failure> {
    let Some(dir) = sessions::records_dir() else {
        return Ok(vec![]);
    };
    let now_ms = jiff::Timestamp::now().as_millisecond();
    let mut live = vec![];
    for record in sessions::read_records(&dir)? {
        let process = sessions::process(record.pid)?;
        if sessions::is_alive(&record, process.as_ref(), now_ms) {
            live.push(record);
        }
    }
    Ok(live)
}

/// Compares directories after resolving symlinks, such as macOS's `/tmp`.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}
