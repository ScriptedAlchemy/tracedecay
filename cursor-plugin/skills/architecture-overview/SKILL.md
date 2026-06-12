---
name: architecture-overview
description: Map the structure of the repo or a directory — modules, public surfaces, dependencies, layering, and coupling hotspots. Use for onboarding, "explain the architecture", "how is this organized", "draw me the module map", "find circular dependencies", or "what are the most coupled files". For quality scoring and tech-debt ranking, use code-health-report instead.
---

# Architecture overview

This skill **maps structure**. Its companion `tokensave:code-health-report` **scores quality** and owns the `tokensave_health` weak-dimension drill-down ladder — don't duplicate that ladder here.

## Workflow

1. **Shape & size:** `tokensave_status` (node/edge/file counts), `tokensave_files` + `tokensave_distribution` (what lives where).
2. **Public surface:** `tokensave_module_api` per top-level directory.
3. **Dependency structure:** `tokensave_dsm` (`format`: `stats`|`clusters`|`matrix` — clusters and layering violations), `tokensave_coupling` (`fan_in`/`fan_out` hubs), `tokensave_circular` (cycles), `tokensave_dependency_depth` (fragile long chains).
4. **Quality triage (optional):** one `tokensave_health` (`details: true`) call for the composite signal. To drill into weak dimensions (complexity, duplication, doc gaps, test risk, god files), hand off to `tokensave:code-health-report` rather than re-running its ladder here.

## Guardrails

- All tools here are read-only and parallel-safe. This skill maps and explains; it does not edit and it does not rank tech debt.

## Output

- A layered module map, the dependency hotspots/violations, and a prioritized structural risk list.
- Pairs with the `docs-canvas` plugin (if installed) for a rendered overview.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
