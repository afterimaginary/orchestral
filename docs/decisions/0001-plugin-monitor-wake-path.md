---
description: A plugin monitor is the path that wakes the orchestrator, unless the long idle test fails, and workers run with the plugin turned off so their session status stays usable.
status: accepted
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
| Background, plugin installed but turned off in `--settings`, idle | `idle` | `idle` | `done` | absent |

With the plugin turned off through `enabledPlugins` in the `--settings` file, its monitor didn't start at all. A session started at the same time without that setting did start it.

So a running monitor does make an idle session look busy in its session record. A session waiting on someone still reports it correctly.

The transcript doesn't give a clean substitute. A normal turn ends with a `system` entry of subtype `turn_duration`, but an idle session later added a `system` entry of subtype `away_summary` after it, and an interrupted turn ended on an `api-request` entry with no `turn_duration`.

## Decision

- A plugin monitor with no `when` key is the wake path. The Stop hook fallback isn't needed unless #2 fails.
- `claim` and `watch` identify their session from `CLAUDE_CODE_SESSION_ID`, with no walk up parent pids.
- Workers run with the Orchestral plugin turned off in their per-worker settings, so no monitor runs in them. Workers call the binary by its absolute path and don't need the plugin. Their session status then stays usable for noticing idle and waiting workers.

## Consequences

- Spawn (#18) needs the plugin's full ID, including its marketplace name, to turn it off. A wrong ID leaves a worker that always looks busy, so the ID should be checked.
- An orchestrator's own `status` reads `shell` while idle. Nothing needs to read that.
- Every event line reaches the model with a suggestion to send a push notification if the user should act now. The skill has to say when that applies.
- A session given no instructions wakes and then asks what the event means, so an orchestrator needs the skill's event rules in context, including after compaction.

## Alternatives

- **Detect idle workers from transcripts.** This keeps every monitor running, but it depends on more undocumented entries, and an interrupted turn leaves no end marker.
- **Let a worker's `watch` exit at once.** This also restores the worker's status, as the table shows, but every worker pays for an extra turn when the exit is reported, and `watch` gains a special case.
- **Start the monitor only when the skill runs, with `when`.** This is the mechanism meant for the job, but a test on 2.1.314 found it didn't start when the skill was run as a slash command. It's worth retesting on later versions.

## Limits

- Each session ran for only a few minutes, and compaction was started by hand on a small conversation. Auto compaction wasn't triggered.
- Survival across a long idle is unproven until #2 reports.
- Only Haiku 5.5 at low effort was used. Delivery happens outside the model, so another model isn't expected to change the result, but that wasn't tested.
- Sessions asking a question were only seen with `AskUserQuestion`. Other ways of waiting, such as a permission prompt, weren't exercised.
- Turning the plugin off was tested with a local-scope install only. A `--settings` value should take precedence over user and project scopes too, but that wasn't tested.
