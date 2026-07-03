---
name: tracedecay-map-architecture
description: 'Use to map repo or directory architecture, including layered modules, dependency hotspots, and structural risks.'
---

# Map architecture

Use when asked to map the repo or a directory's architecture, including layered modules, dependency hotspots, and structural risks.

Route this through the `tracedecay:exploring-code` skill for structure, and `tracedecay:code-health` for dependency hotspots and structural risk.

- **Scope:** the whole repo, or a specific directory if one is named.
- Follow those skills' read-only workflow and guardrails.

Output: a layered module map, dependency hotspots/violations, and a prioritized risk list.
