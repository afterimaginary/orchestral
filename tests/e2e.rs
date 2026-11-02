//! Runs against the installed, logged-in `claude`, and for the sandbox, the
//! sandbox repository on GitHub. Stubs can't catch a change in Claude Code
//! itself, so these run for real, on demand, and never in `verify`: every
//! test here is ignored, and `make e2e` and `make sandbox` run them.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use orchestral::sessions;
use orchestral::state::Store;
use orchestral::{budget, limits, transcript};
use serde_json::Value;

const MODEL: &str = "claude-sonnet-5-5";

/// Clears the caller's Claude Code session variables from this process, so
/// everything it runs, including the library's own `claude` calls, runs as
/// if from a plain terminal. `CLAUDE_CONFIG_DIR` is kept, since it says which
/// account to use.
fn clear_session_env() {
    for (key, _) in std::env::vars() {
        if key == "CLAUDECODE" || (key.starts_with("CLAUDE_") && key != "CLAUDE_CONFIG_DIR") {
            // SAFETY: the make targets run these tests one at a time, and
            // this runs first, before the test starts any thread.
            unsafe { std::env::remove_var(key) };
        }
    }
}

fn run(command: &mut Command) -> Output {
    let output = command.output().expect("the command should start");
    if !output.status.success() {
        panic!(
            "{command:?} failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    output
}

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_owned()
}

fn orchestral(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orchestral"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn orchestral_ok(cwd: &Path, args: &[&str]) -> String {
    let output = orchestral(cwd, args);
    assert!(
        output.status.success(),
        "orchestral {args:?}: {}{}",
        text(&output),
        String::from_utf8_lossy(&output.stderr)
    );
    text(&output)
}

fn git(cwd: &Path, args: &[&str]) -> String {
    text(&run(Command::new("git").args(args).current_dir(cwd)))
}

fn status(cwd: &Path) -> Value {
    serde_json::from_str(&orchestral_ok(cwd, &["status", "--json"])).unwrap()
}

fn worker<'a>(status: &'a Value, name: &str) -> &'a Value {
    status["workers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|worker| worker["name"] == name)
        .unwrap_or_else(|| panic!("{name} isn't in status: {status}"))
}

/// Waits up to `limit` for `done`, checking every few seconds.
fn eventually(what: &str, limit: Duration, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < limit, "timed out waiting for {what}");
        thread::sleep(Duration::from_secs(5));
    }
    eprintln!("{what}: after {} seconds", start.elapsed().as_secs());
}

fn step(message: &str) {
    eprintln!("== {message}");
}

fn installed_version() -> String {
    text(&run(Command::new("claude").arg("--version")))
}

#[test]
#[ignore = "runs the installed claude; run with `make e2e`"]
fn compatibility() {
    clear_session_env();
    step(&format!("claude {}", installed_version()));
    let pinned = fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    eprintln!("fixtures pinned for: {pinned:?}");

    step("a fresh transcript from a print session");
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().canonicalize().unwrap();
    let started = Timestamp::now();
    let reply = run(Command::new("claude")
        .args(["-p", "Reply with the single word ok.", "--model", MODEL])
        .args(["--output-format", "json"])
        .current_dir(&cwd));
    let reply: Value = serde_json::from_slice(&reply.stdout).unwrap();
    let session_id = reply["session_id"].as_str().expect("a session_id");
    let projects = transcript::projects_dir().unwrap();
    let path = transcript::find(&projects, &cwd, session_id).expect("its transcript");

    let context = transcript::read(&path, Timestamp::now(), None).unwrap();
    eprintln!("context: {context:?}");
    assert!(context.tokens > 0, "no context tokens");
    assert!(
        context
            .model
            .as_deref()
            .is_some_and(|model| model.starts_with("claude-sonnet")),
        "model {:?}",
        context.model
    );
    assert!(context.last_request_at.is_some(), "no last request time");
    assert!(context.cache_ttl.is_some(), "no cache lifetime");
    assert!(context.warm, "a session just used should be warm");

    let used = budget::fleet_usage(&[path], Some(started)).unwrap();
    eprintln!("budget: {used}");
    assert!(used.estimated_tokens > 0);

    step("plan limits");
    let limits = limits::read();
    eprintln!("{limits}");
    assert!(limits.available, "{:?}", limits.reason);

    step("claude agents");
    let agents = sessions::parse_agents(&text(&run(
        Command::new("claude").args(["agents", "--json", "--all"])
    )))
    .unwrap();
    eprintln!("{} sessions listed", agents.len());

    fs::remove_dir_all(projects.join(transcript::project_dir_name(&cwd))).ok();
}

/// The local runner's repository and its bare remote, rebuilt on every run
/// at this one path, so the owner trusts it in Claude Code once.
fn local_root() -> PathBuf {
    std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join("orchestral-e2e")
}

#[test]
#[ignore = "runs real workers; run with `make e2e`"]
fn local_runner() {
    clear_session_env();
    let root = local_root();
    let repo = root.join("repo");
    let origin = root.join("origin.git");

    step("refuse while an earlier run's workers are live");
    let agents = sessions::agents(Path::new("claude")).unwrap();
    let live: Vec<_> = agents
        .iter()
        .filter(|agent| {
            // Only workers: an interactive session there, such as the one
            // that accepted the trust prompt, doesn't block a run.
            agent.kind.as_deref() == Some("background")
                && agent.pid.is_some()
                && agent.cwd.as_ref().is_some_and(|cwd| cwd.starts_with(&root))
        })
        .map(|agent| agent.id.clone().unwrap_or_else(|| agent.session_id.clone()))
        .collect();
    assert!(
        live.is_empty(),
        "workers from an earlier run are live; stop them: claude stop {}",
        live.join(" ")
    );

    step(&format!("rebuild {}", repo.display()));
    fs::create_dir_all(&root).unwrap();
    for dir in [&repo, &origin] {
        if dir.exists() {
            fs::remove_dir_all(dir).unwrap();
        }
    }
    git(&root, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
    git(&root, &["clone", "-q", "origin.git", "repo"]);
    fs::create_dir(repo.join(".orchestral")).unwrap();
    fs::write(
        repo.join(".orchestral/policy.md"),
        format!(
            "---\nfleet_token_budget: 5000000\nworkers: 2\ndefault_model: {MODEL}\ndefault_effort: low\n---\nKeep changes small.\n"
        ),
    )
    .unwrap();
    fs::write(repo.join("README.md"), "# e2e\n").unwrap();
    git(&repo, &["add", "."]);
    git(
        &repo,
        &[
            "-c",
            "user.name=e2e",
            "-c",
            "user.email=e2e@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "Start",
        ],
    );
    git(&repo, &["push", "-q", "origin", "main"]);

    step("init, which refuses until the owner trusts the path");
    let init = orchestral(&repo, &["init"]);
    assert!(
        init.status.success(),
        "{}{}\nTrust {} once: run `claude` there and accept the trust prompt. The path stays the same on every run.",
        text(&init),
        String::from_utf8_lossy(&init.stderr),
        repo.display()
    );

    step("up starts a run");
    let up: Value = serde_json::from_str(&orchestral_ok(
        &repo,
        &["up", "--json", "--keepalive-hours", "0.01"],
    ))
    .unwrap();
    let _run = LocalRun {
        repo: repo.clone(),
        supervisor: up["pid"].as_u64().unwrap(),
    };
    assert!(status(&repo)["budget"]["run_started_at"].is_string());

    step("spawn a worker that commits and pushes");
    let spawned = orchestral_ok(
        &repo,
        &[
            "spawn",
            "e2e-hello",
            "--issue",
            "1",
            "--prompt",
            "Create hello.txt containing the word hello, commit it with the message \"Add hello\", and push your branch to origin. There's no GitHub here, so don't open a PR. Then stop.",
        ],
    );
    eprintln!("{spawned}");
    eventually("the worker's push", Duration::from_secs(600), || {
        Command::new("git")
            .args(["show", "e2e-hello:hello.txt"])
            .current_dir(&origin)
            .output()
            .is_ok_and(|output| output.status.success())
    });

    step("status reads the worker");
    let fleet = status(&repo);
    let hello = worker(&fleet, "e2e-hello");
    eprintln!("{hello}");
    assert_eq!(hello["session"], "running");
    assert!(hello["context"]["tokens"].as_u64().unwrap() > 0);
    assert!(hello["wake_cost"]["estimated_tokens"].as_u64().unwrap() > 0);
    assert!(
        fleet["budget"]["used"]["estimated_tokens"]
            .as_u64()
            .unwrap()
            > 0
    );
    eprintln!("{}", orchestral_ok(&repo, &["status"]));

    step("stop and resume");
    eprintln!("{}", orchestral_ok(&repo, &["stop", "e2e-hello"]));
    assert_eq!(worker(&status(&repo), "e2e-hello")["session"], "stopped");
    eprintln!("{}", orchestral_ok(&repo, &["resume", "e2e-hello"]));
    assert_eq!(worker(&status(&repo), "e2e-hello")["session"], "running");
    eprintln!("{}", orchestral_ok(&repo, &["stop", "e2e-hello"]));
    step("done");
}

/// Stops the local run's worker and supervisor, whether the run passed or
/// failed, so the next run doesn't find them live.
struct LocalRun {
    repo: PathBuf,
    supervisor: u64,
}

impl Drop for LocalRun {
    fn drop(&mut self) {
        let _ = orchestral(&self.repo, &["stop", "e2e-hello"]);
        let _ = Command::new("kill")
            .arg(self.supervisor.to_string())
            .output();
    }
}

/// The sandbox clone.
fn sandbox() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap()).join("src/orchestral-sandbox")
}

fn gh(cwd: &Path, args: &[&str]) -> String {
    text(&run(Command::new("gh").args(args).current_dir(cwd)))
}

#[test]
#[ignore = "files an issue and a PR in the sandbox repository; run with `make sandbox`"]
fn sandbox_runner() {
    clear_session_env();
    let clone = sandbox();
    let store = Store::for_repo(&clone).unwrap();

    step("refuse with local changes or live workers");
    let changes = git(&clone, &["status", "--porcelain"]);
    assert!(
        changes.is_empty(),
        "refused: {} has local changes:\n{changes}",
        clone.display()
    );
    let state = store.read().unwrap();
    let agents = sessions::agents(Path::new("claude")).unwrap();
    let live: Vec<&String> = state
        .workers
        .iter()
        .filter(|(_, worker)| {
            agents
                .iter()
                .any(|agent| agent.pid.is_some() && agent.id.is_some() && agent.id == worker.job_id)
        })
        .map(|(name, _)| name)
        .collect();
    assert!(
        live.is_empty(),
        "refused: live workers in the sandbox: {live:?}"
    );
    git(&clone, &["pull", "-q", "--ff-only"]);

    let stamp = Timestamp::now().strftime("%Y%m%d%H%M%S").to_string();
    let name = format!("sbx-{}", &stamp[8..]);
    step(&format!("file an issue for {name}"));
    let url = gh(
        &clone,
        &[
            "issue",
            "create",
            "--title",
            &format!("Sandbox run {stamp}: add a greeting"),
            "--body",
            &format!(
                "Add `greetings/{stamp}.txt` containing `hello`.\n\nFiled by the sandbox runner."
            ),
        ],
    );
    let issue: u64 = url.rsplit('/').next().unwrap().parse().unwrap();
    eprintln!("{url}");
    let _run = SandboxRun {
        clone: clone.clone(),
        name: name.clone(),
        issue,
    };

    step("spawn a worker that opens a PR");
    eprintln!(
        "{}",
        orchestral_ok(
            &clone,
            &[
                "spawn",
                &name,
                "--issue",
                &issue.to_string(),
                "--prompt",
                &format!("Implement #{issue}.")
            ]
        )
    );
    let mut pr = None;
    eventually("the worker's PR", Duration::from_secs(900), || {
        let found = gh(
            &clone,
            &[
                "pr",
                "list",
                "--head",
                &name,
                "--json",
                "number",
                "-q",
                ".[0].number",
            ],
        );
        pr = found.parse::<u64>().ok();
        pr.is_some()
    });
    let pr = pr.unwrap();

    step("status shows the PR");
    let fleet = status(&clone);
    let worker = worker(&fleet, &name);
    eprintln!("{worker}");
    assert_eq!(worker["pr"], pr);

    step("done");
}

/// Undoes what a sandbox run made, whether it passed or failed: stops the
/// worker, removes its worktree, closes any PR it opened with its branch, and
/// closes the issue. Each step is best effort.
struct SandboxRun {
    clone: PathBuf,
    name: String,
    issue: u64,
}

impl Drop for SandboxRun {
    fn drop(&mut self) {
        step("close the PR and issue, and remove the worker");
        let clone = &self.clone;
        let name = self.name.as_str();
        let quiet = |program: &str, args: &[&str]| {
            let _ = Command::new(program).args(args).current_dir(clone).output();
        };
        let _ = orchestral(clone, &["stop", name]);
        // The worktree goes first: `gh pr close --delete-branch` also deletes
        // the local branch, which a worktree would still hold.
        let worktree = clone.join(".claude/worktrees").join(name);
        quiet(
            "git",
            &["worktree", "remove", "--force", worktree.to_str().unwrap()],
        );
        let prs = Command::new("gh")
            .args([
                "pr",
                "list",
                "--head",
                name,
                "--json",
                "number",
                "-q",
                ".[].number",
            ])
            .current_dir(clone)
            .output()
            .map(|output| text(&output))
            .unwrap_or_default();
        for pr in prs.lines() {
            quiet(
                "gh",
                &[
                    "pr",
                    "close",
                    pr,
                    "--delete-branch",
                    "--comment",
                    "<!-- orchestral -->\nClosed by the sandbox runner.",
                ],
            );
        }
        // Without a PR, the branch may still be on either side.
        quiet("git", &["branch", "-D", name]);
        quiet("git", &["push", "-q", "origin", "--delete", name]);
        quiet(
            "gh",
            &[
                "issue",
                "close",
                &self.issue.to_string(),
                "--reason",
                "not planned",
            ],
        );
        if let Ok(store) = Store::for_repo(clone) {
            let _ = store.update(|state| state.workers.remove(name));
        }
    }
}
