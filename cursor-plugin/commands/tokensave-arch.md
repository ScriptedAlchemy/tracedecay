---
name: tokensave-arch
description: Generate a high-level architecture overview of the repo or a directory.
---

# /tokensave-arch

Apply the `tokensave:architecture-overview` skill to the whole repo, or to the directory in `$ARGUMENTS` if provided.

Steps:
1. `tokensave_status` + `tokensave_files` + `tokensave_distribution` for shape.
2. `tokensave_module_api` per top-level directory for the public surface.
3. `tokensave_dsm`, `tokensave_coupling`, `tokensave_dependency_depth`, `tokensave_circular` for dependency structure.
4. `tokensave_hotspots`, `tokensave_god_class`, `tokensave_largest`, `tokensave_gini`, and `tokensave_health` (`details: true`) for focal points and quality.
5. Read-only only — do not edit.

Output: a layered module map, dependency hotspots/violations, and a prioritized risk list. Report any `tokensave_metrics:` savings to the user.
