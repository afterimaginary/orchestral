//! `orchestral status` against a local origin, with stub `claude` and `gh`
//! and fixture transcripts.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::{Stubs, fixture};
use orchestral::state::{Store, Worker};
use orchestral::transcript::project_dir_name;
use serde_json::{Value, json};
use tempfile::TempDir;

const SESSION: &str = "00000000-0000-4000-8000-00000000a001";

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git should run");
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

/// A repository with a policy on `origin/main`, two recorded workers, and a
/// Claude config directory holding the first worker's transcript.
struct Fleet {
    _dir: TempDir,
    main: PathBuf,
    config: PathBuf,
    stubs: Stubs,
}

impl Fleet {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        git(&root, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
        git(&root, &["clone", "-q", "origin.git", "app"]);
        let main = root.join("app");
        fs::create_dir(main.join(".orchestral")).unwrap();
        fs::write(
            main.join(".orchestral/policy.md"),
            "---\nfleet_token_budget: 1000\nworkers: 3\n---\nRules.\n",
        )
        .unwrap();
        git(&main, &["add", "."]);
        git(&main, &["commit", "-q", "-m", "policy"]);
        git(&main, &["push", "-q", "origin", "main"]);

        let config = root.join("config");
        let worktree = main.join(".claude/worktrees/issue-12");
        let projects = config.join("projects").join(project_dir_name(&worktree));
        fs::create_dir_all(&projects).unwrap();
        // A Max account, whose Opus sessions get a 1M window.
        fs::write(
            config.join(".claude.json"),
            r#"{"oauthAccount":{"organizationType":"claude_max"}}"#,
        )
        .unwrap();
        // Moved into the past, so the cache is cold whenever the test runs.
        let transcript = fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/claude/2.1.314/transcripts/basic.jsonl"),
        )
        .unwrap()
        .replace("2026-11-02T", "2026-01-02T");
        fs::write(projects.join(format!("{SESSION}.jsonl")), transcript).unwrap();

        let worker = |name: &str, issue: u64, job: Option<&str>| Worker {
            session_name: format!("app-{name}"),
            issue,
            model: "claude-opus-5-5".into(),
            effort: "high".into(),
            worktree: main.join(".claude/worktrees").join(name),
            branch: name.into(),
            job_id: job.map(String::from),
            review_of: None,
            created_at: "2026-11-02T00:00:00Z".into(),
        };
        Store::for_repo(&main)
            .unwrap()
            .update(|state| {
                state.paused = true;
                state
                    .workers
                    .insert("issue-12".into(), worker("issue-12", 12, Some("a001")));
                state
                    .workers
                    .insert("issue-13".into(), worker("issue-13", 13, None));
            })
            .unwrap();

        Fleet {
            _dir: dir,
            main,
            config,
            stubs: Stubs::new(),
        }
    }

    fn start_run(&self) {
        Store::for_repo(&self.main)
            .unwrap()
            .update(|state| state.run_started_at = Some("2026-01-01T00:00:00Z".into()))
            .unwrap();
    }

    fn reads_everything(&self) {
        let worktree = self.main.join(".claude/worktrees/issue-12");
        self.stubs.respond(
            "claude",
            &["agents"],
            &json!([{
                "id": "a001", "pid": 41001, "sessionId": SESSION, "cwd": worktree,
                "kind": "background", "state": "done"
            }])
            .to_string(),
            0,
        );
        self.stubs.respond(
            "claude",
            &["-p", "/usage"],
            &fixture("claude/2.1.314/usage.txt"),
            0,
        );
        self.stubs.respond(
            "gh",
            &["pr", "list"],
            r#"[{"number":40,"headRefName":"issue-12","isDraft":true},{"number":41,"headRefName":"other","isDraft":false}]"#,
            0,
        );
    }

    fn status(&self, args: &[&str]) -> Output {
        self.stubs
            .orchestral()
            .arg("status")
            .args(args)
            .current_dir(&self.main)
            .env("CLAUDE_CONFIG_DIR", &self.config)
            .output()
            .unwrap()
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn shows_workers_limits_budget_and_pause() {
    let fleet = Fleet::new();
    fleet.reads_everything();

    let output = fleet.status(&[]);

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let text = stdout(&output);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "Fleet: paused");
    assert_eq!(lines[1], "Budget: no run has started; 1000 tokens per run");
    assert_eq!(lines[2], "Plan limits:");
    assert!(
        lines[3].starts_with("  session: 2% used, resets Nov 2 at 6:49am"),
        "{text}"
    );
    assert_eq!(lines[6], "Workers: 2 of 3");
    // basic.jsonl's last response: 20,505 tokens, a 1-hour cache gone cold,
    // so waking rewrites it at 2 per token.
    assert_eq!(
        lines[7],
        "  issue-12 · issue #12 · PR #40 (draft) · running, done · context 20505 tokens (2%) · cache cold · wake about 41010 tokens (estimate)"
    );
    assert_eq!(lines[8], "  issue-13 · issue #13 · unknown");
    assert_eq!(lines.len(), 9, "{text}");
}

#[test]
fn json_carries_the_same_data() {
    let fleet = Fleet::new();
    fleet.reads_everything();

    let output = fleet.status(&["--json"]);

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let status: Value = serde_json::from_str(stdout(&output).trim_end()).unwrap();
    assert_eq!(status["paused"], true);
    assert_eq!(status["worker_cap"], 3);
    assert_eq!(status["budget"]["fleet_token_budget"], 1000);
    assert_eq!(status["budget"]["run_started_at"], Value::Null);
    assert_eq!(status["limits"]["windows"][1]["percent"], 26);
    let worker = &status["workers"][0];
    assert_eq!(worker["name"], "issue-12");
    assert_eq!(worker["pr"], 40);
    assert_eq!(worker["draft"], true);
    assert_eq!(worker["session"], "running");
    assert_eq!(worker["activity"], "done");
    assert_eq!(worker["context"]["tokens"], 20_505);
    assert_eq!(worker["context"]["warm"], false);
    assert_eq!(worker["context"]["window"], 1_000_000);
    assert_eq!(worker["wake_cost"]["estimated_tokens"], 41_010);
    assert_eq!(status["workers"][1]["session"], "unknown");
    assert_eq!(status["workers"][1]["context"], Value::Null);
    assert_eq!(status["warnings"], json!([]));
}

#[test]
fn a_waiting_worker_shows_what_it_waits_for() {
    let fleet = Fleet::new();
    fleet.stubs.respond(
        "claude",
        &["agents"],
        &json!([{"id": "a001", "pid": 41001, "sessionId": SESSION, "state": "blocked",
                 "waitingFor": "input needed"}])
        .to_string(),
        0,
    );

    let output = fleet.status(&["--json"]);

    let status: Value = serde_json::from_str(stdout(&output).trim_end()).unwrap();
    assert_eq!(status["workers"][0]["activity"], "waiting");
    assert_eq!(status["workers"][0]["waiting_for"], "input needed");
}

#[test]
fn a_lost_worker_shows_as_lost_even_while_its_session_runs() {
    let fleet = Fleet::new();
    fleet.reads_everything();
    Store::for_repo(&fleet.main)
        .unwrap()
        .update(|state| {
            state
                .observed
                .insert("issue-12".into(), orchestral::observe::Observed::Lost)
        })
        .unwrap();

    let output = fleet.status(&["--json"]);

    let status: Value = serde_json::from_str(stdout(&output).trim_end()).unwrap();
    assert_eq!(status["workers"][0]["session"], "lost");
}

#[test]
fn shows_the_verdict_on_each_open_pr() {
    let fleet = Fleet::new();
    fleet.reads_everything();
    let review = |sha: &str| orchestral::state::Review {
        sha: sha.into(),
        passed: true,
        failed: Vec::new(),
    };
    Store::for_repo(&fleet.main)
        .unwrap()
        .update(|state| {
            state.reviews.insert(41, review("4f2c9e1a0b"));
            // Merged since, so no longer open.
            state.reviews.insert(39, review("1111111"));
        })
        .unwrap();

    let text = stdout(&fleet.status(&[]));
    let json: Value = serde_json::from_str(stdout(&fleet.status(&["--json"])).trim_end()).unwrap();

    assert!(
        text.contains("\nReviews:\n  PR #41 passed at 4f2c9e1"),
        "{text}"
    );
    assert_eq!(
        json["reviews"],
        json!([{"pr": 41, "sha": "4f2c9e1a0b", "passed": true}])
    );
}

#[test]
fn parts_that_cant_be_read_are_warnings() {
    let fleet = Fleet::new();
    // No stub replies: `claude` and `gh` both fail.

    let output = fleet.status(&[]);

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let text = stdout(&output);
    assert!(text.contains("Plan limits are unavailable"), "{text}");
    assert!(
        text.contains("  issue-12 · issue #12 · unknown\n"),
        "{text}"
    );
    assert!(text.contains("warning: sessions are unknown"), "{text}");
    assert!(text.contains("warning: PRs are unknown"), "{text}");
}

#[test]
fn a_started_run_shows_its_usage() {
    let fleet = Fleet::new();
    fleet.start_run();
    fleet.reads_everything();

    let text = stdout(&fleet.status(&[]));
    let json: Value = serde_json::from_str(stdout(&fleet.status(&["--json"])).trim_end()).unwrap();

    let budget = text.lines().nth(1).unwrap();
    assert!(budget.starts_with("Budget: about "), "{text}");
    assert!(
        budget.ends_with(" tokens (estimate) of 1000 used this run"),
        "{text}"
    );
    assert!(json["budget"]["used"]["estimated_tokens"].as_u64().unwrap() > 0);
    // The worker rows and the budget come from one `claude agents` call.
    let agents_calls = fleet
        .stubs
        .calls("claude")
        .iter()
        .filter(|call| call[0] == "agents")
        .count();
    assert_eq!(agents_calls, 2, "one per status run");
}

#[test]
fn usage_is_unknown_when_the_sessions_are() {
    let fleet = Fleet::new();
    fleet.start_run();

    let text = stdout(&fleet.status(&[]));

    assert_eq!(
        text.lines().nth(1),
        Some("Budget: usage unknown, of 1000"),
        "{text}"
    );
    assert_eq!(
        text.matches("warning: sessions are unknown").count(),
        1,
        "{text}"
    );
}

#[test]
fn an_unreadable_transcript_is_a_warning() {
    let fleet = Fleet::new();
    fleet.reads_everything();
    let worktree = fleet.main.join(".claude/worktrees/issue-12");
    let path = fleet
        .config
        .join("projects")
        .join(project_dir_name(&worktree))
        .join(format!("{SESSION}.jsonl"));
    fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();

    let text = stdout(&fleet.status(&[]));

    assert!(
        text.contains("warning: issue-12's context is unknown"),
        "{text}"
    );
}
