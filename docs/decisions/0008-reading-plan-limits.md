---
description: Plan limits are read by running `/usage` in a throwaway print session, and the fleet resumes once a read shows a limited window has reset, or on the parsed reset time when no read can confirm it.
status: accepted
---

# ADR-0008: Reading plan limits

## Context

The fleet has to wind down near a plan limit and pick up again after the reset, without someone watching. Claude Code doesn't offer the limits through a documented interface, so two questions mattered: whether they can be read reliably from a command, and whether a resume can be triggered after the reset time.

## Test

Tested on Claude Code 2.1.314, on a subscription plan.

**Reading.** The limits were read with:

```
claude -p /usage --no-session-persistence --settings '{"disableAllHooks":true}'
```

It was run from a new temporary directory, without `CLAUDE_CODE_SESSION_ID` or `CLAUDECODE` in its environment, every 5 minutes for about two hours. Every run printed the same shape, saved as `tests/fixtures/claude/2.1.314/usage.txt` with the usage breakdown made generic:

```
Current session: 2% used · resets Nov 2 at 6:49am (Europe/Berlin)
Current week (all models): 26% used · resets Nov 4 at 12:59pm (Europe/Berlin)
Current week (Fable): 0% used · resets Nov 4 at 12:59pm (Europe/Berlin)
```

Each window is one line: a label, a whole percent, a reset time in the account's local time with no year, and the IANA time zone in parentheses. Times on the hour drop the minutes, as in `resets Nov 4 at 1pm`. A breakdown of what used the plan follows, and nothing reads it.

**Resuming.** A stand-in tick parsed the session window's reset time, and once that time had passed and a new read showed the window had moved on, wrote one line to a trigger file. A background session with a plugin monitor following that file (ADR-0001) had been idle for about two hours. At the reset, session usage went from 44% to 2%, and the next reset moved five hours later. The tick wrote the wake line, and the idle session acted on it 3 seconds later.

## Findings

- **The read works.** Every one of the 24 reads parsed, and none needed a terminal. Exit codes and leftover sessions weren't checked on each read.
- **Reset times drift by a minute.** Of the 23 reads of the session window before its reset, 13 gave `1:49am` and 10 gave `1:50am`, back and forth, and the weekly windows moved between `12:59pm` and `1pm`. The first tick treated a changed reset time as a reset, and woke the session two hours early. A reset only counts once the parsed time has passed and a new read shows a reset time at least an hour later.

## Decision

- Limits are read by running the `/usage` command above from a temporary directory. The parser (#16) reads the `Current …` lines and ignores the rest.
- A reset time is parsed in the time zone the line gives, in the current year, unless that puts it more than 180 days in the past, when it's the next year. A read just after a reset can still print the old time, so the year isn't taken as the next occurrence.
- After a wind-down, the resume is confirmed by a read: once a read made after the parsed reset time shows the limited window has moved on, the supervisor emits a wake event, which reaches the orchestrator through its plugin monitor. This applies to whichever window caused the wind-down, session or weekly.
- #8's fallback, a wake on the parsed reset time alone, is kept for when no read can confirm the reset, such as a read that fails or a window that shows no reset time.

## Consequences

- Each read starts a short print session. Reading every few minutes costs little, but should stay on the supervisor's tick, not run per command.
- The format isn't documented. A changed line fails to parse, and the parser should then say the limits are unavailable instead of guessing, which `orchestral limits` already does.
- A reset is confirmed up to one tick late.

## Limits

- One account, one plan and one time zone. Other plans may show other windows, such as a weekly window per model.
- Only the session window was seen resetting. The weekly windows are expected to behave the same way.
- The reset was seen while the account was in use. After a real wind-down nothing uses the plan, and an idle window may print differently, for example without a reset time. The parsed-time fallback covers that.
- Running the read with `CLAUDECODE` set, as from inside a Claude Code session, wasn't tried.
- An API-key account, which has no plan limits, wasn't tried.
