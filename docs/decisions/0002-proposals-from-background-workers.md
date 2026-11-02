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

Things a worker has to know:

- A background session gets the artifact server's own tools (`mcp__claude-artifacts__*`), such as `publish_files`, `publish_version` and `create_from_type`. These aren't the same as the Artifact tool an interactive session has, so prompts should describe the goal rather than name tools.
- Publishing refused files outside what the session can read. Files under the scratchpad directory the test gave it were refused with "only files this session is allowed to read can be attached". After the session copied them into its working directory, publishing worked.
- The session worked around the refusal by itself, but needed two extra attempts to do it.

## Decision

- Workers publish their own proposals, in both formats. The fallback isn't needed.
- A worker writes proposal files inside its own worktree, in a directory kept out of git, and publishes from there.

## Consequences

- The proposal step (#45) tells workers where to write proposal files, so they don't hit the refusal.
- Proposals are private to the owner's account, as the plan expects, so the issue records the choice in words.
- Each proposal is a real artifact in the owner's account, and stays there until the owner deletes it.

## Limits

- One run, on one model, with one artboard. Several variants per proposal, as the policy asks for, weren't tried.
- The worker's prompt was explicit about the steps. A worker given only an issue may need the proposal step's guidance to choose the format.
- The test ran in a normal checkout, not a worktree. Publishing from a worktree is expected to work the same way, since the rule is about what the session can read.
