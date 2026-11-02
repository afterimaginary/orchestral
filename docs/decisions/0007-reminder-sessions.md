---
description: Each reminder comes from a new one-shot background session with Remote Control left on. Whether to turn off the notification presence check is still open.
status: proposed
---

# ADR-0007: One-shot reminder sessions

## Context

A worker waiting on the owner needs a reminder the owner will notice. Repeat notifications from one session are silent, so each reminder comes from a new short-lived session that sends one notification and is then stopped. The open questions were whether the owner actually sees one banner with the right text, and whether the "Remote control enabled" alert every new session can fire gets in the way.

## Test

Tested on Claude Code 2.1.314. Each reminder ran as:

```
claude --bg --name <slug>-nag-<n> --settings <settings> --model claude-haiku-5-5 --effort low \
  -- "Load the PushNotification tool with ToolSearch, then send one PushNotification with status proactive and exactly this text, then stop: <message>"
```

Runs 1 and 2 used an earlier prompt without the `ToolSearch` step, and the rest of the command was the same. The session was stopped afterwards. Delivery was read from Notification Center's accessibility tree every second for 45 seconds, not from the tool's result.

| Run | Settings | Tool result | Seen in Notification Center |
| --- | --- | --- | --- |
| 1 | none | "Not sent — this terminal is active" | One banner, titled with the session name, with the exact text |
| 2 | `remoteControlAtStartup: false` | "Not sent — this terminal is active" | Nothing |
| 3 | `remoteControlAtStartup: false`, presence check off | "Terminal notification sent. Mobile push not sent (Remote Control inactive)." | Nothing |
| 4 | presence check off | "Terminal notification sent. Mobile push requested." | One banner, titled with the session name, with the exact text |

"Presence check off" means `CLAUDE_CODE_DISABLE_NOTIFICATION_PRESENCE_CHECK=1` in the settings' `env`.

What this shows:

- No "Remote control enabled" alert appeared in any run, so there was nothing for `remoteControlAtStartup: false` to avoid.
- Turning Remote Control off removed the banner entirely, with or without the presence check. The banner appears to reach the Mac through Remote Control, since a background session has no terminal of its own to show a "terminal notification" in.
- On the Mac, runs 1 and 4 looked the same: one banner with the exact text. The presence check refused to send in run 1, yet the banner still arrived. The visible difference is that run 4 also requested a mobile push.
- In runs 1 and 2, the Haiku session's first `PushNotification` call failed validation, because the tool's schema wasn't loaded yet. It recovered with `ToolSearch` on its own.

## Decision

- Reminders come from one-shot background sessions, on Haiku at low effort. The fallback of accepting an extra alert isn't needed.
- Remote Control stays on for reminder sessions. `remoteControlAtStartup: false` isn't used, because it stops the banner.
- The reminder prompt tells the session to load `PushNotification` with `ToolSearch` first and send with `status: proactive`.
- **Open:** whether reminder settings set `CLAUDE_CODE_DISABLE_NOTIFICATION_PRESENCE_CHECK=1`. With it, every reminder also requests a phone push, even while the owner is at a terminal. Without it, the Mac banner still arrives, but whether the phone gets anything is unknown. This is the owner's call, and the phone result decides it.

## Consequences

- The reminder's text is the whole message, since the banner's title is only the session name. It has to name the worker, the question and the link.
- Each reminder costs a short Haiku session.

## Limits

- Delivery was read on the Mac only. The phone wasn't checked, and "Mobile push requested" doesn't prove the push arrived.
- Four runs, close together. Behaviour may differ when the Mac is locked or a Focus mode is on.
- The banner in run 1 arrived even though the tool said it didn't send, so which path delivers it isn't fully understood.
