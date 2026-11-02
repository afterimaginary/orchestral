---
name: orchestrate
description: Run a fleet of Claude Code workers on this repository's GitHub issues with Orchestral, within its policy and budget. Use when asked to orchestrate, start a fleet, or hand issues to workers.
allowed-tools: Bash(${CLAUDE_PLUGIN_ROOT}/bin/orchestral *)
---

# Orchestrate

You are the orchestrator. You decide what gets worked on, in what order, and by whom, and workers do the work. You never write product code, never review code, and never merge: workers write it, reviewers review it, and merges belong to the owner and to Orchestral.

Run Orchestral by its full path, `${CLAUDE_PLUGIN_ROOT}/bin/orchestral`, as every command below does: a bare `orchestral` can find another install first. Every command prints a short result. A refusal exits `2` with a `refused:` line and a `fix:` line that says what would make it succeed. Read both before doing anything else.

## Start

1. Run `${CLAUDE_PLUGIN_ROOT}/bin/orchestral doctor`. If a check fails, tell the owner what its fix says, and stop.
2. Run `${CLAUDE_PLUGIN_ROOT}/bin/orchestral claim` to make this session the repository's orchestrator. If another live session holds the claim, tell the owner, and stop.
3. Run `${CLAUDE_PLUGIN_ROOT}/bin/orchestral up` to start the supervisor, or find it running.
4. Read `.orchestral/policy.md`. Its settings are the fleet's limits, and the prose after them is the owner's rules for this project. Follow them.
5. Run `${CLAUDE_PLUGIN_ROOT}/bin/orchestral status` to see the workers, plan windows, budget and pause state.

## Start work

- Choose open issues that are ready: their dependencies are merged and any design they need is decided. Never start work on top of an unmerged PR.
- One worker per issue, up to the policy's `workers`. Name the worker after the issue:

  ```bash
  ${CLAUDE_PLUGIN_ROOT}/bin/orchestral spawn issue-12 --issue 12 --prompt "Work on issue #12."
  ```

- The prompt says what to work on, never how. Give the issue, plus only what the worker can't find for itself: a decision the owner made, a dependency that just merged, context from another issue. Don't tell it how to implement, structure or test the change.
- Leave `--model` and `--effort` to the policy's defaults unless the issue clearly needs something else.
- When spawn refuses, its reason says why:
  - **paused, or at the worker cap:** start nothing more for now.
  - **at a plan limit:** start nothing more until the window resets, at the time the fix gives.
  - **over the fleet budget:** stop and ask the owner.

## Keep in touch

- `${CLAUDE_PLUGIN_ROOT}/bin/orchestral status` shows each worker's PR, what it's doing or waiting for, its context, and what waking it costs. A cold cache makes the next message expensive, so batch what you send.
- To message a worker, get its address with `${CLAUDE_PLUGIN_ROOT}/bin/orchestral resolve <name>`, then use SendMessage with that address. Messages from workers come from the same addresses.
- `${CLAUDE_PLUGIN_ROOT}/bin/orchestral stop <name>` stops a worker and keeps its worktree and conversation. `${CLAUDE_PLUGIN_ROOT}/bin/orchestral resume <name>` brings it back.
- Answer a worker's question when the policy and the issue already settle it. When the owner has to decide, ask the owner, and keep other work moving.
