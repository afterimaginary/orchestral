# Policy

The policy belongs to the person running Orchestral, not the project, so it lives in Orchestral's config directory rather than the repository (ADR-0013). Each policy file's YAML frontmatter holds settings the binary reads. The prose after it holds rules the orchestrator session reads, in the owner's words.

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

## Files

The config directory is `$XDG_CONFIG_HOME/orchestral`, or `~/.config/orchestral` when that's unset. It holds up to two files for a repository:

| File | Applies to |
| --- | --- |
| `policy.md` | Every repository. |
| `<owner>/<repo>/policy.md` | The GitHub repository `origin` points at, lowercased, read from `origin`'s URL as configured (HTTPS, SSH or `git@github.com:` form). A repository whose `origin` isn't on GitHub has no file of its own. |

Either file alone is enough. With both, the repository's settings override the global ones key by key, with `preview` replaced as a whole, and both files' rules apply, the global ones first. `orchestral policy` prints the result and the files it came from.

The repository's own files are read only for `CONTRIBUTING.md`, below. A `.orchestral/policy.md` tracked in the repository isn't read: `orchestral init` moves it to the repository's file, and until then every command that reads the policy fails and says so.

## Settings

| Key | Default | Meaning |
| --- | --- | --- |
| `fleet_token_budget` | none, required | Tokens the whole fleet may use in a run. A whole number above 0. |
| `workers` | `3` | Most workers the fleet may have at once, reviewers started with `--role reviewer` included. Every recorded worker counts, stopped or not, until it's cleaned up, since a stopped one can be resumed. |
| `plan_limit_percent` | `85` | Plan usage, in percent, at or past which no new session starts. Any window counts. |
| `default_model` | `claude-opus-5-5` | Model for a session that doesn't name one. |
| `default_effort` | `high` | Effort for a session that doesn't name one. |
| `merge_method` | `squash` | `squash`, `merge` or `rebase`. |
| `supervisor_merges` | `true`, or `false` when the repository has a `CONTRIBUTING.md` | Whether the supervisor may merge PRs routed to it. An explicit value always wins. |
| `design_variants` | `4` | How many variants a design proposal shows. |
| `nag_minutes` | `[15, 30, 60, 120, 240]` | Gaps, in minutes, between reminders to the owner about a worker waiting on them: the first after it starts waiting, each later one after the reminder before. The last gap repeats. An empty list sends none. |
| `preview` | none | How PR previews run. Without it, there are no previews. |
| `preview.setup` | none | Command run once in a fresh worktree before the preview starts. |
| `preview.run` | none | Command that serves the preview. |
| `preview.url` | none | Used instead of `run` when the host gives each PR a URL. `{pr}` and `{branch}` are filled in. |
| `preview.max_running` | `3` | Most previews running at once. |
| `review_command` | none | What a PR's review checks, inserted verbatim into the instructions a worker gives its review subagent (and into the reviewer prompt), such as `Run /refine on the PR.` None uses the built-in check against the issue's acceptance criteria and non-goals. |

## Rejected files

Every command that reads the policy fails with exit `1` when either file:

- doesn't start with frontmatter between `---` lines,
- has a key not listed above, at the top level or under `preview`,
- has a value of the wrong type, or a `merge_method` not listed,
- has a `preview` with both `run` and `url`, or with neither,

or when neither file has a `fleet_token_budget`, or the one that applies isn't a whole number above 0.

## Who can change it

The config directory is outside every worktree, so no branch, commit or merged PR can change a worker's limits. Each worker's settings also deny the Edit tool, which covers Write and shell redirects, under the config directory, so a worker can't change its limits by accident. This doesn't stop a worker set on it, which could still write there with a script.

`CONTRIBUTING.md` is read from `origin/main` with `git show`, so a branch can't change it, or from the main checkout until `origin/main` exists, before the first push. `origin/main` here is the local remote-tracking ref, as current as the last fetch.

A file may start with a byte order mark and use Windows line endings.
