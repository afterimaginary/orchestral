use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orchestral::outcome::Failure;
use orchestral::policy::Policy;
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git should run");
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

fn write_policy(checkout: &Path, workers: u32) {
    fs::create_dir_all(checkout.join(".orchestral")).unwrap();
    fs::write(
        checkout.join(".orchestral/policy.md"),
        format!("---\nworkers: {workers}\nfleet_token_budget: 1000\n---\nRules.\n"),
    )
    .unwrap();
}

fn workers(cwd: &Path) -> u32 {
    Policy::load(cwd).unwrap().settings.workers
}

/// A bare `origin` and a clone of it, with one commit on `main` locally.
fn repo() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let origin = dir.path().join("origin.git");
    let main = dir.path().join("main");
    git(
        dir.path(),
        &["init", "-q", "--bare", "-b", "main", "origin.git"],
    );
    git(
        dir.path(),
        &["clone", "-q", origin.to_str().unwrap(), "main"],
    );
    git(&main, &["commit", "-q", "--allow-empty", "-m", "start"]);
    (dir, main)
}

#[test]
fn before_the_first_push_the_main_checkout_is_read() {
    let (_dir, main) = repo();
    write_policy(&main, 1);
    fs::write(main.join("CONTRIBUTING.md"), "How to contribute.\n").unwrap();
    git(&main, &["worktree", "add", "-q", "../feature"]);
    let feature = main.join("../feature");
    write_policy(&feature, 5);

    assert_eq!(workers(&main), 1);
    assert_eq!(workers(&feature), 1);
    assert!(!Policy::load(&feature).unwrap().settings.supervisor_merges);
}

#[test]
fn after_a_push_origin_main_wins_over_every_local_copy() {
    let (_dir, main) = repo();
    write_policy(&main, 2);
    git(&main, &["add", "."]);
    git(&main, &["commit", "-q", "-m", "policy"]);
    git(&main, &["push", "-q", "origin", "main"]);
    git(
        &main,
        &["worktree", "add", "-q", "-b", "feature", "../feature"],
    );
    let feature = main.join("../feature");
    write_policy(&main, 9);
    write_policy(&feature, 7);
    git(&feature, &["commit", "-q", "-am", "raise my own limit"]);

    assert_eq!(workers(&main), 2);
    assert_eq!(workers(&feature), 2);
}

#[test]
fn a_pushed_contributing_file_turns_supervisor_merges_off() {
    let (_dir, main) = repo();
    write_policy(&main, 1);
    fs::write(main.join("CONTRIBUTING.md"), "How to contribute.\n").unwrap();
    git(&main, &["add", "."]);
    git(&main, &["commit", "-q", "-m", "policy"]);
    git(&main, &["push", "-q", "origin", "main"]);

    assert!(!Policy::load(&main).unwrap().settings.supervisor_merges);
}

#[test]
fn a_contributing_file_only_in_a_checkout_is_ignored_after_a_push() {
    let (_dir, main) = repo();
    write_policy(&main, 1);
    git(&main, &["add", "."]);
    git(&main, &["commit", "-q", "-m", "policy"]);
    git(&main, &["push", "-q", "origin", "main"]);
    fs::write(main.join("CONTRIBUTING.md"), "How to contribute.\n").unwrap();

    assert!(Policy::load(&main).unwrap().settings.supervisor_merges);
}

#[test]
fn origin_main_without_a_policy_is_an_error() {
    let (_dir, main) = repo();
    git(&main, &["push", "-q", "origin", "main"]);
    write_policy(&main, 1);

    let Err(Failure::Error(message)) = Policy::load(&main) else {
        panic!("expected an error");
    };
    assert_eq!(
        message,
        "there's no .orchestral/policy.md in origin/main; add one"
    );
}
