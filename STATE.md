# Project State

**Goal:** A small workflow layer for Claude Code that plans the work from a brief, runs a fleet of workers within a budget, and brings the user in for designs and decisions that are theirs.

## Where We Are

The repository is set up with the plugin manifest, `make ready`, `make verify` and a `verify` check on a self-hosted runner. Milestone 1's tests are recorded as ADR-0001 to ADR-0008, with one, a long idle, still running. The binary has its CLI conventions, `limits`, which reads the plan's usage windows from `/usage`, `spawn`, which refuses while paused or past the worker cap, plan limit or fleet budget (which counts from the run start the supervisor records), `pause`, `unpause`, `status`, which shows workers, plan windows, budget and pause, `up`, which starts the supervisor, which records an event when a worker goes idle, waits or exits, `watch`, which the plugin's monitor runs to deliver those events to the orchestrator, `stop`, `resume`, `claim`, `resolve`, `init`, `doctor`, and the worker guard and pre-push hooks. `make e2e` and `make sandbox` run real workers on demand. The `/orchestral:orchestrate` skill starts a fleet: doctor, claim, up, policy and status, then spawns workers. No other product behaviour exists yet. The repository is private and written to be made public later.

## Features

- [ ] Prove the risky parts
- [x] Core: one worker, started and watched by the binary
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
- `orchestral spawn`: starts a worker in its own worktree, with its own settings and prompt, and refuses past the fleet's limits
- `orchestral stop` and `resume`: pause a worker and pick it up later
- `orchestral claim` and `resolve`: one orchestrator per repository, and checked messaging addresses
- Worker guard and pre-push hooks: workers can't merge, approve, push to `main`, or post `/approve` or `/hold`
- `orchestral init` and `doctor`: project setup and pre-run checks
- Plan limits read from `/usage` in a throwaway print session, and a resume after the reset shown to wake an idle session (ADR-0008)
- Milestone 2 (Core): supervisor, `status`, spawn limits, real-run tests and the `/orchestrate` skill
- Supervisor ticks and worker events (#28), and the event stream: `orchestral watch` and the plugin monitor (#29)

## Up Next

1. Finish milestone 1: #2 (a long idle) needs at least 48 idle hours.
2. Milestone 3 (Autonomous loop), next GitHub events (#30).

## Needs Your Decision

- None.

## Notes

- Public-facing repository (see `CONTRIBUTING.md`): issues, PRs and commits must be presentable, with no personal identifiers.
- `verify` runs on a self-hosted runner while the repository is private. Before it goes public, it must move to GitHub-hosted runners and the runner must be removed.
- Work order: earliest milestone, then the lowest-numbered issue whose dependencies are closed.
