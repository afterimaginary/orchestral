---
description: Workers stay in auto mode, with narrow allow rules for the exact git and gh commands they run when their orchestrator asks, run one at a time.
status: accepted
---

# ADR-0010: Narrow allow rules for workers

## Context

ADR-0004 launched workers with `--permission-mode auto` and no allow rules, after a test where every request was in the worker's first message. Later sandbox runs (#93) showed that auto mode's classifier only treats the first message as the user's request. A commit, push, `gh pr ready` or issue comment that the orchestrator asks for later in the run, or that the system prompt asks for, is denied as "Unrequested Commit in a Connected App", and the worker stalls until the owner steps in.

Claude Code resolves narrow `permissions.allow` rules before the auto mode classifier, and suspends only broad rules that could run arbitrary code, such as `Bash(*)`. A compound command skips the classifier only when every part of it matches a rule.

The owner chose narrow rules over allowing whole commands or changing the permission mode.

## Decision

- Workers keep `--permission-mode auto`.
- Each author's settings allow the exact forms it needs: `git commit *`, `git push` alone, to `origin <its branch>` (also with `-u` or `--force-with-lease`) or to `origin HEAD`, `gh pr create *`, `gh pr ready` alone or with arguments, `gh pr comment *`, `gh pr edit * --body-file *`, `gh issue comment <its issue> *` and `gh issue edit <its issue> --add-label needs-discussion`. A reviewer gets only `gh pr comment <its PR> *`.
- The worker prompt tells workers to run each git and gh command on its own, and to edit files with file tools rather than shell redirects, so the rules match, and that a denied chained command can be rerun one part at a time.
- The worker guard and the pre-push hook stay the hard limits on merges, approvals and pushes to `main`. No rule names a merge or `main`.

## Test

Sandbox, Claude Code 2.1.316, Sonnet 5.5:

| Run | What happened |
| --- | --- |
| Worker without the prompt rule | Asked by the orchestrator, after its draft PR, to add a line, commit, push and mark the PR ready. It chained the file append, commit, push and `gh pr ready` into one command, and the classifier denied it. Told to retry with separate commands, it declined, treating that as a way around the denial. |
| Fresh worker with the prompt rule | The same request. It wrote the file with its file tool, then ran `git add`, `git commit`, `git push` and `gh pr ready` one by one. None was denied, and the PR was marked ready. |

## Consequences

- A worker can push to its own branch, comment on any PR, and mark any PR ready without the classifier seeing it. Those are routine, and merging still needs the supervisor or the owner.
- A worker that chains commands, or writes them another way, falls back to the classifier and can still be denied. Once denied, it may decline to retry the same action differently.
- An owner who sets `autoMode.classifyAllShell` suspends these rules, and workers stall as before.

## Limits

- One run each way, on a one-line change.
- The rules match command text, so they depend on how Claude Code parses commands.
