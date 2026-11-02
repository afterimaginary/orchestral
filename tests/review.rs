//! `orchestral review`, with a stub `gh`.

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};

use common::Stubs;
use orchestral::state::{Store, Worker};
use serde_json::json;
use tempfile::TempDir;

const FIRST: &str = "1111111aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SECOND: &str = "2222222bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

struct Repo {
    _dir: TempDir,
    main: PathBuf,
}

impl Repo {
    /// Worker `issue-12` on PR #40, and its reviewer `review-40`.
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let main = dir.path().canonicalize().unwrap();
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&main)
            .status()
            .unwrap();
        assert!(status.success());
        let worker = |branch: &str, review_of| Worker {
            session_name: format!("app-{branch}"),
            issue: 12,
            model: "claude-sonnet-5-5".into(),
            effort: "low".into(),
            worktree: main.join(branch),
            branch: branch.into(),
            job_id: None,
            review_of,
            created_at: "2026-11-02T10:00:00Z".into(),
        };
        Store::for_repo(&main)
            .unwrap()
            .update(|state| {
                state
                    .workers
                    .insert("issue-12".into(), worker("issue-12", None));
                state
                    .workers
                    .insert("review-40".into(), worker("review-40", Some(40)));
            })
            .unwrap();
        Repo { _dir: dir, main }
    }

    /// Runs `review` with PR #40 at `head` in state `state`.
    fn review(&self, args: &[&str], head: &str, state: &str) -> (Output, Stubs) {
        let stubs = Stubs::new();
        let view = json!({"state": state, "headRefOid": head, "headRefName": "issue-12"});
        stubs.respond("gh", &["pr", "view", "40"], &view.to_string(), 0);
        stubs.respond("gh", &["issue", "edit"], "", 0);
        let output = stubs
            .orchestral()
            .arg("review")
            .args(args)
            .current_dir(&self.main)
            .output()
            .unwrap();
        (output, stubs)
    }

    fn events(&self) -> Vec<String> {
        let text = std::fs::read_to_string(self.main.join(".git/orchestral/events.jsonl"))
            .unwrap_or_default();
        text.lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["text"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }

    fn store(&self) -> Store {
        Store::for_repo(&self.main).unwrap()
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_pass_is_recorded_for_the_head_commit() {
    let repo = Repo::new();

    let (output, _) = repo.review(&["40", "--pass", "--sha", "1111111"], FIRST, "OPEN");

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let review = &repo.store().read().unwrap().reviews[&40];
    assert_eq!(review.sha, FIRST);
    assert!(review.passed);
    assert_eq!(
        repo.events(),
        ["pr #40 (worker issue-12): review passed at 1111111: no action needed"]
    );
}

#[test]
fn a_verdict_for_another_commit_is_refused() {
    let repo = Repo::new();

    let (output, _) = repo.review(&["40", "--pass", "--sha", "1111111"], SECOND, "OPEN");

    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("PR #40's head is 2222222, not 1111111"));
    assert!(repo.store().read().unwrap().reviews.is_empty());
}

#[test]
fn two_failed_rounds_block_the_issue() {
    let repo = Repo::new();

    let (first, stubs) = repo.review(&["40", "--fail", "--sha", FIRST], FIRST, "OPEN");
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    // A second verdict on the same commit isn't another round.
    repo.review(&["40", "--fail", "--sha", FIRST], FIRST, "OPEN");
    assert!(stubs.calls("gh").iter().all(|call| call[0] != "issue"));

    let (second, stubs) = repo.review(&["40", "--fail", "--sha", SECOND], SECOND, "OPEN");

    assert_eq!(second.status.code(), Some(0), "{}", stderr(&second));
    assert!(
        stubs.calls("gh").contains(
            &["issue", "edit", "12", "--add-label", "blocked"]
                .map(String::from)
                .to_vec()
        )
    );
    let events = repo.events();
    assert_eq!(
        events.first().unwrap(),
        "pr #40 (worker issue-12): review failed at 1111111 (round 1 of 2): have issue-12 fix it"
    );
    assert_eq!(
        events.last().unwrap(),
        "pr #40 (worker issue-12): review failed at 2222222 (round 2 of 2): issue #12 is now blocked, so stop issue-12 and move on"
    );
}

#[test]
fn a_closed_pr_or_one_without_a_reviewer_is_refused() {
    let repo = Repo::new();
    let (closed, _) = repo.review(&["40", "--pass", "--sha", FIRST], FIRST, "MERGED");
    assert_eq!(closed.status.code(), Some(2));
    assert!(stderr(&closed).contains("PR #40 is merged"));

    repo.store()
        .update(|state| state.workers.remove("review-40"))
        .unwrap();
    let (unreviewed, _) = repo.review(&["40", "--pass", "--sha", FIRST], FIRST, "OPEN");
    assert_eq!(unreviewed.status.code(), Some(2));
    assert!(stderr(&unreviewed).contains("PR #40 has no reviewer in this fleet"));
}
