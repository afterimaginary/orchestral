# CLI

The `orchestral` binary is used by people, by scripts and by the orchestrator session. Every command reports its result the same way, so any caller can tell success, an error and a refusal apart without reading the text.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success. Also used for `--help` and `--version`. |
| `1` | An error: something went wrong, or the command line was invalid. |
| `2` | A refusal: the command was valid, but a rule stopped it. |

A usage error exits `1`, not `2`, so `2` always means a rule.

## Output

- Data goes to stdout. Errors and refusals go to stderr, and print nothing on stdout.
- A command that prints data takes `--json`, which prints the same data as a single line of JSON. Without it, the command prints text for people.
- An error prints `error: <message>`. An invalid command line prints the argument parser's own message and usage instead.
- A refusal prints two lines. The first gives the reason, and the second says what would make the command succeed:

  ```text
  refused: the fleet is paused
  fix: run `orchestral unpause`
  ```

## Commands

### `orchestral limits [--json]`

Shows the plan's usage windows, as percent used and reset time. When the limits can't be read, it says they're unavailable, with the reason, and still exits `0`, since unavailable limits are a warning rather than a failure.

Reading the limits isn't built yet, so today the command always reports them unavailable:

```text
Plan limits are unavailable: reading them isn't implemented yet.
```

```json
{"available":false,"reason":"reading them isn't implemented yet"}
```

### `orchestral spawn <name> --issue <n> --prompt <text> [--model <m>] [--effort <e>] [--json]`

Starts a worker on an issue as a background Claude Code session. `<name>` uses lowercase letters, digits and dashes, and is also the branch name. The session is named `<repo slug>-<name>`, where the slug is the main checkout's directory name.

It refuses, exiting `2`, when:

- the session name is longer than 32 characters,
- a worker with that name is already in the state, or
- a live session already has that session name.

Then it:

1. Fetches `origin main` and adds a worktree at `.claude/worktrees/<name>` on a new branch `<name>`, from `origin/main`, and adds `/.claude/worktrees/` to `.git/info/exclude`.
2. Writes the worker's `settings.json` and `prompt.md` to `.git/orchestral/workers/<name>/`.
3. Records the worker in the state, so a crash during launch leaves a visible entry.
4. Runs `claude --bg` in the worktree with Remote Control, those settings and that prompt, auto permission mode, and the model and effort. `--model` and `--effort` override the policy's `default_model` and `default_effort`.
5. Reads the job id from the `backgrounded · <id> · <session name>` line and saves it with the worker.

A launch that fails removes the worker, its worktree and its branch, so the same name can be tried again. When Claude Code doesn't trust the repository, spawn refuses and says to run `claude` once in the main checkout and accept the trust prompt. Claude Code trusts a worktree when it trusts its main checkout. Any other failure is an error.

The worker's settings apply to its process only:

| Setting | Value |
| --- | --- |
| `crossSessionInbound` | `accept`, so the orchestrator can message it |
| `isolatePeerMachines` | `true` |
| `worktree.bgIsolation` | `none`, since it already has a worktree |
| `env` | `ORCHESTRAL_WORKER`, `ORCHESTRAL_REPO` (the main checkout), and the caller's `PATH` |
| `hooks` | A `PreToolUse` hook on `Bash` running `<absolute path to orchestral> hook worker-guard` |
| `enabledPlugins` | `false` for every installed `orchestral@<marketplace>` ID (ADR-0001) |

The prompt names the worker, its issue and its orchestrator (the caller's `CLAUDE_CODE_SESSION_ID`), and sets the rules every worker follows, such as never merging and starting every GitHub comment with `<!-- orchestral -->`.

```text
Started issue-12 as app-issue-12 (job 558b988a) in /home/dev/app/.claude/worktrees/issue-12
```

```json
{"name":"issue-12","session_name":"app-issue-12","job_id":"558b988a","worktree":"/home/dev/app/.claude/worktrees/issue-12"}
```

`orchestral hook worker-guard` is the worker's Bash hook. It allows every command for now.
