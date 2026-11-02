# Project State

**Goal:** A small workflow layer for Claude Code that plans the work from a brief, runs a fleet of workers within a budget, and brings the user in for designs and decisions that are theirs.

## Where We Are

The repository is set up with the plugin manifest, `make ready`, `make verify` and a `verify` check on a self-hosted runner. Milestone 1's tests are recorded as ADR-0001 to ADR-0007, with two still running. The binary has its CLI conventions, `limits`, which reports limits as unavailable until they can be read, `spawn`, which starts a worker, `stop` and `resume`, and `claim` and `resolve`. No other product behaviour exists yet. The repository is private and written to be made public later.

## Features

- [ ] Prove the risky parts
- [ ] Core: one worker, started and watched by the binary
- [ ] Autonomous loop
- [ ] User decision points: designs, findings, merges, previews
- [ ] Test project

## Recently Done

- Repository setup and `verify` check
- Milestones and issues filed for milestones 1 to 4
- Plugin monitor shown to wake idle sessions, with a long idle still to test. Workers run with the plugin turned off (ADR-0001)
- Tested and recorded: proposals from background workers, the merge command, worker permissions under auto mode, `/approve` comments, preview servers and reminder sessions (ADR-0002 to ADR-0007)
- CLI conventions: exit codes, refusals and `--json` (`docs/specs/cli.md`)
- Integration test harness with stub external tools
- State store: `state.json` and `events.jsonl` under the git common directory (`docs/specs/state.md`)
- Policy file: settings and rules in `.orchestral/policy.md`, read from `origin/main` (`docs/specs/policy.md`)
- Context and cache state read from transcripts (`docs/specs/claude-code-internals.md`)
- Session reader: session records, pid liveness and `claude agents --json`
- Fleet token accounting, shown as an estimate (`docs/specs/budget.md`)
- `orchestral spawn`: starts a worker in its own worktree, with its own settings and prompt
- `orchestral stop` and `resume`: pause a worker and pick it up later
- `orchestral claim` and `resolve`: one orchestrator per repository, and checked messaging addresses
- Worker guard and pre-push hooks: workers can't merge, approve, push to `main`, or post `/approve` or `/hold`

## Up Next

1. Finish milestone 1: #8 (reading plan limits) waits for a limit reset, and #2 (a long idle) needs at least 48 idle hours.
2. Milestone 2 (Core), in work order, next refusing at limits (#20, after #16), fleet status (#22, after #16), installing hooks (#24) and the supervisor (#25).

## Needs Your Decision

- None.

## Notes

- Public-facing repository (see `CONTRIBUTING.md`): issues, PRs and commits must be presentable, with no personal identifiers.
- `verify` runs on a self-hosted runner while the repository is private. Before it goes public, it must move to GitHub-hosted runners and the runner must be removed.
- Work order: earliest milestone, then the lowest-numbered issue whose dependencies are closed.
