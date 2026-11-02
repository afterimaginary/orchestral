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

It reads them by running `claude -p /usage --no-session-persistence --settings '{"disableAllHooks":true}'` from a new temporary directory, without the calling session's `CLAUDECODE` or `CLAUDE_CODE_SESSION_ID` (ADR-0008). Each `Current …` line of that output is one window, and the rest is ignored. A window's reset time is read in the time zone its line gives, in the current year unless that puts it more than 180 days in the past, when it's the next year. A line without a reset time gives a window with none.

```text
session: 2% used, resets Nov 2 at 6:49am PST
week (all models): 26% used, resets Nov 4 at 12:59pm PST
week (Fable): 0% used, resets Nov 4 at 12:59pm PST
```

```json
{"available":true,"windows":[{"name":"session","percent":2,"resets":"2026-11-02T06:49:00-08:00[America/Los_Angeles]"}]}
```

The limits are unavailable when `claude` can't run, exits non-zero (the reason then includes its first error line), or hasn't finished within 60 seconds, when it's stopped, when the output has no `Current` lines, or when any `Current` line doesn't parse, so a changed format is never guessed at:

```text
Plan limits are unavailable: claude -p /usage printed no usage windows.
```

```json
{"available":false,"reason":"claude -p /usage printed no usage windows"}
```

### `orchestral status [--json]`

One view of the fleet: whether it's paused, the run's budget, the plan windows from `orchestral limits`, and each worker. A part that can't be read, such as `claude agents` or `gh pr list`, is left empty and named in a warning, and the command still exits `0`.

For each worker it shows:

- its issue, and the open PR whose head is the worker's branch,
- its session: `running` when `claude agents --json --all` lists it with a pid, `stopped` when it's listed without one, or `unknown`,
- what the session is doing, from `waitingFor` first, then `state`,
- its context size and cache state, from its transcript (see [Claude Code internals](claude-code-internals.md)), and
- the cost to wake it: the context sent with the next message, weighted as in [Budget](budget.md). A warm cache is read at 0.1. A cold one is written again at 2 when the session last used a 1-hour cache, and at 1.25 otherwise. It's an estimate.

```text
Fleet: not paused
Budget: about 120000 tokens (estimate) of 3000000 used this run
Plan limits:
  session: 9% used, resets Nov 2 at 6:50am PST
  week (all models): 27% used, resets Nov 4 at 1:00pm PST
Workers: 2 of 3
  issue-12 · issue #12 · PR #40 · running, waiting: input needed · context 33954 tokens (17%) · cache cold · wake about 67908 tokens (estimate)
  issue-13 · issue #13 · unknown
warning: PRs are unknown: gh pr list failed: …
```

Before a run has started, the budget line reads `Budget: no run has started; <n> tokens per run`.

`--json` carries the same data, and is the shape later interfaces read. Fields are only ever added:

| Field | Meaning |
| --- | --- |
| `paused` | Whether spawns are refused. |
| `workers[]` | `name`, `issue`, `pr` (or null), `session_name`, `session`, `activity` (`working`, `waiting`, `done`, `failed` or `unknown`), `waiting_for` (or null), `context` (the transcript reading, or null) and `wake_cost` (`{"estimated_tokens": n}`, or null). |
| `worker_cap` | The policy's `workers`. |
| `limits` | The output of `orchestral limits --json`. |
| `budget` | `used` (`{"estimated_tokens": n}`, or null when it can't be read), `fleet_token_budget` and `run_started_at`. |
| `warnings` | Each part that couldn't be read, and why. |

### `orchestral spawn <name> --issue <n> --prompt <text> [--model <m>] [--effort <e>] [--json]`

Starts a worker on an issue as a background Claude Code session. `<name>` uses lowercase letters, digits and dashes, and is also the branch name. The session is named `<repo slug>-<name>`, where the slug is the main checkout's directory name.

It refuses, exiting `2`, when:

- the session name is longer than 32 characters,
- a worker with that name is already in the state,
- a live session already has that session name,
- the fleet is paused (see `orchestral pause`),
- the state already has `workers` workers, the policy's cap. Every recorded worker counts, stopped or not, until it's cleaned up,
- the run's fleet usage (see [Budget](budget.md)) has reached `fleet_token_budget`. Before a run has started, nothing counts, and until the supervisor (#25) starts runs, no run has. The fleet is every worker whose session `claude agents --json --all` still lists, and the orchestrator, or
- any plan window from `orchestral limits` is at or past `plan_limit_percent`. When several are, the refusal names the one that resets last, and the fix gives its reset time.

When the plan limits can't be read, spawn warns on stderr and carries on, so unavailable limits never stop work:

```text
warning: plan limits are unavailable, so they weren't checked: claude -p /usage exited with exit status: 1
```

The pause and the cap are checked again under the state lock as the worker is recorded, so two spawns at once can't both take the last place.

Then it:

1. Fetches `origin main` and adds a worktree at `.claude/worktrees/<name>` on a new branch `<name>` from `origin/main`, not tracking it, and adds `/.claude/worktrees/` to `.git/info/exclude`.
2. Writes the worker's `settings.json` and `prompt.md` to `.git/orchestral/workers/<name>/`.
3. Records the worker in the state, so a crash during launch leaves a visible entry.
4. Runs `claude --bg` in the worktree with Remote Control, those settings and that prompt, auto permission mode, and the model and effort. `--model` and `--effort` override the policy's `default_model` and `default_effort`.
5. Reads the job id from the `backgrounded · <id> · <session name>` line and saves it with the worker.

A failure after the worktree is added removes the worker, its worktree and its branch, so the same name can be tried again. The exception is `claude --bg` exiting `0` without printing a job id: a session may be running, so everything is kept and it's an error. When Claude Code doesn't trust the repository, spawn refuses and says to run `claude` once in the main checkout and accept the trust prompt. Claude Code trusts a worktree when it trusts its main checkout. Any other failure is an error.

The worker's settings apply to its process only:

| Setting | Value |
| --- | --- |
| `crossSessionInbound` | `accept`, so the orchestrator can message it |
| `isolatePeerMachines` | `true` |
| `worktree.bgIsolation` | `none`, since it already has a worktree |
| `env` | `ORCHESTRAL_WORKER`, `ORCHESTRAL_REPO` (the main checkout), and the caller's `PATH` |
| `hooks` | A `PreToolUse` hook on `Bash` running `'<absolute path to orchestral>' hook worker-guard` |
| `enabledPlugins` | `false` for every installed `orchestral@<marketplace>` ID (ADR-0001) |

The prompt names the worker, its issue and its orchestrator (the caller's `CLAUDE_CODE_SESSION_ID`), and sets the rules every worker follows, such as never merging and starting every GitHub comment with `<!-- orchestral -->`.

```text
Started issue-12 as app-issue-12 (job 558b988a) in /home/dev/app/.claude/worktrees/issue-12
```

```json
{"name":"issue-12","session_name":"app-issue-12","job_id":"558b988a","worktree":"/home/dev/app/.claude/worktrees/issue-12"}
```

### `orchestral pause [--json]` and `orchestral unpause [--json]`

Set and clear the fleet's pause flag in the state. While paused, `spawn` refuses. Changing the flag records an event; setting it to what it already is changes nothing.

```text
The fleet is paused. Spawns are refused until `orchestral unpause`.
```

```json
{"paused":true}
```

### `orchestral hook worker-guard` and `orchestral hook pre-push`

Hooks that stop a worker by accident doing what only the owner or the supervisor should (ADR-0004). They read command text, so they aren't a security boundary: a session that deliberately works around them can.

`hook worker-guard` is the `PreToolUse` hook on `Bash` in every worker's settings. It reads the hook event on stdin and prints a deny decision, with a reason the worker sees, for:

- `gh pr merge`, and `gh api` on `…/pulls/<n>/merge`
- `gh pr review` with `--approve` or `-a`, and `gh api` with `event=APPROVE`
- `git push` with a refspec whose destination is `main`, or with `--all` or `--mirror`
- `/approve` or `/hold` as a word in a command that posts through `gh pr comment`, `gh issue comment`, `gh pr review` or `gh api`, or in a body file it names with `--body-file`, `-F`, or `-F body=@<file>` (ADR-0005). The whole command is checked, so a heredoc body counts.

Commands are split at `;`, `&`, `|`, parentheses and newlines, with quotes and line continuations removed. Flags before a subcommand, such as `gh -R owner/app pr merge` or `git -C app push`, are skipped. Anything else is allowed, with no output.

`hook pre-push` is a git pre-push hook. When `ORCHESTRAL_WORKER` is set, it rejects a push whose remote ref is `refs/heads/main`. Without it, as in the owner's own shell, every push goes through. Installing it into a project is separate.

### `orchestral stop <name> [--json]`

Stops a worker's session and keeps its worktree, branch and conversation. It finds the session in `claude agents` by the worker's job id, and refuses, exiting `2`, unless it's a background session, so an interactive session is never stopped. Then it runs `claude stop <job id>` and waits up to 15 seconds for the process to exit. A worker that isn't running is reported as already stopped.

```text
Stopped issue-12. Its worktree and conversation are kept.
```

### `orchestral resume <name> [--json]`

Resumes a stopped worker with a bare `claude --bg --resume <session id>`, run in its worktree. Any other flag would start a copy under a new id, and the bare form keeps every option the worker was started with. It refuses, exiting `2`, while the worker is still running, and, like `stop`, for anything but a background session. If Claude Code starts a copy anyway, `resume` stops the copy and reports an error. A resumed worker is idle until it gets a message.

```text
Resumed issue-12 (job 558b988a). It's idle until it gets a message.
```

### `orchestral claim [--json]`

Makes the calling Claude Code session the repository's orchestrator, so two orchestrators never give workers conflicting instructions. The session is identified by `CLAUDE_CODE_SESSION_ID` (ADR-0001), and without it, claim is an error.

It refuses, exiting `2`, while another live session holds the claim. A holder whose session has ended loses the claim to the next caller, and claiming again from the holder succeeds.

```text
Session 00000000-0000-4000-8000-0000000000c1 is the orchestrator.
```

### `orchestral resolve <name> [--json]`

Prints the SendMessage address of a worker, or of the claimed orchestrator when `<name>` is `orchestrator`. The address is `uds:<messaging socket>`, which is also the `from` on messages that session sends, so it identifies a sender exactly.

It refuses, exiting `2`, when no live session matches, when more than one does, when a worker's session runs outside its worktree, or when the session has no messaging socket. A worker's session is found by its session name, and the orchestrator's by its session id.

```text
uds:/tmp/cc-socks/41001.sock
```

```json
{"name":"issue-12","session_id":"00000000-0000-4000-8000-00000000a001","address":"uds:/tmp/cc-socks/41001.sock"}
```

### `orchestral init [--json]`

Sets a repository up. First it checks that `claude` and `gh` are installed and logged in, that `git` works, and that Claude Code trusts the main checkout, and refuses, exiting `2`, naming the first problem. Then it:

1. Creates `.orchestral/policy.md` in the main checkout from a template, unless one exists. The template's `fleet_token_budget` is an example to replace. The file takes effect once it's on `origin/main`.
2. Installs the pre-push hook in the hooks directory git uses, which is `core.hooksPath` when set. Orchestral's lines sit between `# >>> orchestral >>>` and `# <<< orchestral <<<`, right after the `#!` line. Running init again replaces that block. An existing hook keeps its own lines, and still gets the refs git passes on stdin. An existing hook that isn't a shell script is refused. If the binary later moves, the block warns on each push and lets it through, until init runs again. When git tracks the hook file, as with a `core.hooksPath` inside the repository, init says not to commit the block, since it names this machine's binary.

```text
Created /home/dev/app/.orchestral/policy.md. Set fleet_token_budget, then commit it and push it to main.
Installed the pre-push hook in /home/dev/app/.git/hooks/pre-push.
```

### `orchestral doctor [--json]`

Checks what a run needs and prints one line per check, as `ok`, `warn` or `FAIL`. It exits `1` when a check fails.

| Check | Fails or warns when |
| --- | --- |
| `repo` | Fails outside a git repository, in place of `trust` and `policy`. |
| `claude` | Fails when it's missing. Warns when its version isn't the one Orchestral was tested on, 2.1.314. |
| `auth` | Fails when `claude auth status` doesn't report `loggedIn`. |
| `gh` | Fails when it's missing or `gh auth status` fails. |
| `git` | Fails when it's missing. |
| `trust` | Fails when Claude Code doesn't trust the main checkout. |
| `policy` | Fails when the policy can't be read. Warns when the main checkout's copy differs from `origin/main`'s, since that one is used. |
| `portless` | Warns when it's missing, since previews can run without it. |
| `tailscale` | Warns when it's missing or not running, since previews can run without it. |

```text
ok   claude     2.1.314
ok   trust      Claude Code trusts /home/dev/app
warn tailscale  not found; preview links won't reach other devices
```

```json
{"checks":[{"name":"claude","status":"ok","detail":"2.1.314"}]}
```
