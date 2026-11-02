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
- An error prints one line: `error: <message>`.
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
