# Budget

A run has one fleet token budget, `fleet_token_budget` in the policy. This page defines what counts against it.

## What a run has used

The total is the weighted tokens of every response in every fleet session's transcript since the run started, including each session's subagent transcripts in `<session id>/subagents/`. Before a run has started, the total is zero.

Each response is counted once. A response is written as several transcript entries sharing a `message.id`, each with the same usage. Responses before the run started, and API errors, which have no usage, add nothing. A session with no transcript yet adds nothing.

## Weights

Tokens are weighted by their API price relative to an input token:

| Tokens | Weight |
| --- | --- |
| Input | 1 |
| Output | 1 |
| Cache writes that live 1 hour | 2 |
| Cache writes that live 5 minutes | 1.25 |
| Cache reads | 0.1 |

Cache writes without a breakdown by lifetime are weighted as 5-minute writes. The weighted sum is rounded to a whole number.

## An estimate

These weights are API prices. Whether a plan's usage limits weigh tokens the same way is unknown, so the total is an estimate. It's shown as `about <n> tokens (estimate)`, and in JSON as `estimated_tokens`.
