---
name: architecture-overview
description: Produce a high-level architecture map of the repo or a directory — modules, dependencies, layering, coupling, and hotspots. Use for onboarding, "explain the architecture", "give me a structural overview", or structural review.
---

# Architecture overview

## Workflow

1. **Shape & size:** `tokensave_status` (node/edge/file counts), `tokensave_files` + `tokensave_distribution` (what lives where).
2. **Public surface:** `tokensave_module_api` per top-level directory.
3. **Triage → `tokensave_health`** (`details: true`): one 0–10000 signal plus the 5-dimension breakdown (acyclicity, depth, equality, redundancy, modularity). Let the weak dimensions decide which drill-downs to run — don't run them all by reflex.
4. **Drill into weak dimensions:** acyclicity → `tokensave_circular` (cycles); modularity → `tokensave_dsm` (clusters, layering violations) + `tokensave_coupling` (`fan_in`/`fan_out`); depth → `tokensave_dependency_depth` (fragile long chains); equality → `tokensave_gini` + `tokensave_largest` (god files). Add `tokensave_hotspots` / `tokensave_god_class` for the worst offenders.

## Guardrails

- All tools here are read-only and parallel-safe. Several recompute structure `tokensave_health` already scores, so use `health` before deeper drill-downs and go only where it's weak. This skill maps and explains; it does not edit. For a quality scorecard (complexity, duplication, doc gaps, risk-weighted test gaps, session deltas), use `tokensave:code-health-report`.

## Output

- A layered module map, the dependency hotspots/violations, and a prioritized risk list.
- Pairs with the `docs-canvas` plugin (if installed) for a rendered overview.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
