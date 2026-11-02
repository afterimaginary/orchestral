# Claude Code internals

Orchestral relies on some Claude Code behaviour that isn't documented. This page records each format it reads, as seen on Claude Code 2.1.314. These can change in any release, so tests run from fixtures stored under the version they were captured from (`tests/fixtures/claude/<version>/`).

## Transcripts

### Location

A session's transcript is `<config>/projects/<project>/<session id>.jsonl`, where `<config>` is `$CLAUDE_CONFIG_DIR` or `~/.claude`.

`<project>` is the session's working directory with every character that isn't an ASCII letter or digit replaced by `-`. For example, `/home/dev/my_app` becomes `-home-dev-my-app`. Long paths are shortened, so when that directory has no transcript, every project directory is scanned for `<session id>.jsonl`.

### Entries

Each line is one JSON object with a `type`. The ones read here:

| `type` | Fields used |
| --- | --- |
| `assistant` | `isSidechain`, `timestamp`, `message.id`, `message.model`, `message.usage` |
| `user` | `timestamp` |
| `attachment` | `timestamp`, and with `attachment.type` of `model`, `attachment.identity.modelId` |
| `system` | with `subtype` of `compact_boundary`, `timestamp` and `compactMetadata.postTokens` |

Other types, such as `queue-operation`, `file-history-delta`, `pr-link` and `api-request`, are ignored.

One response is written as several `assistant` entries sharing a `message.id`, one per content block, each with the same `usage`.

### Context size

The context is the sum of `input_tokens`, `cache_creation_input_tokens` and `cache_read_input_tokens` in the `usage` of the last real `assistant` entry. An entry isn't real when:

- `isSidechain` is `true`, or
- its usage sums to 0, as for an API error, whose `message.model` is `<synthetic>`, or a copy written by compaction.

When `usage.iterations` is present, the sum comes from its last element with `type` of `message` instead. On 2.1.314 every response seen had exactly one iteration, of type `message`. The rule for several iterations follows earlier versions and is tested from a constructed fixture.

### Compaction

A `system` entry with `subtype` of `compact_boundary` marks a compaction, and `compactMetadata.postTokens` gives the context left after it. When one comes after the last real response, it replaces the context size, and nothing is cached.

### Last request time

The time the last response was requested is the `timestamp` of the last `user` or `attachment` entry before that response's first entry.

### Prompt cache

Scanning back from the end, the first `usage.cache_creation` with tokens decides the cache lifetime: `ephemeral_5m_input_tokens` above 0 means 300 seconds, otherwise `ephemeral_1h_input_tokens` above 0 means 3600 seconds. The cache is warm while less than that has passed since the last request, and no compaction followed it.

### Model and context window

`message.model` is the API model, such as `claude-opus-5-5`, and never has a `[1m]` suffix. The model the session was told it is comes from an `attachment` entry with `attachment.type` of `model`, as `attachment.identity.modelId`, which may end in `[1m]`. An identity whose name without `[1m]` differs from `message.model` is out of date, after a model switch, and is ignored.

The context window is:

| Model | Window |
| --- | --- |
| An identity ending in `[1m]` | 1,000,000 |
| Fable | 1,000,000 |
| Sonnet 5 or later | 1,000,000 |
| Haiku | 200,000 |
| Opus 4.6 or later, when `~/.claude.json` has `oauthAccount.organizationType` of `claude_max`, `claude_team` or `claude_enterprise` | 1,000,000 |
| Anything else | Unknown, or 1,000,000 if the context is already over 200,000 |

Percentages of the window aren't capped at 100.

### Reading large files

Only the last 1 MiB is read when it holds a real response, and the whole file otherwise. If that part has no model identity, the first 1 MiB is searched for one, since a session is usually told its identity near the start.
