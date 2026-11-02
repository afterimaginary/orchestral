//! Reads a session's context size and cache state from its transcript.
//!
//! No Claude Code tool shows one session's context to another, so this reads
//! the transcript's JSONL directly. Every format relied on here is
//! undocumented, and is recorded in `docs/specs/claude-code-internals.md`.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Serialize;
use serde_json::Value;

use crate::outcome::Failure;

/// How much of a transcript's end, and of its start, is read before falling
/// back to the whole file.
const CHUNK: u64 = 1024 * 1024;

const MILLION: u64 = 1_000_000;
const STANDARD_WINDOW: u64 = 200_000;

#[derive(Debug, PartialEq, Serialize)]
pub struct Context {
    /// Tokens the next request will send.
    pub tokens: u64,
    /// The model of the last response, as the API reported it.
    pub model: Option<String>,
    /// The model the session says it is, such as `claude-opus-5-5[1m]`, when
    /// it still matches `model`.
    pub identity: Option<String>,
    /// The context window, when it can be told.
    pub window: Option<u64>,
    /// `tokens` as a percentage of `window`. Not capped at 100.
    pub percent: Option<f64>,
    /// When the last request was sent.
    pub last_request_at: Option<Timestamp>,
    /// Compactions found in the part of the transcript that was read.
    pub compactions: Vec<Compaction>,
    /// How long the prompt cache lives after a request, in seconds.
    pub cache_ttl: Option<u64>,
    /// Whether the cache is still warm `now`, so waking the session is cheap.
    pub warm: bool,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Compaction {
    pub at: Option<Timestamp>,
    /// Tokens left after compacting.
    pub post_tokens: u64,
}

/// The directory holding every project's transcripts.
pub fn projects_dir() -> Option<PathBuf> {
    let config = match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".claude"),
    };
    Some(config.join("projects"))
}

/// Finds a session's transcript: by the path rule first, then by scanning
/// every project, since long paths are shortened.
pub fn find(projects: &Path, cwd: &Path, session_id: &str) -> Option<PathBuf> {
    let name = format!("{session_id}.jsonl");
    let expected = projects.join(project_dir_name(cwd)).join(&name);
    if expected.is_file() {
        return Some(expected);
    }
    fs::read_dir(projects)
        .ok()?
        .filter_map(|entry| Some(entry.ok()?.path().join(&name)))
        .find(|path| path.is_file())
}

/// The directory name for a working directory: every character that isn't
/// ASCII alphanumeric becomes `-`.
pub fn project_dir_name(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Reads the context of the transcript at `path`. `organization` is the
/// account's `oauthAccount.organizationType`, which decides some windows.
pub fn read(path: &Path, now: Timestamp, organization: Option<&str>) -> Result<Context, Failure> {
    let io = |err: std::io::Error| Failure::Error(format!("{}: {err}", path.display()));
    let mut file = File::open(path).map_err(io)?;
    let len = file.metadata().map_err(io)?.len();

    let tail = read_lines(&mut file, len.saturating_sub(CHUNK), len).map_err(io)?;
    let entries = if len > CHUNK && last_response(&tail).is_none() {
        read_lines(&mut file, 0, len).map_err(io)?
    } else {
        tail
    };
    // A session is usually told its identity near the start.
    let identity = match latest_identity(&entries) {
        None if len > CHUNK => latest_identity(&read_lines(&mut file, 0, CHUNK).map_err(io)?),
        found => found,
    };
    Ok(context_from(&entries, identity, now, organization))
}

/// Parses the entries between two byte offsets, dropping a partial first
/// line when `start` isn't the beginning, and any line that doesn't parse.
fn read_lines(file: &mut File, start: u64, end: u64) -> std::io::Result<Vec<Value>> {
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity((end - start) as usize);
    file.take(end - start).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.lines();
    if start > 0 {
        lines.next();
    }
    Ok(lines
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

fn context_from(
    entries: &[Value],
    identity: Option<String>,
    now: Timestamp,
    organization: Option<&str>,
) -> Context {
    let response = last_response(entries);
    let mut tokens = response.map(|i| context_tokens(&entries[i])).unwrap_or(0);
    let model = response.and_then(|i| str_at(&entries[i], "/message/model"));

    let compactions: Vec<Compaction> = entries
        .iter()
        .filter(|entry| is_compact_boundary(entry))
        .map(|entry| Compaction {
            at: timestamp(entry),
            post_tokens: entry
                .pointer("/compactMetadata/postTokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        })
        .collect();
    // A compaction after the last response replaces the context, and
    // nothing is cached after it.
    let compacted_since = entries
        .iter()
        .rposition(is_compact_boundary)
        .is_some_and(|boundary| response.is_none_or(|i| boundary > i));
    if compacted_since {
        tokens = compactions.last().map_or(0, |c| c.post_tokens);
    }

    let identity = match (&model, identity) {
        (Some(model), Some(identity)) if matches_model(&identity, model) => Some(identity),
        _ => None,
    };
    let window = model
        .as_deref()
        .and_then(|model| window(model, identity.as_deref(), organization, tokens));
    let last_request_at = response.and_then(|i| last_request_time(entries, i));
    let cache_ttl = cache_ttl(entries);
    let warm = !compacted_since
        && match (last_request_at, cache_ttl) {
            (Some(at), Some(ttl)) => now.duration_since(at).as_secs() < ttl as i64,
            _ => false,
        };

    Context {
        tokens,
        model,
        identity,
        window,
        percent: percent(tokens, window),
        last_request_at,
        compactions,
        cache_ttl,
        warm,
    }
}

/// The index of the last real assistant entry: not a sidechain, and with
/// usage, since API errors and copies made by compaction have none.
fn last_response(entries: &[Value]) -> Option<usize> {
    entries.iter().rposition(|entry| {
        entry["type"] == "assistant" && entry["isSidechain"] != true && context_tokens(entry) > 0
    })
}

/// Input tokens of a response: from the last `usage.iterations` entry of
/// type `message` when there are iterations, else from `usage` itself.
fn context_tokens(entry: &Value) -> u64 {
    let usage = &entry["message"]["usage"];
    let source = usage["iterations"]
        .as_array()
        .and_then(|iterations| iterations.iter().rev().find(|i| i["type"] == "message"))
        .unwrap_or(usage);
    [
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ]
    .iter()
    .map(|key| source[key].as_u64().unwrap_or(0))
    .sum()
}

/// When the request behind the response at `response` was sent: the
/// timestamp of the last user or attachment entry before the response's
/// earliest entry. One response is many entries sharing a `message.id`.
fn last_request_time(entries: &[Value], response: usize) -> Option<Timestamp> {
    let id = &entries[response]["message"]["id"];
    let first = entries[..=response]
        .iter()
        .position(|entry| entry["type"] == "assistant" && &entry["message"]["id"] == id)
        .unwrap_or(response);
    entries[..first]
        .iter()
        .rev()
        .find(|entry| entry["type"] == "user" || entry["type"] == "attachment")
        .and_then(timestamp)
}

/// The cache lifetime the latest cache write used, in seconds.
fn cache_ttl(entries: &[Value]) -> Option<u64> {
    entries.iter().rev().find_map(|entry| {
        let creation = &entry["message"]["usage"]["cache_creation"];
        let tokens = |key: &str| creation[key].as_u64().unwrap_or(0);
        if tokens("ephemeral_5m_input_tokens") > 0 {
            Some(300)
        } else if tokens("ephemeral_1h_input_tokens") > 0 {
            Some(3600)
        } else {
            None
        }
    })
}

/// The latest model identity the session was given, such as
/// `claude-opus-5-5[1m]`.
fn latest_identity(entries: &[Value]) -> Option<String> {
    entries.iter().rev().find_map(|entry| {
        (entry["type"] == "attachment" && entry["attachment"]["type"] == "model")
            .then(|| str_at(entry, "/attachment/identity/modelId"))
            .flatten()
    })
}

/// Whether an identity still describes the API model. The API model never
/// carries the `[1m]` suffix.
fn matches_model(identity: &str, model: &str) -> bool {
    identity.strip_suffix("[1m]").unwrap_or(identity) == model
}

/// The context window for a model, when it can be told.
fn window(
    model: &str,
    identity: Option<&str>,
    organization: Option<&str>,
    tokens: u64,
) -> Option<u64> {
    if identity.is_some_and(|id| id.ends_with("[1m]")) {
        return Some(MILLION);
    }
    let (family, major, minor) = parse_model(model).unwrap_or(("", 0, 0));
    let known = match family {
        "fable" => Some(MILLION),
        "sonnet" if major >= 5 => Some(MILLION),
        "haiku" => Some(STANDARD_WINDOW),
        "opus"
            if (major, minor) >= (4, 6)
                && matches!(
                    organization,
                    Some("claude_max" | "claude_team" | "claude_enterprise")
                ) =>
        {
            Some(MILLION)
        }
        _ => None,
    };
    // More than the standard window in use means the session has the larger one.
    known.or((tokens > STANDARD_WINDOW).then_some(MILLION))
}

/// Splits `claude-opus-4-6` or `claude-opus-4-6-20260101` into its family,
/// major and minor version. In `claude-opus-4-20250514` the 8 digits are a
/// date, so the minor version is 0.
fn parse_model(model: &str) -> Option<(&str, u32, u32)> {
    let mut parts = model.strip_prefix("claude-")?.split('-');
    let family = parts.next()?;
    let major = parts.next()?.parse().ok()?;
    let minor = parts
        .next()
        .filter(|part| part.len() < 8)
        .and_then(|part| part.parse().ok())
        .unwrap_or(0);
    Some((family, major, minor))
}

fn percent(tokens: u64, window: Option<u64>) -> Option<f64> {
    window.map(|window| tokens as f64 * 100.0 / window as f64)
}

fn is_compact_boundary(entry: &Value) -> bool {
    entry["type"] == "system" && entry["subtype"] == "compact_boundary"
}

fn timestamp(entry: &Value) -> Option<Timestamp> {
    entry["timestamp"].as_str()?.parse().ok()
}

fn str_at(entry: &Value, pointer: &str) -> Option<String> {
    entry.pointer(pointer)?.as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_dirs_replace_every_non_alphanumeric_character() {
        assert_eq!(
            project_dir_name(Path::new("/Users/dev/src/my_app.v2")),
            "-Users-dev-src-my-app-v2"
        );
    }

    #[test]
    fn windows_by_model() {
        let window = |model, identity, org, tokens| window(model, identity, org, tokens);
        assert_eq!(window("claude-fable-5-1", None, None, 0), Some(MILLION));
        assert_eq!(window("claude-sonnet-5-5", None, None, 0), Some(MILLION));
        assert_eq!(window("claude-sonnet-4-5", None, None, 0), None);
        assert_eq!(
            window("claude-haiku-5-5", None, None, 0),
            Some(STANDARD_WINDOW)
        );
        assert_eq!(
            window("claude-opus-5-5", Some("claude-opus-5-5[1m]"), None, 0),
            Some(MILLION)
        );
        assert_eq!(
            window("claude-opus-4-6", None, Some("claude_max"), 0),
            Some(MILLION)
        );
        assert_eq!(window("claude-opus-4-5", None, Some("claude_max"), 0), None);
        assert_eq!(
            window("claude-opus-4-20250514", None, Some("claude_max"), 0),
            None
        );
        assert_eq!(
            window("claude-opus-4-6-20260101", None, Some("claude_max"), 0),
            Some(MILLION)
        );
        assert_eq!(window("claude-opus-5-5", None, Some("claude_pro"), 0), None);
        assert_eq!(
            window("claude-opus-5-5", None, None, 250_000),
            Some(MILLION)
        );
    }

    #[test]
    fn percentages_go_past_100() {
        assert_eq!(percent(300_000, Some(STANDARD_WINDOW)), Some(150.0));
    }
}
