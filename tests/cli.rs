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
fn limits_reports_them_unavailable() {
    let output = orchestral(&["limits"]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        stdout(&output),
        "Plan limits are unavailable: reading them isn't implemented yet.\n"
    );
}

#[test]
fn limits_json_is_one_json_line() {
    let output = orchestral(&["limits", "--json"]);

    assert_eq!(output.status.code(), Some(0));
    let parsed: serde_json::Value =
        serde_json::from_str(stdout(&output).trim_end()).expect("stdout should be JSON");
    assert_eq!(parsed["available"], false);
    assert!(parsed["reason"].is_string());
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
