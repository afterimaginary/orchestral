# Orchestral

A small workflow layer for Claude Code, as a plugin plus a Rust binary. Give it a brief, and it plans the work, runs a fleet of worker sessions, respects your budget, and brings you in for the designs and decisions that are yours.

**Status:** early development. A fleet can be started and watched, but it doesn't yet react to events, review or merge; see [`STATE.md`](STATE.md).

## Trying it

Orchestral needs a Rust toolchain (1.85 or later). Install it from this repository's marketplace, in the terminal or in Claude Code Desktop:

```bash
claude plugin marketplace add afterimaginary/orchestral
```

```bash
claude plugin install orchestral@afterimaginary
```

The first session after installing builds the binary, which can take a minute. Or build a checkout and load it directly:

```bash
cargo build --release
claude --plugin-dir /path/to/orchestral
```

In the project, run `orchestral init` once (as `/path/to/orchestral/bin/orchestral init`), set `fleet_token_budget` in `.orchestral/policy.md`, and push the policy to `main`. Then `/orchestral:orchestrate` checks the setup, claims the project, starts the supervisor and hands issues to workers. Give it a brief, as in `/orchestral:orchestrate plan from docs/brief.md`, and it first drafts milestones and issues for you to approve, and files them once you do. The plugin's `bin/orchestral` runs the build from this checkout, and the skill calls it by that path, so another `orchestral` on `PATH` doesn't get in the way.

## Development

Requires a Rust toolchain. To run the checks before every push:

```bash
git config core.hooksPath .githooks
```

| Command       | Does                                                                                       |
| ------------- | ------------------------------------------------------------------------------------------ |
| `make ready`  | Format check, lints, unit tests and CLI tests.                                             |
| `make verify` | `ready`, integration tests, a release build and plugin validation. CI runs it on every PR. |
| `make e2e`     | Real runs against the installed, logged-in `claude`. Never part of `verify` or CI.          |
| `make sandbox` | A real worker on an issue in the sandbox repository. Run before each milestone closes.     |

Integration tests run offline: `tests/common/mod.rs` puts stub `claude`, `gh`, `portless` and `tailscale` scripts on `PATH`, which record their calls and reply with canned output. Output captured from Claude Code is kept under `tests/fixtures/claude/<version>/`.

Stubs can't catch a change in Claude Code itself, so `tests/e2e.rs` holds real runs, ignored by `cargo test`:

- **`make e2e`** runs `compatibility`, which rereads a fresh print session's transcript, the plan limits and `claude agents` with the parsers, and `local_runner`, which rebuilds a repository with a local bare remote at `$TMPDIR/orchestral-e2e/repo` and runs `init`, `up`, `spawn`, `status`, `stop` and `resume` with a real worker on Sonnet. Claude Code only starts background sessions in a trusted directory, so run `claude` in that path once and accept the trust prompt; the runner refuses until then, and never changes Claude Code's settings itself.
- **`make sandbox`** uses the sandbox clone at `~/src/orchestral-sandbox`. It refuses with local changes or live workers, files an issue, spawns a worker that opens a PR, checks `status` shows it, then closes the PR and issue and removes the worker. It grows as the fleet gains steps such as review and merge.

Both clear the caller's Claude Code session variables, so they run as if from a plain terminal.

## Documentation

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md): how the parts fit together
- [`docs/decisions/`](docs/decisions/): decision records
- [`docs/specs/`](docs/specs/): expected behaviour
- [`CONTRIBUTING.md`](CONTRIBUTING.md): how to contribute

## License

[MIT](LICENSE)
