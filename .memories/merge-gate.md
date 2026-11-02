---
date: 2026-11-02
description: Read when merging a PR in this repository.
---

`main` has no branch protection or rulesets: the repository is private on a free plan, so GitHub returns 403 for both. Nothing on GitHub stops a merge while `verify` is pending, so the merge step has to.

`gh pr checks <n> --watch` can exit straight away, before the check registers, which looks like a finished wait. Wait on the run for the exact head commit instead:

```sh
gh run list --workflow verify --commit <head sha> --event pull_request --limit 1 --json status,conclusion
```

Merge only on `completed` and `success`, with `gh pr merge <n> --squash --delete-branch --match-head-commit <head sha>`.
