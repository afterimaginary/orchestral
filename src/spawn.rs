//! `orchestral spawn`: starts a worker in its own worktree, with its own
//! settings and prompt.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde::Serialize;
use serde_json::json;

use crate::outcome::Failure;
use crate::policy::Policy;
use crate::sessions;
use crate::state::{Store, Worker};

/// Claude Code renames a session whose name is taken, and names longer than
/// this get cut, so session names stay within it.
const MAX_SESSION_NAME: usize = 32;

/// Where worker worktrees live, relative to the main checkout.
const WORKTREES: &str = ".claude/worktrees";

const PLUGIN: &str = "orchestral";

pub struct Request {
    pub name: String,
    pub issue: u64,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// What the worker is asked to do first.
    pub prompt: String,
}

#[derive(Debug, Serialize)]
pub struct Spawned {
    pub name: String,
    pub session_name: String,
    pub job_id: String,
    pub worktree: PathBuf,
}

impl fmt::Display for Spawned {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Started {} as {} (job {}) in {}",
            self.name,
            self.session_name,
            self.job_id,
            self.worktree.display()
        )
    }
}

pub fn spawn(cwd: &Path, request: Request) -> Result<Spawned, Failure> {
    let store = Store::for_repo(cwd)?;
    let main = main_checkout(store.dir())?;
    let policy = Policy::load(cwd)?;
    let slug = repo_slug(&main);
    let name = &request.name;
    let session_name = format!("{slug}-{name}");
    check_name(name, &session_name)?;
    refuse_live_duplicate(&session_name)?;
    if store.read()?.workers.contains_key(name) {
        return Err(Failure::Refused {
            reason: format!("a worker named {name} already exists"),
            fix: "choose another name".into(),
        });
    }

    let worktree = main.join(WORKTREES).join(name);
    git(&main, &["fetch", "--quiet", "origin", "main"])?;
    git(
        &main,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            name,
            path_str(&worktree)?,
            "origin/main",
        ],
    )?;
    exclude_worktrees(store.dir())?;

    let dir = store.dir().join("workers").join(name);
    fs::create_dir_all(&dir).map_err(|err| io_error(&dir, err))?;
    let settings = dir.join("settings.json");
    let prompt = dir.join("prompt.md");
    write(&settings, &settings_json(name, &main, &plugin_ids()?)?)?;
    write(&prompt, &worker_prompt(name, request.issue))?;

    let model = request.model.unwrap_or(policy.settings.default_model);
    let effort = request.effort.unwrap_or(policy.settings.default_effort);
    // Recorded before launching, so a crash leaves a visible entry.
    store.update(|state| {
        state.workers.insert(
            name.clone(),
            Worker {
                session_name: session_name.clone(),
                issue: request.issue,
                model: model.clone(),
                effort: effort.clone(),
                worktree: worktree.clone(),
                branch: name.clone(),
                job_id: None,
                created_at: jiff::Timestamp::now().to_string(),
            },
        )
    })?;

    let launched = Command::new("claude")
        .args([
            "--bg",
            "--name",
            &session_name,
            "--remote-control",
            &session_name,
        ])
        .arg("--settings")
        .arg(&settings)
        .arg("--append-system-prompt-file")
        .arg(&prompt)
        .args([
            "--permission-mode",
            "auto",
            "--model",
            &model,
            "--effort",
            &effort,
        ])
        .arg("--")
        .arg(&request.prompt)
        .current_dir(&worktree)
        .output()
        .map_err(|err| Failure::Error(format!("can't run claude: {err}")))
        .and_then(|output| launched_job_id(&output, &session_name, &main));
    let job_id = match launched {
        Ok(id) => id,
        Err(failure) => {
            // Nothing ran, so undo the worker and its empty worktree to let
            // the same name be retried.
            store.update(|state| state.workers.remove(name))?;
            git(
                &main,
                &["worktree", "remove", "--force", path_str(&worktree)?],
            )?;
            git(&main, &["branch", "-D", name])?;
            return Err(failure);
        }
    };
    store.update(|state| {
        if let Some(worker) = state.workers.get_mut(name) {
            worker.job_id = Some(job_id.clone());
        }
    })?;
    store.record(
        "worker",
        &format!("{name} started on issue #{}", request.issue),
    )?;

    Ok(Spawned {
        name: name.clone(),
        session_name,
        job_id,
        worktree,
    })
}

/// The job id `claude --bg` printed. Claude Code trusts a worktree when it
/// trusts the main checkout, so an untrusted workspace is fixed there.
fn launched_job_id(output: &Output, session_name: &str, main: &Path) -> Result<String, Failure> {
    let printed = String::from_utf8_lossy(&output.stdout);
    let errors = String::from_utf8_lossy(&output.stderr);
    if let (true, Some(id)) = (output.status.success(), parse_job_id(&printed)) {
        return Ok(id);
    }
    if printed.contains("Workspace not trusted") || errors.contains("Workspace not trusted") {
        return Err(Failure::Refused {
            reason: format!("Claude Code doesn't trust {}", main.display()),
            fix: format!(
                "run `claude` in {} once and accept the trust prompt",
                main.display()
            ),
        });
    }
    Err(Failure::Error(format!(
        "claude --bg didn't start {session_name}: {}{}",
        printed.trim_end(),
        errors.trim_end()
    )))
}

/// Names become a branch, a directory and part of the session name, so they
/// stay to lowercase letters, digits and dashes.
fn check_name(name: &str, session_name: &str) -> Result<(), Failure> {
    let valid = !name.is_empty()
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !valid {
        return Err(Failure::Error(format!(
            "{name} isn't a valid worker name: use lowercase letters, digits and dashes"
        )));
    }
    if session_name.len() > MAX_SESSION_NAME {
        return Err(Failure::Refused {
            reason: format!(
                "the session name {session_name} is longer than {MAX_SESSION_NAME} characters"
            ),
            fix: format!(
                "use a name of at most {} characters",
                MAX_SESSION_NAME - (session_name.len() - name.len())
            ),
        });
    }
    Ok(())
}

fn refuse_live_duplicate(session_name: &str) -> Result<(), Failure> {
    let Some(dir) = sessions::records_dir() else {
        return Ok(());
    };
    let now_ms = jiff::Timestamp::now().as_millisecond();
    for record in sessions::read_records(&dir)? {
        if record.name.as_deref() != Some(session_name) {
            continue;
        }
        let process = sessions::process(record.pid)?;
        if sessions::is_alive(&record, process.as_ref(), now_ms) {
            return Err(Failure::Refused {
                reason: format!("a live session is already named {session_name}"),
                fix: "choose another name, or stop that session".into(),
            });
        }
    }
    Ok(())
}

/// The main checkout: the parent of the git common directory that holds the
/// store.
fn main_checkout(store: &Path) -> Result<PathBuf, Failure> {
    let common = store.parent().unwrap_or(store);
    let main = common.parent().unwrap_or(common);
    main.canonicalize().map_err(|err| io_error(main, err))
}

/// The main checkout's directory name.
fn repo_slug(main: &Path) -> String {
    main.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into())
}

/// Adds `.claude/worktrees/` to `.git/info/exclude` once.
fn exclude_worktrees(store: &Path) -> Result<(), Failure> {
    let info = store.parent().unwrap_or(store).join("info");
    fs::create_dir_all(&info).map_err(|err| io_error(&info, err))?;
    let exclude = info.join("exclude");
    let mut text = fs::read_to_string(&exclude).unwrap_or_default();
    let line = format!("/{WORKTREES}/");
    if text.lines().any(|existing| existing == line) {
        return Ok(());
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&line);
    text.push('\n');
    write(&exclude, &text)
}

/// The worker's settings. Hooks use the binary's absolute path and `PATH` is
/// set, since a background session doesn't inherit the launching shell's.
fn settings_json(name: &str, main: &Path, plugin_ids: &[String]) -> Result<String, Failure> {
    let exe = std::env::current_exe()
        .map_err(|err| Failure::Error(format!("can't find this binary: {err}")))?;
    let enabled_plugins: serde_json::Map<String, serde_json::Value> = plugin_ids
        .iter()
        .map(|id| (id.clone(), json!(false)))
        .collect();
    let settings = json!({
        "crossSessionInbound": "accept",
        "isolatePeerMachines": true,
        "worktree": {"bgIsolation": "none"},
        "env": {
            "ORCHESTRAL_WORKER": name,
            "ORCHESTRAL_REPO": main,
            "PATH": std::env::var("PATH").unwrap_or_default(),
        },
        "hooks": {"PreToolUse": [{
            "matcher": "Bash",
            "hooks": [{"type": "command", "command": format!("{} hook worker-guard", exe.display())}],
        }]},
        // The plugin's monitor would make a worker look busy (ADR-0001).
        "enabledPlugins": enabled_plugins,
    });
    Ok(serde_json::to_string_pretty(&settings).expect("settings always serialize"))
}

/// Every installed ID of the Orchestral plugin, such as
/// `orchestral@<marketplace>`.
fn plugin_ids() -> Result<Vec<String>, Failure> {
    let output = Command::new("claude")
        .args(["plugin", "list", "--json"])
        .output()
        .map_err(|err| Failure::Error(format!("can't run claude: {err}")))?;
    if !output.status.success() {
        return Err(Failure::Error(format!(
            "claude plugin list failed: {}",
            String::from_utf8_lossy(&output.stderr).trim_end()
        )));
    }
    let plugins: Vec<serde_json::Value> =
        serde_json::from_slice(&output.stdout).map_err(|err| {
            Failure::Error(format!(
                "unexpected `claude plugin list --json` output: {err}"
            ))
        })?;
    Ok(plugins
        .iter()
        .filter_map(|plugin| plugin["id"].as_str())
        .filter(|id| id.split('@').next() == Some(PLUGIN))
        .map(str::to_owned)
        .collect())
}

fn worker_prompt(name: &str, issue: u64) -> String {
    // Spawn runs from the orchestrator's own session.
    let orchestrator = std::env::var("CLAUDE_CODE_SESSION_ID")
        .unwrap_or_else(|_| "the session that started you".into());
    include_str!("prompts/worker.md")
        .replace("{{name}}", name)
        .replace("{{issue}}", &issue.to_string())
        .replace("{{orchestrator}}", &orchestrator)
}

/// The job id from `backgrounded · <id> · <name>`, after removing any ANSI
/// escape sequences.
fn parse_job_id(output: &str) -> Option<String> {
    strip_ansi(output).lines().find_map(|line| {
        let mut parts = line.trim().strip_prefix("backgrounded")?.split('·');
        parts.next()?;
        let id = parts.next()?.trim();
        (!id.is_empty()).then(|| id.to_owned())
    })
}

/// Removes `ESC [ ... <letter>` sequences.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn git(dir: &Path, args: &[&str]) -> Result<(), Failure> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|err| Failure::Error(format!("can't run git: {err}")))?;
    if !output.status.success() {
        return Err(Failure::Error(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim_end()
        )));
    }
    Ok(())
}

fn path_str(path: &Path) -> Result<&str, Failure> {
    path.to_str()
        .ok_or_else(|| Failure::Error(format!("{} isn't valid UTF-8", path.display())))
}

fn write(path: &Path, text: &str) -> Result<(), Failure> {
    fs::write(path, text).map_err(|err| io_error(path, err))
}

fn io_error(path: &Path, err: std::io::Error) -> Failure {
    Failure::Error(format!("{}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_job_id_with_and_without_ansi() {
        let plain = "backgrounded · 558b988a · app-issue-12\n  claude agents   list sessions\n";
        let colored =
            "\u{1b}[2mbackgrounded\u{1b}[0m · \u{1b}[1m558b988a\u{1b}[0m · app-issue-12\n";

        assert_eq!(parse_job_id(plain).as_deref(), Some("558b988a"));
        assert_eq!(parse_job_id(colored).as_deref(), Some("558b988a"));
        assert_eq!(parse_job_id("Error: not trusted\n"), None);
    }

    #[test]
    fn names_are_lowercase_letters_digits_and_dashes() {
        assert!(check_name("issue-12", "app-issue-12").is_ok());
        for bad in ["", "-x", "Issue-12", "issue_12", "a/b"] {
            assert!(
                matches!(check_name(bad, "app-x"), Err(Failure::Error(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_session_name_over_32_characters_is_refused() {
        let name = "a-rather-long-worker-name";
        let session_name = format!("my-project-{name}");

        let Err(Failure::Refused { fix, .. }) = check_name(name, &session_name) else {
            panic!("expected a refusal");
        };
        assert_eq!(fix, "use a name of at most 21 characters");
    }

    #[test]
    fn the_prompt_names_the_worker_and_its_issue() {
        let prompt = worker_prompt("issue-12", 12);

        assert!(prompt.starts_with("You are worker issue-12 on issue #12."));
        assert!(prompt.contains("Start every GitHub comment with <!-- orchestral -->."));
        assert!(prompt.contains("Never merge"));
        assert!(!prompt.contains("{{"));
    }
}
