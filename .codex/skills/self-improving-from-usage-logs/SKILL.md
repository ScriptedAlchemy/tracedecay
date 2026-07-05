---
name: self-improving-from-usage-logs
description: 'Use when mining TraceDecay session logs, analytics, diagnostics, automation artifacts, or agent transcripts to improve TraceDecay code, tools, or packaged skills.'
---

# Self-Improving From Usage Logs

Use real agent behavior as an eval surface: logs reveal where tools are
confusing, silent, too hard to discover, or missing a safe command.

## Workflow

1. Start with adoption and health:
   `tracedecay analytics diagnostics --all --no-sync`, `tracedecay doctor`,
   and `tracedecay tool lcm_status --provider all --json`.
2. Search transcripts for friction phrases, tool failures, and bypasses:
   `tracedecay_message_search`, then narrow with `tracedecay_lcm_grep` or
   `tracedecay_lcm_expand_query`.
3. Inspect automation evidence with `tracedecay:inspecting-automation-cycles`
   and managed-skill evidence with `tracedecay:writing-agent-managed-skills`.
4. Turn patterns into the smallest durable fix: CLI affordance, clearer
   diagnostic field, better skill trigger, missing dashboard summary, or test.
5. Verify with the narrow command that failed in the logs plus the owning Rust
   test or plugin validation suite.

## Opportunity Ranking

| Pattern | Prefer this fix |
|---|---|
| Repeated invalid flag or command | Accept alias or improve CLI help. |
| Counts interpreted incorrectly | Add explicit field names and sample limits. |
| Agents query stores directly | Add/read skill guardrail and expose CLI summary. |
| Skill exists but is not invoked | Improve description trigger and analytics events. |
| Automation skips look like failures | Add grouped run/status summary. |
| Same fact proposed repeatedly | Deduplicate before approval queue. |

## Guardrails

- Do not store secrets, transient failures, or one-off progress as memory.
- Do not convert a single anecdote into a broad rule without at least two
  corroborating sessions, an automation artifact, or a failing command.
- Keep code fixes smaller than the evidence. If logs show a CLI paper cut,
  ship the CLI compatibility fix before redesigning the subsystem.

## Deliverable

Report the evidence source, repeated pattern, ranked opportunity, code or skill
change made, verification command, and any residual adoption gap.
