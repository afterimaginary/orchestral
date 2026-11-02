# Findings

Problems a worker found and didn't act on would go unnoticed if nothing forced a decision. So workers report them, the orchestrator decides each one, and a PR can't merge until every one is decided. The parsing is in `src/findings.rs`. Refusing the merge belongs to the merge check.

## The section

Every PR body has a `## Findings` section. It runs from that heading, alone on its line, to the next line starting `## `, or to the end of the body. Blank lines in it are ignored. It holds either:

- `None`, alone, or
- one line per finding, `- [tag] text`, where the tag ends at the first `]` and the text isn't empty.

```markdown
## Findings

- [pending] Login form allows an empty display name (src/auth.ts)
- [issue #123] Avatar upload has no size limit
- [fixed] Reviewer: off-by-one in pagination
- [dropped: internal script, no user impact] Slow seed script
```

## Outcomes

| Tag | Meaning |
| --- | --- |
| `[pending]` | Reported by the worker, not decided yet. |
| `[issue #N]` | Filed as issue N. |
| `[fixed]` | Fixed in this PR. |
| `[dropped: reason]` | Not worth tracking, because it has no user impact. The reason isn't empty, and can't contain `]`. |

## The check

The check passes when the section is `None` or every finding has a decided outcome. It fails, saying why, when:

- the body has no `## Findings` heading,
- the section is empty,
- a line is neither `None` nor `- [tag] text`, including `None` next to a finding,
- a tag is none of the outcomes above, or
- any finding is still `[pending]`.

## Who does what

- Workers report out-of-scope problems and review findings they declined as `[pending]`, and write `None` when there are none. The worker prompt gives them the section to end every PR body with.
- The orchestrator decides each finding once the review passes, and rewrites its tag in the PR body. Doubt goes to a `priority:low` backlog issue, and a finding is dropped only when it has no user impact. A bug is a blocker to fix now, a `bug` issue for later, or a recorded drop.
- Milestone and final summaries list every drop with its reason, so the owner can promote one.
