//! The supervisor's lifecycle: `orchestral up` starts it detached, and it
//! runs until no worker is open and the keepalive window has passed.
//!
//! Every 2 seconds it checks the workers and records an event when one needs
//! someone; every minute it polls GitHub, logs the fleet's usage and writes
//! a heartbeat when one is due, and every 5 minutes it logs the plan
//! windows. It keeps the machine awake while the fleet has work.

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
use crate::policy::Policy;
use crate::state::Store;
use crate::{gate, github, heartbeat, limits, observe, sessions, transcript};

/// How often the supervisor checks the workers and whether it can exit.
const TICK: Duration = Duration::from_secs(2);

const MINUTE: jiff::SignedDuration = jiff::SignedDuration::from_secs(60);
const FIVE_MINUTES: jiff::SignedDuration = jiff::SignedDuration::from_secs(300);

const MAX_KEEPALIVE_HOURS: f64 = 24.0 * 366.0;

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

    // Wall-clock time, so a machine asleep counts toward the window.
    let keepalive = jiff::SignedDuration::from_secs_f64(keepalive_hours * 3600.0);
    let mut idle_since = jiff::Timestamp::now();
    let mut ticks = Ticks::default();
    loop {
        thread::sleep(TICK);
        // The supervisor stops if another took over its pid file.
        if read_pid(&pid_path).is_none_or(|record| record.pid != pid) {
            log("the pid file no longer names this supervisor; exiting");
            return Ok(());
        }
        ticks.run(&store);
        let open = match store.read() {
            Ok(state) => !state.workers.is_empty(),
            Err(err) => {
                log(&format!("can't read the state: {}", failure_text(&err)));
                true
            }
        };
        if open {
            idle_since = jiff::Timestamp::now();
            continue;
        }
        if jiff::Timestamp::now().duration_since(idle_since) < keepalive {
            continue;
        }
        // Checked again under the lock, so a worker spawned just now isn't
        // left without a supervisor.
        let exited = store.update(|state| {
            if !state.workers.is_empty() {
                return false;
            }
            if read_pid(&pid_path).is_some_and(|record| record.pid == pid) {
                let _ = fs::remove_file(&pid_path);
            }
            true
        })?;
        if exited {
            log("no worker is open and the keepalive window has passed; exiting");
            return Ok(());
        }
        idle_since = jiff::Timestamp::now();
    }
}

/// The work done on each tick. The 2-second tick runs every time; the
/// longer ones run when their interval has passed, by the wall clock, on a
/// thread of their own, so a slow `claude` call never holds up the workers'
/// check. A longer tick that is still running is skipped rather than doubled.
#[derive(Default)]
struct Ticks {
    last_minute: Option<jiff::Timestamp>,
    last_five_minutes: Option<jiff::Timestamp>,
    minute: Option<thread::JoinHandle<()>>,
    five_minutes: Option<thread::JoinHandle<()>>,
}

impl Ticks {
    fn run(&mut self, store: &Store) {
        // Every 2 seconds: worker sessions, from the state on disk.
        match observe::check(store) {
            Ok(changes) => {
                for change in changes {
                    log(&change.text());
                }
            }
            Err(err) => log(&format!("can't check the workers: {}", failure_text(&err))),
        }
        let now = jiff::Timestamp::now();
        if due(self.last_minute, now, MINUTE) && finished(&self.minute) {
            self.last_minute = Some(now);
            // Every minute: GitHub, the fleet's usage this run, and a
            // heartbeat when one is due. The heartbeat comes last, so an
            // event from this poll puts it off.
            let store = store.clone();
            self.minute = Some(thread::spawn(move || {
                match github::poll(&store, Path::new("gh")) {
                    Ok(lines) => lines.iter().for_each(|line| log(line)),
                    Err(err) => log(&format!("can't poll GitHub: {}", failure_text(&err))),
                }
                let used = match fleet_usage(&store) {
                    Ok(used) => {
                        log(&format!("fleet usage: {used}"));
                        Some(used)
                    }
                    Err(err) => {
                        log(&format!("can't read fleet usage: {}", failure_text(&err)));
                        None
                    }
                };
                match beat(&store, used.as_ref()) {
                    Ok(Some(line)) => log(&line),
                    Ok(None) => {}
                    Err(err) => log(&format!("can't write a heartbeat: {}", failure_text(&err))),
                }
            }));
        }
        if due(self.last_five_minutes, now, FIVE_MINUTES) && finished(&self.five_minutes) {
            self.last_five_minutes = Some(now);
            // Every 5 minutes: the plan windows.
            self.five_minutes = Some(thread::spawn(|| {
                log(&format!(
                    "plan limits: {}",
                    limits::read().to_string().replace('\n', "; ")
                ))
            }));
        }
    }
}

fn finished(handle: &Option<thread::JoinHandle<()>>) -> bool {
    handle.as_ref().is_none_or(|handle| handle.is_finished())
}

fn due(last: Option<jiff::Timestamp>, now: jiff::Timestamp, every: jiff::SignedDuration) -> bool {
    last.is_none_or(|last| now.duration_since(last) >= every)
}

fn fleet_usage(store: &Store) -> Result<crate::budget::Estimate, Failure> {
    let state = store.read()?;
    let agents = match state.run_started_at {
        Some(_) => sessions::agents(Path::new("claude"))?,
        None => Vec::new(),
    };
    gate::fleet_usage(&state, &crate::spawn::main_checkout(store.dir())?, &agents)
}

/// Records a heartbeat when one is due, with the fleet's state. A part
/// that can't be read is left out rather than holding up the heartbeat.
fn beat(store: &Store, used: Option<&crate::budget::Estimate>) -> Result<Option<String>, Failure> {
    let state = store.read()?;
    let now = jiff::Timestamp::now();
    // A holder whose session ended can't be woken, and its successor would
    // get every beat at once.
    let Some(holder) = &state.orchestrator else {
        return Ok(None);
    };
    if !crate::claim::is_live(&holder.session_id)? {
        return Ok(None);
    }
    let main = crate::spawn::main_checkout(store.dir())?;
    let context = transcript::projects_dir()
        .and_then(|projects| transcript::find(&projects, &main, &holder.session_id))
        .and_then(|path| {
            transcript::read(&path, now, crate::status::organization().as_deref()).ok()
        });
    let last_request = context.as_ref().and_then(|context| context.last_request_at);
    if !heartbeat::due(store, &state, last_request, now)? {
        return Ok(None);
    }
    let context = context.and_then(|context| context.percent);
    let budget = used
        .zip(Policy::load(&main).ok())
        .map(|(used, policy)| (used.estimated_tokens, policy.settings.fleet_token_budget));
    let line = heartbeat::line(state.workers.len(), context, budget);
    store.record("heartbeat", &line)?;
    Ok(Some(line))
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
        log("not on macOS, so nothing keeps the machine awake");
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
    // A year is far past any real window, and keeps the seconds in range.
    if (0.0..=MAX_KEEPALIVE_HOURS).contains(&hours) {
        Ok(())
    } else {
        Err(Failure::Error(format!(
            "--keepalive-hours must be from 0 to {MAX_KEEPALIVE_HOURS}, not {hours}"
        )))
    }
}

pub(crate) fn failure_text(failure: &Failure) -> String {
    match failure {
        Failure::Error(message) => message.clone(),
        Failure::Refused { reason, .. } => reason.clone(),
    }
}

fn io_error(path: &Path, err: std::io::Error) -> Failure {
    Failure::Error(format!("{}: {err}", path.display()))
}
