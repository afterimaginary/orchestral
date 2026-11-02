//! The idle heartbeat: a `heartbeat` event when nothing else has reached the
//! orchestrator for 55 minutes, so its 1-hour prompt cache stays warm.
//! Waking it warm costs far less than waking it cold.
//!
//! Heartbeats stop with the supervisor, which exits once no worker is open
//! and `--keepalive-hours` have passed.

use std::fs::File;
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::path::Path;

use jiff::{SignedDuration, Timestamp};

use crate::outcome::Failure;
use crate::state::{Event, State, Store};

/// Under the 1-hour cache's lifetime, with time for the turn to start.
pub const INTERVAL: SignedDuration = SignedDuration::from_secs(55 * 60);

/// Whether a heartbeat is due `now`: a session holds the claim, and it has
/// had nothing for `INTERVAL`. Its last turn is the latest of the last event
/// recorded (every event reaches it within seconds, and refreshes its cache
/// as a heartbeat would), the claim, and its own last request, `last_request`,
/// such as a conversation with the owner.
pub fn due(
    store: &Store,
    state: &State,
    last_request: Option<Timestamp>,
    now: Timestamp,
) -> Result<bool, Failure> {
    let Some(holder) = &state.orchestrator else {
        return Ok(false);
    };
    let claimed_at: Timestamp = holder
        .claimed_at
        .parse()
        .map_err(|err| Failure::Error(format!("the claim's time is invalid: {err}")))?;
    let since = [
        last_event_at(&store.dir().join("events.jsonl"))?,
        Some(claimed_at),
        last_request,
    ]
    .into_iter()
    .flatten()
    .max()
    .expect("the claim's time is always there");
    Ok(now.duration_since(since) >= INTERVAL)
}

/// The heartbeat's line. The fleet's state lets the orchestrator check in
/// without running anything.
pub fn line(workers: usize, context: Option<f64>, budget: Option<(u64, u64)>) -> String {
    let mut parts = vec![match workers {
        1 => "1 worker open".to_owned(),
        n => format!("{n} workers open"),
    }];
    if let Some(percent) = context {
        parts.push(format!("your context {percent:.0}%"));
    }
    if let Some((used, budget)) = budget {
        parts.push(format!(
            "budget about {} of {}",
            millions(used),
            millions(budget)
        ));
    }
    format!("heartbeat: {}; no action needed", parts.join(", "))
}

fn millions(tokens: u64) -> String {
    format!("{:.1}M", tokens as f64 / 1_000_000.0)
}

/// When the last complete event in `path` was recorded, read from the end of
/// the file, since it only grows.
fn last_event_at(path: &Path) -> Result<Option<Timestamp>, Failure> {
    let io_error = |err: std::io::Error| Failure::Error(format!("{}: {err}", path.display()));
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(io_error(err)),
    };
    // Far more than one event line.
    let len = file.metadata().map_err(io_error)?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(64 * 1024)))
        .map_err(io_error)?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).map_err(io_error)?;
    let tail = String::from_utf8_lossy(&tail);
    Ok(tail
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<Event>(line).ok())
        .and_then(|event| event.ts.parse().ok()))
}
