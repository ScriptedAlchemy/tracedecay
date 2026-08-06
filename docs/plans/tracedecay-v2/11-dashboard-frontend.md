# TraceDecay V2 Dashboard Frontend

## Status / Role

Normative product plan. Every product PR ships its usable UI slice with its
backend behavior. By current **plan authority**, PR14 completes exactly the
shared shell and Brain, Explorer, Loom, Sessions, Agents, Code, Knowledge,
Delivery, Automations, Observatory, Costs, and Settings, while PR17 adds the
first-class Work workspace and task-graph projections owned semantically by
[Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md).
PR17 also adds the execution-topology lens specified below: independent
execution-placement, branch-topology, review-topology, and integration-strategy
lanes, dependency-commit and merge-order rails,
conflict/proximity evidence, integration proposals and receipts, test/CI state,
and temporal replay over the same canonical Work selection.

The user did not enumerate those twelve workspaces and did not accept the
PR17 timing in recorded speech after asking "ahat about kanban/task graph etc"
on 2026-07-25. He did explicitly require a first-class TraceDecay task
graph/Kanban inspired by Hermes but more powerful. The twelve-workspace/PR17
allocation remains binding plan authority pending an owner scheduling decision;
it must not be attributed to the user.

Earlier component names, route inventories, fixture matrices, packet names,
script lists, and frontend gate layouts are historical implementation evidence,
not prerequisites or artifacts that PR14 or PR17 must recreate. Only actually
independently released public URLs/deep links may retain protocol compatibility;
persisted navigation uses the fresh-store rule. All other retention is judged
by the user journeys, behavior, accessibility,
performance, platform, and regression requirements below.

Pure source-only/internal view helpers, design tokens, wire-visible V2 DTO
revisions, and branch-era routes change in place. Persisted navigation accepts
only its exact final shape; any other database, store, spool, file, or
projection returns typed `ResetRequired` and requires explicit reset or
recreation. No storage reader, migration, backfill, dual write, or census path
exists; references and tests alone are not release evidence.

Current implementation (2026-07-25): the legacy multi-app dashboard (shell,
holographic, lcm, graph, code-diagnostics, savings, settings as separate
bundles) was removed. The real single-app `app-dist` bundle is the product
served at `/`; the legacy placeholder shell is isolated at `/legacy` and must
not be described as the product bundle. Nothing from the deleted apps is a
dependency; retained API handlers remain compatibility surfaces for the
current implementation.

The current PR14 checkpoint is implemented but not accepted. Real Settings
capability and authority-failure state (all seven prior "unsupported" claims
were stale), storage budget findings and unreadable roles, truthful
partial/unverified graph state, discriminated registry outcomes, scoped
failures that no longer masquerade as `not mounted`, and unavailable reads
preserved across Agents, Costs, Knowledge, and Sessions now exist. Explorer
coordination/source-local query and LCM read context, Loom time boundaries,
Delivery, Doctor, storage telemetry, asset serving, and feedback observation
wiring also exist.

**Verification correction (2026-07-27).** Direct backend and frontend coverage
now reaches Settings CAS, Delivery, Explorer routes, Doctor, storage telemetry,
Loom, asset serving, and every dashboard workspace. The daemon-hosted Settings
journey exercises the installed production client, durable reread, and
stale-revision rejection. Historical suite names, command lines, scan counts,
and route matrices are run evidence rather than plan invariants.

Acceptance remains open on the product gaps below. Unavailable data must stay
truthful rather than being replaced with fabricated values.

- **Plan 11 owner:** the runtime performance and payload budgets are
  withdrawn as acceptance criteria by owner decision 2026-07-31 (see the two
  withdrawal sections below). HTTP fault coverage and renderer semantic
  separation remain delivered behavior.
- **Plan 11 owner:** Code still lacks diagnostics, affected tests, code
  health, and branch-aware freshness; Agents lacks its PR14 subagent/handoff
  context; Sessions lacks raw-message drill-down, compaction boundaries, and
  replay; Knowledge lacks contradictions, supersession, and curation;
  Observatory lacks hook hints, event flow, and latency; Automations lacks its
  existing-runtime run history and artifacts; Costs lacks a latency breakdown;
  and Loom's zoom/brush/playback helpers are not wired into its UI.
- **Plan 11 owner:** `redacted` and `locked` are defined in `StateChip`
  but no workspace currently exercises them with supplied backend state.
- **Plan 16 owner:** Explorer's multi-project/repository/worktree pivots
  remain future multi-root work; PR14 still owns the single-root time pivot.
- **Owners: Plans 24 and 32; Plan 17 for public handoff:**
  `RequestCancel` and `RequestExternalHandoff` authority-negative coverage
  belongs to the executable Work/handoff journey and does not block the PR14
  twelve-workspace checkpoint.

**Accessibility and route coverage closed (2026-07-27).** Direct browser
journeys now visit every workspace in the state where it makes a truth claim
and cover the required narrow/desktop viewports, zoom, contrast, forced-colors,
keyboard, and touch behavior. Trapped panes, undersized targets, unnamed
internal scrollers, and forced-colors checkbox state are fixed. Exact scenario,
scan, test, and canary counts are intentionally not plan requirements.

**Payload budgets withdrawn (owner decision, 2026-07-31).** The owner removed
the transfer-payload ceilings as acceptance criteria ("dont care about
dashbaord bugets we can delete that"). `scripts/check-dashboard-budget.mjs`,
its `dashboard-assets` CI step, and the
`tests/dashboard_workflow_contract_test.sh` lock on it are deleted; no payload
budget is measured or enforced. This withdraws only the byte ceilings — it is
not permission to regress code splitting or ship falsified measurements.

Retained code that budget enforcement previously motivated stays on its own
merits: `dashboard/src/viz/chart/echarts.ts` registers only the bar/line/grid/
tooltip modules the product draws, and `Chart` refuses to render a series type
outside that registered set (ECharts answers an unregistered series with an
empty canvas rather than an error, which would be a falsified reading).

The virtualization bound is likewise asserted rather than described:
`VirtualList.dom.test.tsx` proves a 100-row page and the 200-row threshold stay
plainly rendered, and that a 5,000- and 20,000-row list windows to a
viewport-sized set under the 250-element ceiling — with a nonzero lower bound,
because a virtualizer that mounted nothing would otherwise satisfy any ceiling.

One audit finding was checked and refuted: `/api/plugins/graph/strata` does
have a Code-workspace consumer (`Strata.tsx` reads it through
`StructureReadV12Schema` and `CodePage.tsx` renders it), so the Plan 11b
statement that all five structure routes are consumed stands.

Also not yet executed: the real-Chrome visual review, manual NVDA/VoiceOver
completion, and the 12-participant usability study.

**Reachability correction (2026-07-26).** All twelve PR14 workspaces are
registered as lazy routes in `dashboard/src/app/routes.tsx`, and `build.rs`
implements the source-stamp, packaged-asset, and `include_bytes!` embed
contract. An audit score of zero "implemented and reachable" was a labelling
artifact, not evidence that the dashboard or embed path is absent. The
specific unverified and unreachable sub-surfaces named in this plan remain
open; the twelve workspace routes and embed mechanism must not be replanned.

**PR14 surface alignment (2026-07-26).** The five Plan 11b graph-structure
routes now have generated contracts and Code-workspace consumers. Explorer's
duplicate query-run/source schemas were removed in favor of the generated
barrel; only the two routes that still lack Rust wire types retain explicitly
labelled hand-written schemas. Automations consumes the generated scheduler
status type and wires typed pause/resume mutations that replace cached state
only with the server's post-control reading. These are delivered surface
corrections, not acceptance of the still-unverified checkpoint above.

## Rejected and superseded frontend approaches

- **Module Federation is rejected.** The dashboard is one ordinary Rsbuild
  application and React tree, not a federated composition.
- **Vite and an ADR about the bundler are rejected.** The user's exact
  instruction is "use rsbuild. no adr. just pick rsbuild."
- **The pre-PR14 dashboard is rejected as a frontend base.** "Gut the existing
  dashboard" means retained API compatibility cannot restore its multi-app
  composition or visual language.
- **Foundation lanes do not style, structure, or select frontend
  dependencies.** The historical model assignment has changed, but the
  designated design-owner boundary remains.
- **shadcn adoption is on hold.** "Dont use shadcn yet" is the current
  delivery-first instruction; investigation does not authorize adoption.
- **Git-hash-tied record documents are rejected.** Do not create per-commit
  screenshot manifests, acceptance/evaluation records, or evidence packets.
  Real-Chrome review, direct tests, ordinary run output, and owning-plan status
  are the evidence.
- **Generic, clinical, simple, non-world-class, or non-magnificent UI is
  rejected on every page.** Beauty and function are simultaneous acceptance
  criteria, not optional polish.
- **Bottom chrome that steals space from an interactive graph, meaningless
  sparse circles, invisible real activity, bland space-hungry vertical lists,
  bland UML, service-box call chains, embedded-browser QA, and any falsified UI
  are rejected.** Plans 11a/11b carry the concrete replacements.
- **Uncoordinated token deletion or rename is rejected.** A `tokens.css`
  variant removed tokens and broke 109 branch-local references. Change the
  unreleased token and all consumers together to the final shape; retain an
  alias or migration only if a published external consumer is evidenced.
- **The Explorer workspace replacement from `86fd70fe6` is superseded.** It
  was adopted and then reverted by `cb6c71739` to trunk behavior, retaining
  only the independently accepted Hermes LCM parsing. Later coordinator,
  accessibility, and source-binding work does not revive the rejected
  redesign by implication.
- **Forked shared primitives are rejected.** The duplicated `Meter` was
  de-duplicated in `1d83b8071`; dashboard workspaces reuse
  `dashboard/src/ui/instrument.tsx` and other shared primitives rather than
  carrying local copies.

The authoritative frontend-history brief was integrated on 2026-07-25. Its
vendored/forked-Tailwind rejection is explicitly quarantined: it came from a
different bundler-benchmark workspace and is not a quoted TraceDecay rule.
Upstream maintained integration may still be preferred by plan/library-first
reasoning, but not falsely attributed to the user.

## Outcome

The dashboard is TraceDecay's world-class flagship product surface: a polished,
highly interactive connected brain across projects that preserves precise
repository, worktree, branch, session, agent, time, provenance, coverage, and
authority scope. Beauty is a hard acceptance criterion in the user's words:
"its importsnt that it looks really beautiful and functional" and "we wanna
overhaul anything that isnt magnificent and beautful." Visual quality serves
comprehension and function rather than novelty.

## Delivery-first product journeys

The detailed contracts below remain binding and are delivered through two real
product journeys rather than as contract, fixture, or route milestones.

**PR14 finding → evidence → Doctor → confirmed remediation:** a user enters
from Brain, Explorer, Loom, or Code with exact scope; follows a canonical
finding into citations, provenance, coverage, and source progress; opens the
one Doctor application diagnosis; previews and confirms an owner-supplied
configuration, host, or runtime remediation; resumes the durable operation
receipt after reload or restart; and sees an independent post-operation
observation confirm recovery or retain a truthful partial/unavailable/failing
state. Sessions, Agents, Knowledge, Delivery, and Automations provide the
linked context for that investigation. Settings shows effective layered
configuration and validated CAS changes. Observatory and Costs render the same
Plan 26 production observations with denominator, horizon, coverage,
censoring, evidence class, and unavailable reason intact.

**PR17 work item → admitted execution → observed outcome → reviewed
replanning:** a user creates or selects one Plan 24 work item; moves through
Kanban, DAG, timeline, causal, workload, repository, delegation, attempt, and
execution-topology projections without losing TaskId, scope, graph version, or
evidence; runs a legal step through Plan 32 and a real provider adapter;
observes requested/actual identity, lease, progress, artifacts, cancellation,
recovery, tests/CI, integration receipts, and Plan 26 measurements; reviews
independent outcome evidence; and explicitly accepts, rejects, or supersedes a
Plan 24 proposal. No-Git work remains complete. Placement, branch, review, and
integration topology remain independent when repository-linked work is
present.

Every workspace, interaction, state, renderer, accessibility behavior,
performance budget, compatibility surface, and safety rule specified below is
part of one of these journeys or its focused failure/recovery coverage. None is
deferred by this framing.

## Owns

- Navigation, responsive layout, accessibility, interaction state, and client-side presentation.
- Typed API consumption, query caching, optimism only for the closed
  presentation-state allowlist below, and SSE-driven refresh.
- Linked visual exploration across product data and provenance.
- Rendering typed configuration, diagnostics, Doctor findings, legal
  remediation choices, recovery guidance, and operation progress supplied by
  daemon/application owners.
- A renderer-neutral graph/timeline view model with stable node/edge IDs,
  typed relations, selection, filters, clusters, layouts, temporal frames,
  provenance, coverage, and accessible table/text equivalents. Renderer
  adapters own drawing and interaction acceleration only.

## Does not own

- Business rules, authorization decisions, storage, indexing, migration, or repair execution.
- The frontend never starts analyzers, opens LSP connections, merges
  diagnostics, or infers health; it only consumes typed daemon APIs defined by
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md).
- Doctor finding identity, diagnosis, aggregation, severity, health state, or
  remediation orchestration. The canonical Doctor application kernel shipped
  by the PR14 product slice owns those concerns; this dashboard only renders
  its findings and legal actions.
  [Plan 14](14-historical-failure-regression-matrix.md) is the direct
  regression contract for that kernel, not a runtime source of findings.
- An independent Kanban database, developer plan parser/executor, browser-side
  task scheduler, generic orchestration lab, or edit-bundle editor. PR17's
  Kanban/DAG/timeline/causal/workload views are projections over Plan 24
  application state and Plan 32 runtime receipts.
- Git status, worktree, branch-stack, merge, rebase, cherry-pick, ref, index,
  commit, lease, test, or CI authority. Components may submit only an
  application-supplied typed `dry_run`, `apply`, or `cancel` action reference
  and render the resulting receipt. They never construct Git arguments,
  relocate a hunk, infer a merge result, move a ref, acquire/reclaim a lease,
  launch a test, rerun CI, or treat a card move as repository mutation.
- Arbitrary JavaScript workflow authoring or execution.
- Generated compatibility views, route inventories, or a second model of backend behavior.
- Graph/query/storage authority, renderer-local ranking, health, readiness,
  scheduling, repair, or model-route calculations. Ship a permissive default
  renderer; Cosmograph or another commercial/GPU adapter is optional only
  after license, bundle, capability, offline, accessibility, fallback, and
  performance review.

## Finalized implementation architecture (decided 2026-07-23; owner: design)

The shell layout, layout archetypes, token architecture, and component
conventions implementing this architecture are specified in
[Plan 11a](11a-dashboard-design.md); it shares this section's authority.

Build and packaging:

- Rsbuild is the bundler. Decided; no ADR, no comparison, do not revisit.
- One npm package at `dashboard/` producing ONE application: a single shell
  with one lazy, code-split route chunk per workspace. The legacy
  seven-bundle plugin-eval/SDK-injection composition is retired; workspaces
  are ordinary routed modules inside one React tree. Module Federation and
  Vite are rejected; neither is a future seam.
- The Rust integration contract is retained exactly: dist artifacts are
  embedded into the binary at compile time (zero filesystem dependency at
  runtime, offline-complete), `build.rs` keeps the content-stamp
  stale-check + auto `npm ci && npm run build` + fail-fast behavior, and the
  Cargo.toml include whitelist keeps `cargo package` shipping the UI. The
  embedded-asset list moves from per-plugin files to the single app's
  hashed-chunk manifest.
- The Hermes deployment remains a thin wrapper around the same bundles and
  the same Rust server (proxy shim only); it adapts to the single-app
  composition without forking the frontend.

Language, data, and state:

- React 19 + TypeScript in strict mode. npm (lockfile-pinned toolchain,
  including the pinned Playwright).
- React Router owns routes and deep-link mechanics. TanStack Query owns all
  canonical HTTP/SSE-backed server state. Zustand is limited to the bounded
  presentation/optimism allowlist below. Zod validates at the generated wire
  boundary. These replace the legacy hand-rolled `fetchJSON`/hook state
  entirely while retaining exhaustive domain states, revision-monotone
  updates, exact scope, stable IDs, and server-owned product semantics.
- One generated contracts module (`dashboard/src/contracts/`) is the only
  wire boundary: TypeScript types + Zod decoders generated from the Rust
  API-crate schemas. Hand-written request/response shapes are forbidden;
  union switches are exhaustive with `never` checks; unknown variants render
  `unsupported_schema`.
- One SSE module implements the monotone event reducer (dedupe by
  stream/event/revision, stale-generation rejection, refetch-on-gap) and
  feeds TanStack Query patches/invalidations. Workspaces never open ad hoc
  EventSources.

Styling system (design-owned; foundation lanes do not restyle or restructure):

- The following token, palette, theme, typography, spacing, and motion choices
  are design-owner/agent plan decisions. The user supplied no preference on
  typography, colour palette, dark/light mode, spacing scale, motion, or
  easing; never cite these specifics as user authority.
- Tailwind CSS v4, zero-runtime, over a semantic design-token layer expressed
  as CSS custom properties: color/space/type/radius/elevation scales plus
  named tokens for every `DashboardDomainState`, for severity, and for
  evidence-quality — severity and evidence quality are separate token axes,
  never one red/amber/green scale. Dark is the default theme; light,
  `prefers-contrast: more`, and forced-colors are first-class token themes.
- Radix primitives for accessible interaction patterns, composed through a
  small variant layer (class-variance-authority); native elements are
  preferred where a primitive would weaken keyboard, screen-reader, offline,
  or bundle behavior. No runtime CSS-in-JS anywhere (bundle and long-task
  budgets forbid it).
- Do not adopt shadcn yet. Compatibility research may continue, but the current
  user instruction is delivery first and "just leave it for now."
- TanStack Virtual for large lists under the virtualization rules below.

Visualization:

- Renderer-neutral semantics from the generated dashboard contract are the
  only semantic source for graph/timeline views. The historical
  `ProjectionView`/`ProjectionManifest` frontend type design is abandoned; no
  such Rust or TypeScript types exist, and they are not missing PR14 work.
  Sigma.js + Graphology (MIT, WebGL, offline) is the default connected-graph
  renderer adapter — it is the only permissive renderer that honors the
  representative/large graph tiers below;
  `d3-force`/d3 scales remain as layout physics for small ego-views and as
  the scale/axis toolkit for bespoke canvas surfaces (Loom temporal traces,
  conflict heatmaps), which are hand-rolled Canvas/WebGL over D3 scales
  rather than forced through a charting library. ECharts is the single
  quantitative charting library, imported modularly and lazy-loaded per
  route within each route's bundle budget; the earlier Observable Plot
  admission is withdrawn. The user named cosmograph.app as the visual
  benchmark — "i want visuals like that" — not as a library mandate.
  Cosmograph remains only a gated optional GPU adapter for overflow tiers per
  the fallback contract below. No renderer becomes
  graph, query, health, readiness, ranking, or action authority.

Legacy-surface dispositions (from the 2026-07-23 inventory):

- Reachable legacy API families (graph, code-diagnostics, settings,
  holographic/memory, automation, lcm, savings, analytics diagnostics)
  remain the compatibility surface the new workspaces bind to.
- Orphan handlers are deleted with their PR14 replacement, not carried:
  the duplicate holographic fact-proposal family, `fact_trust_history`,
  the unreferenced `automation_jobs_api` family, and the three uncalled
  analytics endpoints. Each deletion lands only when its workspace slice
  ships or the surface is confirmed dead.
- Known backend gaps feeding PR14 are separate completion work, not frontend
  scope: the Doctor finding family has no HTTP surface binding yet. The
  dashboard exposes per-store size/free ratio and whole-store history, but
  Plan 38's per-table `dbstat` samples remain daemon tracing rather than a
  Doctor finding, dashboard payload, or CLI Doctor result. Code-index
  freshness/coverage read models also have no exposure; the PR14 journeys
  require those bindings to exist server-side.

## Frontend ownership and compatibility

The dashboard remains one product package with one responsive shell, one
generated daemon contract boundary, one revision-monotone HTTP/SSE state path,
one reusable evidence surface, and renderer-neutral projection semantics.
Workspaces and inspectors may be reorganized without changing those ownership
boundaries. No workspace may ship as a navigation stub or fixture-only page,
and presentation code must not import or reproduce Git, runtime, task-policy,
CI, persistence, ranking, readiness, health, or remediation authority.

Published workspace and entity URLs remain supported compatibility surfaces,
including Brain, Explorer, Loom, Sessions, Agents, Code, Knowledge, Delivery,
Automations, Observatory, Costs, Settings, Work, and their diagnostic,
Doctor/recovery, work-item, proposal, attempt, and operation inspectors. A
deep link carries opaque scope, selection, entity/graph version, valid and
observation time, filter revision, lens, and evidence-anchor identity. The
execution-topology lens additionally pins the work-item/plan/topology and exact
repository/worktree/ref snapshot identities needed by its contract. Links
never carry private payloads, card or screen coordinates, process/CWD state,
mutable labels, or renderer serialization. Expired, revoked, ambiguous,
denied, stale, missing, or unauthorized identity renders a typed state and
never falls back to title, path, branch label, lane order, active checkout, or
latest version.

## Typed presentation contracts

`DashboardEnvelope<T>` always carries `schema_revision`, exact scope,
entity/graph version, valid and observation time, source watermark,
authorization, coverage, freshness, domain state, legal action references,
and payload. Unknown schema or union variants render `unsupported_schema`; no
default branch may render healthy or empty. TypeScript switches over domain
unions use an exhaustive `never` check.

`DashboardDomainState` is the discriminated union `loading |
complete_zero_findings | ready | partial | stale | locked | denied |
unauthorized | redacted | conflicting | offline | unknown | cancelled |
timed_out | error | unsupported_schema`. `complete_zero_findings` is legal
only when every required
source is supported and completed, coverage is complete, and the canonical
result count is zero. `unauthorized` means identity is absent or expired;
`denied` means a known identity lacks permission; `locked` means a current
lease/CAS/authority blocks an operation while read access may remain legal;
`redacted` retains only server-supplied `source_kind`, opaque source/anchor
ID, source revision, locator class, safe display label, reason code, and legal
action reference; source bytes, prompt/output excerpts, raw paths, arguments,
environment, and secrets are absent. Zero rows or a friendly illustration
never establish a domain state.

The generated workspace payloads and renderer adapter inputs carry stable
entity, edge, cluster, frame, selection, scope, coverage, evidence-anchor,
legal-action, and cursor identities plus accessible node/relation/event rows.
They normalize those semantics for renderer and Work-lens parity. A
projection-specific aggregation declares its hidden stable-ID count and
expansion cursor; it cannot silently drop selected or inaccessible entities.
`ProjectionView` and `ProjectionManifest` were abandoned design names, not
contracts to implement.

`PlannerQueryRequest` is a typed application query, not a browser DSL.
`PlannerQueryRun` carries `run_id`, request/plan/merge revisions, required
source IDs, budgets, cursor, and canonical ordering policy.
`PlannerSourceProgress` carries source ID, typed phase/outcome, completed and
total units when known, watermark, freshness, coverage, omissions, and an
application-defined non-secret error code plus user-safe message. The
[Plan 09](09-application-crate.md) application query coordinator owns source
selection, parallelism, ranking, deduplication, merge precedence, and finality
and exposes the run through [Plan 10](10-api-crate.md). The browser submits one
query, renders independent source progress and partial pages, cancels by run
ID, and ignores stale events. After reconnect it deduplicates by
run/event/revision and refetches on a revision gap.

The frontend does not define or consume an `EvidencePacket` contract. It
renders evidence fields from generated dashboard envelopes through the
`EvidenceTruthStrip`: authority outcomes, freshness, coverage, server rank,
typed scores, retriever contributions, server-authored why-this-result
reasons, citations, omissions, and late-context state. The Rust
`tracedecay_application::EvidencePacket` is a separate live application type
with production consumers and a public export; this frontend correction is
not permission to delete or deprecate it. Every compact result renders the
truth strip with authority, coverage, freshness, citation count, omission
count, and score kind; none may be hidden only in a tooltip or drawer.

- Retriever rows preserve server order and identify retriever/revision, stage,
  contribution/abstention/exclusion/unavailable state, score kind and
  descriptor/calibration revision, reason codes, coverage, and anchor IDs.
- “Confidence” is reserved for a calibrated probability or interval that names
  estimator, calibration revision, cohort, horizon, support, and drift
  validity. Lexical/vector/reranker/heuristic/ordinal values retain their raw
  unit, direction, comparison scope, and revision and never share a normalized
  progress bar or average.
- Coverage carries eligible, examined, matched, excluded, omitted, and unknown
  counts; known cap/sampling state; unit; denominator; and omission reasons.
  Unknown denominators never render `0%`, `100%`, a progress meter, healthy
  styling, or `complete_zero_findings` copy.
- Citations carry stable anchor/source/scope/revision identity, an
  application-supplied content-safe locator from the redacted metadata
  allowlist above,
  content digest when legal, and access state. Expansion rechecks
  authorization and returns `available | redacted | locked | unauthorized |
  denied | stale | revoked | expired | missing | corrupt | partial | error`.
  Stale may expose a successor anchor but never redirects automatically.
  Expanded payloads never enter URLs, local storage, analytics, query keys, or
  durable browser caches.
- Late context is revision-monotone and records pending source IDs plus added,
  removed, and superseded anchor IDs. It preserves focus and announces counts,
  source IDs, authority state, and revision changes only; stale-
  generation updates are discarded and revision gaps trigger a canonical
  refetch. Revocation or deletion immediately replaces already expanded
  content with the typed terminal state.

Client optimism is allowlisted to panel layout, viewport, transient brush,
unsaved query/form input, disclosure open/closed state, playback speed, and
focus restoration. No other optimistic state is permitted. It is prohibited
for health, readiness, rank, coverage, task/graph versions, configuration,
leases/attempts, proposal disposition, remediation, and recovery. Renderer
modules may emit selection, brush, expansion, viewport, and playback intents;
they cannot import command construction, policy/Doctor evaluators, task
evaluators, provider/runtime adapters, or persistence.

## Execution-topology presentation contract

The frontend contract boundary exhaustively decodes the application-owned
`ExecutionTopologyViewV1` and
`ExecutionTopologyEventV1` generated DTOs. It does not redefine Plan 24,
Plan 32, Plan 36, or Plan 37 enums. The view pins one
`WorkProjectionSelection`, graph version, topology revision,
`RepositorySnapshot` digest, valid time, observation time, watermarks,
coverage, freshness, authorization, and canonical ordering policy. Its
payload contains:

- canonical work-item references and optional `worktree_lanes` and
  `branch_stack_lanes`; unsupported, unavailable, denied, partial, stale, or
  omitted lane families remain explicit and never disappear into the base
  Kanban;
- four independently decoded dimensions:
  `execution_placement`, `branch_topology`, `review_topology`, and
  `integration_strategy`. No-Git tasks, unbranched worktrees, local stacks
  without pull requests, and pull-request stacks without managed worktrees
  remain renderable and never synthesize one another;
- GitHub Stacked PR capability state exactly `Unavailable |
  PrivatePreviewDisabled | Enabled | Degraded`, with provider stack ID,
  position, base/head/final-target identity, merge-queue mode, and fallback
  availability only when authorized. Provider stack order never replaces the
  local branch-stack or task DAG;
- separate repository dirty state, native worktree lifecycle/lock state, Plan
  32 lease/authority state, task readiness, runtime state, and evidence health.
  `dirty`, `locked`, `leased`, `blocked`, and `conflicting` are never aliases;
- dependency-commit edges with exact object identity and coverage, and
  application-provided proposed/observed merge-order edges. Commit subjects,
  branch display names, and lane positions are labels only;
- mechanical conflict evidence from native Git intelligence and semantic
  conflict/proximity evidence from Plan 05/37 as independent dimensions with
  their own producer, score kind, calibration revision, coverage, freshness,
  omissions, and anchors. The heatmap never averages them or treats unknown as
  zero;
- integration proposal revisions, exact source/target repository snapshots,
  required dependency commits, predicted impact, required tests/checks,
  alternatives, expiry, evidence, and Plan 24 disposition; plus observed
  native-Git, Plan 32, test, and CI receipts with authority and coverage;
- immutable topology frames and cursors for valid-time/observation-time
  playback. Frames reference events and entities by stable ID and never
  interpolate repository history, invent causality, or replay an effect; and
- application-supplied `TopologyLegalActionV1` values. The action union is
  `RequestDryRun | RequestApply | RequestCancel | RequestExternalHandoff |
  Inspect | ExpandEvidence | Refresh`; each mutating request carries operation/action ID, expected graph,
  work-item, repository-snapshot, runtime, lease-authority and policy versions
  as applicable, idempotency key, expiry, confirmation requirement, and safe
  reason schema.

The generated event union is exactly `SnapshotReplaced |
WorktreeStateChanged | BranchStackChanged | DependencyCommitChanged |
MergeOrderChanged | ConflictProximityChanged | IntegrationProposalChanged |
IntegrationOperationChanged | ReviewTopologyCapabilityChanged |
TestCheckChanged | TopologyFrameAppended`.
Every event carries stream/run identity, event and entity revision, scope,
observation time, source watermark, and coverage. The monotone event reducer
deduplicates by stream/event/revision, rejects stale generations, retains
receipts already observed, and triggers one canonical refetch on a revision
gap. It never derives a branch stack, merge order, conflict result, readiness,
or legal action.

The topology lane board is a synchronized grouping of the canonical selection,
not another board. A work item has one stable selection identity even when
referenced by task, worktree, and stack lanes. Worktree and stack grouping can
be independently enabled only when their lane-family state is `available`;
their off state does not remove entities or change canonical totals.
The dependency-commit rail distinguishes required, present, missing, stale,
denied, and unknown commits. The merge-order rail distinguishes proposed,
accepted-graph, observed-native, superseded, and unknown order; spatial order
never becomes an instruction.

The conflict/proximity view exposes a synchronized accessible matrix with
separate mechanical and semantic columns, relationship paths, freshness,
coverage, omitted counts, and exact evidence expansion. Exact same-range or
symbol overlap remains distinct from configured-threshold proximity.
Denied/private cells expose neither hidden actor, root, address, count, nor
content. Partial or unknown mechanical coverage cannot render “clean merge”;
partial or unknown semantic coverage cannot render “no overlap.”

The execution-topology inspector is rooted in the opaque Plan 24 `TaskId`
(`WorkItemId`) and exact `WorkItemVersionId`. Every lane, rail, heat cell,
proposal, event, receipt, test, and check pivots through that root while
preserving graph/topology/scope/time/watermark/anchor identity. Expansion
rechecks authorization and returns the normal available/redacted/locked/
unauthorized/denied/stale/revoked/expired/missing/corrupt/partial/error
states. A compact card, summary, truncated event tail, or stack alias never
substitutes for the lossless TaskId drill-down.

The integration operation dialog renders only legal actions returned for the
selected exact version. `RequestDryRun` is always effect-free and returns a
new immutable preview or a typed stale/denied/locked/unsupported result.
`RequestApply` is present only where an owning application operation has
mutation authority. Plan 36 exposes it for clean, authorized, no-conflict,
policy-approved fast-forward, two-parent merge, and exact ordered cherry-pick,
as well as its index/commit operations; rebase and force-push never expose it.
GitHub Stacked PR operations expose only an inert explicitly authorized
external handoff, never a browser-owned provider mutation. `RequestCancel`
requests cancellation from
the owning operation/runtime and does not predict whether the native commit
point was crossed. The dialog never optimistically changes a lane, dirty
state, ref, proposal, run, test, or CI result. Reload by operation ID resumes
preview, queued, applying, cancelling, committed, cancelled, partial,
effect-unknown, failed, or recovered receipt state without redispatch.

## Required behavior

- Brain: whole-system and scoped summaries, health, activity, relationships, freshness, and coverage.
- Explorer: pivotable search across messages, sessions, facts, code, projects, repositories, worktrees, and time with provenance visible.
- Loom: interactive temporal and causal traces linking prompts, reasoning, tools, subagents, code changes, branches, commits, PRs, and outcomes.
- Brain, Explorer, Loom, Code, and Work provide zoom, pan, search,
  filtering, brushing, linked selection, semantic clustering, and temporal
  playback within the interaction budgets below over the renderer-neutral view
  model. Stable deep links and scope
  survive overview → finding/entity → investigation → evidence/action
  progressive disclosure.
- Explorer includes a planner-query composer with validation and a plan
  explanation, parallel-source progress, elapsed time and cancellation, typed
  source outcomes, partial result pages, canonical finality, and evidence
  packets. Pending state appears before results; percentages appear only for a
  known denominator. The browser never invents a source, rank, merge, or
  why-this-result explanation.
- Sessions: transcript search, LCM summaries, raw-message drill-down, compaction boundaries, replay context, and provider identity.
- Agents: agent/subagent trees, status, model/provider, handoffs, tool activity, outputs, and failure context.
- Code: symbol search, references, call paths, diagnostics, affected tests, code health, and branch-aware graph freshness; diagnostics show canonical provenance, coverage, freshness, analyzer/gateway state, and conflicts from typed daemon APIs.
- Code replaces any headline universal `quality_signal` with independently
  named typed quantifiers: raw value/unit and numerator/denominator, descriptor
  revision, eligible/covered/unknown/excluded counts, cohort descriptor when
  valid, temporal delta, provenance, and evidence class
  (`measurement | association | calibrated_prediction`). It computes no
  dashboard-local health grade.
- Knowledge: facts, memories, evidence, contradictions, supersession, curation, and cross-project relationships.
- Delivery: changes, commits, branches, worktrees, pull requests, CI, releases, and typed PR17 workflow runs tied to product delivery.
- Automations: schedules, run history, artifacts, approvals, generated skills, memory curation, session reflection, and bounded controls.
- Observatory: hook hints, event flow, latency, failures, daemon health, storage health, queues, and product diagnostics, including canonical analyzer/gateway state, conflicts, coverage, and freshness.
- Costs: provider/model usage, tokens, latency, estimated cost, cache effects, and time/project/session breakdowns.
- Settings: effective layered configuration and application-supplied typed
  patch preview/validation/CAS operations, provider integration, privacy
  controls, retention, and feature controls; it never constructs an
  unvalidated free-form configuration mutation.
- Work (PR17): initiative and work-item views, Kanban, dependency DAG and
  critical path, timeline/history, causal, workload/executor/model, and
  repository/delivery projections over one canonical Plan 24 selection. Every
  card and inspector preserves exact scope/version/evidence, links Plan 32
  lease/attempt/effect history, and renders only application-provided legal
  actions. A lane move never sets readiness directly.
- Work execution topology (PR17): optionally groups that same selection into
  worktree and stacked-branch lanes while preserving task lanes; shows exact
  dependency commits, proposed versus observed merge order, dirty/worktree/
  lease truth, mechanical conflict and semantic proximity side by side,
  integration proposals and receipts, required/observed tests and CI, and
  dual-time playback. The canonical accessible table exposes every entity,
  edge, state, omission, and action available in the visual lane/rail/heatmap
  composition.
- Cross-worktree or cross-branch integration remains a proposal/observation
  journey: exact source/target snapshots → impact/conflict/test evidence →
  application-supplied dry run → explicit legal apply when an owner supports
  it → receipt → independent native/test/CI observation. Unsupported apply,
  stale preview, changed head/base/merge base, dirty target, conflicting
  lease, denied scope, unknown effect, partial checks, and cancelled operation
  are first-class outcomes. The browser never calls Git or CI directly.
- Work task-intelligence views (PR17): task-shape dimensions and calibrated
  ranges; parent/child decomposition comparison and review; ranked eligible
  routes with exclusions, confidence/coverage, requested/actual identity, and
  deterministic fallback; independent-review/outcome evidence; estimate-versus-
  outcome calibration and model-version drift; and live
  split/merge/resize/re-route proposals with explicit accept/reject/supersede
  actions. `Abstained`, `FallbackRecommended`, stale, expired, censored,
  unknown, non-independent review, and insufficient-coverage states are
  visible product states, never blank cards or hidden tooltips.
- Work exposes TaskId-rooted compact context, topology/partition alternatives,
  handoff, escalation, governed experience recall, route and attempt evidence,
  independent review, and exact anchored source expansion. Kanban is one
  projection; cards and summaries never substitute for canonical evidence.
- A proposal preview shows the old and proposed immutable graph versions,
  changed estimates/edges/scope, evidence anchors, expected runtime impact, and
  required separate Plan 32 control. The browser neither recomputes a grade nor
  applies a graph/runtime mutation optimistically.
- Auxiliary-attempt inspectors (PR17) separate the Plan 24 request from the
  Plan 32 lease/attempt. They show requested and actual
  provider/backend/executable/protocol/model/reasoning identity, negotiated
  capabilities and explicit fallback reason, exact worktree/parent lineage,
  sandbox/approval/capability class, bounded context coverage,
  progress/heartbeat and stream coverage, cancellation/kill stage, artifacts,
  resume/reconnect state, and typed terminal outcome. They never display raw
  argv/stdin, environment, secrets, or unredacted provider output.
- `Unsupported`, `Absent`, `Stale`, `Cancelled`, `TimedOut`, `Failed`,
  `Partial`, lost heartbeat, malformed stream, version drift, unknown
  termination, and resume unavailable are distinct visible states. A Claude
  route identifies native Claude Code; a Codex route distinguishes app-server
  from an explicitly approved CLI fallback.
- Work and Doctor views render the one
  canonical PR14 Doctor application finding family whose mandatory regression
  coverage is specified by
  [Plan 14](14-historical-failure-regression-matrix.md):
  Plan 20 desired/effective configuration, Plan 27 observed discovery/
  conformance and remediation references, Plan 32 lease/attempt runtime state,
  and Plan 26 coverage/measurements remain visibly attributable. The UI shows
  unsupported/absent/stale executable, protocol drift, invalid fallback,
  sandbox/capability mismatch, restart/resume failure, stuck lease/attempt, and
  provider availability with evidence, coverage, severity, and owner-specific
  legal actions. It does not infer health, merge findings by label, or invoke a
  private provider/Doctor probe.
- Recovery journeys preserve diagnosis, inert suggestion, canonical operation
  preview, explicit confirmation/human override, owner dispatch, receipt, and
  post-operation verification as separate states. The UI renders only
  application-supplied remediation references, submits a selected reference
  through the typed application API, and never derives remediation from
  diagnosis. Reload by operation ID resumes preview, dispatch, receipt, or
  verification without redispatch.
- Historical Hermes dashboard and delegation UI evidence established useful
  visibility patterns: familiar task lanes, task/run drawers, delegation
  trees, worker inspection, bounded output/event tail, termination,
  diagnostics, dispatch status, and live updates. The old commit and file
  names are evidence, not dependencies or assets to recreate. The Work
  workspace provides those outcomes over canonical
  Plan 24/32 identities, plus explicit stale/partial/recovery state. Kanban is
  one synchronized projection; the delegation tree and attempt timeline are
  linked projections, not alternate task stores.
- Work inspectors expose applicable skill/hint/capability identities,
  provenance, availability, and whether each was delivered to the provider.
  They never render copied prompts, secret-bearing environment, profile
  strings as identity, or provider self-report as accepted completion.
- Every view preserves and displays active scope; cross-scope transitions are explicit.
- Severity/consequence and evidence quality are separate visual and semantic
  dimensions. Coverage, freshness, completeness, missingness,
  sampling/capping, provenance, and uncertainty never collapse into one
  red/amber/green signal.
- Data visualizations have accessible tabular or textual equivalents and keyboard operation.
- Loading, `complete_zero_findings`, ready, partial, stale, locked, denied,
  unauthorized, redacted, conflicting, offline, unknown, cancelled,
  timed-out, error, and unsupported-schema are distinct designed product
  states without color-only cues.
- Large results use server pagination or bounded virtualization; the client never loads an unbounded corpus.
- SSE updates invalidate or patch typed cached data without duplicating server business logic.
- Each product PR includes the UI, tests, and navigation needed to use its behavior; PR14 closes shared-shell and cross-workspace gaps.
- PR17 workflow UI uses typed forms and product operations for concrete workflows; it is not a general JS IDE or plan executor.

## Renderer-neutral interaction and fallback contract

The required default renderer ships under the repository's permissive license
and works offline. Its adapter exposes only mounting, rendering, viewport,
transient-selection, focus, and destruction behavior; callbacks emit stable-ID
presentation intents. Semantic cluster
membership, labels, explanations, causal edges, rank, critical path, legal
actions, readiness, and coverage arrive in generated dashboard payloads and
renderer inputs. An adapter may position or visually bundle entities but may
not create or persist those semantics.

Filtering uses typed application descriptors. A local mask may preview
already-loaded visibility but cannot change authoritative counts, coverage,
saved selection, actions, or complete-zero status; server confirmation
replaces it. Brushing is a transient hit-test preview until committed as
stable IDs. Graph, accessible table, Kanban, DAG, timeline, causal, and
workload views resolve the same selection; missing entities remain labeled
`outside_projection`, `filtered`, `not_loaded`, `denied`, or `stale`.

Temporal playback keeps valid time and observation time separate, consumes
versioned frames/cursors, and supports pause, step, seek, speed, follow-live,
and return-to-live. The browser does not interpolate events into canonical
history or infer causality from proximity. Search, filter, linked selection,
cluster expansion, playback, evidence expansion, and lens changes preserve
scope and deep-link identity.
Execution-topology playback reuses the shared controller and monotone event
reducer. It can replay graph versions, worktree/stack
observations, dependency-commit and merge-order changes, conflict/proximity
evidence, leases/attempts, proposals, operation receipts, tests, and CI
observations. Playback controls are presentation-only: pausing or seeking does
not pause a run, cancel an operation, checkout a ref, or rerun a test.

Cosmograph may be evaluated only as an optional lazy-loaded adapter after
license and transitive-license review. It is never on the default critical
path, never required for a feature, ships no telemetry/remote assets/runtime
downloads, and passes CSP, offline, keyboard, accessible-table, bundle,
semantic parity, WebGL-loss, and memory-pressure gates. GPU picking maps only
to stable model IDs. Unsupported GPU, two context losses in 60 seconds,
initialization over two seconds, restoration failure over one second, or a
representative-tier frame p95 above 33 ms in each of five consecutive
one-second windows falls back within one second without losing URL, scope,
filters, selection, evidence, temporal frame, or legal actions.

## Responsive, accessibility, performance, and usability journeys

**Approved acceptance — desktop-first, not desktop-only (2026-07-28).**
Desktop visual review and golden baselines use 1280×720 and 1440×900. Narrow
viewports are functional/accessibility acceptance surfaces rather than a
second mobile visual-design program: no capability, truth state, provenance,
or action may disappear below `lg`. The Code-workspace 320/768 regression and
zero-axe requirement remain binding.

WCAG 2.2 AA is mandatory. Automated tests cover 320×568, 390×844, 768×1024,
1024×768, 1280×720, and 1440×900 CSS pixels, 200% and 400% zoom,
`prefers-reduced-motion`, `prefers-contrast: more`, and forced colors. At 320
pixels and 400% zoom there is no page-level horizontal scroll, clipped truth
state, lost scope/provenance, or inaccessible action; labeled code/table/graph
regions may scroll internally. Touch targets are at least 44×44 CSS pixels.

Skip links and named landmarks are required. Graph/table widgets use one
active descendant: arrows move deterministically, Home/End move to bounds,
PageUp/PageDown move a viewport, Enter opens, Space selects, and Escape closes
and restores focus. Tab reaches toolbars rather than every graph node. Focus
survives SSE, pagination, virtualization, route/lens and renderer changes.
Canvas/WebGL is supplementary to a synchronized semantic node/relation/event
view; `role="application"` is forbidden. Reduced-motion starts playback paused
and replaces animated layout/zoom/pan with immediate changes. State,
selection, severity, evidence quality, uncertainty, and edge direction use
text/shape/border/pattern as well as color. Query and late-context live regions
coalesce routine updates and announce no more than once per second.

Server pages default to 100 and cap at 500. Virtualization starts above 200
rows, mounts at most 250 row-like elements plus one inspector, preserves
focused/selected entities, and always offers a nonvirtualized paginated mode
of at most 100 rows. Visible metadata includes total when known, loaded range,
sort/filter, cap/partial status, and next-page availability.

Deterministic graph tiers are small 1,000/2,000 nodes/edges, representative
10,000/25,000, large 50,000/150,000, and overflow 100,000/300,000. Raw
rendering above large is forbidden; overflow uses daemon-provided
clustering/slicing or semantic pagination.

**Performance acceptance withdrawn (owner decision, 2026-07-31).** The owner
rescinded every frontend performance acceptance criterion ("dont care about
heap or other fe perf stuff"): the payload ceilings (see "Payload budgets
withdrawn" above), the pinned Playwright measurement rig, the LCP/CLS/
keyboard-ready/input-latency targets, the frame-time and long-task budgets,
the planner first-progress and first-result timing targets, the heap and
retention ceilings, and the sustained-SSE throughput/coalescing rates. No
performance number is measured or gated for acceptance; performance problems
are ordinary bugs. The withdrawal removes only the numeric gates — these
correctness behaviors stand on their own: SSE queue overflow marks the
projection stale and performs one canonical invalidation/refetch rather than
silently dropping events; planner progress remains explicitly pending/partial
rather than presenting a stall as a result; and larger-than-tier selections
use server grouping/paging rather than raw rendering.

Usability acceptance uses exactly 12 participants: at least four keyboard-
only users, three screen-reader users, three users who work at 200–400% zoom
or high contrast, and two regular dashboard/IDE users; cohorts may overlap but
all four cohorts must be represented. Participants must not have implemented
the tested slice. Tasks cover scope identification,
complete-zero versus partial/stale/unknown, exact evidence, graph/table parity,
keyboard query/filter/brush/expansion, truthful query delay and cancellation,
supplied remediation through verified recovery, handoff resume, uncertainty,
unavailable actions, topology lane/table/TaskId parity, mechanical versus
semantic conflict disagreement, stale integration preview, operation-receipt
resume, and valid-time versus observation-time playback. There are zero
wrong-scope, hidden-state, illegal-action, browser-owned Git/CI, or
dispatch-as-recovery outcomes; at least 11/12 complete scope,
evidence, parity, recovery, and action-authority tasks unassisted and 10/12
complete every other task; every screen-reader participant completes the
graph-equivalent task; median Single Ease Question is ≥6/7 and SUS is ≥80.

## Journey implementation and test assets

PR14 implements the finding-to-confirmed-remediation journey as reviewable
slices that remain usable together: generated DTO decoding and exhaustive
state rendering; scope-preserving responsive shell, deep links, inspectors,
and monotone HTTP/SSE state; Brain/Explorer/Loom investigation with planner
progress, linked selection, clustering, playback, evidence packets, and late
context; Code/Observatory/Doctor diagnosis with resumable preview,
confirmation, dispatch, receipt, and verification; complete Sessions, Agents,
Knowledge, Delivery, Automations, Costs, and Settings workspaces with
cross-workspace evidence links; and responsive, accessibility,
assistive-technology, virtualization, renderer parity/fallback, and usability
hardening on that same journey.

PR17 extends the running product with one executable Work loop: one Plan 24
selection across Kanban, DAG, timeline, causal, workload, repository,
delegation, and attempt lenses; proposal diffs, route/exclusion evidence,
requested/actual provider identity, attempts, receipts, and recovery; the
independent placement/branch/review/integration topology dimensions including
no-Git and decoupled cases, dependency commits, merge order, GitHub stack
capability/fallback, dirty/worktree/lease truth, conflict/proximity, tests/CI,
TaskId drill-down, and dual-time playback; and governed dry-run/apply/cancel
controls with stale, denied, locked, unsupported, effect-unknown, crash-safe
receipt, authority-negative, and Plan 26 parity coverage.

Supporting contracts and tests land inside those production slices rather than
as standalone completion phases. Their historical names and layouts are not a
prerequisite spine.

Behavioral tests cover the state taxonomy, planner progress, evidence
expansion, late context, deep links, visual/table parity, renderer fallback,
all Work projections, auxiliary attempts, execution topology, integration
operations, Doctor disagreements, GitHub feedback, graph scale, and SSE churn.
Fixture names and layouts may evolve or consolidate; generated load data is
never product authority, and zero-case or zero-sample runs fail visibly.

Vitest and Testing Library cover contract and DOM behavior, MSW covers
HTTP/SSE faults, Playwright covers supported-browser keyboard, responsive,
smoke, and semantic accessibility behavior, and automated accessibility
tooling covers WCAG checks. Manual
NVDA/Firefox and VoiceOver/Safari completion remains required; screenshots or
automated checks cannot substitute for semantic assertions or assistive-
technology use.

Visual acceptance additionally runs in real Google Chrome, never the
embedded/in-IDE browser whose viewport the user rejected as too small. The
reviewer captures every page and manually clicks through every interaction
state, including movement and live updates. Automated Playwright/Chromium
coverage supports that review but does not substitute for it. Screenshots are
ordinary run output or CI artifacts, not committed per-commit evidence records
or git-hash-tied manifests.

Focused developer commands and the ordinary aggregate frontend/repository test
run may be reorganized as the test layout evolves. That run must execute build,
contract/DOM, accessibility, responsive, renderer parity, authority-negative,
Work topology, SSE, cross-browser end-to-end, smoke, manual
assistive-technology, and usability checks; fail when required cases or samples
do not execute; and preserve the direct all-feature integration checks. Old
script bodies, fixture manifests, and command ordering are historical evidence,
not mandatory recreation.

## Acceptance

- Every page is both beautiful and functional, survives critical real-Chrome
  review, and has no generic/clinical/simple or non-magnificent shipped state.
- The original twelve plan-named workspaces are complete, navigable, and
  accessible by PR14; pending the open responsive decision, they keep all
  functionality below `lg`. Work meets the same bar under the current PR17
  allocation.
- Cross-links preserve scope and provenance across all twelve PR14 workspaces
  and PR17 Work.
- Unit, DOM, accessibility, and smoke tests cover critical journeys and all state classes.
- Performance and payload acceptance criteria are withdrawn (owner decision,
  2026-07-31).
- Direct frontend journeys exercise contract, DOM, accessibility, responsive,
  renderer/semantic parity, authority-negative, SSE, and
  end-to-end behavior. Missing, skipped, or unvisited required states remain
  unresolved.
- Renderer parity compares semantic selection, scope,
  coverage, anchors, state, and keyboard behavior rather than pixels.
- Task-based usability and accessibility tests cover retaining scope across
  progressive disclosure, distinguishing complete-zero from partial, tracing a
  finding to exact evidence, resuming a handoff, understanding uncertainty,
  applying and overriding only legal actions, and distinguishing dispatch from
  verified recovery.
- PR17 DOM/accessibility/parity tests cover decomposition review, routing
  explanation, fallback/abstention, independent-review status, calibration,
  exact model-version drift, censored/unknown outcomes, stale live proposals,
  and explicit human override without browser-local scoring.
- PR17 auxiliary-attempt tests cover provider negotiation, request versus
  attempt lineage, progress/stream truncation, explicit fallback,
  cancellation escalation, restart/resume, artifacts, and all terminal states
  without browser-local process execution, output parsing, provider selection,
  or graph/runtime mutation.
- PR17 execution-topology tests cover no-Git tasks, optional/unsupported
  worktree and local-stack lanes, all four independent dimensions,
  local-stack-without-PR and PR-stack-without-worktree, all four GitHub stack
  capability states plus generic fallback, exact dependency commits, proposed
  versus observed merge order,
  every dirty/worktree/lease state, mechanical versus semantic conflict
  disagreement, required/observed tests and CI, drift/retarget, concurrent
  edit proximity, crash/restart receipt recovery, branch retention, and
  dual-time playback without browser-local Git, scheduler, test, or CI logic.
- Every visual topology reference round-trips through the same TaskId,
  work-item/plan/graph/topology versions, exact repository/worktree/branch
  snapshot, valid/observation time, watermarks, and anchors as its accessible
  row and inspector. Missing, stale, partial, denied, locked, redacted, and
  unsupported data remains visible and cannot fall back to path, lane, branch
  label, current checkout, or latest graph version.
- Authority-negative tests prove `RequestDryRun`, `RequestApply`,
  `RequestCancel`, and `RequestExternalHandoff` are submitted only from application-supplied action
  references with exact expected versions and idempotency identity; duplicate
  clicks return one receipt, stale previews cannot apply, cancellation never
  rewrites a committed receipt, and an ineligible/unsupported native
  integration, rebase, or force operation never gains an apply control.
- PR17 dashboard tests render each canonical auxiliary-provider finding and
  cross-owner disagreement from Plan 14, preserve Plan 20 desired/observed
  revisions and Plan 27/32/26 provenance, and invoke only the supplied typed
  remediation reference. No component-local health formula, implicit config
  write, host repair, lease reclaim, or attempt cancellation is allowed.
- DOM/accessibility tests retain the historically validated familiar derived
  lanes, task/run/delegation drill-down, event tail, diagnostics,
  capacity/blocker reasons, terminal protocol violation, termination,
  skills/hints discoverability, and deterministic restart/recovery without
  browser-owned card status, PID claim, profile routing, or business logic.
- No independent Kanban/task store, developer-plan executor, orchestration lab,
  workflow JavaScript, generated inventory, browser-side model scoring, or
  backend policy duplication remains.
- Direct production-caller tests prove renderer and Kanban code consume supplied
  application results rather than computing identity, rank, semantic clusters,
  causality, readiness, critical path, health, severity, coverage, routes,
  legal actions, or remediation. They prove the browser persists no board,
  task/runtime state, or expanded evidence; never treats lane/order/process
  output as canonical state; rejects stale links/proposals; and preserves the
  same semantics and actions across visual and table/text renderers.
