//! Session records and `claude agents --json`, from 2.1.314 fixtures.

mod common;

use std::path::Path;

use common::{Stubs, fixture};
use orchestral::sessions::{self, Activity, Process, Record};

const NOW_MS: i64 = 1_793_612_000_000;

fn records() -> Vec<Record> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude/2.1.314/sessions");
    sessions::read_records(&dir).unwrap()
}

fn record(pid: u32) -> Record {
    records().into_iter().find(|r| r.pid == pid).unwrap()
}

/// A process that started `seconds_ago` before `NOW_MS`, as `ps` shows it.
fn process(started: &str, seconds_ago: i64) -> Process {
    Process {
        elapsed: seconds_ago,
        started: started.into(),
    }
}

#[test]
fn reads_every_record_and_skips_a_half_written_one() {
    let records = records();

    let names: Vec<_> = records.iter().map(|r| r.name.as_deref().unwrap()).collect();
    assert_eq!(names, ["app-issue-12", "app-issue-13", "app-orchestrator"]);
    assert_eq!(records[1].status.as_deref(), Some("waiting"));
    assert_eq!(records[2].kind.as_deref(), Some("interactive"));
}

#[test]
fn a_missing_directory_has_no_records() {
    assert!(
        sessions::read_records(Path::new("/nonexistent/sessions"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_live_pid_with_a_matching_start_is_alive() {
    let record = record(41001);

    // ps pads a single-digit day with a space, like the record.
    let live = process("Mon Nov  2 08:35:48 2026", 3452);

    assert!(sessions::is_alive(&record, Some(&live), NOW_MS));
}

#[test]
fn a_dead_pid_is_not_alive() {
    assert!(!sessions::is_alive(&record(41001), None, NOW_MS));
}

#[test]
fn a_reused_pid_is_not_alive() {
    let record = record(41001);

    let reused = process("Mon Nov  2 09:30:00 2026", 3);

    assert!(!sessions::is_alive(&record, Some(&reused), NOW_MS));
}

#[test]
fn without_proc_start_the_start_time_is_compared_within_30_seconds() {
    let mut record = record(41002);
    record.proc_start = None;
    let started_ago = (NOW_MS - record.started_at.unwrap()) / 1000;

    let close = process("", started_ago + 20);
    let reused = process("", started_ago - 600);

    assert!(sessions::is_alive(&record, Some(&close), NOW_MS));
    assert!(!sessions::is_alive(&record, Some(&reused), NOW_MS));
}

#[test]
fn this_process_is_alive_by_its_own_ps_entry() {
    let pid = std::process::id();
    let me = sessions::process(pid)
        .unwrap()
        .expect("this process exists");
    let record = Record {
        pid,
        session_id: "self".into(),
        cwd: ".".into(),
        name: None,
        kind: None,
        status: None,
        proc_start: Some(me.started.clone()),
        started_at: None,
        messaging_socket_path: None,
        waiting_for: None,
    };

    let now = sessions::process(pid).unwrap();

    assert!(sessions::is_alive(&record, now.as_ref(), NOW_MS));
}

#[test]
fn waiting_for_is_preferred_to_state() {
    let agents = sessions::parse_agents(&fixture("claude/2.1.314/agents.json")).unwrap();
    let activity: Vec<_> = agents.iter().map(|agent| agent.activity()).collect();

    assert_eq!(
        activity,
        [
            Activity::Done,
            Activity::Waiting("input needed".into()),
            Activity::Unknown,
            Activity::Working,
        ]
    );
}

#[test]
fn agents_runs_claude_agents_json() {
    let stubs = Stubs::new();
    stubs.respond(
        "claude",
        &["agents"],
        &fixture("claude/2.1.314/agents.json"),
        0,
    );

    let agents = sessions::agents(&stubs.dir().join("claude")).unwrap();

    assert_eq!(agents.len(), 4);
    assert_eq!(
        stubs.calls("claude"),
        vec![vec!["agents", "--json", "--all"]]
    );
}

#[test]
fn a_failing_claude_agents_is_an_error() {
    let stubs = Stubs::new();
    stubs.respond("claude", &["agents"], "", 1);

    assert!(sessions::agents(&stubs.dir().join("claude")).is_err());
}
