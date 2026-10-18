---
name: code-health
description: Use when the user asks for a quality, architecture, coupling, or complexity read on a project or directory, or where its structural risks are. Load this skill before the first tool call.
metadata:
  opencode/slash: "true"
---

# Code health

Start with the requested project's health dimensions, then investigate the weak
ones: cycles and depth for architecture, modularity and coupling for boundaries,
and test risk for poorly covered hubs. A score is a lead, not a finding; inspect
the implicated code and its callers. Skip the dimensions the panel lists under
`no evidence:`; the scope gave them nothing to measure, and `quality_signal`
does not summarize such a scope.

Check indexed coverage before comparing scores. Unmounted files may look healthy
in the graph while no build root reaches them; inspect the real build or runtime
mount before calling them live or deleting them. Structural test links do not
prove execution coverage.

For before/after analysis, retain the generation- and path-bound `health_delta`
cursor. Compare the same scope and explain changed coverage rather than treating
a score increase alone as proof of better architecture. Ignore dimensions whose
status is `no_evidence`. A `definition-mismatch` or `definition-unrecorded`
refusal means the cursor predates the current health definition: call again
without `before_cursor` to record a new baseline.

**Report:** the weak dimensions, each with the ranked code that makes it weak,
and the dimensions the panel listed under `no evidence:`. Do not report a score
as a finding.
