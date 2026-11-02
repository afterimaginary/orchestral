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

`orchestral hook worker-guard` is the worker's Bash hook. It allows every command for now.

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
