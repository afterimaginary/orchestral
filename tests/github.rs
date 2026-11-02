//! The supervisor's GitHub poll, against a stub `gh`. Each poll gets fresh
//! stubs, since GitHub changes between polls.

mod common;

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use common::Stubs;
use orchestral::github::{self, Snapshot};
use orchestral::state::{Store, Worker};
use serde_json::{Value, json};
use tempfile::TempDir;

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
        let repo = Repo { _dir: dir, main };
        repo.store()
            .update(|state| {
                state.workers.insert(
                    "issue-12".into(),
                    Worker {
                        session_name: "app-issue-12".into(),
                        issue: 12,
                        model: "sonnet".into(),
                        effort: "low".into(),
                        worktree: PathBuf::from("/nowhere"),
                        branch: "issue-12".into(),
                        job_id: Some("a001".into()),
                        created_at: "2026-11-02T10:00:00Z".into(),
                    },
                );
            })
            .unwrap();
        repo
    }

    fn store(&self) -> Store {
        Store::for_repo(&self.main).unwrap()
    }

    /// Polls once against `github`, and returns the lines and the calls.
    fn poll(&self, github: &GitHub) -> (Vec<String>, Vec<Vec<String>>) {
        let stubs = github.stubs();
        let lines = github::poll(&self.store(), &stubs.dir().join("gh")).unwrap();
        (lines, stubs.calls("gh"))
    }

    fn snapshot(&self) -> Snapshot {
        let text = fs::read_to_string(self.store().dir().join("github.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn edit_snapshot(&self, change: impl FnOnce(&mut Snapshot)) {
        let mut snapshot = self.snapshot();
        change(&mut snapshot);
        fs::write(
            self.store().dir().join("github.json"),
            serde_json::to_string(&snapshot).unwrap(),
        )
        .unwrap();
    }

    fn events(&self) -> Vec<String> {
        fs::read_to_string(self.store().dir().join("events.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| {
                serde_json::from_str::<Value>(line).unwrap()["text"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }
}

/// What the stub `gh` reports.
#[derive(Default, Clone)]
struct GitHub {
    issues: Vec<Value>,
    prs: Vec<Value>,
    comments: Vec<Value>,
    inline: Vec<Value>,
    /// `gh pr view` states, by number.
    views: Vec<(u64, &'static str)>,
    fail: bool,
}

impl GitHub {
    fn stubs(&self) -> Stubs {
        let stubs = Stubs::new();
        if self.fail {
            stubs.respond("gh", &[], "", 1);
            return stubs;
        }
        stubs.respond("gh", &["issue", "list"], &json!(self.issues).to_string(), 0);
        stubs.respond("gh", &["pr", "list"], &json!(self.prs).to_string(), 0);
        for (number, state) in &self.views {
            stubs.respond(
                "gh",
                &["pr", "view", &number.to_string()],
                &json!({ "state": state }).to_string(),
                0,
            );
        }
        let api = |endpoint: &str| format!("repos/{{owner}}/{{repo}}/{endpoint}");
        stubs.respond(
            "gh",
            &[
                "api",
                "--method",
                "GET",
                "--paginate",
                &api("issues/comments"),
            ],
            // gh prints each page as its own array.
            &self
                .comments
                .chunks(2)
                .map(|page| json!(page).to_string())
                .collect::<String>(),
            0,
        );
        stubs.respond(
            "gh",
            &[
                "api",
                "--method",
                "GET",
                "--paginate",
                &api("pulls/comments"),
            ],
            &json!(self.inline).to_string(),
            0,
        );
        stubs
    }
}

fn issue(number: u64, title: &str, labels: &[&str]) -> Value {
    let labels: Vec<Value> = labels.iter().map(|name| json!({ "name": name })).collect();
    json!({ "number": number, "title": title, "labels": labels })
}

fn pr(number: u64, title: &str, branch: &str, draft: bool, checks: Value) -> Value {
    json!({
        "number": number,
        "title": title,
        "isDraft": draft,
        "headRefName": branch,
        "headRefOid": "abc1234",
        "statusCheckRollup": checks,
        "reviews": [],
    })
}

fn check(status: &str, conclusion: &str) -> Value {
    json!([{ "__typename": "CheckRun", "name": "verify", "status": status, "conclusion": conclusion }])
}

/// A comment created now, so it's inside the window the poll reads.
fn comment(id: u64, number: u64, login: &str, body: &str) -> Value {
    json!({
        "id": id,
        "user": { "login": login },
        "body": body,
        "created_at": jiff::Timestamp::now().to_string(),
        "issue_url": format!("https://api.github.com/repos/o/app/issues/{number}"),
    })
}

fn inline(id: u64, number: u64, login: &str, path: &str, line: u64, body: &str) -> Value {
    json!({
        "id": id,
        "user": { "login": login },
        "body": body,
        "created_at": jiff::Timestamp::now().to_string(),
        "pull_request_url": format!("https://api.github.com/repos/o/app/pulls/{number}"),
        "path": path,
        "line": line,
    })
}

/// Issue #12 has a worker on a draft PR, #40, with CI running.
fn baseline() -> GitHub {
    GitHub {
        issues: vec![issue(12, "Add login", &[]), issue(13, "Fix typo", &[])],
        prs: vec![
            pr(40, "Add login", "issue-12", true, check("IN_PROGRESS", "")),
            pr(41, "Bump deps", "deps", false, json!([])),
        ],
        ..GitHub::default()
    }
}

#[test]
fn the_first_poll_takes_the_snapshot_without_events() {
    let repo = Repo::new();
    let (lines, calls) = repo.poll(&baseline());
    assert_eq!(lines, Vec::<String>::new());
    assert!(calls.iter().all(|call| call[0] != "api"), "{calls:?}");
    assert_eq!(repo.snapshot().prs.len(), 2);
}

#[test]
fn issues_and_prs_produce_one_event_per_change() {
    let repo = Repo::new();
    repo.poll(&baseline());
    let now = GitHub {
        issues: vec![
            issue(12, "Add login", &[]),
            issue(14, "Dark mode", &["enhancement"]),
        ],
        prs: vec![
            pr(
                40,
                "Add login",
                "issue-12",
                false,
                check("COMPLETED", "FAILURE"),
            ),
            pr(42, "Docs", "docs", false, check("COMPLETED", "SUCCESS")),
        ],
        views: vec![(41, "MERGED")],
        ..GitHub::default()
    };
    let (lines, _) = repo.poll(&now);
    let expected = [
        "issue #14 \"Dark mode\" opened [enhancement]: decide whether it's ready for a worker",
        "issue #13 \"Fix typo\" closed: no action needed",
        "pr #40 \"Add login\" (worker issue-12): ready for review: route it for review",
        "pr #40 \"Add login\" (worker issue-12): CI failed: have issue-12 fix it",
        "pr #42 \"Docs\" (branch docs): opened, ready for review: route it for review",
        "pr #42 \"Docs\" (branch docs): CI passed: no action needed",
        "pr #41 \"Bump deps\" (branch deps): merged: no action needed",
    ];
    assert_eq!(lines, expected);
    assert_eq!(repo.events(), expected);
    // Nothing changed since, so nothing is reported again.
    let (again, _) = repo.poll(&GitHub {
        views: vec![],
        ..now
    });
    assert_eq!(again, Vec::<String>::new());
}

#[test]
fn a_worker_s_pr_merging_or_closing_names_the_worker() {
    let repo = Repo::new();
    repo.poll(&baseline());
    let (merged, _) = repo.poll(&GitHub {
        prs: vec![pr(41, "Bump deps", "deps", false, json!([]))],
        views: vec![(40, "MERGED")],
        ..baseline()
    });
    assert_eq!(
        merged,
        ["pr #40 \"Add login\" (worker issue-12): merged: clean up issue-12"]
    );

    let repo = Repo::new();
    repo.poll(&baseline());
    let (closed, _) = repo.poll(&GitHub {
        issues: vec![issue(13, "Fix typo", &[])],
        prs: vec![pr(41, "Bump deps", "deps", false, json!([]))],
        views: vec![(40, "CLOSED")],
        ..baseline()
    });
    assert_eq!(
        closed,
        [
            "issue #12 \"Add login\" closed (worker issue-12): stop the worker if it's still on it",
            "pr #40 \"Add login\" (worker issue-12): closed without merging: stop issue-12 if it's still on it",
        ]
    );
}

#[test]
fn every_kind_of_comment_produces_an_event_once() {
    let repo = Repo::new();
    repo.poll(&baseline());
    let mut with_review = baseline();
    with_review.prs[0]["reviews"] = json!([
        { "id": "PRR_1", "author": { "login": "owner" }, "body": "", "state": "APPROVED" },
        // The container of inline comments, with no text of its own.
        { "id": "PRR_2", "author": { "login": "owner" }, "body": "", "state": "COMMENTED" },
    ]);
    let now = GitHub {
        comments: vec![
            comment(1, 40, "owner", "\nPlease rename the flag.\nThanks"),
            comment(2, 13, "someone", "Still happening on main"),
            comment(
                3,
                12,
                "worker-account",
                "<!-- orchestral -->\nI opened PR #40.",
            ),
        ],
        inline: vec![inline(9, 40, "owner", "src/login.rs", 12, "This can panic")],
        ..with_review
    };
    let (lines, calls) = repo.poll(&now);
    assert_eq!(
        lines,
        [
            "pr #40 \"Add login\" (worker issue-12): review from owner, approved: pass it to issue-12",
            "pr #40 \"Add login\" (worker issue-12): comment from owner: \"Please rename the flag.\": pass it to issue-12",
            "issue #13 \"Fix typo\": comment from someone: \"Still happening on main\": answer it if it needs you",
            "pr #40 \"Add login\" (worker issue-12): inline comment from owner on src/login.rs:12: \"This can panic\": pass it to issue-12",
        ]
    );
    // Comments are read from before the last poll, as whole seconds.
    let api = calls.iter().find(|call| call[0] == "api").unwrap();
    assert!(
        api.iter()
            .any(|arg| arg.starts_with("since=") && arg.ends_with('Z') && !arg.contains('.')),
        "{api:?}"
    );
    let (again, _) = repo.poll(&now);
    assert_eq!(again, Vec::<String>::new());
}

#[test]
fn an_old_comment_edited_now_is_not_new() {
    let repo = Repo::new();
    repo.poll(&baseline());
    let mut edited = comment(5, 13, "someone", "Edited later");
    edited["created_at"] = json!("2026-01-01T00:00:00Z");
    let (lines, _) = repo.poll(&GitHub {
        comments: vec![edited],
        ..baseline()
    });
    assert_eq!(lines, Vec::<String>::new());
}

#[test]
fn failing_polls_back_off_and_are_reported_once() {
    let repo = Repo::new();
    repo.poll(&baseline());
    let failing = GitHub {
        fail: true,
        ..GitHub::default()
    };
    let (lines, _) = repo.poll(&failing);
    assert_eq!(lines, Vec::<String>::new());
    // Backing off: the next poll doesn't call gh at all.
    let (_, calls) = repo.poll(&failing);
    assert_eq!(calls, Vec::<Vec<String>>::new());
    let mut reported = Vec::new();
    for _ in 2..=7 {
        repo.edit_snapshot(|snapshot| snapshot.retry_after = None);
        reported.extend(repo.poll(&failing).0);
    }
    assert_eq!(reported.len(), 1, "{reported:?}");
    assert!(
        reported[0].starts_with("watcher: 5 polls failed: gh issue list failed"),
        "{reported:?}"
    );
    // A success clears the failures, and doesn't report the snapshot's
    // contents again.
    repo.edit_snapshot(|snapshot| snapshot.retry_after = None);
    let (lines, _) = repo.poll(&baseline());
    assert_eq!(lines, Vec::<String>::new());
    assert_eq!(repo.snapshot().failures, 0);
}

#[test]
fn failures_before_the_first_snapshot_do_not_make_everything_new() {
    let repo = Repo::new();
    repo.poll(&GitHub {
        fail: true,
        ..GitHub::default()
    });
    repo.edit_snapshot(|snapshot| snapshot.retry_after = None);
    let (lines, _) = repo.poll(&baseline());
    assert_eq!(lines, Vec::<String>::new());
}

#[test]
fn a_pr_whose_outcome_can_t_be_read_fails_the_poll_and_is_read_again() {
    let repo = Repo::new();
    repo.poll(&baseline());
    let gone = GitHub {
        prs: vec![pr(41, "Bump deps", "deps", false, json!([]))],
        ..baseline()
    };
    // No canned reply for `gh pr view 40`, so it fails.
    let (lines, _) = repo.poll(&gone);
    assert_eq!(lines, Vec::<String>::new());
    assert_eq!(repo.snapshot().failures, 1);
    assert!(repo.snapshot().prs.contains_key(&40));
    repo.edit_snapshot(|snapshot| snapshot.retry_after = None);
    let (lines, _) = repo.poll(&GitHub {
        views: vec![(40, "MERGED")],
        ..gone
    });
    assert_eq!(
        lines,
        ["pr #40 \"Add login\" (worker issue-12): merged: clean up issue-12"]
    );
}

#[test]
fn ci_failing_again_after_a_new_push_is_reported() {
    let repo = Repo::new();
    let failing = GitHub {
        prs: vec![pr(
            40,
            "Add login",
            "issue-12",
            false,
            check("COMPLETED", "FAILURE"),
        )],
        ..baseline()
    };
    repo.poll(&failing);
    let mut pushed = failing.clone();
    pushed.prs[0]["headRefOid"] = json!("def5678");
    let (lines, _) = repo.poll(&pushed);
    assert_eq!(
        lines,
        ["pr #40 \"Add login\" (worker issue-12): CI failed: have issue-12 fix it"]
    );
}

#[test]
fn a_pending_review_is_reported_once_submitted() {
    let repo = Repo::new();
    repo.poll(&baseline());
    let mut pending = baseline();
    pending.prs[0]["reviews"] = json!([
        { "id": "PRR_3", "author": { "login": "owner" }, "body": "Draft thoughts", "state": "PENDING" },
    ]);
    assert_eq!(repo.poll(&pending).0, Vec::<String>::new());
    let mut submitted = baseline();
    submitted.prs[0]["reviews"] = json!([
        { "id": "PRR_3", "author": { "login": "owner" }, "body": "Needs a test", "state": "CHANGES_REQUESTED" },
    ]);
    assert_eq!(
        repo.poll(&submitted).0,
        [
            "pr #40 \"Add login\" (worker issue-12): review from owner, changes requested: \"Needs a test\": pass it to issue-12"
        ]
    );
}

#[test]
fn a_comment_on_a_pr_closed_earlier_is_still_called_a_pr() {
    let repo = Repo::new();
    repo.poll(&baseline());
    let mut late = comment(7, 99, "owner", "Thanks for this");
    late["html_url"] = json!("https://github.com/o/app/pull/99#issuecomment-7");
    let (lines, _) = repo.poll(&GitHub {
        comments: vec![late],
        ..baseline()
    });
    assert_eq!(
        lines,
        ["pr #99: comment from owner: \"Thanks for this\": answer it if it needs you"]
    );
}

#[test]
fn only_an_escalation_raised_during_the_run_counts() {
    let repo = Repo::new();
    let store = repo.store();
    let with = |labelled: &[u64]| GitHub {
        issues: [12, 13]
            .iter()
            .map(|&n| {
                let labels: &[&str] = if labelled.contains(&n) {
                    &["needs-discussion"]
                } else {
                    &[]
                };
                issue(n, "Work", labels)
            })
            .collect(),
        ..GitHub::default()
    };
    // #13 was already waiting on the owner before the run.
    repo.poll(&with(&[13]));
    assert!(!github::has_escalation(&store));

    repo.poll(&with(&[12, 13]));
    assert_eq!(repo.snapshot().escalations, [12].into());
    assert!(github::has_escalation(&store));
    repo.poll(&with(&[12, 13]));
    assert!(github::has_escalation(&store));

    repo.poll(&with(&[13]));
    assert!(!github::has_escalation(&store));
}
