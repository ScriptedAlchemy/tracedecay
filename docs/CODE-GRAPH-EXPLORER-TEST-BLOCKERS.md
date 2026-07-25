# Code Graph Explorer test blockers

This follow-up keeps the existing pure-node graph coverage, adds a jsdom/vitest lane for React interaction hooks, and extends the canvas helper seam coverage.

Harness added
- `dashboard/package.json`
  - `npm test` now runs both the existing `node --test` lane and the new `vitest` jsdom lane.
  - `npm run test:dom` runs the new React/jsdom coverage.
- `dashboard/vitest.config.mjs`
- `dashboard/test/vitest.setup.mjs`
- `dashboard/smoke.mjs` now accepts real device profiles via `--profiles=` (`desktop`, `narrow`, `iphone12`, `pixel5`, `ipadmini`).
- `npm run smoke:mobile` drives the smoke harness with Playwright mobile device descriptors.

Covered now
- Existing pure helper coverage remains in:
  - `dashboard/test/graph-explorer-state.test.mjs`
  - `dashboard/test/graph-canvas-logic.test.mjs`
- New extracted interaction/helper coverage:
  - `dashboard/graph/src/useGraphSearch.ts`
  - `dashboard/graph/src/useGraphInspection.ts`
  - `dashboard/test/code-graph-explorer-hooks.vitest.tsx`
    - debounced async search
    - stale-response dropping for rapid queries
    - keyboard navigation / Enter selection in the search popover
    - stale inspector response dropping for rapid node inspection
  - `dashboard/graph/src/canvasHelpers.ts`
  - `dashboard/test/graph-canvas-logic.test.mjs`
    - `toWorldPoint`
    - `zoomCameraAtPoint`
    - `hitTestNode`
    - `neighborhoodIds`

Still intentionally deferred
1. `GraphCanvas.tsx` full real-canvas pointer loop
   - The extracted helper math is covered, but we still do not have an end-to-end assertion for drag vs click discrimination, double-click expand, follow-state cancellation, or hidden-tab render throttling on a live canvas.
   - `npm run smoke` and `npm run smoke:mobile` now pass locally, but that smoke coverage still stops short of the full live-canvas pointer loop.
   - The preferred next step is a Playwright spec/smoke extension that exercises those pointer-loop behaviors directly.

2. Hermetic Playwright smoke is now a green regression lane
   - Verified current state: `npm run smoke` and `npm run smoke:mobile` both pass locally.
   - `npm run smoke:mobile` currently covers the real Playwright phone descriptors `iphone12,pixel5`.
   - `ipadmini` support is available in `dashboard/smoke.mjs` via `--profiles=ipadmini`, but it is not part of the default npm script today.

3. Holographic AssociationGraph live-behavior coverage is still intentionally deferred
   - The smoke harness proves the dashboards boot and core flows render across desktop, narrow, and phone profiles, but it still does not assert AssociationGraph render-count/callback-stability behavior under live pan/zoom interaction.
   - Treat that as a remaining non-blocking gap alongside the missing full GraphCanvas pointer-loop e2e assertion.

Why this boundary remains
- The new hook/helper seams cover the highest-risk async sequencing and canvas math without forcing brittle jsdom-canvas assertions.
- The remaining gaps are specifically deeper live-interaction assertions (GraphCanvas pointer loop and AssociationGraph callback-stability/render behavior), not dashboard startup or harness health.
