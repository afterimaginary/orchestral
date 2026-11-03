//! The owner's commands, as the first line of a comment on a PR's
//! conversation: `/approve`, `/hold` and `/unhold` (ADR-0005). The poll reads
//! them, and only the owner's count. Workers post under the same account, so
//! a comment starting with the Orchestral marker never counts, and the worker
//! guard stops a worker posting one.
//!
//! An approval is for the head commit the owner saw. A poll runs up to a
//! minute after the comment, so it's recorded only when the previous poll,
//! from before the comment, already saw that head. Otherwise a push in
//! between could move it to a commit the owner never saw. A new commit after
//! an approval lapses it.

use std::collections::BTreeMap;

use jiff::Timestamp;

use crate::github::Pr;
use crate::review::short;
use crate::state::State;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Command {
    Approve,
    Hold,
    Unhold,
}

/// The command on a comment's first line, if it has one.
pub fn parse(body: &str) -> Option<Command> {
    let first = body.lines().next()?.trim();
    match first {
        "/approve" => Some(Command::Approve),
        "/hold" => Some(Command::Hold),
        "/unhold" => Some(Command::Unhold),
        _ => None,
    }
}

/// An owner's command on an open PR.
#[derive(Debug, Clone)]
pub struct Request {
    pub pr: u64,
    pub command: Command,
    pub created_at: Timestamp,
}

/// The poll's view: the PRs the previous poll saw and when it started, and
/// the PRs open now.
pub struct Polls<'a> {
    pub before: &'a BTreeMap<u64, Pr>,
    pub before_at: Option<Timestamp>,
    pub now: &'a BTreeMap<u64, Pr>,
}

/// Applies the owner's commands, oldest first, then lapses each approval of
/// a commit that's no longer its PR's head. Forgets approvals and holds of
/// PRs that are no longer open. Returns event lines, each starting with the
/// PR's `subject`.
pub fn apply(
    state: &mut State,
    requests: &[Request],
    polls: &Polls,
    subject: impl Fn(u64) -> String,
) -> Vec<String> {
    let mut lines = Vec::new();
    let mut requests = requests.to_vec();
    requests.sort_by_key(|request| request.created_at);
    for request in &requests {
        let Some(pr) = polls.now.get(&request.pr) else {
            continue;
        };
        let subject = subject(request.pr);
        match request.command {
            Command::Approve => {
                let seen = polls
                    .before
                    .get(&request.pr)
                    .is_some_and(|before| before.head == pr.head)
                    && polls
                        .before_at
                        .is_some_and(|before_at| before_at <= request.created_at);
                if seen {
                    state.approvals.insert(request.pr, pr.head.clone());
                    lines.push(format!(
                        "{subject}: approved by the owner at {}: no action needed",
                        short(&pr.head)
                    ));
                } else {
                    lines.push(format!(
                        "{subject}: the owner's /approve came as its head moved to {}, so it isn't recorded: ask the owner to check the new commit and /approve again",
                        short(&pr.head)
                    ));
                }
            }
            Command::Hold => {
                state.holds.insert(request.pr);
                lines.push(format!(
                    "{subject}: put on hold by the owner: no action needed"
                ));
            }
            Command::Unhold => {
                state.holds.remove(&request.pr);
                lines.push(format!(
                    "{subject}: hold lifted by the owner: no action needed"
                ));
            }
        }
    }
    let lapsed: Vec<(u64, String)> = state
        .approvals
        .iter()
        .filter_map(|(number, approved)| {
            let head = &polls.now.get(number)?.head;
            (head != approved).then(|| (*number, head.clone()))
        })
        .collect();
    for (number, head) in lapsed {
        state.approvals.remove(&number);
        lines.push(format!(
            "{}: new commit {} since the owner's approval, so it needs /approve again: no action needed",
            subject(number),
            short(&head)
        ));
    }
    state
        .approvals
        .retain(|number, _| polls.now.contains_key(number));
    state.holds.retain(|number| polls.now.contains_key(number));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_command_alone_on_the_first_line_counts() {
        assert_eq!(parse("/approve"), Some(Command::Approve));
        assert_eq!(parse("  /hold  \nwhile I look"), Some(Command::Hold));
        assert_eq!(parse("/unhold"), Some(Command::Unhold));
        assert_eq!(parse("Looks good\n/approve"), None);
        assert_eq!(parse("/approved"), None);
        assert_eq!(parse("/approve this"), None);
        assert_eq!(parse(""), None);
    }
}
