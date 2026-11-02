//! PR previews: `orchestral try`, `previews` and `try --stop` change the
//! desired state, and the supervisor starts and stops the servers.
//!
//! A preview runs in the PR's worker worktree, so the server reloads as the
//! worker pushes. The policy's `preview.setup` runs once per worktree, and
//! again when a lockfile changes. Each server is detached in its own process
//! group, so it outlives the supervisor; a restarted supervisor finds it by
//! pid and start time.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::hash::{DefaultHasher, Hasher};
use std::io::Write;
use std::net::TcpListener;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::github::{self, gh_json};
use crate::outcome::Failure;
use crate::policy::{Policy, PreviewStart};
use crate::sessions;
use crate::spawn::main_checkout;
use crate::state::{Preview, Server, State, Store, Want};

/// The longest `preview.setup` may run.
const SETUP_DEADLINE: Duration = Duration::from_secs(15 * 60);

/// Files whose change means the dependencies changed, so setup runs again.
const LOCKFILES: [&str; 12] = [
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lock",
    "bun.lockb",
    "Cargo.lock",
    "Gemfile.lock",
    "poetry.lock",
    "uv.lock",
    "Pipfile.lock",
    "composer.lock",
    "go.sum",
];

/// The result of `orchestral try <pr>`.
#[derive(Debug, Serialize)]
pub struct Tried {
    pub pr: u64,
    /// The link, when the host gives each PR one.
    pub url: Option<String>,
    /// PRs whose previews were stopped to stay within `max_running`.
    pub made_room: Vec<u64>,
    /// False when no supervisor is running to start the server.
    pub supervisor_running: bool,
}

impl fmt::Display for Tried {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if let Some(url) = &self.url {
            return write!(f, "PR #{}'s preview is at {url}", self.pr);
        }
        write!(
            f,
            "Asked for a preview of PR #{}. `orchestral previews` shows its link once it's running.",
            self.pr
        )?;
        for pr in &self.made_room {
            write!(
                f,
                "\nStopped PR #{pr}'s preview to make room; `orchestral try {pr}` restarts it."
            )?;
        }
        if !self.supervisor_running {
            write!(
                f,
                "\nNo supervisor is running, so nothing starts it until `orchestral up`."
            )?;
        }
        Ok(())
    }
}

/// The result of `orchestral try --stop <pr>`.
#[derive(Debug, Serialize)]
pub struct StopRequested {
    pub pr: u64,
    pub had_preview: bool,
}

impl fmt::Display for StopRequested {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.had_preview {
            true => write!(f, "Asked the supervisor to stop PR #{}'s preview.", self.pr),
            false => write!(f, "PR #{} has no preview.", self.pr),
        }
    }
}

/// One line of `orchestral previews`.
#[derive(Debug, Serialize)]
pub struct Listed {
    pub pr: u64,
    /// `starting`, `running`, `failed`, `stopping` or `stopped`.
    pub status: &'static str,
    pub url: Option<String>,
    pub detail: Option<String>,
    pub log: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct Previews {
    pub previews: Vec<Listed>,
}

impl fmt::Display for Previews {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.previews.is_empty() {
            return write!(f, "No previews.");
        }
        let mut first = true;
        for preview in &self.previews {
            if !first {
                writeln!(f)?;
            }
            first = false;
            write!(f, "PR #{} {}", preview.pr, preview.status)?;
            if let Some(url) = &preview.url {
                write!(f, " at {url}")?;
            }
            if let Some(detail) = &preview.detail {
                write!(f, ": {detail}")?;
            }
            write!(f, " (log {})", preview.log.display())?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrView {
    state: String,
    head_ref_name: String,
}

/// `orchestral try <pr>`.
pub fn try_pr(cwd: &Path, pr: u64) -> Result<Tried, Failure> {
    let store = Store::for_repo(cwd)?;
    let main = main_checkout(store.dir())?;
    let settings = Policy::load(cwd)?.settings;
    let preview = settings.preview.ok_or_else(|| Failure::Refused {
        reason: "the policy has no `preview`".into(),
        fix: "add `preview` to .orchestral/policy.md".into(),
    })?;
    let view: PrView = gh_json(
        Path::new("gh"),
        &main,
        &["pr", "view", &pr.to_string(), "--json", "state,headRefName"],
    )?;
    if view.state != "OPEN" {
        return Err(Failure::Refused {
            reason: format!("PR #{pr} is {}", view.state.to_lowercase()),
            fix: "preview an open PR".into(),
        });
    }
    // The host serves it, so nothing runs here.
    if let PreviewStart::Url(url) = preview.start {
        return Ok(Tried {
            pr,
            url: Some(
                url.replace("{pr}", &pr.to_string())
                    .replace("{branch}", &view.head_ref_name),
            ),
            made_room: Vec::new(),
            supervisor_running: true,
        });
    }
    let made_room = store.update(|state| -> Result<Vec<u64>, Failure> {
        let worktree = state
            .workers
            .values()
            .find(|worker| worker.branch == view.head_ref_name && worker.review_of.is_none())
            .map(|worker| worker.worktree.clone())
            .ok_or_else(|| Failure::Refused {
                reason: format!(
                    "no worker has PR #{pr}'s branch {} checked out",
                    view.head_ref_name
                ),
                fix: "preview a PR whose worker is still open".into(),
            })?;
        let now = jiff::Timestamp::now().to_string();
        let entry = state.previews.entry(pr).or_insert_with(|| Preview {
            want: Want::Running,
            requested_at: now.clone(),
            worktree: worktree.clone(),
            server: None,
            set_up_for: None,
            restarts: 0,
            failed_at: None,
        });
        entry.want = Want::Running;
        entry.requested_at = now;
        entry.worktree = worktree;
        entry.restarts = 0;
        entry.failed_at = None;
        Ok(make_room(&mut state.previews, preview.max_running))
    })??;
    Ok(Tried {
        pr,
        url: None,
        made_room,
        supervisor_running: crate::supervisor::running(&store)?.is_some(),
    })
}

/// Stops the oldest previews past `max`, and returns their PRs.
fn make_room(previews: &mut BTreeMap<u64, Preview>, max: u32) -> Vec<u64> {
    let mut running: Vec<(String, u64)> = previews
        .iter()
        .filter(|(_, preview)| preview.want == Want::Running)
        .map(|(pr, preview)| (preview.requested_at.clone(), *pr))
        .collect();
    running.sort();
    let over = running.len().saturating_sub(max as usize);
    running
        .into_iter()
        .take(over)
        .map(|(_, pr)| {
            previews.get_mut(&pr).expect("listed above").want = Want::MadeRoom;
            pr
        })
        .collect()
}

/// `orchestral try --stop <pr>`.
pub fn stop(cwd: &Path, pr: u64) -> Result<StopRequested, Failure> {
    let store = Store::for_repo(cwd)?;
    let had_preview = store.update(|state| match state.previews.get_mut(&pr) {
        Some(preview) => {
            preview.want = Want::Off;
            true
        }
        None => false,
    })?;
    Ok(StopRequested { pr, had_preview })
}

/// `orchestral previews`.
pub fn list(cwd: &Path) -> Result<Previews, Failure> {
    let store = Store::for_repo(cwd)?;
    let state = store.read()?;
    let previews = state
        .previews
        .iter()
        .map(|(pr, preview)| {
            let (status, detail) = match (&preview.want, &preview.server, &preview.failed_at) {
                (Want::Running, _, Some(_)) => (
                    "failed",
                    Some(
                        "it exited twice; a new commit or `orchestral try` starts it again".into(),
                    ),
                ),
                (Want::Running, Some(_), None) => ("running", None),
                (Want::Running, None, None) => ("starting", None),
                (Want::MadeRoom, Some(_), _) | (Want::Off, Some(_), _) => ("stopping", None),
                (Want::MadeRoom, None, _) => (
                    "stopped",
                    Some(format!(
                        "stopped to make room; `orchestral try {pr}` restarts it"
                    )),
                ),
                (Want::Off, None, _) => ("stopped", None),
            };
            Listed {
                pr: *pr,
                status,
                url: preview
                    .server
                    .as_ref()
                    .filter(|_| status == "running")
                    .map(|server| format!("http://localhost:{}", server.port)),
                detail,
                log: log_path(&store, *pr),
            }
        })
        .collect();
    Ok(Previews { previews })
}

/// Whether a preview is wanted, and hasn't failed, or is still running,
/// which keeps the supervisor going.
pub fn wanted(state: &State) -> bool {
    state.previews.values().any(|preview| {
        (preview.want == Want::Running && preview.failed_at.is_none()) || preview.server.is_some()
    })
}

/// Brings the servers in line with the desired state. Run by the supervisor
/// on every tick; returns lines for its log.
pub fn reconcile(store: &Store) -> Result<Vec<String>, Failure> {
    let state = store.read()?;
    if state.previews.is_empty() {
        return Ok(Vec::new());
    }
    let main = main_checkout(store.dir())?;
    let preview = Policy::load(&main)?.settings.preview;
    let run = match preview.as_ref().map(|preview| &preview.start) {
        Some(PreviewStart::Run(run)) => Some(run.clone()),
        _ => None,
    };
    let setup = preview.and_then(|preview| preview.setup);
    let mut lines = Vec::new();
    for (pr, preview) in state.previews {
        lines.extend(reconcile_one(
            store,
            pr,
            preview,
            run.as_deref(),
            setup.as_deref(),
        )?);
    }
    Ok(lines)
}

fn reconcile_one(
    store: &Store,
    pr: u64,
    mut preview: Preview,
    run: Option<&str>,
    setup: Option<&str>,
) -> Result<Vec<String>, Failure> {
    let mut lines = Vec::new();
    let alive = preview.server.as_ref().is_some_and(is_alive);
    let run = match (&preview.want, run) {
        (Want::Running, Some(run)) => run,
        // Not wanted, or the policy no longer runs previews.
        _ => {
            if let Some(server) = preview.server.take().filter(|_| alive) {
                kill(&server);
                lines.push(format!("stopped PR #{pr}'s preview"));
            }
            save(
                store,
                pr,
                |entry| {
                    entry.server = None;
                },
                preview.want == Want::Off,
            )?;
            return Ok(lines);
        }
    };
    let head = head(&preview.worktree);
    let lockfiles = lockfile_hash(&preview.worktree);
    if alive {
        if preview.set_up_for.as_ref() == Some(&lockfiles) {
            return Ok(lines);
        }
        // The dependencies changed: set up again and restart.
        kill(preview.server.as_ref().expect("alive"));
        lines.push(format!(
            "PR #{pr}'s lockfiles changed; setting its preview up again"
        ));
    } else if let Some(failed_at) = &preview.failed_at {
        // A failed preview waits for a new commit.
        if Some(failed_at) == head.as_ref() {
            return Ok(lines);
        }
        preview.failed_at = None;
        preview.restarts = 0;
        lines.push(format!(
            "PR #{pr} has a new commit; starting its preview again"
        ));
    } else if let Some(server) = &preview.server {
        // It exited: restart it once, then give up until a new commit.
        if preview.restarts >= 1 && Some(&server.head) == head.as_ref() {
            let log = log_path(store, pr);
            save(
                store,
                pr,
                |entry| {
                    entry.server = None;
                    entry.failed_at = head.clone();
                },
                false,
            )?;
            let text = format!("PR #{pr}'s preview exited twice; see {}", log.display());
            store.record("preview", &text)?;
            lines.push(text);
            return Ok(lines);
        }
        if Some(&server.head) == head.as_ref() {
            preview.restarts += 1;
        } else {
            preview.restarts = 0;
        }
        lines.push(format!("PR #{pr}'s preview exited; starting it again"));
    }

    let log = log_path(store, pr);
    if preview.set_up_for.as_ref() != Some(&lockfiles)
        && let Some(setup) = setup
        && let Err(err) = run_setup(setup, &preview.worktree, &log)
    {
        save(
            store,
            pr,
            |entry| {
                entry.server = None;
                entry.failed_at = head.clone();
            },
            false,
        )?;
        let text = format!(
            "PR #{pr}'s preview setup failed: {err}; see {}",
            log.display()
        );
        store.record("preview", &text)?;
        lines.push(text);
        return Ok(lines);
    }
    let server = start(
        run,
        &preview.worktree,
        &log,
        head.clone().unwrap_or_default(),
    )?;
    lines.push(format!(
        "started PR #{pr}'s preview on port {} (pid {})",
        server.port, server.pid
    ));
    let restarts = preview.restarts;
    save(
        store,
        pr,
        |entry| {
            entry.server = Some(server);
            entry.set_up_for = Some(lockfiles);
            entry.restarts = restarts;
            entry.failed_at = None;
        },
        false,
    )?;
    Ok(lines)
}

/// Writes a change to one preview, unless `try` or `try --stop` changed what's
/// wanted meanwhile. `remove` drops it once its server is gone.
fn save(
    store: &Store,
    pr: u64,
    change: impl FnOnce(&mut Preview),
    remove: bool,
) -> Result<(), Failure> {
    store.update(|state| {
        if let Some(entry) = state.previews.get_mut(&pr) {
            change(entry);
            if remove && entry.want == Want::Off && entry.server.is_none() {
                state.previews.remove(&pr);
            }
        }
    })
}

/// Stops and forgets the previews of PRs that merged or closed, from the
/// supervisor's last GitHub poll.
pub fn stop_closed(store: &Store) -> Result<Vec<String>, Failure> {
    let Some(open) = github::open_prs(store)? else {
        return Ok(Vec::new());
    };
    let closed: Vec<u64> = store.update(|state| {
        let closed: Vec<u64> = state
            .previews
            .keys()
            .filter(|pr| !open.contains(pr))
            .copied()
            .collect();
        for pr in &closed {
            if let Some(preview) = state.previews.get_mut(pr) {
                preview.want = Want::Off;
            }
        }
        closed
    })?;
    Ok(closed
        .iter()
        .map(|pr| format!("PR #{pr} merged or closed; stopping its preview"))
        .collect())
}

fn run_setup(setup: &str, worktree: &Path, log: &Path) -> Result<(), String> {
    let output = crate::deadline::output(
        Command::new("sh").args(["-c", setup]).current_dir(worktree),
        SETUP_DEADLINE,
        "preview.setup",
    )?;
    let mut file = open_log(log).map_err(|err| err.to_string())?;
    let _ = file.write_all(&output.stdout);
    let _ = file.write_all(&output.stderr);
    match output.status.success() {
        true => Ok(()),
        false => Err(format!("it exited with {}", output.status)),
    }
}

/// Starts `run` detached with `PORT` set, logging to `log`.
fn start(run: &str, worktree: &Path, log: &Path, head: String) -> Result<Server, Failure> {
    let port = free_port()?;
    open_log(log).map_err(|err| Failure::Error(format!("{}: {err}", log.display())))?;
    // `sh` starts the server in the background and exits, so the server
    // isn't the supervisor's child: it can't linger as a zombie, and it
    // outlives the supervisor. `$!` is the server's pid. The new process
    // group, numbered after this `sh`, lets a stop reach the server's own
    // children too.
    let spawned = Command::new("sh")
        .args([
            "-c",
            "sh -c \"$1\" </dev/null >>\"$2\" 2>&1 & echo $!",
            "sh",
            run,
        ])
        .arg(log)
        .current_dir(worktree)
        .env("PORT", port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|err| Failure::Error(format!("can't start the preview: {err}")))?;
    let group = spawned.id();
    let output = spawned
        .wait_with_output()
        .map_err(|err| Failure::Error(format!("can't start the preview: {err}")))?;
    let pid: u32 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .map_err(|_| Failure::Error("the preview's shell didn't print a pid".into()))?;
    // A server that exited at once has no start time, and counts as exited
    // on the next tick.
    let started = sessions::process(pid)?
        .map(|process| process.started)
        .unwrap_or_default();
    Ok(Server {
        pid,
        group,
        started,
        port,
        head,
    })
}

fn is_alive(server: &Server) -> bool {
    matches!(sessions::process(server.pid), Ok(Some(process)) if process.started == server.started)
}

/// Stops a server and its process group.
fn kill(server: &Server) {
    let _ = Command::new("kill")
        .args(["-TERM", "--", &format!("-{}", server.group)])
        .stderr(Stdio::null())
        .status();
    let _ = Command::new("kill")
        .args(["-TERM", &server.pid.to_string()])
        .stderr(Stdio::null())
        .status();
}

fn free_port() -> Result<u16, Failure> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|err| Failure::Error(format!("can't find a free port: {err}")))?;
    listener
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|err| Failure::Error(format!("can't find a free port: {err}")))
}

fn head(worktree: &Path) -> Option<String> {
    crate::git::git(worktree, &["rev-parse", "HEAD"])
        .ok()
        .map(|head| head.trim().to_owned())
}

/// A hash of the worktree's lockfiles. Only compared with the last one, so
/// a different hash after an upgrade just runs setup once more.
fn lockfile_hash(worktree: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    for name in LOCKFILES {
        if let Ok(bytes) = fs::read(worktree.join(name)) {
            hasher.write(name.as_bytes());
            hasher.write(&bytes);
        }
    }
    format!("{:016x}", hasher.finish())
}

fn log_path(store: &Store, pr: u64) -> PathBuf {
    store.dir().join("logs").join(format!("preview-{pr}.log"))
}

fn open_log(path: &Path) -> std::io::Result<fs::File> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    OpenOptions::new().create(true).append(true).open(path)
}
