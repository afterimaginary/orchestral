# Project State

**Goal:** A small workflow layer for Claude Code that plans the work from a brief, runs a fleet of workers within a budget, and brings the user in for designs and decisions that are theirs.

## Where We Are

v0.1.0 is released. Every part runs end to end, from a brief through planning, review and merge, and an Orchestral fleet took [cc-notify-tts](https://github.com/afterimaginary/cc-notify-tts) from a brief to a public release. What's left is testing long unattended runs, such as a fleet idling for days.

## Features

- [x] Plan from a brief: milestones and issues, filed once the owner approves
- [x] Workers: one session per issue, in its own worktree, from issue to reviewed PR
- [x] Limits: worker cap, plan usage limit and fleet token budget, enforced by the binary
- [x] Owner decisions: designs, questions, merges and previews, with reminders until answered
- [x] Supervisor: watches workers and GitHub, wakes the orchestrator and merges approved PRs
- [x] Pacing and recovery: winds down at the plan limit, resumes after the reset, and catches up after a restart
- [ ] Long unattended runs: a fleet idling for days (#2, #44)

## Recently Done

- Release preparation: `verify` on GitHub-hosted macOS and Ubuntu runners, `SECURITY.md`, and v0.1.0 (#133)
- The policy moves out of managed repositories into `~/.config/orchestral`, and the plugin installs at local scope (#132)
- README: how to install, set up and use Orchestral (#127)
- Installs from this repository's marketplace, building its binary on first use (#125)
- Reminders for an asked PR whose worker is gone (#124)

## Up Next

1. A fleet through a long idle (#44)
2. A plugin monitor across a multi-day idle (#2), once a machine can stay up that long

## Needs Your Decision

- None.

## Notes

- `verify` runs on GitHub-hosted macOS and Ubuntu runners; the final `verify` job passes only when both do.
- Work order: earliest milestone, then the lowest-numbered issue whose dependencies are closed.
