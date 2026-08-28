---
design_status: current
evidence_class: concept_synthetic
---

# Brain final state set

This folder is the authoritative implementation reference for Brain. Brain is
the all-project spatial registry: project bodies expose holdings and recency,
repository hubs expose shared checkout structure, and admitted activity may
temporarily connect the exact identities it touched. The neural material is
the information architecture, never decorative firing or invented health.

All identities and values pictured here are concept/synthetic. Production must
bind every label, size, position, edge, and pulse to the evidence ladder in
[`DESIGN-SYSTEM.md`](../../DESIGN-SYSTEM.md). Brain never exposes or
reconstructs private reasoning.

## State manifest

| State | Image | Product brief | Status |
|---|---|---|---|
| Registry overview | [01-registry-overview.png](01-registry-overview.png) | [01-registry-overview.md](01-registry-overview.md) | approved |
| Project hover | [02-project-hover.png](02-project-hover.png) | [02-project-hover.md](02-project-hover.md) | approved |
| Repository zoom | [03-repository-zoom.png](03-repository-zoom.png) | [03-repository-zoom.md](03-repository-zoom.md) | approved |
| Project scoped | [04-project-scoped.png](04-project-scoped.png) | [04-project-scoped.md](04-project-scoped.md) | approved |
| Admitted activity becomes synapse | [05-admitted-activity-synapse.png](05-admitted-activity-synapse.png) | [05-admitted-activity-synapse.md](05-admitted-activity-synapse.md) | approved |

## Shared interaction contract

- Project-body area encodes indexed holdings or another explicitly named mass
  measure; it never encodes popularity, importance, health, or activity.
- Horizontal placement encodes measured recency. Unknown or stale timestamps
  remain typed unknown/stale states instead of receiving fabricated positions.
- Hover inspects without changing scope or firing activity. Click/Enter scopes
  only through the production selection route; Escape or the scope control
  returns to the registry.
- Repository zoom follows an evidenced project-to-repository or checkout
  relation. Inferred or ambiguous relations use their own line treatment and
  remain labelled.
- An activity synapse exists only for admitted activity with exact touched
  identities. The path may traverse only evidenced hops. Heat decays while the
  underlying graph remains stable; idle nodes do not shimmer or pulse.
- Source-private chain-of-thought is never a node, edge, field, or tooltip.
  Visible persisted messages or reasoning summaries may appear only with their
  source class and evidence grade.

## Browser and accessibility contract

- React/DOM owns the shell, scope, filters, legends, inspector, exact table,
  keyboard controls, and accessible names. The shared scene runtime owns dense
  bodies, paths, picking, spatial zoom, and reduced-motion-safe heat states.
- Every scene operation has a keyboard equivalent and a visible focus state.
  The exact project/repository/activity table provides selection, sorting,
  source identity, evidence grade, and the same drill-through routes.
- At 200% zoom, the shell and inspector reflow while the scene enters a larger
  focus aperture; labels and controls are never raster-scaled into illegibility.
- Reduced motion removes path travel, camera interpolation, bloom animation,
  and breathing. Static position, line style, heat, glyph, and text retain the
  full meaning.
- Dense real registries use deterministic clustering and semantic zoom. The
  scene never creates one DOM node per project, and the exact table remains
  virtualized and searchable.

## Production authorities

- The project registry owns project identity, enrollment, recency, holdings,
  and linked-worktree membership.
- Repository and checkout authorities own project-to-repository relations;
  Brain does not infer shared storage from path resemblance alone.
- The admitted dashboard activity projection owns event identity, family,
  timestamp, exact touched project, and supported propagation relation.
- `dashboard/src/workspaces/brain/BrainPage.tsx` and
  `dashboard/src/workspaces/brain/ScopedBrain.tsx` are the V2 route and scope
  composition targets. `dashboard/src/viz/graph/GraphCanvas.propagation.dom.test.tsx`
  names the existing propagation behavior that the shared high-fidelity scene
  layer must preserve or replace with equivalent production evidence.
- [`IMPLEMENTATION.md`](../../IMPLEMENTATION.md) owns the hybrid DOM/scene
  architecture and renderer proof-of-capability decision.
