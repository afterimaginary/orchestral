//! Worker events: the supervisor's check of worker sessions, as a function
//! and through the running supervisor against session records.

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::thread;
use std::time::{Duration, Instant};

use orchestral::observe::{self, Change, Observed};
use orchestral::sessions::{self, Record};
use orchestral::state::{State, Store, Worker};
use serde_json::json;
use tempfile::TempDir;

fn worker(name: &str, job: Option<&str>) -> Worker {
    Worker {
        session_name: format!("app-{name}"),
        issue: 12,
        model: "claude-sonnet-5-5".into(),
        effort: "low".into(),
        worktree: PathBuf::from("/home/dev/app/.claude/worktrees").join(name),
        branch: name.into(),
        job_id: job.map(String::from),
        created_at: "2026-11-02T00:00:00Z".into(),
    }
}

fn record(name: &str, status: &str, waiting_for: Option<&str>) -> Record {
    Record {
        pid: 41001,
        session_id: "s".into(),
        cwd: ".".into(),
        name: Some(format!("app-{name}")),
        kind: Some("background".into()),
        status: Some(status.into()),
        proc_start: None,
        started_at: None,
        messaging_socket_path: None,
        waiting_for: waiting_for.map(String::from),
    }
}

fn state(observed: &[(&str, &str)]) -> State {
    let mut state = State::default();
    state
        .workers
        .insert("issue-12".into(), worker("issue-12", Some("a001")));
    for (name, seen) in observed {
        state.observed.insert((*name).into(), (*seen).into());
    }
    state
}

fn change(observed: Observed, waiting_for: Option<&str>) -> Change {
    Change {
        name: "issue-12".into(),
        observed,
        waiting_for: waiting_for.map(String::from),
    }
}

#[test]
fn going_idle_waiting_or_exiting_is_a_change() {
    let alive = |_: &Record| true;
    let busy = state(&[("issue-12", "busy")]);

    let (idle, _) = observe::changes(&busy, &[record("issue-12", "idle", None)], alive);
    let (waiting, _) = observe::changes(
        &busy,
        &[record("issue-12", "waiting", Some("input needed"))],
        alive,
    );
    let (exited, _) = observe::changes(&busy, &[], alive);

    assert_eq!(idle, vec![change(Observed::Idle, None)]);
    assert_eq!(
        waiting,
        vec![change(Observed::Waiting, Some("input needed"))]
    );
    assert_eq!(exited, vec![change(Observed::Exited, None)]);
    assert_eq!(
        waiting[0].text(),
        "issue-12 is waiting for input needed: see what it needs"
    );
    assert_eq!(
        exited[0].text(),
        "issue-12's session exited: resume it with `orchestral resume issue-12`, or clean it up"
    );
}

#[test]
fn becoming_busy_is_observed_without_a_change() {
    let (changes, observations) = observe::changes(
        &state(&[("issue-12", "idle")]),
        &[record("issue-12", "busy", None)],
        |_| true,
    );

    assert!(changes.is_empty());
    assert_eq!(observations, vec![("issue-12".into(), Observed::Busy)]);
}

#[test]
fn the_same_observation_again_is_no_change() {
    let (changes, observations) = observe::changes(
        &state(&[("issue-12", "idle")]),
        &[record("issue-12", "idle", None)],
        |_| true,
    );

    assert!(changes.is_empty());
    assert!(observations.is_empty());
}

#[test]
fn a_dead_record_is_an_exit_and_an_unlaunched_worker_is_skipped() {
    let mut state = state(&[("issue-12", "busy")]);
    state
        .workers
        .insert("issue-13".into(), worker("issue-13", None));

    let (changes, _) = observe::changes(&state, &[record("issue-12", "busy", None)], |_| false);

    assert_eq!(changes, vec![change(Observed::Exited, None)]);
}

/// A repository with one launched worker, a Claude config directory whose
/// session record belongs to a live `sleep`, and the supervisor, which are
/// all stopped when the test ends.
struct Fleet {
    _dir: TempDir,
    repo: PathBuf,
    config: PathBuf,
    sleeper: Child,
    supervisors: Vec<u32>,
}

impl Fleet {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let repo = root.join("app");
        fs::create_dir(&repo).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
        let config = root.join("config");
        fs::create_dir_all(config.join("sessions")).unwrap();
        Store::for_repo(&repo)
            .unwrap()
            .update(|state| {
                state
                    .workers
                    .insert("issue-12".into(), worker("issue-12", Some("a001")));
            })
            .unwrap();
        let sleeper = Command::new("sleep").arg("120").spawn().unwrap();
        let fleet = Fleet {
            _dir: dir,
            repo,
            config,
            sleeper,
            supervisors: Vec::new(),
        };
        fleet.session("busy");
        fleet
    }

    /// Writes the worker's session record with `status`.
    fn session(&self, status: &str) {
        let pid = self.sleeper.id();
        let started = sessions::process(pid).unwrap().unwrap().started;
        let record = json!({
            "pid": pid, "sessionId": "s", "cwd": "/", "kind": "background",
            "name": "app-issue-12", "status": status, "procStart": started,
        });
        fs::write(
            self.config.join("sessions").join(format!("{pid}.json")),
            record.to_string(),
        )
        .unwrap();
    }

    fn up(&mut self) {
        let output = Command::new(env!("CARGO_BIN_EXE_orchestral"))
            .args(["up", "--json", "--keepalive-hours", "1"])
            .current_dir(&self.repo)
            .env("CLAUDE_CONFIG_DIR", &self.config)
            // The minute tick's usage read needs no `claude` before a run
            // has started, and the 5-minute tick's limits read only warns.
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let up: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        self.supervisors.push(up["pid"].as_u64().unwrap() as u32);
    }

    fn stop_supervisor(&mut self) {
        for pid in self.supervisors.drain(..) {
            let _ = Command::new("kill").arg(pid.to_string()).status();
        }
        thread::sleep(Duration::from_millis(500));
    }

    fn events(&self) -> String {
        fs::read_to_string(
            Store::for_repo(&self.repo)
                .unwrap()
                .dir()
                .join("events.jsonl"),
        )
        .unwrap_or_default()
    }

    fn wait_for_event(&self, text: &str) {
        let start = Instant::now();
        while !self.events().contains(text) {
            assert!(
                start.elapsed() < Duration::from_secs(15),
                "no event {text:?} in:\n{}",
                self.events()
            );
            thread::sleep(Duration::from_millis(200));
        }
    }
}

impl Drop for Fleet {
    fn drop(&mut self) {
        self.stop_supervisor();
        let _ = self.sleeper.kill();
    }
}

fn count(events: &str, text: &str) -> usize {
    events.matches(text).count()
}

#[test]
fn the_supervisor_records_worker_events_and_carries_on_after_a_restart() {
    let mut fleet = Fleet::new();
    fleet.up();

    fleet.session("idle");
    fleet.wait_for_event("issue-12 is idle");

    // A restarted supervisor reads the last observation from the state, so
    // it doesn't report the same idle again.
    fleet.stop_supervisor();
    fleet.up();
    thread::sleep(Duration::from_secs(5));
    assert_eq!(
        count(&fleet.events(), "issue-12 is idle"),
        1,
        "{}",
        fleet.events()
    );

    fleet.session("waiting");
    fleet.wait_for_event("issue-12 is waiting for input");

    fleet.sleeper.kill().unwrap();
    fleet.sleeper.wait().unwrap();
    fleet.wait_for_event("issue-12's session exited");
}
