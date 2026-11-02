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

### `orchestral up [--keepalive-hours <h>] [--json]`

Starts the supervisor in the background, in its own process group with its output going to `.git/orchestral/logs/supervisor.log`, and waits for it to record its pid in `.git/orchestral/supervisor.pid`. When a supervisor is already running, it reports that one and exits `0`.

```text
Started the supervisor (pid 64028).
The supervisor is already running (pid 64028).
```

```json
{"pid":64028,"started":true}
```

The supervisor:

- claims `supervisor.pid` under the state lock, so only one runs, and exits if another holds it or later takes it over,
- starts a new run when no worker is open, setting `run_started_at` and recording a `run` event. With workers open, it continues the run in progress, such as after a crash, and keeps its start,
- on macOS, holds a sleep assertion with `caffeinate -i -w <pid>` for its lifetime. Elsewhere, or without `caffeinate`, it logs that the machine may sleep,
- runs on three ticks, each working from the files on disk, so a restart carries on where it stopped:
  - **every 2 seconds**, it reads the state and the session records. For each launched worker, it observes whether the session is busy (`busy` or `shell`), idle, waiting, or exited (no live record has the worker's session name). When that differs from the worker's last observation in the state, a change to idle, waiting or exited records a `worker` event that names the worker and what to do, such as `issue-12 is idle: check its PR, or send it its next step` or `issue-12 is waiting for input needed: see what it needs`. A blocked session shows as waiting, with what it waits for. Events are recorded before the observation is saved, so a crash between them repeats an event rather than losing it,
  - **every minute**, it polls GitHub and logs the fleet's usage this run. The poll reads open issues and PRs with `gh issue list` and `gh pr list`, and comments with `gh api`: conversation comments on issues and PRs, inline review comments, and reviews. It compares them with the last snapshot in `github.json`, and records one event per change, naming the issue or PR, its title, its worker (the worker whose branch or issue it is), and the action needed:
    - an issue opened or closed,
    - a PR opened, out of draft, CI failed or passed (CI is the PR's status check rollup), merged, or closed without merging,
    - a new comment, inline comment or review, quoting its first line. Comments starting with `<!-- orchestral -->` are Orchestral's own, and are ignored, so workers' comments don't come back as events.

    Examples: `pr #40 "Add login" (worker issue-12): CI failed: have issue-12 fix it`, `issue #13 "Fix typo": comment from someone: "Still happening on main": answer it if it needs you`. The first successful poll only takes the snapshot. Comments are read from 5 minutes before the last successful poll, and comment ids already seen aren't reported again. A comment created before that window and edited since isn't new. A CI result is reported when it changes, or when a new push gives the same result again. Each `gh` call has 60 seconds. A poll fails when any call fails, including reading whether a PR that left the open list merged, and it saves nothing, so the next poll reads it all again. A failed poll waits longer before the next, doubling from 2 minutes up to 16, and the fifth failure in a row records one `watcher: 5 polls failed: <error>` event. A success resets both.
  - also **every minute**, after the poll, it records a `heartbeat` event when a live session holds the claim and has had nothing for 55 minutes: no event recorded, no claim, and no request of its own, such as a conversation with the owner. A claim held by a session that has ended gets no heartbeats, so its successor doesn't receive a pile of them. Every event wakes the orchestrator, so the heartbeat only fills a 55-minute gap, which keeps its 1-hour prompt cache warm. It never needs action: `heartbeat: 2 workers open, your context 41%, budget about 1.2M of 3.0M; no action needed`. The context is the orchestrator's own, from its transcript, and the budget is this run's estimate against the policy's `fleet_token_budget`; either is left out when it can't be read. Heartbeats stop when the supervisor exits, after `--keepalive-hours` with no worker open,
  - **every 5 minutes**, it reads and logs the plan windows.
  - The minute and 5-minute ticks run on threads of their own, so a slow `claude` call never delays the 2-second tick. Each is skipped while its last run is still going,
- every 2 seconds also checks whether it can exit, and exits once no worker is open, no open issue that gained the `needs-discussion` label during the run still has it, as the GitHub snapshot records (a label already there at the first poll doesn't count), and `--keepalive-hours` (default 8, from 0 to 8784, fractions allowed) have passed since either was, by the wall clock, so time asleep counts. It checks again under the lock before removing its pid file, so a worker spawned at that moment keeps it running. A state file it can't read counts as work open.

### `orchestral status [--json]`

One view of the fleet: whether it's paused, the run's budget, the plan windows from `orchestral limits`, and each worker. A part that can't be read, such as `claude agents` or `gh pr list`, is left empty and named in a warning, and the command still exits `0`.

For each worker it shows:

- its issue, and the open PR whose head is the worker's branch, marked `(draft)` while it is one,
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
| `paused` | Whether the fleet is paused (`orchestral pause`). |
| `workers[]` | `name`, `issue`, `pr` (or null), `session_name`, `session`, `activity` (`working`, `waiting`, `done`, `failed` or `unknown`), `waiting_for` (`waitingFor`, or `blocked` when the session's `state` is `blocked`, or null), `context` (or null) and `wake_cost` (`{"estimated_tokens": n}`, or null). |
| `workers[].context` | `tokens`, `model`, `identity`, `window`, `percent`, `last_request_at`, `compactions` (each `at` and `post_tokens`), `cache_ttl` in seconds and `warm`, as defined in [Claude Code internals](claude-code-internals.md). |
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

The prompt names the worker and its issue, tells it to find its orchestrator's SendMessage address with `<this binary's path> resolve orchestrator` (and that messages from that address are its orchestrator's), and sets the rules every worker follows, such as never merging and starting every GitHub comment with `<!-- orchestral -->`.

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

Stops a worker's session and keeps its worktree, branch and conversation. It finds the session in `claude agents` by the worker's job id, and refuses, exiting `2`, unless it's a background session, so an interactive session is never stopped. Then it records the worker as observed exited, so the supervisor doesn't report the stop as a session that exited, runs `claude stop <job id>`, and waits up to 15 seconds for the process to exit. A worker that isn't running is reported as already stopped.

```text
Stopped issue-12. Its worktree and conversation are kept.
```

### `orchestral resume <name> [--json]`

Resumes a stopped worker with a bare `claude --bg --resume <session id>`, run in its worktree. Any other flag would start a copy under a new id, and the bare form keeps every option the worker was started with. It refuses, exiting `2`, while the worker is still running, and, like `stop`, for anything but a background session. If Claude Code starts a copy anyway, `resume` stops the copy and reports an error. A resumed worker is idle until it gets a message.

```text
Resumed issue-12 (job 558b988a). It's idle until it gets a message.
```

### `orchestral cleanup <name> [--force] [--json]`

Removes a finished worker: its session, worktree and local branch, and its entry in the state. A worker is finished when its PR merged or closed, as `gh pr list --head <branch> --state all` shows. PRs from forks are ignored, and a merged PR is preferred over a closed one. Git isn't asked, since a squash merge leaves the branch's own commits off `main`. A worker with no PR, such as a reviewer, can be removed when its branch has no commits beyond `origin/main`, where it started.

It refuses, exiting `2`, while the worker's PR is open, before touching anything. Otherwise it stops the session as `stop` does, so nothing lands after the check, and refuses when the worktree has uncommitted changes, when the branch has commits its PR's head doesn't contain (fetching `pull/<n>/head` when that head isn't local), or when it has commits and no PR. A refusal leaves the worker stopped, with its worktree and branch. `--force` skips these checks and discards that work. Like `stop`, it refuses anything but a background session.

Then it runs `git worktree remove`, after `git worktree prune` so a deleted worktree directory doesn't hold the branch, deletes the local branch, then runs `claude rm <job id>`, retrying for 15 seconds while the stopped session still holds its lock. It never runs `git clean`. A session `claude agents` no longer lists is skipped. It records a `<name> cleaned up` event.

```text
Removed issue-12 and its session, worktree, branch.
```

### `orchestral claim [--json]`

Makes the calling Claude Code session the repository's orchestrator, so two orchestrators never give workers conflicting instructions. The session is identified by `CLAUDE_CODE_SESSION_ID` (ADR-0001), and without it, claim is an error.

It refuses, exiting `2`, while another live session holds the claim. A holder whose session has ended loses the claim to the next caller, and claiming again from the holder succeeds.

```text
Session 00000000-0000-4000-8000-0000000000c1 is the orchestrator.
```

### `orchestral watch`

The event stream: the one way events reach the orchestrator. The plugin's monitor, `orchestral-events` in `monitors/monitors.json`, runs it in every session that loads the plugin. It has no `when` key, because a monitor started that way didn't start when its skill ran as a slash command (ADR-0001). Each line it prints reaches the session as a notification that starts a turn, even when the session is idle.

Every second it:

- finds the repository from its working directory, and stays silent outside one,
- reads the state, and stays silent unless the claim's session is its own `CLAUDE_CODE_SESSION_ID`, so it starts printing within 2 seconds of its session claiming,
- prints the `text` of each complete event in `events.jsonl` with a `seq` after `watch-cursor`, one line each, with any line breaks in the text replaced by spaces. Then it saves the last `seq` printed as the cursor, so a crash in between repeats an event rather than losing it.

A session that takes over the claim carries on from the cursor, so no event is lost. The cursor never moves back, so while two sessions' `watch` overlap during a handoff, an event can be printed twice but never skipped. A cursor at or past the state's `next_seq` was left by a state that has since been reset, and is ignored. An unreadable state or events file counts as nothing to print. It never exits by itself, only when its session ends: its stdout closes, or the session exits and leaves it orphaned.

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
2. Installs the pre-push hook in the hooks directory git uses, which is `core.hooksPath` when set. Orchestral's lines sit between `# >>> orchestral >>>` and `# <<< orchestral <<<`, right after the `#!` line. Running init again replaces that block. An existing hook keeps its own lines, and still gets the refs git passes on stdin. An existing hook that isn't a shell script is refused. If the binary later moves, as a plugin update does, the block warns on each push and, when `ORCHESTRAL_WORKER` is set, still rejects a push to `refs/heads/main` itself, until init runs again. `doctor` warns when the hook is missing or calls a binary that has moved. When git tracks the hook file, as with a `core.hooksPath` inside the repository, init says not to commit the block, since it names this machine's binary.

```text
Created /home/dev/app/.orchestral/policy.md. Set fleet_token_budget, then commit it and push it to main.
Installed the pre-push hook in /home/dev/app/.git/hooks/pre-push.
```

### `orchestral doctor [--json]`

Checks what a run needs and prints one line per check, as `ok`, `warn` or `FAIL`. It exits `1` when a check fails.

| Check | Fails or warns when |
| --- | --- |
| `repo` | Fails outside a git repository, in place of `trust`, `policy` and `hook`. |
| `claude` | Fails when it's missing. Warns when its version isn't the one Orchestral was tested on, 2.1.314. |
| `auth` | Fails when `claude auth status` doesn't report `loggedIn`. |
| `gh` | Fails when it's missing or `gh auth status` fails. |
| `git` | Fails when it's missing. |
| `trust` | Fails when Claude Code doesn't trust the main checkout. |
| `policy` | Fails when the policy can't be read. Warns when the main checkout's copy differs from `origin/main`'s, since that one is used. |
| `hook` | Warns when the pre-push hook isn't installed, or calls a binary that has moved; `orchestral init` fixes both. |
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
