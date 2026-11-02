//! `orchestral watch`, run as the plugin monitor runs it: a long-lived
//! process whose stdout lines are events.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use orchestral::state::{Orchestrator, Store};
use tempfile::TempDir;

const ME: &str = "00000000-0000-4000-8000-0000000000c1";
const SUCCESSOR: &str = "00000000-0000-4000-8000-0000000000c2";

/// Long enough for several polls, so silence means silence.
const QUIET: Duration = Duration::from_secs(3);

struct Watch {
    child: Child,
    lines: Receiver<String>,
}

impl Watch {
    fn start(cwd: &Path, session_id: &str) -> Watch {
        let mut child = Command::new(env!("CARGO_BIN_EXE_orchestral"))
            .arg("watch")
            .current_dir(cwd)
            .env("CLAUDE_CODE_SESSION_ID", session_id)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        Watch { child, lines }
    }

    /// The next line, if one arrives within `within`.
    fn line(&self, within: Duration) -> Option<String> {
        self.lines.recv_timeout(within).ok()
    }

    fn is_running(&mut self) -> bool {
        self.child.try_wait().unwrap().is_none()
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Repo {
    _dir: TempDir,
    main: PathBuf,
}

impl Repo {
    fn new() -> Repo {
        let dir = TempDir::new().unwrap();
        let main = dir.path().canonicalize().unwrap().join("app");
        fs::create_dir_all(&main).unwrap();
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&main)
            .status()
            .unwrap();
        assert!(status.success());
        Repo { _dir: dir, main }
    }

    fn store(&self) -> Store {
        Store::for_repo(&self.main).unwrap()
    }

    fn claim(&self, session_id: &str) {
        self.store()
            .update(|state| {
                state.orchestrator = Some(Orchestrator {
                    session_id: session_id.into(),
                    claimed_at: "2026-11-02T10:00:00Z".into(),
                })
            })
            .unwrap();
    }

    fn record(&self, text: &str) {
        self.store().record("worker", text).unwrap();
    }

    fn cursor(&self) -> String {
        fs::read_to_string(self.store().dir().join("watch-cursor")).unwrap_or_default()
    }

    /// Waits for the cursor, which is saved just after the lines print.
    fn wait_for_cursor(&self, cursor: &str) {
        let start = Instant::now();
        while self.cursor() != cursor {
            assert!(start.elapsed() < QUIET, "the cursor stayed at {:?}", self.cursor());
            thread::sleep(Duration::from_millis(50));
        }
    }
}

#[test]
fn stays_silent_without_the_claim() {
    let repo = Repo::new();
    repo.record("issue-12 is idle");
    repo.claim(SUCCESSOR);
    let mut watch = Watch::start(&repo.main, ME);
    assert_eq!(watch.line(QUIET), None);
    assert!(watch.is_running());
}

#[test]
fn stays_silent_outside_a_repository() {
    let dir = TempDir::new().unwrap();
    let mut watch = Watch::start(dir.path(), ME);
    assert_eq!(watch.line(QUIET), None);
    assert!(watch.is_running());
}

#[test]
fn starts_printing_within_2_seconds_of_claiming() {
    let repo = Repo::new();
    repo.record("issue-12 is idle: check its PR, or send it its next step");
    let watch = Watch::start(&repo.main, ME);
    assert_eq!(watch.line(QUIET), None);
    repo.claim(ME);
    let claimed = Instant::now();
    assert_eq!(
        watch.line(QUIET).as_deref(),
        Some("issue-12 is idle: check its PR, or send it its next step")
    );
    assert!(claimed.elapsed() < Duration::from_secs(2));
}

#[test]
fn prints_each_event_after_the_cursor_once_on_one_line() {
    let repo = Repo::new();
    repo.claim(ME);
    repo.record("one");
    repo.record("two\nlines");
    fs::write(repo.store().dir().join("watch-cursor"), "1\n").unwrap();
    let mut watch = Watch::start(&repo.main, ME);
    assert_eq!(watch.line(QUIET).as_deref(), Some("two lines"));
    repo.record("three");
    assert_eq!(watch.line(QUIET).as_deref(), Some("three"));
    assert_eq!(watch.line(QUIET), None);
    assert_eq!(repo.cursor(), "3\n");
    assert!(watch.is_running());
}

#[test]
fn a_successor_resumes_from_the_cursor() {
    let repo = Repo::new();
    repo.claim(ME);
    repo.record("one");
    let first = Watch::start(&repo.main, ME);
    assert_eq!(first.line(QUIET).as_deref(), Some("one"));
    repo.wait_for_cursor("1\n");
    drop(first);
    // Recorded while no session watches, and then the claim moves.
    repo.record("two");
    repo.claim(SUCCESSOR);
    let successor = Watch::start(&repo.main, SUCCESSOR);
    assert_eq!(successor.line(QUIET).as_deref(), Some("two"));
    assert_eq!(successor.line(QUIET), None);
}

#[test]
fn a_torn_line_waits_for_its_newline() {
    let repo = Repo::new();
    repo.claim(ME);
    let events = repo.store().dir().join("events.jsonl");
    fs::write(&events, r#"{"seq":1,"ts":"2026-11-02T10:00:00Z","kind":"worker","text":"one"}"#)
        .unwrap();
    let watch = Watch::start(&repo.main, ME);
    assert_eq!(watch.line(QUIET), None);
    let mut text = fs::read_to_string(&events).unwrap();
    text.push('\n');
    fs::write(&events, text).unwrap();
    assert_eq!(watch.line(QUIET).as_deref(), Some("one"));
}
