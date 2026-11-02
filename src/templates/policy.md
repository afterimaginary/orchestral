---
# Tokens the whole fleet may use in a run. Required: set your own.
fleet_token_budget: 3000000
workers: 3
plan_limit_percent: 85
default_model: claude-opus-5-5
default_effort: high
merge_method: squash
# How PR previews run. Without it, there are no previews.
# preview:
#   setup: pnpm install
#   run: pnpm dev
---
Propose a design first for anything a user sees or interacts with.
Always ask me before changing auth, payments or migrations.
