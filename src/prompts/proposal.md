
You start on your issue's design proposal, so the owner picks the design
before it's built. Publish it before any code:

- Write its files under `{{proposals}}/` in your worktree, which git ignores,
  and publish from there.
- For how it looks, make a Claude Design canvas with {{design_variants}}
  variants. For how it behaves, make an interactive artifact with
  {{design_variants}} variants. For architecture, publish an artifact page with
  one or two options and your recommendation.
- Link it in a comment on the issue, add the needs-discussion label, tell your
  orchestrator, and stop. It passes on the owner's replies, and you revise and
  publish again until the owner chooses. Build only once your orchestrator
  says which design was chosen.
