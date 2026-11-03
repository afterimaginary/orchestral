//! `orchestral try --check`, booting a real server in a worker's worktree.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::Stubs;
use tempfile::TempDir;

/// A repository whose policy previews with `run`, and `setup` if given.
fn repo(run: &str, setup: Option<&str>) -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let main = dir.path().canonicalize().unwrap();
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&main)
        .status()
        .unwrap();
    assert!(status.success());
    let setup = setup
        .map(|setup| format!("  setup: {setup}\n"))
        .unwrap_or_default();
    std::fs::create_dir(main.join(".orchestral")).unwrap();
    std::fs::write(
        main.join(".orchestral/policy.md"),
        format!("---\nfleet_token_budget: 1000\npreview:\n{setup}  run: {run}\n---\n"),
    )
    .unwrap();
    (dir, main)
}

fn check(main: &Path, worker: Option<&str>) -> Output {
    let stubs = Stubs::new();
    let mut command = stubs.orchestral();
    command
        .args(["try", "--check"])
        .current_dir(main)
        .env_remove("ORCHESTRAL_WORKER");
    if let Some(worker) = worker {
        command.env("ORCHESTRAL_WORKER", worker);
    }
    command.output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn a_preview_that_answers_passes_and_is_stopped() {
    // The server records its pid, so the test can see it stopped.
    let (_dir, main) = repo(
        "echo $$ > server.pid; exec python3 -m http.server $PORT --bind 127.0.0.1",
        Some("touch set-up"),
    );

    let output = check(&main, Some("issue-12"));

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(
        text(&output.stdout).starts_with("The preview booted: it answered 200 after "),
        "{}",
        text(&output.stdout)
    );
    assert!(main.join("set-up").exists());
    let pid = std::fs::read_to_string(main.join("server.pid")).unwrap();
    let alive = Command::new("kill")
        .args(["-0", pid.trim()])
        .status()
        .unwrap()
        .success();
    assert!(!alive, "the server is still running");
}

#[test]
fn a_preview_that_exits_fails_with_its_log() {
    let (_dir, main) = repo("echo missing module; exit 3", None);

    let output = check(&main, Some("issue-12"));

    assert_eq!(output.status.code(), Some(2));
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("the preview didn't boot: it exited with"),
        "{stderr}"
    );
    assert!(stderr.contains("missing module"), "{stderr}");
}

#[test]
fn a_failing_setup_fails_the_check() {
    let (_dir, main) = repo("true", Some("echo no lockfile; exit 1"));

    let output = check(&main, Some("issue-12"));

    assert_eq!(output.status.code(), Some(2));
    let stderr = text(&output.stderr);
    assert!(stderr.contains("setup failed"), "{stderr}");
    assert!(stderr.contains("no lockfile"), "{stderr}");
}

#[test]
fn only_a_worker_runs_it() {
    let (_dir, main) = repo("true", None);

    let output = check(&main, None);

    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("only a worker runs the boot check"));
}

#[test]
fn the_policy_comes_from_origin_main_not_the_branch() {
    // origin/main's policy runs a server; the branch's own copy exits.
    let (_dir, main) = repo("exec python3 -m http.server $PORT --bind 127.0.0.1", None);
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(&main)
            .status()
            .unwrap();
        assert!(status.success());
    };
    git(&["add", "-A"]);
    git(&["commit", "-qm", "policy"]);
    git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    std::fs::write(
        main.join(".orchestral/policy.md"),
        "---\nfleet_token_budget: 1000\npreview:\n  run: exit 7\n---\n",
    )
    .unwrap();

    let output = check(&main, Some("issue-12"));

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
}
