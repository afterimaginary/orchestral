//! `orchestral stop` and `orchestral resume`, with a stub `claude`.

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};

use common::Stubs;
use orchestral::state::{Store, Worker};
use serde_json::json;
use tempfile::TempDir;

const JOB: &str = "558b988a";
const SESSION: &str = "558b988a-05ba-4dbe-bffc-4463e5856505";
/// No process has this pid, so a stopped session exits at once.
const GONE_PID: u32 = 4_000_000;

struct Repo {
    _dir: TempDir,
    main: PathBuf,
    stubs: Stubs,
}

impl Repo {
    /// A repository with one started worker, `issue-12`.
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let main = dir.path().canonicalize().unwrap();
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&main)
            .status()
            .unwrap();
        assert!(status.success());
        Store::for_repo(&main)
            .unwrap()
            .update(|state| {
                state.workers.insert(
                    "issue-12".into(),
                    Worker {
                        session_name: "app-issue-12".into(),
                        issue: 12,
                        model: "claude-opus-5-5".into(),
                        effort: "high".into(),
                        worktree: main.clone(),
                        branch: "issue-12".into(),
                        job_id: Some(JOB.into()),
                        created_at: "2026-11-02T10:00:00Z".into(),
                    },
                )
            })
            .unwrap();
        Repo {
            _dir: dir,
            main,
            stubs: Stubs::new(),
        }
    }

    /// Makes `claude agents` list the worker's session.
    fn lists(&self, kind: &str, pid: Option<u32>) {
        let mut agent = json!({"id": JOB, "sessionId": SESSION, "kind": kind, "name": "app-issue-12", "state": "done"});
        if let Some(pid) = pid {
            agent["pid"] = json!(pid);
        }
        self.stubs
            .respond("claude", &["agents"], &json!([agent]).to_string(), 0);
    }

    fn run(&self, args: &[&str]) -> Output {
        self.stubs
            .orchestral()
            .args(args)
            .current_dir(&self.main)
            .output()
            .unwrap()
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.stubs.calls("claude")
    }
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|arg| arg.to_string()).collect()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn stop_stops_a_running_background_worker() {
    let repo = Repo::new();
    repo.lists("background", Some(GONE_PID));
    repo.stubs
        .respond("claude", &["stop"], "stopped 558b988a\n", 0);

    let output = repo.run(&["stop", "issue-12"]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Stopped issue-12. Its worktree and conversation are kept.\n"
    );
    assert_eq!(
        repo.calls(),
        vec![args(&["agents", "--json", "--all"]), args(&["stop", JOB])]
    );
    assert!(repo.main.exists());
}

#[test]
fn stop_never_touches_an_interactive_session() {
    let repo = Repo::new();
    repo.lists("interactive", Some(GONE_PID));

    let output = repo.run(&["stop", "issue-12"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("isn't a background session"));
    assert_eq!(repo.calls(), vec![args(&["agents", "--json", "--all"])]);
}

#[test]
fn stopping_a_stopped_worker_does_nothing() {
    let repo = Repo::new();
    repo.lists("background", None);

    let output = repo.run(&["stop", "issue-12"]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "issue-12 was already stopped.\n"
    );
    assert_eq!(repo.calls().len(), 1);
}

#[test]
fn resume_is_a_bare_resume_of_the_session() {
    let repo = Repo::new();
    repo.lists("background", None);
    repo.stubs.respond(
        "claude",
        &["--bg"],
        "note: woke session 558b988a with its saved options (--name, --settings, --model).\nbackgrounded · 558b988a · app-issue-12 (idle — send a prompt to start)\n",
        0,
    );

    let output = repo.run(&["resume", "issue-12"]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        repo.calls(),
        vec![
            args(&["agents", "--json", "--all"]),
            args(&["--bg", "--resume", SESSION])
        ]
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Resumed issue-12 (job 558b988a). It's idle until it gets a message.\n"
    );
}

#[test]
fn resume_refuses_a_running_worker() {
    let repo = Repo::new();
    repo.lists("background", Some(GONE_PID));

    let output = repo.run(&["resume", "issue-12"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("issue-12 is still running"));
    assert_eq!(repo.calls().len(), 1);
}

#[test]
fn a_resume_that_starts_a_copy_is_an_error() {
    let repo = Repo::new();
    repo.lists("background", None);
    repo.stubs.respond(
        "claude",
        &["--bg"],
        "note: session 558b988a is already running in the background, so this started a copy as 20c5e5d0.\nbackgrounded · 20c5e5d0 (idle — send a prompt to start)\n",
        0,
    );

    let output = repo.run(&["resume", "issue-12"]);

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("didn't resume issue-12"));
}

#[test]
fn an_unknown_worker_is_an_error() {
    let repo = Repo::new();

    let output = repo.run(&["stop", "issue-99"]);

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("there's no worker named issue-99"));
}
