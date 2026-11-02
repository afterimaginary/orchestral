---
description: Workers publish their own design proposals, both artifact pages and Claude Design canvases, from files inside their worktree.
status: accepted
---

# ADR-0002: Workers publish their own proposals

## Context

A user-visible feature gets a design proposal before any code. Looks go on a Claude Design canvas, while behaviour and architecture go on artifact pages. The worker assigned to the issue knows the work best, so it should publish the proposal. Workers run as `claude --bg` sessions, and it wasn't known whether those can publish at all. The fallback was for the orchestrator to publish, or for workers to write local HTML.

## Test

Tested on Claude Code 2.1.314 with a Sonnet 5.5 background session in auto mode, at low effort. Its prompt asked it to publish a one-paragraph page, then create a Design canvas with one artboard, and to record each link or error. It got no other help.

| Format | Result |
| --- | --- |
| Artifact page | Published. Reading it back returned the page as written. |
| Claude Design canvas | Created from the Design type, with `project/canvas.json` and one artboard, `project/Main.dc.html`, both present when its files were listed. |

Two findings shaped the decision:

- Publishing refused files outside what the session can read. Files under a scratchpad directory outside its working directory were refused with "only files this session is allowed to read can be attached". After the session copied them into its working directory, publishing worked. It found that workaround by itself, after two failed attempts.
- A background session publishes through the artifact server's own tools, not the Artifact tool an interactive session has. Prompts should describe the goal rather than name tools.

## Decision

- Workers publish their own proposals, in both formats. The fallback isn't needed.
- A worker writes proposal files inside its own working directory before publishing. #45 picks the exact directory and keeps it out of git.

## Consequences

- The proposal step (#45) tells workers where to write proposal files, so they don't hit the refusal.
- Each proposal is a real artifact in the owner's account, and stays there until the owner deletes it. Both test artifacts were published as private, which matches #45's rule that the issue records the choice in words.

## Limits

- One run, on one model, with one artboard. Several variants per proposal, which #45 requires, weren't tried.
- The worker's prompt was explicit about the steps. A worker given only an issue may need the proposal step's guidance to choose the format.
- The test ran in a normal checkout, not a worktree. Publishing from a worktree is expected to work the same way, since the rule is about what the session can read.
