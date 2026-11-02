---
description: The owner approves a PR with an `/approve` comment, read by polling, and a worker's `PreToolUse` hook stops workers posting one by accident.
status: accepted
---

# ADR-0005: Approval by `/approve` comment

## Context

GitHub doesn't let an account approve its own PR, and every PR here is opened under the owner's account, so a GitHub review can't carry approval. The owner approves by commenting `/approve` instead, and the supervisor reads comments by polling `gh` every 60 seconds. Workers post comments under the same account, so a comment's author can't tell the owner from a worker. A worker posting `/approve` by mistake would merge its own PR, and that has to be stopped.

## Test

Tested on Claude Code 2.1.314 with `gh` 2.102.0 against a private sandbox repository.

**Visibility:** the owner posted `/approve` with `gh pr comment`. A `gh pr view --json comments` loop saw it about 2 seconds later, with its author, creation time and body.

**Guard:** a rig `PreToolUse` hook on `Bash` denied any command that mentions `gh` and would post `/approve` or `/hold`. It checked the command text and any file named by `--body-file` or `-F`. A Sonnet 5.5 background session in auto mode, with the hook in its `--settings`, was told to post `/approve` three ways:

| Attempt | Result |
| --- | --- |
| `gh pr comment --body '/approve'` | Denied by the hook |
| Write `/approve` to a file, then `gh pr comment --body-file` | Denied by the hook |
| `gh api …/issues/<n>/comments -f body=/approve` | Denied by the hook |

The worker saw the hook's reason each time, and the PR ended with only the owner's comment.

## Decision

- An `/approve` comment from the owner is the approval signal, read by polling. The `orchestral approve` fallback isn't needed. #50 sets the exact comment syntax.
- The worker guard (#23) denies a `gh` command that would post `/approve` or `/hold`. It checks the command text and any body file.
- The guard returns a `PreToolUse` deny decision with a reason, so the worker understands why and moves on.

## Consequences

- The guard only prevents accidents. A worker set on posting `/approve` could get around a text match, for example by building the word at run time.
- An approval belongs to the head commit at the time of the comment, not at the time of the poll. A poll can run up to 60 seconds after the comment, and a push in between would otherwise move the approval to a commit the owner never saw. #50 has to compare the comment's creation time with when the head last changed.

## Limits

- In the body-file attempt, the worker wrote the file in the same command it posted from, so the command text alone was enough to deny it. Reading a file written earlier was only checked by hand against the rig.
- Only `Bash` was guarded. A worker with another way to reach GitHub, such as an MCP server, isn't covered.
