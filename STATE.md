# Project State

**Goal:** A small workflow layer for Claude Code that plans the work from a brief, runs a fleet of workers within a budget, and brings the user in for designs and decisions that are theirs.

## Where We Are

The repository is set up with the plugin manifest, `make ready`, `make verify` and a `verify` check on GitHub-hosted macOS and Ubuntu runners. Milestone 1's tests are recorded as ADR-0001 to ADR-0008, with one, a long idle, deferred. The binary has its CLI conventions, `limits`, which reads the plan's usage windows from `/usage`, `spawn`, which refuses while paused or past the worker cap, plan limit or fleet budget (which counts from the run start the supervisor records), `pause`, `unpause`, `status`, which shows workers, plan windows, budget and pause, `up`, which starts the supervisor, which records an event when a worker goes idle, waits or exits, and for GitHub issues, PRs, CI and comments, and writes an idle heartbeat, `watch`, which the plugin's monitor runs to deliver those events to the orchestrator, `stop`, `resume`, `cleanup`, which removes a finished worker by its PR's state, `claim`, `resolve`, `init`, `doctor`, `policy`, which prints the policy in effect, and the worker guard and pre-push hooks. `make e2e` and `make sandbox` run real workers on demand. The `/orchestral:orchestrate` skill starts a fleet: doctor, claim, up, policy and status, then spawns workers and reacts to fleet events. No other product behaviour exists yet. The repository is being prepared for its first public release.

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
- Policy file: settings and rules in the operator's config directory, a global file layered under a per-repository one, so managed repositories carry no Orchestral files (`docs/specs/policy.md`, ADR-0013)
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
- GitHub events from a minute poll (#30)
- Idle heartbeat every 55 minutes, keeping the orchestrator's cache warm (#32)
- The skill reacts to fleet events (#31)
- Worker questions: workers reach their orchestrator through `resolve`, escalations wait on `needs-discussion` and keep the supervisor running (#35)
- The pre-push hook keeps rejecting a worker's push to `main` after the binary moves, and `doctor` checks the hook (#88)
- Work starts in dependency order: ready issues by milestone and priority, skipping ones that wait on an unmerged PR, an undecided design or already have a PR (#34)
- `orchestral cleanup` removes a finished worker by its PR's state, so a squash merge needs no `--force`, and frees its slot (#42)
- New work waits for the owner's batch approval as `untriaged`, and the run ends when the approved list is resolved (#36)
- Review step: `spawn --role reviewer --pr`, one reviewer per PR, `orchestral review` verdicts by commit, and `blocked` after two failed rounds (#37)
- Planning: from a brief or the open issues, the skill drafts milestones and issues, has a fresh agent check them, publishes the plan as an artifact, and files it once the owner approves (#33)
- Summaries: a milestone summary once none of its issues is left for the fleet, and a final summary with the proposed work at the end of a run, each published as an artifact (#43)
- Pacing: the supervisor winds the fleet down past `plan_limit_percent` and back up after the reset, saves the run's usage every minute, and reports the budget once (#38)
- Compaction: plugin hooks save the fleet's status before the orchestrator compacts, and put its instructions and status back after (#39)
- Recovery: a restarted supervisor reports workers that can't be resumed as lost, once, and an orchestrator taking over checks every open PR once (#40)
- Findings: workers end each PR body with a `## Findings` section, the orchestrator decides each finding, and `src/findings.rs` checks every one is decided, for the merge check to use (#46)
- Merge check: `orchestral merge-check <pr>` shows each merge condition at the head commit, for the owner's route or the supervisor's, and never merges (#47)
- Previews: `orchestral try <pr>` asks for a PR's preview, which the supervisor runs in its worker's worktree on `$PORT`, restarting it once, rerunning setup on a lockfile change, keeping within `max_running` and stopping it on merge or close (#51)
- Tailnet links: where portless is installed, previews run under `portless <repo>-pr-<n>`, registered with Tailscale while it runs; the link goes in a PR comment that's edited on each restart, `status` lists running previews, and stale `tailscale serve` entries from crashed previews are removed (#52)
- Reminders: a worker waiting on the owner, by escalation or a blocked session, gets one-shot notification sessions at the policy's `nag_minutes` gaps (#41)
- Worker allow rules: workers stay in auto mode with narrow allow rules for their own git and gh commands, run one at a time, so they act on the orchestrator's later requests (#93)
- Self-review: each worker reviews its own PR with a fresh subagent and records the verdict, so reviews never need a worker slot, with its `review` command allowed so auto mode doesn't deny the verdict (#100, #115)
- Design proposals: `spawn --proposal` starts a worker on its issue's design proposal, which it publishes and links on the issue for the owner to choose, and `status` shows what proposals spend (#45)
- Merge routing: `orchestral route` puts each PR on the owner's route or the supervisor's, and a PR that only needs the owner's approval is labelled `needs-discussion`, reminded about and keeps the supervisor running (#48)
- Supervisor merges: every minute the supervisor merges each PR whose merge check passes, at the checked head commit, with the policy's `merge_method`, and records an event (#49)
- Costly actions: deploys, deploy integrations, billable resources and new accounts always escalate to the owner, every PR takes the owner's route when `main` deploys, and worker settings carry no credentials (#54)
- Comment commands: the owner's `/approve`, `/hold` and `/unhold` comments record an approval of the head they saw and set or clear a hold, a new commit lapses an approval, and other comments go to the PR's worker (#50)
- Boot check: `orchestral try --check` boots a worker's preview from its worktree and checks it answers before the PR reaches the owner, workers attach screenshots as an artifact page, and the orchestrator starts a preview for each PR waiting on the owner (#53)
- Installs from this repository's marketplace, building its binary on first use (ADR-0012)
- The policy moves out of managed repositories into `~/.config/orchestral`, with `orchestral policy` and an `init` import, and the plugin installs at local scope (#131)
- Release preparation: `verify` on GitHub-hosted macOS and Ubuntu runners, `SECURITY.md`, the pre-push hook follows plugin updates (#126), `claim` warns when no event monitor is running (#128), workers get the files `.worktreeinclude` lists (#129), and `spawn --pr` lets a fresh worker take over a PR (#130)

## Up Next

1. #2 (a 48-hour idle) is deferred until a machine can stay up that long.
2. Milestone 3 (Autonomous loop): the long-idle run (#44) is deferred.
3. First public release, v0.1.0.

## Needs Your Decision

None.

## Notes

- Public-facing repository (see `CONTRIBUTING.md`): issues, PRs and commits must be presentable, with no personal identifiers.
- `verify` runs on GitHub-hosted macOS and Ubuntu runners; the final `verify` job passes only when both do.
- Work order: earliest milestone, then the lowest-numbered issue whose dependencies are closed.
