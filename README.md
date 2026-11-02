# Orchestral

A small workflow layer for Claude Code, as a plugin plus a Rust binary. Give it a brief, and it plans the work, runs a fleet of worker sessions, respects your budget, and brings you in for the designs and decisions that are yours.

**Status:** early development. Nothing is usable yet; see [`STATE.md`](STATE.md).

## Development

Requires a Rust toolchain. To run the checks before every push:

```bash
git config core.hooksPath .githooks
```

| Command       | Does                                                                                       |
| ------------- | ------------------------------------------------------------------------------------------ |
| `make ready`  | Format check, lints, unit tests and CLI tests.                                             |
| `make verify` | `ready`, integration tests, a release build and plugin validation. CI runs it on every PR. |

Integration tests run offline: `tests/common/mod.rs` puts stub `claude`, `gh`, `portless` and `tailscale` scripts on `PATH`, which record their calls and reply with canned output. Output captured from Claude Code is kept under `tests/fixtures/claude/<version>/`.

## Documentation

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md): how the parts fit together
- [`docs/decisions/`](docs/decisions/): decision records
- [`docs/specs/`](docs/specs/): expected behaviour
- [`CONTRIBUTING.md`](CONTRIBUTING.md): how to contribute

## License

[MIT](LICENSE)
