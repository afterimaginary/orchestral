//! PR previews: `orchestral try`, `previews` and `try --stop` change the
//! desired state, and the supervisor starts and stops the servers.
//!
//! A preview runs in the PR's worker worktree, so the server reloads as the
//! worker pushes. The policy's `preview.setup` runs once per worktree, and
//! again when a lockfile changes. Each server is detached in its own process
//! group, so it outlives the supervisor; a restarted supervisor finds it by
//! pid and start time.
//!
//! Where portless is installed, each server runs under a named portless
//! route, registered with `tailscale serve` while Tailscale is running, and
//! its link is posted on the PR (ADR-0006). Otherwise it gets a free port on
//! localhost as `$PORT`.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::hash::{DefaultHasher, Hasher};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::github::{self, gh_json};
use crate::outcome::Failure;
use crate::policy::{Policy, PreviewStart};
use crate::sessions;
use crate::spawn::{main_checkout, repo_slug};
use crate::state::{Preview, PreviewFailure, Server, State, Store, Want};

/// The longest `preview.setup` may run.
const SETUP_DEADLINE: Duration = Duration::from_secs(15 * 60);

/// How long a stopped server has to exit before it's killed.
const STOP_GRACE: Duration = Duration::from_secs(5);

const POLL: Duration = Duration::from_millis(100);

/// How long a portless route has to show up in `portless list`.
const ROUTE_DEADLINE: Duration = Duration::from_secs(15);

/// The longest a `portless` or `tailscale` query may take.
const TOOL_DEADLINE: Duration = Duration::from_secs(10);

/// The local ports portless gives its servers. A `tailscale serve` entry
/// proxying to one of them may be a crashed preview's.
const PORTLESS_PORTS: std::ops::RangeInclusive<u16> = 4000..=4999;

/// The external tools previews use. Tests point these at stand-ins.
pub struct Tools {
    pub gh: PathBuf,
    pub portless: PathBuf,
    pub tailscale: PathBuf,
}

impl Default for Tools {
    fn default() -> Self {
        Tools {
            gh: "gh".into(),
            portless: "portless".into(),
            tailscale: "tailscale".into(),
        }
    }
}

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
    // From the main checkout, as the supervisor reads it.
    let settings = Policy::load(&main)?.settings;
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
            supervisor_running: crate::supervisor::running(&store)?.is_some(),
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
            branch: view.head_ref_name.clone(),
            server: None,
            set_up_for: None,
            restarts: 0,
            failure: None,
            comment: None,
        });
        entry.want = Want::Running;
        entry.requested_at = now;
        entry.worktree = worktree;
        entry.branch = view.head_ref_name.clone();
        entry.restarts = 0;
        entry.failure = None;
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
            let (status, detail) = match (&preview.want, &preview.server, &preview.failure) {
                (Want::Running, _, Some(failure)) => (
                    "failed",
                    Some(format!(
                        "{}; a new commit or `orchestral try {pr}` starts it again",
                        failure.why
                    )),
                ),
                (Want::Running, Some(_), None) => ("running", None),
                (Want::Running, None, None) => ("starting", None),
                (_, Some(_), _) => ("stopping", None),
                (Want::MadeRoom, None, _) => (
                    "stopped",
                    Some(format!(
                        "stopped to make room; `orchestral try {pr}` restarts it"
                    )),
                ),
                (Want::Off | Want::Closed, None, _) => ("stopped", None),
            };
            Listed {
                pr: *pr,
                status,
                url: preview
                    .server
                    .as_ref()
                    .filter(|_| status == "running")
                    .map(link),
                detail,
                log: log_path(&store, *pr),
            }
        })
        .collect();
    Ok(Previews { previews })
}

/// A running server's link.
pub(crate) fn link(server: &Server) -> String {
    server
        .url
        .clone()
        .unwrap_or_else(|| format!("http://localhost:{}", server.port))
}

/// Whether a preview is wanted, and hasn't failed, or is still running,
/// which keeps the supervisor going.
pub fn wanted(state: &State) -> bool {
    state.previews.values().any(|preview| {
        (preview.want == Want::Running && preview.failure.is_none()) || preview.server.is_some()
    })
}

/// Brings the servers in line with the desired state. Run by the supervisor
/// on every tick; returns lines for its log. A preview that can't be
/// handled is logged, and the others carry on.
pub fn reconcile(store: &Store, tools: &Tools) -> Result<Vec<String>, Failure> {
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
        match reconcile_one(
            store,
            tools,
            &main,
            pr,
            preview,
            run.as_deref(),
            setup.as_deref(),
        ) {
            Ok(more) => lines.extend(more),
            Err(err) => lines.push(format!(
                "can't check PR #{pr}'s preview: {}",
                crate::supervisor::failure_text(&err)
            )),
        }
    }
    Ok(lines)
}

fn reconcile_one(
    store: &Store,
    tools: &Tools,
    main: &Path,
    pr: u64,
    mut preview: Preview,
    run: Option<&str>,
    setup: Option<&str>,
) -> Result<Vec<String>, Failure> {
    let mut lines = Vec::new();
    let alive = preview.server.as_ref().is_some_and(is_alive);
    if preview.want != Want::Running {
        if let Some(server) = preview.server.as_ref().filter(|_| alive) {
            stop_server(server);
            lines.push(format!("stopped PR #{pr}'s preview"));
        }
        update(store, pr, &preview, |entry| entry.server = None)?;
        return Ok(lines);
    }
    if preview.failure.is_some() && alive {
        // Can't happen after a clean failure, but a stray server mustn't stay.
        stop_server(preview.server.as_ref().expect("alive"));
    }
    let head = head(&preview.worktree);
    let Some(run) = run else {
        // The policy no longer runs previews here.
        if alive {
            stop_server(preview.server.as_ref().expect("alive"));
        }
        if preview.failure.is_none() {
            fail(
                store,
                pr,
                &preview,
                head,
                "the policy no longer runs previews",
            )?;
            lines.push(format!(
                "stopped PR #{pr}'s preview: the policy no longer runs previews"
            ));
        }
        return Ok(lines);
    };
    let Some(head) = head else {
        if alive {
            stop_server(preview.server.as_ref().expect("alive"));
        }
        if preview.failure.is_none() {
            let why = format!("its worktree {} can't be read", preview.worktree.display());
            fail(store, pr, &preview, None, &why)?;
            let text = format!("PR #{pr}'s preview stopped: {why}");
            store.record("preview", &text)?;
            lines.push(text);
        }
        return Ok(lines);
    };
    let lockfiles = lockfile_hash(&preview.worktree);
    if alive {
        if preview.set_up_for.as_ref() == Some(&lockfiles) {
            return Ok(lines);
        }
        // The dependencies changed: set up again and restart.
        stop_server(preview.server.as_ref().expect("alive"));
        lines.push(format!(
            "PR #{pr}'s lockfiles changed; setting its preview up again"
        ));
    } else if let Some(failure) = &preview.failure {
        // A failed preview waits for a new commit.
        if failure.at == head {
            return Ok(lines);
        }
        preview.failure = None;
        preview.restarts = 0;
        lines.push(format!(
            "PR #{pr} has a new commit; starting its preview again"
        ));
    } else if let Some(server) = &preview.server {
        // It exited: restart it once, then give up until a new commit.
        let same_commit = server.head == head;
        if preview.restarts >= 1 && same_commit {
            let log = log_path(store, pr);
            fail(store, pr, &preview, Some(head), "it exited twice")?;
            let text = format!("PR #{pr}'s preview exited twice; see {}", log.display());
            store.record("preview", &text)?;
            lines.push(text);
            return Ok(lines);
        }
        preview.restarts = if same_commit { preview.restarts + 1 } else { 0 };
        lines.push(format!("PR #{pr}'s preview exited; starting it again"));
    }

    let log = log_path(store, pr);
    let mut set_up_for = preview.set_up_for.clone();
    if set_up_for.as_ref() != Some(&lockfiles) {
        if let Some(setup) = setup
            && let Err(err) = run_setup(setup, &preview.worktree, &log)
        {
            fail(store, pr, &preview, Some(head), "its setup failed")?;
            let text = format!(
                "PR #{pr}'s preview setup failed: {err}; see {}",
                log.display()
            );
            store.record("preview", &text)?;
            lines.push(text);
            return Ok(lines);
        }
        // After setup, which may write a lockfile itself.
        set_up_for = Some(lockfile_hash(&preview.worktree));
    }
    let name = route_name(main, pr);
    let (server, tailnet) = start(tools, &name, run, &preview.worktree, &log, head, &mut lines)?;
    lines.push(format!(
        "started PR #{pr}'s preview at {} (pid {})",
        link(&server),
        server.pid
    ));
    // The link goes on the PR only when other devices can open it.
    let comment = match tailnet.as_deref() {
        Some(url) => match post_link(tools, main, pr, url, preview.comment) {
            Ok(id) => Some(id),
            Err(err) => {
                lines.push(format!(
                    "can't post PR #{pr}'s preview link: {}",
                    crate::supervisor::failure_text(&err)
                ));
                preview.comment
            }
        },
        None => preview.comment,
    };
    let restarts = preview.restarts;
    update(store, pr, &preview, |entry| {
        entry.server = Some(server);
        entry.set_up_for = set_up_for;
        entry.restarts = restarts;
        entry.failure = None;
        entry.comment = comment;
    })?;
    Ok(lines)
}

/// Records why a preview stopped trying.
fn fail(
    store: &Store,
    pr: u64,
    seen: &Preview,
    head: Option<String>,
    why: &str,
) -> Result<(), Failure> {
    update(store, pr, seen, |entry| {
        entry.server = None;
        entry.failure = Some(PreviewFailure {
            at: head.unwrap_or_default(),
            why: why.to_owned(),
        });
    })
}

/// Writes the supervisor's change to one preview. When `try` asked for it
/// again since `seen` was read, only the server and setup are kept, since
/// `try` reset the rest. A closed PR's preview is forgotten once its server
/// is gone.
fn update(
    store: &Store,
    pr: u64,
    seen: &Preview,
    change: impl FnOnce(&mut Preview),
) -> Result<(), Failure> {
    store.update(|state| {
        let Some(entry) = state.previews.get_mut(&pr) else {
            return;
        };
        if entry.requested_at == seen.requested_at {
            change(entry);
        } else {
            let mut changed = entry.clone();
            change(&mut changed);
            entry.server = changed.server;
            entry.set_up_for = changed.set_up_for;
            entry.comment = changed.comment;
        }
        if entry.want == Want::Closed && entry.server.is_none() {
            state.previews.remove(&pr);
        }
    })
}

/// Stops the previews of PRs that merged or closed, from the supervisor's
/// last GitHub poll, and forgets them once their servers are gone.
pub fn stop_closed(store: &Store) -> Result<Vec<String>, Failure> {
    let Some(open) = github::open_prs(store)? else {
        return Ok(Vec::new());
    };
    let closed: Vec<u64> = store.update(|state| {
        let closed: Vec<u64> = state
            .previews
            .iter()
            .filter(|(pr, preview)| !open.contains(pr) && preview.want != Want::Closed)
            .map(|(pr, _)| *pr)
            .collect();
        for pr in &closed {
            if let Some(preview) = state.previews.get_mut(pr) {
                preview.want = Want::Closed;
            }
        }
        closed
    })?;
    Ok(closed
        .iter()
        .map(|pr| format!("PR #{pr} merged or closed; stopping its preview"))
        .collect())
}

/// Runs `preview.setup` in the worktree, its output going straight to the
/// log so a hung setup still leaves a trace.
fn run_setup(setup: &str, worktree: &Path, log: &Path) -> Result<(), String> {
    let file = open_log(log).map_err(|err| format!("{}: {err}", log.display()))?;
    let mut child = Command::new("sh")
        .args(["-c", setup])
        .current_dir(worktree)
        .stdin(Stdio::null())
        .stdout(file.try_clone().map_err(|err| err.to_string())?)
        .stderr(file)
        .process_group(0)
        .spawn()
        .map_err(|err| format!("can't run it: {err}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("it exited with {status}")),
            Ok(None) if started.elapsed() < SETUP_DEADLINE => thread::sleep(POLL),
            Ok(None) => {
                signal("KILL", &format!("-{}", child.id()));
                let _ = child.wait();
                return Err(format!(
                    "it didn't finish within {} minutes",
                    SETUP_DEADLINE.as_secs() / 60
                ));
            }
            Err(err) => return Err(format!("can't wait for it: {err}")),
        }
    }
}

/// Starts `run` detached, logging to `log`, and returns the server and its
/// tailnet link, if it has one. Under portless, the route is named `name`;
/// otherwise the server gets a free port as `$PORT`.
fn start(
    tools: &Tools,
    name: &str,
    run: &str,
    worktree: &Path,
    log: &Path,
    head: String,
    lines: &mut Vec<String>,
) -> Result<(Server, Option<String>), Failure> {
    let portless = succeeds(Command::new(&tools.portless).arg("--version"));
    let tailscale = portless && succeeds(Command::new(&tools.tailscale).arg("status"));
    if tailscale {
        lines.extend(clear_stale_serves(tools));
    }
    open_log(log).map_err(|err| Failure::Error(format!("{}: {err}", log.display())))?;
    // `sh` starts the server in the background and exits, so the server
    // isn't the supervisor's child: it can't linger as a zombie, and it
    // outlives the supervisor. `$!` is the server's pid. The new process
    // group, numbered after this `sh`, lets a stop reach the server's own
    // children too.
    let mut command = Command::new("sh");
    command
        .args([
            "-c",
            "log=$1; shift; \"$@\" </dev/null >>\"$log\" 2>&1 & echo $!",
            "sh",
        ])
        .arg(log);
    let mut port = 0;
    if portless {
        command
            .arg(&tools.portless)
            .args([name, "--force", "sh", "-c", run]);
        match tailscale {
            true => command.env("PORTLESS_TAILSCALE", "1"),
            false => command.env_remove("PORTLESS_TAILSCALE"),
        };
    } else {
        port = free_port()?;
        command
            .args(["sh", "-c", run])
            .env("PORT", port.to_string());
    }
    let spawned = command
        .current_dir(worktree)
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
    let mut pid: u32 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .map_err(|_| Failure::Error("the preview's shell didn't print a pid".into()))?;
    let mut url = None;
    let mut tailnet = None;
    if portless {
        // portless may sit behind a wrapper, so its own pid comes from its
        // route.
        match wait_for_route(tools, name, tailscale) {
            Some(route) => {
                pid = route.pid;
                port = route.port;
                tailnet = route.tailnet.clone();
                url = Some(route.tailnet.unwrap_or(route.url));
            }
            None => lines.push(format!(
                "portless didn't list the route {name}; see {}",
                log.display()
            )),
        }
    }
    // A server that exited at once has no start time, and counts as exited
    // on the next tick.
    let started = sessions::process(pid)?
        .map(|process| process.started)
        .unwrap_or_default();
    let server = Server {
        pid,
        group,
        started,
        port,
        head,
        portless,
        url,
    };
    Ok((server, tailnet))
}

/// `<repo slug>-pr-<n>`, as a hostname label.
fn route_name(main: &Path, pr: u64) -> String {
    let slug: String = repo_slug(main)
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("{}-pr-{pr}", slug.trim_matches('-'))
}

/// One route in `portless list`.
#[derive(Debug, Clone)]
struct Route {
    url: String,
    port: u16,
    pid: u32,
    /// Its tailnet link, once registered with Tailscale.
    tailnet: Option<String>,
}

/// The routes `portless list` shows, or none when it can't be read.
fn routes(tools: &Tools) -> Vec<Route> {
    let Ok(output) = crate::deadline::output(
        Command::new(&tools.portless).arg("list"),
        TOOL_DEADLINE,
        "portless",
    ) else {
        return Vec::new();
    };
    parse_routes(&strip_colors(&String::from_utf8_lossy(&output.stdout)))
}

/// Reads lines like
/// `https://app.localhost  ->  localhost:4321  (pid 123)`, each optionally
/// followed by `tailscale: <url>` or `funnel: <url>`.
fn parse_routes(text: &str) -> Vec<Route> {
    let mut routes: Vec<Route> = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(url) = line
            .strip_prefix("tailscale:")
            .or_else(|| line.strip_prefix("funnel:"))
        {
            if let Some(route) = routes.last_mut() {
                route.tailnet = Some(url.trim().to_owned());
            }
            continue;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        let [url, "->", target, "(pid", pid] = words[..] else {
            continue;
        };
        let port = target.rsplit(':').next().and_then(|port| port.parse().ok());
        let pid = pid.trim_end_matches(')').parse().ok();
        if let (Some(port), Some(pid)) = (port, pid) {
            routes.push(Route {
                url: url.to_owned(),
                port,
                pid,
                tailnet: None,
            });
        }
    }
    routes
}

fn strip_colors(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // An SGR sequence: ESC [ ... m.
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        } else {
            plain.push(c);
        }
    }
    plain
}

/// Waits for the route named `name`, and for its tailnet link when
/// `tailscale` is on. Without the link by the deadline, the route is
/// returned as it is.
fn wait_for_route(tools: &Tools, name: &str, tailscale: bool) -> Option<Route> {
    let host = format!("https://{name}.localhost");
    let deadline = Instant::now() + ROUTE_DEADLINE;
    let mut found = None;
    loop {
        found = routes(tools)
            .into_iter()
            .find(|route| route.url == host || route.url.starts_with(&format!("{host}:")))
            .or(found);
        let done = found
            .as_ref()
            .is_some_and(|route: &Route| !tailscale || route.tailnet.is_some());
        if done || Instant::now() >= deadline {
            return found;
        }
        thread::sleep(POLL * 5);
    }
}

/// Removes `tailscale serve` entries a crashed preview left: a single `/`
/// proxy to a local port portless hands out, with nothing listening there
/// and no portless route using it. Other entries are left alone, and it
/// never resets the whole configuration.
fn clear_stale_serves(tools: &Tools) -> Vec<String> {
    let Ok(output) = crate::deadline::output(
        Command::new(&tools.tailscale).args(["serve", "status", "--json"]),
        TOOL_DEADLINE,
        "tailscale",
    ) else {
        return Vec::new();
    };
    let Ok(config) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return Vec::new();
    };
    let in_use: Vec<u16> = routes(tools).iter().map(|route| route.port).collect();
    let mut lines = Vec::new();
    let Some(web) = config.get("Web").and_then(|web| web.as_object()) else {
        return lines;
    };
    for (host_port, entry) in web {
        let Some(serve_port) = host_port.rsplit(':').next() else {
            continue;
        };
        let https = config["TCP"][serve_port]["HTTPS"].as_bool() == Some(true);
        let Some(handlers) = entry.get("Handlers").and_then(|h| h.as_object()) else {
            continue;
        };
        let target = match (handlers.len(), handlers.get("/")) {
            (1, Some(handler)) => handler.get("Proxy").and_then(|proxy| proxy.as_str()),
            _ => None,
        };
        let Some(port) = target.and_then(local_port) else {
            continue;
        };
        if !https || !PORTLESS_PORTS.contains(&port) || in_use.contains(&port) || listening(port) {
            continue;
        }
        let off = format!("--https={serve_port}");
        let removed = crate::deadline::output(
            Command::new(&tools.tailscale).args(["serve", &off, "off"]),
            TOOL_DEADLINE,
            "tailscale",
        );
        lines.push(match removed {
            Ok(output) if output.status.success() => format!(
                "removed a stale tailscale serve entry on port {serve_port}, for local port {port}"
            ),
            _ => format!("can't remove the stale tailscale serve entry on port {serve_port}"),
        });
    }
    lines
}

/// The port of a proxy target on this machine, such as
/// `http://127.0.0.1:4321`.
fn local_port(target: &str) -> Option<u16> {
    let rest = target.strip_prefix("http://")?.trim_end_matches('/');
    let (host, port) = rest.rsplit_once(':')?;
    matches!(host, "127.0.0.1" | "localhost")
        .then(|| port.parse().ok())
        .flatten()
}

fn listening(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok()
}

fn succeeds(command: &mut Command) -> bool {
    crate::deadline::output(command, TOOL_DEADLINE, "tool")
        .is_ok_and(|output| output.status.success())
}

/// Creates the PR's link comment, or updates it when there is one, and
/// returns its id.
fn post_link(
    tools: &Tools,
    main: &Path,
    pr: u64,
    url: &str,
    comment: Option<u64>,
) -> Result<u64, Failure> {
    let body = format!(
        "<!-- orchestral -->\nPreview: {url}\n\nThis link is updated when the preview restarts."
    );
    let body = format!("body={body}");
    match comment {
        Some(id) => {
            let path = format!("repos/{{owner}}/{{repo}}/issues/comments/{id}");
            github::gh_output(
                &tools.gh,
                main,
                &["api", "-X", "PATCH", &path, "-f", &body, "--silent"],
            )?;
            Ok(id)
        }
        None => {
            let path = format!("repos/{{owner}}/{{repo}}/issues/{pr}/comments");
            let id =
                github::gh_output(&tools.gh, main, &["api", &path, "-f", &body, "--jq", ".id"])?;
            String::from_utf8_lossy(&id)
                .trim()
                .parse()
                .map_err(|_| Failure::Error("gh api didn't give the comment's id".into()))
        }
    }
}

fn is_alive(server: &Server) -> bool {
    matches!(sessions::process(server.pid), Ok(Some(process)) if process.started == server.started)
}

/// Stops a server and its process group: SIGTERM, then SIGKILL if it's
/// still running after a grace period, so it never keeps its port. Under
/// portless, SIGINT to the route's pid comes first, which lets portless
/// remove its route and `tailscale serve` entry.
fn stop_server(server: &Server) {
    if server.portless {
        signal("INT", &server.pid.to_string());
        let deadline = Instant::now() + STOP_GRACE;
        while is_alive(server) && Instant::now() < deadline {
            thread::sleep(POLL);
        }
    }
    // The group too, for any children left behind.
    let group = format!("-{}", server.group);
    signal("TERM", &group);
    if is_alive(server) {
        signal("TERM", &server.pid.to_string());
    }
    let deadline = Instant::now() + STOP_GRACE;
    while is_alive(server) && Instant::now() < deadline {
        thread::sleep(POLL);
    }
    if is_alive(server) {
        signal("KILL", &group);
        signal("KILL", &server.pid.to_string());
    }
}

/// Sends `signal` to a pid, or to a process group as `-<group>`.
fn signal(signal: &str, target: &str) {
    let _ = Command::new("kill")
        .args([&format!("-{signal}"), "--", target])
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
