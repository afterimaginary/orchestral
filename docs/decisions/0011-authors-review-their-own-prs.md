---
description: Each worker reviews its own PR with a fresh-context subagent and records the verdict, instead of the orchestrator starting a separate reviewer session.
status: accepted
---

# ADR-0011: Authors review their own PRs

## Context

#37 gave every PR a separate reviewer session, started by the orchestrator and counted against the policy's `workers` cap. An author keeps its slot until its PR merges or is blocked, so when every slot holds an author with a ready PR, no reviewer can start and nothing can merge. A sandbox run with `workers: 2` hit exactly that (#100).

A separate reviewer session also costs the orchestrator work it shouldn't need to do: starting reviewers, reminding idle ones, cleaning them up, and relaying a failed review to the author, who then waits for another reviewer.

The owner wants one worker to own its issue from start to merge, with the orchestrator coordinating rather than managing reviews.

## Decision

- Before marking its PR ready, a worker starts a fresh subagent with the Agent tool, not a fork, and gives it only the PR number, the issue number and what to check: the policy's `review_command`, or by default that the PR meets the issue's acceptance criteria and keeps to its non-goals. The subagent posts its report as one PR comment and doesn't edit code.
- The worker fixes what holds up, lists the findings it declines as `[pending]`, and records the verdict on the reviewed commit with `orchestral review`. A failed round means a fix, then a fresh subagent on the new commit. Two failed rounds block the issue, as before.
- `orchestral review` accepts the PR's author as well as a reviewer. `merge-check` still requires a passed review at the head commit.
- The orchestrate skill stops starting reviewers. `spawn --role reviewer` stays, unused by the skill, until #48 decides whether PRs the supervisor merges need an independent reviewer.

## Consequences

- No slot is ever needed for a review, so a full fleet can't deadlock on one.
- The author keeps its context while fixing review findings.
- The author writes its reviewer's brief, decides which findings to decline, and records the verdict. That's acceptable while the owner approves every merge, since the owner is the independent check. It's weak for unattended merges, which is why #48 has to decide it.
- A subagent's usage counts under its worker's session.
