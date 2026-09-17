---
name: self-improving-from-usage-logs
description: "Find repeated TraceDecay tool or skill friction in session logs and turn supported patterns into scoped fixes."
---

# Self-Improving From Usage Logs

Start from the reported symptom or a bounded session window. Search with
`tracedecay_message_search`, then use `tracedecay_lcm_grep` or
`tracedecay_lcm_expand_query` for the relevant context. Check analytics or
health only when needed to distinguish a behavior gap from an unavailable
service; a full cross-project scan is not a prerequisite.

For automation failures, use the repo-local `inspecting-automation-cycles`
skill. For managed-skill writer outcomes, use `writing-agent-managed-skills`.
Read only the evidence relevant to the suspected failure.

## Choose the smallest durable fix

| Observed pattern | Investigate |
|---|---|
| Repeated invalid command | Misleading help, trigger text, or a missing affordance. |
| Misinterpreted counts | Field semantics, scope, and sample limits. |
| Direct store queries | Missing supported retrieval or analytics surface. |
| Missed useful skill | Trigger specificity and task relevance. |
| Healthy skips treated as failures | Status explanations and grouping. |
| Repeated fact proposals | The canonical deduplication/validation path. |

Corroborate a broad rule with repeated sessions or direct reproducible evidence.
A single failing command can justify its own narrow fix. Do not add compatibility
aliases or redesign a subsystem merely because an agent used it incorrectly.
Keep secrets, transient failures, and progress notes out of durable memory.

Use supported CLI, MCP, or dashboard retrieval. The legacy
`scripts/friction-scan.sh` reads profile databases directly; do not run it as
an analytics fallback. Missing metrics are a limitation, not successful empty
results or a reason to bypass the public authority.

When improvement is authorized, apply the smallest supported change and verify
the behavior that failed. Use the relevant code or skill validation once;
review-only work does not require builds. Report the cited pattern, change or
recommendation, verification, and remaining uncertainty.
