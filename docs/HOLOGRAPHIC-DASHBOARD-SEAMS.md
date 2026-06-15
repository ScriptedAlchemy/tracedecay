# Holographic Dashboard — Component Seam Map

Audit of `dashboard/holographic/src/` for complexity hotspots and practical split
points. Scope: `SemanticMap.tsx`, `CurationPanel.tsx`, `AssociationGraph.tsx`.
**No refactors were performed** — this is a map of seams, risk, extraction order,
and test targets for follow-up cards.

## Method & ground truth

- Read all three files in full + parent `HolographicMemoryPage.tsx` (the consumer)
  + the already-extracted support modules.
- LOC (current): `AssociationGraph.tsx` **607**, `SemanticMap.tsx` **1196**,
  `CurationPanel.tsx` **1340**.
- **Test coverage: none.** Zero `.test`/`.spec` files anywhere under `dashboard/`.
  `web/package.json` has **no `test` script and no vitest/jest/testing-library
  dependency**, and there is no test config at repo root or in `web/`. The
  holographic plugin builds via esbuild (`build.from-hermes.mjs`) against
  `web/node_modules` and has no `package.json` of its own. → Adding any test
  requires **introducing a runner (vitest) + a jsdom React testing setup from
  scratch** as a prerequisite task. Pure-helper extractions are testable with
  node-only vitest (no DOM) and are the right first target.
- AssociationGraph has already had the heavy lifting extracted into:
  `associationGraphTypes/Layout/Adjacency/Utils`, `graphHitTest`, `graphViewBox`,
  `GraphSvgLayers`, `NodeDetailPanel`. It is the **best-decomposed** of the three
  and the model to copy. SemanticMap and CurationPanel have had **none** of this.

Legend for risk: **L** low (pure/move-only, behavior-preserving), **M** medium
(touches effects/render order or imperative DOM), **H** high (subtle timing,
ref mirrors, async orchestration).

---

## 1. `AssociationGraph.tsx` (607) — risk: **L** (residual), already well-split

### Responsibilities still mixed in the component
- **Filter + legend toolbar JSX** (`~388–505`): degree slider, kind-toggle
  chips, edge legend, hidden-entities badge — a large presentational block with
  no hook dependencies beyond `setMinDegree`/`setHiddenKinds`. **Candidate:**
  `<GraphToolbar/>`.
- **Interaction controller** (`241–361`): `selectNode`, `queueHover`
  (rAF-batched hover), `handleHoverLeave`, `ensureNodeInView`, `focusNode`,
  `onNodeKeyDown` (full arrow-key/Home/Enter/Escape roving), `onNodeFocus`,
  `registerNodeEl`, `onSvgFocus/Blur`, `frameSelected`. Cohesive unit.
  **Candidate:** `useGraphInteraction(...)` hook returning the stable callbacks.
- **View lifecycle effects** (`155–202`): settle-tracking, edge-fade-in,
  filter-change reframe, roving-tabindex target selection. These are the
  genuinely subtle part (multiple `eslint-disable exhaustive-deps`).

### Risky prop/state flows
- Callback stability is load-bearing: `selectNode`/`queueHover`/`onNodeKeyDown`
  are `useCallback` with `[]`/stable deps and read live state **via refs**
  (`selectedIdRef`, `visibleNodesRef`) specifically to keep `GraphNodes`
  (`memo`) from re-rendering during pan/zoom. **Any refactor that re-introduces
  state into their dep arrays regresses pan performance.** This invariant must be
  preserved and is a prime test target.
- `setView` is mutated from 5 sources (wheel, pan, touch/pinch, `fitTo`,
  settle-tracking, filter-reframe). The `viewRef` mirror + `userInteractedRef`
  gate the layout-follow effect; getting the order wrong re-snaps the view under
  the user.

### Recommended seams
1. `<GraphToolbar/>` (toolbar+legend JSX) — **L**.
2. `useGraphInteraction()` hook — **M** (callback-stability invariant).
3. Leave the 4 view-lifecycle effects in place; they are hard to test in
   isolation and tightly coupled to `layout.frame`.

---

## 2. `SemanticMap.tsx` (1196) — risk: **M–H**, **least decomposed, highest ROI**

### Responsibilities mixed in one file/component
- **Pure helpers (no React)**, all trivially extractable + unit-testable:
  `buildGrid` (`60–69`), `findNearest` (`71–101`), `buildDensity` (`111–158`,
  incl. 3×3 box blur), `capturePointer`/`releasePointer` (`167–181`). Plus
  types/constants (`MapTransform`, `PlacedPoint`, `DragState`, `IDENTITY`,
  `MIN_ZOOM/MAX_ZOOM/FIT_MAX_ZOOM`, `GRID_CELL`, `HOVER_RADIUS`,
  `DENSITY_AUTO_THRESHOLD`). **Candidate:** `semanticMap/hitTest.ts` (grid +
  nearest), `semanticMap/density.ts`, `semanticMap/geometry.ts` (transforms +
  pointer capture). This alone removes ~250 lines and is **L** risk.
- **Transform/zoom/pan machinery** (`236–438`, `461–498`): `applyTransform`
  (imperative DOM write + deferred 90 ms state commit via `commitTimerRef` +
  `--hv-k` CSS-var invalidation), `screenToBase`, `localPos`, `zoomAt`,
  `fitToPlaced`. The perf comment at `229–235` explains why it's imperative.
  **Candidate:** `useSemanticTransform()` hook. **Risk M–H** — ref mirrors
  (`transformRef` mirrors `transform`) are a dual source of truth.
- **Pointer/keyboard gesture state machine** (`500–657`): `updateHover`,
  `onPointerDown/Move/Up` (pan vs box-select via `DragState`), `onKeyDown`
  (arrow pan, +/-/0 zoom, Escape clear). **Candidate:** `useSemanticGestures()`.
  **Risk M** — `dragRef` mirrors `drag` state explicitly because pointerdown/up
  can land in the same task before React commits (comment `214–217`).
- **Cross-view focus** (`442–457`): `appliedFocusTokenRef` "apply once per
  `token`" effect that selects/pins facts + `fitToPlaced`. Driven by parent via
  the `focus?: SemanticMapFocus` prop (`HolographicMemoryPage` `showPairOnMap`,
  `816–827`). **Risk M** — depends on `layout.byId` existing when the token
  fires; if data loads late the focus is silently dropped.
- **`SidePanel`** (`990–1196`, ~200 lines, already a sub-component in-file):
  virtualized fact list (`useVirtualList`), roving arrow-key selection, and its
  **own** async fetch (`getMemoryFact` to backfill truncated 200-char content).
  **Candidate:** own file `SemanticMapSidePanel.tsx`. **Risk L** (self-contained).
- **Render JSX** (`735–985`): toolbar (mode toggles, zoom, density, reset),
  legend, encoding chips, `<svg>` + layers, recovery overlay. **Candidate:**
  `<MapToolbar/>`, `<MapOverlay/>`.

### Risky prop/state flows
- **Render-phase state update** (`267–272`): `if (query !== lastQuery) {
  setLastQuery(query); setLoading(true); setError(""); setSelection(null); }`
  during render. This is React's "adjust state on prop change" pattern — works
  but fragile; the four setters fire as a derived-state side effect. Prefer
  moving into the existing `useEffect` fetch block (`274–292`) keyed on `query`.
  **Risk M.**
- **`applyTransform` deferred-commit timer** + `transformRef`/`commitTimerRef`/
  `rafRef` cleanup interplay (`255–260`, `473–498`). If cleanup ordering breaks,
  a stale timer can write an old transform to the DOM. Test target.
- `zoomAt` early-returns when clamped `k === prev.k` (`392–393`); `fitToPlaced`
  clamps to `FIT_MAX_ZOOM`. Boundary behavior worth pinning.

### Recommended seams (per concern)
1. `semanticMap/hitTest.ts` + `density.ts` + `geometry.ts` (pure) — **L**.
2. `SemanticMapSidePanel.tsx` (extract existing `SidePanel`) — **L**.
3. `<MapToolbar/>` — **L**.
4. `useSemanticTransform()` (transform/zoom/fit) — **M–H**.
5. `useSemanticGestures()` (pointer/keyboard) — **M**.
6. Move the `query`-change reset out of render into the fetch effect — **M**.

---

## 3. `CurationPanel.tsx` (1340) — risk: **M** (pure helpers trivial; data lifecycle subtle)

### Responsibilities mixed in one file
- **Pure helpers + constants** (`31–303`), ~270 lines, **100% unit-testable, L risk, biggest LOC win:**
  - constants `ACTION_GROUPS` (`31–62`), `DIAGNOSTIC_COUNT_KEYS` (`129–137`),
    `COUNT_LABELS` (`139–151`).
  - `describe` (`64–101`), `splitTags` (`103–108`), `diffTags` (`111–123`),
    `isBookkeepingTag` (`125–127`), `countLabel` (`153–155`), `actionRisk`
    (`157–168`), `riskClass` (`170–181`), `groupActions` (`183–191`),
    `formatCounts` (`193–196`), `metadataValue` (`217–221`), `activityTone`
    (`223–234`), `formatActivityTime` (`236–246`), `activityStatus` (`248–256`),
    `activityStatusClass` (`258–269`), `formatHistoryTime` (`276–287`),
    `formatOplogTime` (`290–293`), `oplogDetailSummary` (`296–303`).
  - **Candidate:** `curation/format.ts` (text/count/time) + `curation/risk.ts`
    (actionRisk/riskClass/groupActions/ACTION_GROUPS).
- **Presentational sub-components** (already self-contained, in-file) → own files
  (**L**): `MetadataRow` (`198–215`), `TagBucket` (`376–409`), `ActionRow`
  (`411–570`, ~160 lines, heavy per-op branching — the biggest leaf),
  `ActionGroup` (`572–632`), `ActivityScroller` (`305–374`), `InlineConfirm`
  (`640–692`).
- **Tab render branches** (`975–1307`) → per-tab components (**L–M**): plan
  branch (`975–1062`), activity (already delegated to `ActivityScroller`), and
  the **history branch (`1100–1306`, ~200 lines of `MetadataRow` calls)** →
  `<CuratorHistoryTab/>` + `<CuratorConfigTable/>`.
- **Data lifecycle / orchestration** (`694–891`, the real risk): 18 `useState`,
  3 refs, **7 `useEffect`**, plus imperative `preview()`/`apply()`. **Candidate:**
  `useCurationData()` hook. **Risk M–H.**

### Risky prop/state flows
- **`preview()` / `apply()`** (`784–828`) are imperative orchestration:
  `setActiveTab("activity")` → fire `loadActivity` → `postMemoryCurate` → in
  `.then` set `report` + `savedAt` + clear stale flags → `loadSavedPreview` +
  `loadActivity` + `loadStatus` → `setActiveTab("plan")`. The double tab switch
  (activity-while-running, then plan-on-done) and the multi-setter sequencing
  are subtle; only `loading`/`applying` are reset in `finally`. **Risk M–H.**
- **Polling effect** (`875–884`): interval is `900 ms` while `loading||applying`
  else `2500 ms`, and is **suspended when `panelRef.offsetParent === null`**
  (panel hidden by `display:none` in the keep-mounted shell). Correctness depends
  on the visibility check; a regression would silently poll a hidden tab.
- **Cross-effect coupling**: `loadActivity` re-runs `loadSavedPreview` when the
  latest `finish` event was a `dry_run` (`766–772`); the 4 tab-lazy-load effects
  (`851–873`) each guard on different state. Easy to introduce a duplicate fetch
  or miss a cleanup.
- `loadSavedPreview` closes over `loading`/`applying` (in its dep array) so it is
  recreated per run — fine, but it means `previewSavedAtRef` is the real
  source of truth for dedup, not React state.

### Recommended seams
1. `curation/format.ts` + `curation/risk.ts` (pure) — **L**, do first.
2. Extract leaf components to `curation/` files — **L**.
3. `<CuratorHistoryTab/>` (+ `<CuratorConfigTable/>`) — **L**.
4. `useCurationData()` hook (effects + preview/apply + polling) — **M–H**, last.

---

## Suggested extraction order (lowest risk → highest)

| # | Extraction | File(s) | Risk | Why this order |
|---|---|---|---|---|
| 1 | Pure helpers + constants → `curation/format.ts`, `curation/risk.ts` | CurationPanel | **L** | Biggest LOC win, zero React, immediately unit-testable |
| 2 | `semanticMap/hitTest.ts` + `density.ts` + `geometry.ts` | SemanticMap | **L** | Same; unblocks hook extraction later |
| 3 | Leaf presentational components → own files (`ActionRow`, `ActionGroup`, `TagBucket`, `InlineConfirm`, `ActivityScroller`, `MetadataRow`; `SemanticMapSidePanel.tsx`) | both | **L** | Self-contained, move-only |
| 4 | `<GraphToolbar/>`, `<MapToolbar/>`, `<CuratorHistoryTab/>` + `<CuratorConfigTable/>` | all three | **L** | Pure JSX hoist |
| 5 | SemanticMap: move `query`-change reset out of render into the fetch effect | SemanticMap | **M** | Removes the render-phase setState before touching gestures |
| 6 | `useSemanticTransform()` / `useSemanticGestures()` | SemanticMap | **M–H** | Preserves imperative-perf + ref-mirror invariants |
| 7 | `useGraphInteraction()` hook | AssociationGraph | **M** | Must preserve callback-stability invariant |
| 8 | `useCurationData()` hook (effects + preview/apply + polling) | CurationPanel | **M–H** | Most subtle; do last with tests in place |

AssociationGraph needs the **least** work — its items (1–2 of its seams) are
optional polish. SemanticMap and CurationPanel are where the real debt is.

---

## Behaviors that should be covered by tests

> **Prerequisite:** add vitest (+ @testing-library/react + jsdom) wired to
> `web/node_modules`. Steps 1–2 below need node-only vitest (no DOM); the rest
> need the React setup. No existing tests to preserve — all are net-new.

### Pure helpers (fast, node-only, do alongside extractions 1–2)
- **CurationPanel `curation/risk.ts`**: `actionRisk` maps every op → expected
  tier (delete/merge/reflect = high; entity_merge/recategorize = medium;
  retag/entity_prune/entity_classify = low; unknown = review); `groupActions`
  puts each op in the right bucket and sends unknowns to "other"; `riskClass`
  returns the right class per tier.
- **`describe`**: one case per `op` (merge/entity_merge/entity_prune/
  entity_classify/delete/retag/recategorize/reflect) incl. `similarity`,
  `duplicate_of`, `supersedes`, and name-vs-id fallback branches.
- **`diffTags` / `splitTags` / `isBookkeepingTag`**: kept/removed/added buckets;
  empty string, whitespace, duplicate, and `cat:`/`target:` bookkeeping cases.
- **`formatHistoryTime`/`formatOplogTime`/`formatActivityTime`**: valid ISO,
  unix-seconds, and unparseable-input fallback paths.
- **SemanticMap `hitTest.ts`**: `buildGrid` cells contain the placed points;
  `findNearest` returns the closest point within radius and `null` outside it,
  including across cell boundaries.
- **SemanticMap `density.ts`**: `buildDensity` returns `[]` for empty input,
  respects the `DENSITY_AUTO_THRESHOLD`, and opacities are capped at 0.34.

### Component / interaction behaviors (need jsdom + testing-library)
- **SemanticMap transform**: `zoomAt` clamps to `[MIN_ZOOM, MAX_ZOOM]` and
  early-returns when already at the clamp; `fitToPlaced` clamps to
  `FIT_MAX_ZOOM`; `screenToBase` is the exact inverse of the applied transform.
- **SemanticMap cross-view focus**: given a `focus` with a new `token`, it
  selects the ids, pins `pinId` (or first id), and fits the view **once**;
  repeating the same `token` is a no-op; a `token` that fires before data loads
  is re-applied (document the current drop-vs-queue behavior and pin it).
- **SemanticMap gestures**: pan drag (moved=false → click pins hovered point;
  moved=true → translates); box-select (tiny rect < 4 px clears selection;
  larger rect selects enclosed facts); keyboard arrows pan, +/- zoom, 0 resets,
  Escape clears selection + pin.
- **SemanticMap `query` prop change** (after extraction 5): changing `query`
  sets loading, clears error + selection, and triggers exactly one fetch with
  cancellation of the prior in-flight request.
- **AssociationGraph interaction invariant**: panning/zooming does **not**
  re-render `GraphNodes` (callback-stability) — assert via render-count spy.
- **AssociationGraph keyboard**: arrow keys move to `nearestInDirection`,
  Enter/Space selects, Escape clears selection, Home goes to highest-degree
  node; filtered-out nodes are never the roving target.
- **AssociationGraph view lifecycle**: during settling the view tracks the
  layout bounds; once the user interacts it stops tracking; changing
  min-degree/kind after settle animates a reframe.
- **CurationPanel preview/apply orchestration**: `preview()` flips to activity
  tab, then back to plan on success, sets `report`+`savedAt`, clears stale flag,
  and resets `loading` in `finally`; failure sets `error` and still resets
  `loading`. `apply()` requires a non-empty dry-run plan (Apply disabled
  otherwise), opens confirm, and on confirm clears `previewSavedAt` + calls
  `onApplied`.
- **CurationPanel polling**: the interval uses 900 ms while loading/applying and
  2500 ms otherwise, and **does not poll while `panelRef.offsetParent === null`**
  (hidden panel).
- **CurationPanel activity coupling**: when the latest `finish` event is a
  `dry_run`, `loadActivity` triggers a `loadSavedPreview` refresh.
- **CurationPanel tab lazy-load**: switching to history loads status + oplog
  once; switching to activity loads activity once; plan tab refreshes the saved
  preview.

---

## Notes for follow-up cards

- **No large refactors in this task** — confirmed; only this report was written.
- Each numbered extraction above is a natural standalone card (the board already
  has child tasks for the area). Sequence per the table; gate the **M–H** hook
  extractions (6–8) on the pure-helper tests (1–2) landing first so the
  behavior-preserving splits have a safety net.
- The `applyTransform` imperative-commit pattern (SemanticMap) and the
  callback-stability invariant (AssociationGraph) are the two places most likely
  to silently regress under refactor — call them out in each extraction card's
  acceptance criteria.
