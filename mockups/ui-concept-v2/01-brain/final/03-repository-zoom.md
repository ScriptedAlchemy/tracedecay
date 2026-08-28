---
design_status: current
evidence_class: concept_synthetic
---

# Repository zoom

## User job

Understand how projects, linked worktrees, checkouts, and their repository body
relate before moving into a scoped project or another evidence workspace.

## Product behavior

- Zooming the selected project expands its repository hub and checkout/worktree
  relations while keeping a minimap and breadcrumb back to the registry.
- The hub is a relation/identity node, never another holdings-sized project.
  Project-body mass remains based on holdings; repository and checkout glyphs
  use fixed categorical sizes.
- Solid paths are exact registered relations. Dashed inferred paths, ambiguous
  candidates, stale registrations, missing repositories, and non-git projects
  retain distinct labels and line styles.
- Semantic zoom moves from repository bundle to named worktrees/checkouts; it
  does not expose thousands of labels at once or use force layout that changes
  causal meaning between visits.

## Interaction and fallback

Pointer-centered zoom, `-`/`+`/`Fit`, mouse/trackpad pan, keyboard traversal,
and breadcrumb navigation operate on the same deterministic coordinates.
Selecting a project scopes Brain; selecting a repository relation can open its
exact table row or the corresponding Delivery/Code evidence route when that
route exists. The accessible tree/table lists every project, repository,
worktree, relation grade, and route.

## Acceptance gates

Collapsed bundles expose counts with unambiguous semantics. Reduced motion uses
instant camera states. At 200% browser zoom the canvas receives a dedicated
focus aperture and all controls remain DOM text. Dense repositories virtualize
their exact fallback and aggregate scene geometry deterministically.

## Production authorities

The project registry owns project identity. Repository, branch, checkout, and
linked-worktree authorities own the displayed relations; path resemblance does
not become an exact edge. Renderer and route targets are listed in
[`README.md`](README.md).
