//! The worker guard and pre-push hooks, run as Claude Code and git run them.

mod common;

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use common::Stubs;
use serde_json::{Value, json};
use tempfile::TempDir;

/// Runs `hook worker-guard` on a `PreToolUse` event for `command`.
fn guard(command: &str, cwd: &Path) -> Output {
    let event = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
        "cwd": cwd,
    });
    let mut child = Stubs::new()
        .orchestral()
        .args(["hook", "worker-guard"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(event.to_string().as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn decision(output: &Output) -> Option<String> {
    assert_eq!(output.status.code(), Some(0));
    let printed = String::from_utf8_lossy(&output.stdout);
    if printed.trim().is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(&printed).unwrap();
    assert_eq!(value["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    value["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .map(String::from)
}

#[test]
fn the_guard_denies_each_command_a_worker_mustnt_run() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("comment.md"),
        "/hold until the owner looks\n",
    )
    .unwrap();

    for command in [
        "gh pr merge 12 --squash",
        "gh api -X PUT repos/owner/app/pulls/12/merge",
        "gh pr review 12 --approve",
        "git push --force origin HEAD:main",
        "gh pr comment 12 --body /approve",
        "gh issue comment 12 --body /hold",
        "gh pr review 12 --comment --body /approve",
        "gh pr comment 12 --body-file comment.md",
        "gh pr comment 12 -F comment.md",
        "gh api repos/owner/app/issues/12/comments -F body=@comment.md",
    ] {
        assert_eq!(
            decision(&guard(command, dir.path())).as_deref(),
            Some("deny"),
            "{command}"
        );
    }
}

#[test]
fn the_guard_allows_everyday_commands() {
    let dir = TempDir::new().unwrap();

    for command in [
        "cargo test",
        "git push -u origin issue-12",
        "gh pr create --draft --fill",
        "gh pr comment 12 --body 'Ready for review.'",
    ] {
        assert_eq!(decision(&guard(command, dir.path())), None, "{command}");
    }
}

fn git(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
        .args(args)
        .current_dir(dir)
        .env_remove("ORCHESTRAL_WORKER")
        .output()
        .unwrap()
}

/// A clone of a bare origin, with the pre-push hook calling this build.
fn repo_with_hook() -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    assert!(
        git(
            dir.path(),
            &["init", "-q", "--bare", "-b", "main", "origin.git"]
        )
        .status
        .success()
    );
    assert!(
        git(dir.path(), &["clone", "-q", "origin.git", "app"])
            .status
            .success()
    );
    let app = dir.path().join("app");
    assert!(
        git(&app, &["commit", "-q", "--allow-empty", "-m", "start"])
            .status
            .success()
    );
    let hook = app.join(".git/hooks/pre-push");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\nexec '{}' hook pre-push \"$@\"\n",
            env!("CARGO_BIN_EXE_orchestral")
        ),
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    (dir, app)
}

#[test]
fn pre_push_rejects_a_workers_push_to_main() {
    let (_dir, app) = repo_with_hook();

    let output = Command::new("git")
        .args(["push", "-q", "origin", "HEAD:main"])
        .current_dir(&app)
        .env("ORCHESTRAL_WORKER", "issue-12")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("workers can't push to main"));
}

#[test]
fn pre_push_allows_a_workers_branch() {
    let (_dir, app) = repo_with_hook();

    let output = Command::new("git")
        .args(["push", "-q", "origin", "HEAD:issue-12"])
        .current_dir(&app)
        .env("ORCHESTRAL_WORKER", "issue-12")
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
}

#[test]
fn pre_push_allows_the_owners_push_to_main() {
    let (_dir, app) = repo_with_hook();

    let output = git(&app, &["push", "-q", "origin", "HEAD:main"]);

    assert!(output.status.success(), "{output:?}");
}
