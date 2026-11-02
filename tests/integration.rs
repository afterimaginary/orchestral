//! Tests that drive stubbed external tools. `make verify` runs these, and
//! `make ready` leaves them out.

mod common;

use common::{Stubs, fixture};

#[test]
fn a_stub_replies_from_a_fixture_and_records_its_calls() {
    let stubs = Stubs::new();
    stubs.respond(
        "claude",
        &["--version"],
        &fixture("claude/2.1.314/version.txt"),
        0,
    );

    let output = stubs.command("claude").arg("--version").output().unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "2.1.314 (Claude Code)\n"
    );
    assert_eq!(stubs.calls("claude"), vec![vec!["--version"]]);
}

#[test]
fn arguments_keep_their_spaces_and_newlines() {
    let stubs = Stubs::new();
    stubs.respond("claude", &["--bg"], "", 0);

    stubs
        .command("claude")
        .args(["--bg", "--", "Work on issue #12.\nThen stop."])
        .status()
        .unwrap();

    assert_eq!(
        stubs.calls("claude"),
        vec![vec!["--bg", "--", "Work on issue #12.\nThen stop."]]
    );
}

#[test]
fn the_first_rule_whose_arguments_match_wins() {
    let stubs = Stubs::new();
    stubs.respond("gh", &["pr", "view"], "view\n", 0);
    stubs.respond("gh", &["pr"], "any pr\n", 3);

    let view = stubs
        .command("gh")
        .args(["pr", "view", "7"])
        .output()
        .unwrap();
    let list = stubs.command("gh").args(["pr", "list"]).output().unwrap();

    assert_eq!(String::from_utf8_lossy(&view.stdout), "view\n");
    assert_eq!(list.status.code(), Some(3));
    assert_eq!(String::from_utf8_lossy(&list.stdout), "any pr\n");
}

#[test]
fn a_call_without_a_canned_reply_fails_loudly() {
    let stubs = Stubs::new();

    let output = stubs.command("tailscale").arg("status").output().unwrap();

    assert_eq!(output.status.code(), Some(99));
    assert!(String::from_utf8_lossy(&output.stderr).contains("no canned reply for: status"));
    assert_eq!(stubs.calls("tailscale"), vec![vec!["status"]]);
}

#[test]
fn every_tool_resolves_to_its_stub() {
    let stubs = Stubs::new();

    for tool in common::TOOLS {
        let output = stubs
            .command("sh")
            .args(["-c", &format!("command -v {tool}")])
            .output()
            .unwrap();

        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim_end(),
            stubs.dir().join(tool).to_str().unwrap()
        );
    }
}

#[test]
fn the_binary_runs_offline_with_the_stubs() {
    let stubs = Stubs::new();

    let output = stubs.orchestral().arg("limits").output().unwrap();

    // `limits` runs `claude`, which reaches the stub, and nothing else.
    assert_eq!(output.status.code(), Some(0));
    for tool in common::TOOLS {
        let expected = usize::from(tool == "claude");
        assert_eq!(stubs.calls(tool).len(), expected, "{tool} calls");
    }
}
