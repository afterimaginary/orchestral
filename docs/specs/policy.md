# Policy

Each project has one policy file, `.orchestral/policy.md`, tracked in its repository. Its YAML frontmatter holds settings the binary reads. The prose after it holds rules the orchestrator session reads, in the project owner's words.

```markdown
---
workers: 3
plan_limit_percent: 85
fleet_token_budget: 3000000
preview:
  setup: pnpm install
  run: pnpm dev
---
Propose a design first for anything a user sees or interacts with.
Always ask me before changing auth, payments or migrations.
```

## Settings

| Key | Default | Meaning |
| --- | --- | --- |
| `fleet_token_budget` | none, required | Tokens the whole fleet may use in a run. A whole number above 0. |
| `workers` | `3` | Most workers and reviewers the fleet may have at once. Every recorded worker counts, stopped or not, until it's cleaned up, since a stopped one can be resumed. |
| `plan_limit_percent` | `85` | Plan usage, in percent, at or past which no new session starts. Any window counts. |
| `default_model` | `claude-opus-5-5` | Model for a session that doesn't name one. |
| `default_effort` | `high` | Effort for a session that doesn't name one. |
| `merge_method` | `squash` | `squash`, `merge` or `rebase`. |
| `supervisor_merges` | `true`, or `false` when the repository has a `CONTRIBUTING.md` | Whether the supervisor may merge PRs routed to it. An explicit value always wins. |
| `design_variants` | `4` | How many variants a design proposal shows. |
| `nag_minutes` | `[15, 30, 60, 120, 240]` | Minutes after a question starts waiting at which the owner is reminded. |
| `preview` | none | How PR previews run. Without it, there are no previews. |
| `preview.setup` | none | Command run once in a fresh worktree before the preview starts. |
| `preview.run` | none | Command that serves the preview. |
| `preview.url` | none | Used instead of `run` when the host gives each PR a URL. `{pr}` and `{branch}` are filled in. |
| `preview.max_running` | `3` | Most previews running at once. |
| `review_command` | none | What a reviewer session runs. None uses the built-in reviewer prompt. |

## Rejected files

Every command that reads the policy fails with exit `1` when the file:

- doesn't start with frontmatter between `---` lines,
- has a key not listed above, at the top level or under `preview`,
- has no `fleet_token_budget`, or one that isn't a whole number above 0,
- has a value of the wrong type, or a `merge_method` not listed,
- has a `preview` with both `run` and `url`, or with neither.

## Which copy is read

The policy is read from `origin/main` with `git show`, so a worker can't change its own limits from a branch or a local edit. Until `origin/main` exists, before the first push, the main checkout's copy is read instead. Once `origin/main` exists, a policy missing from it is an error.

Whether the repository has a `CONTRIBUTING.md` is checked in the same place.

`origin/main` here is the local remote-tracking ref, so the policy is as current as the last fetch. The ref is shared by every worktree, so this stops a worker changing its limits by accident, not a worker set on it: one could still move or delete the ref.

A file may start with a byte order mark and use Windows line endings.
