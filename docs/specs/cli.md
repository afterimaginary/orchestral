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
- before its first tick, checks each worker in the state for whether it can be resumed: its worktree must exist, and `claude agents --json --all` must still list its session. A worker recorded more than 2 minutes ago that never launched can't be resumed either. A worker with no job id, from a launch whose output wasn't read, can be resumed if a session with its name is listed. Each worker that can't records one `worker` event, `issue-12 is lost: its worktree is gone, so it can't be resumed: clean it up with `orchestral cleanup issue-12`, and start its issue again, on its PR if it has one`. When `claude agents` can't be read, it records one `supervisor` event saying so instead, since a lost worker would then be reported as exited. The worker is then observed as `lost` until it's cleaned up, so neither this check nor the 2-second tick reports it again. A worker whose session simply ended, as after a reboot, isn't lost: the 2-second tick reports it as exited, to resume,
- on macOS, holds a sleep assertion with `caffeinate -i -w <pid>` for its lifetime. Elsewhere, or without `caffeinate`, it logs that the machine may sleep,
- runs on three ticks, each working from the files on disk, so a restart carries on where it stopped:
  - **every 2 seconds**, it reads the state and the session records. For each launched worker, it observes whether the session is busy (`busy` or `shell`), idle, waiting, or exited (no live record has the worker's session name). When that differs from the worker's last observation in the state, a change to idle, waiting or exited records a `worker` event that names the worker and what to do, such as `issue-12 is idle: check its PR, or send it its next step` or `issue-12 is waiting for input needed: see what it needs`. A blocked session shows as waiting, with what it waits for. Events are recorded before the observation is saved, so a crash between them repeats an event rather than losing it,
  - **every minute**, it polls GitHub, and saves and logs the fleet's usage this run. The first time in a run that its usage reaches `fleet_token_budget`, it records `budget: about 3000000 tokens (estimate) of 3000000 used this run: start nothing new, let workers finish their turn, and ask the owner how to continue`. Spawn already refuses past the budget, so the event only tells the orchestrator, once a run. The poll reads open issues and PRs with `gh issue list` and `gh pr list`, and comments with `gh api`: conversation comments on issues and PRs, inline review comments, and reviews. It compares them with the last snapshot in `github.json`, and records one event per change, naming the issue or PR, its title, its worker (the worker whose branch or issue it is), and the action needed:
    - an issue opened or closed, or approved, which is losing the `untriaged` label. An `untriaged` issue waits for the owner's approval, so its opening and closing need no action,
    - a PR opened, out of draft, CI failed or passed (CI is the PR's status check rollup), merged, or closed without merging,
    - a new comment, inline comment or review, quoting its first line. Comments starting with `<!-- orchestral -->` are Orchestral's own, and are ignored, so workers' comments don't come back as events.

    Examples: `pr #40 "Add login" (worker issue-12): CI failed: have issue-12 fix it`, `issue #13 "Fix typo": comment from someone: "Still happening on main": answer it if it needs you`. The first successful poll only takes the snapshot. Comments are read from 5 minutes before the last successful poll, and comment ids already seen aren't reported again. A comment created before that window and edited since isn't new. A CI result is reported when it changes, or when a new push gives the same result again. Each `gh` call has 60 seconds. A poll fails when any call fails, including reading whether a PR that left the open list merged, and it saves nothing, so the next poll reads it all again. A failed poll waits longer before the next, doubling from 2 minutes up to 16, and the fifth failure in a row records one `watcher: 5 polls failed: <error>` event. A success resets both.
  - also **every minute**, after the poll, it reminds the owner about each worker waiting on them: one whose issue gained `needs-discussion` during the run, as the GitHub snapshot records, or one whose session is observed `waiting`. A session blocked by a rate limit, whose job record's `needs` starts with `rate limited`, waits on the plan rather than the owner, and is left out. The first reminder goes out after the first of the policy's `nag_minutes`, and each later one after the next gap from the one before. Past the end of the list, the last gap repeats. A worker that stops waiting, because the label was removed or its session moved on, loses its count, so waiting again starts from the first gap. A reviewer shares its issue, so only the implementer is reminded about an escalation. Each reminder is a new one-shot session, `claude --bg --name <slug>-nag-<n> --remote-control <slug>-nag-<n> --settings '{"env":{"CLAUDE_CODE_DISABLE_NOTIFICATION_PRESENCE_CHECK":"1"}}' --permission-mode default --model claude-haiku-5-5 --effort low`, run from the main checkout. Remote Control is what delivers the banner, and the presence check is off because a new background session always counts its terminal as active, so with it on nothing is sent. The session is told to load `PushNotification` with `ToolSearch` and send exactly one notification, with status proactive (ADR-0009). The text comes after the instructions, marked as data to send rather than instructions to follow, since it can quote GitHub. The text names the worker, its session, the question and the issue's link: `issue-12 (app-issue-12) is waiting on you: Should sign-up need an email? https://github.com/o/app/issues/12`. A period is added only when the question doesn't already end in punctuation. For an escalation, the question is the line after `Question for the owner:` in the newest Orchestral comment on the issue posted by the account `gh` is authenticated as, which is the owner's. When that comment has no question, or there's no such comment, it's `a decision on "<title>"`. For a blocked session, it's the `detail` from the job record. A reminder session is stopped with `claude stop` once it's no longer `working`, or after 5 minutes. A launch that fails is logged and tried again the next minute. A launch that succeeds counts as sent even if `claude --bg` printed no job id. That session can't be tracked, so the log says it won't be stopped. A failure to stop sessions is logged and doesn't hold up the reminders due. Reminders are logged rather than recorded as events, since the orchestrator has nothing to do about them,
  - also **every minute**, after the poll, it records a `heartbeat` event when a live session holds the claim and has had nothing for 55 minutes: no event recorded, no claim, and no request of its own, such as a conversation with the owner. A claim held by a session that has ended gets no heartbeats, so its successor doesn't receive a pile of them. Every event wakes the orchestrator, so the heartbeat only fills a 55-minute gap, which keeps its 1-hour prompt cache warm. It never needs action: `heartbeat: 2 workers open, your context 41%, budget about 1.2M of 3.0M; no action needed`. The context is the orchestrator's own, from its transcript, and the budget is this run's estimate against the policy's `fleet_token_budget`; either is left out when it can't be read. Heartbeats stop when the supervisor exits, after `--keepalive-hours` with no worker open,
  - **every 5 minutes**, it reads and logs the plan windows. When a window reaches `plan_limit_percent`, the fleet winds down: it records `limits: the session window is 87% used, past plan_limit_percent (85%): start nothing new until it resets at Nov 2 at 6:00pm PST, and let workers finish their turn`, naming the window that resets last, as spawn's refusal does. Running workers aren't stopped. If another window passes the limit meanwhile, the fleet stays wound down for that one. Once no window is at the limit, and a read shows the window below it or its reset time has passed, it records `limits: the session window has reset: start ready work again`. Each change is one event, recorded before the state is saved, so a crash between them repeats the event rather than losing it.
  - **every 2 seconds**, on a thread of its own, it keeps the previews in line with what `orchestral try` asked for (see below), and **every minute**, after the poll, it stops the previews of PRs that merged or closed, as the poll's snapshot shows. A poll that failed stops nothing,
  - The minute and 5-minute ticks run on threads of their own, so a slow `claude` call never delays the 2-second tick. Each is skipped while its last run is still going,
- every 2 seconds also checks whether it can exit, and exits once no worker is open, no preview is wanted or running (a failed one doesn't count), no open issue that gained the `needs-discussion` or `blocked` label during the run still has it, as the GitHub snapshot records (a label already there at the first poll doesn't count), and `--keepalive-hours` (default 8, from 0 to 8784, fractions allowed) have passed since either was, by the wall clock, so time asleep counts. It checks again under the lock before removing its pid file, so a worker spawned at that moment keeps it running. A state file it can't read counts as work open.

### `orchestral status [--json]`

One view of the fleet: whether it's paused, the run's budget, the plan windows from `orchestral limits`, and each worker. A part that can't be read, such as `claude agents` or `gh pr list`, is left empty and named in a warning, and the command still exits `0`.

For each worker it shows:

- its issue, and the open PR whose head is the worker's branch, marked `(draft)` while it is one,
- its session: `lost` when the supervisor found it can't be resumed, or else `running` when `claude agents --json --all` lists it with a pid, `stopped` when it's listed without one, or `unknown`,
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
Reviews:
  PR #38 passed at 4f2c9e1
Previews:
  PR #40 (issue-12) at https://host.example.ts.net:8443
warning: PRs are unknown: gh pr list failed: …
```

Before a run has started, the budget line reads `Budget: no run has started; <n> tokens per run`. Reviews lists the latest verdict recorded with `orchestral review` on each open PR, at the head commit it was for, and is left out when there's none. Previews lists each preview that's wanted, running and hasn't failed, with its branch and link, and is left out when there's none.

`--json` carries the same data, and is the shape later interfaces read. Fields are only ever added:

| Field | Meaning |
| --- | --- |
| `paused` | Whether the fleet is paused (`orchestral pause`). |
| `workers[]` | `name`, `issue`, `pr` (or null), `session_name`, `session`, `activity` (`working`, `waiting`, `done`, `failed` or `unknown`), `waiting_for` (`waitingFor`, or `blocked` when the session's `state` is `blocked`, or null), `context` (or null) and `wake_cost` (`{"estimated_tokens": n}`, or null). |
| `workers[].context` | `tokens`, `model`, `identity`, `window`, `percent`, `last_request_at`, `compactions` (each `at` and `post_tokens`), `cache_ttl` in seconds and `warm`, as defined in [Claude Code internals](claude-code-internals.md). |
| `worker_cap` | The policy's `workers`. |
| `limits` | The output of `orchestral limits --json`. |
| `budget` | `used` (`{"estimated_tokens": n}`, or null when it can't be read), `fleet_token_budget` and `run_started_at`. |
| `reviews[]` | `pr`, `sha` (the head commit the verdict is for) and `passed`, for each open PR with a recorded verdict. |
| `previews[]` | `pr`, `branch` and `url`, for each preview that's wanted, running and hasn't failed. |
| `warnings` | Each part that couldn't be read, and why. |

### `orchestral spawn <name> --issue <n> [--role worker|reviewer] [--pr <n>] --prompt <text> [--model <m>] [--effort <e>] [--json]`

Starts a worker on an issue as a background Claude Code session. With `--role reviewer --pr <n>`, it starts a reviewer of that PR instead: the same kind of session, with the reviewer prompt below. `--pr` is required for a reviewer and refused for a worker. `<name>` uses lowercase letters, digits and dashes, and is also the branch name. The session is named `<repo slug>-<name>`, where the slug is the main checkout's directory name.

It refuses, exiting `2`, when:

- the session name is longer than 32 characters,
- a worker with that name is already in the state,
- for a reviewer, the PR already has one,
- a live session already has that session name,
- the fleet is paused (see `orchestral pause`),
- the state already has `workers` workers, the policy's cap. Every recorded worker counts, stopped or not, until it's cleaned up,
- the run's fleet usage (see [Budget](budget.md)) has reached `fleet_token_budget`. Before a run has started, nothing counts, and until the supervisor (#25) starts runs, no run has. The fleet is every worker whose session `claude agents --json --all` still lists, and the orchestrator, or
- the supervisor has the fleet wound down at a plan limit, and the limits can't be read to check it again, or
- any plan window from `orchestral limits` is at or past `plan_limit_percent`. When several are, the refusal names the one that resets last, and the fix gives its reset time.

When the plan limits can't be read, spawn warns on stderr and carries on, so unavailable limits alone never stop work:

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

A reviewer's prompt names the PR and its issue. It says what to check: the policy's `review_command` verbatim when set, or else that the PR meets the issue's acceptance criteria and keeps to its non-goals. It asks for realistic failures in normal use rather than speculative hardening. The reviewer posts one PR comment, then records its verdict on the head commit it reviewed with `orchestral review`, and never edits code.

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

### `orchestral hook pre-compact` and `orchestral hook session-compact`

The plugin's `hooks/hooks.json` runs both, so the orchestrator carries on correctly after its conversation is compacted. Each reads the hook event on stdin, and acts only when its `session_id` holds the claim on the repository at its `cwd`. Any other session, or input that can't be read, gets nothing. Both exit `0` on any problem they expect, such as an unreadable status, and never exit `2`, so they never block a compaction. Each has a 30-second timeout, so a slow `gh` or `claude` call can't hold up the compaction for long. The context puts the current status first and the one from before compaction after it, so the current one survives if Claude Code shortens a long context.

- `hook pre-compact` is the `PreCompact` hook. It saves the fleet's status, as `orchestral status` prints it, to `orchestrator/before-compact.txt` in the store.
- `hook session-compact` is the `SessionStart` hook for the `compact` source. Its `additionalContext` tells the orchestrator that it was compacted. It points to the orchestrate skill and the binary under `CLAUDE_PLUGIN_ROOT`, and gives the status saved before compaction and the status now. Then it deletes the saved status, so a later compaction never shows a stale one. A status that can't be read is replaced by a line saying why.

### `orchestral merge-check <pr> [--json]`

Shows whether PR `<pr>` could merge at its head commit, condition by condition, and never merges. It reads the PR with `gh pr view`, then checks:

- the fleet isn't paused,
- the PR is open, isn't a draft, targets `main`, and GitHub reports it `MERGEABLE`; an `UNKNOWN` answer fails until GitHub has worked it out,
- the owner hasn't put it on hold (the state's `holds`),
- CI passed: at least one check on the head commit succeeded and none failed, so checks that were all skipped or neutral don't count,
- a review passed at the head commit (the state's `reviews`),
- every finding is decided (see [Findings](findings.md)).

Then the route, from the state's `routes`, where no record means the owner's:

- **the owner's:** the owner approved the head commit (the state's `approvals`). An approval of an earlier commit doesn't count.
- **the supervisor's:** the policy's `supervisor_merges` is on, and the PR changes nothing under `.orchestral/` or `.github/`, which are the owner's to review. It reads the changed paths from the GitHub API only on this route, and a rename counts as changing both its old and new path.

```text
PR #42 at 4f2c9e1, merged by the owner:
  pass  not paused
  pass  open
  pass  not a draft
  pass  targets main
  pass  no merge conflict
  pass  not held
  FAIL  CI passed: checks are still running
  pass  review passed
  pass  findings decided
  pass  approved by the owner
It can't merge yet: 1 condition failed.
```

It exits `0` when every condition passes, and otherwise refuses, exiting `2`. `--json` prints `pr`, `head`, `route` (`human` or `supervisor`) and `conditions`, each with `name`, `passed` and `detail`, the reason it failed.

### `orchestral try <pr> [--stop] [--json]` and `orchestral previews [--json]`

`try` asks for a preview of open PR `<pr>`, so the owner can test it. It needs a `preview` section in the policy, and refuses without one, or when the PR isn't open.

When the policy's `preview.url` is set, the host serves each PR, so nothing starts: `try` prints the link with `{pr}` and `{branch}` filled in.

```text
PR #42's preview is at https://add-login.example.dev
```

Otherwise it records the preview in the state, and the supervisor runs it. It refuses when no worker has the PR's branch checked out, since the preview runs in that worker's worktree and reloads as the worker changes it. Asking for a preview past `preview.max_running` stops the oldest one asked for. It says which, and that it's still listed by `previews` with how to restart it:

```text
Asked for a preview of PR #42. `orchestral previews` shows its link once it's running.
Stopped PR #40's preview to make room; `orchestral try 40` restarts it.
```

When no supervisor is running, it adds that nothing starts the preview until `orchestral up`. `--json` prints `pr`, `url` (the hosted link, or null), `made_room` and `supervisor_running`.

`try --stop <pr>` asks the supervisor to stop the PR's preview. It stays listed as stopped, so a later `try` doesn't run setup again. `--json` prints `pr` and `had_preview`.

On every 2-second tick, the supervisor:

- runs `preview.setup` in the worktree before a preview first starts, and again when a lockfile there changes (such as `package-lock.json`, `pnpm-lock.yaml` or `Cargo.lock`), stopping a running server first. A lockfile setup writes itself doesn't count as a change. Setup has 15 minutes, and its output goes straight to the log. If it fails, the preview isn't started and the supervisor records a `preview` event,
- starts `preview.run` with `sh -c` in the worktree, detached in its own process group, so the server outlives the supervisor. A restarted supervisor finds a running server by its pid and start time and leaves it alone,
- where `portless --version` runs, starts it as `portless <repo slug>-pr-<n> --force sh -c <run>`, which sets `PORT` itself (ADR-0006). The slug is the main checkout's directory name, lowercased, with anything but letters and digits as `-`. `PORTLESS_TAILSCALE=1` is set when `tailscale status` succeeds, and removed otherwise. After each start, it waits up to 15 seconds for the route in `portless list` (and its `tailscale:` or `funnel:` link, when Tailscale is on), and keeps the route's pid, port and link: the tailnet link when there is one, or else the route's `https://<name>.localhost` link. When the route doesn't show up, or portless exits first, it stops what it started, logs that, and falls back as if portless weren't installed. Without portless, `PORT` is set to a free local port and the link is `http://localhost:<port>`,
- before a start with Tailscale on, removes stale `tailscale serve` entries: an HTTPS entry whose only handler is a `/` proxy to `127.0.0.1` or `localhost` on a port from 4000 to 4999, where nothing listens (on `127.0.0.1` or `::1`) and no portless route uses that port. When `portless list` can't be read, it removes nothing. It removes each with `tailscale serve --https=<port> off`, never `tailscale serve reset`, and leaves every other entry alone,
- after a start with a tailnet link, posts the link on the PR as one comment starting `<!-- orchestral -->`, and edits that comment on later starts, so the link stays right after a restart. A later start without a tailnet link edits it to say there's no link for now, since the old one could reach another preview. A failure to post is logged, and the preview keeps running,
- restarts a server that exited once. If it exits again at the same commit, it records `PR #42's preview exited twice; see <log>` as a `preview` event, and waits for a new commit in the worktree to start it again. A crash before a reboot counts as one exit, so the server comes back after it,
- stops a preview that's no longer wanted with SIGTERM to its process group, then SIGKILL if it's still running 5 seconds later. Under portless, it first sends SIGINT to the pid `portless list` showed for the route, not to any wrapper above it, and gives portless 12 seconds, longer than its own 10 before it kills the app, to remove its route and `tailscale serve` entry,
- stops trying a preview whose worktree can't be read, recording a `preview` event, or that the policy no longer runs, until a new commit or `try`. A problem with one preview is logged, and the others carry on.

Setup and server output go to `.git/orchestral/logs/preview-<pr>.log`.

`previews` lists each preview as `starting`, `running` with its link, `failed` with why, `stopping`, or `stopped` (stopped to make room, with how to restart it, or by `try --stop`), and its log:

```text
PR #40 stopped: stopped to make room; `orchestral try 40` restarts it (log .git/orchestral/logs/preview-40.log)
PR #42 running at https://host.example.ts.net:8443 (log .git/orchestral/logs/preview-42.log)
```

`--json` prints `previews`, each with `pr`, `status`, `url`, `detail` and `log`.

### `orchestral review <pr> --pass|--fail --sha <sha> [--json]`

Records a reviewer's verdict on PR `<pr>`'s head commit, in the state's `reviews`. It refuses, exiting `2`, when the PR isn't open, when `<sha>` (at least 7 characters) isn't the PR's current head, or when the fleet has no reviewer for the PR. A verdict holds only for the commit it names.

A failed verdict on a commit that hasn't failed before is a round. On the second round, it adds the `blocked` label to the issue the reviewer was started for, creating the label if the repository lacks it, and the fleet moves on. The label is added before anything is recorded, so when it fails the verdict can be recorded again. It records one `pr` event:

```text
pr #40 (worker issue-12): review passed at 1111111: clean up review-40
pr #40 (worker issue-12): review failed at 1111111 (round 1 of 2): clean up review-40, and have issue-12 fix it
pr #40 (worker issue-12): review failed at 2222222 (round 2 of 2): issue #12 is now blocked, so clean up review-40 and issue-12, and move on
```

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

Removes a finished worker: its session, worktree and local branch, and its entry in the state. A worker is finished when its PR merged or closed, as `gh pr list --head <branch> --state all` shows, or when its open PR failed its last review round, which leaves the PR open for the owner. PRs from forks are ignored, and a merged PR is preferred over a closed one. Git isn't asked, since a squash merge leaves the branch's own commits off `main`. A worker with no PR, such as a reviewer, can be removed when its branch has no commits beyond `origin/main`, where it started.

It refuses, exiting `2`, while the worker's PR is open and not blocked by reviews, before touching anything. Otherwise it stops the session as `stop` does, so nothing lands after the check, and refuses when the worktree has uncommitted changes, when the branch has commits its PR's head doesn't contain (fetching `pull/<n>/head` when that head isn't local), or when it has commits and no PR. A refusal leaves the worker stopped, with its worktree and branch. `--force` skips these checks and discards that work. Like `stop`, it refuses anything but a background session.

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
