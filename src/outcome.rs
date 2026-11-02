//! How every command reports its result: data on stdout, failures on stderr,
//! and an exit code that tells an error from a refusal.

use std::fmt::Display;
use std::io::Write;
use std::process::ExitCode;

use serde::Serialize;

/// Why a command didn't succeed.
#[derive(Debug)]
pub enum Failure {
    /// Something went wrong. Exit code 1.
    Error(String),
    /// A rule stopped the command. Exit code 2.
    // No command refuses yet; the first ones arrive with spawn and claim.
    #[allow(dead_code)]
    Refused {
        reason: String,
        /// What would make the command succeed.
        fix: String,
    },
}

impl Failure {
    fn exit_code(&self) -> u8 {
        match self {
            Failure::Error(_) => 1,
            Failure::Refused { .. } => 2,
        }
    }

    fn write_to(&self, stderr: &mut impl Write) -> std::io::Result<()> {
        match self {
            Failure::Error(message) => writeln!(stderr, "error: {message}"),
            Failure::Refused { reason, fix } => {
                writeln!(stderr, "refused: {reason}")?;
                writeln!(stderr, "fix: {fix}")
            }
        }
    }
}

/// Prints a command's data: as one line of JSON with `--json`, or as text.
pub fn print<T: Serialize + Display>(data: &T, json: bool) -> Result<(), Failure> {
    let text = if json {
        serde_json::to_string(data).map_err(|err| Failure::Error(err.to_string()))?
    } else {
        data.to_string()
    };
    println!("{text}");
    Ok(())
}

/// Turns a command's result into its exit code, writing any failure to stderr.
pub fn finish(result: Result<(), Failure>) -> ExitCode {
    finish_to(result, &mut std::io::stderr())
}

fn finish_to(result: Result<(), Failure>, stderr: &mut impl Write) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            // If stderr itself fails there's nowhere left to report it.
            let _ = failure.write_to(stderr);
            ExitCode::from(failure.exit_code())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(result: Result<(), Failure>) -> (ExitCode, String) {
        let mut stderr = Vec::new();
        let code = finish_to(result, &mut stderr);
        (code, String::from_utf8(stderr).unwrap())
    }

    #[test]
    fn success_exits_zero_silently() {
        assert_eq!(run(Ok(())), (ExitCode::SUCCESS, String::new()));
    }

    #[test]
    fn an_error_exits_one_with_its_message() {
        let failure = Failure::Error("state file is corrupt".into());
        assert_eq!(
            run(Err(failure)),
            (ExitCode::from(1), "error: state file is corrupt\n".into())
        );
    }

    #[test]
    fn a_refusal_exits_two_with_its_reason_and_fix() {
        let failure = Failure::Refused {
            reason: "the fleet is paused".into(),
            fix: "run `orchestral unpause`".into(),
        };
        assert_eq!(
            run(Err(failure)),
            (
                ExitCode::from(2),
                "refused: the fleet is paused\nfix: run `orchestral unpause`\n".into()
            )
        );
    }
}
