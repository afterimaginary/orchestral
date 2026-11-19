---
description: The policy lives in the operator's config directory, as a global file layered under a per-repository one, and the plugin is enabled at local scope, so Orchestral writes nothing into the repositories it manages.
status: accepted
---

# ADR-0013: The policy lives with the operator, not the project

## Context

Orchestral's policy has been a tracked `.orchestral/policy.md` in each managed repository, read from `origin/main` so a worker can't change its own limits from a branch. Setup also enabled the plugin at project scope, which commits `orchestral@<marketplace>` into the project's tracked `.claude/settings.json`.

Both are the operator's configuration, not the project's. A reader of the repository can't resolve the plugin, and the policy's limits and owner-voice rules read as noise in a project that has nothing to do with them. Most rules, such as when to ask the owner, are the same in every project, yet each repository had to repeat them.

## Decision

- The policy is read from the operator's config directory, `$XDG_CONFIG_HOME/orchestral`, or `~/.config/orchestral` when that's unset:
  - `policy.md` applies to every repository,
  - `<owner>/<repo>/policy.md` applies to the GitHub repository that `origin` points at.
- The two are layered. Per-repository settings override global ones key by key, with `preview` replaced as a whole. Both files' rules apply, global first. Either file alone is enough.
- Nothing is read from the managed repository except its `CONTRIBUTING.md` on `origin/main`, which still sets the `supervisor_merges` default. There's no in-repository policy option.
- `orchestral policy` prints the effective policy, and the orchestrator and workers read it through that command.
- The README installs the plugin with `--scope local`, which writes the project's untracked `.claude/settings.local.json`. User scope would run the plugin's monitor in every session, which makes sessions report as busy (ADR-0001).
- `init` moves a tracked `.orchestral/policy.md` to the per-repository file. Until the file it would move to exists, commands that read the policy fail and point at `init`, so a repository's rules are never silently dropped.

## Consequences

- A worker still can't raise its limits by accident: the config directory is outside every worktree, so no branch or merged PR can change it, and worker settings deny Edit and Write there. A shell command could still write it, as one could move `origin/main` before. Routing no longer needs to keep `.orchestral/` changes from the supervisor.
- The policy is no longer versioned or reviewed with the project, and collaborators on a repository don't share it. Orchestral is run by one operator, so that's the intended trade.
- A repository whose `origin` isn't on GitHub, or that's renamed or transferred, gets only the global layer until its directory is created or renamed to match. `doctor` lists which files apply.
- Moving to another machine means copying the config directory, which can live in the operator's dotfiles.
