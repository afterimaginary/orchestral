//! The supervisor's check of worker sessions: what each one is doing, from
//! the session records, and an event whenever a worker needs someone.
//!
//! The last observation of each worker is kept in the state, so a restarted
//! supervisor carries on from it rather than reporting everything again.

use serde::{Deserialize, Serialize};

use crate::outcome::Failure;
use crate::sessions::{self, Record};
use crate::state::{State, Store};

/// What a worker's session is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Observed {
    /// Working, or running a Monitor.
    Busy,
    Idle,
    /// Waiting for input. Claude Code reports a blocked session this way,
    /// with what it waits for.
    Waiting,
    /// No live session has the worker's name.
    Exited,
    /// Can't be resumed: its worktree or session is gone. Set by a
    /// restarted supervisor, and kept until the worker is cleaned up.
    Lost,
}

impl Observed {
    fn from_record(record: Option<&Record>) -> Observed {
        let Some(record) = record else {
            return Observed::Exited;
        };
        match record.status.as_deref() {
            Some("idle") => Observed::Idle,
            Some("waiting") => Observed::Waiting,
            _ => Observed::Busy,
        }
    }
}

/// A worker whose session changed to something that needs a response.
#[derive(Debug, PartialEq)]
pub struct Change {
    pub name: String,
    pub observed: Observed,
    /// What a waiting session waits for, such as `input needed`.
    pub waiting_for: Option<String>,
}

impl Change {
    /// The event text: the worker, what happened, and what to do.
    pub fn text(&self) -> String {
        let name = &self.name;
        match self.observed {
            Observed::Idle => format!("{name} is idle: check its PR, or send it its next step"),
            Observed::Waiting => format!(
                "{name} is waiting for {}: see what it needs",
                self.waiting_for.as_deref().unwrap_or("input")
            ),
            Observed::Exited => format!(
                "{name}'s session exited: resume it with `orchestral resume {name}`, or clean it up"
            ),
            Observed::Busy => format!("{name} is working"),
            Observed::Lost => format!("{name} is lost"),
        }
    }
}

/// Compares each launched worker's session with its last observation. A
/// worker that just became busy needs nothing, so only changes to idle,
/// waiting or exited are returned, but every change is in `observations`.
pub fn changes(
    state: &State,
    records: &[Record],
    alive: impl Fn(&Record) -> bool,
) -> (Vec<Change>, Vec<(String, Observed)>) {
    let mut changes = Vec::new();
    let mut observations = Vec::new();
    for (name, worker) in &state.workers {
        // A worker isn't watched until its session has launched.
        if worker.job_id.is_none() {
            continue;
        }
        // A lost worker stays lost until it's cleaned up.
        if state.observed.get(name) == Some(&Observed::Lost) {
            continue;
        }
        let record = records
            .iter()
            .find(|record| record.name.as_deref() == Some(&worker.session_name) && alive(record));
        let observed = Observed::from_record(record);
        if state.observed.get(name) == Some(&observed) {
            continue;
        }
        observations.push((name.clone(), observed));
        if observed != Observed::Busy {
            changes.push(Change {
                name: name.clone(),
                observed,
                waiting_for: record.and_then(|record| record.waiting_for.clone()),
            });
        }
    }
    (changes, observations)
}

/// One check: reads the state and the session records, records an event for
/// each change, then saves the observations. Events are recorded first, so a
/// crash in between repeats an event rather than losing it.
pub fn check(store: &Store) -> Result<Vec<Change>, Failure> {
    let state = store.read()?;
    let records = match sessions::records_dir() {
        Some(dir) => sessions::read_records(&dir)?,
        None => Vec::new(),
    };
    let now_ms = jiff::Timestamp::now().as_millisecond();
    let (changes, observations) = changes(&state, &records, |record| {
        sessions::process(record.pid)
            .ok()
            .flatten()
            .is_some_and(|process| sessions::is_alive(record, Some(&process), now_ms))
    });
    for change in &changes {
        store.record("worker", &change.text())?;
    }
    store.update(|state| {
        for (name, observed) in observations {
            if state.workers.contains_key(&name) {
                state.observed.insert(name, observed);
            }
        }
        // Workers that are gone don't keep an observation.
        let names: Vec<String> = state.workers.keys().cloned().collect();
        state.observed.retain(|name, _| names.contains(name));
    })?;
    Ok(changes)
}
