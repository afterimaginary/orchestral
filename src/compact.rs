//! Hooks that carry the orchestrator across a compaction. Before it, the
//! fleet's status is saved; after it, the standing instructions and the
//! status before and after go back into the orchestrator's context.
//!
//! Both act only for the session holding the claim. A hook that exits
//! non-zero can block or spoil a compaction, so every problem they expect
//! becomes a line in the output instead, and they never exit `2`.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::state::Store;
use crate::status;
use crate::supervisor::failure_text;

/// Where the status before compaction is kept.
const SNAPSHOT: &str = "orchestrator/before-compact.txt";

/// `hook pre-compact`: saves the fleet's status.
pub fn pre_compact() {
    let Some((store, cwd)) = orchestrator_store() else {
        return;
    };
    let text = status_text(&cwd);
    let path = store.dir().join(SNAPSHOT);
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(path, text);
}

/// `hook session-compact`: prints the standing instructions and the fleet's
/// status as the `SessionStart` hook's added context.
pub fn session_compact() {
    let Some((store, cwd)) = orchestrator_store() else {
        return;
    };
    let path = store.dir().join(SNAPSHOT);
    let before = fs::read_to_string(&path)
        .unwrap_or_else(|_| "(no status was saved before compaction)".into());
    let _ = fs::remove_file(&path);
    let now = status_text(&cwd);
    let root = std::env::var_os("CLAUDE_PLUGIN_ROOT").map(PathBuf::from);
    let skill = root
        .as_ref()
        .map(|root| {
            root.join("skills/orchestrate/SKILL.md")
                .display()
                .to_string()
        })
        .unwrap_or_else(|| "skills/orchestrate/SKILL.md in the Orchestral plugin".into());
    let orchestral = root
        .as_ref()
        .map(|root| root.join("bin/orchestral").display().to_string())
        .unwrap_or_else(|| "bin/orchestral in the Orchestral plugin".into());
    let context = include_str!("prompts/after-compact.md")
        .replace("{{skill}}", &skill)
        .replace("{{orchestral}}", &orchestral)
        .replace("{{before}}", before.trim_end())
        .replace("{{now}}", now.trim_end());
    let output = json!({"hookSpecificOutput": {
        "hookEventName": "SessionStart",
        "additionalContext": context,
    }});
    println!("{output}");
}

/// The repository's store and the hook's working directory, when the
/// session running the hook holds this repository's claim.
fn orchestrator_store() -> Option<(Store, PathBuf)> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).ok()?;
    let event: Value = serde_json::from_str(&input).ok()?;
    let session = event["session_id"].as_str()?;
    let cwd = PathBuf::from(event["cwd"].as_str()?);
    let store = Store::for_repo(Path::new(&cwd)).ok()?;
    let holder = store.read().ok()?.orchestrator?;
    (holder.session_id == session).then_some((store, cwd))
}

/// The fleet's status, or why it couldn't be read.
fn status_text(cwd: &Path) -> String {
    match status::status(cwd) {
        Ok(status) => status.to_string(),
        Err(err) => format!("(the status couldn't be read: {})", failure_text(&err)),
    }
}
