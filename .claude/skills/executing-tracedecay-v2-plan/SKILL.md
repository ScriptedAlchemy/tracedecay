---
name: executing-tracedecay-v2-plan
description: Invoke the shared minimal TraceDecay V2 product-delivery contract.
---

# Deliver TraceDecay V2 work

Resolve the repository root and read
`.codex/skills/executing-tracedecay-v2-plan/SKILL.md`. That file is the only
shared contract.

Do not create a Claude-local parser, authority graph, generated dispatch state,
completion ledger, script mirror, or fallback implementation. Plan Markdown is
human context only. Dispatch bounded product implementation directly, split
every item across multiple agents or subagents, use no automatic timeouts, and
verify the integrated product diff with focused tests.
