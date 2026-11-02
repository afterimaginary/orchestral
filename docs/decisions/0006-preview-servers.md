---
description: The supervisor runs previews with `portless <name> --force` and Tailscale registration, reads each link from `portless list` after every start, and stops a preview with SIGINT to the pid portless reports.
status: accepted
---

# ADR-0006: Supervisor-owned preview servers

## Context

The owner tests a PR from a preview link, often on a phone. The link has to outlive the session that asked for it, so the supervisor owns the server processes rather than a worker or the orchestrator. On machines that have it, portless gives each server a named route, and its Tailscale option registers the server with `tailscale serve` so other devices on the tailnet can reach it.

## Test

Tested with portless 0.15.6 and Tailscale 1.104.1, with `PORTLESS_TAILSCALE=1`. Each preview was a small static server reading `PORT`.

A stand-in supervisor, a plain shell script detached with `nohup`, started two previews with `portless <name> --force <cmd>` and exited.

| Step | Result |
| --- | --- |
| Start two previews | Both routes appeared in `portless list`, each with a local port in the 4000–4999 range and a tailnet URL. Both URLs served their own content after the stand-in had exited. |
| Restart one | SIGINT to the pid `portless list` showed removed its route and its `tailscale serve` entry. Starting it again gave a new local port, and its link was read from `portless list` and served the right content. |
| Stop both | SIGINT to each pid shown removed both routes and both `tailscale serve` entries. No server processes were left. |

The machine already had a stale `tailscale serve` entry on port 443. Because of it, the previews got ports 8443 and 8444 instead of 443 and 8443. In this run, the restarted preview happened to get its old tailnet port back, but that isn't guaranteed, because ports are handed out in start order.

## Decision

- The supervisor starts each preview with `portless <name> --force <run command>`, detached, with `PORTLESS_TAILSCALE=1` when Tailscale is available. The `$PORT` with `tailscale serve` fallback isn't needed.
- `<name>` is `<repo slug>-pr-<n>`. An explicit name lets the supervisor find each PR's route in `portless list`, and doesn't depend on the project's own portless configuration. `portless run` would infer the name instead, and that form wasn't tested.
- After every start, the supervisor reads the preview's link from `portless list` rather than assuming the previous one.
- The supervisor stops a preview with SIGINT to the pid `portless list` shows for its route, never to a wrapper above it.
- Before starting previews, the supervisor clears stale `tailscale serve` entries using the rule in #52. It never runs `tailscale serve reset`.

## Consequences

- A preview survives the session and the process that started it, as long as the machine stays on.
- A crash or hard kill is expected to leave a stale entry, which pushes later previews to higher ports until it's cleared. This wasn't tested here (see Limits), but the pre-existing entry shows the effect.
- Without Tailscale, previews still get a local portless route, but no link for other devices.
- Without portless, previews fall back to plain `$PORT` on localhost (#51), also with no link for other devices.

## Limits

- The servers were a static file server, not a real dev server with hot reload.
- A hard kill and the stale-entry cleanup weren't tested here. The pre-existing stale entry was left in place.
- The links were fetched from this machine, not from a phone on the tailnet.
