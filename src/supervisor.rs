//! The supervisor's lifecycle: `orchestral up` starts it detached, and it
//! runs until no worker is open and the keepalive window has passed.
//!
//! Its tick work comes later; for now it records the run and keeps the
//! machine awake while the fleet has work.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::outcome::Failure;
use crate::sessions;
use crate::state::Store;

/// How often the supervisor checks whether it can exit.
const TICK: Duration = Duration::from_secs(2);

/// How long `up` waits for a new supervisor to record itself.
const START_TIMEOUT: Duration = Duration::from_secs(10);

/// `supervisor.pid`: the running supervisor. The start time tells it apart
/// from a later process that reuses the pid.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct PidFile {
    pid: u32,
    started: String,
}

/// The result of `orchestral up`.
#[derive(Debug, Serialize)]
pub struct Up {
    pub pid: u32,
    /// False when a supervisor was already running.
    pub started: bool,
}

impl fmt::Display for Up {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.started {
            write!(f, "Started the supervisor (pid {}).", self.pid)
        } else {
            write!(f, "The supervisor is already running (pid {}).", self.pid)
        }
    }
}

/// Starts the supervisor detached, unless one is already running.
pub fn up(cwd: &Path, keepalive_hours: f64) -> Result<Up, Failure> {
    check_keepalive(keepalive_hours)?;
    let store = Store::for_repo(cwd)?;
    if let Some(pid) = running(&store)? {
        return Ok(Up {
            pid,
            started: false,
        });
    }
    let log = log_file(&store)?;
    let exe = std::env::current_exe()
        .map_err(|err| Failure::Error(format!("can't find the orchestral binary: {err}")))?;
    let mut child = Command::new(exe)
        .args([
            "supervise",
            "--keepalive-hours",
            &keepalive_hours.to_string(),
        ])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(
            log.try_clone()
                .map_err(|err| io_error(&log_path(&store), err))?,
        )
        .stderr(log)
        // Its own process group, so it outlives the caller's terminal.
        .process_group(0)
        .spawn()
        .map_err(|err| Failure::Error(format!("can't start the supervisor: {err}")))?;

    let deadline = Instant::now() + START_TIMEOUT;
    while Instant::now() < deadline {
        if let Some(pid) = running(&store)? {
            // Another `up` at the same moment may have won; either way one
            // supervisor runs, and this child exits on its own.
            return Ok(Up {
                pid,
                started: pid == child.id(),
            });
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(Failure::Error(format!(
                "the supervisor exited at start with {status}; see {}",
                log_path(&store).display()
            )));
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(Failure::Error(format!(
        "the supervisor didn't start within {} seconds; see {}",
        START_TIMEOUT.as_secs(),
        log_path(&store).display()
    )))
}

/// The supervisor itself, run by `up` as `orchestral supervise`.
pub fn supervise(cwd: &Path, keepalive_hours: f64) -> Result<(), Failure> {
    check_keepalive(keepalive_hours)?;
    let store = Store::for_repo(cwd)?;
    let pid = std::process::id();
    let started = sessions::process(pid)?
        .ok_or_else(|| Failure::Error("can't find this process with ps".into()))?
        .started;
    let pid_path = pid_path(&store);

    // Under the lock, so two supervisors can't both claim the pid file.
    let new_run = store.update(|state| -> Result<Option<bool>, Failure> {
        if read_pid(&pid_path).is_some_and(|other| other.pid != pid && is_alive(&other)) {
            return Ok(None);
        }
        let record = PidFile { pid, started };
        let text = serde_json::to_string(&record).expect("a pid file always serializes");
        fs::write(&pid_path, text).map_err(|err| io_error(&pid_path, err))?;
        // With no worker open, this is a new run. Otherwise the supervisor
        // is coming back to a run in progress, such as after a crash.
        let new_run = state.workers.is_empty() || state.run_started_at.is_none();
        if new_run {
            state.run_started_at = Some(jiff::Timestamp::now().to_string());
        }
        Ok(Some(new_run))
    })??;
    let Some(new_run) = new_run else {
        log("another supervisor is running; exiting");
        return Ok(());
    };
    if new_run {
        store.record("run", "a new run started")?;
    }
    log(&format!(
        "started (pid {pid}, keepalive {keepalive_hours} hours, {})",
        if new_run {
            "new run"
        } else {
            "continuing the run"
        }
    ));
    let _awake = keep_awake(pid);

    let keepalive = Duration::from_secs_f64(keepalive_hours * 3600.0);
    let mut idle_since = Instant::now();
    loop {
        thread::sleep(TICK);
        // The supervisor stops if another took over its pid file.
        if read_pid(&pid_path).is_none_or(|record| record.pid != pid) {
            log("the pid file no longer names this supervisor; exiting");
            return Ok(());
        }
        let open = match store.read() {
            Ok(state) => !state.workers.is_empty(),
            Err(err) => {
                log(&format!("can't read the state: {}", failure_text(&err)));
                true
            }
        };
        if open {
            idle_since = Instant::now();
        } else if idle_since.elapsed() >= keepalive {
            break;
        }
    }
    store.update(|_| {
        if read_pid(&pid_path).is_some_and(|record| record.pid == pid) {
            let _ = fs::remove_file(&pid_path);
        }
    })?;
    log("no worker is open and the keepalive window has passed; exiting");
    Ok(())
}

/// The running supervisor's pid, if there is one.
fn running(store: &Store) -> Result<Option<u32>, Failure> {
    Ok(read_pid(&pid_path(store))
        .filter(is_alive)
        .map(|record| record.pid))
}

fn is_alive(record: &PidFile) -> bool {
    matches!(sessions::process(record.pid), Ok(Some(process)) if process.started == record.started)
}

fn read_pid(path: &Path) -> Option<PidFile> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

fn pid_path(store: &Store) -> PathBuf {
    store.dir().join("supervisor.pid")
}

fn log_path(store: &Store) -> PathBuf {
    store.dir().join("logs").join("supervisor.log")
}

fn log_file(store: &Store) -> Result<fs::File, Failure> {
    let path = log_path(store);
    let dir = path.parent().expect("the log has a directory");
    fs::create_dir_all(dir).map_err(|err| io_error(dir, err))?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|err| io_error(&path, err))
}

/// A line in the log, which `up` connects to the supervisor's stderr.
fn log(message: &str) {
    let _ = writeln!(
        std::io::stderr(),
        "{} {message}",
        jiff::Timestamp::now()
            .round(jiff::Unit::Second)
            .unwrap_or_default()
    );
}

/// On macOS, holds a sleep assertion until this process exits. Elsewhere,
/// or without `caffeinate`, the machine may sleep, which is logged.
fn keep_awake(pid: u32) -> Option<std::process::Child> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    match Command::new("caffeinate")
        .args(["-i", "-w", &pid.to_string()])
        .stdin(Stdio::null())
        .spawn()
    {
        Ok(child) => Some(child),
        Err(err) => {
            log(&format!(
                "can't run caffeinate, so the machine may sleep: {err}"
            ));
            None
        }
    }
}

fn check_keepalive(hours: f64) -> Result<(), Failure> {
    if hours.is_finite() && hours >= 0.0 {
        Ok(())
    } else {
        Err(Failure::Error(format!(
            "--keepalive-hours must be 0 or more, not {hours}"
        )))
    }
}

fn failure_text(failure: &Failure) -> String {
    match failure {
        Failure::Error(message) => message.clone(),
        Failure::Refused { reason, .. } => reason.clone(),
    }
}

fn io_error(path: &Path, err: std::io::Error) -> Failure {
    Failure::Error(format!("{}: {err}", path.display()))
}
