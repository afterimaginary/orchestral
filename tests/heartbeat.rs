//! The idle heartbeat's timing, with an injected clock, and its effect on
//! the cache the transcript reader reports.

use std::fs;
use std::io::Write;
use std::path::Path;

use jiff::{SignedDuration, Timestamp};
use orchestral::heartbeat;
use orchestral::state::{Orchestrator, State, Store};
use orchestral::transcript;
use tempfile::TempDir;

fn at(time: &str) -> Timestamp {
    format!("2026-11-02T{time}Z").parse().unwrap()
}

fn claimed(store: &Store) -> State {
    store
        .update(|state| {
            state.orchestrator = Some(Orchestrator {
                session_id: "00000000-0000-4000-8000-0000000000c1".into(),
                claimed_at: "2026-11-02T10:00:00Z".into(),
            });
        })
        .unwrap();
    store.read().unwrap()
}

fn event_at(store: &Store, time: &str) {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(store.dir().join("events.jsonl"))
        .unwrap();
    writeln!(
        file,
        r#"{{"seq":1,"ts":"2026-11-02T{time}Z","kind":"worker","text":"issue-12 is idle"}}"#
    )
    .unwrap();
}

#[test]
fn no_heartbeat_without_a_claim() {
    let dir = TempDir::new().unwrap();
    let store = Store::at(dir.path().to_path_buf());
    let state = store.read().unwrap();
    assert!(!heartbeat::due(&store, &state, at("23:00:00")).unwrap());
}

#[test]
fn due_55_minutes_after_the_claim_when_nothing_happened() {
    let dir = TempDir::new().unwrap();
    let store = Store::at(dir.path().to_path_buf());
    let state = claimed(&store);
    assert!(!heartbeat::due(&store, &state, at("10:54:59")).unwrap());
    assert!(heartbeat::due(&store, &state, at("10:55:00")).unwrap());
}

#[test]
fn any_event_puts_it_off() {
    let dir = TempDir::new().unwrap();
    let store = Store::at(dir.path().to_path_buf());
    let state = claimed(&store);
    event_at(&store, "10:30:00");
    assert!(!heartbeat::due(&store, &state, at("11:24:59")).unwrap());
    assert!(heartbeat::due(&store, &state, at("11:25:00")).unwrap());
    // A heartbeat is an event too, so the next is 55 minutes after it.
    event_at(&store, "11:25:00");
    assert!(!heartbeat::due(&store, &state, at("12:19:59")).unwrap());
    assert!(heartbeat::due(&store, &state, at("12:20:00")).unwrap());
}

#[test]
fn a_torn_last_line_falls_back_to_the_event_before() {
    let dir = TempDir::new().unwrap();
    let store = Store::at(dir.path().to_path_buf());
    let state = claimed(&store);
    event_at(&store, "10:30:00");
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(store.dir().join("events.jsonl"))
        .unwrap();
    write!(file, r#"{{"seq":2,"ts":"2026-11-02T11:2"#).unwrap();
    assert!(heartbeat::due(&store, &state, at("11:25:00")).unwrap());
}

#[test]
fn the_line_reports_the_fleet() {
    assert_eq!(
        heartbeat::line(3, Some(41.2), Some((2_100_000, 3_000_000))),
        "heartbeat: 3 workers open, your context 41%, budget about 2.1M of 3.0M; no action needed"
    );
    assert_eq!(
        heartbeat::line(1, None, None),
        "heartbeat: 1 worker open; no action needed"
    );
}

/// A session's last request was at 10:00:07, with a 1-hour cache. A
/// heartbeat turn 55 minutes later keeps the cache warm past 11:00:07.
#[test]
fn a_heartbeat_inside_the_cache_ttl_keeps_the_cache_warm() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude/2.1.314/transcripts/basic.jsonl");
    let later = at("11:50:00");
    assert!(!transcript::read(&fixture, later, None).unwrap().warm);

    let beat = at("10:00:07") + heartbeat::INTERVAL;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("beat.jsonl");
    let mut text = fs::read_to_string(&fixture).unwrap();
    text.push_str(&format!(
        r#"{{"parentUuid":null,"isSidechain":false,"type":"user","uuid":"00000000-0000-4000-8000-000000000009","timestamp":"{beat}","sessionId":"00000000-0000-4000-8000-000000000001","message":{{"role":"user","content":"<task-notification>heartbeat: 1 worker open; no action needed</task-notification>"}}}}
{{"parentUuid":null,"isSidechain":false,"type":"assistant","uuid":"00000000-0000-4000-8000-00000000000a","timestamp":"{}","sessionId":"00000000-0000-4000-8000-000000000001","version":"2.1.314","message":{{"model":"claude-opus-5-5","id":"msg_01C","type":"message","role":"assistant","stop_reason":"end_turn","usage":{{"input_tokens":3,"cache_creation_input_tokens":60,"cache_read_input_tokens":20503,"output_tokens":12,"cache_creation":{{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":60}}}},"content":[{{"type":"text","text":"No action needed."}}]}}}}
"#,
        beat + SignedDuration::from_secs(3)
    ));
    fs::write(&path, text).unwrap();
    let context = transcript::read(&path, later, None).unwrap();
    assert_eq!(context.last_request_at, Some(beat));
    assert_eq!(context.cache_ttl, Some(3600));
    assert!(context.warm);
}
