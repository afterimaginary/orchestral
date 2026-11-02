mod common;

use common::{Stubs, fixture};

const USAGE_ARGS: [&str; 5] = [
    "-p",
    "/usage",
    "--no-session-persistence",
    "--settings",
    r#"{"disableAllHooks":true}"#,
];

fn limits(stubs: &Stubs, args: &[&str]) -> (i32, String) {
    let output = stubs
        .orchestral()
        .arg("limits")
        .args(args)
        .output()
        .unwrap();
    (
        output.status.code().unwrap(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

#[test]
fn shows_each_window_from_usage() {
    let stubs = Stubs::new();
    stubs.respond(
        "claude",
        &USAGE_ARGS,
        &fixture("claude/2.1.314/usage.txt"),
        0,
    );

    let (code, stdout) = limits(&stubs, &[]);

    assert_eq!(code, 0);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 3, "{stdout}");
    assert!(
        lines[0].starts_with("session: 2% used, resets Nov 2 at 6:49am"),
        "{stdout}"
    );
    assert!(
        lines[1].starts_with("week (all models): 26% used"),
        "{stdout}"
    );
    assert!(lines[2].starts_with("week (Fable): 0% used"), "{stdout}");
    assert_eq!(
        stubs.calls("claude"),
        vec![USAGE_ARGS.map(String::from).to_vec()]
    );
}

#[test]
fn json_lists_the_windows() {
    let stubs = Stubs::new();
    stubs.respond(
        "claude",
        &USAGE_ARGS,
        &fixture("claude/2.1.314/usage.txt"),
        0,
    );

    let (code, stdout) = limits(&stubs, &["--json"]);

    assert_eq!(code, 0);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim_end()).unwrap();
    assert_eq!(parsed["available"], true);
    assert_eq!(parsed["windows"][1]["name"], "week (all models)");
    assert_eq!(parsed["windows"][1]["percent"], 26);
    let resets = parsed["windows"][1]["resets"].as_str().unwrap();
    assert!(
        resets.contains("-11-04T12:59:00-08:00[America/Los_Angeles]"),
        "{resets}"
    );
}

#[test]
fn a_failed_read_is_unavailable_but_succeeds() {
    let stubs = Stubs::new();
    stubs.respond("claude", &USAGE_ARGS, "", 1);

    let (code, stdout) = limits(&stubs, &["--json"]);

    assert_eq!(code, 0);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim_end()).unwrap();
    assert_eq!(parsed["available"], false);
    assert!(
        parsed["reason"].as_str().unwrap().contains("exited"),
        "{stdout}"
    );
}

#[test]
fn output_without_windows_is_unavailable() {
    let stubs = Stubs::new();
    stubs.respond("claude", &USAGE_ARGS, "Not logged in\n", 0);

    let (code, stdout) = limits(&stubs, &[]);

    assert_eq!(code, 0);
    assert_eq!(
        stdout,
        "Plan limits are unavailable: claude -p /usage printed no usage windows.\n"
    );
}
