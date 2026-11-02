//! Fleet token totals from 2.1.314 fixture transcripts.

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use orchestral::budget::{self, Estimate};

const WORKER: &str = "00000000-0000-4000-8000-00000000b001";
const OTHER: &str = "00000000-0000-4000-8000-00000000b002";

fn transcript(id: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude/2.1.314/fleet")
        .join(format!("{id}.jsonl"))
}

fn at(time: &str) -> Option<Timestamp> {
    Some(format!("2026-11-02T{time}Z").parse().unwrap())
}

fn total(ids: &[&str], since: Option<Timestamp>) -> u64 {
    let paths: Vec<PathBuf> = ids.iter().map(|id| transcript(id)).collect();
    budget::fleet_usage(&paths, since).unwrap().estimated_tokens
}

// The responses in the fixtures, weighted:
// msg_01A, in two entries: 3 + 100 + 20,000 × 2 (1-hour writes) = 40,103
// msg_01B: 2 + 50 + 500 × 2 + 20,003 × 0.1 = 3,052.3
// msg_01Sub, a subagent: 4 + 40 + 8,000 × 1.25 (5-minute writes) = 10,044
// msg_01O, another session: 1 + 10 + 30,000 × 0.1 = 3,011
// msg_01Early comes before the run, and the API error has no usage.

#[test]
fn sums_every_session_and_subagent_since_the_run_started() {
    assert_eq!(total(&[WORKER, OTHER], at("10:00:00")), 56_210);
}

#[test]
fn leaves_out_responses_before_the_run_started() {
    // msg_01A's entries are at 10:00:05 and 10:00:06.
    assert_eq!(total(&[WORKER, OTHER], at("10:00:10")), 16_107);
}

#[test]
fn a_response_that_started_before_the_run_adds_nothing() {
    assert_eq!(total(&[WORKER, OTHER], at("10:00:06")), 16_107);
}

#[test]
fn before_a_run_the_total_is_zero() {
    assert_eq!(
        budget::fleet_usage(&[transcript(WORKER)], None).unwrap(),
        Estimate::default()
    );
}

#[test]
fn a_session_without_a_transcript_yet_adds_nothing() {
    assert_eq!(total(&[OTHER, "not-started"], at("10:00:00")), 3_011);
}
