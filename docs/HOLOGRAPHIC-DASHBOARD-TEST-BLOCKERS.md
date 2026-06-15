# Holographic dashboard test blockers

This follow-up keeps the existing pure-helper coverage and adds a DOM-capable harness for the highest-value interaction seams.

Harness added
- `dashboard/package.json`
  - `npm test` now runs both the existing `node --test` suites and a new `vitest` jsdom lane.
  - `npm run test:node` keeps the old pure-node suites intact.
  - `npm run test:dom` runs the jsdom/vitest coverage.
- `dashboard/vitest.config.mjs`
- `dashboard/test/vitest.setup.mjs`

Covered now
- Existing pure helper coverage remains in:
  - `dashboard/test/holographic-curation-helpers.test.mjs`
  - `dashboard/test/holographic-semantic-map-helpers.test.mjs`
- New extracted interaction/helper coverage:
  - `dashboard/holographic/src/semanticMap/transform.ts`
  - `dashboard/holographic/src/semanticMap/gestures.ts`
  - `dashboard/test/semantic-map-interactions.vitest.ts`
    - `zoomAt` cursor anchoring + clamp behavior
    - `fitToPlaced` framing + `FIT_MAX_ZOOM` cap
    - keyboard pan / zoom / reset / escape handling
    - pan drag math, box-select thresholding, and click-hit selection
  - `dashboard/holographic/src/curation/useCurationData.ts`
  - `dashboard/test/curation-data.vitest.tsx`
    - `preview()` activity-tab → plan-tab orchestration
    - `apply()` confirmation close + callback handoff
    - visibility-gated polling (`panelRef.offsetParent === null`)

Still intentionally deferred
- `AssociationGraph` callback-stability and render-count invariants around the memoized SVG subtree are still not covered. They need focused render instrumentation around `GraphNodes` / `GraphLabels` so we can assert pan/zoom does not re-render the memoized layers.
- `SemanticMap` and `CurationPanel` now have extracted logic coverage, but not a full mounted component test over the real SVG/panel markup. The risky logic is pinned; the final DOM composition/integration layer is still thinner than a full user-flow spec.

Why this boundary remains
- The extracted helpers/hooks lock down the perf-sensitive and sequencing-heavy logic without introducing brittle snapshots of the full rendered shells.
- `AssociationGraph` remains the one holographic interaction area that still needs a component-level render counter rather than more pure/helper coverage.
