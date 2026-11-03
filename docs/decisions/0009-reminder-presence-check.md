---
description: Reminder sessions run with the notification presence check off, since a new background session always counts its terminal as active and otherwise never sends.
status: accepted
---

# ADR-0009: Reminder sessions turn off the presence check

## Context

ADR-0007 chose one-shot background sessions for reminders, with Remote Control on, and left `PushNotification`'s presence check on, because in its first run a banner seemed to arrive even though the tool said it hadn't sent one.

Sandbox runs of the finished reminders (#41) disproved that. Six reminder sessions, over two runs on Claude Code 2.1.314 and 2.1.316, each got "Not sent — this terminal is active", and the owner, away from the terminal and watching the phone, saw no banner. A new background session has no terminal anyone is looking at, but the check still counts it as active.

A seventh session, launched the same way but with `CLAUDE_CODE_DISABLE_NOTIFICATION_PRESENCE_CHECK=1` in its settings, got "Terminal notification sent. Mobile push requested.", and the owner saw the banner. That matches ADR-0007's run 4, the other run with the check off.

## Decision

- Reminder sessions get `--settings '{"env":{"CLAUDE_CODE_DISABLE_NOTIFICATION_PRESENCE_CHECK":"1"}}'`. The setting applies to the reminder session only.
- The rest of ADR-0007 stands: one-shot sessions on Haiku at low effort, Remote Control on, a prompt that loads `PushNotification` with `ToolSearch` and sends with `status: proactive`, and the session stopped afterwards.

## Consequences

- A reminder is sent even while the owner is at a terminal. Reminders only go out after the policy's `nag_minutes` gaps, so this costs at most an occasional banner the owner didn't need.
- The tool's result is a fair signal again: "Mobile push requested" means it sent.

## Limits

- Delivery was confirmed by the owner's report, for one banner on the phone.
- It relies on an environment variable Claude Code doesn't document as stable. `docs/specs/claude-code-internals.md` lists it, so a change shows up there.
