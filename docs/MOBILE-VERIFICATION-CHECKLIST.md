# Mobile Visual Verification Checklist — Dashboards

A worker-runnable checklist for visually verifying the **Code Graph**, **Holographic
Memory**, and **LCM** dashboards (plus the shared shell that hosts them) on mobile
viewports. It is grounded in the two component seam audits this work was derived
from, and pins each check to the real route, component, CSS seam, or risky
invariant it covers.

Companion docs (read these for the "why" behind the regression-risk checks):

- `docs/HOLOGRAPHIC-DASHBOARD-SEAMS.md` — `SemanticMap.tsx`, `AssociationGraph.tsx`,
  `CurationPanel.tsx` seams, risk ratings, and the load-bearing invariants.
- `docs/CODE-GRAPH-EXPLORER-SEAMS.md` — `CodeGraphExplorer.tsx` + `GraphCanvas.tsx`
  seams, and the canvas interaction/`propsRef` invariants.
- `docs/dashboard.md`, `docs/dashboard-port-handoff.md` — how the dashboard is
  served and launched.
- `dashboard/smoke.mjs` — the existing playwright smoke harness this checklist
  extends (see "How to run" below).

Scope notes:

- **In scope:** the standalone `tracedecay dashboard` shell and the three named
  plugin tabs. These same bundles power the Hermes-hosted variants
  (`/tracedecay` wrapper tab + the `/holographic-memory` and `/lcm` tabs), so a
  pass here covers both surfaces.
- **Out of scope (but same shell):** the **Savings & Cost** tab. It shares the
  shell header/tab-bar behavior in §2 and has its own `@media (max-width: 560px)`
  breakpoint (`dashboard/savings/src/styles.css`); spot-check it only if a change
  touches it.

---

## 1. How to run (so a worker needs no extra context)

The dashboard is a React SPA served by the Rust `tracedecay dashboard` server. The
shell (`dashboard/shell/src/main.jsx`) fetches the plugin manifest list from
`/api/dashboard/plugins`, injects each plugin's CSS+JS, and renders registered
components behind a tab bar. Tabs deep-link via `?tab=<plugin>`; the Holographic
page additionally deep-links its sub-view via `?view=<key>`.

**Option A — hermetic, no setup (matches CI):** `dashboard/smoke.mjs` already
builds a throwaway indexed project, boots `cargo run -- dashboard --port 0`,
and drives it with headless Playwright. The default run still covers the legacy
desktop+narrow baseline, and the harness now also supports real device profiles.
Extend it rather than reinventing. From `dashboard/`:

```
node smoke.mjs                       # hermetic: spawns its own server
npm run smoke:mobile                 # hermetic: iPhone 12 + Pixel 5
node smoke.mjs --url=http://127.0.0.1:7341/   # point at a running server
node smoke.mjs --profiles=iphone12,pixel5,ipadmini
```

**Option B — against this repo's real index (more realistic data):**

```
cargo run -- dashboard --host 127.0.0.1 --port 7341
# then open in a browser, or point smoke.mjs at it with --url=
```

The dashboard refuses to start without a TraceDecay index; this repo already has
`.tracedecay/`, so Option B works out of the box here.

**Real device profiles are now wired into the harness.** The default
`node smoke.mjs` run still tests `{desktop 1280×900, narrow 420×900}` for
baseline parity. `npm run smoke:mobile` covers the real Playwright device
profiles `{iphone12, pixel5}`. The narrow width still approximates a phone but
is **not** a real mobile context — it lacks `isMobile`, `hasTouch`, device scale,
and the mobile UA. `ipadmini` is available via `--profiles=ipadmini`, but it is
not part of the default npm script today. For the checks below, use playwright
device descriptors with touch + mobile enabled:

```js
import { devices } from "playwright";
const iPhone12  = devices["iPhone 12"];       // 390×844, deviceScale 3, hasTouch, isMobile
const pixel5    = devices["Pixel 5"];         // 393×851, deviceScale 2.75, hasTouch, isMobile
const iPadMini  = devices["iPad Mini"];       // 768×1024, isMobile (touch)
// Or an explicit narrow context with touch on:
const ctx = await browser.newContext({ viewport:{width:360,height:740}, isMobile:true, hasTouch:true });
```

Each numbered check below has a binary pass/fail. Record results per device.
Screenshots: capture one per (check, device) on any failure — store under
`.tmp/mobile-qa/` (untracked) and reference the path in the report.

---

## 2. Device / viewport matrix

Test every dashboard at each of these. The breakpoints column is the
**existing** CSS seam the width is meant to exercise (so you know what is
supposed to change, vs. what is a genuine regression).

| Profile | Width×Height | Touch | Purpose / breakpoint exercised |
|---|---|---|---|
| iPhone 12 / 13 / 14 (SE-like) | 390×844 | yes | Small phone; below all plugin breakpoints |
| iPhone SE (2nd gen) | 375×667 | yes | Shortest common phone; tests vertical scroll + address-bar `vh` |
| Pixel 5 / 7 | 393×851 | yes | Android Chrome; bottom bar + overscroll |
| Galaxy S9+ / S20 | 360×740 | yes | Narrowest common phone; densest layout stress |
| iPad Mini (portrait) | 768×1024 | yes | Straddles holographic `md:`/`sm:` and LCM/graph/shell `760px` lines |
| iPad (landscape) | 1024×768 | yes | Holographic `lg:` kicks in; graph `1180px` canvas split |
| Desktop narrow (existing smoke) | 420×900 | no | Regression baseline — keep for parity with `smoke.mjs` |
| Desktop (existing smoke) | 1280×900 | no | Regression baseline |

Breakpoint reference (current code):

- **Shell:** `@media (max-width: 760px)` — header stacks vertically, project name
  hidden, **tab bar becomes horizontally scrollable** (`overflow-x:auto`),
  controls right-align (`dashboard/shell/src/styles.css:802`).
- **Code Graph:** `@media (max-width: 1180px)` → canvas layout `1fr`;
  `@media (max-width: 860px)` → toolbar `1fr`, canvas height
  `clamp(20rem,55vh,30rem)` (`dashboard/graph/src/styles.css:624,630`).
- **Holographic:** Tailwind-style `sm:`(640) / `md:`(768) / `lg:`(1024) /
  `xl:`(1280) + a custom `@media (max-width: 720px)`
  (`dashboard/holographic/src/styles.css:407,416,423,436,452`).
- **LCM:** `@media (max-width: 760px)` — top bar / heads / result foot /
  pager go columnar (`dashboard/lcm/src/style.css:1189`).

---

## 3. Global shell checks (apply to ALL three dashboards)

These are in the shell, so verify them once and then confirm they hold while
each plugin tab is active.

1. **Header reflows at ≤760px.** At a phone width the header is a vertical
   stack: brand on top, then the tab bar, then the controls. PASS = no header
   clipping, no horizontal page scrollbar caused by the header.
   (`shell/src/styles.css:802`, `main.jsx:374`)
2. **Tab bar is horizontally scrollable on phones** (hidden-scrollbar pattern),
   not wrapped and not clipped. All tabs (Holographic Memory, LCM, Code Graph,
   Savings & Cost, TraceDecay) are reachable by swiping the bar; the active tab
   scrolls into view. PASS = every tab is reachable; no tab is permanently
   off-screen. (`shell/src/styles.css` `.ts-shell-tabs { overflow-x:auto }`)
3. **`?tab=` deep-linking + back/forward** round-trips on mobile. Open
   `/?tab=hermes-lcm`, `/?tab=graph`, `/?tab=holographic`; confirm the right tab
   activates, then browser Back/Forward returns to the previous tab. PASS =
   active tab and URL never disagree. (`main.jsx:82-100,211-220`)
4. **Digit + arrow keyboard shortcuts do not fire inside plugin surfaces.**
   Shell switches tabs on digit keys 1–9 and arrow-keys on the tablist, but
   skips widgets that "own" their keyboard (`svg, [role=application],
   [role=listbox], input, textarea`). On a phone with a hardware/Bluetooth
   keyboard, focusing the semantic map / association graph / graph canvas / a
   fact list and pressing a digit must NOT yank tabs. (`main.jsx:309-329`)
5. **Visited panels stay mounted (hidden) — idle-while-hidden.** Switch away
   from the Code Graph canvas, then come back. The exploration must survive
   (nodes still present). PASS = canvas state preserved AND the hidden tab does
   not pin CPU (the `GraphCanvas` rAF loop early-returns while
   `canvas.offsetParent===null`). Regression target: §7 risk R-G2.
   (`main.jsx:469-489`; `CODE-GRAPH-EXPLORER-SEAMS.md` idle-while-hidden)
6. **Theme toggle (dark/light) works and persists.** Tap the ☾/☀ control on a
   phone; the whole shell + active plugin restyles immediately, and the choice
   survives reload (`localStorage` `td-theme`). For the Code Graph canvas
   specifically, confirm accent colors re-sample after the flip (the canvas
   caches theme tokens and re-reads only on `<html data-theme>` change).
   Regression target: §7 risk R-G3. (`main.jsx:228-239`; GraphCanvas theme
   invalidation seam)
7. **Sticky header does not overlap content** and the connection banner
   (`ts-disconnected-banner`, shown only when the server is unreachable) does
   not cover interactive controls. (`main.jsx:439-451`)
8. **Plugin ErrorBoundary isolates crashes.** Not easily triggered manually —
   skip unless you can force a plugin error; PASS = one tab crashing shows the
   "Plugin crashed / Retry" card, other tabs still work. (`main.jsx:106-136`)

---

## 4. Holographic Memory dashboard (`/holographic-memory`)

Route: `?tab=holographic` (manifest `tab.path: /holographic-memory`). Source:
`dashboard/holographic/src/`. The page is `HolographicMemoryPage.tsx`; it has a
5-way sub-view switch (`ViewKey`): **Inspector · Semantic Map · Graph ·
Similarity · Curation**, deep-linked via `?view=<key>` and driven by the
`.hv-viewswitch` tab bar with icons. Cross-view: a Similarity pair jumps to the
Semantic Map with a focus token (`showPairOnMap`).

### 4.1 Navigation & view switch
1. All five view tabs are reachable on a phone; the `.hv-viewswitch` bar wraps
   or scrolls without clipping (it is `flex … max-w-full`).
   (`HolographicMemoryPage.tsx:841-851`)
2. `?view=map|graph|similarity|curation` deep-links land on the right view;
   `?view=inspector` (or no param) lands on the overview. (`:795-812`)
3. **Cross-view focus (Similarity → Map):** tapping a similar pair on a phone
   switches to Semantic Map, pins the fact, and fits the view exactly once
   (repeating the same pair is a no-op). Regression target: §7 R-H1.
   (`showPairOnMap`, `:816-827`)

### 4.2 Inspector view (overview cards)
4. Stat strip, Facts list, Entities/Memory Banks lists, HRR coverage gauges,
   trust histogram, growth chart all render without horizontal overflow. PASS =
   no page-level horizontal scrollbar; long DB paths / names truncate
   (`min-w-0` + `truncate` everywhere).
5. **Scroll regions use bounded heights, not the full page.** Facts list is
   `max-h-[60vh] md:max-h-[38rem]`; Entities/Banks `max-h-[50vh]`. Confirm on a
   short phone (SE 667px) that these scroll internally and the page scrolls
   too — they must not fight each other. (`FactList :282`,
   `EntityAndBankLists :341,378`)
6. Brushable histogram + growth-chart daily/cumulative toggle are tappable
   (≥44px targets) and the brush drag does not pan the page.

### 4.3 Semantic Map view (holographic memory map — REQUIRED explicit checks)
Source: `SemanticMap.tsx` (SVG, `touch-action:none` on the map surface).
7. **Pan/zoom via touch does not scroll the page.** The map has
   `touch-action:none`; a one-finger drag pans the map, not the document, and
   the browser does not hijack it as pull-to-refresh. (`styles.css:652`)
8. **Pinch-to-zoom zooms the map** (clamp `[MIN_ZOOM, MAX_ZOOM]`) and the world
   point under the fingers stays put; zoom clamps at the bounds (no snap past).
9. **Tap vs drag discrimination:** a tap (move < ~4px) pins the hovered point;
   a drag translates. Box-select: a tiny rect (< 4px) clears selection; a larger
   rect selects enclosed facts.
10. **Cross-view focus fits once** (repeat of 4.1.3 but visually): the selected
    fact is centered/pinned, the view does not jitter or re-fit on every render.
    Regression target: §7 R-H2 (the `applyTransform` imperative-commit +
    ref-mirror invariant).
11. **Virtualized SidePanel** scrolls independently; arrow-key selection works
    with a BT keyboard; fetching truncated fact content (backfill) does not
    blank the list. (`SidePanel`, `useVirtualList`)
12. **Keyboard** (BT): arrows pan, `+`/`-` zoom, `0` reset, `Escape` clears
    selection+pin. (`SemanticMap.tsx` gesture/keyboard handlers)

### 4.4 Graph view (Association Graph)
Source: `AssociationGraph.tsx` (SVG force graph, `touch-action:none`).
13. **Pan/zoom/pinch on the graph does not scroll the page** (`touch-action:none`,
    `styles.css:673`); node tap selects, drag pans.
14. **Degree slider + kind-toggle chips + edge legend** are reachable and
    tappable on a phone; changing them re-frames the graph (filter-change
    reframe effect).
15. **Roving-tabindex keyboard nav** (BT keyboard): arrows move to
    `nearestInDirection`, Enter/Space selects, Escape clears, Home →
    highest-degree node; filtered-out nodes are never the roving target.
16. **Pan/zoom does not re-render nodes** (callback-stability invariant) —
    visually: panning/zooming is smooth, no flicker/stutter. Regression target:
    §7 R-H3. This is still a deferred/non-blocking manual verification target —
    the current smoke harness does not assert AssociationGraph render-count or
    callback-stability behavior automatically. (`AssociationGraph`
    callback-stability seam)
17. NodeDetailPanel renders caller/callee lists without horizontal overflow.

### 4.5 Similarity view
18. Pairs list renders; tapping a pair triggers §4.1.3 cross-view focus to Map.
    No horizontal overflow on pair rows.

### 4.6 Curation view
Source: `CurationPanel.tsx` (tabs: Plan / Activity / History; Preview/Apply
orchestration; polling).
19. **Plan / Activity / History tabs** are reachable on a phone; switching tabs
    lazy-loads the right data once each. PASS = no duplicate fetches, no blank
    tab.
20. **Preview button** (disabled until a dry-run plan exists) and **Apply**
    (confirm-gated) are tappable; Preview flips to Activity while running then
    back to Plan on completion. Regression target: §7 R-H4 (preview/apply
    multi-setter orchestration + double tab switch).
21. **Polling suspends while hidden.** The panel polls every 900ms while
    loading/applying, 2500ms otherwise, and **stops when the panel is hidden**
    (`panelRef.offsetParent===null`). Because visited shell tabs stay mounted,
    switching away from Holographic must pause this polling — confirm via
    network panel that requests stop on the background tab. Regression target:
    §7 R-H5. (`CurationPanel` polling seam)
22. Action rows (delete/merge/reflect risk tiers) wrap without clipping; risk
    badges and time formatters truncate long content.

---

## 5. Code Graph dashboard (graph tab)

Route: `?tab=graph` (manifest registers plugin name `graph`, label "Code Graph";
no explicit `tab.path`, appears as its own shell tab). Source:
`dashboard/graph/src/`. `CodeGraphExplorer.tsx` has an **Overview/Canvas** view
toggle (`<nav class="tsg-views">`), a debounced symbol search with a results
dropdown (`role="listbox"`), a filter chip-bar, canvas controls (breadcrumbs +
Find-path / Fit / Clear), and a detail Inspector. `GraphCanvas.tsx` is an
imperative Canvas2D surface with `touch-action:none`.

### 5.1 Navigation & layout
1. **Overview ↔ Canvas toggle** is reachable and tappable on a phone.
2. **Responsive split:** at `≤1180px` the canvas+detail layout collapses to a
   single column (`grid-template-columns:1fr`); at `≤860px` the toolbar stacks
   and the canvas shrinks to `clamp(20rem,55vh,30rem)`. Confirm the canvas
   remains visible/usable at 360–390px. (`graph/src/styles.css:624,630`)
3. **Auto-populated default slice:** on first visit the canvas self-populates
   with the seedless default slice (no search needed); the empty state is NOT
   shown. (The existing `smoke.mjs` asserts this — keep asserting on mobile.)

### 5.2 Search & filtering
4. **Search box** is tappable; the keyboard opens on focus without the dropdown
   being covered by it; the 180ms-debounced results dropdown (`role="listbox"`)
   is reachable and scrollable on a phone. (`CodeGraphExplorer.tsx:475-494`)
5. **Stale-response sequencing:** type fast / fire two searches quickly; only
   the latest result populates the dropdown. Regression target: §7 R-G1.
6. **Filter chip-bar** wraps without clipping; selecting a family/language chip
   hides non-matching nodes AND culls orphan edges (both endpoints must
   survive). Clearing all chips restores the full set. (`applyFilters` seam)
7. **Find-path mode:** entering path mode, tapping a first node then a second
   distinct node runs the path; after a completed run the next tap starts a new
   path. Reachable by touch on a phone.

### 5.3 Canvas interactions (graph explorer — REQUIRED explicit checks)
8. **`touch-action:none` is honored:** dragging on the canvas pans the graph,
   never the document; pinch zooms the graph; the page does not pull-to-refresh
   over the canvas. (`graph/src/styles.css:297`)
9. **Cursor/finger-anchored zoom:** pinch/wheel keeps the world point under the
   fingers fixed (a node under your finger stays there as you zoom); zoom clamps
   at `[0.12, 5]`. (`zoomAtPoint`/`clampZoom` seam)
10. **Tap vs drag vs double-tap discrimination:** tap a node (move < 4px) →
    selects + opens Inspector; drag a node → moves it (force reheat); drag empty
    space → pans; double-tap a node → expands its neighbors. Regression target:
    §7 R-G3 (gesture discrimination + the 4px threshold).
11. **Camera follow state machine:** Fit frames all nodes and keeps framing
    while the layout settles; the first manual pan/pinch stops follow (no
    mid-gesture re-snap to the auto-fit). Regression target: §7 R-G4.
12. **`expandNode` cap:** expanding toward `CANVAS_NODE_CAP` (600) surfaces the
    cap error instead of expanding further (visible error banner, not a silent
    stall).
13. **Clear** resets graph + path + filters, then reloads the default view with
    filters cleared *before* the reload (so the default slice is actually
    visible, not hidden by a stale chip). (`clearCanvas` seam)
14. **DPR/resize:** rotate the phone (portrait↔landscape) and confirm the canvas
    backing store resizes without going blurry/clipped (DPR/resize drift check).
15. **Inspector (DetailPanel)** caller/callee lists + edge-kind badges wrap and
    scroll without horizontal overflow.

---

## 6. LCM dashboard (`/lcm`)

Route: `?tab=hermes-lcm` (manifest `tab.path: /lcm`). Source:
`dashboard/lcm/src/index.js` (hand-written vanilla JS via
`React.createElement` — no build step). It has a search interface (search head,
result cards, pager), recent-sessions list, and a modal **Drawer** for
session/message/node detail (`role="dialog"`, `aria-modal`), plus
CompressionBars/TimelineChart/markdown rendering.

1. **Responsive column collapse at ≤760px:** the top bar, search/section/msg
   heads, and result foot stack vertically; the pager stretches full-width.
   (`lcm/src/style.css:1189`)
2. **Tables / wide rows scroll horizontally** (`overflow-x:auto`), they must not
   blow out the page width. WATCH: any element using `min-width:max-content`
   (`lcm/src/style.css:730`) is a likely horizontal-overflow source on a 360px
   phone — confirm it scrolls, not the page.
3. **Search:** the box is tappable; typing returns result cards; the pager is
   usable by touch; snippets highlight query terms and wrap without clipping.
4. **Drawer detail (mobile-critical):**
   - Opening a session/message/node slides the drawer in from the right; it
     covers `min(640px, 92vw)` × full height with a blurred overlay.
     (`hermes-lcm-drawer` CSS)
   - The overlay is tap-to-close; the ✕ button and Back arrow are ≥44px tappable;
     Escape (BT keyboard) closes.
   - **Focus management:** opening moves focus into the panel; closing restores
     focus to the opener (not `<body>`). Regression target: §7 R-L1.
   - **Body scroll lock:** while the drawer is open, the page behind it must not
     scroll when you drag inside the drawer — the drawer body scrolls instead.
   - The drawer must not be taller than the viewport with no way to reach its
     header/footer (test on SE 667px).
5. **Keyboard shortcuts** (BT): the page-level `onKeyDown` (and
   `isPanelHidden()` gating) must not fire while focus is in an input, the
   drawer, or a scroll region. (`lcm/src/index.js:1518-1561`)
6. **Recent Sessions / empty state:** both render correctly at phone widths; the
   empty-state orb + message are centered and not clipped. (The existing
   `smoke.mjs --expect-lcm=` asserts which state appears.)
7. **Compression bars / timeline chart** render without horizontal overflow and
   remain legible at 360px; long tool outputs use horizontal scroll where
   needed, not page overflow.

---

## 7. Visual-regression risk register (the load-bearing invariants)

These are the places most likely to silently break on mobile or under future
refactor, per the seam audits. Each has a check above (cross-referenced); treat
any failure here as a real regression, not a cosmetic nit.

- **R-G1 — Graph stale-response sequencing** (`searchSeq`/`inspectSeq`/`defaultSeq`
  via `lib/sequence.ts`). Check §5.5. *A break repoints the Inspector or dropdown
  at a stale node/result.* (`CODE-GRAPH-EXPLORER-SEAMS.md`)
- **R-G2 — GraphCanvas idle-while-hidden** (`canvas.offsetParent` guard + `[]`
  effect deps + `propsRef` stable handlers). Check §3.5. *A break burns CPU at
  60fps on a background tab and rebinds listeners on every render.*
- **R-G3 — GraphCanvas gesture discrimination + 4px click/drag threshold.** Check
  §5.3.10. *A break makes taps select-drag or pans instead of selecting.*
- **R-G4 — GraphCanvas camera follow state machine** (`followIdRef`/`followFitRef`,
  cleared on wheel/pointer-down). Check §5.3.11. *A break re-snaps the camera
  under the user mid-pan.*
- **R-H1 — SemanticMap cross-view focus (apply-once per token; late-data drop).**
  Check §4.1.3 / §4.3.10. *A break drops a focus that fires before data loads, or
  re-fits every render.* (`HOLOGRAPHIC-DASHBOARD-SEAMS.md`)
- **R-H2 — SemanticMap `applyTransform` imperative commit + ref mirrors**
  (`transformRef`/`commitTimerRef`/`rafRef` cleanup). Check §4.3.10. *A break
  writes a stale transform to the DOM.* Highest mobile risk because touch drives
  this path continuously.
- **R-H3 — AssociationGraph callback-stability invariant** (stable
  `useCallback`s read via refs so `GraphNodes` don't re-render during pan/zoom).
  Check §4.4.16. *A break regresses pan/zoom smoothness — the #1 mobile
  perceptual issue.*
- **R-H4 — CurationPanel preview()/apply() orchestration** (double tab switch +
  multi-setter sequencing). Check §4.6.20. *A break leaves the panel on the
  wrong tab or with stale flags after a run.*
- **R-H5 — CurationPanel polling-while-hidden** (`panelRef.offsetParent===null`).
  Check §4.6.21. *A break silently polls a hidden tab.* Especially important on
  mobile because the shell keeps visited tabs mounted.
- **R-L1 — LCM Drawer focus management + scroll lock.** Check §6.4. *A break
  drops focus to `<body>` on close, traps scroll to the background page, or
  strands the drawer footer off-screen on short phones.*

---

## 8. Touch-specific checks (cross-cutting)

Apply these across all three dashboards:

1. **`touch-action:none` surfaces never scroll the page** and never trigger
   browser pull-to-refresh / pinch-zoom-the-document. Surfaces: Code Graph
   canvas (`graph/src/styles.css:297`), Holographic Semantic Map
   (`styles.css:652`) and Association Graph (`styles.css:673`).
2. **Tap targets ≥ 44×44px** for all primary controls: shell tabs, theme toggle,
   view-switch tabs, chip toggles, Find-path/Fit/Clear, drawer ✕/Back, pager
   buttons, Preview/Apply.
3. **No accidental text selection / callout on long-press** over canvas/SVG
   interaction surfaces (the gesture handlers should prevent it; a regression
   shows the iOS callout menu mid-drag).
4. **No 300ms click delay / double-tap-to-zoom hijack** on interactive controls
   (the viewport meta + touch-action should prevent it).
5. **Orientation change** (portrait↔landscape) does not break layout, does not
   strand the sticky header over content, and canvas/SVG surfaces re-measure
   (ResizeObserver/SVG viewBox) without going blank or blurry.
6. **Overscroll behavior:** the page uses the browser's native scroll on lists;
   confirm overscroll does not reveal a white gap behind the dark shell, and
   that `overscroll-behavior` on inner scroll regions contains the bounce.

---

## 9. Overflow / scrolling checks (cross-cutting)

1. **No page-level horizontal scrollbar** on any dashboard at 360–414px widths.
   Known likely sources to scrutinize: LCM `min-width:max-content`
  (`lcm/src/style.css:730`); any `grid-cols-[fixed fixed …]` that doesn't collapse
   below the holographic `720px` breakpoint; wide tables in LCM (must use their
   `overflow-x:auto` containers).
2. **`vh` vs `dvh` on mobile:** the shell uses `min-height:100vh`
   (`shell/src/styles.css:149`) and Holographic uses
   `lg:max-h-[calc(100vh-2rem)]` and `max-h-[60vh]`/`max-h-[50vh]`. On mobile
   browsers the dynamic URL bar makes `100vh` taller than the visible area.
   PASS = no content is permanently hidden behind the address/bar, and no
   bottom-of-screen control is unreachable without scrolling. (If you find a
   real `vh`-caused occlusion, file it — `dvh`/`svh` is the fix; do not paper
   over it.)
3. **Inner scroll regions scroll, the page scrolls, and they don't fight:** Facts
   list, Entities/Banks, SemanticMap SidePanel, graph Inspector, Curation tabs,
   LCM drawer body. Each should scroll independently when its content overflows.
4. **Sticky shell header** stays pinned during long scrolls and never overlaps
   the first interactive row of the active plugin.
5. **Horizontal scroll regions use the hidden-scrollbar pattern** where intended
   (shell tab bar). Confirm the scrollbar is visually hidden but the region is
   still swipe-scrollable.

---

## 10. Pass / fail criteria

A dashboard **passes** a device profile when **every** numbered check in its
section (plus §3 global checks, §8 touch, §9 overflow) is PASS for that profile.

- **Hard fail (must fix before merge):** any §7 risk-register check fails; any
  page-level horizontal scrollbar at a phone width; any primary control
  unreachable/untappable on a phone; any drawer/modal that traps scroll or
  strands content behind the mobile chrome; canvas/SVG surface scrolling the
  page instead of the content.
- **Soft fail (file a follow-up, non-blocking):** a `vh`-vs-`dvh` occlusion with
  a workaround available; a tap target slightly under 44px that is still usable;
  a cosmetic reflow gap at a specific breakpoint.
- **Record per profile:** device name, width×height, list of
  `§N.M → PASS|FAIL|SOFT (note)`, screenshot paths for any non-PASS.

When all three dashboards pass §3 + their own section + §8 + §9 on at least
**iPhone 12, iPhone SE, Pixel 5, and iPad Mini** profiles, the mobile QA gate is
green. Today the automated baseline is `node smoke.mjs` (desktop+narrow) plus
`npm run smoke:mobile` (iPhone 12 + Pixel 5). Re-run both before signing off.
If you expand the automated matrix to cover `ipadmini`, verify that lane
explicitly rather than assuming it from the default phone script.
