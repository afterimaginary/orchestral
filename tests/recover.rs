//! A restarted supervisor's check for workers that can't come back.

use std::path::Path;

use jiff::Timestamp;
use orchestral::observe::{self, Observed};
use orchestral::recover;
use orchestral::sessions::Agent;
use orchestral::state::{State, Store, Worker};
use tempfile::TempDir;

const NOW: &str = "2026-11-02T12:00:00Z";

fn worker(worktree: &Path, job_id: Option<&str>, created_at: &str) -> Worker {
    Worker {
        session_name: "app-issue-12".into(),
        issue: 12,
        model: "claude-sonnet-5-5".into(),
        effort: "low".into(),
        worktree: worktree.into(),
        branch: "issue-12".into(),
        job_id: job_id.map(Into::into),
        review_of: None,
        created_at: created_at.into(),
    }
}

fn agent(id: &str) -> Agent {
    Agent {
        id: Some(id.into()),
        session_id: "558b988a-05ba-4dbe-bffc-4463e5856505".into(),
        pid: None,
        name: Some("app-issue-12".into()),
        cwd: None,
        kind: Some("background".into()),
        state: Some("done".into()),
        waiting_for: None,
    }
}

fn lost(worker: Worker, agents: &[Agent]) -> Vec<(String, &'static str)> {
    let mut state = State::default();
    state.workers.insert("issue-12".into(), worker);
    recover::lost(&state, agents, NOW.parse::<Timestamp>().unwrap())
}

#[test]
fn a_worker_with_its_worktree_and_session_is_not_lost() {
    let dir = TempDir::new().unwrap();

    let lost = lost(
        worker(dir.path(), Some("558b988a"), "2026-11-02T10:00:00Z"),
        &[agent("558b988a")],
    );

    assert!(lost.is_empty());
}

#[test]
fn a_missing_worktree_or_session_loses_the_worker() {
    let dir = TempDir::new().unwrap();
    let gone = dir.path().join("gone");

    let no_worktree = lost(
        worker(&gone, Some("558b988a"), "2026-11-02T10:00:00Z"),
        &[agent("558b988a")],
    );
    let no_session = lost(
        worker(dir.path(), Some("558b988a"), "2026-11-02T10:00:00Z"),
        &[agent("0000aaaa")],
    );

    assert_eq!(
        no_worktree,
        [("issue-12".to_owned(), "its worktree is gone")]
    );
    assert_eq!(
        no_session,
        [(
            "issue-12".to_owned(),
            "Claude Code no longer has its session"
        )]
    );
}

#[test]
fn a_launch_that_never_finished_is_lost_only_after_a_grace_period() {
    let dir = TempDir::new().unwrap();

    let launching = lost(worker(dir.path(), None, "2026-11-02T11:59:00Z"), &[]);
    let never = lost(worker(dir.path(), None, "2026-11-02T11:00:00Z"), &[]);

    assert!(launching.is_empty());
    assert_eq!(
        never,
        [("issue-12".to_owned(), "its session never launched")]
    );
}

#[test]
fn a_lost_worker_is_reported_once_and_not_as_exited() {
    let dir = TempDir::new().unwrap();
    let mut state = State::default();
    state.workers.insert(
        "issue-12".into(),
        worker(
            &dir.path().join("gone"),
            Some("558b988a"),
            "2026-11-02T10:00:00Z",
        ),
    );
    state.observed.insert("issue-12".into(), Observed::Lost);

    let again = recover::lost(&state, &[], NOW.parse().unwrap());
    let (changes, observations) = observe::changes(&state, &[], |_| true);

    assert!(again.is_empty());
    assert!(changes.is_empty());
    assert!(observations.is_empty());
}

#[test]
fn a_launch_whose_job_id_was_not_read_is_found_by_name() {
    let dir = TempDir::new().unwrap();
    let mut running = agent("558b988a");
    running.id = Some("0000aaaa".into());

    let lost = lost(worker(dir.path(), None, "2026-11-02T11:00:00Z"), &[running]);

    assert!(lost.is_empty());
}

#[test]
fn a_restarted_supervisor_reports_a_lost_worker_once() {
    let repo = TempDir::new().unwrap();
    let main = repo.path().canonicalize().unwrap();
    let status = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&main)
        .status()
        .unwrap();
    assert!(status.success());
    let store = Store::for_repo(&main).unwrap();
    store
        .update(|state| {
            state.workers.insert(
                "issue-12".into(),
                worker(&main.join("gone"), Some("558b988a"), "2026-11-02T10:00:00Z"),
            )
        })
        .unwrap();
    let now = NOW.parse::<Timestamp>().unwrap();

    let first = recover::report(&store, &[], now).unwrap();
    let second = recover::report(&store, &[], now).unwrap();

    assert_eq!(
        first,
        [
            "issue-12 is lost: its worktree is gone, so it can't be resumed: clean it up with `orchestral cleanup issue-12`, and start its issue again, on its PR if it has one"
        ]
    );
    assert!(second.is_empty());
    assert_eq!(
        store.read().unwrap().observed.get("issue-12"),
        Some(&Observed::Lost)
    );
    let events = std::fs::read_to_string(store.dir().join("events.jsonl")).unwrap();
    assert_eq!(events.matches("is lost").count(), 1);
}
