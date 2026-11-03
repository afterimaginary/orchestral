//! The state store: `state.json` and `events.jsonl`, shared by CLI commands
//! and the supervisor through files, with no IPC.
//!
//! Every change happens under an advisory lock on `lock`. `state.json` is
//! replaced atomically through a temp file and rename, so a reader never sees
//! a partial write and doesn't need the lock.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::outcome::Failure;

/// The `state.json` schema this build reads and writes.
pub const SCHEMA: u32 = 12;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct State {
    pub schema: u32,
    /// The `seq` the next event gets.
    pub next_seq: u64,
    /// Workers by name.
    pub workers: BTreeMap<String, Worker>,
    /// The session that claimed this repository, if any.
    pub orchestrator: Option<Orchestrator>,
    /// While set, spawns are refused.
    pub paused: bool,
    /// When the current run started, in RFC 3339 UTC. The fleet budget
    /// counts from here.
    pub run_started_at: Option<String>,
    /// What the supervisor last saw each worker's session doing: `busy`,
    /// `idle`, `waiting` or `exited`.
    pub observed: BTreeMap<String, crate::observe::Observed>,
    /// The latest review verdict on each PR, by PR number.
    pub reviews: BTreeMap<u64, Review>,
    /// The plan window the fleet wound down for, until it resets.
    pub wound_down: Option<WoundDown>,
    /// The run's fleet usage as the supervisor last read it.
    pub usage: Option<Usage>,
    /// Whether this run has reached its `fleet_token_budget`.
    pub budget_reached: bool,
    /// Each PR's merge route, by PR number. A PR without one goes to the
    /// owner.
    pub routes: BTreeMap<u64, Route>,
    /// The head commit the owner approved on each PR.
    pub approvals: BTreeMap<u64, String>,
    /// PRs the owner put on hold.
    pub holds: BTreeSet<u64>,
    /// PR previews, by PR number.
    pub previews: BTreeMap<u64, Preview>,
    /// Reminders to the owner, by the worker waiting on them.
    pub nags: BTreeMap<String, Nag>,
    /// Reminder sessions that haven't been stopped yet.
    pub nag_sessions: Vec<NagSession>,
    /// The number in the next reminder session's name.
    pub next_nag: u64,
}

/// A worker waiting on the owner, and the reminders sent about it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Nag {
    /// When the worker started waiting, in RFC 3339 UTC.
    pub since: String,
    pub sent: u32,
    /// RFC 3339, in UTC.
    pub last_sent_at: Option<String>,
}

/// A one-shot reminder session, stopped once it has sent its notification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NagSession {
    pub job_id: String,
    /// RFC 3339, in UTC.
    pub started_at: String,
}

/// A PR's preview: what's wanted, from `try`, and the server the supervisor
/// runs for it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Preview {
    pub want: Want,
    /// When `try` last asked for it, in RFC 3339 UTC. The oldest is stopped
    /// first to stay within `max_running`.
    pub requested_at: String,
    /// The worker worktree it runs in.
    pub worktree: PathBuf,
    /// The PR's branch.
    pub branch: String,
    pub server: Option<Server>,
    /// The lockfile hash `preview.setup` last ran for.
    pub set_up_for: Option<String>,
    /// Restarts since it last started cleanly at its commit.
    pub restarts: u32,
    /// Why it stopped trying. It stays down until a new commit or `try`.
    pub failure: Option<PreviewFailure>,
    /// The PR comment holding its tailnet link, updated on each start.
    pub comment: Option<u64>,
}

/// A preview that stopped trying.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PreviewFailure {
    /// The worktree's commit then, empty when it couldn't be read.
    pub at: String,
    pub why: String,
}

/// Whether a preview should run.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Want {
    Running,
    /// Stopped for a newer preview past `max_running`.
    MadeRoom,
    /// Stopped by `try --stop`.
    Off,
    /// Its PR merged or closed. Forgotten once its server is gone.
    Closed,
}

/// A running preview server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Server {
    /// The server's pid, or under portless, the pid `portless list` shows
    /// for its route.
    pub pid: u32,
    /// Its process group, which a stop signals.
    pub group: u32,
    /// Its start time from `ps`, telling it apart from a reused pid.
    pub started: String,
    pub port: u16,
    /// The worktree's commit when it started.
    pub head: String,
    /// Whether portless runs it, so a stop sends SIGINT to `pid`.
    pub portless: bool,
    /// Its link from `portless list`: the tailnet one when it has one.
    pub url: Option<String>,
}

/// Who merges a PR.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MergeRoute {
    /// The owner, after approving its head commit.
    Human,
    /// The supervisor, once every check passes.
    Supervisor,
}

/// A PR's merge route, and why the orchestrator chose it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Route {
    pub to: MergeRoute,
    pub reason: String,
}

/// A plan window past `plan_limit_percent`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WoundDown {
    pub window: String,
    /// When the window resets, in RFC 3339 UTC, if `/usage` gave a time.
    pub resets_at: Option<String>,
}

/// The run's weighted token total at one moment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub estimated_tokens: u64,
    /// RFC 3339, in UTC.
    pub at: String,
}

/// A reviewer's verdict on one commit of a PR.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Review {
    /// The head commit the verdict is for.
    pub sha: String,
    pub passed: bool,
    /// Commits of this PR that failed review, oldest first.
    pub failed: Vec<String>,
}

/// The session acting as orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Orchestrator {
    pub session_id: String,
    /// RFC 3339, in UTC.
    pub claimed_at: String,
}

impl Default for State {
    fn default() -> Self {
        State {
            schema: SCHEMA,
            next_seq: 1,
            workers: BTreeMap::new(),
            orchestrator: None,
            paused: false,
            run_started_at: None,
            observed: BTreeMap::new(),
            reviews: BTreeMap::new(),
            wound_down: None,
            usage: None,
            budget_reached: false,
            routes: BTreeMap::new(),
            approvals: BTreeMap::new(),
            holds: BTreeSet::new(),
            previews: BTreeMap::new(),
            nags: BTreeMap::new(),
            nag_sessions: Vec::new(),
            next_nag: 1,
        }
    }
}

/// A worker session and its worktree.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Worker {
    /// `<repo slug>-<name>`.
    pub session_name: String,
    pub issue: u64,
    pub model: String,
    pub effort: String,
    pub worktree: PathBuf,
    pub branch: String,
    /// The id `claude --bg` printed, once the launch succeeded.
    pub job_id: Option<String>,
    /// For a reviewer, the PR it reviews.
    pub review_of: Option<u64>,
    /// RFC 3339, in UTC.
    pub created_at: String,
}

/// One line of `events.jsonl`.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Event {
    pub seq: u64,
    /// RFC 3339, in UTC.
    pub ts: String,
    pub kind: String,
    pub text: String,
}

#[derive(Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// The store for the repository containing `cwd`. Every worktree of a
    /// repository shares one store, under its git common directory.
    pub fn for_repo(cwd: &Path) -> Result<Store, Failure> {
        let output = Command::new("git")
            .args(["rev-parse", "--git-common-dir"])
            .current_dir(cwd)
            .output()
            .map_err(|err| Failure::Error(format!("can't run git: {err}")))?;
        if !output.status.success() {
            return Err(Failure::Error(format!(
                "can't find the git repository for {}: {}",
                cwd.display(),
                String::from_utf8_lossy(&output.stderr).trim_end()
            )));
        }
        let common = String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned();
        // git prints a path relative to `cwd` when it can.
        Ok(Store::at(cwd.join(common).join("orchestral")))
    }

    /// The store in `dir`, for tests and for callers that already know it.
    pub fn at(dir: PathBuf) -> Store {
        Store { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Reads the current state, or the default when none was written yet.
    pub fn read(&self) -> Result<State, Failure> {
        let path = self.dir.join("state.json");
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(State::default()),
            Err(err) => return Err(io_error(&path, err)),
        };
        let invalid = |err: serde_json::Error| {
            Failure::Error(format!("{} is invalid: {err}", path.display()))
        };
        // Check the version before the shape, since another schema may have
        // a different one.
        let value: serde_json::Value = serde_json::from_str(&text).map_err(invalid)?;
        let schema = &value["schema"];
        if *schema != SCHEMA {
            return Err(Failure::Error(format!(
                "{} has schema {schema}, and this orchestral reads schema {SCHEMA}",
                path.display(),
            )));
        }
        serde_json::from_value(value).map_err(invalid)
    }

    /// Changes the state under the lock, and writes it back atomically.
    pub fn update<T>(&self, change: impl FnOnce(&mut State) -> T) -> Result<T, Failure> {
        let _lock = self.lock()?;
        let mut state = self.read()?;
        let result = change(&mut state);
        self.write(&state)?;
        Ok(result)
    }

    /// Appends an event to `events.jsonl` with the next `seq`.
    pub fn record(&self, kind: &str, text: &str) -> Result<Event, Failure> {
        let _lock = self.lock()?;
        let mut state = self.read()?;
        let event = Event {
            seq: state.next_seq,
            ts: jiff::Timestamp::now().to_string(),
            kind: kind.to_owned(),
            text: text.to_owned(),
        };
        state.next_seq += 1;
        // State first: a crash between the two writes skips a seq rather
        // than reusing one.
        self.write(&state)?;
        let path = self.dir.join("events.jsonl");
        let mut line = serde_json::to_string(&event).expect("an event always serializes");
        line.push('\n');
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)
            .map_err(|err| io_error(&path, err))?;
        // A crash mid-append leaves a line without its newline. End it, so
        // the torn line can't swallow this event.
        if !ends_with_newline(&mut file).map_err(|err| io_error(&path, err))? {
            line.insert(0, '\n');
        }
        file.write_all(line.as_bytes())
            .map_err(|err| io_error(&path, err))?;
        Ok(event)
    }

    /// Takes the advisory lock, which is released when the file is dropped.
    fn lock(&self) -> Result<File, Failure> {
        fs::create_dir_all(&self.dir).map_err(|err| io_error(&self.dir, err))?;
        let path = self.dir.join("lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|err| io_error(&path, err))?;
        file.lock().map_err(|err| io_error(&path, err))?;
        Ok(file)
    }

    fn write(&self, state: &State) -> Result<(), Failure> {
        let path = self.dir.join("state.json");
        // Writes hold the lock, so one temp name is enough.
        let temp = self.dir.join("state.json.tmp");
        let json = serde_json::to_string_pretty(state).expect("state always serializes");
        let mut file = File::create(&temp).map_err(|err| io_error(&temp, err))?;
        file.write_all(json.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|err| io_error(&temp, err))?;
        fs::rename(&temp, &path).map_err(|err| io_error(&path, err))
    }
}

/// Whether a file is empty or ends with a newline.
fn ends_with_newline(file: &mut File) -> std::io::Result<bool> {
    if file.metadata()?.len() == 0 {
        return Ok(true);
    }
    let mut last = [0];
    file.seek(SeekFrom::End(-1))?;
    file.read_exact(&mut last)?;
    Ok(last[0] == b'\n')
}

fn io_error(path: &Path, err: std::io::Error) -> Failure {
    Failure::Error(format!("{}: {err}", path.display()))
}
