//! Winding down at plan limits and the budget, with limits built by hand
//! and an injected clock.

use std::process::Command;

use jiff::{Timestamp, Zoned};
use orchestral::budget::Estimate;
use orchestral::limits::{Limits, Window};
use orchestral::pace;
use orchestral::state::Store;
use tempfile::TempDir;

const LIMIT: u8 = 85;

fn store() -> (TempDir, Store) {
    let dir = TempDir::new().unwrap();
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    let store = Store::for_repo(dir.path()).unwrap();
    (dir, store)
}

fn at(time: &str) -> Timestamp {
    time.parse().unwrap()
}

/// The session window at `percent`, resetting at 18:00 UTC.
fn session(percent: u8) -> Limits {
    Limits {
        available: true,
        reason: None,
        windows: vec![Window {
            name: "session".into(),
            percent,
            resets: Some("2026-11-02T18:00:00Z[UTC]".parse::<Zoned>().unwrap()),
        }],
    }
}

fn unavailable() -> Limits {
    Limits {
        available: false,
        reason: Some("claude -p /usage exited with exit status: 1".into()),
        windows: Vec::new(),
    }
}

fn events(store: &Store) -> Vec<String> {
    std::fs::read_to_string(store.dir().join("events.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["text"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

#[test]
fn past_the_limit_the_fleet_winds_down_once_and_resumes_after_the_reset() {
    let (_dir, store) = store();
    let now = at("2026-11-02T16:00:00Z");

    assert_eq!(
        pace::plan_limits(&store, &session(80), LIMIT, now).unwrap(),
        None
    );
    let down = pace::plan_limits(&store, &session(87), LIMIT, now).unwrap();
    // Still past the limit on the next read: no second event.
    let again = pace::plan_limits(&store, &session(90), LIMIT, now).unwrap();
    let up = pace::plan_limits(&store, &session(3), LIMIT, at("2026-11-02T18:05:00Z")).unwrap();

    assert_eq!(
        down.unwrap(),
        "limits: the session window is 87% used, past plan_limit_percent (85%): start nothing new until it resets at Nov 2 at 6:00pm UTC, and let workers finish their turn"
    );
    assert_eq!(again, None);
    assert_eq!(
        up.unwrap(),
        "limits: the session window has reset: start ready work again"
    );
    assert_eq!(events(&store).len(), 2);
    assert_eq!(store.read().unwrap().wound_down, None);
}

#[test]
fn unreadable_limits_resume_only_after_the_reset_time() {
    let (_dir, store) = store();
    pace::plan_limits(&store, &session(87), LIMIT, at("2026-11-02T16:00:00Z")).unwrap();

    let early =
        pace::plan_limits(&store, &unavailable(), LIMIT, at("2026-11-02T17:59:00Z")).unwrap();
    let late =
        pace::plan_limits(&store, &unavailable(), LIMIT, at("2026-11-02T18:00:00Z")).unwrap();

    assert_eq!(early, None);
    assert!(late.unwrap().contains("has reset"));
}

#[test]
fn the_budget_is_reported_once_and_the_usage_saved_every_time() {
    let (_dir, store) = store();
    let used = |tokens| Estimate {
        estimated_tokens: tokens,
    };

    assert_eq!(
        pace::budget(&store, &used(900), 1000, at("2026-11-02T16:00:00Z")).unwrap(),
        None
    );
    let reached = pace::budget(&store, &used(1000), 1000, at("2026-11-02T16:01:00Z")).unwrap();
    let again = pace::budget(&store, &used(1100), 1000, at("2026-11-02T16:02:00Z")).unwrap();

    assert_eq!(
        reached.unwrap(),
        "budget: about 1000 tokens (estimate) of 1000 used this run: start nothing new, let workers finish their turn, and ask the owner how to continue"
    );
    assert_eq!(again, None);
    let state = store.read().unwrap();
    assert!(state.budget_reached);
    let usage = state.usage.unwrap();
    assert_eq!(usage.estimated_tokens, 1100);
    assert_eq!(usage.at, "2026-11-02T16:02:00Z");
}
