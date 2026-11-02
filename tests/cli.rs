use std::process::{Command, Output};

fn orchestral(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orchestral"))
        .args(args)
        .output()
        .expect("the binary should run")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn prints_its_version() {
    let output = orchestral(&["--version"]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        stdout(&output),
        concat!("orchestral ", env!("CARGO_PKG_VERSION"), "\n")
    );
}

#[test]
fn a_usage_error_exits_one_not_two() {
    // Exit code 2 is reserved for refusals by a rule.
    let output = orchestral(&["limits", "--no-such-flag"]);

    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).starts_with("error:"));
}

#[test]
fn a_missing_command_exits_one() {
    let output = orchestral(&[]);

    assert_eq!(output.status.code(), Some(1));
}
