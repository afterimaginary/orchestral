//! `orchestral limits`: the plan's usage windows, read from Claude Code's
//! `/usage` command (ADR-0008).

use std::fmt;
use std::fs;
use std::process::Command;
use std::time::Duration;

use jiff::fmt::strtime;
use jiff::tz::TimeZone;
use jiff::{ToSpan, Zoned};
use serde::Serialize;

/// How reset times are shown, such as `Nov 4 at 1:00pm PST`.
pub const RESET_FORMAT: &str = "%b %-d at %-I:%M%P %Z";

/// How long a read may take. A read normally takes a few seconds.
const DEADLINE: Duration = Duration::from_secs(60);

/// The plan's usage windows, or why they can't be read.
#[derive(Debug, Serialize)]
pub struct Limits {
    pub available: bool,
    /// Why the limits are unavailable, when they are.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<Window>,
}

/// One usage window, such as `session` or `week (all models)`.
#[derive(Debug, PartialEq, Serialize)]
pub struct Window {
    pub name: String,
    /// Percent of the window used.
    pub percent: u8,
    /// When the window resets, if the line gives a time.
    pub resets: Option<Zoned>,
}

impl Limits {
    fn unavailable(reason: impl Into<String>) -> Limits {
        Limits {
            available: false,
            reason: Some(reason.into()),
            windows: Vec::new(),
        }
    }
}

impl fmt::Display for Limits {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if let Some(reason) = &self.reason {
            return write!(f, "Plan limits are unavailable: {reason}.");
        }
        let lines: Vec<String> = self
            .windows
            .iter()
            .map(|window| match &window.resets {
                Some(resets) => format!(
                    "{}: {}% used, resets {}",
                    window.name,
                    window.percent,
                    resets.strftime(RESET_FORMAT)
                ),
                None => format!("{}: {}% used", window.name, window.percent),
            })
            .collect();
        write!(f, "{}", lines.join("\n"))
    }
}

/// Runs `/usage` in a throwaway print session. Failing to read the limits is
/// reported as unavailable, never as an error, so it can't stop other work.
pub fn read() -> Limits {
    match run_usage() {
        Ok(output) => match parse(&output, &Zoned::now()) {
            Ok(windows) => Limits {
                available: true,
                reason: None,
                windows,
            },
            Err(reason) => Limits::unavailable(reason),
        },
        Err(reason) => Limits::unavailable(reason),
    }
}

/// Runs the read from a new temporary directory, outside any session, so no
/// project settings, hooks or session apply to it.
fn run_usage() -> Result<String, String> {
    // A directory that already exists is refused, so nothing left in it can
    // apply to the read.
    let dir = std::env::temp_dir().join(format!(
        "orchestral-usage-{}-{}",
        std::process::id(),
        jiff::Timestamp::now().as_nanosecond()
    ));
    fs::create_dir(&dir).map_err(|err| format!("can't create {}: {err}", dir.display()))?;
    let output = crate::deadline::output(
        Command::new("claude")
            .args([
                "-p",
                "/usage",
                "--no-session-persistence",
                "--settings",
                r#"{"disableAllHooks":true}"#,
            ])
            .current_dir(&dir)
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CLAUDECODE"),
        DEADLINE,
        "claude -p /usage",
    );
    let _ = fs::remove_dir_all(&dir);
    let output = output?;
    if !output.status.success() {
        let errors = String::from_utf8_lossy(&output.stderr);
        let cause = errors.lines().find(|line| !line.trim().is_empty());
        return Err(match cause {
            Some(cause) => format!(
                "claude -p /usage exited with {}: {}",
                output.status,
                cause.trim()
            ),
            None => format!("claude -p /usage exited with {}", output.status),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Reads the `Current …` lines of `/usage` output and ignores the rest. Any
/// `Current` line that doesn't parse fails the whole read, so a changed
/// format reports the limits unavailable instead of guessing.
pub fn parse(output: &str, now: &Zoned) -> Result<Vec<Window>, String> {
    let windows = output
        .lines()
        .filter_map(|line| line.trim().strip_prefix("Current "))
        .map(|line| {
            parse_window(line, now).ok_or_else(|| format!("can't parse \"Current {line}\""))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if windows.is_empty() {
        return Err("claude -p /usage printed no usage windows".into());
    }
    Ok(windows)
}

/// Parses `session: 2% used · resets Nov 2 at 6:49am (America/Los_Angeles)`.
/// The reset part is optional.
fn parse_window(line: &str, now: &Zoned) -> Option<Window> {
    let (name, rest) = line.split_once(": ")?;
    let (percent, rest) = rest.split_once("% used")?;
    let resets = match rest.trim() {
        "" => None,
        rest => Some(parse_reset(rest.strip_prefix("· resets ")?, now)?),
    };
    Some(Window {
        name: name.to_string(),
        percent: percent.parse().ok()?,
        resets,
    })
}

/// Parses `Nov 2 at 6:49am (America/Los_Angeles)`, or `Nov 4 at 1pm (…)` on
/// the hour. The line gives no year: it's the current year, unless that puts
/// the time more than 180 days in the past, when it's the next year.
fn parse_reset(text: &str, now: &Zoned) -> Option<Zoned> {
    let (when, zone) = text.strip_suffix(')')?.rsplit_once(" (")?;
    let zone = TimeZone::get(zone).ok()?;
    let mut time = ["%b %d at %I:%M%p", "%b %d at %I%p"]
        .into_iter()
        .find_map(|format| strtime::parse(format, when).ok())?;
    let this_year = now.with_time_zone(zone.clone()).year();
    time.set_year(Some(this_year)).ok()?;
    let resets = time.to_datetime().ok()?.to_zoned(zone.clone()).ok()?;
    if resets < now.checked_sub(180.days()).ok()? {
        time.set_year(Some(this_year + 1)).ok()?;
        return time.to_datetime().ok()?.to_zoned(zone).ok();
    }
    Some(resets)
}

#[cfg(test)]
mod tests {
    use jiff::civil::DateTime;

    use super::*;

    const USAGE: &str = include_str!("../tests/fixtures/claude/2.1.314/usage.txt");

    fn at(datetime: &str) -> Zoned {
        datetime
            .parse::<DateTime>()
            .unwrap()
            .in_tz("America/Los_Angeles")
            .unwrap()
    }

    fn window(name: &str, percent: u8, resets: &str) -> Window {
        Window {
            name: name.into(),
            percent,
            resets: Some(at(resets)),
        }
    }

    #[test]
    fn reads_every_current_window_from_the_pinned_output() {
        let windows = parse(USAGE, &at("2026-11-02T04:00")).unwrap();

        assert_eq!(
            windows,
            vec![
                window("session", 2, "2026-11-02T06:49"),
                window("week (all models)", 26, "2026-11-04T12:59"),
                window("week (Fable)", 0, "2026-11-04T12:59"),
            ]
        );
    }

    #[test]
    fn a_time_on_the_hour_has_no_minutes() {
        let output =
            "Current week (all models): 26% used · resets Nov 4 at 1pm (America/Los_Angeles)";

        let windows = parse(output, &at("2026-11-02T04:00")).unwrap();

        assert_eq!(
            windows,
            vec![window("week (all models)", 26, "2026-11-04T13:00")]
        );
    }

    #[test]
    fn a_reset_is_parsed_in_the_zone_the_line_gives() {
        let output = "Current session: 2% used · resets Nov 2 at 6:49am (Europe/London)";

        let windows = parse(output, &at("2026-11-02T04:00")).unwrap();

        let expected = "2026-11-02T06:49"
            .parse::<DateTime>()
            .unwrap()
            .in_tz("Europe/London")
            .unwrap();
        assert_eq!(windows[0].resets, Some(expected));
    }

    #[test]
    fn a_reset_just_passed_stays_in_the_current_year() {
        // A read just after a reset can still print the old time.
        let output = "Current session: 44% used · resets Nov 2 at 1:49am (America/Los_Angeles)";

        let windows = parse(output, &at("2026-11-02T01:52")).unwrap();

        assert_eq!(windows[0].resets, Some(at("2026-11-02T01:49")));
    }

    #[test]
    fn a_reset_more_than_180_days_back_is_next_year() {
        let output =
            "Current week (all models): 5% used · resets Jan 3 at 9am (America/Los_Angeles)";

        let windows = parse(output, &at("2026-12-30T10:00")).unwrap();

        assert_eq!(windows[0].resets, Some(at("2027-01-03T09:00")));
    }

    #[test]
    fn a_window_without_a_reset_time_has_none() {
        let windows = parse("Current session: 0% used", &at("2026-11-02T04:00")).unwrap();

        assert_eq!(
            windows,
            vec![Window {
                name: "session".into(),
                percent: 0,
                resets: None,
            }]
        );
    }

    #[test]
    fn output_without_current_lines_is_unavailable() {
        let output = "You are currently using your subscription\n";

        assert!(parse(output, &at("2026-11-02T04:00")).is_err());
    }

    #[test]
    fn a_changed_current_line_fails_the_read() {
        let output = format!("{USAGE}\nCurrent session: about a fifth used\n");

        let err = parse(&output, &at("2026-11-02T04:00")).unwrap_err();

        assert!(err.contains("about a fifth used"), "{err}");
    }

    #[test]
    fn an_unknown_time_zone_fails_the_read() {
        let output = "Current session: 2% used · resets Nov 2 at 6:49am (Mars/Olympus)";

        assert!(parse(output, &at("2026-11-02T04:00")).is_err());
    }
}
