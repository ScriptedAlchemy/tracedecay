# 11b — Structure visualization: anatomy, transit, disagreement

Status: concept approved, mockups + wire recon complete (2026-07-25). Sequenced
after PR14 dashboard delivery. Mockups live on branch
`worktree-agent-ac56015e63cbf0f86` under `mockups/structure-viz/` (three
self-contained HTML pages, dark+light shots, per-concept notes).

## Why these three

TraceDecay holds three graphs over the same code: the static call graph, the
temporal graph (which sessions touched which files), and the semantic graph
(facts/entities citing code by name). Conventional architecture diagrams draw
only the first, which is why they are inert — they restate what the compiler
already knows. Every surface below either composes the layers on one field or
draws the *disagreement* between them.

## Wire truth (recon summary — design against this, not against hope)

| Relation | Granularity | Where | Status |
| --- | --- | --- | --- |
| callers/callees one hop | symbol | `graph_queries.rs` `caller_rows`/`callee_rows`, `/api/plugins/graph/node/{id}/neighbors` | servable today |
| sessions that edited code | **file only**, Claude-provider rollup (`sessions.metadata_json.$.edited_files[]`); Codex per-message `$.files[]`, no rollup; other hosts none | `src/sessions/claude/record_metadata.rs:463` | needs thin endpoint; JSON-scan, unindexed |
| "discussed" | free-text FTS on the name (`session_messages_fts`) | global db | noisy for common names; secondary arm only |
| facts citing a symbol | **name match** (memory_entities.normalized_name, v2 payload FTS) — not symbol identity | `src/db/memory_v2/…` | needs thin endpoint; same-name collision caveat |
| covering tests | symbol (pure graph computation, callers depth-3 ∩ test files) | `handlers/health.rs:987` `handle_test_map` | servable today via in-process `project_graph` |
| live activity | **project only** (`ActivityPulseV1` has no path/symbol) | `src/dashboard/activity_bus.rs:83` | per-symbol strike needs a wire-contract change (stability tests exist) |
| call chain | symbol, directed, calls-only, single shortest path, depth ≤20 | `queries.rs:133` `get_call_chain` (handler exists, never registered) | needs thin endpoint |
| stratification | **file-level** dependency depth (Tarjan SCC → longest path), DSM clusters | `src/graph/health.rs:218/:733` | needs thin endpoint; adjacency scan unpaginated/uncached |
| co-change | **file pairs**, Claude sessions only, one `json_each` self-join | derivable; SQL in recon report | needs thin endpoint; provider-complete or symbol-level needs new extraction |

Note: the existing `/api/plugins/graph/path` route is UNDIRECTED and
edge-kind-agnostic. It is not a call chain and must not be reused as one.

## Surface 1 — Symbol Anatomy (build first)

Drill-in from the Code spine (click a hub) — not a new nav page. One composed
field: the symbol as a machined plate carrying only measured fields (`absent`
printed, never blanked); callers left / callees right as bars on ONE shared
call-site scale, kind hues from `kindColor.ts`; below it the non-code
afferents: sessions strip on a real time axis, facts with trust rails anchored
at zero, covering tests with staleness, live strike state.

Honesty deltas from the mockup (mandatory):
- Strip title is "sessions that edited this FILE" — symbol-level linkage does
  not exist. Caption declares provider coverage ("Claude sessions; Codex
  partial; other hosts unrecorded").
- No per-symbol "last strike". Live section scopes to the project until
  `ActivityPulseV1` carries a path (separate wire-contract decision).
- Facts section captioned "citing this name" — name match, not identity.
- Unrecorded message counts drawn hollow at zero height (mockup already does
  this — keep it).

Endpoints: `/api/plugins/graph/node/{id}/sessions` (edited_files json_each +
FTS arm), `/api/plugins/graph/node/{id}/facts`, `/api/plugins/graph/node/{id}/tests`
(in-process `project_graph` test-map). Neighbors endpoint exists.

## Surface 2 — Call-chain transit map (cheapest, build second)

Two symbols → the shortest directed calls-only route drawn as a transit line
across horizontal strata bands ordered by measured file dependency depth.
Stations = symbols (inherit their file's depth — caption says so), hatched
full-height "NO STATION" bands for crates the chain never enters (a chart that
drops skipped layers cannot show a layer being skipped), foot ruler carrying
per-hop depth delta, upward hops drawn at true steepness in the error hue with
the caption stating a climb is a boundary-crossing observation, not proof of a
bug.

Endpoints: `/api/plugins/graph/call-chain` (register the existing
`get_call_chain`), `/api/plugins/graph/strata` (dependency_depth + DSM
ordering; budget the unpaginated adjacency scan — cache per graph generation).
Chain selection rule: user-picked endpoints, shortest route, captioned as
exactly that (single path; k-shortest is a later question).

## Surface 3 — Disagreement field (build last; premise gated)

Call edges overlaid with co-change edges in three declared states: coupled-and-
called (quiet), called-never-co-touched (dotted), co-changed-but-unlinked
(loud, drawn on top, enumerated in full below the field, legend counts computed
from the edge list so caption and picture cannot drift). Nodes with no session
attribution drawn hollow — absence unmeasured, not zero.

Gates before build:
1. Reframe FILE-granular (mockup draws symbol pairs; only file pairs are
   derivable). Node = file, cluster = module/crate.
2. Resolve the false-positive class: trait impls and generated bindings
   co-change with no direct call edge and would flood the loud state. Candidate
   remedy: suppress pairs connected by ANY static edge kind (uses/imports/
   contains), not just calls, and say so in the legend.
3. Provider coverage declared on the surface ("derived from Claude sessions
   only") until Codex rollup or a session→file materialization exists.

## Sequencing

1. Anatomy (thin endpoints + drill-in route; the payoff that justifies the
   symbol list).
2. Transit (two endpoints, machinery exists).
3. Disagreement (after gates 1–3; consider a session→file indexed
   materialization first — it also de-risks Anatomy's JSON-scan).

Open wire-contract question tracked separately: adding `path` to
`ActivityPulseV1` + coalescing key so live strikes can reach file/symbol
surfaces (stability tests at `events_api.rs:718`, `activity_bus.rs:152` must
move with it).

## Owner redirect (2026-07-25)

The transit map read as service routing, not what was asked. The intent is the
graph tools made visible — callers/callees traced over surrounding types, the
structure of many files and many functions in a file, "the topography of the
codebase" — broader and more futuristic than any two-point route. Direction
under exploration (mockups in flight): one continuous, semantically-zoomed
space rather than pages —

- CORTEX (macro): modules as relief terrain — depth-strata placement, area =
  symbol mass, contour lines = measured connectivity density, churn heat,
  bundled cross-module call rivers.
- TRACE (hero): a selected function floods the terrain — caller tributaries
  converging, callee delta fanning, flow width = call sites, impl/trait
  membranes as translucent enclosures the flow enters and exits.
- CORE SAMPLE (micro): files as vertical strat columns in true line order
  (start_line/end_line), internal call arcs, external edges to sibling columns.

Surfaces 1 and 3 of the original plan (Anatomy, Disagreement) are unaffected.
The transit map demotes to "maybe, inside Trace" — its endpoint work
(registering directed get_call_chain, strata service) is still the right
backend for Trace and proceeds unchanged.

## Sensory contract (owner: "kinesthetic synesthesia", 2026-07-25)

The direction in one phrase: structure you can FEEL through motion. Sensation
channels are measurements — the honesty rule extends from "position encodes a
stated measurement" to "sensation encodes a stated measurement". One mapping,
app-wide, so the body learns it once:

| Feel | Measurement | Mechanics |
| --- | --- | --- |
| weight / inertia | connectedness (degree / mass) | hover-response latency, bloom depth, settle time scale with degree; leaves flick, hubs are slow and deep |
| tension / deformation | coupling strength (call-site count) | edges as springs, stiffness = call sites; drag deforms the neighborhood proportionally — coupled code moves as flesh, loose code trails |
| texture / grain | cyclomatic complexity | contour tightness, surface roughness |
| warmth | churn recency | heat tint that decays over real time |
| pulse | live activity (SSE strikes) | shipped — the existing strike/bloom machinery |

Consequences:
- Reduced-motion: every felt channel needs a static equivalent (weight → size
  already; tension → edge thickness; pulse → pinned-lit) — the a11y story is a
  first-class rendering mode, not a degradation.
- Static mockups settle the SPATIAL language only. Feel requires a live
  prototype: round two is an interactive page with real spring physics over a
  real subgraph, reviewed in Chrome at full viewport.
- Performance boundary: spring simulation over the ~80-250-node subgraph cap is
  cheap; the cortex (aggregated regions) simulates dozens of bodies, not
  thousands.
