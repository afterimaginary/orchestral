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

/// A repository with a policy and the state from `state()`, changed by
/// `change`.
fn repo(change: impl FnOnce(&mut State)) -> (TempDir, std::path::PathBuf) {
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
        "---\nfleet_token_budget: 1000\nsupervisor_merges: true\n---\n",
    )
    .unwrap();
    let store = Store::for_repo(&main).unwrap();
    store
        .update(|s| {
            *s = state();
            change(s);
        })
        .unwrap();
    (dir, main)
}

fn view(checks: Value) -> String {
    json!({
        "state": "OPEN", "isDraft": false, "baseRefName": "main",
        "mergeable": "MERGEABLE", "headRefOid": HEAD,
        "body": "## Findings\n\nNone\n",
        "statusCheckRollup": checks
    })
    .to_string()
}

fn merge_check(stubs: &Stubs, main: &std::path::Path, json: bool) -> std::process::Output {
    let mut command = stubs.orchestral();
    command.args(["merge-check", "42"]).current_dir(main);
    if json {
        command.arg("--json");
    }
    command.output().unwrap()
}

#[test]
fn the_command_prints_each_condition_and_refuses_when_one_fails() {
    let (_dir, main) = repo(|_| {});
    let stubs = Stubs::new();
    stubs.respond(
        "gh",
        &["pr", "view", "42"],
        &view(json!([{"status": "IN_PROGRESS", "conclusion": null}])),
        0,
    );

    let text = merge_check(&stubs, &main, false);
    let json = merge_check(&stubs, &main, true);

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
    assert!(stdout.ends_with("1 condition failed.\n"), "{stdout}");
    let json: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(json["route"], "human");
    let names: Vec<&str> = json["conditions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|condition| condition["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"CI passed"));
    assert!(names.contains(&"approved by the owner"));
    // The owner's route doesn't need the changed paths, and nothing merges.
    assert!(
        stubs
            .calls("gh")
            .iter()
            .all(|call| call[0] == "pr" && call[1] == "view")
    );
}

#[test]
fn only_skipped_checks_are_not_a_pass() {
    let (_dir, main) = repo(|_| {});
    let stubs = Stubs::new();
    stubs.respond(
        "gh",
        &["pr", "view", "42"],
        &view(json!([
            {"status": "COMPLETED", "conclusion": "SKIPPED"},
            {"status": "COMPLETED", "conclusion": "NEUTRAL"}
        ])),
        0,
    );

    let output = merge_check(&stubs, &main, false);

    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("\n  FAIL  CI passed: no check has succeeded\n"),
        "{stdout}"
    );
}

#[test]
fn the_supervisor_route_reads_renames_out_of_a_checked_directory() {
    let (_dir, main) = repo(|state| {
        state.approvals.clear();
        supervisor_route(state);
    });
    let stubs = Stubs::new();
    stubs.respond(
        "gh",
        &["pr", "view", "42"],
        &view(json!([{"status": "COMPLETED", "conclusion": "SUCCESS"}])),
        0,
    );
    stubs.respond(
        "gh",
        &["api", "repos/{owner}/{repo}/pulls/42/files"],
        "ci/verify.yml\n.github/workflows/verify.yml\n",
        0,
    );

    let output = merge_check(&stubs, &main, false);

    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains(
            "FAIL  no change to .orchestral/ or .github/: it changes .github/workflows/verify.yml\n"
        ),
        "{stdout}"
    );
    let api = stubs
        .calls("gh")
        .into_iter()
        .find(|call| call[0] == "api")
        .unwrap();
    assert!(api.contains(&"--paginate".to_string()), "{api:?}");
}

/// The poll's snapshot, with PR #42 at `head` on branch `issue-12`.
fn snapshot(main: &std::path::Path, head: &str) {
    let snapshot = json!({
        "issues": {},
        "prs": {"42": {
            "title": "Add login", "draft": false, "branch": "issue-12",
            "head": head, "ci": "passed", "reviews": []
        }},
        "polled_at": "2026-11-02T12:00:00Z",
        "seen_comments": [], "failures": 0, "retry_after": null, "escalations": []
    });
    std::fs::write(
        main.join(".git/orchestral/github.json"),
        snapshot.to_string(),
    )
    .unwrap();
}

fn passing() -> Value {
    json!([{"status": "COMPLETED", "conclusion": "SUCCESS"}])
}

fn merges(stubs: &Stubs) -> Vec<Vec<String>> {
    stubs
        .calls("gh")
        .into_iter()
        .filter(|call| call[..2] == ["pr", "merge"])
        .collect()
}

fn events(main: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(main.join(".git/orchestral/events.jsonl"))
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

#[test]
fn the_supervisor_merges_an_approved_pr_at_its_checked_head() {
    let (_dir, main) = repo(|_| {});
    snapshot(&main, HEAD);
    let stubs = Stubs::new();
    stubs.respond("gh", &["pr", "view", "42"], &view(passing()), 0);
    stubs.respond("gh", &["pr", "merge", "42"], "", 0);
    let store = Store::for_repo(&main).unwrap();

    let lines = merge::merge_ready(&store, &stubs.dir().join("gh")).unwrap();

    assert_eq!(
        merges(&stubs),
        [["pr", "merge", "42", "--squash", "--match-head-commit", HEAD]]
    );
    let event = "pr #42 \"Add login\" (branch issue-12): merged by the supervisor at 4f2c9e1: start any work it unblocks";
    assert_eq!(lines, [event]);
    assert_eq!(events(&main), [event]);
    // The next poll doesn't report it again.
    assert!(
        orchestral::github::open_pr_snapshot(&store)
            .unwrap()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn the_supervisor_route_merges_without_an_approval_by_the_policy_method() {
    let (_dir, main) = repo(|state| {
        state.approvals.clear();
        supervisor_route(state);
    });
    std::fs::write(
        main.join(".orchestral/policy.md"),
        "---\nfleet_token_budget: 1000\nsupervisor_merges: true\nmerge_method: rebase\n---\n",
    )
    .unwrap();
    snapshot(&main, HEAD);
    let stubs = Stubs::new();
    stubs.respond("gh", &["pr", "view", "42"], &view(passing()), 0);
    stubs.respond(
        "gh",
        &["api", "repos/{owner}/{repo}/pulls/42/files"],
        "src/app.rs\n",
        0,
    );
    stubs.respond("gh", &["pr", "merge", "42"], "", 0);

    merge::merge_ready(&Store::for_repo(&main).unwrap(), &stubs.dir().join("gh")).unwrap();

    assert_eq!(
        merges(&stubs),
        [["pr", "merge", "42", "--rebase", "--match-head-commit", HEAD]]
    );
}

#[test]
fn nothing_merges_without_the_owners_approval_or_with_a_failed_condition() {
    // No approval on the owner's route: not even read.
    let (_dir, main) = repo(|state| state.approvals.clear());
    snapshot(&main, HEAD);
    let stubs = Stubs::new();
    assert!(
        merge::merge_ready(&Store::for_repo(&main).unwrap(), &stubs.dir().join("gh"))
            .unwrap()
            .is_empty()
    );
    assert!(stubs.calls("gh").is_empty());

    // Approved, but a finding is still pending.
    let (_dir, main) = repo(|_| {});
    snapshot(&main, HEAD);
    let stubs = Stubs::new();
    let body = json!({
        "state": "OPEN", "isDraft": false, "baseRefName": "main",
        "mergeable": "MERGEABLE", "headRefOid": HEAD,
        "body": "## Findings\n\n- [pending] A typo\n", "statusCheckRollup": passing()
    });
    stubs.respond("gh", &["pr", "view", "42"], &body.to_string(), 0);
    merge::merge_ready(&Store::for_repo(&main).unwrap(), &stubs.dir().join("gh")).unwrap();
    assert!(merges(&stubs).is_empty());
    assert!(events(&main).is_empty());
}

#[test]
fn a_moved_head_is_not_merged() {
    // The poll saw the approved head, but a commit was pushed since.
    let (_dir, main) = repo(|_| {});
    snapshot(&main, HEAD);
    let stubs = Stubs::new();
    let moved = view(passing()).replace(HEAD, "9999999aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    stubs.respond("gh", &["pr", "view", "42"], &moved, 0);

    merge::merge_ready(&Store::for_repo(&main).unwrap(), &stubs.dir().join("gh")).unwrap();

    assert!(merges(&stubs).is_empty());
}

#[test]
fn a_refused_merge_is_logged_and_left_for_the_next_minute() {
    let (_dir, main) = repo(|_| {});
    snapshot(&main, HEAD);
    let stubs = Stubs::new();
    stubs.respond("gh", &["pr", "view", "42"], &view(passing()), 0);
    // GitHub refuses, as when the head moved after the check.
    stubs.respond("gh", &["pr", "merge", "42"], "", 1);
    let store = Store::for_repo(&main).unwrap();

    let lines = merge::merge_ready(&store, &stubs.dir().join("gh")).unwrap();

    assert_eq!(lines.len(), 1);
    assert!(lines[0].starts_with("can't merge PR #42: "), "{lines:?}");
    assert!(events(&main).is_empty());
    assert_eq!(
        orchestral::github::open_pr_snapshot(&store)
            .unwrap()
            .unwrap()
            .len(),
        1
    );
}
