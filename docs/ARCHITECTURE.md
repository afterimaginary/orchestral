# Architecture

Orchestral is a Claude Code plugin plus a Rust binary. Claude decides and Rust enforces: an orchestrator session plans and assigns work, and the binary runs processes, reads data and refuses anything that breaks a limit or gate. GitHub is the source of truth for issues, PRs, CI and merges.

**Only the repository skeleton and the CLI's conventions exist today.** This page describes the intended structure, and will track what is built.

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

| Part                 | Does                                                                                                                       | Never does                                                           |
| -------------------- | -------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------- |
| Orchestrator session | Turns the brief into issues, picks work, answers worker questions within policy, routes PRs, decides findings              | Writes product code, reviews code, merges                            |
| Worker session       | Implements one issue, posts proposals, opens the PR, reports findings, boots its preview                                   | Merges, approves, decides its own out-of-scope findings, spawns sessions |
| Reviewer session     | A fresh worker that reviews one PR and records a verdict                                                                   | Edits the PR's code                                                  |
| Supervisor           | Long-running loop: watches GitHub and sessions, writes events, sends reminders, keeps previews running, performs merges    | Makes judgment calls                                                 |
| CLI                  | Short-lived commands that read state, change desired state under a lock, and refuse what breaks a rule                     | Runs long-lived work                                                 |

CLI commands write desired state under an advisory lock, and the supervisor reconciles on each tick. There is no IPC, so recovering from a crash is the normal path: on restart it reconciles again. Every event and heartbeat reaches the orchestrator as a stdout line from `orchestral watch`, read by a plugin monitor.
