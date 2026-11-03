//! `orchestral try --check`: the boot check a worker runs before its PR
//! reaches the owner. It runs the policy's preview `setup` and `run` in the
//! worker's own worktree, waits for an HTTP answer that isn't a server error,
//! stops the server, and says whether it booted. A PR whose preview can't
//! start shouldn't wait on the owner.

use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::outcome::Failure;
use crate::policy::{Policy, PreviewStart};
use crate::preview;
use crate::state::Store;

/// How long the server has to answer.
const BOOT_DEADLINE: Duration = Duration::from_secs(120);

/// How long it has to stop after SIGTERM.
const STOP_GRACE: Duration = Duration::from_secs(5);

const POLL: Duration = Duration::from_millis(250);

/// The log lines a failure quotes.
const TAIL: usize = 20;

#[derive(Debug, Serialize)]
pub struct Booted {
    /// The HTTP status it answered with.
    pub status: u16,
    pub seconds: u64,
    pub log: PathBuf,
}

impl fmt::Display for Booted {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "The preview booted: it answered {} after {}s, and it's stopped again. Its log is {}.",
            self.status,
            self.seconds,
            self.log.display()
        )
    }
}

/// `orchestral try --check`, from a worker's worktree.
pub fn check(cwd: &Path) -> Result<Booted, Failure> {
    let worker = std::env::var("ORCHESTRAL_WORKER").unwrap_or_default();
    if worker.is_empty() {
        return Err(Failure::Refused {
            reason: "only a worker runs the boot check".into(),
            fix: "ask for a running preview with `orchestral try <pr>` instead".into(),
        });
    }
    // Read from `origin/main`, so a worker's branch can't change what's run.
    let preview = Policy::load(cwd)?
        .settings
        .preview
        .ok_or_else(|| Failure::Refused {
            reason: "the policy has no `preview`, so there's nothing to boot".into(),
            fix: "carry on without the check".into(),
        })?;
    let run = match preview.start {
        PreviewStart::Run(run) => run,
        PreviewStart::Url(_) => {
            return Err(Failure::Refused {
                reason: "the host serves the previews, so nothing boots here".into(),
                fix: "check the PR's preview link from the host instead".into(),
            });
        }
    };
    let worktree = PathBuf::from(crate::git::git(cwd, &["rev-parse", "--show-toplevel"])?.trim());
    let store = Store::for_repo(cwd)?;
    let log = store.dir().join("logs").join(format!("boot-{worker}.log"));
    if let Some(dir) = log.parent() {
        fs::create_dir_all(dir)
            .map_err(|err| Failure::Error(format!("{}: {err}", dir.display())))?;
    }
    // Each check starts a fresh log.
    let file = fs::File::create(&log)
        .map_err(|err| Failure::Error(format!("{}: {err}", log.display())))?;
    drop(file);
    let failed = |why: String| Failure::Refused {
        reason: format!("the preview didn't boot: {why}{}", tail(&log)),
        fix: format!(
            "fix it and run the check again; the full log is {}",
            log.display()
        ),
    };
    if let Some(setup) = &preview.setup {
        preview::run_setup(setup, &worktree, &log)
            .map_err(|why| failed(format!("setup failed: {why}")))?;
    }
    let port = preview::free_port()?;
    let file = preview::open_log(&log).map_err(|err| Failure::Error(err.to_string()))?;
    let mut child = Command::new("sh")
        .args(["-c", &run])
        .current_dir(&worktree)
        .env("PORT", port.to_string())
        .stdin(Stdio::null())
        .stdout(
            file.try_clone()
                .map_err(|err| Failure::Error(err.to_string()))?,
        )
        .stderr(file)
        .process_group(0)
        .spawn()
        .map_err(|err| failed(format!("can't run it: {err}")))?;
    let group = format!("-{}", child.id());
    let started = Instant::now();
    let outcome = loop {
        if let Ok(Some(status)) = child.try_wait() {
            break Err(format!("it exited with {status} before answering on $PORT"));
        }
        if let Some(status) = answer(port).filter(|status| *status < 500) {
            break Ok(status);
        }
        if started.elapsed() >= BOOT_DEADLINE {
            break Err(format!(
                "nothing answered on $PORT ({port}) within {}s",
                BOOT_DEADLINE.as_secs()
            ));
        }
        thread::sleep(POLL);
    };
    let seconds = started.elapsed().as_secs();
    // Stop the whole group, so the server's own children go too.
    preview::signal("TERM", &group);
    let stopping = Instant::now();
    while child.try_wait().ok().flatten().is_none() && stopping.elapsed() < STOP_GRACE {
        thread::sleep(POLL);
    }
    preview::signal("KILL", &group);
    let _ = child.wait();
    let status = outcome.map_err(failed)?;
    Ok(Booted {
        status,
        seconds,
        log,
    })
}

/// The status of a plain `GET /` on the port, or `None` while nothing
/// answers.
fn answer(port: u16) -> Option<u16> {
    // A server bound to `localhost` may listen on IPv6 only.
    let local: [IpAddr; 2] = [Ipv4Addr::LOCALHOST.into(), Ipv6Addr::LOCALHOST.into()];
    let mut stream = local.into_iter().find_map(|ip| {
        TcpStream::connect_timeout(&SocketAddr::new(ip, port), Duration::from_secs(1)).ok()
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    stream
        .write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .ok()?;
    let mut head = [0u8; 64];
    let read = stream.read(&mut head).ok()?;
    // `HTTP/1.1 200 OK`
    String::from_utf8_lossy(&head[..read])
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

/// The log's last lines, for a failure's reason.
fn tail(log: &Path) -> String {
    let text = fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    match lines.is_empty() {
        true => String::new(),
        false => format!("\n{}", lines[lines.len().saturating_sub(TAIL)..].join("\n")),
    }
}
