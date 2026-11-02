//! Running git.

use std::path::Path;
use std::process::Command;

use crate::outcome::Failure;

/// Runs `git args` in `dir` and returns its stdout. A non-zero exit is an
/// error carrying git's stderr.
pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<String, Failure> {
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
