use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;

use orchestral::state::{Event, SCHEMA, State, Store};
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git should run")
        .status;
    assert!(status.success(), "git {args:?} failed");
}

fn events(store: &Store) -> Vec<Event> {
    fs::read_to_string(store.dir().join("events.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).expect("each line is one event"))
        .collect()
}

#[test]
fn concurrent_writers_lose_no_update_or_event() {
    let dir = TempDir::new().unwrap();
    let writers = 8;
    let each = 25;

    thread::scope(|scope| {
        for writer in 0..writers {
            let path = dir.path().to_owned();
            scope.spawn(move || {
                // A store per writer, as separate processes would have.
                let store = Store::at(path);
                for n in 0..each {
                    store.record("test", &format!("{writer}-{n}")).unwrap();
                }
            });
        }
    });

    let store = Store::at(dir.path().to_owned());
    let events = events(&store);
    let total = writers * each;
    assert_eq!(events.len(), total);
    let seqs: BTreeSet<u64> = events.iter().map(|event| event.seq).collect();
    assert_eq!(seqs, (1..=total as u64).collect());
    let texts: BTreeSet<&str> = events.iter().map(|event| event.text.as_str()).collect();
    assert_eq!(texts.len(), total);
    assert_eq!(store.read().unwrap().next_seq, total as u64 + 1);
}

#[test]
fn an_event_line_has_seq_ts_kind_and_text() {
    let dir = TempDir::new().unwrap();
    let store = Store::at(dir.path().to_owned());

    store.record("worker", "issue-12 opened PR #40").unwrap();

    let line = fs::read_to_string(dir.path().join("events.jsonl")).unwrap();
    let value: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
    let keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["kind", "seq", "text", "ts"]);
    assert_eq!(value["seq"], 1);
    assert!(value["ts"].as_str().unwrap().ends_with('Z'));
}

#[test]
fn a_new_store_reads_as_the_default_state() {
    let dir = TempDir::new().unwrap();

    let state = Store::at(dir.path().join("orchestral")).read().unwrap();

    assert_eq!(state, State::default());
    assert_eq!(state.schema, SCHEMA);
}

#[test]
fn an_update_is_written_and_leaves_no_temp_file() {
    let dir = TempDir::new().unwrap();
    let store = Store::at(dir.path().to_owned());

    store.update(|state| state.next_seq = 7).unwrap();

    assert_eq!(store.read().unwrap().next_seq, 7);
    let names: BTreeSet<String> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, ["lock", "state.json"].map(String::from).into());
}

#[test]
fn a_state_from_another_schema_is_an_error() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("state.json"),
        r#"{"schema": 99, "next_seq": 1}"#,
    )
    .unwrap();

    let result = Store::at(dir.path().to_owned()).read();

    assert!(result.is_err());
}

#[test]
fn every_worktree_resolves_to_the_same_store() {
    let dir = TempDir::new().unwrap();
    let main = dir.path().join("main");
    fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q"]);
    git(&main, &["commit", "-q", "--allow-empty", "-m", "start"]);
    git(&main, &["worktree", "add", "-q", "../feature"]);
    let nested = main.join("src");
    fs::create_dir(&nested).unwrap();

    let from_main = Store::for_repo(&main).unwrap();
    let from_nested = Store::for_repo(&nested).unwrap();
    let from_worktree = Store::for_repo(&dir.path().join("feature")).unwrap();

    let expected = main.join(".git/orchestral").canonicalize_parent();
    for store in [from_main, from_nested, from_worktree] {
        assert_eq!(store.dir().canonicalize_parent(), expected);
    }
}

#[test]
fn outside_a_repository_is_an_error() {
    let dir = TempDir::new().unwrap();

    assert!(Store::for_repo(dir.path()).is_err());
}

/// Resolves symlinks and `..` in a path whose last part may not exist yet.
trait CanonicalizeParent {
    fn canonicalize_parent(&self) -> std::path::PathBuf;
}

impl CanonicalizeParent for Path {
    fn canonicalize_parent(&self) -> std::path::PathBuf {
        let parent = self.parent().unwrap().canonicalize().unwrap();
        parent.join(self.file_name().unwrap())
    }
}
