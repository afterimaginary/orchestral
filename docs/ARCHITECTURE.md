# Architecture

Orchestral is a Claude Code plugin plus a Rust binary. Claude decides and Rust enforces: an orchestrator session plans and assigns work, and the binary runs processes, reads data and refuses anything that breaks a limit or gate. GitHub is the source of truth for issues, PRs, CI and merges.

```text
user ── brief, approvals, feedback ──▶ orchestrator session (Claude, /orchestrate skill)
                                         ▲ events on stdout       │ orchestral CLI, SendMessage
                                         │                        ▼
                          plugin monitor: orchestral watch     worker sessions (claude --bg)
                                         ▲                        ▲ one worktree each
                                         │ events.jsonl           │ spawn, stop
                                         │                        │
                          supervisor: orchestral up ──────────────┘
                            reads transcripts, /usage, gh
                            owns previews, nags, merges
                                         │
                                      GitHub (issues, PRs, CI, comments)
```

| Part                 | Does                                                                                                                                                        | Never does                                                               |
| -------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| Orchestrator session | Turns the brief into issues, picks work, answers worker questions within policy, routes PRs, decides findings                                               | Writes product code, reviews code, merges                                |
| Worker session       | Implements one issue, posts proposals, opens the PR, has a fresh subagent review it and records the verdict (ADR-0011), reports findings, boots its preview | Merges, approves, decides its own out-of-scope findings, spawns sessions |
| Reviewer session     | Optional: a separate session started with `spawn --role reviewer` that reviews one PR and records a verdict; the orchestrate skill doesn't start one        | Edits the PR's code                                                      |
| Supervisor           | Long-running loop: watches GitHub and sessions, writes events, sends reminders, keeps previews running, performs merges                                     | Makes judgment calls                                                     |
| CLI                  | Short-lived commands that read state, change desired state under a lock, and refuse what breaks a rule                                                      | Runs long-lived work                                                     |

CLI commands write desired state under an advisory lock, and the supervisor reconciles on each tick. There is no IPC, so recovering from a crash is the normal path: on restart it reconciles again. Every event and heartbeat reaches the orchestrator as a stdout line from `orchestral watch`, read by a plugin monitor. The plugin's compaction hooks put the orchestrator's instructions and the fleet's status back into its context after it's compacted. A copy installed from the repository's marketplace arrives as source, so a session-start hook builds its binary (ADR-0012).

Orchestral writes nothing of its own into the repositories it manages. The policy lives in the operator's config directory, a global file layered under a per-repository one, outside every worktree, and the plugin is enabled at local scope (ADR-0013). Run state lives under the git common directory (`docs/specs/state.md`).
