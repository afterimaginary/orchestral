# Security

Orchestral runs on your machine: a Claude Code plugin and the `orchestral` binary it builds. The binary starts Claude Code sessions, runs `git` and `gh` as you, and keeps its state under the repository's git directory and its policy in your config directory. Its only network use is through those tools: GitHub through `gh`, and, if you turn previews on, `portless` and Tailscale for preview links.

Workers act with your Claude Code login and your GitHub credentials, so the binary's guard (no merging, approving or pushing to `main` from a worker) is a guard against mistakes, not a security boundary. Run a fleet only on repositories and issues you trust.

## Reporting a vulnerability

Please report it privately through GitHub: on the repository's **Security** tab, choose **Report a vulnerability**. Don't open a public issue for it.

Include what you found, how to reproduce it, and the version (`orchestral --version`, or the commit). You'll get a reply within a week.

## Supported versions

Only the latest release gets fixes.
