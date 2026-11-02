//! `orchestral cleanup`, against real git worktrees with stub `claude` and
//! `gh`.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
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
    worktree: PathBuf,
    stubs: Stubs,
}

impl Repo {
    /// A clone of a bare origin with one worker, `issue-12`, whose branch
    /// has a commit pushed to origin.
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let origin = root.join("origin.git");
        let main = root.join("app");
        git(&root, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
        git(&root, &["clone", "-q", origin.to_str().unwrap(), "app"]);
        git(&main, &["commit", "-q", "--allow-empty", "-m", "start"]);
        git(&main, &["push", "-q", "origin", "HEAD:main"]);
        let worktree = main.join(".claude/worktrees/issue-12");
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "issue-12",
                worktree.to_str().unwrap(),
            ],
        );
        fs::write(worktree.join("login.txt"), "login\n").unwrap();
        git(&worktree, &["add", "login.txt"]);
        git(&worktree, &["commit", "-q", "-m", "Add login"]);
        git(&worktree, &["push", "-q", "origin", "issue-12"]);
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
                        worktree: worktree.clone(),
                        branch: "issue-12".into(),
                        job_id: Some(JOB.into()),
                        created_at: "2026-11-02T10:00:00Z".into(),
                    },
                )
            })
            .unwrap();
        let stubs = Stubs::new();
        stubs.respond("claude", &["stop"], "", 0);
        stubs.respond("claude", &["rm"], "", 0);
        Repo {
            _dir: dir,
            main,
            worktree,
            stubs,
        }
    }

    /// Makes `claude agents` list the worker's session, running.
    fn lists(&self, kind: &str) {
        let agent = json!({"id": JOB, "sessionId": SESSION, "kind": kind, "name": "app-issue-12", "state": "done", "pid": GONE_PID});
        self.stubs
            .respond("claude", &["agents"], &json!([agent]).to_string(), 0);
    }

    /// Makes `gh pr list` show one PR for the branch, at the branch's
    /// current head.
    fn pr(&self, state: &str) {
        let head = git(&self.worktree, &["rev-parse", "HEAD"]);
        let prs = json!([{"number": 40, "state": state, "headRefOid": head.trim()}]);
        self.stubs
            .respond("gh", &["pr", "list"], &prs.to_string(), 0);
    }

    fn run(&self, args: &[&str]) -> Output {
        self.stubs
            .orchestral()
            .args(args)
            .current_dir(&self.main)
            .output()
            .unwrap()
    }

    fn has_worker(&self) -> bool {
        Store::for_repo(&self.main)
            .unwrap()
            .read()
            .unwrap()
            .workers
            .contains_key("issue-12")
    }

    fn has_branch(&self) -> bool {
        Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", "refs/heads/issue-12"])
            .current_dir(&self.main)
            .status()
            .unwrap()
            .success()
    }

    /// The `claude` subcommands called, in order.
    fn claude_calls(&self) -> Vec<String> {
        self.stubs
            .calls("claude")
            .into_iter()
            .map(|call| call[0].clone())
            .collect()
    }

    /// Asserts a refusal that left everything in place.
    fn assert_refused(&self, output: &Output, reason: &str) {
        assert_eq!(output.status.code(), Some(2), "{}", stderr(output));
        assert!(stderr(output).contains(reason), "{}", stderr(output));
        assert!(self.worktree.exists());
        assert!(self.has_branch());
        assert!(self.has_worker());
        assert!(
            !self
                .claude_calls()
                .iter()
                .any(|call| call == "stop" || call == "rm")
        );
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {}", stderr(&output));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_squash_merged_worker_is_removed_without_force() {
    let repo = Repo::new();
    repo.lists("background");
    repo.pr("MERGED");
    // A squash merge puts the change on main as a new commit, so the
    // branch's own commit never reaches main.
    git(&repo.main, &["merge", "-q", "--squash", "issue-12"]);
    git(&repo.main, &["commit", "-q", "-m", "Add login (#40)"]);
    git(&repo.main, &["push", "-q", "origin", "HEAD:main"]);

    let output = repo.run(&["cleanup", "issue-12"]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(!repo.worktree.exists());
    assert!(!repo.has_branch());
    assert!(!repo.has_worker());
    // Stopped before the worktree goes, and removed after.
    assert_eq!(repo.claude_calls(), ["agents", "agents", "stop", "rm"]);
}

#[test]
fn a_closed_pr_finishes_the_worker_too() {
    let repo = Repo::new();
    repo.lists("background");
    repo.pr("CLOSED");

    let output = repo.run(&["cleanup", "issue-12"]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(!repo.has_worker());
}

#[test]
fn refuses_while_the_pr_is_open() {
    let repo = Repo::new();
    repo.lists("background");
    repo.pr("OPEN");

    let output = repo.run(&["cleanup", "issue-12"]);

    repo.assert_refused(&output, "PR #40 is still open");
}

#[test]
fn a_worker_without_a_pr_is_removed_only_without_commits() {
    let repo = Repo::new();
    repo.lists("background");
    repo.stubs.respond("gh", &["pr", "list"], "[]", 0);

    let output = repo.run(&["cleanup", "issue-12"]);
    repo.assert_refused(&output, "commits and no merged or closed PR");

    // A reviewer's worktree stays at origin/main.
    git(&repo.worktree, &["reset", "-q", "--hard", "origin/main"]);
    let output = repo.run(&["cleanup", "issue-12"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(!repo.has_worker());
}

#[test]
fn refuses_uncommitted_changes_unless_forced() {
    let repo = Repo::new();
    repo.lists("background");
    repo.pr("MERGED");
    fs::write(repo.worktree.join("notes.txt"), "draft\n").unwrap();

    let output = repo.run(&["cleanup", "issue-12"]);
    repo.assert_refused(&output, "uncommitted changes");

    let forced = repo.run(&["cleanup", "issue-12", "--force"]);
    assert_eq!(forced.status.code(), Some(0), "{}", stderr(&forced));
    assert!(!repo.worktree.exists());
    assert!(!repo.has_worker());
}

#[test]
fn refuses_commits_the_pr_does_not_have() {
    let repo = Repo::new();
    repo.lists("background");
    repo.pr("MERGED");
    git(
        &repo.worktree,
        &["commit", "-q", "--allow-empty", "-m", "After the PR"],
    );

    let output = repo.run(&["cleanup", "issue-12"]);

    repo.assert_refused(&output, "commits its PR doesn't have");
}

#[test]
fn never_touches_an_interactive_session() {
    let repo = Repo::new();
    repo.lists("interactive");
    repo.pr("MERGED");

    let output = repo.run(&["cleanup", "issue-12"]);

    repo.assert_refused(&output, "isn't a background session");
}
