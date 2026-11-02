## Verification

- Before `/refine`: `make ready`
- The full suite, `make verify`, runs as the PR's required `verify` check. Do not run it locally unless you are changing it or reproducing a failure.

## Project notes

- **Merging:** the maintainer merges every PR in this repository. Agents open PRs, get them through review and `verify`, then stop.
- **Git hooks:** a new clone needs `git config core.hooksPath .githooks` for the pre-push `make ready`.
