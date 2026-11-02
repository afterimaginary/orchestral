# State

CLI commands and the supervisor share state through files. There is no IPC: a command changes the desired state under a lock, and the supervisor reconciles on its next tick, so a crash recovers by reconciling again.

## Location

State lives in `orchestral/` inside the repository's git common directory, which `git rev-parse --git-common-dir` gives. Every worktree of a repository resolves to the same directory, normally `.git/orchestral/` in the main checkout. Git never tracks it.

| File | Holds |
| --- | --- |
| `state.json` | The current state. |
| `events.jsonl` | Every event, one per line, append-only. |
| `github.json` | The supervisor's last GitHub snapshot: open issues and PRs, PR drafts, heads, CI results and reviews, comment ids seen recently, when the last poll succeeded, and its failures and backoff. |
| `watch-cursor` | The `seq` of the last event `orchestral watch` delivered to the orchestrator, as a number and a newline. |
| `lock` | The advisory lock. Its contents don't matter. |
| `supervisor.pid` | The running supervisor, as `{"pid": n, "started": "<ps lstart>"}`. The start time tells it from a later process that reuses the pid. |
| `logs/supervisor.log` | The supervisor's log, one timestamped line per entry. |

## `state.json`

A JSON object with a `schema` version, currently `6`. A file with any other version is an error, so an older or newer binary never misreads it. A missing file reads as the default state.

| Field | Meaning |
| --- | --- |
| `schema` | The schema version. |
| `next_seq` | The `seq` the next event gets, starting at `1`. |
| `workers` | Each worker by name: its session name, issue, model, effort, worktree, branch, the job id `claude --bg` printed (null until launched), for a reviewer the PR it reviews as `review_of` (null for a worker), and when it was created. |
| `orchestrator` | The session that claimed the repository, as its session id and when it claimed, or null. |
| `paused` | Whether the fleet is paused. While it is, spawns are refused. |
| `run_started_at` | When the current run started, in RFC 3339 UTC, or null before the first run. The supervisor sets it when it starts with no worker open, and the fleet budget counts from here. |
| `observed` | What the supervisor last saw each launched worker's session doing, by worker name: `busy`, `idle`, `waiting` or `exited`. |
| `reviews` | The latest review verdict on each PR, by PR number: the head commit it's for as `sha`, whether it `passed`, and the commits that `failed` review, oldest first. |

Later features add their own fields here. Any change to the fields raises `schema`, so an older binary refuses the file rather than dropping fields it doesn't know.

## `events.jsonl`

Each line is one event:

```json
{"seq":1,"ts":"2026-11-02T08:52:10.123Z","kind":"worker","text":"issue-12 opened PR #40"}
```

| Field | Meaning |
| --- | --- |
| `seq` | Increases by one per event. A process crash can skip one, but never reuses one. A power loss can, since the files aren't synced to disk on every write. |
| `ts` | When the event was recorded, in RFC 3339 UTC. |
| `kind` | What the event is about. |
| `text` | The event, for people and the orchestrator. |

Lines are only ever appended. `orchestral watch` delivers them to the orchestrator and records the last one delivered in `watch-cursor` (see `docs/specs/cli.md`).

## Writes

- Every change to `state.json`, and every event appended, happens while holding an exclusive advisory lock on `lock`.
- `state.json` is written to a temp file in the same directory, synced, then renamed over the old file. A reader never sees a partial write, so reading doesn't take the lock.
- Recording an event writes `state.json` first, with `next_seq` advanced, and then appends the line. If a crash left the last line without its newline, the append ends that line first, so only the torn event is lost.
