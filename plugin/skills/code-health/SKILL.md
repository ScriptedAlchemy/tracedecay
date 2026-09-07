---
name: code-health
description: 'An overall quality, architecture, coupling, duplication, or test-risk read of a project or directory. Covers the health dimensions to start from, treating scores as leads and confirming redundancy findings, and generation-bound before/after deltas. Daemon or registry failures are operational diagnostics.'
---

# Code health

Start with the requested project's health dimensions, then investigate the weak
ones: cycles and depth for architecture, modularity and coupling for boundaries,
redundancy for duplicate implementations, and test risk for poorly covered hubs.
A score is a lead, not a finding; inspect the implicated code and its callers.

Check indexed coverage before comparing scores. Unmounted files may look healthy
in the graph while no build root reaches them; inspect the real build or runtime
mount before calling them live or deleting them. Structural test links do not
prove execution coverage.

Use `redundancy` for body similarity. `definite` is consolidation evidence;
verify `likely`, especially vector-only matches. `naming_only` and `similar`
identify names, not interchangeable implementations. Scope expensive duplicate
analysis to the area under review; cached results still need their scope checked.

For before/after analysis, retain the generation- and path-bound `health_delta`
cursor. Compare the same scope and explain changed coverage rather than treating
a score increase alone as proof of better architecture.
