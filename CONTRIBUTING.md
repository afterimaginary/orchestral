# Contributing

Thanks for your interest in improving Orchestral.

## Before you start

- **Bugs:** open an issue with what you ran, what you expected and what happened, including your `claude --version`.
- **Features and larger changes:** open an issue first so the approach can be agreed before you write code.

## Development setup

Requires a Rust toolchain. To run `make ready` before every push:

```bash
git config core.hooksPath .githooks
```

| Command       | Does                                                                                         |
| ------------- | -------------------------------------------------------------------------------------------- |
| `make ready`  | Format check, lints, unit tests and CLI tests. Run it before opening a pull request.          |
| `make verify` | `ready`, integration tests, a release build and plugin validation. CI runs it on every PR.   |

## Pull requests

- Keep each pull request to one change, and link the issue it resolves.
- Add or update tests for any change in behaviour.
- Use [Conventional Commits](https://www.conventionalcommits.org/) for the pull request title, e.g. `fix(spawn): refuse a duplicate session name`.
- Fill in the pull request template, including what you ran to verify the change.
