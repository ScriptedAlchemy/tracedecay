---
name: architecture-overview
description: Produce a high-level architecture map of the repo or a directory — modules, dependencies, layering, coupling, and hotspots. Use for onboarding, "explain the architecture", "give me a structural overview", or structural review.
disable-model-invocation: true
---

# Architecture overview

## Workflow

1. **Shape & size:** `tokensave_status` (node/edge/file counts), `tokensave_files` + `tokensave_distribution` (what lives where).
2. **Public surface:** `tokensave_module_api` per top-level directory.
3. **Dependency structure:** `tokensave_dsm` (clusters, density, layering violations), `tokensave_coupling` (`fan_in`/`fan_out`), `tokensave_dependency_depth` (fragile long chains), `tokensave_circular` (cycles).
4. **Focal points:** `tokensave_hotspots`, `tokensave_god_class`, `tokensave_largest`, `tokensave_gini` (inequality / god files).
5. **Health → `tokensave_health`** (`details: true`) for the 5-dimension breakdown (acyclicity, depth, equality, redundancy, modularity).

## Guardrails

- All tools here are read-only and parallel-safe. This skill maps and explains; it does not edit.

## Output

- A layered module map, the dependency hotspots/violations, and a prioritized risk list.
- Pairs with the `docs-canvas` plugin for a rendered overview.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
