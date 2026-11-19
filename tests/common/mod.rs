//! Integration test harness: runs commands with stub `claude`, `gh`,
//! `portless` and `tailscale` on `PATH`, so tests run offline.
//!
//! Each stub records its calls and replies from rules a test sets with
//! [`Stubs::respond`]. A call no rule matches fails with exit 99.

#![allow(dead_code)] // Each test file uses a different part of the harness.

use std::cell::Cell;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// The external tools the binary calls.
pub const TOOLS: [&str; 4] = ["claude", "gh", "portless", "tailscale"];

/// Only system directories follow the stubs on `PATH`, so a real tool can't
/// be reached by accident.
const SYSTEM_PATH: &str = "/usr/bin:/bin";

const UNIT: char = '\u{1f}';
const RECORD: char = '\u{1e}';

pub struct Stubs {
    dir: TempDir,
    rules: Cell<usize>,
    /// `XDG_CONFIG_HOME` for the commands it runs.
    config_home: PathBuf,
}

impl Stubs {
    pub fn new() -> Self {
        let dir = TempDir::new().expect("a temp dir for stubs");
        let script = fs::read(manifest_path("tests/stubs/stub.sh")).expect("the stub script");
        for tool in TOOLS {
            let path = dir.path().join(tool);
            fs::write(&path, &script).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::create_dir(dir.path().join("calls")).unwrap();
        fs::create_dir(dir.path().join("rules")).unwrap();
        let config_home = dir.path().join("config");
        Stubs {
            dir,
            rules: Cell::new(0),
            config_home,
        }
    }

    /// Makes `tool` print `stdout` and exit with `code` when its arguments
    /// start with `args`. Rules are tried in the order they were added.
    pub fn respond(&self, tool: &str, args: &[&str], stdout: &str, code: i32) {
        let n = self.rules.get();
        self.rules.set(n + 1);
        let rule = |ext: &str| self.dir.path().join(format!("rules/{tool}.{n:04}.{ext}"));
        let prefix: String = args.iter().map(|arg| format!("{arg}{UNIT}")).collect();
        fs::write(rule("args"), prefix).unwrap();
        fs::write(rule("stdout"), stdout).unwrap();
        fs::write(rule("code"), code.to_string()).unwrap();
    }

    /// Uses `home` as `XDG_CONFIG_HOME`, so several stubs can share one
    /// policy.
    pub fn with_config_home(mut self, home: &Path) -> Self {
        self.config_home = home.to_owned();
        self
    }

    /// Orchestral's config directory for commands this harness runs, kept
    /// out of the real one through `XDG_CONFIG_HOME`.
    pub fn config(&self) -> PathBuf {
        self.config_home.join("orchestral")
    }

    /// Writes the global policy file.
    pub fn write_policy(&self, text: &str) {
        write_policy_in(&self.config_home, text);
    }

    /// The directory holding the stubs.
    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// Every call `tool` received, in order, as its arguments.
    pub fn calls(&self, tool: &str) -> Vec<Vec<String>> {
        let log = fs::read_to_string(self.dir.path().join("calls").join(tool)).unwrap_or_default();
        log.split_terminator(RECORD)
            .map(|call| call.split_terminator(UNIT).map(String::from).collect())
            .collect()
    }

    /// A command for `program` that finds the stubs first on `PATH`.
    pub fn command(&self, program: impl AsRef<Path>) -> Command {
        let mut command = Command::new(program.as_ref());
        command.env(
            "PATH",
            format!("{}:{SYSTEM_PATH}", self.dir.path().display()),
        );
        command.env("XDG_CONFIG_HOME", &self.config_home);
        command
    }

    /// The `orchestral` binary, with the stubs on `PATH`.
    pub fn orchestral(&self) -> Command {
        self.command(env!("CARGO_BIN_EXE_orchestral"))
    }
}

/// Writes the global policy file under the `XDG_CONFIG_HOME` `home`.
pub fn write_policy_in(home: &Path, text: &str) {
    let path = home.join("orchestral/policy.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// Reads a fixture from `tests/fixtures/`. Output captured from Claude Code is
/// stored under the version it came from, as `claude/<version>/<name>`.
pub fn fixture(path: &str) -> String {
    let full = manifest_path("tests/fixtures").join(path);
    fs::read_to_string(&full).unwrap_or_else(|err| panic!("fixture {}: {err}", full.display()))
}

fn manifest_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}
