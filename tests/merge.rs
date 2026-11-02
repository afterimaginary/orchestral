//! The merge check: one test per condition, and the command against stub
//! `gh`.

mod common;

use std::process::Command;

use common::Stubs;
use orchestral::github::Ci;
use orchestral::merge::{self, MergeCheck, Pr};
use orchestral::policy::Policy;
use orchestral::state::{MergeRoute, Review, Route, State, Store};
use serde_json::{Value, json};
use tempfile::TempDir;

const HEAD: &str = "4f2c9e1a0b5d3c1e6a2f4c8e9d7b1f2e3a4b5c6d";

fn pr() -> Pr {
    Pr {
        number: 42,
        state: "OPEN".into(),
        draft: false,
        base: "main".into(),
        mergeable: "MERGEABLE".into(),
        head: HEAD.into(),
        body: "Closes #12\n\n## Findings\n\nNone\n".into(),
        ci: Ci::Passed,
        files: vec!["src/app.rs".into()],
    }
}

/// A state where PR 42 passed review and the owner approved its head.
fn state() -> State {
    let mut state = State::default();
    state.reviews.insert(
        42,
        Review {
            sha: HEAD.into(),
            passed: true,
            failed: Vec::new(),
        },
    );
    state.approvals.insert(42, HEAD.into());
    state
}

fn supervisor_route(state: &mut State) {
    state.routes.insert(
        42,
        Route {
            to: MergeRoute::Supervisor,
            reason: "an internal refactor".into(),
        },
    );
}

fn settings(supervisor_merges: bool) -> orchestral::policy::Settings {
    let text =
        format!("---\nfleet_token_budget: 1000\nsupervisor_merges: {supervisor_merges}\n---\n");
    Policy::parse(&text, false).unwrap().settings
}

/// The names of the conditions that failed.
fn failed(check: &MergeCheck) -> Vec<&'static str> {
    check
        .conditions
        .iter()
        .filter(|condition| !condition.passed)
        .map(|condition| condition.name)
        .collect()
}

fn check(pr: &Pr, state: &State) -> MergeCheck {
    merge::evaluate(pr, state, &settings(true))
}

#[test]
fn an_approved_pr_meeting_every_condition_can_merge() {
    let check = check(&pr(), &state());

    assert!(check.mergeable(), "{check}");
    assert_eq!(check.route, MergeRoute::Human);
}

#[test]
fn each_pr_condition_fails_on_its_own() {
    type Change = fn(&mut Pr);
    let cases: [(Change, &str); 9] = [
        (|pr| pr.state = "MERGED".into(), "open"),
        (|pr| pr.draft = true, "not a draft"),
        (|pr| pr.base = "release".into(), "targets main"),
        (
            |pr| pr.mergeable = "CONFLICTING".into(),
            "no merge conflict",
        ),
        (|pr| pr.mergeable = "UNKNOWN".into(), "no merge conflict"),
        (|pr| pr.ci = Ci::None, "CI passed"),
        (|pr| pr.ci = Ci::Pending, "CI passed"),
        (|pr| pr.ci = Ci::Failed, "CI passed"),
        (
            |pr| pr.body = "## Findings\n\n- [pending] A typo\n".into(),
            "findings decided",
        ),
    ];
    for (change, condition) in cases {
        let mut pr = pr();
        change(&mut pr);
        assert_eq!(failed(&check(&pr, &state())), [condition], "{pr:?}");
    }
    let mut no_findings = pr();
    no_findings.body = "Closes #12".into();
    assert_eq!(failed(&check(&no_findings, &state())), ["findings decided"]);
}

#[test]
fn a_pause_or_a_hold_refuses() {
    let mut paused = state();
    paused.paused = true;
    let mut held = state();
    held.holds.insert(42);

    assert_eq!(failed(&check(&pr(), &paused)), ["not paused"]);
    assert_eq!(failed(&check(&pr(), &held)), ["not held"]);
}

#[test]
fn the_review_must_pass_at_the_head_commit() {
    let mut failed_review = state();
    failed_review.reviews.get_mut(&42).unwrap().passed = false;
    let mut old_review = state();
    old_review.reviews.get_mut(&42).unwrap().sha = "0000000".into();
    let mut no_review = state();
    no_review.reviews.clear();

    for state in [failed_review, old_review, no_review] {
        assert_eq!(failed(&check(&pr(), &state)), ["review passed"]);
    }
}

#[test]
fn the_owner_route_needs_an_approval_at_the_head_commit() {
    let mut old = state();
    old.approvals.insert(42, "0000000".into());
    let mut none = state();
    none.approvals.clear();

    let old = check(&pr(), &old);
    assert_eq!(failed(&old), ["approved by the owner"]);
    assert_eq!(
        old.conditions.last().unwrap().detail,
        "the approval was for an earlier commit"
    );
    assert_eq!(failed(&check(&pr(), &none)), ["approved by the owner"]);
}

#[test]
fn the_supervisor_route_needs_no_approval_but_refuses_owner_paths_and_a_disabled_policy() {
    let mut routed = state();
    routed.approvals.clear();
    supervisor_route(&mut routed);

    assert!(check(&pr(), &routed).mergeable());
    assert_eq!(
        failed(&merge::evaluate(&pr(), &routed, &settings(false))),
        ["supervisor merges allowed"]
    );
    for path in [".orchestral/policy.md", ".github/workflows/verify.yml"] {
        let mut pr = pr();
        pr.files.push(path.into());
        let check = check(&pr, &routed);
        assert_eq!(failed(&check), ["no change to .orchestral/ or .github/"]);
        assert_eq!(
            check.conditions.last().unwrap().detail,
            format!("it changes {path}")
        );
    }
}

#[test]
fn the_command_prints_each_condition_and_refuses_when_one_fails() {
    let dir = TempDir::new().unwrap();
    let main = dir.path().canonicalize().unwrap();
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&main)
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::create_dir(main.join(".orchestral")).unwrap();
    std::fs::write(
        main.join(".orchestral/policy.md"),
        "---\nfleet_token_budget: 1000\n---\n",
    )
    .unwrap();
    let store = Store::for_repo(&main).unwrap();
    store.update(|s| *s = state()).unwrap();
    let stubs = Stubs::new();
    stubs.respond(
        "gh",
        &["pr", "view", "42"],
        &json!({
            "number": 42, "state": "OPEN", "isDraft": false, "baseRefName": "main",
            "mergeable": "MERGEABLE", "headRefOid": HEAD,
            "body": "## Findings\n\nNone\n",
            "statusCheckRollup": [{"status": "IN_PROGRESS", "conclusion": null}]
        })
        .to_string(),
        0,
    );
    stubs.respond("gh", &["pr", "diff", "42"], "src/app.rs\n", 0);

    let text = stubs
        .orchestral()
        .args(["merge-check", "42"])
        .current_dir(&main)
        .output()
        .unwrap();
    let json = stubs
        .orchestral()
        .args(["merge-check", "42", "--json"])
        .current_dir(&main)
        .output()
        .unwrap();

    assert_eq!(text.status.code(), Some(2));
    let stdout = String::from_utf8(text.stdout).unwrap();
    assert!(
        stdout.starts_with("PR #42 at 4f2c9e1, merged by the owner:\n  pass  not paused\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("\n  FAIL  CI passed: checks are still running\n"),
        "{stdout}"
    );
    assert!(
        stdout.ends_with("It can't merge yet: 1 condition failed.\n"),
        "{stdout}"
    );
    let json: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(json["route"], "human");
    assert_eq!(json["conditions"].as_array().unwrap().len(), 10);
    // It reads GitHub and never merges.
    assert!(stubs.calls("gh").iter().all(|call| call[1] != "merge"));
}
