# State

CLI commands and the supervisor share state through files. There is no IPC: a command changes the desired state under a lock, and the supervisor reconciles on its next tick, so a crash recovers by reconciling again.

## Location

State lives in `orchestral/` inside the repository's git common directory, which `git rev-parse --git-common-dir` gives. Every worktree of a repository resolves to the same directory, normally `.git/orchestral/` in the main checkout. Git never tracks it.

| File | Holds |
| --- | --- |
| `state.json` | The current state. |
| `events.jsonl` | Every event, one per line, append-only. |
| `lock` | The advisory lock. Its contents don't matter. |

## `state.json`

A JSON object with a `schema` version, currently `1`. A file with any other version is an error, so an older or newer binary never misreads it. A missing file reads as the default state.

| Field | Meaning |
| --- | --- |
| `schema` | The schema version. |
| `next_seq` | The `seq` the next event gets, starting at `1`. |

Later features add their own fields here.

## `events.jsonl`

Each line is one event:

```json
{"seq":1,"ts":"2026-11-02T08:52:10.123Z","kind":"worker","text":"issue-12 opened PR #40"}
```

| Field | Meaning |
| --- | --- |
| `seq` | Increases by one per event. A crash can skip one, but never reuses one. |
| `ts` | When the event was recorded, in RFC 3339 UTC. |
| `kind` | What the event is about. |
| `text` | The event, for people and the orchestrator. |

Lines are only ever appended. Reading events out, and tracking which were delivered, belong to the event stream.

## Writes

- Every change to `state.json`, and every event appended, happens while holding an exclusive advisory lock on `lock`.
- `state.json` is written to a temp file in the same directory, synced, then renamed over the old file. A reader never sees a partial write, so reading doesn't take the lock.
- Recording an event writes `state.json` first, with `next_seq` advanced, and then appends the line.
