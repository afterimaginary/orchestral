//! `orchestral init` and `orchestral doctor`: setting a project up, and
//! finding problems before a run rather than during it.

use std::fmt;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;
use serde_json::Value;

use crate::outcome::Failure;
use crate::policy::Policy;
use crate::spawn::main_checkout;
use crate::state::Store;

/// The Claude Code version the undocumented formats were captured from
/// (`docs/specs/claude-code-internals.md`).
pub const TESTED_CLAUDE: &str = "2.1.314";

const POLICY: &str = ".orchestral/policy.md";
const BLOCK_START: &str = "# >>> orchestral >>>";
const BLOCK_END: &str = "# <<< orchestral <<<";

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub checks: Vec<Check>,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for check in &self.checks {
            let status = match check.status {
                Status::Ok => "ok  ",
                Status::Warn => "warn",
                Status::Fail => "FAIL",
            };
            writeln!(f, "{status} {:<10} {}", check.name, check.detail)?;
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct Initialized {
    /// The policy file, when init created it.
    pub created_policy: Option<PathBuf>,
    pub hook: PathBuf,
    /// Whether git tracks the hook, as with a `core.hooksPath` in the
    /// repository.
    pub hook_tracked: bool,
}

impl fmt::Display for Initialized {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self.created_policy {
            Some(path) => writeln!(
                f,
                "Created {}. Set fleet_token_budget, then commit it and push it to main.",
                path.display()
            )?,
            None => writeln!(f, "Kept the existing {POLICY}.")?,
        }
        write!(f, "Installed the pre-push hook in {}.", self.hook.display())?;
        if self.hook_tracked {
            write!(
                f,
                "\nThat file is tracked, and the block names this machine's orchestral, so don't commit the block."
            )?;
        }
        Ok(())
    }
}

/// Checks everything a run needs. A failed check makes the report an error,
/// after it's printed.
pub fn doctor(cwd: &Path) -> Report {
    let mut checks = vec![claude_version(), claude_auth(), gh_auth(), git_version()];
    match Store::for_repo(cwd).and_then(|store| main_checkout(store.dir())) {
        Ok(main) => {
            checks.push(trust(&main));
            checks.push(policy(cwd, &main));
        }
        Err(
            Failure::Error(message)
            | Failure::Refused {
                reason: message, ..
            },
        ) => checks.push(fail("repo", &message)),
    }
    checks.push(portless());
    checks.push(tailscale());
    Report { checks }
}

impl Report {
    pub fn failed(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.status == Status::Fail)
            .count()
    }
}

/// Checks prerequisites and trust, creates the policy from a template when
/// there's none, and installs the pre-push hook.
pub fn init(cwd: &Path) -> Result<Initialized, Failure> {
    let main = main_checkout(Store::for_repo(cwd)?.dir())?;
    let prerequisites = [
        claude_version(),
        claude_auth(),
        gh_auth(),
        git_version(),
        trust(&main),
    ];
    if let Some(failed) = prerequisites.iter().find(|c| c.status == Status::Fail) {
        return Err(Failure::Refused {
            reason: format!("{}: {}", failed.name, failed.detail),
            fix: "fix it, then run `orchestral init` again; `orchestral doctor` checks everything"
                .into(),
        });
    }

    let policy = main.join(POLICY);
    let created_policy = if policy.exists() {
        None
    } else {
        let dir = policy.parent().expect("the policy is in a directory");
        fs::create_dir_all(dir).map_err(|err| io_error(dir, err))?;
        fs::write(&policy, include_str!("templates/policy.md"))
            .map_err(|err| io_error(&policy, err))?;
        Some(policy)
    };
    let hook = install_pre_push(&main)?;
    let hook_tracked = Command::new("git")
        .args(["ls-files", "--error-unmatch"])
        .arg(&hook)
        .current_dir(&main)
        .output()
        .is_ok_and(|output| output.status.success());
    Ok(Initialized {
        created_policy,
        hook,
        hook_tracked,
    })
}

/// Adds or replaces Orchestral's block in the pre-push hook git runs, in
/// `core.hooksPath` when it's set. The block runs first and hands the refs
/// on stdin to the rest of an existing hook.
fn install_pre_push(main: &Path) -> Result<PathBuf, Failure> {
    let hooks = git_output(
        main,
        &["rev-parse", "--path-format=absolute", "--git-path", "hooks"],
    )?;
    let hooks = PathBuf::from(hooks.trim());
    fs::create_dir_all(&hooks).map_err(|err| io_error(&hooks, err))?;
    let path = hooks.join("pre-push");
    let existing = match fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(io_error(&path, err)),
    };
    if let Some(first) = existing.as_deref().and_then(|text| text.lines().next())
        && first.starts_with("#!")
        && !["sh", "bash", "dash", "zsh"].iter().any(|shell| {
            first.ends_with(&format!("/{shell}")) || first.ends_with(&format!(" {shell}"))
        })
    {
        return Err(Failure::Refused {
            reason: format!("{} isn't a shell script ({first})", path.display()),
            fix: "call `orchestral hook pre-push \"$@\"` from it yourself, passing stdin through, and exit if it fails".into(),
        });
    }
    let text = with_block(existing.as_deref(), &block()?);
    fs::write(&path, text).map_err(|err| io_error(&path, err))?;
    let mut permissions = fs::metadata(&path)
        .map_err(|err| io_error(&path, err))?
        .permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    fs::set_permissions(&path, permissions).map_err(|err| io_error(&path, err))?;
    Ok(path)
}

fn block() -> Result<String, Failure> {
    let exe = std::env::current_exe()
        .map_err(|err| Failure::Error(format!("can't find this binary: {err}")))?;
    let exe = exe.display().to_string().replace('\'', "'\\''");
    Ok(format!(
        "{BLOCK_START}
# Rejects a worker's push to main. Managed by `orchestral init`.
orchestral_refs=$(cat)
if [ -x '{exe}' ]; then
  printf '%s\\n' \"$orchestral_refs\" | '{exe}' hook pre-push \"$@\" || exit 1
else
  echo \"orchestral isn't at {exe} any more; run orchestral init again\" >&2
fi
exec 0<<ORCHESTRAL_REFS
$orchestral_refs
ORCHESTRAL_REFS
{BLOCK_END}
"
    ))
}

/// `hook` with `block` in place of any earlier one, or after its first
/// line when it starts with `#!`.
fn with_block(hook: Option<&str>, block: &str) -> String {
    let Some(hook) = hook else {
        return format!("#!/bin/sh\n{block}");
    };
    if let (Some(start), Some(end)) = (hook.find(BLOCK_START), hook.find(BLOCK_END)) {
        let end = end + BLOCK_END.len();
        let end = if hook[end..].starts_with('\n') {
            end + 1
        } else {
            end
        };
        return format!("{}{block}{}", &hook[..start], &hook[end..]);
    }
    match hook.split_once('\n') {
        Some((first, rest)) if first.starts_with("#!") => format!("{first}\n{block}{rest}"),
        _ => format!("#!/bin/sh\n{block}{hook}"),
    }
}

fn claude_version() -> Check {
    match run("claude", &["--version"]) {
        Some((true, out)) => {
            let version = out.split_whitespace().next().unwrap_or_default().to_owned();
            if version == TESTED_CLAUDE {
                ok("claude", version)
            } else {
                warn(
                    "claude",
                    format!("{version} is untested; Orchestral was tested on {TESTED_CLAUDE}"),
                )
            }
        }
        _ => fail("claude", "not found; install Claude Code"),
    }
}

fn claude_auth() -> Check {
    let logged_in = run("claude", &["auth", "status"])
        .and_then(|(_, out)| serde_json::from_str::<Value>(&out).ok())
        .is_some_and(|status| status["loggedIn"] == true);
    if logged_in {
        ok("auth", "Claude Code is logged in".into())
    } else {
        fail(
            "auth",
            "Claude Code isn't logged in; run `claude auth login`",
        )
    }
}

fn gh_auth() -> Check {
    match run("gh", &["auth", "status"]) {
        Some((true, _)) => ok("gh", "logged in".into()),
        Some((false, _)) => fail("gh", "not logged in; run `gh auth login`"),
        None => fail("gh", "not found; install the GitHub CLI"),
    }
}

fn git_version() -> Check {
    match run("git", &["--version"]) {
        Some((true, out)) => ok("git", out.trim().trim_start_matches("git version ").into()),
        _ => fail("git", "not found; install git"),
    }
}

/// Claude Code won't start a background session in a directory whose trust
/// prompt was never accepted, and a worktree is trusted when its main
/// checkout is.
fn trust(main: &Path) -> Check {
    let config = match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) => PathBuf::from(dir).join(".claude.json"),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".claude.json"),
    };
    let trusted = fs::read_to_string(config)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .is_some_and(|config| {
            config["projects"][main.display().to_string()]["hasTrustDialogAccepted"] == true
        });
    if trusted {
        ok("trust", format!("Claude Code trusts {}", main.display()))
    } else {
        fail(
            "trust",
            &format!(
                "Claude Code doesn't trust {}; run `claude` there once and accept the trust prompt",
                main.display()
            ),
        )
    }
}

fn policy(cwd: &Path, main: &Path) -> Check {
    if let Err(
        Failure::Error(message)
        | Failure::Refused {
            reason: message, ..
        },
    ) = Policy::load(cwd)
    {
        return fail("policy", &message);
    }
    let local = fs::read_to_string(main.join(POLICY)).ok();
    let pushed = git_output(main, &["show", &format!("origin/main:{POLICY}")]).ok();
    match (local, pushed) {
        (Some(local), Some(pushed)) if local != pushed => warn(
            "policy",
            "the local policy differs from origin/main, which is the copy that's used".into(),
        ),
        _ => ok("policy", "valid".into()),
    }
}

fn portless() -> Check {
    match run("portless", &["--version"]) {
        Some((true, out)) => ok("portless", out.trim().into()),
        _ => warn(
            "portless",
            "not found; previews run without stable URLs".into(),
        ),
    }
}

fn tailscale() -> Check {
    let state = run("tailscale", &["status", "--json"]).and_then(|(_, out)| {
        serde_json::from_str::<Value>(&out)
            .ok()
            .and_then(|status| status["BackendState"].as_str().map(String::from))
    });
    match state.as_deref() {
        Some("Running") => ok("tailscale", "running".into()),
        Some(state) => warn(
            "tailscale",
            format!("{state}; preview links won't reach other devices"),
        ),
        None => warn(
            "tailscale",
            "not found; preview links won't reach other devices".into(),
        ),
    }
}

/// Runs a program, giving whether it succeeded and its stdout, or `None`
/// when it can't run.
fn run(program: &str, args: &[&str]) -> Option<(bool, String)> {
    let output = Command::new(program).args(args).output().ok()?;
    Some((
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    ))
}

fn git_output(dir: &Path, args: &[&str]) -> Result<String, Failure> {
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
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn ok(name: &'static str, detail: String) -> Check {
    Check {
        name,
        status: Status::Ok,
        detail,
    }
}

fn warn(name: &'static str, detail: String) -> Check {
    Check {
        name,
        status: Status::Warn,
        detail,
    }
}

fn fail(name: &'static str, detail: &str) -> Check {
    Check {
        name,
        status: Status::Fail,
        detail: detail.into(),
    }
}

fn io_error(path: &Path, err: std::io::Error) -> Failure {
    Failure::Error(format!("{}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: &str = "# >>> orchestral >>>\nnew\n# <<< orchestral <<<\n";

    #[test]
    fn a_new_hook_is_the_block_alone() {
        assert_eq!(with_block(None, BLOCK), format!("#!/bin/sh\n{BLOCK}"));
    }

    #[test]
    fn the_block_goes_first_in_an_existing_hook() {
        let hook = "#!/bin/bash\nmake ready\n";

        assert_eq!(
            with_block(Some(hook), BLOCK),
            format!("#!/bin/bash\n{BLOCK}make ready\n")
        );
    }

    #[test]
    fn installing_again_replaces_the_block() {
        let hook = "#!/bin/sh\n# >>> orchestral >>>\nold\n# <<< orchestral <<<\nmake ready\n";

        assert_eq!(
            with_block(Some(hook), BLOCK),
            format!("#!/bin/sh\n{BLOCK}make ready\n")
        );
    }

    #[test]
    fn the_template_is_a_valid_policy() {
        let policy = Policy::parse(include_str!("templates/policy.md"), false).unwrap();

        assert_eq!(policy.settings.fleet_token_budget, 3_000_000);
    }
}
