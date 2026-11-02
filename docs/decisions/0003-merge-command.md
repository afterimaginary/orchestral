---
description: The supervisor merges with `gh pr merge --match-head-commit <sha>` under the owner's token, and treats a refused merge as something to re-check on a later tick.
status: accepted
---

# ADR-0003: The supervisor's merge command

## Context

Only the supervisor merges, and only after the merge check passes for a specific head commit. The supervisor is a plain process, not a Claude session, and uses the owner's `gh` login. The risk is a commit landing between the check and the merge, so that unchecked code gets merged.

## Test

Tested with `gh` 2.102.0 against a private sandbox repository. Each `gh` call ran under `env -i` with only `HOME` and `PATH` set, so nothing came from a Claude session. Before each merge, the script waited until GitHub reported the PR's head as the expected commit and its mergeable state as `MERGEABLE`.

| Case | Command | Result |
| --- | --- | --- |
| Merge at the checked head | `gh pr merge <n> --squash --match-head-commit <head>` | Merged, shown as merged by the owner's account |
| Head moved after the check | the same, with the old head | Refused: "Head branch was modified. Review and try the merge again." Exit 1, PR still open |
| Head moved, merge API | `PUT /repos/{owner}/{repo}/pulls/{n}/merge` with `sha` set to the old head | Refused with HTTP 409 and the same message |
| Merge at the new head | `gh pr merge` with the new head | Merged |

An unplanned run also showed what happens when a merge is tried too soon. Right after a push, while GitHub was still working out whether the PR could merge, it refused with "Pull Request is not mergeable".

## Decision

- The supervisor merges with `gh pr merge <n> --match-head-commit <sha>`, where `<sha>` is the head the merge check passed. #49 takes the method flag from the policy's `merge_method`, with `--squash` as the default.
- The supervisor doesn't wait for GitHub's mergeable state before merging. A refused merge isn't retried at once. The supervisor waits for the next tick and runs the whole merge check again, because a refusal usually means the PR changed or GitHub hasn't settled.
- The merge API with `sha` refuses a moved head in the same way, so it can replace `gh pr merge` if that ever changes. A successful merge through the API wasn't tested.

## Consequences

- Merges show as merged by the owner, as expected for a single-account setup.
- A merge can only use the exact head commit that passed review and CI, which the merge check (#47) relies on.
- The squash still combines that head with whatever `main` is at merge time, and CI never ran on that combination. A private repository on the Free plan can't require branches to be up to date, so this gap stays unless the merge check adds its own base check.
- The refusal messages are GitHub's, and may change, so the supervisor shouldn't parse them. A non-zero exit is enough.

## Limits

- One sandbox repository, with no required checks or branch protection, since neither is available on a private repository on the Free plan.
- Only the squash method was used.
