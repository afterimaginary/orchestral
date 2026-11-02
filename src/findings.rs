//! The `## Findings` section of a PR body. Workers report what they found
//! and didn't act on as `[pending]`, and the orchestrator decides each one.
//! A PR whose findings aren't all decided can't merge.
//!
//! The section runs from its heading to the next `## ` heading or the end of
//! the body. It holds `None`, or lines of `- [tag] text`, where the tag ends
//! at the first `]`.

use std::fmt;

/// The heading that starts the section.
pub const HEADING: &str = "## Findings";

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Not decided yet.
    Pending,
    /// Filed as an issue.
    Issue(u64),
    Fixed,
    /// Not worth tracking, with why.
    Dropped(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub outcome: Outcome,
    pub text: String,
}

/// Why a Findings section isn't valid.
#[derive(Debug, PartialEq)]
pub enum Problem {
    /// The body has no `## Findings` heading.
    Missing,
    /// The section has nothing in it.
    Empty,
    /// A line isn't `None` or `- [tag] text`.
    Line(String),
    /// A tag isn't one of the outcomes.
    Tag(String),
    /// Findings still `[pending]`.
    Pending(Vec<String>),
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Problem::Missing => write!(f, "the PR body has no `{HEADING}` section"),
            Problem::Empty => write!(
                f,
                "the `{HEADING}` section is empty: write `None` if there are none"
            ),
            Problem::Line(line) => write!(
                f,
                "`{line}` in `{HEADING}` isn't `None` or `- [outcome] text`"
            ),
            Problem::Tag(tag) => write!(
                f,
                "`[{tag}]` in `{HEADING}` isn't `[issue #N]`, `[fixed]` or `[dropped: reason]`"
            ),
            Problem::Pending(texts) => write!(
                f,
                "{} finding{} still pending: {}",
                texts.len(),
                if texts.len() == 1 { " is" } else { "s are" },
                texts.join("; ")
            ),
        }
    }
}

/// Parses the section, `[pending]` included. `None` gives an empty list.
pub fn parse(body: &str) -> Result<Vec<Finding>, Problem> {
    let mut lines = body.lines().map(str::trim_end);
    if !lines.any(|line| line == HEADING) {
        return Err(Problem::Missing);
    }
    let section: Vec<&str> = lines
        .take_while(|line| !line.starts_with("## "))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    match section.as_slice() {
        [] => Err(Problem::Empty),
        ["None"] => Ok(Vec::new()),
        lines => lines.iter().map(|line| finding(line)).collect(),
    }
}

/// The section, with every finding decided.
pub fn check(body: &str) -> Result<Vec<Finding>, Problem> {
    let findings = parse(body)?;
    let pending: Vec<String> = findings
        .iter()
        .filter(|finding| finding.outcome == Outcome::Pending)
        .map(|finding| finding.text.clone())
        .collect();
    if !pending.is_empty() {
        return Err(Problem::Pending(pending));
    }
    Ok(findings)
}

fn finding(line: &str) -> Result<Finding, Problem> {
    let bad = || Problem::Line(line.to_owned());
    let rest = line.strip_prefix("- [").ok_or_else(bad)?;
    let (tag, text) = rest.split_once(']').ok_or_else(bad)?;
    let text = text.trim();
    if text.is_empty() {
        return Err(bad());
    }
    Ok(Finding {
        outcome: outcome(tag).ok_or_else(|| Problem::Tag(tag.to_owned()))?,
        text: text.to_owned(),
    })
}

fn outcome(tag: &str) -> Option<Outcome> {
    match tag {
        "pending" => Some(Outcome::Pending),
        "fixed" => Some(Outcome::Fixed),
        _ => {
            if let Some(number) = tag.strip_prefix("issue #") {
                return number
                    .parse()
                    .ok()
                    .filter(|_| number.bytes().all(|byte| byte.is_ascii_digit()))
                    .map(Outcome::Issue);
            }
            let reason = tag.strip_prefix("dropped:")?.trim();
            (!reason.is_empty()).then(|| Outcome::Dropped(reason.to_owned()))
        }
    }
}
