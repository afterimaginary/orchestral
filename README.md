# Orchestral

A small workflow layer for Claude Code, as a plugin plus a Rust binary. Give it a brief, and it plans the work, runs a fleet of worker sessions, respects your budget, and brings you in for the designs and decisions that are yours.

**Status:** early development. Nothing is usable yet; see [`STATE.md`](STATE.md).

## Development

Requires a Rust toolchain. To run the checks before every push:

```bash
git config core.hooksPath .githooks
```

| Command       | Does                                                                     |
| ------------- | ------------------------------------------------------------------------ |
| `make ready`  | Format check, lints and tests.                                           |
| `make verify` | `ready`, a release build and plugin validation. CI runs it on every PR.  |

## Documentation

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md): how the parts fit together
- [`docs/decisions/`](docs/decisions/): decision records
- [`docs/specs/`](docs/specs/): expected behaviour
- [`CONTRIBUTING.md`](CONTRIBUTING.md): how to contribute

## License

[MIT](LICENSE)
