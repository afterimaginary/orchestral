---
description: A plugin monitor is the path that wakes the orchestrator, and session status fields aren't used to tell whether a session with a monitor is idle.
status: accepted
---

# ADR-0001: A plugin monitor is the wake path

## Context

The orchestrator spends most of a run idle, and has to react to fleet events without polling. Every event is meant to reach it as a stdout line from a plugin monitor. This test checks that mechanism before anything depends on it. It doesn't cover a long idle, which has its own test (#2).

## Test

Tested on Claude Code 2.1.314 with Haiku 5.5 at low effort.

The rig is a small plugin whose monitor has no `when` key. It records its environment, then copies each new line of a trigger file to stdout. One `claude --bg` session and one interactive session each loaded the plugin with `--plugin-dir`. Each session finished its turn or sat at its prompt before a line was written, and `/compact` was run in each through a terminal.

| Check | Background session | Interactive session |
| --- | --- | --- |
| Monitor starts with the session | Yes | Yes |
| Idle session wakes on a line | Yes, the session acted on it about 3 seconds after the write | Yes, the event was in the transcript within a second of the write |
| Line delivered after `/compact` | Yes, about 3 seconds after the write, with the same monitor process still running | Yes, within a second, with the same monitor task. The line was written as compaction finished, and it was delivered right after |
| `CLAUDE_CODE_SESSION_ID` in the monitor's environment | Matches the session | Matches the session |

A running monitor changes what a session reports. With its last turn finished and nothing happening for a minute, the background session showed:

| Source | Field | Value |
| --- | --- | --- |
| Session record | `status` | `shell` |
| `claude agents --json` | `status` | `busy` |
| `claude agents --json` | `state` | `done` before its first wake, `working` after it |
| `claude agents --json` | `waitingFor` | absent |
| Transcript | last timestamped entry | `system`, subtype `turn_duration` |

## Decision

- A plugin monitor with no `when` key is the wake path. The Stop hook fallback isn't needed.
- `claim` and `watch` identify their session from `CLAUDE_CODE_SESSION_ID`, with no walk up parent pids.
- Session `status` and `state` aren't used to decide that a session with a monitor is idle. The monitor starts with every session that loads the plugin, so those fields would never read as idle. Idle comes from the transcript, where the last entry is a finished turn, and `waitingFor` still reports a session waiting on someone.

## Consequences

- #28 has to detect idle workers from transcripts rather than the session record, and #15 shouldn't report `status` as idle or busy.
- Every event line also reaches the model with a suggestion to send a push notification if the user should act now. The skill has to say when that applies, or an event could cause needless alerts.
- A session given no instructions wakes and then asks what the event means, so an orchestrator needs the skill's event rules in context, including after compaction.

## Limits

- Each session ran for only a few minutes, and compaction was started by hand on a small conversation. Auto compaction wasn't triggered.
- Survival across a long idle is unproven until #2 reports.
- Only Haiku 5.5 at low effort was used. Delivery happens outside the model, so another model isn't expected to change the result, but that wasn't tested.
- `waitingFor` was only seen absent here. Its behaviour on a session that's really waiting wasn't exercised.
