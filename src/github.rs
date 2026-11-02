//! The supervisor's GitHub poll: each minute it reads open issues, open PRs
//! and new comments with `gh`, compares them with the last snapshot, and
//! records one event per change.
//!
//! The snapshot, `github.json`, also holds the poll's failures and backoff,
//! so a restarted supervisor carries on from it. The first poll only takes
//! the snapshot, so a new run doesn't report everything already there.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::process::Command;

use jiff::{SignedDuration, Timestamp};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::outcome::Failure;
use crate::state::{State, Store};

/// Marks Orchestral's own comments, which are never reported back.
pub const MARKER: &str = "<!-- orchestral -->";

/// Comments are read from this long before the last poll, so one that
/// GitHub indexes late isn't missed. Ids already seen aren't reported twice.
const OVERLAP: SignedDuration = SignedDuration::from_secs(300);

/// Failed polls in a row before one event says so.
const REPORT_AFTER: u32 = 5;

/// How long one `gh` call may take before the poll fails.
const GH_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// The longest wait between polls while they keep failing.
const MAX_BACKOFF: SignedDuration = SignedDuration::from_secs(16 * 60);

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Snapshot {
    pub issues: BTreeMap<u64, Issue>,
    pub prs: BTreeMap<u64, Pr>,
    /// When the last successful poll started. Comments are read from here,
    /// less the overlap.
    pub polled_at: Option<Timestamp>,
    /// Comment ids from the overlap window, so none is reported twice.
    pub seen_comments: BTreeSet<String>,
    /// Failed polls since the last success.
    pub failures: u32,
    /// While failing, no poll runs before this.
    pub retry_after: Option<Timestamp>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Issue {
    pub title: String,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pr {
    pub title: String,
    pub draft: bool,
    pub branch: String,
    pub head: String,
    pub ci: Ci,
    /// Review ids already reported.
    pub reviews: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Ci {
    None,
    Pending,
    Passed,
    Failed,
}

/// One poll, if one is due. Returns the event lines it recorded. The poll
/// that first succeeds only takes the snapshot.
pub fn poll(store: &Store, gh: &Path) -> Result<Vec<String>, Failure> {
    let path = store.dir().join("github.json");
    let old = read_snapshot(&path)?;
    let now = Timestamp::now();
    if old
        .as_ref()
        .and_then(|old| old.retry_after)
        .is_some_and(|retry| now < retry)
    {
        return Ok(Vec::new());
    }
    let attempt = || -> Result<(Snapshot, Vec<String>), Failure> {
        let main = crate::spawn::main_checkout(store.dir())?;
        let state = store.read()?;
        let fetched = fetch(gh, &main, old.as_ref(), now)?;
        let lines = match old.as_ref().filter(|old| old.polled_at.is_some()) {
            Some(old) => changes(old, &fetched, &state, gh, &main)?,
            None => Vec::new(),
        };
        Ok((fetched.snapshot, lines))
    };
    let (snapshot, lines) = match attempt() {
        // A successful poll starts the snapshot afresh, failures included.
        Ok(result) => result,
        Err(err) => {
            let mut snapshot = old.unwrap_or_default();
            snapshot.failures += 1;
            let wait = SignedDuration::from_secs(60 << snapshot.failures.min(4));
            snapshot.retry_after = Some(now + wait.min(MAX_BACKOFF));
            let mut lines = Vec::new();
            if snapshot.failures == REPORT_AFTER {
                lines.push(format!(
                    "watcher: {REPORT_AFTER} polls failed: {}",
                    first_line(&crate::supervisor::failure_text(&err))
                ));
            }
            (snapshot, lines)
        }
    };
    // Events first, so a crash before the snapshot is saved repeats an event
    // rather than losing it.
    for line in &lines {
        let kind = line.split([' ', ':']).next().unwrap_or("github");
        store.record(kind, line)?;
    }
    write_snapshot(&path, &snapshot)?;
    Ok(lines)
}

/// What one successful read of GitHub found.
struct Fetched {
    snapshot: Snapshot,
    comments: Vec<Comment>,
}

/// A new comment or review.
struct Comment {
    number: u64,
    /// Whether `number` is a PR rather than an issue.
    on_pr: bool,
    author: String,
    body: String,
    kind: CommentKind,
}

enum CommentKind {
    /// On an issue or a PR's conversation.
    Conversation,
    /// On a line of a PR's diff.
    Inline { path: String, line: Option<u64> },
    /// A PR review, with its state such as `APPROVED`.
    Review { state: String },
}

fn fetch(
    gh: &Path,
    main: &Path,
    old: Option<&Snapshot>,
    now: Timestamp,
) -> Result<Fetched, Failure> {
    let issues: Vec<GhIssue> = gh_json(
        gh,
        main,
        &[
            "issue",
            "list",
            "--state",
            "open",
            "--limit",
            "500",
            "--json",
            "number,title,labels",
        ],
    )?;
    let prs: Vec<GhPr> = gh_json(
        gh,
        main,
        &[
            "pr",
            "list",
            "--state",
            "open",
            "--limit",
            "200",
            "--json",
            "number,title,isDraft,headRefName,headRefOid,statusCheckRollup,reviews",
        ],
    )?;
    let mut snapshot = Snapshot {
        polled_at: Some(now),
        ..Snapshot::default()
    };
    let mut comments = Vec::new();
    for issue in issues {
        snapshot.issues.insert(
            issue.number,
            Issue {
                title: issue.title,
                labels: issue.labels.into_iter().map(|label| label.name).collect(),
            },
        );
    }
    for pr in prs {
        let known = old.and_then(|old| old.prs.get(&pr.number));
        let mut reviews = BTreeSet::new();
        for review in pr.reviews {
            // A review that only holds inline comments has no body of its
            // own; its comments are reported one by one.
            // A pending review isn't submitted yet, so it's read again until
            // it is.
            if review.state == "PENDING" {
                continue;
            }
            let empty = review.body.trim().is_empty() && review.state == "COMMENTED";
            if !empty
                && !known.is_some_and(|known| known.reviews.contains(&review.id))
                && !is_ours(&review.body)
            {
                comments.push(Comment {
                    number: pr.number,
                    on_pr: true,
                    author: review.author.map(|author| author.login).unwrap_or_default(),
                    body: review.body,
                    kind: CommentKind::Review {
                        state: review.state,
                    },
                });
            }
            reviews.insert(review.id);
        }
        snapshot.prs.insert(
            pr.number,
            Pr {
                title: pr.title,
                draft: pr.is_draft,
                branch: pr.head_ref_name,
                head: pr.head_ref_oid,
                ci: ci(&pr.status_check_rollup),
                reviews,
            },
        );
    }
    // Comments are read only after a first snapshot, from where the last
    // successful poll started.
    if let Some(since) = old.and_then(|old| old.polled_at) {
        let since = since - OVERLAP;
        // Whole seconds, rounded down, as GitHub expects.
        let since_param = format!(
            "since={}",
            Timestamp::from_second(since.as_second()).unwrap_or(since)
        );
        // After downtime or backoff the window can hold more than a page.
        let comments_at = |endpoint: &str| -> Result<Vec<GhComment>, Failure> {
            gh_pages(
                gh,
                main,
                &[
                    "api",
                    "--method",
                    "GET",
                    "--paginate",
                    &format!("repos/{{owner}}/{{repo}}/{endpoint}"),
                    "-f",
                    &since_param,
                    "-f",
                    "per_page=100",
                ],
            )
        };
        let conversation = comments_at("issues/comments")?;
        let inline = comments_at("pulls/comments")?;
        let seen = old.map(|old| &old.seen_comments);
        let mut add = |prefix: &str, comment: GhComment, inline: bool| {
            // The query returns comments updated since, so an old comment
            // edited now is left out.
            if comment.created_at < since {
                return;
            }
            let id = format!("{prefix}{}", comment.id);
            let new = !seen.is_some_and(|seen| seen.contains(&id));
            snapshot.seen_comments.insert(id);
            let url = if inline {
                &comment.pull_request_url
            } else {
                &comment.issue_url
            };
            let Some(number) = url.as_deref().and_then(number_from_url) else {
                return;
            };
            if !new || is_ours(&comment.body) {
                return;
            }
            // A PR's conversation comments come from the issues endpoint
            // too, and only their page link tells them apart.
            let on_pr = inline
                || comment
                    .html_url
                    .as_deref()
                    .is_some_and(|url| url.contains("/pull/"));
            comments.push(Comment {
                number,
                on_pr,
                author: comment.user.map(|user| user.login).unwrap_or_default(),
                body: comment.body,
                kind: if inline {
                    CommentKind::Inline {
                        path: comment.path.unwrap_or_default(),
                        line: comment.line,
                    }
                } else {
                    CommentKind::Conversation
                },
            });
        };
        for comment in conversation {
            add("c", comment, false);
        }
        for comment in inline {
            add("r", comment, true);
        }
    }
    Ok(Fetched { snapshot, comments })
}

/// The event lines between the last snapshot and this poll.
fn changes(
    old: &Snapshot,
    new: &Fetched,
    state: &State,
    gh: &Path,
    main: &Path,
) -> Result<Vec<String>, Failure> {
    let mut lines = Vec::new();
    let worker_for_issue = |number: u64| {
        state
            .workers
            .iter()
            .find(|(_, worker)| worker.issue == number)
            .map(|(name, _)| name.as_str())
    };
    let worker_for_branch = |branch: &str| {
        state
            .workers
            .iter()
            .find(|(_, worker)| worker.branch == branch)
            .map(|(name, _)| name.as_str())
    };

    for (number, issue) in &new.snapshot.issues {
        if !old.issues.contains_key(number) {
            let labels = match issue.labels.is_empty() {
                true => String::new(),
                false => format!(" [{}]", issue.labels.join(", ")),
            };
            lines.push(format!(
                "issue #{number} \"{}\" opened{labels}: decide whether it's ready for a worker",
                issue.title
            ));
        }
    }
    for (number, issue) in &old.issues {
        if !new.snapshot.issues.contains_key(number) {
            lines.push(match worker_for_issue(*number) {
                Some(worker) => format!(
                    "issue #{number} \"{}\" closed (worker {worker}): stop the worker if it's still on it",
                    issue.title
                ),
                None => format!("issue #{number} \"{}\" closed: no action needed", issue.title),
            });
        }
    }

    for (number, pr) in &new.snapshot.prs {
        let worker = worker_for_branch(&pr.branch);
        let subject = pr_subject(*number, pr, worker);
        let before = old.prs.get(number);
        if before.is_none() {
            lines.push(match (pr.draft, worker) {
                (true, Some(_)) => format!("{subject}: opened as a draft: no action needed"),
                (true, None) => format!("{subject}: opened as a draft: decide what to do with it"),
                (false, _) => format!("{subject}: opened, ready for review: route it for review"),
            });
        } else if before.is_some_and(|before| before.draft) && !pr.draft {
            lines.push(format!("{subject}: ready for review: route it for review"));
        }
        // A result is new when it changed, or came from a new push.
        let ci_before = before.map_or(Ci::None, |before| before.ci);
        if pr.ci != ci_before || before.is_some_and(|before| before.head != pr.head) {
            match (pr.ci, worker) {
                (Ci::Failed, Some(worker)) => {
                    lines.push(format!("{subject}: CI failed: have {worker} fix it"))
                }
                (Ci::Failed, None) => {
                    lines.push(format!("{subject}: CI failed: check whether it matters"))
                }
                (Ci::Passed, _) => lines.push(format!("{subject}: CI passed: no action needed")),
                _ => {}
            }
        }
    }
    for (number, pr) in &old.prs {
        if new.snapshot.prs.contains_key(number) {
            continue;
        }
        let worker = worker_for_branch(&pr.branch);
        let subject = pr_subject(*number, pr, worker);
        // Failing here fails the poll, so the outcome is read again rather
        // than guessed.
        let merged = gh_json::<GhPrState>(
            gh,
            main,
            &["pr", "view", &number.to_string(), "--json", "state"],
        )?
        .state
            == "MERGED";
        lines.push(match (merged, worker) {
            (true, Some(worker)) => format!("{subject}: merged: clean up {worker}"),
            (true, None) => format!("{subject}: merged: no action needed"),
            (false, Some(worker)) => {
                format!("{subject}: closed without merging: stop {worker} if it's still on it")
            }
            (false, None) => format!("{subject}: closed without merging: no action needed"),
        });
    }

    for comment in &new.comments {
        let quoted = quote(&comment.body);
        let author = &comment.author;
        let pr = new
            .snapshot
            .prs
            .get(&comment.number)
            .or_else(|| old.prs.get(&comment.number));
        let (subject, worker) = match pr {
            Some(pr) => {
                let worker = worker_for_branch(&pr.branch);
                (pr_subject(comment.number, pr, worker), worker)
            }
            // A PR closed before the last poll is no longer in either
            // snapshot.
            None if comment.on_pr => (format!("pr #{}", comment.number), None),
            None => {
                let worker = worker_for_issue(comment.number);
                let title = new
                    .snapshot
                    .issues
                    .get(&comment.number)
                    .or_else(|| old.issues.get(&comment.number))
                    .map(|issue| format!(" \"{}\"", issue.title))
                    .unwrap_or_default();
                let subject = match worker {
                    Some(worker) => format!("issue #{}{title} (worker {worker})", comment.number),
                    None => format!("issue #{}{title}", comment.number),
                };
                (subject, worker)
            }
        };
        let action = match worker {
            Some(worker) => format!("pass it to {worker}"),
            None => "answer it if it needs you".to_owned(),
        };
        let what = match &comment.kind {
            CommentKind::Conversation => format!("comment from {author}: {quoted}"),
            CommentKind::Inline { path, line } => match line {
                Some(line) => format!("inline comment from {author} on {path}:{line}: {quoted}"),
                None => format!("inline comment from {author} on {path}: {quoted}"),
            },
            CommentKind::Review { state } => {
                let verdict = match state.as_str() {
                    "APPROVED" => "approved",
                    "CHANGES_REQUESTED" => "changes requested",
                    _ => "commented",
                };
                match comment.body.trim().is_empty() {
                    true => format!("review from {author}, {verdict}"),
                    false => format!("review from {author}, {verdict}: {quoted}"),
                }
            }
        };
        lines.push(format!("{subject}: {what}: {action}"));
    }
    Ok(lines)
}

fn pr_subject(number: u64, pr: &Pr, worker: Option<&str>) -> String {
    match worker {
        Some(worker) => format!("pr #{number} \"{}\" (worker {worker})", pr.title),
        None => format!("pr #{number} \"{}\" (branch {})", pr.title, pr.branch),
    }
}

/// The CI result for a PR's head, from its status check rollup.
fn ci(checks: &[GhCheck]) -> Ci {
    if checks.is_empty() {
        return Ci::None;
    }
    let failed = |result: &str| {
        matches!(
            result,
            "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE"
        )
    };
    let mut pending = false;
    for check in checks {
        // A check run has a status and a conclusion; a commit status has a
        // state.
        let result = check
            .conclusion
            .as_deref()
            .or(check.state.as_deref())
            .unwrap_or("");
        if failed(result) {
            return Ci::Failed;
        }
        let complete = match &check.status {
            Some(status) => status == "COMPLETED",
            None => !matches!(result, "PENDING" | "EXPECTED" | ""),
        };
        pending |= !complete;
    }
    if pending { Ci::Pending } else { Ci::Passed }
}

fn is_ours(body: &str) -> bool {
    body.trim_start().starts_with(MARKER)
}

/// A comment's first line with text, quoted and cut to a readable length.
fn quote(body: &str) -> String {
    let line = first_line(body);
    let mut quoted: String = line.chars().take(160).collect();
    if quoted.len() < line.len() {
        quoted.push('…');
    }
    format!("\"{quoted}\"")
}

fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
}

/// The issue or PR number at the end of an API URL.
fn number_from_url(url: &str) -> Option<u64> {
    url.rsplit('/').next()?.parse().ok()
}

fn gh_json<T: DeserializeOwned>(gh: &Path, main: &Path, args: &[&str]) -> Result<T, Failure> {
    let stdout = gh_output(gh, main, args)?;
    serde_json::from_slice(&stdout).map_err(|err| unexpected(args, err))
}

/// Every page of a `gh api --paginate` list. gh prints each page as its own
/// JSON array.
fn gh_pages<T: DeserializeOwned>(gh: &Path, main: &Path, args: &[&str]) -> Result<Vec<T>, Failure> {
    let stdout = gh_output(gh, main, args)?;
    let mut items = Vec::new();
    for page in serde_json::Deserializer::from_slice(&stdout).into_iter::<Vec<T>>() {
        items.extend(page.map_err(|err| unexpected(args, err))?);
    }
    Ok(items)
}

fn gh_output(gh: &Path, main: &Path, args: &[&str]) -> Result<Vec<u8>, Failure> {
    let output = crate::deadline::output(
        Command::new(gh).args(args).current_dir(main),
        GH_DEADLINE,
        "gh",
    )
    .map_err(Failure::Error)?;
    if !output.status.success() {
        return Err(Failure::Error(format!(
            "gh {} failed: {}",
            args[..2.min(args.len())].join(" "),
            first_line(&String::from_utf8_lossy(&output.stderr))
        )));
    }
    Ok(output.stdout)
}

fn unexpected(args: &[&str], err: serde_json::Error) -> Failure {
    Failure::Error(format!(
        "gh {} gave unexpected output: {err}",
        args[..2.min(args.len())].join(" ")
    ))
}

fn read_snapshot(path: &Path) -> Result<Option<Snapshot>, Failure> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|err| Failure::Error(format!("{} is invalid: {err}", path.display()))),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(Failure::Error(format!("{}: {err}", path.display()))),
    }
}

fn write_snapshot(path: &Path, snapshot: &Snapshot) -> Result<(), Failure> {
    let temp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(snapshot).expect("a snapshot always serializes");
    fs::write(&temp, json)
        .and_then(|()| fs::rename(&temp, path))
        .map_err(|err| Failure::Error(format!("{}: {err}", path.display())))
}

#[derive(Deserialize)]
struct GhIssue {
    number: u64,
    title: String,
    labels: Vec<GhLabel>,
}

#[derive(Deserialize)]
struct GhLabel {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPr {
    number: u64,
    title: String,
    is_draft: bool,
    head_ref_name: String,
    head_ref_oid: String,
    #[serde(default)]
    status_check_rollup: Vec<GhCheck>,
    #[serde(default)]
    reviews: Vec<GhReview>,
}

#[derive(Deserialize)]
struct GhCheck {
    status: Option<String>,
    conclusion: Option<String>,
    state: Option<String>,
}

#[derive(Deserialize)]
struct GhReview {
    id: String,
    author: Option<GhUser>,
    #[serde(default)]
    body: String,
    state: String,
}

#[derive(Deserialize)]
struct GhUser {
    login: String,
}

#[derive(Deserialize)]
struct GhPrState {
    state: String,
}

#[derive(Deserialize)]
struct GhComment {
    id: u64,
    user: Option<GhUser>,
    #[serde(default)]
    body: String,
    created_at: Timestamp,
    issue_url: Option<String>,
    html_url: Option<String>,
    pull_request_url: Option<String>,
    path: Option<String>,
    line: Option<u64>,
}
