# TraceDecay UI concept V2

Jayse Hansen / Cantina Avengers FUI grammar applied to TraceDecay's fourteen
dashboard workspaces. Borrow the grammar—night glass, hairline frames, amber
attention, cyan signal, and measured fields—without copying Marvel marks.

These are synthetic lookbook stills, not runtime evidence or visual-audit
goldens. Do not use them as `dashboard/audit-baselines/`.

## Shared authorities

- [DESIGN-SYSTEM.md](DESIGN-SYSTEM.md) owns the logo, visual tokens, shell
  dimensions, color meaning, typography, and prompt floor.
- [NAVIGATION.md](NAVIGATION.md) owns the fourteen workspace names, numbers,
  order, routes, and persistent shell behavior.
- [INTERACTION-STATES.md](INTERACTION-STATES.md) owns the semantic-state and
  interaction coverage expected for each workspace.
- Each `<NN>-<workspace>/README.md` owns that workspace's explicit semantic-
  state manifest and asset history.
- Each PNG's exact same-stem Markdown file owns that plate's intent, entry
  condition, visible state, interactions, truth boundary, and history.

The more specific authority may add workspace detail but may not contradict a
shared authority. When a proposed plate exposes behavior the shipping product
does not, label that path unavailable or omit the control; a concept image
cannot supply the missing integration.

## Layout and lifecycle

Keep every asset in its numbered workspace folder:

```text
mockups/ui-concept-v2/
  <NN>-<workspace>/
    README.md
    <image-stem>.png
    <image-stem>.md
```

Version prefixes record iteration order only. They do not declare an asset
current, make a later asset preferable, or supersede an earlier semantic state.
Do not flatten plates into the root, create a parallel `drafts/` tree, overwrite
an older image, or add Git symlinks.

Every screen README declares status explicitly in two coordinated sections:

1. **Canonical semantic-state matrix:** one row per required semantic state,
   linking the current same-stem explainer and naming its entry condition.
2. **Asset ledger:** every PNG in the folder exactly once, with an explicit
   `current`, `superseded`, or `rejected` lifecycle and a short reason.

An image is eligible for `current` only after both its PNG and exact same-stem
explainer exist. A state can retain its current plate while a later-numbered
experiment is rejected, and one workspace can have several current plates when
they represent different semantic states.

## Contribution rules

1. Start from the shared prompt floor and the workspace coverage in
   `INTERACTION-STATES.md`. Preserve the normalized shell and route order.
2. Add the PNG and `<image-stem>.md` together. The explainer must record intent,
   entry condition, visible state, supported interactions, production truth
   boundary, synthetic-data disclosure, and lifecycle history.
3. Update the screen README in the same change. Link the new explainer from the
   canonical matrix only if it is the chosen plate for that semantic state, and
   add the PNG exactly once to the asset ledger.
4. When replacing a state, mark the prior asset `superseded`; when an image
   invents behavior or fails the visual brief, mark it `rejected` and say why.
   Never derive either result from its filename or version number.
5. Visibly stamp fixture-based final plates `CONCEPT / SYNTHETIC`. Do not claim
   counts, topology, freshness, health, controls, or success that no named
   production authority supplies.
