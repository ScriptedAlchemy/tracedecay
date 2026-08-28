---
design_status: current
evidence_class: concept_synthetic
---

# Project hover

## User job

Inspect one project in place before deciding whether to scope into it, without
losing the all-project field or confusing inspection with activity.

## Product behavior

- Pointer hover or keyboard focus raises one project body and opens a DOM
  inspector with stable project identity, path/registration state, indexed
  holdings, recency source, repository links, and evidence grades.
- Unrelated bodies dim only enough to establish focus; their position, size,
  status, and labels do not change.
- Hover never fires a synapse, changes the selected project, mutates scope,
  reports health, or silently follows a repository relation.
- Enter/Space selects the focused body; Escape dismisses the inspection. Touch
  and switch input use the same explicit focus/select sequence.

## Evidence and fallback

The inspector distinguishes project-registry facts from repository relations
and activity summaries. Missing, ambiguous, stale, and unavailable fields stay
typed. Persisted messages may be linked as source artifacts, but private
reasoning is not shown or reconstructed. The exact registry table exposes the
same focus and select actions without hover.

## Acceptance gates

Focus has a non-color 2px outline and accessible name. Reduced motion removes
the raise/halo transition. At 200% zoom the inspector becomes a reflowed region
or focus mode rather than covering the selected body. Scene picking and DOM
focus must remain synchronized for dense registries.

## Production authorities

The project registry supplies all inspector facts; repository and activity
authorities are separately labelled. The DOM/scene and route boundaries are
defined by [`README.md`](README.md) and
[`IMPLEMENTATION.md`](../../IMPLEMENTATION.md).
