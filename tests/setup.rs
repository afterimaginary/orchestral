//! `orchestral init` and `orchestral doctor`, with stub tools and a local
//! origin.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::Stubs;
use serde_json::{Value, json};
use tempfile::TempDir;

const VERSION: &str = "2.1.314 (Claude Code)\n";

fn git(dir: &Path, args: &[&str]) -> Output {
    let output = Command::new("git")
        .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
        .args(args)
        .current_dir(dir)
        .env_remove("ORCHESTRAL_WORKER")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    output
}

struct Repo {
    dir: TempDir,
    main: PathBuf,
    stubs: Stubs,
}

impl Repo {
    /// A clone of a bare origin, trusted by Claude Code, with every tool
    /// working.
    fn new() -> Self {
        Repo::with(VERSION, true, true)
    }

    /// Claude Code at `version`, both tools logged in or not, and portless
    /// and Tailscale present or missing.
    fn with(version: &str, logged_in: bool, preview_tools: bool) -> Self {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        git(&root, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
        git(&root, &["clone", "-q", "origin.git", "app"]);
        let main = root.join("app");
        git(&main, &["commit", "-q", "--allow-empty", "-m", "start"]);
        fs::create_dir(root.join("config")).unwrap();
        let repo = Repo {
            dir,
            main,
            stubs: Stubs::new(),
        };
        repo.trust(true);
        let stubs = &repo.stubs;
        stubs.respond("claude", &["--version"], version, 0);
        if logged_in {
            stubs.respond("claude", &["auth", "status"], r#"{"loggedIn": true}"#, 0);
            stubs.respond("gh", &["auth", "status"], "", 0);
        } else {
            stubs.respond("claude", &["auth", "status"], r#"{"loggedIn": false}"#, 1);
            stubs.respond("gh", &["auth", "status"], "", 1);
        }
        if preview_tools {
            stubs.respond("portless", &["--version"], "0.15.6\n", 0);
            stubs.respond(
                "tailscale",
                &["status", "--json"],
                r#"{"BackendState": "Running"}"#,
                0,
            );
        }
        repo
    }

    fn trust(&self, trusted: bool) {
        let config = json!({"projects": {
            self.main.display().to_string(): {"hasTrustDialogAccepted": trusted}
        }});
        fs::write(self.config().join(".claude.json"), config.to_string()).unwrap();
    }

    fn config(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap().join("config")
    }

    fn run(&self, args: &[&str]) -> Output {
        self.stubs
            .orchestral()
            .args(args)
            .current_dir(&self.main)
            .env("CLAUDE_CONFIG_DIR", self.config())
            .output()
            .unwrap()
    }

    /// Pushes a valid policy to origin.
    fn push_policy(&self) {
        fs::create_dir_all(self.main.join(".orchestral")).unwrap();
        fs::write(
            self.main.join(".orchestral/policy.md"),
            "---\nfleet_token_budget: 1000\n---\nRules.\n",
        )
        .unwrap();
        git(&self.main, &["add", "."]);
        git(&self.main, &["commit", "-q", "-m", "policy"]);
        git(&self.main, &["push", "-q", "origin", "main"]);
    }

    /// Each check's status from `doctor --json`, by name.
    fn doctor(&self) -> (Option<i32>, Value) {
        let output = self.run(&["doctor", "--json"]);
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        let statuses = report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| (c["name"].as_str().unwrap().to_owned(), c["status"].clone()))
            .collect();
        (output.status.code(), Value::Object(statuses))
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn doctor_passes_a_ready_project() {
    let repo = Repo::new();
    repo.push_policy();

    let (code, statuses) = repo.doctor();

    assert_eq!(code, Some(0));
    assert_eq!(
        statuses,
        json!({"claude": "ok", "auth": "ok", "gh": "ok", "git": "ok", "trust": "ok",
               "policy": "ok", "portless": "ok", "tailscale": "ok"})
    );
}

#[test]
fn doctor_warns_on_an_untested_claude_code() {
    let repo = Repo::with("2.2.0 (Claude Code)\n", true, true);
    repo.push_policy();

    let (code, statuses) = repo.doctor();

    assert_eq!(code, Some(0));
    assert_eq!(statuses["claude"], "warn");
}

#[test]
fn missing_portless_and_tailscale_only_warn() {
    let repo = Repo::with(VERSION, true, false);
    repo.push_policy();

    let (code, statuses) = repo.doctor();

    assert_eq!(code, Some(0));
    assert_eq!(statuses["portless"], "warn");
    assert_eq!(statuses["tailscale"], "warn");
}

#[test]
fn doctor_fails_an_untrusted_repository() {
    let repo = Repo::new();
    repo.push_policy();
    repo.trust(false);

    let (code, statuses) = repo.doctor();

    assert_eq!(code, Some(1));
    assert_eq!(statuses["trust"], "fail");
}

#[test]
fn doctor_fails_when_logged_out() {
    let repo = Repo::with(VERSION, false, true);
    repo.push_policy();

    let (code, statuses) = repo.doctor();

    assert_eq!(code, Some(1));
    assert_eq!(statuses["auth"], "fail");
    assert_eq!(statuses["gh"], "fail");
}

#[test]
fn doctor_warns_when_the_local_policy_differs() {
    let repo = Repo::new();
    repo.push_policy();
    fs::write(
        repo.main.join(".orchestral/policy.md"),
        "---\nfleet_token_budget: 2000\n---\nRules.\n",
    )
    .unwrap();

    let (code, statuses) = repo.doctor();

    assert_eq!(code, Some(0));
    assert_eq!(statuses["policy"], "warn");
}

#[test]
fn init_creates_the_policy_and_installs_the_hook() {
    let repo = Repo::new();

    let output = repo.run(&["init"]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let policy = fs::read_to_string(repo.main.join(".orchestral/policy.md")).unwrap();
    assert!(policy.contains("fleet_token_budget: 3000000"));
    let hook = fs::read_to_string(repo.main.join(".git/hooks/pre-push")).unwrap();
    assert!(hook.starts_with("#!/bin/sh\n# >>> orchestral >>>\n"));
}

#[test]
fn init_keeps_an_existing_policy_and_runs_twice_cleanly() {
    let repo = Repo::new();
    repo.push_policy();

    repo.run(&["init"]);
    let output = repo.run(&["init"]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(String::from_utf8_lossy(&output.stdout).contains("Kept the existing"));
    let hook = fs::read_to_string(repo.main.join(".git/hooks/pre-push")).unwrap();
    assert_eq!(hook.matches("# >>> orchestral >>>").count(), 1);
}

#[test]
fn init_respects_hooks_path_and_keeps_an_existing_hook_working() {
    let repo = Repo::new();
    git(&repo.main, &["config", "core.hooksPath", ".githooks"]);
    fs::create_dir(repo.main.join(".githooks")).unwrap();
    // An existing hook that reads the refs git passes on stdin.
    let existing = "#!/bin/sh\ncat > \"$(git rev-parse --git-dir)/refs-seen\"\n";
    let hook = repo.main.join(".githooks/pre-push");
    fs::write(&hook, existing).unwrap();

    let output = repo.run(&["init"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = fs::read_to_string(&hook).unwrap();
    assert!(text.contains("# >>> orchestral >>>"));
    assert!(text.ends_with("cat > \"$(git rev-parse --git-dir)/refs-seen\"\n"));

    // A worker's push to main is rejected.
    let worker = Command::new("git")
        .args(["push", "-q", "origin", "HEAD:main"])
        .current_dir(&repo.main)
        .env("ORCHESTRAL_WORKER", "issue-12")
        .output()
        .unwrap();
    assert!(!worker.status.success());
    assert!(stderr(&worker).contains("workers can't push to main"));

    // The owner's push goes through, and the existing hook still sees the refs.
    git(&repo.main, &["push", "-q", "origin", "HEAD:main"]);
    let seen = fs::read_to_string(repo.main.join(".git/refs-seen")).unwrap();
    assert!(seen.contains("refs/heads/main"), "{seen:?}");
}

#[test]
fn init_refuses_an_untrusted_repository() {
    let repo = Repo::new();
    repo.trust(false);

    let output = repo.run(&["init"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("doesn't trust"));
    assert!(!repo.main.join(".orchestral/policy.md").exists());
}
