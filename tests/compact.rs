//! The compaction hooks, with stub `claude` and `gh`.

mod common;

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use common::Stubs;
use orchestral::state::{Orchestrator, Store};
use serde_json::{Value, json};
use tempfile::TempDir;

const SESSION: &str = "0b5d3c1e-6a2f-4c8e-9d7b-1f2e3a4b5c6d";

struct Repo {
    _dir: TempDir,
    main: PathBuf,
    stubs: Stubs,
}

impl Repo {
    /// A repository whose claim `SESSION` holds.
    fn new() -> Self {
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
        Store::for_repo(&main)
            .unwrap()
            .update(|state| {
                state.orchestrator = Some(Orchestrator {
                    session_id: SESSION.into(),
                    claimed_at: "2026-11-02T10:00:00Z".into(),
                })
            })
            .unwrap();
        Repo {
            _dir: dir,
            main,
            stubs: Stubs::new(),
        }
    }

    fn hook(&self, name: &str, session: &str) -> Output {
        self.hook_input(
            name,
            &json!({"session_id": session, "cwd": self.main}).to_string(),
        )
    }

    fn hook_input(&self, name: &str, input: &str) -> Output {
        let mut child = self
            .stubs
            .orchestral()
            .args(["hook", name])
            .env("CLAUDE_PLUGIN_ROOT", "/plugins/orchestral")
            .current_dir(&self.main)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn snapshot(&self) -> PathBuf {
        self.main
            .join(".git/orchestral/orchestrator/before-compact.txt")
    }
}

#[test]
fn the_orchestrator_gets_its_instructions_and_status_back() {
    let repo = Repo::new();

    let before = repo.hook("pre-compact", SESSION);
    assert_eq!(before.status.code(), Some(0));
    assert!(repo.snapshot().exists());
    let after = repo.hook("session-compact", SESSION);

    assert_eq!(after.status.code(), Some(0));
    let output: Value = serde_json::from_slice(&after.stdout).unwrap();
    assert_eq!(
        output["hookSpecificOutput"]["hookEventName"],
        "SessionStart"
    );
    let context = output["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.contains("/plugins/orchestral/skills/orchestrate/SKILL.md"));
    assert!(context.contains("/plugins/orchestral/bin/orchestral"));
    assert!(context.contains("Before compaction, it was:\n\nFleet: not paused"));
    assert!(context.contains("The fleet is now:\n\nFleet: not paused"));
    // Used once, so a later compaction doesn't show a stale status.
    assert!(!repo.snapshot().exists());
}

#[test]
fn any_other_session_is_left_alone() {
    let repo = Repo::new();

    let before = repo.hook("pre-compact", "another-session");
    let after = repo.hook("session-compact", "another-session");

    assert_eq!(before.status.code(), Some(0));
    assert_eq!(after.status.code(), Some(0));
    assert!(after.stdout.is_empty());
    assert!(!repo.snapshot().exists());
}

#[test]
fn bad_input_never_fails_the_hook() {
    let repo = Repo::new();

    for name in ["pre-compact", "session-compact"] {
        let output = repo.hook_input(name, "not json");
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stdout.is_empty());
    }
}
