//! `orchestral spawn` against a local origin, with a stub `claude`.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::Stubs;
use orchestral::sessions;
use orchestral::state::Store;
use serde_json::{Value, json};
use tempfile::TempDir;

const ORCHESTRATOR: &str = "00000000-0000-4000-8000-0000000000c1";

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git should run");
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

/// A bare origin whose `main` has a policy, a clone of it named `app`, and a
/// Claude config directory.
struct Repo {
    dir: TempDir,
    main: PathBuf,
    stubs: Stubs,
}

impl Repo {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        git(&root, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
        git(&root, &["clone", "-q", "origin.git", "app"]);
        let main = root.join("app");
        fs::create_dir(main.join(".orchestral")).unwrap();
        fs::write(
            main.join(".orchestral/policy.md"),
            "---\nfleet_token_budget: 1000\ndefault_model: claude-sonnet-5-5\ndefault_effort: medium\n---\nRules.\n",
        )
        .unwrap();
        git(&main, &["add", "."]);
        git(&main, &["commit", "-q", "-m", "policy"]);
        git(&main, &["push", "-q", "origin", "main"]);
        fs::create_dir(root.join("config")).unwrap();

        let stubs = Stubs::new();
        stubs.respond(
            "claude",
            &["plugin", "list", "--json"],
            r#"[{"id":"orchestral@local","enabled":true},{"id":"other@market","enabled":true}]"#,
            0,
        );
        Repo { dir, main, stubs }
    }

    fn config(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap().join("config")
    }

    fn launches(&self, stdout: &str, code: i32) {
        self.stubs.respond("claude", &["--bg"], stdout, code);
    }

    fn spawn(&self, args: &[&str]) -> Output {
        self.stubs
            .orchestral()
            .arg("spawn")
            .args(args)
            .current_dir(&self.main)
            .env("CLAUDE_CONFIG_DIR", self.config())
            .env("CLAUDE_CODE_SESSION_ID", ORCHESTRATOR)
            .output()
            .unwrap()
    }

    fn store(&self) -> Store {
        Store::for_repo(&self.main).unwrap()
    }

    fn worker_file(&self, name: &str, file: &str) -> String {
        fs::read_to_string(self.store().dir().join("workers").join(name).join(file)).unwrap()
    }

    fn bg_calls(&self) -> Vec<Vec<String>> {
        self.stubs
            .calls("claude")
            .into_iter()
            .filter(|call| call.first().map(String::as_str) == Some("--bg"))
            .collect()
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

const STARTED: &str = "backgrounded · 558b988a · app-issue-12\n  claude agents   list sessions\n";

#[test]
fn starts_a_worker_in_its_own_worktree() {
    let repo = Repo::new();
    repo.launches(STARTED, 0);

    let output = repo.spawn(&["issue-12", "--issue", "12", "--prompt", "Implement #12."]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let worktree = repo.main.join(".claude/worktrees/issue-12");
    assert!(stdout(&output).starts_with("Started issue-12 as app-issue-12 (job 558b988a)"));
    assert!(worktree.join(".orchestral/policy.md").exists());

    let settings_path = repo.store().dir().join("workers/issue-12/settings.json");
    let prompt_path = repo.store().dir().join("workers/issue-12/prompt.md");
    let expected: Vec<String> = [
        "--bg",
        "--name",
        "app-issue-12",
        "--remote-control",
        "app-issue-12",
        "--settings",
        settings_path.to_str().unwrap(),
        "--append-system-prompt-file",
        prompt_path.to_str().unwrap(),
        "--permission-mode",
        "auto",
        "--model",
        "claude-sonnet-5-5",
        "--effort",
        "medium",
        "--",
        "Implement #12.",
    ]
    .map(String::from)
    .to_vec();
    assert_eq!(repo.bg_calls(), vec![expected]);

    let state = repo.store().read().unwrap();
    let worker = &state.workers["issue-12"];
    assert_eq!(worker.job_id.as_deref(), Some("558b988a"));
    assert_eq!(worker.issue, 12);
    assert_eq!(worker.branch, "issue-12");
    assert_eq!(worker.worktree, worktree);

    let exclude = fs::read_to_string(repo.main.join(".git/info/exclude")).unwrap();
    assert!(exclude.lines().any(|line| line == "/.claude/worktrees/"));
}

#[test]
fn the_worker_gets_its_own_settings_and_prompt() {
    let repo = Repo::new();
    repo.launches(STARTED, 0);

    repo.spawn(&["issue-12", "--issue", "12", "--prompt", "Go."]);

    let settings: Value =
        serde_json::from_str(&repo.worker_file("issue-12", "settings.json")).unwrap();
    assert_eq!(settings["crossSessionInbound"], "accept");
    assert_eq!(settings["worktree"]["bgIsolation"], "none");
    assert_eq!(
        settings["enabledPlugins"],
        json!({"orchestral@local": false})
    );
    assert_eq!(settings["env"]["ORCHESTRAL_WORKER"], "issue-12");
    assert_eq!(
        settings["env"]["ORCHESTRAL_REPO"],
        repo.main.to_str().unwrap()
    );
    assert!(
        settings["env"]["PATH"]
            .as_str()
            .unwrap()
            .contains(repo.stubs.dir().to_str().unwrap())
    );
    let hook = settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(
        hook.starts_with("'/") && hook.ends_with("' hook worker-guard"),
        "{hook}"
    );

    let prompt = repo.worker_file("issue-12", "prompt.md");
    // The worker finds its orchestrator's address with this binary.
    let resolve = format!(
        "`'{}' resolve orchestrator`",
        env!("CARGO_BIN_EXE_orchestral")
    );
    assert!(prompt.contains(&resolve), "{prompt}");
}

#[test]
fn model_and_effort_override_the_policy() {
    let repo = Repo::new();
    repo.launches(STARTED, 0);

    repo.spawn(&[
        "issue-12",
        "--issue",
        "12",
        "--model",
        "claude-opus-5-5",
        "--effort",
        "xhigh",
        "--prompt",
        "Go.",
    ]);

    let call = &repo.bg_calls()[0];
    let after = |flag: &str| &call[call.iter().position(|arg| arg == flag).unwrap() + 1];
    assert_eq!(after("--model"), "claude-opus-5-5");
    assert_eq!(after("--effort"), "xhigh");
}

#[test]
fn a_worker_name_in_use_is_refused() {
    let repo = Repo::new();
    repo.launches(STARTED, 0);
    repo.spawn(&["issue-12", "--issue", "12", "--prompt", "Go."]);

    let output = repo.spawn(&["issue-12", "--issue", "13", "--prompt", "Go."]);

    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("a worker named issue-12 already exists"));
    assert_eq!(repo.bg_calls().len(), 1);
}

#[test]
fn a_live_session_with_the_name_is_refused() {
    let repo = Repo::new();
    // This test's own process stands in for the live session.
    let pid = std::process::id();
    let started = sessions::process(pid).unwrap().unwrap().started;
    let records = repo.config().join("sessions");
    fs::create_dir(&records).unwrap();
    fs::write(
        records.join(format!("{pid}.json")),
        json!({"pid": pid, "sessionId": "s", "cwd": "/", "name": "app-issue-12", "procStart": started}).to_string(),
    )
    .unwrap();

    let output = repo.spawn(&["issue-12", "--issue", "12", "--prompt", "Go."]);

    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("a live session is already named app-issue-12"));
    assert!(repo.bg_calls().is_empty());
    assert!(!repo.main.join(".claude/worktrees/issue-12").exists());
}

#[test]
fn a_session_name_over_32_characters_is_refused() {
    let repo = Repo::new();

    let output = repo.spawn(&[
        "a-very-long-worker-name-indeed",
        "--issue",
        "12",
        "--prompt",
        "Go.",
    ]);

    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("longer than 32 characters"));
}

#[test]
fn a_failed_launch_is_undone_so_the_name_can_be_retried() {
    let repo = Repo::new();
    repo.launches("", 1);

    let output = repo.spawn(&["issue-12", "--issue", "12", "--prompt", "Go."]);

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("didn't start app-issue-12"));
    assert!(repo.store().read().unwrap().workers.is_empty());
    assert!(!repo.main.join(".claude/worktrees/issue-12").exists());
    let branch = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", "refs/heads/issue-12"])
        .current_dir(&repo.main)
        .status()
        .unwrap();
    assert!(!branch.success());
}

#[test]
fn a_launch_that_exits_0_without_a_job_id_keeps_the_worker() {
    let repo = Repo::new();
    repo.launches("something unexpected\n", 0);

    let output = repo.spawn(&["issue-12", "--issue", "12", "--prompt", "Go."]);

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("didn't start app-issue-12"));
    // A session may be running there, so nothing is removed.
    assert_eq!(
        repo.store().read().unwrap().workers["issue-12"].job_id,
        None
    );
    assert!(repo.main.join(".claude/worktrees/issue-12").exists());
}

#[test]
fn an_untrusted_repository_is_refused_with_how_to_trust_it() {
    let repo = Repo::new();
    // As 2.1.314 prints it, with the worktree's path.
    repo.launches(
        "Workspace not trusted. Run `claude` in /x/.claude/worktrees/issue-12 once and accept the trust prompt, then retry.\n",
        1,
    );

    let output = repo.spawn(&["issue-12", "--issue", "12", "--prompt", "Go."]);

    assert_eq!(output.status.code(), Some(2));
    let main = repo.main.display();
    assert!(stderr(&output).contains(&format!(
        "fix: run `claude` in {main} once and accept the trust prompt"
    )));
}

impl Repo {
    fn orchestral(&self, args: &[&str]) -> Output {
        self.stubs
            .orchestral()
            .args(args)
            .current_dir(&self.main)
            .output()
            .unwrap()
    }

    /// Replaces the policy's settings on `origin/main`, keeping the budget.
    fn policy(&self, settings: &str) {
        fs::write(
            self.main.join(".orchestral/policy.md"),
            format!("---\nfleet_token_budget: 1000\n{settings}---\nRules.\n"),
        )
        .unwrap();
        git(&self.main, &["commit", "-q", "-am", "policy"]);
        git(&self.main, &["push", "-q", "origin", "main"]);
    }

    fn usage(&self, output: &str) {
        self.stubs.respond("claude", &["-p", "/usage"], output, 0);
    }
}

fn assert_refused(output: &Output, reason: &str, fix: &str) {
    assert_eq!(output.status.code(), Some(2), "{}", stderr(output));
    let stderr = stderr(output);
    assert!(stderr.contains(&format!("refused: {reason}")), "{stderr}");
    assert!(stderr.contains(&format!("fix: {fix}")), "{stderr}");
}

#[test]
fn a_paused_fleet_refuses_until_unpaused() {
    let repo = Repo::new();
    repo.launches(STARTED, 0);

    assert_eq!(repo.orchestral(&["pause"]).status.code(), Some(0));
    let refused = repo.spawn(&["issue-12", "--issue", "12", "--prompt", "Go."]);
    assert_eq!(repo.orchestral(&["unpause"]).status.code(), Some(0));
    let started = repo.spawn(&["issue-12", "--issue", "12", "--prompt", "Go."]);

    assert_refused(&refused, "the fleet is paused", "run `orchestral unpause`");
    assert_eq!(started.status.code(), Some(0), "{}", stderr(&started));
    assert_eq!(repo.bg_calls().len(), 1);
    let events = fs::read_to_string(repo.store().dir().join("events.jsonl")).unwrap();
    assert!(
        events.contains(r#""text":"the fleet is paused""#),
        "{events}"
    );
    assert!(
        events.contains(r#""text":"the fleet is unpaused""#),
        "{events}"
    );
}

#[test]
fn spawning_past_the_worker_cap_is_refused() {
    let repo = Repo::new();
    repo.policy("workers: 1\n");
    repo.launches(STARTED, 0);

    let first = repo.spawn(&["issue-12", "--issue", "12", "--prompt", "Go."]);
    let second = repo.spawn(&["issue-13", "--issue", "13", "--prompt", "Go."]);

    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    assert_refused(
        &second,
        "the fleet is at its cap of 1 workers",
        "wait until a worker is cleaned up after its PR merges",
    );
    assert!(!repo.main.join(".claude/worktrees/issue-13").exists());
    assert_eq!(repo.bg_calls().len(), 1);
}

#[test]
fn a_window_at_the_plan_limit_is_refused() {
    let repo = Repo::new();
    repo.policy("plan_limit_percent: 80\n");
    repo.usage(
        "Current session: 95% used · resets Nov 2 at 6:49am (America/Los_Angeles)\n\
         Current week (all models): 80% used · resets Nov 4 at 1pm (America/Los_Angeles)\n",
    );
    repo.launches(STARTED, 0);

    let output = repo.spawn(&["issue-12", "--issue", "12", "--prompt", "Go."]);

    // The zone abbreviation, PST or PDT, depends on the year.
    assert_refused(
        &output,
        "the week (all models) window is 80% used, at or past plan_limit_percent (80%)",
        "wait until it resets at Nov 4 at 1:00pm P",
    );
    assert!(repo.bg_calls().is_empty());
}

#[test]
fn windows_under_the_plan_limit_spawn() {
    let repo = Repo::new();
    repo.usage("Current session: 84% used · resets Nov 2 at 6:49am (America/Los_Angeles)\n");
    repo.launches(STARTED, 0);

    let output = repo.spawn(&["issue-12", "--issue", "12", "--prompt", "Go."]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(!stderr(&output).contains("warning"), "{}", stderr(&output));
}

#[test]
fn unreadable_plan_limits_warn_and_spawn() {
    let repo = Repo::new();
    repo.stubs.respond("claude", &["-p", "/usage"], "", 1);
    repo.launches(STARTED, 0);

    let output = repo.spawn(&["issue-12", "--issue", "12", "--prompt", "Go."]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        stderr(&output).starts_with("warning: plan limits are unavailable"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_run_over_its_fleet_budget_is_refused() {
    const SESSION: &str = "00000000-0000-4000-8000-00000000b002";
    let repo = Repo::new();
    // A worker whose transcript holds about 3,011 tokens since the run
    // started, over the policy's 1,000.
    let worktree = repo.main.join(".claude/worktrees/issue-11");
    let projects = repo
        .config()
        .join("projects")
        .join(orchestral::transcript::project_dir_name(&worktree));
    fs::create_dir_all(&projects).unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/claude/2.1.314/fleet")
            .join(format!("{SESSION}.jsonl")),
        projects.join(format!("{SESSION}.jsonl")),
    )
    .unwrap();
    repo.store()
        .update(|state| {
            state.run_started_at = Some("2026-11-02T00:00:00Z".into());
            state.workers.insert(
                "issue-11".into(),
                orchestral::state::Worker {
                    session_name: "app-issue-11".into(),
                    issue: 11,
                    model: "claude-sonnet-5-5".into(),
                    effort: "medium".into(),
                    worktree: worktree.clone(),
                    branch: "issue-11".into(),
                    job_id: Some("job11".into()),
                    review_of: None,
                    created_at: "2026-11-02T00:00:01Z".into(),
                },
            );
        })
        .unwrap();
    repo.stubs.respond(
        "claude",
        &["agents"],
        &json!([{"id": "job11", "sessionId": SESSION, "cwd": worktree}]).to_string(),
        0,
    );
    repo.launches(STARTED, 0);

    let output = repo.spawn(&["issue-12", "--issue", "12", "--prompt", "Go."]);

    assert_refused(
        &output,
        "the fleet has used about 3011 tokens (estimate), and fleet_token_budget is 1000",
        "ask the owner to raise fleet_token_budget in .orchestral/policy.md",
    );
    assert!(repo.bg_calls().is_empty());
}

#[test]
fn a_reviewer_gets_the_review_prompt_and_the_policy_s_review_command() {
    let repo = Repo::new();
    fs::write(
        repo.main.join(".orchestral/policy.md"),
        "---\nfleet_token_budget: 1000\nreview_command: Run /refine on the PR.\n---\nRules.\n",
    )
    .unwrap();
    git(&repo.main, &["commit", "-q", "-am", "review command"]);
    git(&repo.main, &["push", "-q", "origin", "main"]);
    repo.launches(STARTED, 0);

    let output = repo.spawn(&[
        "review-40",
        "--issue",
        "12",
        "--role",
        "reviewer",
        "--pr",
        "40",
        "--prompt",
        "Review PR #40.",
    ]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let prompt = repo.worker_file("review-40", "prompt.md");
    assert!(
        prompt.contains("reviewing PR #40 against issue #12"),
        "{prompt}"
    );
    assert!(prompt.contains("- Run /refine on the PR.\n"), "{prompt}");
    assert!(prompt.contains("review 40 --pass --sha <head>"), "{prompt}");
    let state = repo.store().read().unwrap();
    assert_eq!(state.workers["review-40"].review_of, Some(40));
}

#[test]
fn a_pr_gets_one_reviewer() {
    let repo = Repo::new();
    repo.launches(STARTED, 0);
    let review = |name: &str| {
        repo.spawn(&[
            name,
            "--issue",
            "12",
            "--role",
            "reviewer",
            "--pr",
            "40",
            "--prompt",
            "Review PR #40.",
        ])
    };
    assert_eq!(review("review-40").status.code(), Some(0));

    let second = review("review-40b");

    assert_eq!(second.status.code(), Some(2));
    assert!(stderr(&second).contains("PR #40 already has a reviewer, review-40"));
}

#[test]
fn a_reviewer_needs_a_pr_and_a_worker_takes_none() {
    let repo = Repo::new();

    let reviewer = repo.spawn(&[
        "review-40",
        "--issue",
        "12",
        "--role",
        "reviewer",
        "--prompt",
        "Review.",
    ]);
    let worker = repo.spawn(&[
        "issue-12", "--issue", "12", "--pr", "40", "--prompt", "Work.",
    ]);

    assert_eq!(reviewer.status.code(), Some(1), "{}", stderr(&reviewer));
    assert_eq!(worker.status.code(), Some(1), "{}", stderr(&worker));
    assert!(stderr(&worker).contains("--pr is only for `--role reviewer`"));
}
