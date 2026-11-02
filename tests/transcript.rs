//! Context reading from transcript fixtures in the Claude Code 2.1.314 format.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use orchestral::transcript::{self, Compaction, Context};
use tempfile::TempDir;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude/2.1.314/transcripts")
        .join(name)
}

fn at(time: &str) -> Timestamp {
    format!("2026-11-02T{time}Z").parse().unwrap()
}

fn read(name: &str, now: &str, organization: Option<&str>) -> Context {
    transcript::read(&fixture(name), at(now), organization).unwrap()
}

#[test]
fn reads_the_last_real_response() {
    let context = read("basic.jsonl", "10:10:00", Some("claude_max"));

    assert_eq!(
        context,
        Context {
            // From msg_01B, skipping the zero-usage API error after it.
            tokens: 2 + 500 + 20_003,
            model: Some("claude-opus-5-5".into()),
            identity: Some("claude-opus-5-5".into()),
            window: Some(1_000_000),
            percent: Some(2.0505),
            // The tool result before msg_01B, not the queue operation after it.
            last_request_at: Some(at("10:00:07")),
            compactions: vec![],
            cache_ttl: Some(3600),
            warm: true,
        }
    );
}

#[test]
fn the_cache_goes_cold_after_its_ttl() {
    assert!(!read("basic.jsonl", "11:00:08", None).warm);
    assert!(read("haiku.jsonl", "10:05:00", None).warm);
    assert!(!read("haiku.jsonl", "10:06:00", None).warm);
}

#[test]
fn a_five_minute_cache_and_a_standard_window() {
    let context = read("haiku.jsonl", "10:01:00", None);

    assert_eq!(context.cache_ttl, Some(300));
    assert_eq!(context.window, Some(200_000));
}

#[test]
fn a_compaction_after_the_last_response_replaces_the_context() {
    let context = read("compacted.jsonl", "10:05:30", None);

    assert_eq!(context.tokens, 30_000);
    assert_eq!(
        context.compactions,
        [Compaction {
            at: Some(at("10:05:00")),
            post_tokens: 30_000
        }]
    );
    assert!(!context.warm, "nothing is cached after a compaction");
}

#[test]
fn iterations_use_the_last_message_iteration() {
    let context = read("iterations.jsonl", "10:01:00", None);

    assert_eq!(context.tokens, 5 + 1000 + 60_000);
}

#[test]
fn sidechain_entries_are_skipped() {
    assert_eq!(read("sidechain.jsonl", "10:01:00", None).tokens, 5000);
}

#[test]
fn an_identity_that_no_longer_matches_the_model_is_ignored() {
    let context = read("identity-changed.jsonl", "10:01:00", None);

    assert_eq!(context.model.as_deref(), Some("claude-sonnet-5-5"));
    assert_eq!(context.identity, None);
}

#[test]
fn percentages_are_not_capped() {
    // 1M of context in use with an identity that only says 200K would be
    // read as 1M, so test the cap with Haiku's fixed window instead.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("big.jsonl");
    let text = fs::read_to_string(fixture("haiku.jsonl"))
        .unwrap()
        .replace("39409", "250000");
    fs::write(&path, text).unwrap();

    let context = transcript::read(&path, at("10:01:00"), None).unwrap();

    assert_eq!(context.percent, Some(125.0015));
}

#[test]
fn a_large_file_is_read_from_its_tail_and_head() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("large.jsonl");
    let basic = fs::read_to_string(fixture("basic.jsonl")).unwrap();
    let (head, tail) = basic.split_once('\n').unwrap();
    let mut file = fs::File::create(&path).unwrap();
    // The identity, then over 1 MiB of other entries, then the rest.
    writeln!(file, "{head}").unwrap();
    let filler = format!(
        r#"{{"type":"queue-operation","operation":"enqueue","timestamp":"2026-11-02T09:00:00Z","content":"{}"}}"#,
        "x".repeat(1000)
    );
    for _ in 0..1200 {
        writeln!(file, "{filler}").unwrap();
    }
    write!(file, "{tail}").unwrap();
    drop(file);
    assert!(fs::metadata(&path).unwrap().len() > 1024 * 1024);

    let context = transcript::read(&path, at("10:10:00"), Some("claude_max")).unwrap();

    assert_eq!(context.tokens, 20_505);
    assert_eq!(context.identity.as_deref(), Some("claude-opus-5-5"));
}

#[test]
fn finds_a_transcript_by_the_path_rule_or_by_scanning() {
    let projects = TempDir::new().unwrap();
    let ruled = projects.path().join("-home-dev-app");
    let shortened = projects.path().join("-home-dev-a-very-long-path-cut-short");
    fs::create_dir_all(&ruled).unwrap();
    fs::create_dir_all(&shortened).unwrap();
    fs::write(ruled.join("one.jsonl"), "").unwrap();
    fs::write(shortened.join("two.jsonl"), "").unwrap();

    let find = |id| transcript::find(projects.path(), Path::new("/home/dev/app"), id);

    assert_eq!(find("one"), Some(ruled.join("one.jsonl")));
    assert_eq!(find("two"), Some(shortened.join("two.jsonl")));
    assert_eq!(find("three"), None);
}
