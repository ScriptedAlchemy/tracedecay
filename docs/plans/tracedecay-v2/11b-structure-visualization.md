# 11b — Structure visualization: anatomy, transit, disagreement

Status: user direction captured and agent concept/mockup exploration complete
(2026-07-25); the concepts are not themselves user-approved specifications.
Delivery timing is an **open owner question**. The user raised topography and
"kinestetic synastisya" while the core dashboard delivery was active and never agreed to defer them
until after it. The current roadmap sequencing is therefore a plan proposal,
not a user decision. Historical mockups live on branch
`worktree-agent-ac56015e63cbf0f86` under `mockups/structure-viz/` (three
self-contained HTML pages, dark+light shots, per-concept notes).

## Why these three

TraceDecay holds three graphs over the same code: the static call graph, the
temporal graph (which sessions touched which files), and the semantic graph
(facts/entities citing code by name). Conventional architecture diagrams draw
only the first, which is why they are inert — they restate what the compiler
already knows. Every surface below either composes the layers on one field or
draws the *disagreement* between them.

The user explicitly rejected "bland UML" as the structural idiom. The visual
quality benchmark is cosmograph.app — visuals like it, not adoption of its
library — and every structure surface must be beautiful, functional, and
truthful rather than merely diagrammatically correct.

## Wire truth (recon summary — design against this, not against hope)

| Relation | Granularity | Where | Status |
| --- | --- | --- | --- |
| callers/callees one hop | symbol | `graph_queries.rs` `caller_rows`/`callee_rows`, `/api/plugins/graph/node/{id}/neighbors` | servable today |
| sessions that edited code | **file only**, Claude-provider rollup (`sessions.metadata_json.$.edited_files[]`); Codex per-message `$.files[]`, no rollup; other hosts none | `src/sessions/claude/record_metadata.rs:463`, `/api/plugins/graph/node/{id}/sessions` | generated contract and `NodeEvidence` consumer live; JSON-scan, unindexed |
| "discussed" | free-text FTS on the name (`session_messages_fts`) | global db | noisy for common names; secondary arm only |
| facts citing a symbol | **name match** (memory_entities.normalized_name, v2 payload FTS) — not symbol identity | `src/db/memory_v2/…`, `/api/plugins/graph/node/{id}/facts` | generated contract and `NodeEvidence` consumer live; same-name collision caveat |
| covering tests | symbol (pure graph computation, callers depth-3 ∩ test files) | `handlers/health.rs:987` `handle_test_map`, `/api/plugins/graph/node/{id}/tests` | generated contract and `NodeEvidence` consumer live |
| live activity | **project only** (`ActivityPulseV1` has no path/symbol) | `src/dashboard/activity_bus.rs:83` | per-symbol strike needs a wire-contract change (stability tests exist) |
| call chain | symbol, directed, calls-only, single shortest path, depth ≤20 | `queries.rs:133` `get_call_chain`, `/api/plugins/graph/call-chain` | generated contract and Code `CallChain` consumer live |
| stratification | **file-level** dependency depth (Tarjan SCC → longest path), DSM clusters | `src/graph/health.rs:218/:733`, `/api/plugins/graph/strata` | generated contract and Code `Strata` consumer live; adjacency scan unpaginated/uncached |
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

**End-to-end status (2026-07-26; not acceptance).** All three endpoints above
are implemented and registered. Their generated wire contracts, backend tests,
typed frontend reader, and `NodeEvidence` consumer now exist. Surface 1 is
routed through the Code workspace; it is no longer a backend-only gap.

## Surface 2 — Call-chain transit map (historical standalone proposal)

Two symbols → the shortest directed calls-only route drawn as a transit line
across horizontal strata bands ordered by measured file dependency depth.
Stations = symbols (inherit their file's depth — caption says so), hatched
full-height "NO STATION" bands for crates the chain never enters (a chart that
drops skipped layers cannot show a layer being skipped), foot ruler carrying
per-hop depth delta, upward hops drawn at true steepness in the error hue with
the caption stating a climb is a boundary-crossing observation, not proof of a
bug.

Endpoints: `/api/plugins/graph/call-chain`, `/api/plugins/graph/strata`
(dependency_depth + DSM ordering; budget the unpaginated adjacency scan —
cache per graph generation). Chain selection rule: user-picked endpoints,
shortest route, captioned as exactly that (single path; k-shortest is a later
question).

**End-to-end status (2026-07-26; not acceptance).** Both endpoints are
implemented and registered at `src/dashboard/mod.rs:1013-1018`; the earlier
instruction to register `get_call_chain` is complete. The shared typed reader
now feeds `CallChain` and `Strata` components in the Code workspace. Surface 2
is no longer a backend-only gap.

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

## User rejection and redirect (2026-07-25)

The transit map read as service routing, which the user rejected: call chains
must not look like service boxes. His stated intent is graph tools made visible
— callers/callees traced over surrounding types, the structure of many files
and many functions in a file, "the topography of the code base" — broader and
more futuristic than any two-point route. The agent-developed direction under
exploration is one continuous, semantically zoomed space rather than pages:

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

## Impressionistic sensory direction (2026-07-25)

The user's words were "like kinestetic synastisya," immediately hedged with "i
dont wuite know but you get the idea i mean or i want." Treat that as
impressionistic direction, not a motion, physics, colour, or interaction
specification. The measurement mappings below are agent/design-owner
proposals: structure you can feel through motion while extending the honesty
rule from "position encodes a stated measurement" to "sensation encodes a
stated measurement." One mapping is proposed app-wide so the body learns it
once:

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

## Rendering strategy (user permitted full custom, 2026-07-25)

Library constraints must not cap the vision — custom rendering (D3-style
hand-rolled canvas/WebGL, game-engine techniques, physics engine) is approved
where a library falls short.

Decision frame:
- Shipped core-dashboard surfaces stay on Sigma — they work, they're gated, no churn.
- The topography/trace surfaces target a CUSTOM renderer from the start:
  - Physics: hand-rolled Verlet/spring integrator first (≤250-node subgraph cap
    makes this trivial, zero deps, exact control of the weight/tension feel).
    Escalate to a wasm physics engine (e.g. Rapier) only if collision/joint
    needs outgrow it — not for springs.
  - Drawing: Canvas2D first (terrain contours, membranes, bundled flows are
    path-heavy 2D work where Canvas2D + offscreen layering is simpler and
    theme-safe); WebGL (regl/pixi-class or hand-rolled) when node counts or
    glow/displacement effects demand it. Three.js only if we go literal 3D
    relief — undecided, prototype will tell.
- Keep the honesty invariants renderer-independent: a pure layout/simulation
  module (like brain/field.ts) computes positions/forces from measurements;
  the renderer only draws. Tests hit the pure module.

## Topography round one — coordinator verdict (2026-07-25)

Four sheets on branch `worktree-agent-a3433e6e6f36a4201` under
`mockups/code-topography/` (dark+light, notes, README; kindColor + tokens
transcribed, honest aggregation statements throughout).

- TRACE is the hero and lands the ask: callers as tributaries, callees as
  delta, per-edge call-site widths, impl/trait membranes with ports, hop rings
  not elevation (captioned — the call graph merely happens to run downhill),
  dashed mouths for edges leaving the graph, all over the dimmed cortex relief
  so shoreline crossings are cross-module calls. 26 nodes at ≤3 hops = direct
  render, inside subgraph caps. Round two builds THIS as the live physics
  prototype (custom canvas + Verlet springs; channels are already rivers —
  they want to flow).
- CORTEX is the right macro underlay: real contour interval (0.50 e/sym,
  indexed 5th), √-area mass, aggregation stated (19 regions ⟵ 1206 symbols),
  weather strip honestly project-level. Open question adopted: contours should
  probably encode coupling ratio (internal÷external) rather than e/sym to
  avoid double-counting with area — decide in round two.
- CORE SAMPLE answers "shape of the inside of a file, six files at once";
  the handlers.rs-vs-decision.rs comparison is instant and impossible in a
  tree view. Its (b) option — cores collapsing toward hairline fabric as count
  grows — is the growth path.
- LENS is adopted as the NAVIGATION MODEL, not necessarily a sheet: zoom is a
  position on an aggregation-ratio continuum, never a screen replacement; the
  subgraph cap becomes a readable position on the ruler. This is how
  Cortex→Trace→Core Sample connect in the product. Its self-critique (three
  grammars on one sheet) is accepted — as a literal surface it is optional.

Synthesis: ONE navigable space. Far = CORTEX. Touch a symbol = TRACE floods.
Enter a file = CORE SAMPLE. LENS is the theory of motion between them. The
sensory contract rides on top: channel = spring, mass = weight, churn = warmth.

## Visual artifact index (reference images)

This branch-tied index is historical design evidence, not a delivery authority,
acceptance record, or artifact set that future lanes must recreate. Do not add
per-commit mockup/evaluation documents or git-hash-tied screenshot manifests.
Current implementation must instead be reviewed in real Google Chrome at full
viewport, with every relevant interaction state exercised and screenshotted;
the embedded/in-IDE browser is rejected as too small for this review.
Existing design-round artifacts are on mockup branches (kept off trunk — heavy
PNGs). View an existing file without switching branches:
`git show <branch>:<path>` (binary shots: `git show <branch>:<path> > /tmp/x.png`).

| Artifact | Branch | Path |
| --- | --- | --- |
| Symbol Anatomy sheet (dark/light) | `worktree-agent-ac56015e63cbf0f86` | `mockups/structure-viz/shots/symbol-anatomy-{dark,light}.png` |
| Call-chain transit sheet | same | `mockups/structure-viz/shots/call-chain-transit-{dark,light}.png` |
| Disagreement field sheet | same | `mockups/structure-viz/shots/disagreement-field-{dark,light}.png` |
| CORTEX relief sheet | `worktree-agent-a3433e6e6f36a4201` | `mockups/code-topography/shots/cortex-{dark,light}.png` |
| TRACE watershed sheet (hero) | same | `mockups/code-topography/shots/trace-{dark,light}.png` |
| CORE SAMPLE strat sheet | same | `mockups/code-topography/shots/core-sample-{dark,light}.png` |
| LENS zoom-as-position sheet | same | `mockups/code-topography/shots/lens-{dark,light}.png` |
| Live physics prototype + QA keyframes | `worktree-agent-af882d6565fbab159` | `mockups/code-topography/prototype/` (`shots/{hub,leaf}-{1..4}*.png`, `README.md` tuning table) |
| PR-421 reasoning weave (real data) | same branch, uncommitted copy also served in-session | `mockups/code-topography/prototype/pr421-weave.html` |
| Shipped-state pixel baselines (all 12 workspaces × themes × widths) | trunk | `dashboard/audit-baselines/` |

Design notes accompany each sheet in the same directory (`notes/` or
sibling `.md`), stating per-channel encodings, backing data, and open
questions — read the note before reusing a sheet as a spec.
