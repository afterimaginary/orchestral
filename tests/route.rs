//! `orchestral route` and asking the owner to merge, with a stub `gh`.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::Stubs;
use orchestral::route;
use orchestral::state::{MergeRoute, Orchestrator, Review, Route, Store};
use serde_json::{Value, json};
use tempfile::TempDir;

const HEAD: &str = "4f2c9e1a0b5d3c1e6a2f4c8e9d7b1f2e3a4b5c6d";
const SESSION: &str = "0b7c6f9e-1d2a-4c3b-9e8f-7a6b5c4d3e2f";

/// A repository whose orchestrator is `SESSION`.
fn repo(supervisor_merges: bool) -> (TempDir, PathBuf) {
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
        format!("---\nfleet_token_budget: 1000\nsupervisor_merges: {supervisor_merges}\n---\n"),
    )
    .unwrap();
    Store::for_repo(&main)
        .unwrap()
        .update(|state| {
            state.orchestrator = Some(Orchestrator {
                session_id: SESSION.into(),
                claimed_at: "2026-11-02T10:00:00Z".into(),
            })
        })
        .unwrap();
    (dir, main)
}

fn store(main: &Path) -> Store {
    Store::for_repo(main).unwrap()
}

/// Runs `route 42 <to>` from `session`, with PR #42 open and changing
/// `files`.
fn run(main: &Path, to: &str, session: Option<&str>, files: &str) -> (Output, Stubs) {
    let stubs = Stubs::new();
    stubs.respond("gh", &["pr", "view", "42"], r#"{"state":"OPEN"}"#, 0);
    stubs.respond(
        "gh",
        &["api", "repos/{owner}/{repo}/pulls/42/files"],
        files,
        0,
    );
    let mut command = stubs.orchestral();
    command
        .args(["route", "42", to, "--reason", "an internal refactor"])
        .current_dir(main)
        .env_remove("CLAUDE_CODE_SESSION_ID");
    if let Some(session) = session {
        command.env("CLAUDE_CODE_SESSION_ID", session);
    }
    (command.output().unwrap(), stubs)
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn the_orchestrator_records_a_route() {
    let (_dir, main) = repo(true);

    let (owner, _) = run(&main, "human", Some(SESSION), "src/app.rs\n");
    assert_eq!(owner.status.code(), Some(0), "{}", stderr(&owner));
    assert_eq!(
        store(&main).read().unwrap().routes[&42].to,
        MergeRoute::Human
    );

    let (supervisor, _) = run(&main, "supervisor", Some(SESSION), "src/app.rs\n");
    assert_eq!(supervisor.status.code(), Some(0), "{}", stderr(&supervisor));
    let routes = store(&main).read().unwrap().routes;
    assert_eq!(
        routes[&42],
        Route {
            to: MergeRoute::Supervisor,
            reason: "an internal refactor".into()
        }
    );
}

#[test]
fn only_the_orchestrator_routes() {
    let (_dir, main) = repo(true);

    for session in [None, Some("another-session")] {
        let (output, _) = run(&main, "human", session, "");
        assert_eq!(output.status.code(), Some(2));
        assert!(
            stderr(&output).contains("only the orchestrator routes PRs"),
            "{}",
            stderr(&output)
        );
    }
    assert!(store(&main).read().unwrap().routes.is_empty());
}

#[test]
fn a_closed_pr_is_refused() {
    let (_dir, main) = repo(true);
    let stubs = Stubs::new();
    stubs.respond("gh", &["pr", "view", "42"], r#"{"state":"MERGED"}"#, 0);

    let output = stubs
        .orchestral()
        .args(["route", "42", "human", "--reason", "x"])
        .current_dir(&main)
        .env("CLAUDE_CODE_SESSION_ID", SESSION)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("PR #42 is merged"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn the_supervisor_route_is_refused_when_the_owner_must_merge() {
    // The policy keeps merging to the owner.
    let (_dir, main) = repo(false);
    let (output, _) = run(&main, "supervisor", Some(SESSION), "src/app.rs\n");
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("supervisor_merges is off"));

    // It changes CI config.
    let (_dir, main) = repo(true);
    let (output, _) = run(
        &main,
        "supervisor",
        Some(SESSION),
        "src/app.rs\n.github/workflows/verify.yml\n",
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("changes .github/workflows/verify.yml"),
        "{}",
        stderr(&output)
    );

    // The owner was already asked to merge it.
    store(&main).update(|state| state.asked.insert(42)).unwrap();
    let (output, _) = run(&main, "supervisor", Some(SESSION), "src/app.rs\n");
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("already been asked"));
    assert!(store(&main).read().unwrap().routes.is_empty());
}

/// The poll's snapshot, with each PR as (number, draft, CI).
fn snapshot(main: &Path, prs: &[(u64, bool, &str)]) {
    let prs: serde_json::Map<String, Value> = prs
        .iter()
        .map(|&(number, draft, ci)| {
            (
                number.to_string(),
                json!({
                    "title": "A change", "draft": draft, "branch": format!("issue-{number}"),
                    "head": HEAD, "ci": ci, "reviews": []
                }),
            )
        })
        .collect();
    let snapshot = json!({
        "issues": {}, "prs": prs, "polled_at": "2026-11-02T12:00:00Z",
        "seen_comments": [], "failures": 0, "retry_after": null, "escalations": []
    });
    std::fs::write(
        main.join(".git/orchestral/github.json"),
        snapshot.to_string(),
    )
    .unwrap();
}

fn view(body: &str) -> String {
    json!({
        "state": "OPEN", "isDraft": false, "baseRefName": "main",
        "mergeable": "MERGEABLE", "headRefOid": HEAD, "body": body,
        "statusCheckRollup": [{"status": "COMPLETED", "conclusion": "SUCCESS"}]
    })
    .to_string()
}

fn passed(state: &mut orchestral::state::State, pr: u64) {
    state.reviews.insert(
        pr,
        Review {
            sha: HEAD.into(),
            passed: true,
            failed: Vec::new(),
        },
    );
}

fn edits(stubs: &Stubs) -> Vec<Vec<String>> {
    stubs
        .calls("gh")
        .into_iter()
        .filter(|call| call[..2] == ["pr", "edit"])
        .collect()
}

#[test]
fn the_owner_is_asked_once_when_only_their_approval_is_missing() {
    let (_dir, main) = repo(true);
    let store = store(&main);
    store
        .update(|state| {
            // 42 only needs the owner; 43 is a draft; 44's CI is running;
            // 45 has a pending finding; 46 is the supervisor's.
            for pr in 42..=46 {
                passed(state, pr);
            }
            state.routes.insert(
                46,
                Route {
                    to: MergeRoute::Supervisor,
                    reason: "a refactor".into(),
                },
            );
        })
        .unwrap();
    snapshot(
        &main,
        &[
            (42, false, "passed"),
            (43, true, "passed"),
            (44, false, "pending"),
            (45, false, "passed"),
            (46, false, "passed"),
        ],
    );
    let stubs = Stubs::new();
    stubs.respond(
        "gh",
        &["pr", "view", "42"],
        &view("## Findings\n\nNone\n"),
        0,
    );
    stubs.respond(
        "gh",
        &["pr", "view", "45"],
        &view("## Findings\n\n- [pending] A typo\n"),
        0,
    );
    stubs.respond("gh", &["label", "create"], "", 1);
    stubs.respond("gh", &["pr", "edit"], "", 0);
    let gh = stubs.dir().join("gh");

    let lines = route::ask_owner(&store, &gh).unwrap();

    assert_eq!(
        lines,
        [
            "pr #42: ready for the owner to merge, and labelled needs-discussion: nothing to do until they approve it"
        ]
    );
    assert_eq!(
        edits(&stubs),
        [["pr", "edit", "42", "--add-label", "needs-discussion"]]
    );
    assert_eq!(
        store
            .read()
            .unwrap()
            .asked
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        [42]
    );

    // Asked already, so not again.
    assert!(route::ask_owner(&store, &gh).unwrap().is_empty());
    assert_eq!(edits(&stubs).len(), 1);

    // Once it's no longer open, it's forgotten.
    snapshot(&main, &[(43, true, "passed")]);
    route::ask_owner(&store, &gh).unwrap();
    assert!(store.read().unwrap().asked.is_empty());
}
