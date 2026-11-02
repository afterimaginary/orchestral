//! PR previews: `try`, `previews` and `try --stop` against stub `gh`, and
//! the supervisor's reconcile against a small test server, on `$PORT` or
//! under a stand-in portless.

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use common::Stubs;
use orchestral::github::Snapshot;
use orchestral::preview::{self, Tools};
use orchestral::state::{State, Store, Want, Worker};
use serde_json::{Value, json};
use tempfile::TempDir;

/// Serves the worktree's files on `$PORT`.
const SERVER: &str = "exec python3 -m http.server \"$PORT\" --bind 127.0.0.1";

struct Repo {
    _dir: TempDir,
    main: PathBuf,
    store: Store,
    stubs: Stubs,
}

impl Drop for Repo {
    /// Stops any server a failed test left running.
    fn drop(&mut self) {
        if let Ok(state) = self.store.read() {
            for server in state.previews.values().filter_map(|p| p.server.as_ref()) {
                let _ = Command::new("kill")
                    .args(["-TERM", &server.pid.to_string()])
                    .stderr(std::process::Stdio::null())
                    .status();
            }
        }
    }
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// A repository with a policy whose `preview` section is `preview`, and a
/// worker on `branch-<n>` for each PR in `prs`, each in a worktree of its
/// own with an `index.html` saying which PR it is.
fn repo(preview: &str, prs: &[u64]) -> Repo {
    let dir = TempDir::new().unwrap();
    let main = dir.path().canonicalize().unwrap().join("main");
    std::fs::create_dir_all(main.join(".orchestral")).unwrap();
    std::fs::write(
        main.join(".orchestral/policy.md"),
        format!("---\nfleet_token_budget: 1000\npreview:\n{preview}---\n"),
    )
    .unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    git(&main, &["add", "."]);
    git(
        &main,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "init",
        ],
    );
    let store = Store::for_repo(&main).unwrap();
    let stubs = Stubs::new();
    for &pr in prs {
        let branch = format!("branch-{pr}");
        let worktree = dir.path().canonicalize().unwrap().join(&branch);
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &branch,
                worktree.to_str().unwrap(),
            ],
        );
        std::fs::write(worktree.join("index.html"), format!("PR {pr}\n")).unwrap();
        store
            .update(|state| {
                state.workers.insert(
                    branch.clone(),
                    Worker {
                        session_name: format!("main-{branch}"),
                        issue: pr,
                        model: "m".into(),
                        effort: "e".into(),
                        worktree,
                        branch: branch.clone(),
                        job_id: None,
                        review_of: None,
                        created_at: "2026-11-02T00:00:00Z".into(),
                    },
                );
            })
            .unwrap();
        stubs.respond(
            "gh",
            &["pr", "view", &pr.to_string()],
            &json!({"state": "OPEN", "headRefName": branch}).to_string(),
            0,
        );
    }
    Repo {
        _dir: dir,
        main,
        store,
        stubs,
    }
}

impl Repo {
    fn run(&self, args: &[&str]) -> Output {
        self.stubs
            .orchestral()
            .args(args)
            .current_dir(&self.main)
            .output()
            .unwrap()
    }

    fn stdout(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn reconcile(&self) -> Vec<String> {
        let tools = Tools {
            gh: self.stubs.dir().join("gh"),
            portless: self.stubs.dir().join("portless"),
            tailscale: self.stubs.dir().join("tailscale"),
        };
        preview::reconcile(&self.store, &tools).unwrap()
    }

    /// Replaces the `portless` stub with a stand-in that runs servers.
    fn use_portless(&self) {
        for (from, to) in [
            ("tests/stubs/portless-fake.sh", "portless"),
            ("tests/stubs/portless-route.py", "portless-route.py"),
        ] {
            let source = Path::new(env!("CARGO_MANIFEST_DIR")).join(from);
            std::fs::copy(source, self.stubs.dir().join(to)).unwrap();
        }
    }

    /// The pid the stand-in portless lists for a route, if it's listed.
    fn route_pid(&self, name: &str) -> Option<u32> {
        let route = self.stubs.dir().join(format!("routes/{name}.route"));
        let text = std::fs::read_to_string(route).ok()?;
        text.split(' ').nth(2)?.parse().ok()
    }

    fn state(&self) -> State {
        self.store.read().unwrap()
    }

    fn port(&self, pr: u64) -> u16 {
        self.state().previews[&pr].server.as_ref().unwrap().port
    }

    fn pid(&self, pr: u64) -> u32 {
        self.state().previews[&pr].server.as_ref().unwrap().pid
    }

    fn url(&self, pr: u64) -> Option<String> {
        self.state().previews[&pr]
            .server
            .as_ref()
            .unwrap()
            .url
            .clone()
    }
}

/// The body served on `port`, waiting for the server to come up.
fn fetch(port: u16) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
            stream
                .write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
                .unwrap();
            let mut response = String::new();
            let _ = stream.read_to_string(&mut response);
            return response.split("\r\n\r\n").nth(1).map(str::to_owned);
        }
        thread::sleep(Duration::from_millis(100));
    }
    None
}

/// Whether nothing accepts connections on `port` within a few seconds.
fn closed(port: u16) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_err() {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    false
}

#[test]
fn a_preview_starts_serves_its_worktree_and_stops() {
    let repo = repo(&format!("  run: '{SERVER}'\n"), &[42]);

    let tried = repo.stdout(&["try", "42"]);
    assert!(
        tried.starts_with("Asked for a preview of PR #42."),
        "{tried}"
    );
    assert!(tried.contains("No supervisor is running"), "{tried}");
    assert_eq!(
        repo.stdout(&["previews"]).split(' ').nth(2),
        Some("starting")
    );

    repo.reconcile();
    let port = repo.port(42);
    assert_eq!(fetch(port).as_deref(), Some("PR 42\n"));
    let listed = repo.stdout(&["previews"]);
    assert!(
        listed.starts_with(&format!("PR #42 running at http://localhost:{port} (log ")),
        "{listed}"
    );
    // A running server is left alone, as after a supervisor restart.
    let pid = repo.pid(42);
    assert!(repo.reconcile().is_empty());
    assert_eq!(repo.pid(42), pid);

    assert_eq!(
        repo.stdout(&["try", "42", "--stop"]),
        "Asked the supervisor to stop PR #42's preview.\n"
    );
    assert_eq!(repo.reconcile(), ["stopped PR #42's preview"]);
    assert!(closed(port));
    assert_eq!(repo.state().previews[&42].want, Want::Off);
    assert!(
        repo.stdout(&["previews"])
            .starts_with("PR #42 stopped (log ")
    );
}

#[test]
fn past_max_running_the_oldest_preview_stops() {
    let repo = repo("  run: 'exec sleep 600'\n  max_running: 1\n", &[1, 2]);

    repo.stdout(&["try", "1"]);
    repo.reconcile();
    let tried = repo.stdout(&["try", "2"]);

    assert!(
        tried.contains("\nStopped PR #1's preview to make room; `orchestral try 1` restarts it."),
        "{tried}"
    );
    repo.reconcile();
    let state = repo.state();
    assert_eq!(state.previews[&1].want, Want::MadeRoom);
    assert_eq!(state.previews[&1].server, None);
    assert!(state.previews[&2].server.is_some());
    let listed = repo.stdout(&["previews"]);
    assert!(
        listed.starts_with(
            "PR #1 stopped: stopped to make room; `orchestral try 1` restarts it (log "
        ),
        "{listed}"
    );

    // Asking again restarts it, and makes room in turn.
    repo.stdout(&["try", "1"]);
    repo.reconcile();
    let state = repo.state();
    assert!(state.previews[&1].server.is_some());
    assert_eq!(state.previews[&2].want, Want::MadeRoom);
    repo.stdout(&["try", "1", "--stop"]);
    repo.reconcile();
}

#[test]
fn a_crash_restarts_once_then_waits_for_a_new_commit() {
    let repo = repo("  run: 'echo crashed; exit 1'\n", &[7]);
    repo.stdout(&["try", "7"]);
    let worktree = repo.state().previews[&7].worktree.clone();

    let first = repo.reconcile();
    thread::sleep(Duration::from_millis(300));
    let second = repo.reconcile();
    thread::sleep(Duration::from_millis(300));
    let third = repo.reconcile();

    assert!(first[0].starts_with("started PR #7's preview"), "{first:?}");
    assert_eq!(second[0], "PR #7's preview exited; starting it again");
    assert!(
        third[0].starts_with("PR #7's preview exited twice; see "),
        "{third:?}"
    );
    let events = std::fs::read_to_string(repo.store.dir().join("events.jsonl")).unwrap();
    let event: Value = serde_json::from_str(events.lines().last().unwrap()).unwrap();
    assert_eq!(event["kind"], "preview");
    let log = std::fs::read_to_string(repo.store.dir().join("logs/preview-7.log")).unwrap();
    assert_eq!(log, "crashed\ncrashed\n");
    assert!(
        repo.stdout(&["previews"])
            .starts_with("PR #7 failed: it exited twice")
    );
    // Failed, it stays down and doesn't keep the supervisor running.
    assert!(repo.reconcile().is_empty());
    assert!(!preview::wanted(&repo.state()));

    std::fs::write(worktree.join("fix.txt"), "fix").unwrap();
    git(&worktree, &["add", "."]);
    git(
        &worktree,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "fix",
        ],
    );
    let after = repo.reconcile();
    assert_eq!(
        after[0],
        "PR #7 has a new commit; starting its preview again"
    );
}

#[test]
fn setup_runs_once_and_again_when_a_lockfile_changes() {
    let repo = repo(
        "  setup: 'echo setup >> setups.txt'\n  run: 'exec sleep 600'\n",
        &[3],
    );
    repo.stdout(&["try", "3"]);
    let worktree = repo.state().previews[&3].worktree.clone();

    repo.reconcile();
    let pid = repo.pid(3);
    repo.reconcile();
    assert_eq!(
        std::fs::read_to_string(worktree.join("setups.txt")).unwrap(),
        "setup\n"
    );

    std::fs::write(worktree.join("package-lock.json"), "{}").unwrap();
    let lines = repo.reconcile();

    assert_eq!(
        lines[0],
        "PR #3's lockfiles changed; setting its preview up again"
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("setups.txt")).unwrap(),
        "setup\nsetup\n"
    );
    assert_ne!(repo.pid(3), pid);
    repo.stdout(&["try", "3", "--stop"]);
    repo.reconcile();
}

#[test]
fn a_failed_setup_records_an_event_and_starts_nothing() {
    let repo = repo("  setup: 'exit 3'\n  run: 'exec sleep 600'\n", &[4]);
    repo.stdout(&["try", "4"]);

    let lines = repo.reconcile();

    assert!(
        lines[0].starts_with("PR #4's preview setup failed: it exited with"),
        "{lines:?}"
    );
    assert_eq!(repo.state().previews[&4].server, None);
    assert!(repo.reconcile().is_empty());
    assert!(
        repo.stdout(&["previews"])
            .starts_with("PR #4 failed: its setup failed;")
    );
}

#[test]
fn previews_of_merged_or_closed_prs_stop() {
    let repo = repo("  run: 'exec sleep 600'\n", &[5, 6]);
    repo.stdout(&["try", "5"]);
    repo.stdout(&["try", "6"]);
    repo.reconcile();
    let snapshot = |open: &[u64]| {
        let snapshot = Snapshot {
            polled_at: Some(jiff::Timestamp::now()),
            ..Snapshot::default()
        };
        let mut value = serde_json::to_value(&snapshot).unwrap();
        for pr in open {
            value["prs"][pr.to_string()] = json!({
                "title": "t", "draft": false, "branch": format!("branch-{pr}"),
                "head": "abc", "ci": "passed", "reviews": []
            });
        }
        std::fs::write(repo.store.dir().join("github.json"), value.to_string()).unwrap();
    };

    snapshot(&[6]);
    let lines = preview::stop_closed(&repo.store).unwrap();
    repo.reconcile();

    assert_eq!(lines, ["PR #5 merged or closed; stopping its preview"]);
    let state = repo.state();
    assert!(!state.previews.contains_key(&5));
    assert!(state.previews[&6].server.is_some());
    repo.stdout(&["try", "6", "--stop"]);
    repo.reconcile();
}

#[test]
fn a_hosted_preview_gives_its_link_and_starts_nothing() {
    let repo = repo("  url: 'https://{branch}.example.dev/?pr={pr}'\n", &[9]);

    assert_eq!(
        repo.stdout(&["try", "9"]),
        "PR #9's preview is at https://branch-9.example.dev/?pr=9\n"
    );
    assert!(repo.state().previews.is_empty());
}

#[test]
fn try_refuses_without_a_preview_policy_or_a_worker() {
    let repo = repo("  run: 'exec sleep 600'\n", &[]);
    repo.stubs.respond(
        "gh",
        &["pr", "view", "8"],
        &json!({"state": "OPEN", "headRefName": "elsewhere"}).to_string(),
        0,
    );
    repo.stubs.respond(
        "gh",
        &["pr", "view", "10"],
        &json!({"state": "MERGED", "headRefName": "x"}).to_string(),
        0,
    );

    let no_worker = repo.run(&["try", "8"]);
    let merged = repo.run(&["try", "10"]);
    std::fs::write(
        repo.main.join(".orchestral/policy.md"),
        "---\nfleet_token_budget: 1000\n---\n",
    )
    .unwrap();
    let no_policy = repo.run(&["try", "8"]);

    for (output, reason) in [
        (
            no_worker,
            "no worker has PR #8's branch elsewhere checked out",
        ),
        (merged, "PR #10 is merged"),
        (no_policy, "the policy has no `preview`"),
    ] {
        assert_eq!(output.status.code(), Some(2));
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.starts_with(&format!("refused: {reason}\n")),
            "{stderr}"
        );
    }
}

#[test]
fn setup_runs_once_per_worktree_across_a_stop_and_a_lockfile_setup_writes() {
    // Setup writes a lockfile, as `npm install` does with none committed.
    let repo = repo(
        "  setup: 'echo setup >> setups.txt; echo {} > package-lock.json'\n  run: 'exec sleep 600'\n",
        &[11],
    );
    repo.stdout(&["try", "11"]);
    let worktree = repo.state().previews[&11].worktree.clone();

    repo.reconcile();
    let pid = repo.pid(11);
    assert!(repo.reconcile().is_empty());
    assert_eq!(repo.pid(11), pid);
    repo.stdout(&["try", "11", "--stop"]);
    repo.reconcile();
    repo.stdout(&["try", "11"]);
    repo.reconcile();

    assert_eq!(
        std::fs::read_to_string(worktree.join("setups.txt")).unwrap(),
        "setup\n"
    );
    repo.stdout(&["try", "11", "--stop"]);
    repo.reconcile();
}

#[test]
fn a_missing_worktree_fails_once_without_stopping_the_others() {
    let repo = repo("  run: 'exec sleep 600'\n", &[12, 13]);
    repo.stdout(&["try", "12"]);
    repo.stdout(&["try", "13"]);
    let gone = repo.state().previews[&12].worktree.clone();
    std::fs::remove_dir_all(&gone).unwrap();

    let lines = repo.reconcile();

    assert_eq!(
        lines[0],
        format!(
            "PR #12's preview stopped: its worktree {} can't be read",
            gone.display()
        )
    );
    assert!(repo.state().previews[&13].server.is_some());
    assert!(repo.reconcile().is_empty());
    let listed = repo.stdout(&["previews"]);
    assert!(
        listed.starts_with("PR #12 failed: its worktree "),
        "{listed}"
    );
    repo.stdout(&["try", "13", "--stop"]);
    repo.reconcile();
}

#[test]
fn a_policy_without_previews_stops_them() {
    let repo = repo("  run: 'exec sleep 600'\n", &[14]);
    repo.stdout(&["try", "14"]);
    repo.reconcile();
    std::fs::write(
        repo.main.join(".orchestral/policy.md"),
        "---\nfleet_token_budget: 1000\n---\n",
    )
    .unwrap();

    let lines = repo.reconcile();

    assert_eq!(
        lines,
        ["stopped PR #14's preview: the policy no longer runs previews"]
    );
    let state = repo.state();
    assert_eq!(state.previews[&14].server, None);
    assert!(!preview::wanted(&state));
}

/// A local port in portless's range that nothing listens on.
fn unused_portless_port() -> u16 {
    (4000..=4999)
        .find(|&port| {
            TcpListener::bind(("127.0.0.1", port)).is_ok()
                && TcpStream::connect(("127.0.0.1", port)).is_err()
        })
        .expect("a free port from 4000 to 4999")
}

#[test]
fn under_portless_a_preview_posts_its_tailnet_link_and_stops_with_sigint() {
    let repo = repo(&format!("  run: '{SERVER}'\n"), &[42]);
    repo.use_portless();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let busy = TcpListener::bind(("127.0.0.1", unused_portless_port())).unwrap();
    let busy_port = busy.local_addr().unwrap().port();
    let stale = unused_portless_port();
    let proxy =
        |port: u16| json!({"Handlers": {"/": {"Proxy": format!("http://127.0.0.1:{port}")}}});
    let serve = json!({
        "TCP": {"443": {"HTTPS": true}, "8443": {"HTTPS": true}, "8444": {"HTTPS": true}, "8445": {"HTTPS": true}},
        "Web": {
            // Not a port portless hands out.
            "host.example.ts.net:443": proxy(listener.local_addr().unwrap().port()),
            // Stale: nothing listens, and no route uses it.
            "host.example.ts.net:8443": proxy(stale),
            // Something listens there.
            "host.example.ts.net:8444": proxy(busy_port),
            // More than a single proxy.
            "host.example.ts.net:8445": {"Handlers": {
                "/": {"Proxy": format!("http://127.0.0.1:{stale}")},
                "/api": {"Proxy": "http://127.0.0.1:3000"},
            }},
        },
    });
    drop(listener);
    repo.stubs.respond("tailscale", &["status"], "", 0);
    repo.stubs.respond(
        "tailscale",
        &["serve", "status", "--json"],
        &serve.to_string(),
        0,
    );
    repo.stubs
        .respond("tailscale", &["serve", "--https=8443", "off"], "", 0);
    repo.stubs.respond("gh", &["api", "-X", "PATCH"], "", 0);
    repo.stubs.respond("gh", &["api"], "555\n", 0);

    repo.stdout(&["try", "42"]);
    let lines = repo.reconcile();

    assert_eq!(
        lines[0],
        format!("removed a stale tailscale serve entry on port 8443, for local port {stale}")
    );
    let port = repo.port(42);
    let url = format!("https://host.example.ts.net:{port}");
    assert_eq!(repo.url(42).as_deref(), Some(url.as_str()));
    assert_eq!(fetch(port).as_deref(), Some("PR 42\n"));
    // The route's own pid, not the wrapper's.
    assert_eq!(repo.route_pid("main-pr-42"), Some(repo.pid(42)));
    assert_eq!(repo.state().previews[&42].comment, Some(555));
    let body = format!(
        "body=<!-- orchestral -->\nPreview: {url}\n\nThis link is updated when the preview restarts."
    );
    assert!(repo.stubs.calls("gh").contains(&vec![
        "api".to_owned(),
        "repos/{owner}/{repo}/issues/42/comments".into(),
        "-f".into(),
        body,
        "--jq".into(),
        ".id".into(),
    ]));
    assert_eq!(
        repo.stdout(&["previews"]).split(" (log").next(),
        Some(format!("PR #42 running at {url}").as_str())
    );

    // A crash: the restarted server's new link replaces the old one.
    let group = repo.state().previews[&42].server.as_ref().unwrap().group;
    Command::new("kill")
        .args(["-KILL", "--", &format!("-{group}")])
        .status()
        .unwrap();
    assert!(closed(port));
    let lines = repo.reconcile();
    assert!(
        lines.contains(&"PR #42's preview exited; starting it again".to_owned()),
        "{lines:?}"
    );
    let port = repo.port(42);
    let url = format!("https://host.example.ts.net:{port}");
    assert_eq!(repo.url(42).as_deref(), Some(url.as_str()));
    let patch = repo.stubs.calls("gh").pop().unwrap();
    assert_eq!(
        patch[..4],
        [
            "api",
            "-X",
            "PATCH",
            "repos/{owner}/{repo}/issues/comments/555"
        ]
    );
    assert!(patch[5].contains(&url), "{patch:?}");

    repo.stdout(&["try", "42", "--stop"]);
    assert_eq!(repo.reconcile(), ["stopped PR #42's preview"]);
    // The route went away, which only portless's SIGINT handling does.
    assert_eq!(repo.route_pid("main-pr-42"), None);
    assert!(closed(port));

    let calls = repo.stubs.calls("tailscale");
    let removed: Vec<_> = calls
        .iter()
        .filter(|call| call.last().map(String::as_str) == Some("off"))
        .collect();
    // Only the stale entry, before each start, since the stub still lists it.
    assert!(!removed.is_empty());
    assert!(
        removed
            .iter()
            .all(|call| call[..] == ["serve", "--https=8443", "off"]),
        "{removed:?}"
    );
    assert!(!calls.iter().flatten().any(|arg| arg == "reset"));
    drop(busy);
}

#[test]
fn without_tailscale_a_portless_preview_has_a_local_link_and_no_comment() {
    let repo = repo(&format!("  run: '{SERVER}'\n"), &[43]);
    repo.use_portless();
    repo.stubs.respond("tailscale", &["status"], "", 1);

    repo.stdout(&["try", "43"]);
    repo.reconcile();

    assert_eq!(
        repo.url(43).as_deref(),
        Some("https://main-pr-43.localhost")
    );
    assert_eq!(fetch(repo.port(43)).as_deref(), Some("PR 43\n"));
    assert_eq!(repo.stubs.calls("tailscale"), [["status"]]);
    assert!(!repo.stubs.calls("gh").iter().any(|call| call[0] == "api"));
    repo.stdout(&["try", "43", "--stop"]);
    repo.reconcile();
    assert_eq!(repo.route_pid("main-pr-43"), None);
}

#[test]
fn when_portless_runs_no_route_the_preview_falls_back_to_port() {
    let repo = repo(&format!("  run: '{SERVER}'\n"), &[44]);
    repo.stubs
        .respond("portless", &["--version"], "0.15.6\n", 0);
    repo.stubs
        .respond("portless", &["list"], "No active routes.\n", 0);
    repo.stubs.respond("portless", &["main-pr-44"], "", 1);
    repo.stubs.respond("tailscale", &["status"], "", 1);

    repo.stdout(&["try", "44"]);
    let lines = repo.reconcile();

    assert!(
        lines[0]
            .starts_with("portless didn't list the route main-pr-44, so it runs on $PORT instead"),
        "{lines:?}"
    );
    assert_eq!(repo.url(44), None);
    assert!(!repo.state().previews[&44].server.as_ref().unwrap().portless);
    assert_eq!(fetch(repo.port(44)).as_deref(), Some("PR 44\n"));
}
