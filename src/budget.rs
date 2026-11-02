//! Fleet token accounting: one number for what a run has used so far.
//!
//! Tokens are weighted by their API price relative to input tokens. Whether
//! plan limits weigh them the same way is unknown, so the total is an
//! estimate, and is always shown as one.

use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Serialize;
use serde_json::Value;

use crate::outcome::Failure;

/// Weighted tokens a run has used.
#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Estimate {
    pub estimated_tokens: u64,
}

impl fmt::Display for Estimate {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "about {} tokens (estimate)", self.estimated_tokens)
    }
}

/// Sums the weighted tokens of every response in `transcripts`, and in each
/// one's subagent transcripts, since the run started. Before a run has
/// started, the total is zero.
pub fn fleet_usage(
    transcripts: &[PathBuf],
    run_started_at: Option<Timestamp>,
) -> Result<Estimate, Failure> {
    let Some(since) = run_started_at else {
        return Ok(Estimate::default());
    };
    let mut seen = HashSet::new();
    let mut total = 0.0;
    for path in transcripts {
        for file in std::iter::once(path.clone()).chain(subagent_transcripts(path)) {
            let text = match fs::read_to_string(&file) {
                Ok(text) => text,
                // A session that hasn't written anything yet has used nothing.
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => return Err(Failure::Error(format!("{}: {err}", file.display()))),
            };
            for entry in text
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            {
                if entry["type"] != "assistant" {
                    continue;
                }
                // One response spans many entries sharing a `message.id`, and
                // its first entry decides whether it's part of the run.
                let Some(id) = entry["message"]["id"].as_str() else {
                    continue;
                };
                if !seen.insert(id.to_owned()) {
                    continue;
                }
                let at: Option<Timestamp> =
                    entry["timestamp"].as_str().and_then(|ts| ts.parse().ok());
                if at.is_some_and(|at| at >= since) {
                    total += weighted(&entry["message"]["usage"]);
                }
            }
        }
    }
    Ok(Estimate {
        estimated_tokens: total.round() as u64,
    })
}

/// `<dir>/<id>/subagents/*.jsonl` for the transcript `<dir>/<id>.jsonl`.
fn subagent_transcripts(transcript: &Path) -> Vec<PathBuf> {
    let dir = transcript.with_extension("").join("subagents");
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    files.sort();
    files
}

/// `input + output + cache writes × (2 for 1 hour, 1.25 for 5 minutes) +
/// cache reads × 0.1`.
fn weighted(usage: &Value) -> f64 {
    let tokens = |value: &Value| value.as_u64().unwrap_or(0) as f64;
    let creation = &usage["cache_creation"];
    let cache_writes = if creation.is_object() {
        tokens(&creation["ephemeral_1h_input_tokens"]) * 2.0
            + tokens(&creation["ephemeral_5m_input_tokens"]) * 1.25
    } else {
        // Without a breakdown, price writes at the 5-minute rate.
        tokens(&usage["cache_creation_input_tokens"]) * 1.25
    };
    tokens(&usage["input_tokens"])
        + tokens(&usage["output_tokens"])
        + cache_writes
        + tokens(&usage["cache_read_input_tokens"]) * 0.1
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn weights_each_kind_of_token() {
        let usage = json!({
            "input_tokens": 10,
            "output_tokens": 20,
            "cache_creation_input_tokens": 300,
            "cache_read_input_tokens": 1000,
            "cache_creation": {"ephemeral_1h_input_tokens": 100, "ephemeral_5m_input_tokens": 200}
        });

        assert_eq!(
            weighted(&usage),
            10.0 + 20.0 + 100.0 * 2.0 + 200.0 * 1.25 + 100.0
        );
    }

    #[test]
    fn writes_without_a_breakdown_use_the_five_minute_rate() {
        let usage = json!({"cache_creation_input_tokens": 400});

        assert_eq!(weighted(&usage), 500.0);
    }

    #[test]
    fn the_estimate_says_it_is_one() {
        let estimate = Estimate {
            estimated_tokens: 1200,
        };

        assert_eq!(estimate.to_string(), "about 1200 tokens (estimate)");
        assert_eq!(
            serde_json::to_string(&estimate).unwrap(),
            r#"{"estimated_tokens":1200}"#
        );
    }
}
