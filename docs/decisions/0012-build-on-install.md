---
description: A copy of the plugin installed from a marketplace builds its own binary with cargo in a SessionStart hook, rather than downloading a prebuilt release.
status: accepted
---

# ADR-0012: Installed copies build their own binary

## Context

Claude Code Desktop can't load a plugin from a directory, only from a marketplace (#81). A marketplace install copies the repository's files into Claude Code's plugin cache, with no compiled binary, so `bin/orchestral` has nothing to run.

The other ways to get a binary there cost more than they give at this stage. Prebuilt releases need release CI per platform, an authenticated download while the repository is private, and possibly macOS signing. A marketplace entry that builds a local checkout doesn't test installing from GitHub at all.

## Decision

- The repository is its own marketplace: `.claude-plugin/marketplace.json` lists the plugin with `"source": "./"`.
- A `SessionStart` hook runs `hooks/build`. When the plugin has no build, it runs `cargo build --release --locked` with Cargo's target directory in the plugin's data folder (`${CLAUDE_PLUGIN_DATA}`), and copies the binary to `target/release/orchestral` in the installed copy, where `bin/orchestral` already looks.
- If the build fails, the hook tells the session which cargo it used and where the build log is, and Orchestral's commands fail with the existing "isn't built" error.

## Consequences

- Installing needs a Rust toolchain new enough for edition 2024. The hook falls back to `~/.cargo/bin`, `/opt/homebrew/bin` and `/usr/local/bin` when its `PATH` has no cargo.
- The first session after an install or update waits for a release build before Claude's first response. Later builds reuse the target directory in the data folder, which survives updates.
- A checkout loaded with `--plugin-dir` that's already built is left alone.
- Prebuilt releases can replace this later without changing `bin/orchestral`'s contract.
