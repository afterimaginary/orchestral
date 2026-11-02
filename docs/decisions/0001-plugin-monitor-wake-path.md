---
description: A plugin monitor is the path that wakes the orchestrator, unless the long idle test fails, and a worker's monitor should exit at once so the worker's session status stays usable.
status: proposed
---

# ADR-0001: A plugin monitor is the wake path

## Context

The orchestrator spends most of a run idle, and has to react to fleet events without polling. Every event is meant to reach it as a stdout line from a plugin monitor. This test checks that mechanism before anything depends on it. It doesn't cover a long idle, which has its own test (#2).

The monitor starts with every session that loads the plugin, so the test also checks what a running monitor does to the status a session reports. The supervisor is meant to read that status to notice idle and waiting workers.

## Test

Tested on Claude Code 2.1.314 with Haiku 5.5 at low effort.

The rig is a small plugin whose monitor has no `when` key. It records its environment, then copies each new line of a trigger file to stdout. Sessions were told to run a shell command when an event arrived, so an acknowledgement shows that a turn ran. `/compact` was run through a terminal, and a line was written at least 5 seconds after compaction finished.

| Check | Background session | Interactive session |
| --- | --- | --- |
| Monitor starts with the session | Yes | Yes |
| Idle session acts on a line | Yes, in about 3 seconds | Yes, in about 3 seconds |
| Session acts on a line after `/compact` | Yes, in about 3 seconds, with the same monitor process | Yes, in about 5 seconds |
| `CLAUDE_CODE_SESSION_ID` in the monitor's environment | Matches the session | Matches the session |

### What sessions report

| Session | Session record `status` | `claude agents --json` `status` | `state` | `waitingFor` |
| --- | --- | --- | --- | --- |
| Background, no monitor, idle | `idle` | `idle` | `done` | absent |
| Background, monitor running, idle | `shell` | `busy` | `done` before its first wake, `working` for at least 10 minutes after it | absent |
| Interactive, monitor running, idle | `shell` | `busy` | absent | absent |
| Background, no monitor, asking a question | `waiting` | `waiting` | `blocked` | `input needed` |
| Background, monitor running, asking a question | `waiting` | `waiting` | `blocked` | `input needed` |
| Background, monitor exited at startup, idle | `idle` | `idle` | `done` | absent |

So a running monitor does make an idle session look busy in its session record. A session waiting on someone still reports it correctly.

The transcript doesn't give a clean substitute. A normal turn ends with a `system` entry of subtype `turn_duration`, but an idle session later added a `system` entry of subtype `away_summary` after it, and an interrupted turn ended on an `api-request` entry with no `turn_duration`.

## Decision

- A plugin monitor with no `when` key is the wake path. The Stop hook fallback isn't needed unless #2 fails.
- `claim` and `watch` identify their session from `CLAUDE_CODE_SESSION_ID`, with no walk up parent pids.
- `watch` exits at once when `ORCHESTRAL_WORKER` is set, so only sessions that can orchestrate keep a monitor running. A worker's session status then stays usable for noticing idle and waiting workers.

## Consequences

- The event stream's "never exits by itself" holds for non-worker sessions only.
- A worker gets one extra short turn at startup, because the monitor's exit arrives as a notification.
- An orchestrator's own `status` reads `shell` while idle. Nothing needs to read that.
- Every event line reaches the model with a suggestion to send a push notification if the user should act now. The skill has to say when that applies.
- A session given no instructions wakes and then asks what the event means, so an orchestrator needs the skill's event rules in context, including after compaction.

## Alternatives

- **Detect idle workers from transcripts.** This keeps every monitor running, but it depends on more undocumented entries, and an interrupted turn leaves no end marker.
- **Disable the plugin in worker settings.** This would avoid the extra turn, but it wasn't tested, and it depends on how the plugin was installed.

## Limits

- Each session ran for only a few minutes, and compaction was started by hand on a small conversation. Auto compaction wasn't triggered.
- Survival across a long idle is unproven until #2 reports.
- Only Haiku 5.5 at low effort was used. Delivery happens outside the model, so another model isn't expected to change the result, but that wasn't tested.
- Sessions asking a question were only seen with `AskUserQuestion`. Other ways of waiting, such as a permission prompt, weren't exercised.
- The exit test used a stand-in for `ORCHESTRAL_WORKER`, since `watch` doesn't exist yet.
