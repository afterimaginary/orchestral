//! `orchestral watch`: the one way events reach the orchestrator. A plugin
//! monitor runs it in every session, and each line it prints wakes the
//! session, even when idle.
//!
//! It prints only while its session holds the claim, and prints each event
//! after `watch-cursor`, the last `seq` delivered. A session that takes over
//! the claim carries on from there, so no event is lost.

use std::fs::{self, File};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::thread;
use std::time::Duration;

use crate::outcome::Failure;
use crate::state::{Event, Store};

/// How often it checks the claim and new events. Under 2 seconds, so it
/// starts printing within 2 seconds of a claim.
const POLL: Duration = Duration::from_secs(1);

/// Runs until its session ends: stdout closes, or the session that started
/// it exits and leaves it orphaned.
pub fn watch(cwd: &Path) -> Result<(), Failure> {
    let session_id = std::env::var("CLAUDE_CODE_SESSION_ID").unwrap_or_default();
    let mut store = None;
    let mut events = Events::default();
    let mut stdout = std::io::stdout().lock();
    loop {
        // A silent watch never writes, so it can't notice stdout closing.
        if std::os::unix::process::parent_id() == 1 {
            return Ok(());
        }
        // Outside a git repository it stays silent, but the directory may
        // become one later.
        if store.is_none() {
            store = Store::for_repo(cwd).ok();
        }
        if let Some(store) = &store
            && !session_id.is_empty()
        {
            let (lines, last) = deliverable(store, &session_id, &mut events);
            for line in lines {
                writeln!(stdout, "{line}")
                    .and_then(|()| stdout.flush())
                    .map_err(|err| Failure::Error(format!("can't print events: {err}")))?;
            }
            // Events already printed aren't read again by this process, so
            // a cursor it can't save only matters to a successor.
            if let Some(seq) = last
                && let Err(err) = save_cursor(store, seq)
            {
                eprintln!("error: can't save the watch cursor: {err}");
            }
        }
        thread::sleep(POLL);
    }
}

/// The lines to print now, events after the cursor while `session_id` holds
/// the claim, and the last `seq` among them. Anything unreadable counts as
/// nothing to print, since a state written by a newer orchestral or a store
/// not set up yet are both silent cases.
fn deliverable(store: &Store, session_id: &str, events: &mut Events) -> (Vec<String>, Option<u64>) {
    let Ok(state) = store.read() else {
        return (Vec::new(), None);
    };
    if state
        .orchestrator
        .is_none_or(|holder| holder.session_id != session_id)
    {
        return (Vec::new(), None);
    }
    let cursor = valid_cursor(store, state.next_seq);
    let Ok(new) = events.read_new(&store.dir().join("events.jsonl")) else {
        return (Vec::new(), None);
    };
    let new: Vec<Event> = new
        .into_iter()
        .filter(|event| event.seq > cursor.unwrap_or(0))
        .collect();
    // An event is one line, whatever its text holds.
    let lines = new
        .iter()
        .map(|event| event.text.replace(['\r', '\n'], " "))
        .collect();
    (lines, new.last().map(|event| event.seq))
}

/// Reads `events.jsonl` from where the last read ended, since it only grows.
#[derive(Default)]
struct Events {
    offset: u64,
}

impl Events {
    /// The complete lines added since the last read. A line still being
    /// written has no newline yet, and is read next time.
    fn read_new(&mut self, path: &Path) -> std::io::Result<Vec<Event>> {
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        // A file shorter than before was replaced, so read it from the start.
        if file.metadata()?.len() < self.offset {
            self.offset = 0;
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        let Some(end) = text.rfind('\n') else {
            return Ok(Vec::new());
        };
        self.offset += end as u64 + 1;
        // A line torn by a crash isn't an event, so it's skipped.
        Ok(text[..end]
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect())
    }
}

/// The cursor, unless it's at or past the next `seq`: then it outlived a
/// reset state, and would hide the new events.
fn valid_cursor(store: &Store, next_seq: u64) -> Option<u64> {
    read_cursor(store).filter(|&cursor| cursor < next_seq)
}

fn read_cursor(store: &Store) -> Option<u64> {
    fs::read_to_string(store.dir().join("watch-cursor"))
        .ok()
        .and_then(|text| text.trim().parse().ok())
}

/// Saves the cursor after the lines are printed, so a crash in between
/// repeats an event rather than losing it. During a handoff the old holder
/// can still be printing, so the cursor never moves back.
fn save_cursor(store: &Store, seq: u64) -> std::io::Result<()> {
    let next_seq = store.read().map_or(0, |state| state.next_seq);
    if valid_cursor(store, next_seq).is_some_and(|cursor| cursor >= seq) {
        return Ok(());
    }
    let temp = store.dir().join("watch-cursor.tmp");
    fs::write(&temp, format!("{seq}\n"))?;
    fs::rename(&temp, store.dir().join("watch-cursor"))
}
