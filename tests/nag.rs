//! Reminders to the owner about workers waiting on them, with an injected
//! clock, and stub `claude` and `gh`.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;

use common::Stubs;
use jiff::{SignedDuration, Timestamp};
use orchestral::nag::{self, Waiting, Why};
use orchestral::observe::Observed;
use orchestral::state::{Nag, State, Store, Worker};
use serde_json::json;
use tempfile::TempDir;

const T0: &str = "2026-11-02T12:00:00Z";

fn at(minutes: i64) -> Timestamp {
    T0.parse::<Timestamp>().unwrap() + SignedDuration::from_mins(minutes)
}

fn worker(issue: u64, job_id: Option<&str>) -> Worker {
    Worker {
        session_name: format!("app-issue-{issue}"),
        issue,
        model: "claude-sonnet-5-5".into(),
        effort: "low".into(),
        worktree: PathBuf::from("/nowhere"),
        branch: format!("issue-{issue}"),
        job_id: job_id.map(Into::into),
        review_of: None,
        created_at: T0.into(),
    }
}

fn waiting(worker: &str) -> Waiting {
    Waiting {
        worker: worker.into(),
        issue: 12,
        why: Why::Escalated,
    }
}

#[test]
fn each_gap_counts_from_the_reminder_before_and_the_last_repeats() {
    let gaps = [15, 30];
    let mut nag = Nag {
        since: at(0).to_string(),
        sent: 0,
        last_sent_at: None,
    };

    assert!(!nag::due(&nag, &gaps, at(14)));
    assert!(nag::due(&nag, &gaps, at(15)));
    nag.sent = 1;
    nag.last_sent_at = Some(at(15).to_string());
    assert!(!nag::due(&nag, &gaps, at(44)));
    assert!(nag::due(&nag, &gaps, at(45)));
    // Past the end of the list, the last gap stays.
    nag.sent = 2;
    nag.last_sent_at = Some(at(45).to_string());
    assert!(!nag::due(&nag, &gaps, at(74)));
    assert!(nag::due(&nag, &gaps, at(75)));
    assert!(!nag::due(&nag, &[], at(1000)));
}

#[test]
fn the_count_starts_again_once_the_worker_stops_waiting() {
    let mut nags = BTreeMap::new();
    let both = [waiting("issue-12"), waiting("issue-13")];
    nag::track(&mut nags, &both, &[15], at(0));
    nags.get_mut("issue-12").unwrap().sent = 3;

    // The owner answered issue-12.
    let due = nag::track(&mut nags, &both[1..], &[15], at(20));
    assert_eq!(due, [&both[1]]);
    assert!(!nags.contains_key("issue-12"));

    // It waits again: a new record, so the first gap applies.
    let due = nag::track(&mut nags, &both[..1], &[15], at(30));
    assert!(due.is_empty());
    assert_eq!(nags["issue-12"].sent, 0);
    assert_eq!(nags["issue-12"].since, at(30).to_string());
}

#[test]
fn an_escalation_or_a_blocked_session_waits_on_the_owner_but_a_rate_limit_does_not() {
    let mut state = State::default();
    for (issue, observed) in [
        (12, Observed::Waiting),
        (13, Observed::Waiting),
        (14, Observed::Waiting),
        (15, Observed::Idle),
    ] {
        let name = format!("issue-{issue}");
        state.workers.insert(name.clone(), worker(issue, None));
        state.observed.insert(name, observed);
    }
    let escalated = BTreeSet::from([12]);

    let waiting = nag::waiting(&state, &escalated, |worker| match worker.issue {
        14 => None,
        _ => Some("approve a push".into()),
    });

    let whys: Vec<_> = waiting
        .iter()
        .map(|w| (w.worker.as_str(), &w.why))
        .collect();
    assert_eq!(
        whys,
        [
            ("issue-12", &Why::Escalated),
            ("issue-13", &Why::Session("approve a push".into())),
        ]
    );
}

#[test]
fn the_message_names_the_worker_the_question_and_the_link() {
    let text = nag::message(
        "issue-12",
        "app-issue-12",
        "Should sign-up need an email?",
        "https://github.com/o/app/issues/12",
    );
    let long = nag::message("issue-12", "app-issue-12", &"x".repeat(400), "u");

    assert_eq!(
        text,
        "issue-12 (app-issue-12) is waiting on you: Should sign-up need an email?. https://github.com/o/app/issues/12"
    );
    assert!(long.contains(&format!("{}….", "x".repeat(160))), "{long}");
}

fn with_question() -> serde_json::Value {
    json!({
        "url": "https://github.com/o/app/issues/12",
        "title": "Sign-up",
        "comments": [
            {"body": "<!-- orchestral -->\n**Question for the owner:** Should sign-up need an email?\n\nContext."},
            {"body": "A comment from someone else."}
        ]
    })
}

struct Repo {
    _dir: TempDir,
    store: Store,
    stubs: Stubs,
}

impl Repo {
    /// A repository whose issue #12 was escalated during the run, with
    /// worker issue-12 on it.
    fn escalated(issue: serde_json::Value) -> Self {
        let dir = TempDir::new().unwrap();
        let main = dir.path().join("app");
        std::fs::create_dir_all(main.join(".orchestral")).unwrap();
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&main)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::write(
            main.join(".orchestral/policy.md"),
            "---\nfleet_token_budget: 1000\nnag_minutes: [15, 30]\n---\n",
        )
        .unwrap();
        let store = Store::for_repo(&main).unwrap();
        store
            .update(|state| {
                state
                    .workers
                    .insert("issue-12".into(), worker(12, Some("aaaa1111")))
            })
            .unwrap();
        std::fs::write(
            store.dir().join("github.json"),
            json!({
                "issues": {"12": {"title": "Sign-up", "labels": ["needs-discussion"]}},
                "prs": {}, "polled_at": T0, "seen_comments": [], "failures": 0,
                "retry_after": null, "escalations": [12]
            })
            .to_string(),
        )
        .unwrap();
        let stubs = Stubs::new();
        stubs.respond("gh", &["issue", "view", "12"], &issue.to_string(), 0);
        stubs.respond(
            "claude",
            &["--bg"],
            "backgrounded · 5eed0001 · app-nag-1\n",
            0,
        );
        Repo {
            _dir: dir,
            store,
            stubs,
        }
    }

    fn check(&self, minutes: i64) -> Vec<String> {
        nag::check(
            &self.store,
            &self.tool("claude"),
            &self.tool("gh"),
            at(minutes),
        )
        .unwrap()
    }

    fn tool(&self, name: &str) -> PathBuf {
        self.stubs.dir().join(name)
    }
}

fn arg_after<'a>(call: &'a [String], flag: &str) -> Option<&'a str> {
    let index = call.iter().position(|arg| arg == flag)?;
    call.get(index + 1).map(String::as_str)
}

#[test]
fn a_due_reminder_starts_a_one_shot_haiku_session_and_stops_it_afterwards() {
    let repo = Repo::escalated(with_question());

    assert!(repo.check(0).is_empty());
    assert!(repo.stubs.calls("claude").is_empty());
    let lines = repo.check(15);

    let calls = repo.stubs.calls("claude");
    assert_eq!(calls.len(), 1, "{calls:?}");
    let call = &calls[0];
    assert_eq!(arg_after(call, "--name"), Some("app-nag-1"));
    assert_eq!(arg_after(call, "--model"), Some("claude-haiku-5-5"));
    assert_eq!(arg_after(call, "--effort"), Some("low"));
    assert!(!call.iter().any(|arg| arg == "--permission-mode"));
    let prompt = call.last().unwrap();
    assert!(prompt.starts_with("Load the PushNotification tool with ToolSearch"));
    assert!(prompt.ends_with(
        "issue-12 (app-issue-12) is waiting on you: Should sign-up need an email?. https://github.com/o/app/issues/12"
    ));
    assert_eq!(lines.len(), 1);
    let state = repo.store.read().unwrap();
    assert_eq!(state.nags["issue-12"].sent, 1);
    assert_eq!(state.nag_sessions[0].job_id, "5eed0001");

    // Once its turn is done, the session is stopped, and nothing else is due.
    repo.stubs.respond(
        "claude",
        &["agents"],
        &json!([{"id": "5eed0001", "pid": 4242, "sessionId": "s", "state": "done"}]).to_string(),
        0,
    );
    repo.stubs.respond("claude", &["stop"], "", 0);
    repo.check(16);

    let calls = repo.stubs.calls("claude");
    assert!(
        calls.iter().any(|call| call == &["stop", "5eed0001"]),
        "{calls:?}"
    );
    let launches = calls.iter().filter(|call| call[0] == "--bg").count();
    assert_eq!(launches, 1);
    assert!(repo.store.read().unwrap().nag_sessions.is_empty());
}

#[test]
fn a_failed_launch_is_tried_again_the_next_minute() {
    let repo = Repo::escalated(with_question());
    let failing = Stubs::new();
    failing.respond("claude", &["--bg"], "", 1);
    repo.check(0);

    let lines = nag::check(
        &repo.store,
        &failing.dir().join("claude"),
        &repo.tool("gh"),
        at(15),
    )
    .unwrap();
    assert!(
        lines[0].starts_with("can't remind the owner about issue-12"),
        "{lines:?}"
    );
    assert_eq!(repo.store.read().unwrap().nags["issue-12"].sent, 0);

    repo.check(16);
    assert_eq!(repo.store.read().unwrap().nags["issue-12"].sent, 1);
}

#[test]
fn without_an_escalation_comment_the_reminder_names_the_issue() {
    let repo = Repo::escalated(json!({"url": "u", "title": "Sign-up", "comments": []}));
    repo.check(0);
    repo.check(15);

    let prompt = repo.stubs.calls("claude")[0].last().unwrap().clone();
    assert!(
        prompt.ends_with("is waiting on you: a decision on \"Sign-up\". u"),
        "{prompt}"
    );
}
