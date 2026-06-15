# Code Graph Explorer — Component Seam Map

Audit of `dashboard/graph/src/CodeGraphExplorer.tsx` and `GraphCanvas.tsx` for
component complexity hotspots and practical split points. Scope: graph rendering,
layout, interaction handlers, filtering/search/state coordination, and
canvas-specific concerns. **No refactors were performed** — this is a map of
seams, risk, extraction order, and test targets for follow-up cards.

## Method & ground truth

- Read both target files in full + the support modules they consume
  (`types.ts`, `api.ts`, `simulation.ts`, `labelLayout.ts`, `defaultView.ts`,
  `OverviewPanel.tsx`, `entry.tsx`) + `dashboard/lib/sequence.ts` (the
  stale-response guard primitive) + the consumer shell SDK surface
  (`lib/sdk`, `lib/format`).
- LOC (current): `CodeGraphExplorer.tsx` **662** (24.7 KB), `GraphCanvas.tsx`
  **609** (22.6 KB).
- **Test coverage: partial, and the harness already exists.** Unlike the
  holographic plugin, the graph plugin *does* have tests: `dashboard/test/
  graph-logic.test.mjs` — **10 tests, all passing** (`node --test`, verified
  this run). It exercises the two already-extracted pure modules:
  - `defaultView.ts` (view limits, all `canvasEmptyMessage` branches)
  - `labelLayout.ts` (cap-for-area, priority vs degree, sticky bypass, cap bound)
  The harness bundles TS → ESM via esbuild (`test/helpers/module-loader.mjs`)
  and runs pure node `test:` — **no DOM, no vitest, no React-testing-library
  required for pure-helper tests.** `playwright` is already a devDependency, so
  canvas/interaction tests are feasible later without new infra. Caveat:
  `dashboard/package.json` has **no `test` npm script** (only `build`/`smoke`);
  tests are invoked directly as `node --test test/*.test.mjs` from `dashboard/`.
- Decomposition status: `CodeGraphExplorer.tsx` has **two presentational
  sub-components already in-file** (`Legend`, `DetailPanel`) but keeps **all
  state, async orchestration, and the entire render tree** in the one
  component. `GraphCanvas.tsx` has **module-level pure helpers already
  factored** (`withAlpha`, `edgeStyle`, `fitCameraToNodes`, `readTheme`) but
  the **entire rendering + interaction core is one ~370-line `useEffect`** —
  the single biggest hotspot in the area.

Legend for risk: **L** low (pure/move-only, behavior-preserving), **M** medium
(touches effects/render order, imperative DOM, or ref mirrors), **H** high
(subtle timing, async orchestration, rAF/capture/cleanup interplay).

---

## 1. `CodeGraphExplorer.tsx` (662) — risk: **M** (orchestration; pure wins available)

### Responsibilities mixed in one file/component
- **Pure helpers (no React)**, trivially extractable + unit-testable on the
  existing harness:
  - `edgeKey` (`27–29`). **Candidate:** `graphExplorer/graphState.ts`.
  - `toggleSet` (`421–426`).
  - The **filter computation** in the `visible` useMemo (`393–408`): node filter
    by `kindFamily` / `languageForPath` / `dirScope` prefix, then edges kept only
    when both endpoints survive — the orphan-edge culling is the load-bearing
    detail. **Candidate:** `applyFilters(nodes, edges, filters)`.
  - `chipOptions` useMemo (`411–419`): families/languages present in the loaded
    set. **Candidate:** `deriveChipOptions(nodes)`.
  - `mergeGraph` (`177–188`) splits into pure `mergeNodesInto(prev, add)` /
    `mergeEdgesInto(prev, add)` (spread-merge per id / per `edgeKey`). These
    are the core of the **progressive-exploration** behavior.
  - All pure → `graphExplorer/graphState.ts` + `graphExplorer/filters.ts`.
  **L risk**, and the highest-value first move: none of this is currently
  tested.
- **In-file presentational components** (already self-contained) → own files
  (**L**, move-only): `Legend` (`38–56`), `DetailPanel` (`58–130`, the
  Inspector — node metadata + caller/callee lists + edge-kind badges).
- **Search subsystem** (`342–390` + JSX `475–494`): `onQueryChange` (180 ms
  debounce + `searchSeq` stale-drop), `onSearchKeyDown` (Enter/↑/↓/Escape
  dropdown nav), the dropdown JSX. **Candidate:** `<SearchBox/>` (owns query,
  results, open state) or a `useGraphSearch()` hook. **Risk M** — the
  stale-response sequencing is load-bearing (see below).
- **Path-finder mode** (`158–161`, `272–292`, `294–311`, controls JSX
  `540–573`): `pathMode`/`pathFrom`/`pathTo`/`pathResult` + `runPath` + the
  path-pick coordination inside `selectInCanvas` (`298–308`, "first pick or
  re-pick after a completed run starts a new path"). **Candidate:**
  `usePathFinder({ mergeGraph, focusNode })` hook. **Risk M** — couples into
  `selectInCanvas`.
- **Graph accumulation + default-view lifecycle** (`144–150`, `167–206`,
  `215–251`, `439–453`): `graphNodes`/`graphEdges` maps, `defaultViewRef` +
  `defaultSeq`, `loadDefaultView`, `clearCanvas`, `expandNode` (with
  `CANVAS_NODE_CAP` gate), `showDirectedNeighbors`. **Candidate:**
  `useCanvasGraph()` hook. **Risk M–H** — owns the "search replaces the
  pristine default view" invariant.
- **Inspection** (`148–149`, `256–270`): `selected`/`neighbors` + `inspectSeq` +
  `inspect` (parallel `api.node` + `api.neighbors`). **Candidate:**
  `useInspection()` hook. **Risk M** — sequencing invariant.
- **Render JSX** (`455–661`): toolbar (kicker + Overview/Canvas tabs), totals,
  error banner, view switch, canvas controls (breadcrumbs + Find-path/Fit/Clear),
  chip-bar, canvas-layout (shell + footer + DetailPanel). **Candidate:**
  `<Toolbar/>`, `<FilterChipBar/>` (`576–618`), `<CanvasControls/>` (`517–574`).

### Risky prop/state flows
- **The default-view-replacement invariant** (`167–171`, `318–329`):
  `defaultViewRef` is `true` only while the canvas shows the untouched seedless
  default slice. The first `focusSymbol` from search, when this flag is set,
  *clears* `graphNodes`/`graphEdges`, resets path state, and calls
  `defaultSeq.invalidate()` to abort the in-flight default load — so the
  focused neighborhood replaces rather than merges into the hub slice. Once the
  user has built a custom exploration, focusing merges in as before. **A
  regression here either leaves stale hub-slice nodes stuck in a focused view,
  or wipes an in-progress exploration on the next focus.** Prime test target.
- **Three independent stale-response guards** (`searchSeq`, `inspectSeq`,
  `defaultSeq`, all via `makeSequence` from `lib/sequence.ts`):
  - `onQueryChange` (`382–388`): drops a search response if a newer query fired.
  - `inspect` (`258–268`): drops a stale neighbors/detail pair so the Inspector
    and "Show callers/callees" actions never repoint at the wrong node.
  - `loadDefaultView` (`192–204`): drops a default slice if the user took over
    the canvas (`defaultSeq.invalidate()` in `focusSymbol`).
  Each `seq.isCurrent(ticket)` check is load-bearing; the `makeSequence`
  contract (`next`/`isCurrent`/`invalidate`) is the shared primitive.
- **`expandNode` cap** (`233–236`): gates on `graphNodes.size >=
  CANVAS_NODE_CAP` (600) and surfaces an error rather than expanding. The check
  is correct only because `graphNodes.size` is in the `useCallback` dep array
  (`250`), so the callback is recreated as size changes — worth pinning with a
  test.
- **`clearCanvas` reset ordering** (`215–229`): resets graph + path + *filters*
  before re-loading the default view. The comment (`213–214`) calls out that
  stale filter chips would otherwise hide the reloaded default slice and leave
  the canvas looking broken. Order-sensitive.
- **`selectInCanvas` path-mode branch** (`298–308`): "if `!pathFrom || pathTo`"
  treats a completed run as a fresh start — subtle, easy to break the
  start-then-target semantics.

### Recommended seams (per concern)
1. `graphExplorer/graphState.ts` (`edgeKey`, `mergeNodesInto`, `mergeEdgesInto`)
   + `graphExplorer/filters.ts` (`applyFilters`, `deriveChipOptions`,
   `toggleSet`) — **L**.
2. `Legend.tsx` + `DetailPanel.tsx` (extract the two in-file components) — **L**.
3. `<FilterChipBar/>` + `<CanvasControls/>` (pure JSX hoist) — **L**.
4. `<SearchBox/>` (or `useGraphSearch()`) — **M** (preserve the `searchSeq`
   stale-drop + 180 ms debounce).
5. `useInspection()` — **M** (preserve the `inspectSeq` invariant).
6. `usePathFinder()` — **M** (preserve the start/re-pick branch in
   `selectInCanvas`).
7. `useCanvasGraph()` (accumulation + default-view lifecycle) — **M–H**; the
   default-view-replacement invariant is the hardest part. Do last with the
   pure-helper tests (1) in place.

---

## 2. `GraphCanvas.tsx` (609) — risk: **H** (imperative Canvas2D; one giant effect)

### Responsibilities mixed in one file/component
- **Pure helpers + constants (no React, no DOM)**, extractable + testable on
  the existing harness:
  - `withAlpha` (`67–73`): `#rrggbb` → `rgba()`; non-hex passthrough.
  - `edgeStyle` (`112–119`) + `EDGE_STYLE_DEFS`/`DEFAULT_EDGE_STYLE_DEF`
    (`40–63`) + `DIM_ALPHA` (`65`): per-kind accent/alpha/dash/width, default
    fallback. **Candidate:** `graphCanvas/style.ts`. **L** — fully pure given a
    `theme` arg.
  - `fitCameraToNodes` (`126–162`): bounding-box framing + zoom clamp
    `[0.12, 5]` + `0.88` padding + smooth (lerp `0.2`) vs snap. Pure math given
    `(size, sim, camera)`. **Candidate:** `graphCanvas/camera.ts`. **L** — prime
    test target.
- **Near-pure closures** (currently inner functions of the big effect, only read
  refs) → hoist to pure functions taking explicit args:
  - `toWorld` (`241–248`): screen→world given camera + rect. **L**.
  - `hitTest` (`250–267`): nearest node within `radius + 6/k`. **L–M**.
  - `neighborhood` (`269–278`): id set of a node's direct neighbors. **L**.
  - The **cursor-anchored zoom** math in `onWheel` (`487–493`): keep the world
    point under the cursor fixed across a `k` change. Extractable as
    `zoomAtPoint(camera, screen, k)`. **L–M**.
  - The **label-box builder** inside `render` (`400–435`): builds `LabelBox[]`
    from `(sim, camera, props, size)` before handing to `selectLabels`. Pure
    given inputs — **Candidate:** `graphCanvas/labels.ts::buildLabelBoxes`. **L**
    and immediately testable (it is what feeds the already-tested
    `labelLayout.ts`).
  - **Candidate home:** `graphCanvas/camera.ts`, `graphCanvas/hitTest.ts`,
    `graphCanvas/labels.ts`.
- **The render pass** `render()` (`298–450`, ~150 lines): full Canvas2D draw —
  DPR/resize, camera transform, edges (+path highlight +dim), nodes
  (+selection/hover/path ring +collapsed-neighbor `+N` badge), label placement
  via `selectLabels`. Reads refs but every input could be passed as args.
  **Candidate:** `graphCanvas/render.ts::render(ctx, state)`. **Risk M** — large
  imperative draw; hard to unit-test directly (needs a canvas), but extracting
  isolates it and lets the *pure* label-box builder above be tested.
- **Interaction handlers** (`482–564`): `onWheel` (cursor-anchored zoom, clears
  follow flags), `onPointerDown`/`Move`/`Up` (node-drag vs pan vs click, with
  the `moved > 4 px` click/drag threshold + pointer capture),
  `onDoubleClick` (expand), `onPointerLeave` (clear hover). **Candidate:**
  `useCanvasInteraction(canvas, simRef, cameraRef, propsRef, refs…)`. **Risk
  M–H** — the `propsRef` stable-handler pattern is load-bearing (see below).
- **Animation loop** `frame()` (`452–480`): rAF loop, **idle while hidden**
  (`canvas.offsetParent` guard), sim `tick`, focus-follow lerp (`0.18`),
  fit-follow (`fitCameraToNodes` smooth). Coupled to the follow state machine.
  Keep inside the interaction hook or a `useCanvasAnimation()`. **Risk M**.
- **Observers / theme** (`566–586`): `ResizeObserver` (refresh `sizeRef`),
  `MutationObserver` on `document.documentElement[data-theme]` (invalidate
  `themeRef`). Micro-effects. **Risk L–M**.

### Risky prop/state flows
- **The `propsRef` stable-handler invariant** (`188–189`):
  `propsRef.current = { selectedId, pathIds, focusId, onSelect, onExpand }` is
  reassigned on every render, so the imperative event handlers (bound once
  inside the big effect) read *live* props without rebinding listeners. This is
  the canvas analogue of the AssociationGraph callback-stability invariant from
  `HOLOGRAPHIC-DASHBOARD-SEAMS.md`. **The big `useEffect` deliberately has `[]`
  deps (`606`); any refactor that re-introduces props into its dep array
  re-runs setup, rebinds listeners, and restarts rAF — thrash.** Must be
  preserved and is the #1 thing a test should pin.
- **Camera follow state machine** (`followIdRef` + `followFitRef`,
  `200–203`): three mutually-exclusive modes — **focus-follow** (camera lerps
  to `focusId` while layout settles), **fit-follow** (keep whole graph framed
  while it spreads), and **manual** (user in control). Wheel (`484`) and
  pointer-down (`504–505`) clear the follow flags; `fitSignal` sets fit-follow
  (`230–231`); focus sets focus-follow (`212–213`). Getting the clear-order
  wrong re-snaps the camera under the user mid-pan. Prime interaction test
  target.
- **Render throttle + idle-while-hidden** (`needsRenderRef` + `456`):
  `render()` runs only when the sim is active or `needsRenderRef` is set, and
  the whole loop early-returns while `canvas.offsetParent` is null (shell keeps
  visited tabs mounted). **A regression in the hidden-guard burns CPU at 60 fps
  on a background tab.** Test target.
- **DPR / resize drift check** (`303–306`): backing store resized only when
  `round(w*dpr)`/`round(h*dpr)` drift. Stale `sizeRef` after a fast resize =
  blurry or clipped canvas.
- **Theme invalidation** (`579–586`): `themeRef` cached and only re-read when
  `<html data-theme>` flips (canvas2D can't resolve `var()`, so accents are
  sampled here). A regression leaves wrong accent colors after a theme flip.
- **Click/drag threshold** (`545–546`): a pointer-up is a *click* (→ `onSelect`)
  only when it moved `< 4 px`. The `node.fx/fy` pin (`513–514`) and release
  (`548–549`) drive the force reheat. Boundary worth pinning.

### Recommended seams (per concern)
1. `graphCanvas/style.ts` (`withAlpha`, `edgeStyle`, `EDGE_STYLE_DEFS`,
   `DIM_ALPHA`) — **L**.
2. `graphCanvas/camera.ts` (`fitCameraToNodes`, `zoomAtPoint`, `clampZoom`,
   `toWorld`) — **L–M**.
3. `graphCanvas/hitTest.ts` (`hitTest`, `neighborhood`) — **L–M**.
4. `graphCanvas/labels.ts` (`buildLabelBoxes`) — **L** (feeds already-tested
   `selectLabels`).
5. `graphCanvas/render.ts` (`render`) — **M** (isolate; canvas-mocked or
   visually tested).
6. `useCanvasInteraction()` (handlers + animation loop + observers, preserving
   `propsRef` + `[]` deps) — **M–H**. Extract last, after 1–4 have tests.

---

## Suggested extraction order (lowest risk → highest)

| # | Extraction | File(s) | Risk | Why this order |
|---|---|---|---|---|
| 1 | Pure graph/filter helpers → `graphExplorer/graphState.ts`, `graphExplorer/filters.ts` | CodeGraphExplorer | **L** | Zero React, immediately testable on existing harness; unblocks the M–H hooks |
| 2 | Pure style/camera/hitTest/labels → `graphCanvas/style.ts`, `camera.ts`, `hitTest.ts`, `labels.ts` | GraphCanvas | **L–M** | Same; removes ~120 lines of pure math from the imperative file |
| 3 | Extract in-file components → `Legend.tsx`, `DetailPanel.tsx` | CodeGraphExplorer | **L** | Self-contained, move-only |
| 4 | `<FilterChipBar/>`, `<CanvasControls/>` | CodeGraphExplorer | **L** | Pure JSX hoist |
| 5 | `types.ts` pure-fn coverage (`languageForPath`, `kindFamily`, `colorForKind`) + `simulation.ts` coverage | — | **L** | Net-new tests for already-pure code; safety net before the hooks |
| 6 | `<SearchBox/>` / `useGraphSearch()` | CodeGraphExplorer | **M** | Preserve `searchSeq` stale-drop + 180 ms debounce |
| 7 | `useInspection()` + `usePathFinder()` | CodeGraphExplorer | **M** | Preserve the `inspectSeq` invariant + path start/re-pick branch |
| 8 | `graphCanvas/render.ts` | GraphCanvas | **M** | Isolate the draw; gate on labels.ts test |
| 9 | `useCanvasGraph()` (accumulation + default-view lifecycle) | CodeGraphExplorer | **M–H** | Owns the default-view-replacement invariant; do with tests in place |
| 10 | `useCanvasInteraction()` (handlers + rAF + observers) | GraphCanvas | **M–H** | Must preserve `propsRef`/`[]`-dep + follow state machine; do last |

`CodeGraphExplorer.tsx` carries more *breadth* of debt (orchestration across
many concerns); `GraphCanvas.tsx` carries more *depth* (one ~370-line effect).
Both have cheap pure-helper wins (1–2) that should land first.

---

## Behaviors that should be covered by tests

> **Prerequisite is already met** for pure-helper tests: the harness
> (`node --test` + esbuild bundling via `test/helpers/module-loader.mjs`) exists
> and passes. Steps 1–6 below are **node-only, no DOM** — same shape as the
> existing `graph-logic.test.mjs`. Interaction/canvas behaviors (7–8) need a
> real canvas + pointer events → **playwright** (already a devDependency) is the
> right tool; jsdom can't drive Canvas2D. Consider adding a `"test"` npm script
> to `dashboard/package.json` to wire `node --test test/*.test.mjs` into `build`/CI.

### Pure helpers (fast, node-only, do alongside extractions 1–2 + step 5)
- **`types.ts` (currently untested):** `languageForPath` over every extension in
  the table (`rs`→rust … `css`→web) + unknown/no-dot → `"other"`/`"unknown"`;
  `kindFamily` maps representative kinds to each family (`fn`/`type`/`trait`/
  `module`/`value`/`impl`/`other`); `colorForKind` resolves to the family color.
- **`simulation.ts` (currently untested):** `nodeRadius` is monotonic in degree
  and bounded `[7, 20]`; `createSimulation` **preserves positions of previously-
  seen nodes** (prevById reuse) and seeds new nodes around the anchor; new nodes
  placed near `anchorId` when given; `visibleDegree` counts match the edge set;
  `tick` cools `alpha` toward `ALPHA_MIN`; `isActive()` flips false once cooled;
  `reheat(target)` raises alpha but never above target.
- **`graphExplorer/filters.ts` (after extraction):** `applyFilters` hides nodes
  failing kind/lang/dir filters, **drops edges whose endpoints don't both
  survive** (orphan-edge culling), and is a no-op when all filter sets are empty;
  `deriveChipOptions` returns sorted unique families/languages actually present;
  `toggleSet` adds/removes and returns a new `Set`.
- **`graphExplorer/graphState.ts` (after extraction):** `edgeKey` is stable per
  `source>target:kind`; `mergeNodesInto` spreads new over existing (newer fields
  win, existing fields preserved); `mergeEdgesInto` dedupes by `edgeKey`.
- **`graphCanvas/style.ts` (after extraction):** `withAlpha` on valid
  `#rrggbb`, on shorthand/invalid (passthrough), on uppercase hex;
  `edgeStyle` returns each known kind's def and falls back to
  `DEFAULT_EDGE_STYLE_DEF` for an unknown kind.
- **`graphCanvas/camera.ts` (after extraction):** `fitCameraToNodes` clamps `k`
  to `[0.12, 5]`, applies `0.88` padding, is a no-op on empty sim, and in
  `smooth` mode lerps camera toward the target by `0.2` (vs snap when false);
  `zoomAtPoint` keeps the world point under the cursor fixed; `clampZoom`
  respects `[0.12, 5]`.
- **`graphCanvas/hitTest.ts` + `labels.ts` (after extraction):** `hitTest`
  returns the nearest node within `radius + 6/k` and `null` outside it;
  `neighborhood` returns the node plus both-endpoint neighbors;
  `buildLabelBoxes` excludes off-screen nodes, assigns priority
  `hovered < selected < path < highlight/focus < rest`, and marks hovered/
  selected sticky.

### Component / interaction behaviors (need playwright; canvas + pointer events)
- **`CodeGraphExplorer` default-view replacement:** with the pristine default
  slice loaded (`defaultViewRef` true), the first `focusSymbol` clears the
  accumulated nodes/edges and invalidates the in-flight default load; after a
  custom exploration exists, a new focus *merges* instead. Assert via a mocked
  `api` + visible-node counts.
- **Stale-response sequencing:** fire two searches quickly; only the latest
  result populates the dropdown. Same for `inspect` (rapid node clicks never
  repoint the Inspector at a stale node) and the default load (taking over the
  canvas drops the in-flight default response).
- **`expandNode` cap:** at `graphNodes.size >= 600` it surfaces the cap error
  and does not call `api.subgraph`; below the cap it expands and merges.
- **`clearCanvas`:** resets graph + path + filters, then reloads the default
  view — assert filters are cleared *before* the reload so the default slice is
  visible.
- **Path-finder:** entering path mode resets `pathFrom/To/Result`; selecting a
  first node sets `pathFrom`; a second distinct node sets `pathTo` and fires
  `runPath`; after a completed run, the next selection starts a new path.
- **`GraphCanvas` cursor-anchored zoom:** a wheel-up zoom keeps the world point
  under the cursor fixed (screen coords of a node don't drift); zoom clamps at
  `0.12`/`5`.
- **`GraphCanvas` gesture discrimination:** pointer-down on a node + move `< 4
  px` + up → `onSelect` fires (click); move `>= 4 px` → node drags (`fx/fy`
  pinned), no `onSelect`; pointer-down on empty space + move → pans; double-
  click on a node → `onExpand`.
- **`GraphCanvas` follow state machine:** `fitSignal` frames all nodes and keeps
  framing while settling; any wheel/pointer-down stops follow; `focusId` lerps
  the camera to the node while the layout is active and stops on manual
  interaction.
- **`GraphCanvas` idle-while-hidden:** while `canvas.offsetParent === null` the
  rAF loop does not `tick`/`render` (no CPU burn on a hidden tab); it resumes on
  the first visible frame.
- **`GraphCanvas` theme invalidation:** flipping `<html data-theme>` causes the
  next frame to re-sample theme tokens (accent colors update); no flip → cached
  tokens reused.

---

## Notes for follow-up cards

- **No large refactors in this task** — confirmed; only this report was written.
- Each numbered extraction is a natural standalone card. Sequence per the table;
  gate the **M–H** hook extractions (7–10) on the pure-helper tests (1–2, 5)
  landing first so the behavior-preserving splits have a safety net.
- The **`propsRef` stable-handler invariant** (GraphCanvas big effect, `[]`
  deps) and the **default-view-replacement invariant** (CodeGraphExplorer
  `defaultViewRef`/`defaultSeq`) are the two places most likely to silently
  regress under refactor — call them out in each extraction card's acceptance
  criteria.
- **Three quick wins need no extraction at all** — `types.ts` and
  `simulation.ts` are already pure and currently untested; adding coverage is a
  standalone **L** card with zero source changes (step 5 in the table).
- Consider wiring a `"test"` script in `dashboard/package.json`
  (`node --test test/*.test.mjs`) so the existing 10 tests (and any new ones)
  are gated in `build`/CI rather than invoked manually.
