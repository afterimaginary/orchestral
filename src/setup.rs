//! `orchestral init` and `orchestral doctor`: setting a project up, and
//! finding problems before a run rather than during it.

use std::fmt;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;
use serde_json::Value;

use crate::git::git;
use crate::outcome::Failure;
use crate::policy::{self, Files, Policy, TRACKED_PATH};
use crate::spawn::main_checkout;
use crate::state::Store;

/// The Claude Code version the undocumented formats were captured from
/// (`docs/specs/claude-code-internals.md`).
pub const TESTED_CLAUDE: &str = "2.1.314";

const BLOCK_START: &str = "# >>> orchestral >>>";
const BLOCK_END: &str = "# <<< orchestral <<<";

/// Where a marketplace install's build sits in its version's directory.
const BUILD: &str = "target/release/orchestral";

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
    /// What init did about the policy.
    pub policy: PolicySetup,
    pub hook: PathBuf,
    /// Whether git tracks the hook, as with a `core.hooksPath` in the
    /// repository.
    pub hook_tracked: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum PolicySetup {
    /// Wrote the template to `path`.
    Created { path: PathBuf },
    /// Moved the repository's tracked policy to `path`.
    Moved { path: PathBuf },
    /// Left the existing policy files alone.
    Kept { paths: Vec<PathBuf> },
}

impl fmt::Display for Initialized {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self.policy {
            PolicySetup::Created { path } => writeln!(
                f,
                "Created {}. Set fleet_token_budget there.",
                path.display()
            )?,
            PolicySetup::Moved { path } => writeln!(
                f,
                "Moved the repository's {TRACKED_PATH} to {}. Remove it from the repository with `git rm {TRACKED_PATH}`, and push.",
                path.display()
            )?,
            PolicySetup::Kept { paths } => {
                let paths: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
                writeln!(f, "Kept the policy in {}.", paths.join(" and "))?
            }
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
            checks.push(policy(cwd));
            checks.push(hook(&main));
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

    let policy = set_up_policy(&main)?;
    let hook = install_pre_push(&main)?;
    let hook_tracked = Command::new("git")
        .args(["ls-files", "--error-unmatch"])
        .arg(&hook)
        .current_dir(&main)
        .output()
        .is_ok_and(|output| output.status.success());
    Ok(Initialized {
        policy,
        hook,
        hook_tracked,
    })
}

/// Moves a tracked policy into the config directory when the repository
/// has one and the target file doesn't exist yet, and otherwise creates the
/// template when there's no policy file at all.
fn set_up_policy(main: &Path) -> Result<PolicySetup, Failure> {
    let files = Files::for_repo(&policy::config_dir()?, main)?;
    let target = files.target().to_owned();
    let write = |text: &str| {
        let dir = target.parent().expect("the policy is in a directory");
        fs::create_dir_all(dir).map_err(|err| io_error(dir, err))?;
        fs::write(&target, text).map_err(|err| io_error(&target, err))
    };
    if !target.exists()
        && let Some(text) = policy::tracked(main, TRACKED_PATH)?
    {
        write(&text)?;
        return Ok(PolicySetup::Moved { path: target });
    }
    let existing = files.existing();
    if existing.is_empty() {
        write(include_str!("templates/policy.md"))?;
        return Ok(PolicySetup::Created { path: target });
    }
    Ok(PolicySetup::Kept { paths: existing })
}

/// Adds or replaces Orchestral's block in the pre-push hook git runs, in
/// `core.hooksPath` when it's set. The block runs first and hands the refs
/// on stdin to the rest of an existing hook.
fn install_pre_push(main: &Path) -> Result<PathBuf, Failure> {
    let hooks = git(
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
    let newest = match versions_dir(&exe) {
        Some(versions) => format!(
            "# A plugin update installs its build next to this one, so run the newest.
orchestral_versions='{}'
for orchestral_copy in \"$orchestral_versions\"/*/{BUILD}; do
  if [ -x \"$orchestral_copy\" ] && {{ [ ! -x \"$orchestral_bin\" ] || [ \"$orchestral_copy\" -nt \"$orchestral_bin\" ]; }}; then
    orchestral_bin=$orchestral_copy
  fi
done
",
            quoted(versions)
        ),
        None => String::new(),
    };
    let exe = quoted(&exe);
    Ok(format!(
        "{BLOCK_START}
# Rejects a worker's push to main. Managed by `orchestral init`.
orchestral_refs=$(cat)
orchestral_bin='{exe}'
{newest}if [ -x \"$orchestral_bin\" ]; then
  printf '%s\\n' \"$orchestral_refs\" | \"$orchestral_bin\" hook pre-push \"$@\" || exit 1
else
  echo \"orchestral isn't at $orchestral_bin any more; run orchestral init again\" >&2
  # Without the binary, still reject a worker's push to main, as
  # guard::pre_push does.
  if [ -n \"$ORCHESTRAL_WORKER\" ] && printf '%s\\n' \"$orchestral_refs\" |
    awk '$3 == \"refs/heads/main\" {{ found = 1 }} END {{ exit !found }}'; then
    echo \"workers can't push to main; push your branch and open a PR\" >&2
    exit 1
  fi
fi
exec 0<<ORCHESTRAL_REFS
$orchestral_refs
ORCHESTRAL_REFS
{BLOCK_END}
"
    ))
}

/// The directory holding every installed version of the plugin, when `exe`
/// is a marketplace install's build, at
/// `plugins/cache/<marketplace>/<plugin>/<version>/target/release/orchestral`
/// (ADR-0012).
fn versions_dir(exe: &Path) -> Option<&Path> {
    let versions = exe.ancestors().nth(4)?;
    let cache = versions.ancestors().nth(2)?;
    (exe.ends_with(BUILD) && cache.ends_with("plugins/cache")).then_some(versions)
}

/// `path` for use inside single quotes in the hook.
fn quoted(path: &Path) -> String {
    path.display().to_string().replace('\'', "'\\''")
}

/// The binary the hook runs: `exe`, or a build in another version's
/// directory under `versions` that's newer, as the hook's loop picks.
fn newest(exe: PathBuf, versions: Option<&Path>) -> Option<PathBuf> {
    let executable = |path: &Path| {
        fs::metadata(path)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    };
    let modified = |path: &Path| fs::metadata(path).and_then(|meta| meta.modified()).ok();
    let mut copies: Vec<PathBuf> = versions
        .and_then(|versions| fs::read_dir(versions).ok())
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok().map(|entry| entry.path().join(BUILD)))
        .collect();
    copies.sort();
    let mut bin = executable(&exe).then_some(exe);
    for copy in copies.into_iter().filter(|copy| executable(copy)) {
        if bin
            .as_deref()
            .is_none_or(|bin| modified(&copy) > modified(bin))
        {
            bin = Some(copy);
        }
    }
    bin
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

fn policy(cwd: &Path) -> Check {
    let policy = match Policy::load(cwd) {
        Ok(policy) => policy,
        Err(
            Failure::Error(message)
            | Failure::Refused {
                reason: message, ..
            },
        ) => return fail("policy", &message),
    };
    let sources: Vec<String> = policy
        .sources
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    let sources = sources.join(" and ");
    if matches!(policy::tracked(cwd, TRACKED_PATH), Ok(Some(_))) {
        return warn(
            "policy",
            format!(
                "valid, from {sources}; the repository's {TRACKED_PATH} isn't read, so remove it"
            ),
        );
    }
    ok("policy", format!("valid, from {sources}"))
}

/// Whether the pre-push hook has Orchestral's block, and a binary it calls
/// is still there. A plugin update moves the binary, and the hook follows a
/// marketplace install to its newest version.
fn hook(main: &Path) -> Check {
    let hook = git(
        main,
        &["rev-parse", "--path-format=absolute", "--git-path", "hooks"],
    )
    .map(|hooks| PathBuf::from(hooks.trim()).join("pre-push"));
    let text = hook.ok().and_then(|hook| fs::read_to_string(hook).ok());
    let Some(block) = text
        .as_deref()
        .and_then(|text| text.find(BLOCK_START).map(|start| &text[start..]))
    else {
        return warn(
            "hook",
            "the pre-push hook isn't installed; run `orchestral init`".into(),
        );
    };
    let Some(exe) = assigned(block, "orchestral_bin") else {
        return warn(
            "hook",
            "the pre-push hook is out of date; run `orchestral init` again".into(),
        );
    };
    let versions = assigned(block, "orchestral_versions").map(PathBuf::from);
    match newest(PathBuf::from(exe), versions.as_deref()) {
        Some(bin) => ok("hook", format!("calls {}", bin.display())),
        None => warn(
            "hook",
            "the pre-push hook calls a binary that has moved; run `orchestral init` again".into(),
        ),
    }
}

/// The single-quoted value the hook's `block` assigns to `name`.
fn assigned(block: &str, name: &str) -> Option<String> {
    let (_, rest) = block.split_once(&format!("\n{name}='"))?;
    let (value, _) = rest.split_once("'\n")?;
    Some(value.replace("'\\''", "'"))
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
    fn only_a_marketplace_install_has_a_versions_directory() {
        let installed = Path::new(
            "/home/dev/.claude/plugins/cache/market/orchestral/abc123/target/release/orchestral",
        );
        assert_eq!(
            versions_dir(installed),
            Some(Path::new(
                "/home/dev/.claude/plugins/cache/market/orchestral"
            ))
        );
        let checkout = Path::new("/home/dev/src/orchestral/target/release/orchestral");
        assert_eq!(versions_dir(checkout), None);
        let debug = Path::new(
            "/home/dev/.claude/plugins/cache/market/orchestral/abc123/target/debug/orchestral",
        );
        assert_eq!(versions_dir(debug), None);
    }

    #[test]
    fn the_template_is_a_valid_policy() {
        let policy = Policy::parse(include_str!("templates/policy.md"), false).unwrap();

        assert_eq!(policy.settings.fleet_token_budget, 3_000_000);
    }
}
