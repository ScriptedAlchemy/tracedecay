---
name: diagnosing-analytics
description: 'Explain TraceDecay usage, hook capture, attribution, or adoption analytics using supported diagnostics and source coverage.'
---

# Diagnosing analytics

Durable usage analytics, project hook JSONL, and session archives are different
sources. Read their supported diagnostics and attribution rather than querying
private databases or treating one source's count as the complete history.

The analytics diagnostics sync imports new hook rows; its no-sync mode is a
read-only view. Analytics MCP reads do not themselves refresh ingestion. Inspect
included versus total rows, malformed input, provider attribution, and temporal
coverage. A null project can mean global scope, not a broken project join.

For missing native hooks, inspect host registration and doctor evidence; Codex
trust gating can prevent capture. Import is incremental, so replayed source rows
must not be mistaken for new activity.

Invocation counts measure use, not task success. Compare outcomes, unnecessary
calls, false triggers, and missed useful routing before recommending more skill
or hint exposure. Trace a discrepancy to its capture, import, attribution, or
read scope before changing configuration.
