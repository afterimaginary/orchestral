//! Which Claude Code sessions are alive, and what they're waiting for.
//!
//! Session records and `claude agents --json` are undocumented, and are
//! recorded in `docs/specs/claude-code-internals.md`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::outcome::Failure;

/// How far a record's `startedAt` may be from the process's real start, in
/// seconds, when `procStart` is missing.
const START_TOLERANCE: i64 = 30;

/// One session record from the sessions directory.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub pid: u32,
    pub session_id: String,
    pub cwd: PathBuf,
    pub name: Option<String>,
    /// `interactive` or `background`.
    pub kind: Option<String>,
    /// `idle`, `busy` or `waiting`, or `shell` while a Monitor runs.
    pub status: Option<String>,
    /// The process start time as `ps -o lstart=` prints it in UTC.
    pub proc_start: Option<String>,
    /// The session start time in Unix milliseconds.
    pub started_at: Option<i64>,
    pub messaging_socket_path: Option<PathBuf>,
}

/// A running process, as `ps` reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct Process {
    /// Seconds since the process started.
    pub elapsed: i64,
    /// The start time, as `ps -o lstart=` prints it in UTC.
    pub started: String,
}

/// The directory holding session records.
pub fn records_dir() -> Option<PathBuf> {
    let config = match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".claude"),
    };
    Some(config.join("sessions"))
}

/// Every record in `dir`. Files that don't parse are skipped, since a
/// session may be writing its record at the time.
pub fn read_records(dir: &Path) -> Result<Vec<Record>, Failure> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(err) => return Err(Failure::Error(format!("{}: {err}", dir.display()))),
    };
    let mut records: Vec<Record> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| serde_json::from_str(&fs::read_to_string(path).ok()?).ok())
        .collect();
    records.sort_by_key(|record| record.pid);
    Ok(records)
}

/// Whether a record's session is still running. A pid alone isn't enough,
/// since the system reuses pids: the process must also have started when the
/// record says it did.
pub fn is_alive(record: &Record, process: Option<&Process>, now_ms: i64) -> bool {
    let Some(process) = process else {
        return false;
    };
    match (&record.proc_start, record.started_at) {
        (Some(proc_start), _) => normalize(proc_start) == normalize(&process.started),
        (None, Some(started_at)) => {
            let real_start_ms = now_ms - process.elapsed * 1000;
            (real_start_ms - started_at).abs() <= START_TOLERANCE * 1000
        }
        (None, None) => false,
    }
}

/// `ps` pads single-digit days with a space, so compare words.
fn normalize(time: &str) -> String {
    time.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Looks up a process with `ps`, in UTC and the C locale so its start time
/// compares with `procStart`. `None` when there's no such process.
pub fn process(pid: u32) -> Result<Option<Process>, Failure> {
    let output = Command::new("ps")
        .args(["-o", "etime=,lstart=", "-p", &pid.to_string()])
        .env("TZ", "UTC")
        .env("LC_ALL", "C")
        .output()
        .map_err(|err| Failure::Error(format!("can't run ps: {err}")))?;
    if !output.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.trim();
    let (etime, started) = line
        .split_once(char::is_whitespace)
        .ok_or_else(|| Failure::Error(format!("unexpected ps output: {line}")))?;
    let elapsed = parse_etime(etime)
        .ok_or_else(|| Failure::Error(format!("unexpected ps elapsed time: {etime}")))?;
    Ok(Some(Process {
        elapsed,
        started: started.trim().to_owned(),
    }))
}

/// Parses `ps` elapsed time, `[[dd-]hh:]mm:ss`, into seconds.
fn parse_etime(etime: &str) -> Option<i64> {
    let (days, clock) = match etime.split_once('-') {
        Some((days, clock)) => (days.parse::<i64>().ok()?, clock),
        None => (0, etime),
    };
    let parts: Vec<i64> = clock
        .split(':')
        .map(|part| part.parse().ok())
        .collect::<Option<_>>()?;
    let (hours, minutes, seconds) = match parts[..] {
        [minutes, seconds] => (0, minutes, seconds),
        [hours, minutes, seconds] => (hours, minutes, seconds),
        _ => return None,
    };
    Some(((days * 24 + hours) * 60 + minutes) * 60 + seconds)
}

/// One entry of `claude agents --json`.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    pub session_id: String,
    pub pid: Option<u32>,
    pub name: Option<String>,
    pub cwd: Option<PathBuf>,
    /// `background` or `interactive`.
    pub kind: Option<String>,
    /// `working`, `blocked`, `done` or `failed`. Background sessions only.
    pub state: Option<String>,
    /// What the session is waiting for, such as `input needed`.
    pub waiting_for: Option<String>,
}

/// What a session is doing, from `waitingFor` first, then `state`.
#[derive(Debug, PartialEq)]
pub enum Activity {
    Waiting(String),
    Working,
    Done,
    Failed,
    Unknown,
}

impl Agent {
    pub fn activity(&self) -> Activity {
        if let Some(reason) = &self.waiting_for {
            return Activity::Waiting(reason.clone());
        }
        match self.state.as_deref() {
            Some("working") => Activity::Working,
            Some("blocked") => Activity::Waiting("blocked".into()),
            Some("done") => Activity::Done,
            Some("failed") => Activity::Failed,
            _ => Activity::Unknown,
        }
    }
}

/// Runs `<claude> agents --json --all`.
pub fn agents(claude: &Path) -> Result<Vec<Agent>, Failure> {
    let output = Command::new(claude)
        .args(["agents", "--json", "--all"])
        .output()
        .map_err(|err| Failure::Error(format!("can't run {}: {err}", claude.display())))?;
    if !output.status.success() {
        return Err(Failure::Error(format!(
            "claude agents failed: {}",
            String::from_utf8_lossy(&output.stderr).trim_end()
        )));
    }
    parse_agents(&String::from_utf8_lossy(&output.stdout))
}

pub fn parse_agents(json: &str) -> Result<Vec<Agent>, Failure> {
    serde_json::from_str(json)
        .map_err(|err| Failure::Error(format!("unexpected `claude agents --json` output: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_etime_form() {
        assert_eq!(parse_etime("00:07"), Some(7));
        assert_eq!(parse_etime("12:34"), Some(12 * 60 + 34));
        assert_eq!(parse_etime("01:00:00"), Some(3600));
        assert_eq!(
            parse_etime("05-11:54:57"),
            Some(((5 * 24 + 11) * 60 + 54) * 60 + 57)
        );
        assert_eq!(parse_etime("soon"), None);
    }
}
