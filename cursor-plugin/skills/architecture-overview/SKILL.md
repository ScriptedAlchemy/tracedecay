---
name: architecture-overview
description: Use when mapping repo or directory architecture, module structure, public APIs, dependency layers, circular dependencies, coupling hotspots, or onboarding; use tracedecay:code-health-report for quality scoring and tech-debt ranking.
---

# Architecture overview

This skill **maps structure**. Its companion `tracedecay:code-health-report` **scores quality** and owns the `tracedecay_health` weak-dimension drill-down ladder — don't duplicate that ladder here.

## Workflow

1. **Shape & size:** `tracedecay_status` (node/edge/file counts), `tracedecay_files` + `tracedecay_distribution` (what lives where).
2. **Public surface:** `tracedecay_module_api` per top-level directory.
3. **Dependency structure:** `tracedecay_dsm` (`format`: `stats`|`clusters`|`matrix` — clusters and layering violations), `tracedecay_coupling` (`fan_in`/`fan_out` hubs), `tracedecay_circular` (cycles), `tracedecay_dependency_depth` (fragile long chains).
4. **Quality triage (optional):** one `tracedecay_health` (`details: true`) call for the composite signal. To drill into weak dimensions (complexity, duplication, doc gaps, test risk, god files), hand off to `tracedecay:code-health-report` rather than re-running its ladder here.

## Guardrails

- All tools here are read-only and parallel-safe. This skill maps and explains; it does not edit and it does not rank tech debt.

## Output

- A layered module map, the dependency hotspots/violations, and a prioritized structural risk list.
- Pairs with the `docs-canvas` plugin (if installed) for a rendered overview.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
