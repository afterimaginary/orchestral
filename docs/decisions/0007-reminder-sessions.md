---
description: Each reminder comes from a new one-shot background session with Remote Control left on and no other notification settings.
status: accepted
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
| 1 | none | "Not sent — this terminal is active" | One banner, titled with the session name, with the exact text. The phone also showed it. |
| 2 | `remoteControlAtStartup: false` | "Not sent — this terminal is active" | Nothing |
| 3 | `remoteControlAtStartup: false`, presence check off | "Terminal notification sent. Mobile push not sent (Remote Control inactive)." | Nothing |
| 4 | presence check off | "Terminal notification sent. Mobile push requested." | One banner, titled with the session name, with the exact text. The phone also showed it. |

"Presence check off" means `CLAUDE_CODE_DISABLE_NOTIFICATION_PRESENCE_CHECK=1` in the settings' `env`.

What this shows:

- No "Remote control enabled" alert appeared in any run, so there was nothing for `remoteControlAtStartup: false` to avoid.
- Turning Remote Control off removed the banner entirely, with or without the presence check. The banner appears to reach the Mac through Remote Control, since a background session has no terminal of its own to show a "terminal notification" in.
- Runs 1 and 4 looked the same on the Mac and on the phone. The presence check refused to send in run 1, yet both banners still arrived, so turning it off changes nothing the owner sees.
- In runs 1 and 2, the Haiku session's first `PushNotification` call failed validation, because the tool's schema wasn't loaded yet. It recovered with `ToolSearch` on its own.

## Decision

- Reminders come from one-shot background sessions, on Haiku at low effort. The fallback of accepting an extra alert isn't needed.
- Remote Control stays on for reminder sessions. `remoteControlAtStartup: false` isn't used, because it stops the banner.
- The reminder prompt tells the session to load `PushNotification` with `ToolSearch` first and send with `status: proactive`.
- Reminder settings leave the presence check on. Turning it off made no visible difference.

## Consequences

- The reminder's text is the whole message, since the banner's title is only the session name. It has to name the worker, the question and the link.
- Each reminder costs a short Haiku session.

## Limits

- The phone result was the owner's report of what arrived, not a recorded read.
- Four runs, close together. Behaviour may differ when the Mac is locked or a Focus mode is on.
- The banner in run 1 arrived even though the tool said it didn't send, so which path delivers it isn't fully understood.
