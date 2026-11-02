//! `orchestral up` and the supervisor's lifecycle, in a scratch repository.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use orchestral::state::{Store, Worker};
use serde_json::Value;
use tempfile::TempDir;

/// A repository, and any supervisor a test started, which is killed when
/// the test ends.
struct Repo {
    _dir: TempDir,
    path: PathBuf,
    pids: Vec<u32>,
}

impl Repo {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let path = dir.path().canonicalize().unwrap();
        let output = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&path)
            .output()
            .unwrap();
        assert!(output.status.success());
        Repo {
            _dir: dir,
            path,
            pids: Vec::new(),
        }
    }

    fn up(&mut self, keepalive_hours: &str) -> Value {
        let output: Output = Command::new(env!("CARGO_BIN_EXE_orchestral"))
            .args(["up", "--json", "--keepalive-hours", keepalive_hours])
            .current_dir(&self.path)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        let up: Value = serde_json::from_slice(&output.stdout).unwrap();
        self.pids.push(up["pid"].as_u64().unwrap() as u32);
        up
    }

    fn store(&self) -> Store {
        Store::for_repo(&self.path).unwrap()
    }

    fn file(&self, name: &str) -> PathBuf {
        self.store().dir().join(name)
    }

    fn add_worker(&self) {
        self.store()
            .update(|state| {
                state.workers.insert(
                    "issue-12".into(),
                    Worker {
                        session_name: "app-issue-12".into(),
                        issue: 12,
                        model: "claude-opus-5-5".into(),
                        effort: "high".into(),
                        worktree: self.path.join(".claude/worktrees/issue-12"),
                        branch: "issue-12".into(),
                        job_id: None,
                        created_at: "2026-11-02T00:00:00Z".into(),
                    },
                )
            })
            .unwrap();
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        for pid in &self.pids {
            let _ = Command::new("kill").arg(pid.to_string()).output();
        }
    }
}

fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .unwrap()
        .status
        .success()
}

/// Waits up to `limit` for `done`.
fn eventually(limit: Duration, done: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if done() {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    done()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn starts_detached_and_records_a_new_run() {
    let mut repo = Repo::new();

    let up = repo.up("1");

    let pid = up["pid"].as_u64().unwrap() as u32;
    assert_eq!(up["started"], true);
    assert!(alive(pid));
    assert!(read(&repo.file("supervisor.pid")).contains(&format!("\"pid\":{pid}")));
    // The pid file comes first, so the rest may follow a moment later.
    let wait = Duration::from_secs(5);
    assert!(eventually(wait, || repo
        .store()
        .read()
        .is_ok_and(|state| state.run_started_at.is_some())));
    assert!(eventually(wait, || read(&repo.file("events.jsonl"))
        .contains("a new run started")));
    assert!(eventually(wait, || read(&repo.file("logs/supervisor.log"))
        .contains(&format!("started (pid {pid}"))));
}

#[test]
fn a_second_up_finds_the_running_supervisor() {
    let mut repo = Repo::new();
    let first = repo.up("1");

    let second = repo.up("1");

    assert_eq!(second["started"], false);
    assert_eq!(second["pid"], first["pid"]);
}

#[test]
fn exits_once_no_worker_is_open_and_the_keepalive_has_passed() {
    let mut repo = Repo::new();
    // 7.2 seconds, several ticks long.
    let pid = repo.up("0.002")["pid"].as_u64().unwrap() as u32;

    thread::sleep(Duration::from_secs(4));
    assert!(alive(pid), "exited inside the keepalive window");
    assert!(eventually(Duration::from_secs(15), || !alive(pid)));
    assert!(!repo.file("supervisor.pid").exists());
    assert!(read(&repo.file("logs/supervisor.log")).contains("keepalive window has passed"));
}

#[test]
fn stays_up_while_a_worker_is_open() {
    let mut repo = Repo::new();
    repo.add_worker();
    let pid = repo.up("0.0005")["pid"].as_u64().unwrap() as u32;

    thread::sleep(Duration::from_secs(6));
    assert!(alive(pid), "exited with a worker open");

    repo.store().update(|state| state.workers.clear()).unwrap();
    assert!(eventually(Duration::from_secs(15), || !alive(pid)));
}

#[test]
fn coming_back_to_a_run_in_progress_keeps_its_start() {
    let mut repo = Repo::new();
    repo.add_worker();
    repo.store()
        .update(|state| state.run_started_at = Some("2026-11-02T08:00:00Z".into()))
        .unwrap();

    repo.up("1");

    assert_eq!(
        repo.store().read().unwrap().run_started_at.as_deref(),
        Some("2026-11-02T08:00:00Z")
    );
    assert!(!read(&repo.file("events.jsonl")).contains("a new run started"));
}

#[test]
fn a_stale_pid_file_is_replaced() {
    let mut repo = Repo::new();
    fs::create_dir_all(repo.store().dir()).unwrap();
    fs::write(
        repo.file("supervisor.pid"),
        r#"{"pid":1,"started":"Thu Jan  1 00:00:00 1970"}"#,
    )
    .unwrap();

    let up = repo.up("1");

    assert_eq!(up["started"], true);
    assert_ne!(up["pid"], 1);
}
