# Orchestral

A small workflow layer for Claude Code, as a plugin plus a Rust binary. Give it a brief, and it plans the work, runs a fleet of worker sessions, respects your budget, and brings you in for the designs and decisions that are yours.

**Status:** early. Every part runs end to end on a test repository, from planning through review to merge. Long unattended runs, such as a fleet idling overnight, are still being tested, so stay within reach on a first run. Progress is in [`STATE.md`](STATE.md).

## How it works

You talk to one Claude Code session, the **orchestrator**. It turns a brief into milestones and GitHub issues, starts a **worker** session for each issue it can work on, and passes messages between you and the workers. Each worker works in its own git worktree (with the ignored files your `.worktreeinclude` lists, such as `.env`), opens a PR, has it reviewed by a fresh subagent, and marks it ready. A background **supervisor** process watches the workers and GitHub, wakes the orchestrator when something happens, and merges PRs once they've been approved and every check passes.

Claude decides and the binary enforces: the `orchestral` binary refuses anything that breaks your policy, such as going past the worker cap, the plan's usage limit or the fleet's token budget, and workers can't merge, approve or push to `main`.

## Requirements

- macOS or Linux, with Claude Code logged in (`claude auth status`). Orchestral is tested on Claude Code 2.1.314, and `doctor` warns on other versions.
- The GitHub CLI, logged in (`gh auth status`), and a project on GitHub
- Rust 1.89 or later, to build the binary when the plugin is installed
- Optional, for PR previews you can open from other devices: `portless` and Tailscale

## Install

Add this repository as a plugin marketplace, then install the plugin in each project you want to run it in:

```bash
claude plugin marketplace add afterimaginary/orchestral
```

```bash
claude plugin install orchestral@afterimaginary --scope local
```

Run the install from a terminal inside the project. Local scope enables the plugin for you in that project only, in `.claude/settings.local.json`, which git ignores, so nothing Orchestral-specific is committed to the project. Claude Code Desktop reads the same settings, so its sessions in that project get the plugin too. Avoid user scope: the plugin's event monitor would run in every session you open, and a session running it always looks busy. The first session after an install or update builds the binary, which can take a minute.

To work on Orchestral itself, build a checkout and load it directly instead:

```bash
cargo build --release
claude --plugin-dir /path/to/orchestral
```

## Set up a project

1. Open Claude Code in the project once and accept the trust prompt. Workers can't start in an untrusted folder.
2. In that session, ask Claude to run the plugin's `orchestral init` by its full path, `~/.claude/plugins/cache/afterimaginary/orchestral/<version>/bin/orchestral`, where `<version>` is the newest folder there. Use the full path because another `orchestral` on `PATH` could be found first, and `init` writes the path of the binary it ran into the hook. It checks `claude`, `gh`, `git` and trust, creates a policy in your config directory, and installs a pre-push hook that stops workers pushing to `main`.
3. Edit the policy file `init` names. Set `fleet_token_budget`, the tokens the whole fleet may use in a run. Below the settings, write your rules in plain words, such as "Ask me before changing auth or billing."

The policy is yours, not the project's, so it lives outside the repository, in `~/.config/orchestral` (or `$XDG_CONFIG_HOME/orchestral`), and workers can't change it from a branch. `policy.md` there applies to every repository, and `<owner>/<repo>/policy.md` to one GitHub repository, overriding the global settings one by one and adding its rules after the global ones. `orchestral policy` shows the result. A project that tracks an older `.orchestral/policy.md` gets it moved there by `init`; then remove it from the repository.

A policy for a first run:

```markdown
---
workers: 2
fleet_token_budget: 3000000
supervisor_merges: false
---
Propose a design first for anything a user sees.
Ask me before changing auth, payments or data migrations.
```

Every setting, and its default, is in [`docs/specs/policy.md`](docs/specs/policy.md).

## Run it

In a Claude Code session in the project, start the orchestrator:

- **From a brief:** `/orchestral:orchestrate plan from docs/brief.md`. It drafts milestones and issues and publishes the plan for you to review. Nothing is filed until you approve it in the session.
- **From open issues:** `/orchestral:orchestrate`. It works through the open issues, lowest milestone first. Issues with clear acceptance criteria and non-goals get the best results, since each PR is reviewed against them.

The orchestrator runs `doctor` first and stops if anything fails, telling you what to fix. Then it claims the project (one orchestrator per repository), starts the supervisor, and starts workers up to the policy's `workers` limit. Keep that session open: it's how the fleet reaches you.

## Your part

The orchestrator asks you only for what's yours to decide, in the orchestrator session. A question, a design proposal or a PR waiting for you to merge also gets the `needs-discussion` label, and while it waits Orchestral sends reminders as push notifications to the Claude app, through Remote Control, on the schedule set by `nag_minutes`. Plans, `untriaged` issues and `blocked` issues get no reminders: they wait until you act.

| When | What you do |
| --- | --- |
| A plan is ready | Review the published plan, and approve it or say what to change, in the session. |
| A design proposal is ready | Open the link on the issue, pick a variant or ask for changes, by replying in the session or on the issue. |
| A worker has a question | Answer on the issue, or in the session. |
| A PR is ready for you to merge | Check it, using the preview link if the policy has a `preview`, then comment `/approve` on the PR. The supervisor merges it once every check passes. |
| You want a PR to wait | Comment `/hold`, and `/unhold` to lift it. |
| A worker found other work | It's filed as an `untriaged` issue. Remove the label to approve it, or close the issue to decline. |
| An issue is `blocked` | Its PR failed review twice. Decide what happens next: fix it yourself, rewrite the issue, or close it. |

Each command goes alone on the first line of a PR comment. An `/approve` covers the commit you saw: a new commit needs a new `/approve`. Deploys, deploy setup, paid services and new accounts always come to you, even when an issue seems to allow them.

The supervisor merges a PR without your `/approve` only when the orchestrator routes it there, which needs `supervisor_merges: true` in the policy and a change you wouldn't need to see, such as an internal refactor. Changes to `.github/` always come to you. When the repository has a `CONTRIBUTING.md`, `supervisor_merges` defaults to off.

## Keep an eye on it

Ask the orchestrator in plain words, and it runs these by the plugin's full path. To run one yourself, use the full path from [Set up a project](#set-up-a-project).

| Command | Does |
| --- | --- |
| `orchestral status` | Workers and their PRs, review verdicts, plan usage, budget used and whether the fleet is paused. |
| `orchestral pause` / `unpause` | Stop starting new workers and merging PRs, and start again. Running workers finish what they're doing, and approved PRs merge after `unpause`. |
| `orchestral stop <worker>` / `resume <worker>` | Stop a worker and keep its work, then pick it up again later. |
| `orchestral previews` | The running PR previews and their links. |

The fleet stops starting work at the plan's usage limit (`plan_limit_percent`) and resumes after the window resets. At the token budget it stops and asks you how to continue. The run ends when every approved issue is closed: the orchestrator publishes a summary of what merged and what's left. The supervisor exits by itself once nothing is left to watch.

Every command, with its output, is in [`docs/specs/cli.md`](docs/specs/cli.md).

## Development

Requires a Rust toolchain. To run the checks before every push:

```bash
git config core.hooksPath .githooks
```

| Command       | Does                                                                                       |
| ------------- | ------------------------------------------------------------------------------------------ |
| `make ready`  | Format check, lints, unit tests and CLI tests.                                             |
| `make verify` | `ready`, integration tests, a release build and plugin validation. CI runs it on every PR. |
| `make e2e`     | Real runs against the installed, logged-in `claude`. Never part of `verify` or CI.          |
| `make sandbox` | A real worker on an issue in the sandbox repository. Run before each milestone closes.     |

Integration tests run offline: `tests/common/mod.rs` puts stub `claude`, `gh`, `portless` and `tailscale` scripts on `PATH`, which record their calls and reply with canned output. Output captured from Claude Code is kept under `tests/fixtures/claude/<version>/`.

Stubs can't catch a change in Claude Code itself, so `tests/e2e.rs` holds real runs, ignored by `cargo test`:

- **`make e2e`** runs `compatibility`, which rereads a fresh print session's transcript, the plan limits and `claude agents` with the parsers, and `local_runner`, which rebuilds a repository with a local bare remote at `$TMPDIR/orchestral-e2e/repo` and runs `init`, `up`, `spawn`, `status`, `stop` and `resume` with a real worker on Sonnet. Claude Code only starts background sessions in a trusted directory, so run `claude` in that path once and accept the trust prompt; the runner refuses until then, and never changes Claude Code's settings itself.
- **`make sandbox`** uses a clone of a throwaway GitHub repository you own, at `$ORCHESTRAL_SANDBOX` (default `~/src/orchestral-sandbox`). It refuses with local changes or live workers, files an issue, spawns a worker that opens a PR, checks `status` shows it, then closes the PR and issue and removes the worker. It grows as the fleet gains steps such as review and merge.

Both clear the caller's Claude Code session variables, so they run as if from a plain terminal.

## Documentation

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md): how the parts fit together
- [`docs/decisions/`](docs/decisions/): decision records
- [`docs/specs/`](docs/specs/): expected behaviour
- [`CONTRIBUTING.md`](CONTRIBUTING.md): how to contribute

## License

[MIT](LICENSE)
