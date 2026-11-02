//! `orchestral claim` and `orchestral resolve`, from session records.
//!
//! This test process stands in for live sessions, and a pid no process has
//! for dead ones.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::Stubs;
use orchestral::sessions;
use orchestral::state::{Orchestrator, Store, Worker};
use serde_json::json;
use tempfile::TempDir;

const ME: &str = "00000000-0000-4000-8000-0000000000c1";
const OTHER: &str = "00000000-0000-4000-8000-0000000000c2";
const GONE_PID: u32 = 4_000_000;

struct Repo {
    dir: TempDir,
    main: PathBuf,
    stubs: Stubs,
    records: usize,
}

impl Repo {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let main = root.join("app");
        fs::create_dir_all(main.join(".claude/worktrees/issue-12")).unwrap();
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&main)
            .status()
            .unwrap();
        assert!(status.success());
        fs::create_dir_all(root.join("config/sessions")).unwrap();
        Repo {
            dir,
            main,
            stubs: Stubs::new(),
            records: 0,
        }
    }

    fn worktree(&self) -> PathBuf {
        self.main.join(".claude/worktrees/issue-12")
    }

    fn store(&self) -> Store {
        Store::for_repo(&self.main).unwrap()
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
                        worktree: self.worktree(),
                        branch: "issue-12".into(),
                        job_id: Some("558b988a".into()),
                        review_of: None,
                        created_at: "2026-11-02T10:00:00Z".into(),
                    },
                )
            })
            .unwrap();
    }

    fn set_holder(&self, session_id: &str) {
        self.store()
            .update(|state| {
                state.orchestrator = Some(Orchestrator {
                    session_id: session_id.into(),
                    claimed_at: "2026-11-02T10:00:00Z".into(),
                })
            })
            .unwrap();
    }

    /// Writes a session record, alive unless `live` is false.
    fn session(&mut self, session_id: &str, name: &str, cwd: &Path, live: bool) {
        let pid = if live { std::process::id() } else { GONE_PID };
        let started = sessions::process(std::process::id())
            .unwrap()
            .unwrap()
            .started;
        self.records += 1;
        let record = json!({
            "pid": pid,
            "sessionId": session_id,
            "cwd": cwd,
            "name": name,
            "procStart": started,
            "messagingSocketPath": format!("/tmp/cc-socks/{}.sock", self.records),
        });
        let path = self
            .dir
            .path()
            .join(format!("config/sessions/{}.json", self.records));
        fs::write(path, record.to_string()).unwrap();
    }

    fn run(&self, args: &[&str], session: Option<&str>) -> Output {
        let mut command = self.stubs.orchestral();
        command
            .args(args)
            .current_dir(&self.main)
            .env("CLAUDE_CONFIG_DIR", self.dir.path().join("config"))
            .env_remove("CLAUDE_CODE_SESSION_ID");
        if let Some(id) = session {
            command.env("CLAUDE_CODE_SESSION_ID", id);
        }
        command.output().unwrap()
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn holder(repo: &Repo) -> Option<String> {
    repo.store()
        .read()
        .unwrap()
        .orchestrator
        .map(|o| o.session_id)
}

#[test]
fn claim_records_this_session() {
    let repo = Repo::new();

    let output = repo.run(&["claim"], Some(ME));

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        format!("Session {ME} is the orchestrator.\n")
    );
    assert_eq!(holder(&repo).as_deref(), Some(ME));
}

#[test]
fn claim_needs_a_session_id() {
    let repo = Repo::new();

    let output = repo.run(&["claim"], None);

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("CLAUDE_CODE_SESSION_ID isn't set"));
}

#[test]
fn claiming_again_from_the_holder_succeeds() {
    let mut repo = Repo::new();
    repo.session(ME, "app", &repo.main.clone(), true);
    repo.set_holder(ME);

    let output = repo.run(&["claim"], Some(ME));

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
}

#[test]
fn claim_refuses_while_a_live_session_holds_it() {
    let mut repo = Repo::new();
    repo.session(OTHER, "app", &repo.main.clone(), true);
    repo.set_holder(OTHER);

    let output = repo.run(&["claim"], Some(ME));

    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains(&format!("live session {OTHER} holds the claim")));
    assert_eq!(holder(&repo).as_deref(), Some(OTHER));
}

#[test]
fn a_dead_holders_claim_can_be_taken() {
    let mut repo = Repo::new();
    repo.session(OTHER, "app", &repo.main.clone(), false);
    repo.set_holder(OTHER);

    let output = repo.run(&["claim"], Some(ME));

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).contains(&format!("taking over from ended session {OTHER}")));
    assert_eq!(holder(&repo).as_deref(), Some(ME));
}

#[test]
fn resolve_prints_a_live_workers_address() {
    let mut repo = Repo::new();
    repo.add_worker();
    repo.session("w", "app-issue-12", &repo.worktree(), true);

    let output = repo.run(&["resolve", "issue-12"], None);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stdout(&output), "uds:/tmp/cc-socks/1.sock\n");
}

#[test]
fn resolve_prints_the_orchestrators_address() {
    let mut repo = Repo::new();
    repo.session(ME, "app", &repo.main.clone(), true);
    repo.set_holder(ME);

    let output = repo.run(&["resolve", "orchestrator", "--json"], None);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let printed: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(
        printed,
        json!({"name": "orchestrator", "session_id": ME, "address": "uds:/tmp/cc-socks/1.sock"})
    );
}

#[test]
fn resolve_refuses_a_dead_worker() {
    let mut repo = Repo::new();
    repo.add_worker();
    repo.session("w", "app-issue-12", &repo.worktree(), false);

    let output = repo.run(&["resolve", "issue-12"], None);

    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("issue-12 has no live session"));
}

#[test]
fn resolve_refuses_an_ambiguous_worker() {
    let mut repo = Repo::new();
    repo.add_worker();
    repo.session("w1", "app-issue-12", &repo.worktree(), true);
    repo.session("w2", "app-issue-12", &repo.worktree(), true);

    let output = repo.run(&["resolve", "issue-12"], None);

    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("2 live sessions match issue-12"));
}

#[test]
fn resolve_refuses_a_worker_outside_its_worktree() {
    let mut repo = Repo::new();
    repo.add_worker();
    repo.session("w", "app-issue-12", &repo.main.clone(), true);

    let output = repo.run(&["resolve", "issue-12"], None);

    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("not its worktree"));
}

#[test]
fn resolve_refuses_a_dead_orchestrator() {
    let mut repo = Repo::new();
    repo.session(ME, "app", &repo.main.clone(), false);
    repo.set_holder(ME);

    let output = repo.run(&["resolve", "orchestrator"], None);

    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("orchestrator has no live session"));
}
