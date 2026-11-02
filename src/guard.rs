//! Hooks that stop a worker doing what only the owner or the supervisor
//! should: merging, approving, pushing to `main`, and posting `/approve` or
//! `/hold`.
//!
//! They stop accidents. They aren't a security boundary: they read command
//! text, and a session that deliberately works around them can.

use std::fs;
use std::io::Read;
use std::path::Path;

use serde_json::{Value, json};

use crate::outcome::Failure;

const MAIN: &str = "refs/heads/main";

/// `hook worker-guard`: reads a `PreToolUse` event for `Bash` on stdin and
/// prints a deny decision when the command is one a worker mustn't run.
pub fn worker_guard() -> Result<(), Failure> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|err| Failure::Error(format!("can't read the hook input: {err}")))?;
    let event: Value = serde_json::from_str(&input)
        .map_err(|err| Failure::Error(format!("the hook input isn't JSON: {err}")))?;
    let Some(command) = event["tool_input"]["command"].as_str() else {
        return Ok(());
    };
    let cwd = Path::new(event["cwd"].as_str().unwrap_or("."));
    if let Some(reason) = denial(command, cwd) {
        let decision = json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": format!("{reason} Workers leave this to the owner or the supervisor."),
        }});
        println!("{decision}");
    }
    Ok(())
}

/// `hook pre-push`: a git pre-push hook that rejects a worker's push to
/// `main`. It reads `<local ref> <local sha> <remote ref> <remote sha>` lines
/// on stdin, and lets everyone else's pushes through.
pub fn pre_push() -> Result<(), Failure> {
    if std::env::var("ORCHESTRAL_WORKER")
        .unwrap_or_default()
        .is_empty()
    {
        return Ok(());
    }
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|err| Failure::Error(format!("can't read the refs being pushed: {err}")))?;
    if input
        .lines()
        .any(|line| line.split_whitespace().nth(2) == Some(MAIN))
    {
        return Err(Failure::Error(
            "workers can't push to main; push your branch and open a PR".into(),
        ));
    }
    Ok(())
}

/// Why `command` is denied, if it is. Relative body files are read from
/// `cwd`.
pub fn denial(command: &str, cwd: &Path) -> Option<&'static str> {
    let segments = segments(command);
    let mut uses_gh = false;
    for words in &segments {
        if let Some(gh) = words.iter().position(|w| is_program(w, "gh")) {
            uses_gh = true;
            let args = &words[gh + 1..];
            if has_pair(args, "pr", "merge") || is_api_merge(args) {
                return Some("Merging a PR is denied.");
            }
            if has_pair(args, "pr", "review") && args.iter().any(|a| a == "--approve" || a == "-a")
            {
                return Some("Approving a PR is denied.");
            }
        }
        if let Some(git) = words.iter().position(|w| is_program(w, "git"))
            && let Some(push) = words[git + 1..].iter().position(|w| w == "push")
            && words[git + 1 + push + 1..].iter().any(|w| targets_main(w))
        {
            return Some("Pushing to main is denied.");
        }
    }
    if uses_gh {
        let bodies = segments.iter().flat_map(|words| body_files(words));
        let posts_command = mentions_command(command)
            || bodies.into_iter().any(|file| {
                fs::read_to_string(cwd.join(file)).is_ok_and(|text| mentions_command(&text))
            });
        if posts_command {
            return Some("Posting /approve or /hold is denied, since the owner uses them.");
        }
    }
    None
}

/// Splits a command line into the words of each simple command, at `;`,
/// `&`, `|` and newlines, removing quotes. It's a rough reading of shell
/// syntax, which is enough to catch accidents.
fn segments(command: &str) -> Vec<Vec<String>> {
    let mut segments = vec![];
    let mut words = vec![];
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = command.chars();
    let end_word = |words: &mut Vec<String>, word: &mut String, in_word: &mut bool| {
        if *in_word {
            words.push(std::mem::take(word));
            *in_word = false;
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                word.extend(chars.by_ref().take_while(|&c| c != '\''));
            }
            '"' => {
                in_word = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => word.extend(chars.next()),
                        c => word.push(c),
                    }
                }
            }
            '\\' => {
                in_word = true;
                word.extend(chars.next());
            }
            ';' | '&' | '|' | '\n' | '(' | ')' => {
                end_word(&mut words, &mut word, &mut in_word);
                if !words.is_empty() {
                    segments.push(std::mem::take(&mut words));
                }
            }
            c if c.is_whitespace() => end_word(&mut words, &mut word, &mut in_word),
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    end_word(&mut words, &mut word, &mut in_word);
    if !words.is_empty() {
        segments.push(words);
    }
    segments
}

/// `gh`, or a path ending in `/gh`.
fn is_program(word: &str, name: &str) -> bool {
    word == name || word.ends_with(&format!("/{name}"))
}

fn has_pair(args: &[String], first: &str, second: &str) -> bool {
    args.windows(2)
        .any(|pair| pair[0] == first && pair[1] == second)
}

/// `gh api …/pulls/<n>/merge`.
fn is_api_merge(args: &[String]) -> bool {
    args.first().is_some_and(|a| a == "api")
        && args
            .iter()
            .any(|a| a.contains("pulls/") && a.trim_end_matches('/').ends_with("/merge"))
}

/// A refspec whose destination is `main`, such as `main`, `HEAD:main` or
/// `+refs/heads/main`.
fn targets_main(word: &str) -> bool {
    let destination = word.rsplit(':').next().unwrap_or(word);
    let destination = destination.trim_start_matches('+');
    destination == "main" || destination == MAIN
}

/// Files a `gh` command posts as a body: `--body-file <f>`, `-F <f>`, and
/// `-F body=@<f>` or `--field body=@<f>` for `gh api`.
fn body_files(words: &[String]) -> Vec<&str> {
    let mut files = vec![];
    for (i, word) in words.iter().enumerate() {
        let next = words.get(i + 1).map(String::as_str);
        let value = match word.as_str() {
            "--body-file" | "-F" | "--field" => next,
            _ => word.strip_prefix("--body-file="),
        };
        if let Some(value) = value {
            let file = value.strip_prefix("body=@").unwrap_or(value);
            if file != "-" && !file.contains('=') {
                files.push(file);
            }
        }
    }
    files
}

/// Whether `text` has `/approve` or `/hold` as a word, as in
/// `/approve`, `body=/approve` or `"/hold."`, but not `src/hold`.
fn mentions_command(text: &str) -> bool {
    ["/approve", "/hold"].iter().any(|command| {
        text.match_indices(command).any(|(at, _)| {
            let before = text[..at].chars().next_back();
            let after = text[at + command.len()..].chars().next();
            !before.is_some_and(|c| c.is_alphanumeric() || c == '/' || c == '_')
                && !after.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-')
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn denied(command: &str) -> bool {
        denial(command, Path::new("/nonexistent")).is_some()
    }

    #[test]
    fn denies_merging() {
        assert!(denied("gh pr merge 12 --squash"));
        assert!(denied("cd app && gh -R owner/app pr merge 12"));
        assert!(denied("/opt/homebrew/bin/gh pr merge"));
        assert!(denied("gh api -X PUT repos/owner/app/pulls/12/merge"));
        assert!(!denied("gh pr view 12"));
        assert!(!denied("echo 'run gh pr list then merge'"));
    }

    #[test]
    fn denies_approving() {
        assert!(denied("gh pr review 12 --approve"));
        assert!(denied("gh pr review -a 12"));
        assert!(!denied("gh pr review 12 --comment -b 'Looks fine'"));
    }

    #[test]
    fn denies_pushing_to_main() {
        assert!(denied("git push origin main"));
        assert!(denied("git push --force origin HEAD:main"));
        assert!(denied("git push origin +refs/heads/main"));
        assert!(denied("git -C app push origin feature:main"));
        assert!(!denied("git push -u origin issue-12"));
        assert!(!denied("git push --force-with-lease origin issue-12"));
        assert!(!denied("git checkout main"));
    }

    #[test]
    fn denies_posting_approve_or_hold() {
        assert!(denied("gh pr comment 12 --body '/approve'"));
        assert!(denied("gh issue comment 12 -b \"/hold\""));
        assert!(denied("gh pr review 12 --comment --body 'ok /approve'"));
        assert!(denied(
            "gh api repos/o/a/issues/12/comments -f body=/approve"
        ));
        assert!(!denied("gh pr comment 12 --body 'see src/hold/approve.rs'"));
        assert!(!denied("gh pr comment 12 --body 'waiting for approval'"));
        assert!(!denied("echo /approve"));
    }

    #[test]
    fn reads_body_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("body.md"), "Done.\n\n/approve\n").unwrap();
        std::fs::write(dir.path().join("fine.md"), "Done.\n").unwrap();
        let denied = |command: &str| denial(command, dir.path()).is_some();

        assert!(denied("gh pr comment 12 --body-file body.md"));
        assert!(denied("gh pr comment 12 -F body.md"));
        assert!(denied("gh pr comment 12 --body-file=body.md"));
        assert!(denied(
            "gh api repos/o/a/issues/12/comments -F body=@body.md"
        ));
        assert!(!denied("gh pr comment 12 --body-file fine.md"));
    }
}
