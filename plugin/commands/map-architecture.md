---
description: Map repo or directory architecture, including layered modules, dependency hotspots, and structural risks.
argument-hint: "[path]"
---

# Map architecture

Map the architecture of the whole repo, or `$ARGUMENTS` if a directory was given. Read-only.

1. Shape & size: `tracedecay_status` (node/edge/file counts), `tracedecay_files` + `tracedecay_distribution` (what lives where).
2. Public surface: `tracedecay_module_api` per top-level directory.
3. Dependency structure: `tracedecay_dsm` (clusters and layering violations), `tracedecay_coupling` (`fan_in`/`fan_out` hubs), `tracedecay_circular` (cycles), `tracedecay_dependency_depth` (fragile long chains).

This reports and prioritizes; it does not edit.

Output: a layered module map, dependency hotspots/violations, and a prioritized risk list. If any result includes a `tracedecay_metrics:` line, report the savings.
