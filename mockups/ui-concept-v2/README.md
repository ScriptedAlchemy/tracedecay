# TraceDecay UI concept V2

Jayse Hansen / Cantina Avengers FUI grammar applied to TraceDecay's fourteen
dashboard workspaces. Borrow the grammar—night glass, hairline frames, amber
attention, cyan signal, and measured fields—without copying Marvel marks.

These are implementation-facing concept plates, not runtime receipts or
visual-audit goldens. Sample data is synthetic unless a same-stem brief names a
reviewed real-evidence packet and binds each visible claim to its exact source.
Do not use these images as `dashboard/audit-baselines/`.

## Shared authorities

- [DESIGN-SYSTEM.md](DESIGN-SYSTEM.md) owns the logo, visual tokens, shell
  dimensions, color meaning, typography, and prompt floor.
- [NAVIGATION.md](NAVIGATION.md) owns the fourteen workspace names, numbers,
  order, routes, and persistent shell behavior.
- [INTERACTION-STATES.md](INTERACTION-STATES.md) owns the semantic-state and
  interaction coverage expected for each workspace.
- [IMPLEMENTATION.md](IMPLEMENTATION.md) owns the hybrid React/DOM and shared
  scene architecture, deterministic layout, density strategy, exact fallbacks,
  and renderer proof-of-capability decision.
- [CLEANUP.md](CLEANUP.md) records the exact retained authority and removed
  superseded/rejected concept-only assets.
- [GALLERY.md](GALLERY.md) renders every authoritative final plate in one
  reviewable sequence.
- Each `<NN>-<workspace>/final/README.md` is that workspace's authoritative
  final state manifest and shared product/interaction contract.
- Each final PNG's exact same-stem Markdown file owns that state's user job,
  behavior, evidence boundary, acceptance gates, and production authorities.
- Each `<NN>-<workspace>/README.md` routes to the final manifest and names the
  workspace's production authorities.

The more specific authority may add workspace detail but may not contradict a
shared authority. When a proposed plate exposes behavior the shipping product
does not, label that path unavailable or omit the control; a concept image
cannot supply the missing integration.

## Layout and lifecycle

Keep every asset in its numbered workspace folder. Reviewed implementation
references live only in `final/`; rejected and superseded studies are removed
from the branch tip after replacement acceptance and remain recoverable in Git
history:

```text
mockups/ui-concept-v2/
  <NN>-<workspace>/
    README.md
    final/
      README.md
      <state-stem>.png
      <state-stem>.md
```

`final/README.md` is the only authoritative state manifest. Do not flatten
plates into the root, overwrite an accepted image without its brief and
manifest update, or add Git symlinks.

Each workspace's `final/README.md` lists every approved state, image, same-stem
product brief, and review status. [CLEANUP.md](CLEANUP.md) is the branch-tip
deletion receipt for superseded and rejected studies.

A final image is eligible for `current` only after its PNG, exact same-stem
brief, and `final/README.md` manifest entry exist and the plate has been
visually reviewed. One workspace may have several final plates when each
represents a distinct semantic or interaction state.

## Contribution rules

1. Start from the shared prompt floor and the workspace coverage in
   `INTERACTION-STATES.md`. Preserve the normalized shell and route order.
2. Add the final PNG and `<state-stem>.md` together. The brief must record user
   job, product behavior, interaction and evidence contract, production truth
   boundary, acceptance gates, and named authorities.
3. Update `final/README.md` and the parent README in the same change. A final
   state must appear exactly once in the final manifest.
4. When replacing a state, mark the prior asset `superseded`; when an image
   invents behavior or fails the visual brief, mark it `rejected` and say why.
   After the replacement is accepted, remove the rejected/superseded pair from
   the branch tip and update `CLEANUP.md`. Never derive lifecycle from a
   filename or version number.
5. Visibly stamp synthetic plates `CONCEPT / SYNTHETIC DATA`. A reviewed real-
   evidence plate must label its evidence packet and source classes; never use
   unreviewed customer/operator data or claim counts, topology, freshness,
   health, controls, or success that no named production authority supplies.
6. Every final state includes keyboard, reduced-motion, 200%-zoom/reflow,
   dense-real-data, and exact text/table/transcript fallback gates.
