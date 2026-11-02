# Project State

**Goal:** A small workflow layer for Claude Code that plans the work from a brief, runs a fleet of workers within a budget, and brings the user in for designs and decisions that are theirs.

## Where We Are

The repository is set up: a minimal binary that only prints its version, the plugin manifest, `make ready` and `make verify`, and a `verify` check on a self-hosted runner. No product behaviour exists yet. The repository is private and written to be made public later.

## Features

- [ ] Prove the risky parts
- [ ] Core: one worker, started and watched by the binary
- [ ] Autonomous loop
- [ ] User decision points: designs, findings, merges, previews
- [ ] Test project

## Recently Done

- Repository setup and `verify` check

## Up Next

1. File the milestones and issues

## Needs Your Decision

- None.

## Notes

- Public-facing repository (see `CONTRIBUTING.md`): issues, PRs and commits must be presentable, with no personal identifiers.
- `verify` runs on a self-hosted runner while the repository is private. Before it goes public, it must move to GitHub-hosted runners and the runner must be removed.
- Work order: earliest milestone, then the lowest-numbered issue whose dependencies are closed.
