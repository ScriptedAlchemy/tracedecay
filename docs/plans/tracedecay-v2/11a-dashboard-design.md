# Dashboard design: shell, layout, and visual system

Design authority for the PR14 fresh-start dashboard. Owned by the design
owner; foundation and workspace lanes implement against this document and do
not make structural, styling, or dependency decisions. Binding contracts
(journeys, typed envelopes, state taxonomy, accessibility and performance
acceptance criteria) live in [Plan 11](11-dashboard-frontend.md); this file decides how
they look, sit, and move. Decided 2026-07-23.

Plan 11's **Rejected and superseded frontend approaches** section is binding
design history. In particular, never delete or rename existing design tokens
to simplify a theme, and never fork a workspace-local copy of a shared
primitive; use additive token evolution and the shared UI primitive owner.

**Visual-audit correction (2026-07-26).** `dashboard/stories/audit.ts` now makes
render failures, uncaught page errors, axe violations, and any pixelmatch
baseline drift set a failing process exit status. The baseline-diff language
below is implemented audit behavior; the former run that measured pixel drift
and still exited successfully is superseded.

## User authority versus design decisions

The user supplied the quality bar — beautiful and functional, world class,
novel, interactive, and magnificent on every page — and named "very generic
and clinical and simple" as the failure mode. He did **not** state a preference
for typography, colour palette, dark/light mode, spacing scale, motion, or
easing. Every concrete choice on those axes below is a design-owner/agent plan
decision, not a user quote, and may be revised by that owner while preserving
truth, accessibility, performance, and the quality bar.

The only user-named external visual benchmark is cosmograph.app: visuals like
it, not adoption of its library. Linear, Vercel, Perfetto, and every other
reference below are design research selected by agents, not user endorsements.

The following rejected forms must not return: generic/clinical/simple UI;
bottom-panel chrome that steals the interactive graph; a sparse circular
single-project Brain whose geometry means nothing; a Brain that shows no live
neurons while agents are actively working; bland vertical lists as the dominant
space-consuming treatment; embedded-browser visual QA; and any fake count,
health state, activity, or backend capability.

The tooling boundary from Plan 11 also applies here: Rsbuild without a bundler
ADR, no Module Federation or Vite, and no shadcn adoption yet.

## Design principles

1. **Truth before polish.** The plan's sixteen domain states, coverage
   denominators, and evidence quality are the product. Layout and styling
   exist to make truthful states legible at a glance — never to smooth them
   over. An `unknown` is designed, not blank.
2. **Calm density.** Dense data with generous rhythm: one type scale, one
   spacing scale, restrained borders, whitespace doing the separation work.
   No more than one competing accent per region.
3. **Progressive disclosure along the journeys.** Overview → finding/entity →
   investigation → evidence/action is the plan's spine; each step reveals the
   next level in place (inspector, not page-jump) and deep links capture the
   exact position.
4. **Keyboard is a first-class pointer.** Every flow works keyboard-only per
   the plan's active-descendant rules; the command palette is the fastest
   path to anything.
5. **Severity is not quality.** Two independent visual axes everywhere:
   severity/consequence (how bad) and evidence quality (how sure). They never
   blend into one color.
6. **Beauty and function are one acceptance criterion.** A technically
   truthful page that remains generic, clinical, simple, or visibly
   non-magnificent is not accepted.

## Shell layout

One responsive shell, four fixed regions plus content:

- **Left navigation rail.** The twelve workspaces (Brain, Explorer, Loom,
  Sessions, Agents, Code, Knowledge, Delivery, Automations, Observatory,
  Costs, Settings; Work joins in PR17), icon+label, collapsible to icons at
  narrow widths, bottom-anchored Settings. The rail is navigation only — no
  status, no badges except a single Doctor attention dot driven by typed
  findings.
- **Top scope bar.** The active scope (profile/project/repository/worktree/
  branch/time window) rendered as removable chips, always visible per the
  plan's scope rule; cross-scope transitions are explicit chip edits. Right
  side: freshness indicator (SSE liveness + last watermark), theme toggle,
  command palette affordance.
- **Command palette** (Cmd/Ctrl-K, Radix Dialog): scope-aware search across
  workspaces, entities, saved deep links, and *legal actions only* — an
  action appears solely when its typed action reference exists for the
  current selection. Palette results carry the same truth metadata as lists
  (state chip, scope).
- **Right inspector panel.** The universal drill-down surface: finding,
  entity, TaskId, operation, citation. Resizable, keyboard-reachable
  (Enter opens, Escape closes and restores focus), URL-addressed (the deep
  link encodes inspector identity), stackable one level (breadcrumb, no
  infinite drawers). Evidence expansion happens here, never in tooltips.
- **Status strip** (bottom, one line): daemon connection state, active
  query/run progress with cancel, background operation receipts. Live
  regions coalesce announcements to ≤1/s per the plan. It must remain a
  single unobtrusive line or collapse/relocate; bottom chrome may never steal
  meaningful height from an interactive graph.

## Layout archetypes

Every workspace composes from four archetypes; no bespoke layouts:

1. **Overview grid** (Brain, Costs, Observatory landing): responsive card
   grid, each card one read model with its truth strip; cards link into
   archetype 2/3. No card renders a computed grade.
2. **Explorer split** (Explorer, Sessions, Knowledge, Agents, Delivery,
   Automations): left query/filter column (collapsible), center
   virtualized result table/list (36px data rows, 44px touch targets,
   sticky header, left-aligned text / right-aligned tabular numbers /
   state chips leading), right inspector. The planner-progress panel
   renders per-source progress rows here. The synchronized list/table is an
   accessible evidence surface, not the bland dominant composition or a
   massive real-estate consumer.
3. **Canvas + table** (Loom, Code graph, Brain map, PR17 topology): the
   renderer-neutral canvas above, the synchronized accessible table below
   (toggleable to table-only), shared selection, playback controls docked
   to the canvas, cluster/aggregation counts always visible.
4. **Config surface** (Settings, per-workspace preferences): sectioned
   forms, effective-vs-desired layered values side by side, typed patch
   preview → validate → CAS confirm as distinct steps.

## Scope model: all-projects first (every workspace)

The entire dashboard defaults to ALL-projects scope — one connected brain.
Every workspace renders the cross-project aggregate as its primary view and
narrows to a specific project (or repository/worktree/branch/session) only
through the ordinary scope-bar chips or an explicit in-view scope transition
(e.g. clicking a project cluster/row), which updates the URL scope. No
workspace is a project picker; narrowing never changes a workspace's shape,
only its population. Concretely:

- Brain: one aggregate map across every registered project — projects as
  named clusters/regions, cross-project edges styled distinctly; clicking a
  cluster is an explicit narrow.
- Explorer/Sessions/Knowledge: search and lists span all projects by
  default, with a project column/facet; Knowledge shows cross-project fact
  relationships as first-class rows and edges.
- Loom/Agents: traces and agent trees across projects, project as a lane
  grouping dimension when scope is wide.
- Code/Delivery: aggregate views list per-project/per-repo summaries and
  drill into one repo's graph/rails via explicit narrowing (a code graph is
  inherently per-repository; the aggregate level shows repo cards + health).
- Observatory/Costs/Automations/Settings: fleet-wide by default (all
  daemons/stores/providers), project as a grouping/facet; Settings shows
  layered config with per-project overlays side by side.

Rules that hold everywhere:

- Aggregates stay within tier budgets via daemon-side aggregation; project-
  level grouping is the natural first clustering level.
- Aggregate coverage statements enumerate projects consulted vs unavailable;
  an unreachable project's store renders as a truthful partial/unavailable
  region or row — never silently omitted.
- Deep links always capture whether scope was all-projects or narrowed, and
  to what.

## Visual system

Everything in this section is the design owner's selected implementation,
including theme default, palette roles, typeface, scale, spacing, radius, and
motion. None is attributed to a user preference.

Token architecture (Tailwind v4, two-stage so runtime theming works):

- Raw channel values live on `:root` and `[data-theme="light"]` /
  `[data-theme="dark"]` / `[data-contrast="more"]` scopes; a **non-inline**
  `@theme` maps semantic tokens onto those variables (values must not be
  baked at build time). Forced-colors mode defers to system colors
  (`forced-color-adjust: auto` by default; opt-outs only where a state chip
  would lose meaning, with a system-color replacement).
- Semantic token families: `surface-{0..3}` (elevation by background, not
  shadow), `text-{primary,secondary,muted}`, `edge-{subtle,strong}`,
  `accent` (single hue), and the truth families below. Role names only;
  no raw palette names in components.
- **Domain-state tokens**: one token per Plan 11 state
  (`state-loading`, `state-ready`, `state-partial`, `state-stale`,
  `state-locked`, `state-denied`, `state-unauthorized`, `state-redacted`,
  `state-conflicting`, `state-offline`, `state-unknown`,
  `state-cancelled`, `state-timed-out`, `state-error`,
  `state-unsupported-schema`, `state-complete-zero`). Every state chip is
  token + icon + label — never color alone.
- **Severity axis**: five ordered steps (`sev-info` → `sev-critical`),
  expressed as border/fill weight on a single hue ramp.
- **Evidence-quality axis**: four steps (`ev-measured`, `ev-associated`,
  `ev-predicted-calibrated`, `ev-unknown`), expressed as fill pattern/
  solidity (solid, hatched, dotted outline, dashed outline) so it survives
  monochrome and forced colors and can never be confused with severity.
- Dark is the default theme; light and contrast-more are complete first-class
  mappings, not overrides of a few tokens.

Typography and rhythm:

- One stack: `Inter var` (bundled, offline) for UI, `ui-monospace` stack for
  code/identifiers, `font-variant-numeric: tabular-nums` on all data cells.
- Type scale: 12/13/14/16/20/24 px; body-data is 13, UI chrome 14. Spacing
  on a 4px grid; data-row height 36px; panel padding 16px; card gap 12px.
- Radius 6px standard, 4px chips; borders 1px `edge-subtle`; elevation by
  surface step, shadows reserved for overlays only.

Motion:

- Durations 120ms (state), 180ms (panel), easing standard-decelerate;
  nothing animates position on data update (values change in place with a
  brief background pulse using `state-*` at 8% alpha). Reduced-motion
  replaces all of it with instant changes and starts playback paused per the
  plan.

## Aesthetic quality bar

Beautiful and functional are one requirement, not a tradeoff. The reference
benchmark named by the user is cosmograph.app's visual quality; this is not a
library-selection instruction. The design owner additionally selected the
best-crafted developer tools of this era as research references (Linear's
calm, Vercel's restraint, Perfetto's density done legibly). Those latter
references are agent choices. TraceDecay should sit comfortably beside them
and feel unmistakably its own.

- **Depth without noise.** Layered dark surfaces (`surface-0..3`) create
  space; hairline 1px edges, no decorative shadows, one accent hue used
  sparingly so state/severity color always reads as signal. Light theme gets
  the same care, not an inversion pass.
- **Typography is the interface.** Inter with proper optical sizing,
  tabular numerals everywhere data lives, tight-but-breathing line heights,
  true monospace identifiers. If a screen looks wrong, fix rhythm and weight
  before adding lines or color.
- **Micro-interaction polish.** Hover/focus/active states designed for every
  interactive element (token-driven, 120ms); focus rings are crisp
  two-tone, never browser-default; in-place value pulses instead of layout
  shifts; skeletons match final geometry exactly (zero CLS by design).
- **Iconography**: Lucide (ISC license), 16px grid, 1.5px stroke,
  consistent metaphors registered per domain entity (finding, generation,
  worktree, anchor…) in one icon map — no ad-hoc icon picks in workspaces.
- **Data-ink discipline in charts**: no gridline lattices, direct labels
  over legends where feasible, axis text at `text-muted`, series colors from
  the categorical ramp with perceptually even lightness in both themes;
  uncertainty always drawn (bands/intervals), never implied.
- **Crafted truthful states.** Empty/partial/stale/denied states are
  designed compositions (icon + one sentence + next action), unique per
  workspace where the meaning differs — never a generic illustration, never
  a bare spinner. `complete_zero_findings` celebrates quietly with its
  coverage receipt.
- **Design review.** Every workspace slice is opened in real Google Chrome;
  the reviewer captures every page and manually clicks through every
  interaction state, including movement and live updates. The embedded browser
  is rejected because its viewport is too small. Automated Playwright
  screenshots support this review and guard compositions afterward, but no
  per-commit acceptance record document or git-hash-tied evidence manifest is
  created.

## Continuous visual enforcement

Visual quality is checked by machinery throughout development, not only at
review:

The responsive target remains an explicit owner question. The user's
2026-07-06 "desktop resolution only please" conflicts with his 2026-07-25
below-1024px functionality bug and requested 320/768 checks at zero axe
violations. Until he decides, the provisional test posture is desktop-sized
review screenshots plus no hidden or broken functionality below `lg`; neither
side is recorded as the final product requirement.

- **Screenshot harness** (`npm run visual:audit`): Playwright renders every
  registered surface (workspace routes, archetype demos, each component's
  key states from a story registry) across both themes x three breakpoints
  (320, 768, 1440) x reduced-motion and contrast-more, against MSW fixture
  data covering the domain states. Output is ordinary ephemeral run evidence,
  not a committed per-commit gallery or machine record. The registry is
  product-test code: adding a workspace or state without registering its story
  fails the audit.
- **Baseline diffing**: stable route/state pixel baselines are direct
  product-test fixtures;
  `visual:audit --diff` fails on pixel drift beyond a perceptual threshold
  (odiff/pixelmatch) outside explicitly re-approved regions. Baseline
  changes are reviewed as direct product-test changes with the implementation;
  do not require an isolated baseline commit or separate evidence document.
- **Automated audit rules** run over the same renders: axe (WCAG) per
  surface, token-compliance scan (computed styles must resolve to token
  variables — raw hex/px drift fails), focus-visibility check (every
  focusable element screenshotted in :focus-visible), CLS probe (skeleton
  vs settled geometry), and information-density heuristics (min touch
  targets, max competing accents per region) as warnings.
- **Agent audit loop**: every implementation lane must run the dev server,
  exercise its surface in real Google Chrome, screenshot every page, manually
  click through every interaction state, and LOOK at it. Preview/Playwright
  tooling may support but not replace that pass. Report deviations truthfully
  in the normal work summary; do not attach a git-hash-tied self-audit record.
  "Compiles and tests pass" is not done; unaudited UI is unreviewable.
- **CI**: the visual suite runs on the pinned frontend verification runner;
  diffs and axe failures block; the gallery uploads as a run artifact so
  review happens on real renders, not local claims.

## Component conventions

- **EvidenceTruthStrip**: one-line strip on every compact result — authority
  icon, coverage fraction (never a bare percent without denominator),
  freshness age, citation count, omission count, score kind label. Always
  visible; the inspector expands each element.
- Tables: TanStack Virtual above 200 rows per the plan's mounts budget;
  selection is row-level with checkbox column appearing on first selection;
  column set customization persisted in the presentation allowlist only.
- Empty states: `complete_zero_findings` gets a designed confirmation
  (what was covered, when); every other "nothing here" renders its true
  domain state. No illustrations standing in for unknown.
- Forms never submit on Enter from a text field inside multi-field CAS
  surfaces; destructive/mutating actions always show the typed preview
  first.
- Charts (ECharts, lazy): one categorical palette derived from the token
  ramp, direct labeling over legends where ≤4 series, axis units mandatory,
  uncertainty bands rendered when the read model provides intervals.
- Graph canvases: Sigma.js + Graphology behind the ProjectionView adapter
  boundary (WebGL, node/edge reducers fed only by semantic tokens);
  d3-force physics for small ego-views; bespoke dense surfaces (Loom
  temporal trace, conflict/proximity heatmap) are Canvas + D3 scales with
  the synchronized accessible table as the semantic source of truth.

## Visualization catalog (decided 2026-07-23 from deep research)

Every data domain gets a designed home. Three engineering pillars are built
once and reused everywhere:

1. **The track engine** (bespoke, Perfetto-model): one viewport-sized canvas,
   lane virtualization via a visible-track bounds list, zoned hit-testing to
   stable IDs (CPU interval-tree, no GPU readback), visible-time-window data
   querying with server-side quantization, Canvas2D first with an optional
   eval-free WebGL batch path (CSP forbids eval-based WebGL — the speedscope
   lesson). Serves: Loom span waterfalls, causal arcs, dual-time scrubber
   lanes, execution-topology lanes/rails, commit rails, conflict heatmap.
2. **The playback controller** (bespoke): event-sourced fold over immutable
   frames (state at T = fold of frames ≤ cursor; never interpolate),
   two-track dual-time scrubber (observation lane + valid lane, typed
   event-kind markers, lockable cursors, retroactive corrections marked by
   shape), follow-live via suspend-on-scroll-back + floating return-to-live.
   Shared by Loom, Sessions replay, and execution topology.
3. **The selection store** (Zustand, stable-ID pub/sub using the crossfilter
   technique — filter mask over sorted ID indices, IDs only, no payloads):
   one selection resolved identically across graph/table/lanes/timeline.

Per-domain assignments:

| Domain | Visualization | Engine |
|---|---|---|
| Code/Brain/Knowledge graphs | force topology, ego views | Sigma.js + Graphology (color-picking, reducers) |
| Task DAGs / decomposition (Plan 24) | layered DAG + critical path | d3-dag (MIT) headless in a Worker; tight-tree-style ranking, IndexedDB layout cache keyed by graph version, edge virtualization |
| Commit/branch history (Delivery) | commit rails | hand-rolled canvas, pvigier active-branches algorithm, interval-tree hits |
| PR review / merge queue / checks | review lanes + check matrix | track-engine lanes + accessible matrix (Graphite/GitHub-merge-box interaction model) |
| Execution topology (PR17) | lanes, rails, heat cells | track engine (Perfetto track hierarchy for worktree/stack grouping) |
| Agent/session traces (Loom, Agents) | span waterfall; spawn = nested collapsible track, handoff = span-link arc | track engine (Honeycomb depth-collapse, Datadog hover-re-root) |
| Causality across lanes (Loom) | swimlane timeline + cross-lane arcs; Sigma ego-lens for pure topology | track engine + Sigma; W&B-style cluster-collapse ≥N with expansion cursor |
| LCM lineage | SVG icicle (d3-flame-graph, Apache-2.0); speedscope-style eval-free canvas as scale escalation; Sandwich-style raw↔summary drill-down | d3-flame-graph / bespoke |
| LCM token flow | Sankey, band = tokens, layoutIterations 0 (pinned order) | ECharts |
| Compaction epochs | boundary rules on the shared playback axis + token-per-epoch strip | playback controller + ECharts |
| Transcripts (Sessions) | virtualized stream with measured-height cache, data-model minimap (roles, boundaries, search-hit ticks), ARIA treeview turns, n/N hit navigation | TanStack Virtual + bespoke minimap |
| Fact embeddings (Knowledge) | WebGL scatter, spatial-index lasso → stable IDs, zoom-hierarchical cluster labels (Nomic pattern), projection provenance always visible (UMAP axes labeled relative-only) | regl-scatterplot (MIT) gated on a CSP/eval audit of regl; fallback: eval-free bespoke point layer; embedding-atlas as design reference |
| Conflict/proximity matrix | split-cell canvas: two independent color ramps + dual legend (never blended); table exposes both channels as separate columns | bespoke canvas + D3 scales |
| Distributions over time (scores, latency) | Honeycomb-style column-histogram heatmap, log color scale, region-select → comparison | bespoke canvas + D3 |
| Calibration (estimates vs outcomes) | reliability diagram: y=x diagonal, adaptive equal-count bins, per-bin n + CIs visible, low-n bins greyed | ECharts |
| Storage/cost hierarchies | treemap (area = actual; color = budget utilization ONLY when budget known); icicle when depth > 3 | ECharts |
| Time series (Observatory/Costs) | streaming append + sliding window, LTTB sampling; visible truncation labels | ECharts; uPlot (MIT, ~50 KB) as the bounded escalation for the hottest always-live panels |
| Row micro-viz | SVG-per-cell sparkline/coverage/freshness/score-kind glyph with mandatory text equivalents; shared-canvas escalation if profiling demands | bespoke (D3 scales) |

Disqualifications and adoption criteria (from license/size research): elkjs (EPL-2.0,
~500 KB) is banned from the default path; Cosmograph the product is
CC BY-NC and disqualified — the optional GPU overflow adapter targets
**cosmos.gl (MIT, OpenJS)** instead, under Plan 11's existing acceptance criteria. Any
WebGL dependency must pass an eval/CSP audit before adoption.

## Implementation notes for foundation lanes

- `dashboard/src/theme/tokens.css` holds the raw scopes + `@theme` mapping;
  `dashboard/src/ui/` holds the variant-layer primitives (CVA over Radix)
  that consume only semantic tokens. Workspace lanes consume `ui/` and
  archetype shells; they do not write raw Tailwind color/spacing utilities
  outside the token set (lint-enforced via a class allowlist check in CI).
- The four archetypes ship as shell components (`ui/archetypes/`) with the
  regions as slots; workspaces fill slots and own only their read-model
  wiring and workspace-specific panels.

## Real-profile findings (2026-07-25, first live review at scale)

Verified against the owner's real profile (43 repositories, 109.1K symbols,
257.6K edges, 2,298 files, live SSE): the instrument language holds at real
magnitude — display-tier counts, degree rails on most-connected symbols, and
the connection-honest signal panel all read correctly. Guidance below is
measured, not hypothetical; align future dashboard slices with it.

1. **A layout may never imply structure the data lacks.** The all-projects
   constellation ring-packs disconnected components; with 43 repos of one
   checkout each this renders a circle, which the owner correctly read as
   "what is that supposed to mean?" Rule: every macro-composition must encode
   a REAL property (recency, activity, filesystem locality) and the caption
   must name what it encodes. Packing artifacts that read as geometry are
   design defects even when the data shown is truthful.

2. **Scoped Brain must become the project's brain.** Selecting a project
   currently narrows the registry view to a sparse hub+checkout pair. The
   scoped gateway already serves the project's code-graph neighborhood,
   stores, facts, and sessions; the scoped view should answer "what does
   TraceDecay know about THIS project" with the graph as the field and
   instrument readouts around it.

3. **Dense connected components need density-aware rendering.** An 80-symbol
   real neighborhood fuses into an unreadable mass under settings tuned on
   sparse fixtures (screen-px node radii vs graph-space spacing; outliers
   dilating the camera bbox). Fixture graphs under ~40 nodes systematically
   under-test this; every graph surface must also be judged against a dense
   real neighborhood before shipping.

4. **Fixtures under-represent scale shape, not just scale size.** Real data
   differs in DISTRIBUTION (43 single-checkout repos; one 5069-degree hub
   symbol) rather than only in count. Wire-true fixtures should include at
   least one skewed-distribution case per graph surface.

5. **What the live review validated:** offline-vs-idle honesty (READY + zero
   rate + climbing age while connected-quiet), the magnitude-rail idiom at
   real degree ranges, and the ranked most-connected list against real paths.
   These carry to future surfaces as-is.

6. **Real agent activity must be visible.** The user rejected a Brain with no
   visible neurons while agents were active. Render real
   activity strikes from the admitted wire source; when that source cannot
   provide finer scope, say so. Never synthesize decorative firing.
