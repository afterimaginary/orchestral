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

fn write(path: &Path, workers: u32) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        format!("---\nworkers: {workers}\nfleet_token_budget: 1000\n---\nRules {workers}.\n"),
    )
    .unwrap();
}

/// A bare `origin`, a clone of it with one commit on `main` locally, and an
/// empty config directory.
fn repo() -> (TempDir, PathBuf, PathBuf) {
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
    let config = dir.path().join("config/orchestral");
    (dir, main, config)
}

fn load(config: &Path, cwd: &Path) -> Policy {
    Policy::load_from(config, cwd).unwrap()
}

#[test]
fn the_global_policy_applies_to_every_checkout_and_tracked_copies_are_ignored() {
    let (_dir, main, config) = repo();
    write(&config.join("policy.md"), 2);
    write(&main.join(".orchestral/policy.md"), 9);
    git(&main, &["add", "."]);
    git(&main, &["commit", "-q", "-m", "policy"]);
    git(&main, &["push", "-q", "origin", "main"]);
    git(
        &main,
        &["worktree", "add", "-q", "-b", "feature", "../feature"],
    );
    let feature = main.join("../feature");
    write(&feature.join(".orchestral/policy.md"), 7);

    assert_eq!(load(&config, &main).settings.workers, 2);
    assert_eq!(load(&config, &feature).settings.workers, 2);
    assert_eq!(load(&config, &feature).sources, [config.join("policy.md")]);
}

#[test]
fn a_github_origin_adds_its_repository_layer() {
    let (_dir, main, config) = repo();
    git(
        &main,
        &["remote", "set-url", "origin", "git@github.com:Acme/App.git"],
    );
    write(&config.join("policy.md"), 2);
    write(&config.join("acme/app/policy.md"), 4);
    write(&config.join("acme/other/policy.md"), 6);

    let policy = load(&config, &main);

    assert_eq!(policy.settings.workers, 4);
    assert_eq!(policy.rules, "Rules 2.\n\nRules 4.");
    assert_eq!(
        policy.sources,
        [config.join("policy.md"), config.join("acme/app/policy.md")]
    );
}

#[test]
fn a_contributing_file_turns_supervisor_merges_off_before_the_first_push() {
    let (_dir, main, config) = repo();
    write(&config.join("policy.md"), 1);
    fs::write(main.join("CONTRIBUTING.md"), "How to contribute.\n").unwrap();

    assert!(!load(&config, &main).settings.supervisor_merges);
}

#[test]
fn a_pushed_contributing_file_turns_supervisor_merges_off() {
    let (_dir, main, config) = repo();
    write(&config.join("policy.md"), 1);
    fs::write(main.join("CONTRIBUTING.md"), "How to contribute.\n").unwrap();
    git(&main, &["add", "."]);
    git(&main, &["commit", "-q", "-m", "contributing"]);
    git(&main, &["push", "-q", "origin", "main"]);

    assert!(!load(&config, &main).settings.supervisor_merges);
}

#[test]
fn a_contributing_file_only_in_a_checkout_is_ignored_after_a_push() {
    let (_dir, main, config) = repo();
    write(&config.join("policy.md"), 1);
    git(&main, &["push", "-q", "origin", "main"]);
    fs::write(main.join("CONTRIBUTING.md"), "How to contribute.\n").unwrap();

    assert!(load(&config, &main).settings.supervisor_merges);
}

#[test]
fn no_policy_is_an_error_naming_where_to_create_one() {
    let (_dir, main, config) = repo();

    let Err(Failure::Error(message)) = Policy::load_from(&config, &main) else {
        panic!("expected an error");
    };
    assert_eq!(
        message,
        format!(
            "there's no policy; run `orchestral init` to create {}",
            config.join("policy.md").display()
        )
    );
}
