# TraceDecay V2 Brain Dashboard Implementation Plan

**Goal:** Replace the project-tab dashboard with one profile-wide investigative workbench whose default Brain, Explorer, Causal Loom, domain workspaces, graph lenses, and replay labs make every captured agent, turn, tool, code, Git, delivery, memory, skill, automation, policy, and outcome relationship inspectable while preserving complete sanitized-native evidence and approved V1 semantics.

**Architecture:** A route-lazy React/TypeScript application consumes generated HTTP V2 read-model and command contracts; one URL-addressable `InvestigationStateV1` coordinates scope, time, query, selection, linked composition, comparison, renderer, and inspector across every route. Server-side aggregation and frozen vector watermarks bound data, a single selected snapshot-cache implementation owns typed snapshots, an explicit SSE state machine applies typed deltas, and measured graph/layout/chart/editor implementations expose synchronized outline/table fallbacks instead of attempting a universal graph hairball.

**Tech Stack:** React 19; TypeScript 5.9; the repository's existing Rsbuild/Rspack dashboard pipeline; a TraceDecay-specific URL routing layer selected and recorded in PR 25A rather than inferred from the historical React Router corpus; Web Workers; Vitest/Testing Library; Playwright and `@axe-core/playwright`; Rust/Axum embedded assets and generated OpenAPI/JSON Schema client contracts. PR 25A/25B records a foundation ADR for every additional production routing, cache, virtualization, graph, layout, chart, scale, or editor dependency. TanStack Query/Virtual, Sigma.js/Graphology, deck.gl/custom WebGL, Canvas/worker, ELK.js, ECharts, D3, and CodeMirror are candidates only; selection requires measured reuse, bundle/runtime/maintenance cost, accessibility, deterministic export, CSP, and 10× corpus evidence, and rejected prototypes/dependencies are deleted.

[`20-configuration-control-plane.md`](20-configuration-control-plane.md) is authoritative for `/settings`: its registry generates every form, source chain, validation rule, impact, CLI/API recipe, and drift state. The frontend cannot retain dashboard-only toggles, hidden config, or its own defaults/precedence.

[`18-secret-detection-redaction-and-private-data-safety.md`](18-secret-detection-redaction-and-private-data-safety.md) is authoritative for render/export/clipboard/URL/telemetry eligibility, protected references, redaction controls, and the Privacy workspace. Feature code cannot render an unclassified string or infer safety from a transport shape.

[`21-cli-mcp-tool-surface-and-output-unification.md`](21-cli-mcp-tool-surface-and-output-unification.md) is authoritative for the capability command palette, guided actions, typed output/error/coverage views, and cross-surface examples. The dashboard consumes generated typed views and field descriptors; it never parses MCP Markdown, runs CLI commands, or recreates pagination/rendering semantics.

[`22-incremental-context-scout-and-suggestion-envelopes.md`](22-incremental-context-scout-and-suggestion-envelopes.md) owns Context Scout state, delivery/outcome semantics, Observatory controls, Loom lane, and Hint Lab replay. [`23-session-lcm-temporal-retrieval-and-evaluation.md`](23-session-lcm-temporal-retrieval-and-evaluation.md) owns the Search Quality Lab's temporal/current/as-of/copy/summary-DAG explanations and evaluation contracts; its formerly separate "Search Lab" is folded into the Search Quality Lab and is not a distinct lab or route. The browser only renders those typed views.

[`24-canonical-task-plan-graph-and-multi-agent-executor.md`](24-canonical-task-plan-graph-and-multi-agent-executor.md) owns the Work product semantics: initiative/plan/task/attempt inspectors, boards as saved canonical `TraceQueryV1`, dependency DAG, critical path, timeline/causal/workload/executor/repository/agent/All lenses, advisory-work-claim versus authoritative-lease overlap evidence, context-packet inspection, and Orchestration Lab. The frontend never stores board-local tasks, defines a task query DSL, derives readiness, selects routes, or treats drag/drop as arbitrary status mutation.

UI implementation reading path is fixed: plans 10/17 for generated transport contracts; plan 21 for cross-surface view/rendering semantics; plans 18/20 for safety and effective configuration; plans 22/23/24 for scout, temporal retrieval/evaluation, and Work behavior; plan 26 for accounting/denominators; then this plan for interaction and visualization. Product/design review starts here, then follows those same ownership links before changing behavior. No role may treat this file as a replacement domain contract.

---

## 1. Contract lock

This plan refines master-plan PRs 4A and 25–32. It depends on the V2 domain, query, policy, application, API, hook, and tool-catalog plans; it does not move their business logic into the browser.

1. `/` means active-profile **All/Brain**, not the most recently selected project. A project page is a saved filtered investigation over the same models and components.
2. There is one logical Brain and several typed graph lenses. Git ancestry, code calls, thread/Turn membership, agent delegation, time order, memory similarity, and automation lineage remain distinct edge vocabularies and visual encodings.
3. A `Turn` is a first-class interval with the context visible at its start and the messages, provider-visible reasoning artifacts, goals, tools/results, files/code, hints/retrieval/memory, costs, and state produced by its end.
4. Every aggregate states exact versus sampled counts, denominator, hidden count, coverage, watermark, and aggregation/layout version. Unknown is never displayed as zero; inferred is never displayed as observed; correlation is never styled as causation.
5. Sanitized native records are never silently discarded. Merged PR #410's copied-subagent-prompt dedupe and domain `MessageOrigin`/`MessageView` become explicit UI modes with representative and hidden-copy counts plus source provenance; protected plaintext, if retained, is available only through the separate elevated quarantine workflow.
6. Canonical V2 read models and commands are the application/API responsibility. Feature modules do not join raw endpoints, synthesize source-of-truth counts, interpret tool schemas, or write stores directly.
7. Every lab evaluator is read-only against production state. The shared experiment operation may persist only immutable replay artifacts and explicitly granted model/egress cost. Exact, recorded-result, and current best-effort are distinct persistent labels; unavailable historical inputs are shown as unavailable, not substituted silently.
8. The app is local-only and loopback-secured by default. Sensitive literals never enter URLs, browser history, analytics, SSE subscription URLs, cache keys persisted in clear text, clipboard links, or catalog rows.
9. Accessibility, mobile behavior, table parity, partial/offline behavior, deterministic export, and visual QA are acceptance gates in every feature PR. PR 32 audits and polishes them; it does not introduce them for the first time.
10. Existing plugins keep their complete read and write behavior until the owning V2 workspace passes behavior/action parity. There is no blanket read-only transition.
11. No decorative dashboard-card grid, fake metric, gratuitous badge/pill, particle field, 3D graph, perpetual force animation, or color-only evidence encoding is allowed. Use open canvases, rails, lists, matrices, tables, timelines, and one clear focal artifact. “World class” means faster and more accurate investigation, not visual spectacle.
12. The shipped UI is code-native. Image-generated concepts are implementation references, not rasterized application screens.
13. Doctor/provider/daemon branding is evidence-driven: foreign-owned packages are informational, partial integrations are labeled partial, and upgrade completion requires a durable drain/recovery receipt rather than a green icon inferred from process exit.
14. Repository/worktree cleanup is daemon-authoritative. The browser renders only generated eligibility, blocker, retention, operation, and receipt views and invokes only a generated `legal_capabilities` action; it never lists/removes filesystem entries, runs Git/CLI commands, reads a database, or treats `task-graph edit clean` as physical worktree cleanup. Plan 21's command deletes only a managed task-edit bundle. A Git-worktree cleanup control remains unavailable until plans 08/09/21/24 publish the distinct operation-specific capability and sealed views.

The normative publication snapshot is [master §2.6](../2026-07-09-tracedecay-brain-rewrite.md#26-current-master-accepted-changes) plus [plan 13](13-research-provenance-and-context-anchors.md). The UI inventory also exposes untracked branch/session variants, bounded consolidation progress, lifecycle deferrals, registry-healing decisions, FTS maintenance health, graph-checkpoint safety, and restart-safe applied-manifest retirement as typed evidence rather than hidden repair.

## 2. Product model and design direction

The product is an evidence workbench for one interconnected system:

```text
profile
  ├─ projects / repositories / worktrees / refs / commits / PRs / releases
  ├─ threads / sessions / Turns / messages / visible reasoning / context / goals
  ├─ actors / agents / subagents / delegations / handoffs / workflows
  ├─ tools / code symbols / files / patches / diagnostics / tests / impact
  ├─ facts / decisions / contradictions / retrieval / trust / memory versions
  └─ schedules / curator-reflector-skill-writer runs / candidates / autonomy decisions / effects / recoveries / skills / outcomes

identity + time + scope + evidence + provenance + coverage connect every lane
```

The interface has one visual thesis: **Evidence Cartography — a quiet investigative instrument whose stable atlas, temporal strata, and causal threads make a living system navigable without turning it into a hairball**. Dense evidence is revealed through semantic zoom, linked brushing, lens composition, and progressive disclosure rather than by surrounding the user with equally weighted cards. This is the north-star hypothesis, not permission to freeze a palette or renderer before concept validation.

### 2.1 Concept contract before implementation

PR 4A/25B must use the frontend design workflow to generate **three genuinely different, complete visual directions against the same frozen redacted corpus**. One direction must test Evidence Cartography; the other two must challenge its hierarchy, typography, spatial model, and interaction language rather than merely recolor it. The principal user selects one direction only after a recorded critique and task walkthrough. No production component, palette, typeface, icon family, renderer dependency, or motion token is frozen before this gate.

Each direction includes one coherent storyboard, not a hero-header fragment:

- Brain desktop at `1440×1000` across L0 profile, L1 territory, L2 neighborhood, and L3 evidence, with stable positions through zoom/expansion;
- Brain mobile portrait at `390×844` and landscape at `844×390`, with focused neighborhood, one temporal lane, command and inspector sheets;
- Causal Loom desktop with overview density, agent tree, lanes, replay playhead, selected Turn, synchronized graph/transcript/code-diff, and impact wake;
- Universal Explorer desktop/mobile with query, precise result table, graph/matrix pivot, explain drawer, derived timeline lane, and comparison collection;
- Experiment cockpit with baseline/candidate columns, pipeline trace, branch graph, sweep matrix, explanation/output diff, and a saved reproducibility recipe;
- Work task detail plus Repository Work cleanup overlay/queue on desktop and mobile: related repositories/worktrees/branches/PRs, active agents/attempts, retention clocks, blockers, safe confirmation evidence, and terminal receipts must remain legible without creating a second workspace inventory;
- Atlas, Trace, Compare, Lab, and Triage workspace compositions in dense, sparse, partial, error, light, dark, reduced-motion, and 200% text states;
- the complete loading, empty, stale, partial, offline, locked, redacted, incompatible, context-loss, and fatal state board.

Concepts use realistic fixed-corpus labels and distributions, preserve the route/data anatomy in this plan, and invent no metric or action. Review rejects generic card mosaics, default node-link diagrams, interchangeable SaaS styling, decorative glow, illegible miniature charts, detached legends, hover-only values, unstable layouts, and screens that cannot show dense real evidence. Approval records the chosen direction, rejected alternatives, design rationale, task failures, and an extraction ledger for container model, type system, icon/glyph family, semantic marks, spacing, motion, and responsive continuation. Save approved references under `dashboard/design/concepts/`; implementation verification compares exact-state browser captures and transition recordings against them with `view_image`.

### 2.2 Screen anatomy

Desktop, width `>= 1120px`:

```text
┌ command/status bar: scope · time · query · live/as-of · compare · health · save/export ┐
├ left outline/filter rail ┬ dominant canvas/table/timeline ┬ universal inspector          ┤
│ 240–360 px, resizable    │ flex, never nested in a card   │ 320–520 px, resizable       │
├──────────────────────────┴ bottom density/time brush, route-owned, optional ────────────┤
└ status line: snapshot · coverage · hidden/sampled · retention · latency · privacy ──────┘
```

- At `840–1119px`, the outline collapses to a drawer; the inspector remains dockable or becomes an overlay.
- Mobile portrait presents the primary evidence surface first. Outline/filter/inspector are separate bottom sheets with apply/cancel/reset and restored focus.
- Mobile landscape provides a two-region graph/timeline + inspector layout; it is not a stretched portrait stack.
- Panel sizes are user preferences. Entity/time/query selection remains shared investigation state.

“Dominant canvas” means a clear focal surface, not one fixed renderer. Five registered compositions cover the product without creating five state systems:

| Composition | Purpose | Bounded slots |
|---|---|---|
| **Atlas** | Orient across the whole Brain and move between semantic zoom levels. | stable topology + aligned temporal activity + inspector |
| **Trace** | Follow a workflow, session, agent, Turn, or task through time and impact. | Causal Loom + transcript/code/diff + inspector |
| **Compare** | Explain differences between entities, intervals, branches, versions, or runs. | A/B or baseline/variants + aligned diff + evidence ledger |
| **Lab** | Replay, branch, sweep, minimize, and reproduce one engine decision. | pipeline/branch graph + synchronized result surfaces + manifest rail |
| **Triage** | Rank and resolve health, privacy, coordination, retrieval, or work issues precisely. | table/matrix + trend or neighborhood + inspector |

Each composition has one to four typed slots, a registered layout, bounded split geometry, dock state, and active slot. All slots share one frozen/live snapshot, scope/time/query, selection, brush, coverage, inspector, and history transaction. A user may replace a slot's renderer or save its geometry, but cannot create a renderer-local filter or second selection store.

### 2.3 Generated visual-semantic ontology

Create one catalog-generated ontology in `dashboard/packages/design-system/src/visual-ontology.ts` and semantic CSS variables in `tokens.css`. It is the only authority for entity-family silhouette/glyph, scope contour/cluster container, evidence ring, relation stroke/arrowhead, temporal/freshness treatment, coverage/privacy texture, label priority, semantic zoom/LOD representation, selection/focus/compare state, and icon meaning. Graph, timeline, table, chart, inspector, export, and accessibility outline consume the same entries; feature code may not create a local legend or reuse a mark for a different meaning. Unknown catalog entries render through an explicit neutral fallback and remain inspectable.

The state-color portion is:

| Meaning | Color role | Required redundant encoding |
|---|---|---|
| Neutral observed context | neutral foreground/surface | solid line or plain shape |
| Primary committed selection | focus accent | thicker outline + focus marker |
| Comparison A/B | comparison A/B accents | `A`/`B` glyph + line pattern |
| Direct causal evidence | causal accent | arrow + solid connector + text evidence class |
| Inferred correlation | correlation accent | dotted connector + confidence label |
| Temporal-only proximity | muted | hairline/no arrow + “temporal” label |
| Warning/error | severity scale | icon + text + severity word |
| Stale/partial/offline | state scale | hatch/dash + state label |
| Sensitive/redacted/locked | privacy scale | lock/redaction icon + text |
| Late-arriving event | ingest accent | outlined timestamp marker + occurred/ingested times |

The ontology is validated in contrast, grayscale, deuteranopia, protanopia, and tritanopia screenshots plus collision fixtures for adjacent entity families, edge classes, nested scopes, partial/privacy overlays, focus versus selection, and comparison variants. Generated legends are local to the current composition and expose only visible semantics. Domain categories register reviewed shapes/icons through the catalog; they cannot repurpose state colors or ship feature-local encodings.

### 2.4 Concrete design language and tokens

The visual language is quiet, technical, high-density, and evidence-first: neutral surfaces, one focus color, restrained semantic accents, strong typography, direct labels, and open canvas bands instead of nested card chrome. Both light and dark are first-class; neither is an inversion filter. The table below is a measurable **starting hypothesis**, not a locked brand system: the approved §2.1 direction may replace fonts, palette values, radii, and spacing while preserving accessibility, density, semantic-role, local-font, and performance constraints. `tokens.css` publishes approved primitives only through semantic aliases, and charts/Canvas/WebGL consume the generated ontology rather than hardcoded colors.

| Family | Required baseline tokens |
|---|---|
| Font | Concept phase tests at least two locally bundled/system typography pairings with distinct information hierarchy; no network font dependency. Evidence/code uses a legible local monospace and all time/count/cost roles use tabular numerals. The extraction ledger records the approved families, weights, fallback metrics, and licensing. |
| Type scale | `caption 12/16`, `body-sm 13/18`, `body 14/20`, `label 14/18/600`, `title-sm 16/24/600`, `title 20/28/650`, `display-sm 28/36/650`, `display 40/48/650`; no viewport-dependent type below `12 px`. |
| Spacing | Base `4 px`; named steps `0, 4, 8, 12, 16, 24, 32, 48, 64`. Layout gutters `16/24/32`; evidence-row indentation `16`; no arbitrary one-off spacing without a concept-ledger exception. |
| Density | `compact=.875`, `comfortable=1`, `spacious=1.125` applied to spacing/row-height tokens; list rows `32/40/48 px`. Pointer targets remain at least `44×44 px` regardless of density and may use invisible hit padding. |
| Radius | `0` for evidence tables/canvas seams, `4 px` controls, `8 px` sheets/panels, `12 px` dialogs; fully rounded shapes only for a real binary toggle/compact tag, never every label. |
| Border/elevation | `1 px` semantic border is the default separation. Elevation 0 none; 1 = `0 1px 2px rgb(0 0 0 / .12)`; 2 = `0 8px 24px rgb(0 0 0 / .18)`. Dark mode uses border/luminance before shadow. No floating-card shadow mosaic. |
| Focus | `2 px` focus ring plus `2 px` offset, tokenized and never clipped; focus color meets 3:1 against adjacent surfaces. Selected and focused are distinct states. |
| Motion | durations `80 ms` feedback, `140 ms` selection, `220 ms` spatial transition; `cubic-bezier(.2,.8,.2,1)`. Nothing loops. Reduced motion makes state changes immediate while retaining non-motion marks and announcements. |

Palette hypotheses, subject to concept approval and automated contrast correction before token freeze:

| Semantic token | Light | Dark |
|---|---:|---:|
| `canvas` / `surface` / `surface-raised` | `#f7f8fa` / `#ffffff` / `#ffffff` | `#0b1017` / `#121a24` / `#182330` |
| `text` / `text-muted` / `border` | `#171a1f` / `#5c6573` / `#d9dee7` | `#edf2f7` / `#a8b3c2` / `#314050` |
| `focus` / `selection-fill` | `#1457d9` / `color-mix(in srgb, #1457d9 12%, transparent)` | `#79a8ff` / `color-mix(in srgb, #79a8ff 16%, transparent)` |
| `causal` / `correlation` / `temporal` | `#087f5b` / `#6f42c1` / `#667085` | `#4fd1a8` / `#c4a7ff` / `#98a2b3` |
| `warning` / `error` / `privacy` | `#a35a00` / `#b42318` / `#7a3e9d` | `#ffb454` / `#ff8078` / `#dda5ff` |

Tinted backgrounds use `color-mix(in srgb, var(--semantic-color) N%, var(--surface))` at registered `8/12/18%` strengths; components do not embed hex/rgba. CI verifies WCAG contrast at every text/icon/border role in both themes and blocks a token version that fails. Theme, density, contrast, and reduced-motion changes update CSS variables without remounting route state or renderers.

### 2.5 Hermes Kanban UI heritage disposition

Plan 13 PR 2A must assess Hermes dashboard files, tests, and interaction flows individually before implementation. Compatible React/TypeScript components, tests, styles, and interaction logic may be `direct_port` under the recorded MIT notice; behavior may be `behavioral_port`; incompatible data/plugin/SQL boundaries are `redesign`; rejected paths are `drop`. The ledger pins source/file spans, destination, provenance/license handling, source-to-test mapping, and the regression that proves any redesign is at least as strong. The generated V2 client and canonical state model remain mandatory regardless of disposition:

| Hermes pattern | Disposition | V2 result |
|---|---|---|
| Task drawer sections: metadata, status/actions, diagnostics, dependencies, attachments, comments, events, worker log, run history | **Assess for direct/behavioral port** | Reuse compatible components/tests or map behavior into generated Work inspector-panel descriptors and the task/attempt subpanels in §13.0; preserve one canonical selection and URL, not drawer-local truth. |
| Structured diagnostic actions with unknown-kind fallback | **Port contract** | Render plan 24 `DiagnosticEnvelopeV1`; unknown actions stay visible/disabled, and generated `legal_capabilities` controls invocation. |
| Attention strip, server-age rings, progress pill, multi-select | **Assess then improve** | Reuse compatible interaction/tests; use server/projector attention signals, explicit staleness labels, exact completed/total/unknown progress, and named transactional batch commands such as `work_items.assign_set`. No client clock/status inference. |
| Theme variables, `color-mix` tints, density multiplier | **Adopt** | Implement §2.4 light/dark semantic tokens and density/accessibility invariants. |
| Full-board long poll/refetch, single component/store, client-only filtering, unvirtualized columns | **Drop** | Use server `TraceQueryV1` projections, typed SSE deltas, route containers, isolated view instances, and virtualized rows/columns. |
| Dark-only hardcoded colors, title tooltip as sole explanation, `window.confirm`/`prompt`, emoji status | **Drop** | Both themes, direct text/icon/shape encoding, accessible dialogs/forms, typed confirmation commands, no emoji as evidence/state. |
| Pre-action confirmation as the only recovery | **Beat** | Ordinary task/view edits commit as versioned commands and expose exact history plus registered reopen or compensating-transition actions where the domain permits; there is no generic undo token. Archive/soft-delete retains identity/history. Irreversible external effects use plan-09 confirmation and compensation semantics. Autonomous curation remains fully policy-driven with no per-item undo/apply/reject queue. |

## 3. Information architecture and route ownership

`dashboard/app/src/routes.tsx` defines route metadata once: path, label, feature owner, required capabilities, lazy import, default renderer, keyboard help, and migration-only legacy paths. Current route metadata/help never advertises a stale name.

The persistent responsive workspace switcher is the primary product map; command search complements it but never replaces navigation. Its six groups are **Brain**, **Investigate** (Explorer, Loom, Sessions, Agents, Coordination, Code, Knowledge, Delivery), **Work** (initiatives, plans, tasks, attempts, executors, scheduler, saved views), **Operate** (Automations, Evolution, Observatory, Privacy, Costs), **Labs**, and **Settings**. It preserves pinned and recent workspaces. Observatory routes are read/diagnose surfaces and Settings routes are configure/control surfaces; cross-links preserve the exact entity, scope, time, and operation rather than duplicating controls.

| Route | Primary question | Feature owner | Default artifact |
|---|---|---|---|
| `/` | What is TraceDecay doing, learning, changing, and failing? | `features/brain` | clustered profile topology + aligned activity |
| `/explore` | Where is this evidence and how is it connected? | `features/explorer` | result table + selected pivot |
| `/timeline` | What happened, in what order, and what did it affect? | `features/causal-loom` | density + virtualized causal lanes |
| `/sessions`, `/sessions/:id`, `/turns/:id` | What context and work occurred in this thread/Turn? | `features/sessions` | session list / Turn evidence outline |
| `/agents`, `/agents/:id` | Which agents collaborated and with what outcomes? | `features/agents` | delegation tree + Turn sequence |
| `/work`, `/work/initiatives/:initiativeId`, `/work/plans/:planId/versions/:version`, `/work/tasks/:workItemId`, `/work/attempts/:attemptId`, `/work/offers/:offerId`, `/work/packets/:packetId`, `/work/executors`, `/work/scheduler`, `/work/edit-bundles/:editBundleId`, `/work/notifications`, `/work/notifications/:notificationId` | What work exists, how is it gated/routed/executed, and what context/outcomes connect it to the Brain? | `features/work` | initiative/plan outline + saved Kanban/DAG/task/attempt projection + managed declarative edit workspace; saved Work projections open the canonical `/saved/:viewId` resource; offer, packet, notification, and edit-workspace IDs deep-link to exact typed details |
| `/coordination` | Which nearby agents may overlap, and what safe action is warranted? | `features/coordination` | evidence-ranked presence/overlap ledger + worktree map |
| `/goals/:id` | How did this Codex goal or provider-native objective evolve and finish? | `features/agents` | versioned goal/plan/Turn evidence ledger |
| `/workflows/:id` | How did the captured provider workflow execute? | `features/automations` | run waterfall + artifact lineage |
| `/code`, `/code/entities/:id`, `/code/compare` | What code changed, depends on it, and is affected? | `features/code` | symbol/snapshot graph + code viewer |
| `/graphs/:lens` | Open one graph vocabulary over shared state | thin generated route preset into the shared Brain/Explorer graph slot; no `features/graphs` package | lens-specific renderer |
| `/knowledge`, `/knowledge/facts/:id`, `/knowledge/entities/:id` | What does TraceDecay know and why? | `features/knowledge` | fact/version/provenance views |
| `/delivery`, `/projects/:id`, `/projects/:id/branches/:branch`, `/pulls/:id` | What was produced, observed, or encountered in Git/delivery? | `features/delivery` | Git/PR graph + evidence ledger |
| `/automations`, `/automation/runs/:id`, `/skills`, `/skills/:id`, `/evolution` | How is the system autonomously curating and improving itself? | `features/automations` | complete run/skill inventory + autonomy decision/effect/outcome ledger |
| `/observatory` | Is capture, storage, projection, privacy, and query healthy? | `features/observatory` | project × subsystem matrix |
| `/observatory/context-scout` | Is incremental suggestion preparation useful, timely, quiet, private, and healthy? | `features/observatory` + `features/hints` | trigger/silence/envelope/delivery/outcome funnel + queue/model/tool/host state |
| `/observatory/sync` | Is the shared Brain connected, converged, private, backed up, and correctly fenced? | `features/observatory` + `features/brain-topology` | node/store/repository/sync topology + lag/spool/conflict/recovery matrix |
| `/privacy` | Is the mandatory sanitizer effective across every source/sink, and what is blocked or needs remediation? | `features/privacy` | coverage/unknown matrix + safe remediation lineage |
| `/costs` | Where do tokens, latency, and cost go? | `features/costs` | precise ledger + trends |
| `/playgrounds/:lab` | What would this versioned engine decide, and why? | `features/playgrounds` | shared replay workbench |
| `/saved/:viewId` | Reopen a classified saved investigation | `features/saved-views` | saved route + state |
| `/settings`, `/settings/context-scout`, `/settings/integrations`, `/settings/brain` | Which effective settings, capabilities, host integrations, enrolled Brain nodes, and placements govern behavior? | `features/settings` | scoped settings form + source labels; context-scout renders plan 22's controls, integrations renders plan 27 host bundles, and Brain renders plan 28 node/enrollment/grant/placement/replica/backup/failover controls |

Legal `:lens` values are `git`, `code`, `threads`, `agents`, `turns`, `tasks`, `plans`, `memory`, and `automation`. A timeline is a Loom overlay/composition, not a tenth graph destination. Legal `:lab` values are `hints`, `retrieval`, `ingest`, `query`, `search-quality`, `scope-federation`, `correlation`, `coordination`, `orchestration`, `scheduler`, `memory`, `policy-diff`, and `privacy`; the canonical `/evolution` workspace supplies the fourteenth lab through its shared `lab` composition: Hint, Retrieval, Search Quality, Coordination, Orchestration, Ingest, Query, Correlation, Scheduler, Memory, Policy Diff, Evolution, Scope/Federation, and Privacy. The `privacy` slug displays as "Privacy & Secret Safety Lab" and supersedes the retired `secret-safety` route name; plan 23's temporal "Search Lab" content is folded into the Search Quality Lab, not a separate lab. Hint Lab includes deterministic and incremental-scout replay, Search Quality includes temporal session/LCM retrieval, and Orchestration replays plan/task/executor/context/lease decisions at `/playgrounds/orchestration` (the only route spelling). Every route is a configured view over plan 10 §8.5's generic experiment/run API and a catalog-owned typed evaluator; no lab gets a bespoke HTTP lifecycle. `/work/views/:id`, `/playgrounds/evolution`, and `/graphs/timeline` are migration-only redirects to `/saved/:id`, `/evolution`, and `/timeline`; they never appear in current route metadata.

Route changes do not clear scope/time/query/selection unless the destination cannot represent the selected entity. In that case, the selection stays pinned in the inspector and the main view explains the unsupported relation. Browser back/forward restores complete committed investigation states, not only route names.

Superseded route names are recorded here so no ledger dangles: the master plan's `/threads` and bare `/turns` list views are served by `/sessions` and the `threads`/`turns` lenses; `/activity` becomes the registered Brain/Explorer `triage` saved preset rather than a route; `/agents/nearby` became `/coordination`; `/proposals` is retired by the no-approval-queue autonomy model and its content lives under `/evolution`; `/secret-safety` became `/playgrounds/privacy`. Bounded migration-only mappings live in `migration-paths.ts` and disappear at cutover; no current route metadata advertises the stale names.

## 4. Shared investigation state

Generate the only persisted cross-feature state model from plan 01 into `dashboard/packages/query-state/src/investigation.ts`; the frontend adds no wire fields or enums. The TypeScript below is the generated shape shown for implementation review:

```ts
// VisualSelectionV1, VisualSelectionAtomV1, VisualSelectionOriginV1, and
// SelectionActionV1 are generated from plan 01; query-state imports them and
// defines no browser-local selection union.

export type UniversalPanelIdV1 =
  | "summary" | "evidence" | "relations" | "native" | "history" | "actions";

// Additional panels are catalog-generated capability descriptors, not a
// route-specific union or conditional tab switch.
export interface InspectorPanelRefV1 {
  panelOwner: RegistryEntryId;
  panelId: RegistryEntryId;
}

export interface VisualizationStateV1 {
  rendererSpecId: RegistryEntryId;
  graph: GraphCompositionSpecV1 | null;
  viewport: SchemaBoundValueRef;
  scaleState: SchemaBoundValueRef;
  lanes: readonly RegistryEntryId[];
  lod: VisualizationLodV1;
  playhead: UtcMicros | null;
  synchronizationGroup: RegistryEntryId | null;
}

export interface WorkspaceCompositionV1 {
  kind: "atlas" | "trace" | "compare" | "lab" | "triage";
  layoutId: RegistryEntryId; // registered layout@version
  slots: readonly {
    id: RegistryEntryId;
    artifact: "graph" | "timeline" | "table" | "matrix" | "distribution" | "small_multiples" | "transcript" | "code_diff" | "manifest";
    dock: "primary" | "left" | "right" | "bottom" | "overlay";
    sizeBasisPoints: number;
    visualization: VisualizationStateV1;
  }[]; // 1..=4, unique IDs; validated against the composition registry
  activeSlotId: RegistryEntryId;
}

export interface InvestigationStateV1 {
  version: 1;
  profileId: ProfileId;
  scope: {
    selector: ScopeSelectorV2;
    resolution: ScopeResolutionV2 | null;
  };
  time: {
    occurred: InvestigationTimeRangeV1;
    knowledgeAsOf: UtcMicros | null;
    live: boolean;
    compare: null | readonly [InvestigationTimeRangeV1, InvestigationTimeRangeV1];
  };
  query: {
    queryFingerprint: PrivacyDomainBoundLocatorDigest | null;
    protectedDraftId: ProtectedDraftId | null;
    facets: readonly FacetSelectionV1[];
    messageView: MessageView;
  };
  focus: {
    selected: VisualSelectionV1 | null;
    retrievalAnchors: readonly RetrievalAnchorId[];
    retrievalRecipeId: RetrievalRecipeId | null;
    pinned: readonly EntityRef[];
    path: readonly EntityRef[];
    collectionId: EntityId | null;
  };
  view: {
    composition: WorkspaceCompositionV1;
  };
  inspector: { panel: InspectorPanelRefV1 };
}
```

Route lens slugs map explicitly to generated application enums: `git→Git`, `code→Code`, `threads→Thread`, `agents→Agent`, `turns→Turn`, `tasks→Task`, `plans→Plan`, `memory→Memory`, and `automation→AutomationSkill`. This mapping is generated/fixture-tested; the fixture asserts that the route `:lens` slug list and generated `GraphLensV1` enum stay identical (nine entries) so a lens can never be routable but URL-unrepresentable. The active graph slot's `visualization.graph.primary_lens` follows the route; its bounded overlay lenses and bridge kinds remain explicit URL/saved state. Feature code never title-cases or guesses an enum.

Selection, comparison, and inspector semantics:

- Every selection kind the inspector supports (section 9.2) is a generated domain `VisualSelectionV1` variant, including entity set/lasso, event, relation, path, aggregate membership digest, time range, facet, and bounded comparison. URL encoding is a generated safe opaque-ID/digest codec; protected facet values remain behind `protectedDraftId` and never enter the URL.
- Period comparison lives in `time.compare` as explicit A/B ranges; entity/aggregate/path/relation/time-range pair comparison is a `comparison` selection. The section 9.1 compare toggle populates exactly one of these two homes; there is no third comparison shape.
- The six `UniversalPanelIdV1` panels are always registered. Plan 24 §12.6's Work-only content is generated as entity/capability panel descriptors owned by the task catalog, not added to a frontend union: specification, dependencies, acceptance, assignments, attempts, packets, decisions, impact, costs, and audit descriptors declare supported entity kinds, required capabilities, ordering, and nearest legal fallback. Persist `{panelOwner,panelId}`. When selection changes, migrate explicitly to the nearest legal panel with an announced focus-preserving notice; never accumulate route conditionals or retain an unusable tab.
- The `audit`/`evidence` tabs for plans, documents, initiatives, work items, attempts, agents, and subagents expose versioned research manifests from plan 13: immutable manifest-entry `ResearchAnchorId`, nonempty canonical `RetrievalAnchorId` links, contributor/session/role/output attribution, unresolved attribution gaps, source/catalog/Git watermarks, drift, coverage, redaction state, and retrieval recipe. Selecting an entry resolves evidence only through its canonical retrieval anchors; the UI never treats a research entry ID, response handle, URL, or search rank as payload authority. "Copy evidence" copies the canonical anchor plus optional manifest-entry context.
- `view.composition.layoutId` and every slot's renderer/layout/viewport/scale/lanes/LOD/playhead/synchronization state are committed independently in URL/saved-view state. Values come from composition/renderer/lane registries. Restoring an unregistered composition, slot artifact, renderer, layout, overlay lens, bridge kind, or lane ID shows an explicit reset notice and falls back to the closest legal preset — a restored URL never silently no-ops.

### 4.1 State ownership

- **URL:** route; opaque profile/repository/project/worktree/ref/entity/saved-view IDs; non-sensitive time bounds; renderer/lens/layout; the committed generated `VisualSelectionV1` safe projection; facet IDs; transcript mode. Serialize arrays in stable sorted order and omit defaults.
- **Encrypted profile storage:** query literals, annotations, collections, protected saved views, replay input payload references, redaction decisions. URLs hold only an opaque `protectedDraftId` scoped to the local profile.
- **IndexedDB:** versioned bounded response cache, local uncommitted annotation drafts, route recovery checkpoint, deterministic layout coordinates keyed by snapshot/query/layout version. Payloads obey server sensitivity and retention metadata; locked/profile-sign-out purges protected records.
- **Local preferences:** theme, density, keyboard layout, panel geometry, last nonsensitive route, reduced-motion override. Never store entity payloads here.
- **Renderer-local:** hover, lasso-in-progress, drag state, provisional camera, worker job, GPU buffers. Commit selection/camera only on interaction end.

`ScopeSelectorV2` (defined in plan 16 §4) and `ScopeResolutionV2` (defined in plan 01) are imported from the generated contract transported per plans 10/17; query state does not define another `mode/include/projectIds` selector. URL serialization retains the canonical selector's nonsensitive opaque roots/exclusions/time/policies/limits and the resolution ID; candidate details and safe aliases stay in the bounded cache. `retrievalAnchors` are server-issued domain `RetrievalAnchorId`s for session/thread/Turn/message/agent/subagent/workflow/goal/Git evidence. The browser uses `retrieval_anchors.metadata_batch_get` for bounded safe identity/state labels, `retrieval_anchors.resolve` only when an authorized inspector requests exact record/payload content, and `retrieval_recipes.execute` for a protected versioned rerun; it never treats metadata as payload authority. Anchors survive cursor, SSE, and migration response-handle expiry. Sensitive retrieval inputs remain behind `protectedDraftId`; copied links, saved views, collections, annotations, exports, and route recovery carry a `RetrievalRecipeV1` (defined in plan 01) or protected recipe ref, never an ephemeral response handle alone.

Scope defaults and ambiguity behavior are fixed:

- A new investigation starts at explicit active-profile `All`; cwd, last route, recent project, and selected entity never narrow it silently.
- The chooser is a lazy tree: All → repository → project → checkout/worktree → ref/snapshot, with explicit multi-select and an entity collection overlay. Project routes apply a visible filter over the same state.
- Every selected/candidate scope shows kind, canonical disambiguated `owner/repository/project/worktree/ref` label, authorized provenance, index generation, and fresh/stale/partial/unavailable state. Same-name items are never distinguished by color or truncation alone.
- Name/path/alias input calls generated scope resolution. Ambiguity opens a keyboard/touch-accessible candidate list; choosing one resubmits the preserved canonical request with its signed retry token in one step. The UI does not rebuild the query, guess by cwd, or ask the user to retype it.
- CLI/MCP/API equivalents exported from the workbench use the same opaque IDs and resolution token; scope semantics, candidates/order, provenance, coverage, and errors must match exactly.

`dashboard/packages/query-state/src/url.test.ts` must prove canonical round trips (including every generated `VisualSelectionV1` variant and A/B compare range), default elision, back/forward, unknown-version refusal, the lens-slug/`GraphLensV1` equality fixture, legal/illegal overlay and bridge combinations, every registered composition/slot layout, unknown layout/lane-ID reset behavior, and absence of sensitive literal fixtures. `history.ts` debounces replace-state during brushing but pushes selection, route, compare, and committed time changes.

### 4.2 Transcript modes and #410 seam

Every transcript/session/Turn/search surface shows a persistent mode control with the seven generated plan-01 `MessageView` values. Each response includes the generated `TranscriptVisibility` block — a plan 10/17 schema type re-exported through `app/src/contracts`, reproduced here for reference and never redefined as a local interface:

```ts
export interface TranscriptVisibility {
  mode: MessageView;
  rawRowCount: number;
  normalizedRepresentativeCount: number;
  visibleCount: number;
  hiddenCopyCount: number;
  hiddenByKind: Readonly<Record<"copied_parent_prompt" | "subagent" | "protocol_tool_result", number>>;
  representativeSets: readonly {
    representativeEventId: string;
    memberSourceRefs: readonly string[];
    algorithm: string;
    confidence: number | null;
  }[];
}
```

- `native_rows` compiles to domain `MessageView::NativeRows` and shows every sanitized stored source row plus original order/source offsets and redaction/coverage state.
- `normalized_representative` is the default and compiles to `MessageView::RepresentativeRows`; copied/native rows may group, but each group displays the suppression count, representative rule/version, source observations, expansion cursor, and every represented entity ID.
- `human_best_effort` compiles to `MessageView::HumanBestEffort`; each row retains domain `MessageOrigin` and unknown-origin counts remain visible.
- `direct_user` compiles to `MessageView::DirectUser` and shows excluded delegated/protocol counts plus one-click mode change.
- `delegated_agents` compiles to `MessageView::DelegatedAgents` and shows parent task/agent evidence.
- `tool_results` and `provider_protocol` compile to `MessageView::ToolResults` and `MessageView::ProviderProtocol` respectively; wrapper protocol is never conflated with actual tool results.

“Show native rows” follows `messages.expand_native` or issues `NativeRows` at the same frozen snapshot. The frontend does not create a combined “both” result/count or a second classification algorithm.

The exact generated wire values are `view=native_rows|representative_rows|human_best_effort|direct_user|delegated_agents|tool_results|provider_protocol`; frontend aliases are mapped by a checked generated table and never serialized by title-casing or guesswork.

No empty state says “no messages” when another mode contains records. It says, for example, “0 direct-user messages; 18 delegated prompts and 42 protocol/tool rows are available.” Exports record mode, native/visible/hidden counts, representative membership, algorithm version, and privacy-domain-bound source fingerprints.

### 4.3 Saved-view, collection, and annotation records

These are the shareability deliverables. All three are generated in the plan 10/17 schema (routes in plan 10 §8.6) and re-exported through `app/src/contracts`; the browser defines no parallel record. `SavedViewV1` is consumed exactly from plan 01—owner actor/scope, classification/redaction, `Investigation|Task|Experiment` definition, live-or-frozen snapshot manifest/watermark, complete sharing policy, active bundle, expiry, version/timestamps/revocation. Its experiment variant also preserves selected run, cell, stage, comparison, comparison cell, reduction, and playhead. This plan intentionally does not repeat a TypeScript `SavedViewV1` shape that could drift. Collection and annotation reference shapes add frontend size/interaction detail:

```ts
export interface CollectionV1 {
  id: string;                          // PK
  version: number;
  name: string;                        // unique per (ownerScope, name)
  ownerScope: DeclaredScope;
  memberAnchors: readonly string[];    // RetrievalAnchorId only; no embedded records
  memberRefs: readonly { kind: "entity" | "event" | "relation"; id: string }[];
  recipeRef: string | null;
  watermark: string;
  annotationRefs: readonly string[];   // AnnotationV1 IDs
  createdAt: string;
  updatedAt: string;
}

export interface AnnotationV1 {
  id: string;                          // PK
  version: number;
  ownerScope: DeclaredScope;
  target:
    | { kind: "anchor"; anchorId: string }
    | { kind: "range"; laneId: string; from: string; to: string };
  bodyRef: string;                     // encrypted-profile-storage ref; plaintext never leaves it
  sensitivity: DataSensitivity;
  audience: "private" | "profile" | "explicit_grants";
  redactionState: "none" | "redacted" | "pending_sanitization";
  createdAt: string;
  updatedAt: string;
}
```

Keys, indexes, retention, and size envelopes: primary key is `id` for all three; uniqueness is `(ownerScope, name)` for saved views and collections; the server indexes saved views by owner/definition-kind/route-or-lens/expiry, collections by owner, and annotations by target anchor. Investigation, task, and experiment variants use the same store, optimistic version, classification/sharing/redaction state, share plan/start/revoke, grant/subscription invalidation, expiry, reopen/reauthorize, and frozen-input unavailable behavior; there is no `SavedTaskViewV1` or `SavedExperimentViewV1` table or ID. Investigation scenes form a merge-free parent-linked trail: the user can retrace, branch, narrate, share, and export a bounded evidence story while every reopen reauthorizes anchors and reports snapshot/layout drift. The experiment variant references immutable experiment/run/cell/stage/comparison/comparison-cell/reduction/playhead identity and never embeds manifests, inputs, or outputs. The browser holds records only in the bounded IndexedDB cache (section 8.1), with annotation bodies and protected query literals confined to encrypted profile storage. Size envelopes: a serialized `SavedViewV1` is `<= 32 KiB` (state plus refs; no payload text and at most `100` scene refs); a `CollectionV1` holds `<= 10,000` member anchors/refs and serializes to `<= 256 KiB`; an `AnnotationV1` body is `<= 4 KiB`. Records over-envelope are rejected by command validation with the exceeded bound, not truncated silently. Embedded `InvestigationStateV1` restores through the same versioned codec as URLs: unknown versions are refused with an explicit incompatible state.

## 5. Frontend repository and package structure

The rewrite creates focused packages; `app` remains composition glue:

```text
dashboard/
├── design/
│   ├── concepts/                         # approved reference images, desktop/mobile/state boards
│   ├── extraction-ledger.md              # tokens, type, icons, container and motion inventory
│   └── fidelity-ledger.md                # concept/render mismatches and fixes
├── app/
│   ├── index.html
│   └── src/
│       ├── main.tsx                      # one React root
│       ├── app.tsx                       # providers + router only
│       ├── router.tsx                    # browser router/history fallback contract
│       ├── routes.tsx                    # route metadata and lazy imports
│       ├── providers.tsx                 # query, investigation, theme, capability providers
│       ├── error-boundary.tsx
│       ├── generated/
│       │   ├── catalog.ts                # tool-catalog generator output; never hand-edited
│       │   └── commands.ts               # tool-catalog generator output; never hand-edited
│       ├── shell/
│       │   ├── WorkbenchShell.tsx
│       │   ├── CommandBar.tsx
│       │   ├── OutlineRail.tsx
│       │   ├── InspectorDock.tsx
│       │   ├── TimeBrushDock.tsx
│       │   ├── CoverageStatusLine.tsx
│       │   └── MobileSheets.tsx
│       ├── contracts/                    # UI-safe aliases/compositions over official client types
│       ├── shared/
│       │   ├── inspector/                # universal typed evidence/history/actions inspector
│       │   ├── renderers/                # graph/DAG/canvas/matrix/table + frozen export scene
│       │   ├── charts/                   # ECharts wrapper, labels, descriptions, table parity
│       │   └── code-viewer/              # code/diff/message/source/redaction views
│       ├── features/
│       │   ├── brain/                    # topology, aggregate/neighborhood LOD, matrix, workers
│       │   ├── causal-loom/              # density/lane/agent/transcript/impact timeline
│       │   ├── playgrounds/              # shared lab workbench plus typed lab pages
│       │   ├── activity/                 # remaining entries are domain feature boundaries, not packages
│       │   ├── explorer/
│       │   ├── graphs/
│       │   ├── sessions/
│       │   ├── agents/
│       │   ├── coordination/
│       │   ├── work/
│       │   ├── code/
│       │   ├── knowledge/
│       │   ├── delivery/
│       │   ├── automations/
│       │   ├── observatory/
│       │   ├── hints/
│       │   ├── privacy/
│       │   ├── costs/
│       │   ├── evolution/
│       │   ├── saved-views/
│       │   └── settings/
│       └── migration-paths.ts           # bounded pre-cutover mappings; empty after cutover
├── packages/
│   ├── api-client/                       # thin browser auth/bootstrap binding over root packages/tracedecay-client
│   │   ├── package.json
│   │   ├── src/client.ts
│   │   ├── src/errors.ts
│   │   ├── src/sse.ts
│   │   └── test/{contract,sse}.test.ts
│   ├── data-client/src/
│   │   ├── query-client.ts
│   │   ├── keys.ts
│   │   ├── snapshots.ts
│   │   ├── subscription.ts
│   │   ├── delta-reducer.ts
│   │   ├── offline-cache.ts
│   │   └── capability-gates.ts
│   ├── query-state/src/
│   │   ├── investigation.ts
│   │   ├── defaults.ts
│   │   ├── url.ts
│   │   ├── history.ts
│   │   ├── store.ts
│   │   ├── persistence.ts
│   │   ├── selection.ts
│   │   ├── research.ts                 # stable anchors/recipes; no response handles
│   │   └── protected-drafts.ts
│   ├── design-system/src/
│   │   ├── tokens.css
│   │   ├── typography.css
│   │   ├── reset.css
│   │   ├── visual-ontology.ts          # generated from plan-08 catalog; no feature-local semantics
│   │   ├── icons.tsx
│   │   ├── controls/
│   │   ├── layout/
│   │   ├── table/
│   │   ├── states/
│   │   └── a11y/
│   └── testing/src/
│       ├── fixtures.ts
│       ├── render.tsx
│       ├── a11y.ts
│       ├── fake-sse.ts
│       └── deterministic-time.ts
├── tests/
│   ├── contract/
│   ├── component/
│   ├── e2e/
│   ├── visual/
│   ├── accessibility/
│   ├── performance/
│   └── fixtures/
├── build.mjs
├── build.shared.mjs                      # existing programmatic Rsbuild/Rspack configs and asset inventory
├── vitest.config.mts
├── playwright.config.ts
├── tsconfig.json
├── package.json
└── package-lock.json
```

Rules:

- A production file targets `<= 500` lines; no new file may exceed `800` lines. Route modules contain composition and data loading, not renderer algorithms or endpoint joins.
- Root `packages/tracedecay-client` is the only OpenAPI-generated TypeScript HTTP/problem/SSE schema/runtime. Dashboard `packages/api-client` is a small browser cookie/CSRF/bootstrap binding and re-export layer over that official package; it contains no generated schema or competing pager/event/problem types. `app/src/contracts` provides UI-safe aliases/compositions over the official generated schema and may not duplicate transport/domain types. Tool-catalog generation owns `app/src/generated/{catalog,commands}.ts`; UI contracts re-export their typed IDs/schemas rather than generating a second catalog.
- Initial physical package admission is deliberately smaller than the responsibility map: only `api-client`, `data-client`, `query-state`, `design-system`, and `testing` are dashboard packages. UI-safe aliases live at `app/src/contracts/`; Brain, timeline, inspector, labs, renderers, charts, code viewer, and domain workspaces live under `app/src/features/` or `app/src/shared/`. Promote one to a package only after two independent production consumers and a measured bundle/build benefit; a route using two components is not two consumers. ESLint boundaries enforce ownership without package proliferation.
- `packages/data-client` is the only owner of TanStack Query keys, snapshot caching, SSE, IndexedDB cache, and capability gating.
- `packages/query-state` is the only owner of URL/history/persistence semantics.
- `app/src/shared/renderers`, `app/src/shared/charts`, and the Brain/Causal-Loom feature visualization modules own drawing; other feature modules supply typed models and callbacks. These are ESLint ownership boundaries, not packages.
- Every feature has a route/container boundary and pure presenters. Containers call generated-client query/command hooks, translate the one `InvestigationStateV1`, and select a generated read model; presenters accept sealed models plus typed callbacks and never fetch, join endpoints, read URL/storage, inspect raw problems, or infer legal actions.
- Generated-client hooks are the sole network entry (`useSnapshotQuery`, `useCursorPage`, `useSubscription`, `useCapabilityCommand`, and `useOperation`). A lint forbids feature imports from `fetch`, Axum routes, raw OpenAPI internals, and V1 client modules.
- Global state is limited to the committed investigation, auth/capability/theme, and query cache. Each open saved-view/render instance owns an isolated `ViewInstanceStateV1` keyed by `(saved_view_id, instance_id)` for draft facets, grouping, column geometry, expansion, local selection gesture, scroll/window cursor, and per-slot transient viewport gesture layered over committed `VisualizationStateV1`. Closing an instance destroys it; changing one board cannot mutate another instance or a canonical task.
- Optimistic UI follows one closed state machine: `idle → optimistic_pending → accepted_projection | conflict_reverted | rejected_reverted | operation_pending → accepted_projection | failed_reverted`. The pre-command model/version is retained until an accepted projection arrives. Conflicts render server/current versions and legal retry/rebase actions; partial batch results revert only failed items. This is client-state reconciliation, not a curation preview/apply/rollback workflow.
- Each renderer creates at most one Canvas/WebGL context and one worker pool. Hidden routes suspend workers and animation frames and release large GPU buffers after a bounded idle period.
- `features/*` may depend on packages, never another feature's internal files. Shared functionality moves to a package after two proven consumers.
- No package imports V1 plugin source. Migration adapters live at the route boundary only while the explicit migration flag is active and disappear at cutover.
- The frontend uses one package manager: npm with committed `package-lock.json` files (`npm ci` in CI) for both `dashboard/` and the separately versioned root `packages/tracedecay-client` workspace. The dashboard consumes only the client's built artifact. No pnpm lockfile, script, or command appears in either workspace.

## 6. Frontend build ADR and embedded-asset boundary

Create `docs/adr/dashboard-v2-build-boundary.md` before extending the production asset graph. It documents current authority; it is not a bundler-selection ADR. Repository evidence fixes the starting topology:

- `dashboard/package.json` uses npm and runs `node build.mjs`; there is no Vite script, dependency, config, or provisional authority.
- `dashboard/build.mjs` orchestrates the production build, while `dashboard/build.shared.mjs` constructs Rsbuild/Rspack configs programmatically. Production emits the standalone shell plus one single-file bundle per legacy plugin, aliases plugin React imports to the host shim, then emits the Hermes wrapper and source stamp. There is no checked-in `rsbuild.config.ts` and no Module Federation topology.
- `dashboard/dev/run.mjs` starts one Rsbuild HMR graph with the `/api` proxy and real shared React for local development.
- Rust `build.rs` inventories sources/assets, rebuilds missing or stale dashboard artifacts through the npm script when needed, and publishes the asset stamp. `src/dashboard/assets.rs` embeds the emitted JS/CSS with `include_bytes!`/`include_str!`; the shipped binary has no Node, Rsbuild, or Rspack runtime dependency.
- V2 adds its application entry, lazy chunks, manifest, and history fallback through that existing pipeline while legacy plugin bundles coexist behind the bounded product cutover. A future bundler change is a separate intentional product decision with its own proposal, evidence, migration, rollback, and approval; Task 2 neither evaluates nor authorizes one.

Historical Rsbuild, Rspack, Vite, and React Router discussions and external repositories are retained only as frozen scenario/retrieval/scope conformance evidence. Their names in fixtures never make them TraceDecay product dependencies, supported build targets, required repositories, migration destinations, or reasons to run a bundler bakeoff.

The ADR records non-comparative regression evidence for the current pipeline:

| Dimension | Required evidence |
|---|---|
| Production build | cold/warm duration, peak RSS, emitted chunks, gzip/Brotli sizes, deterministic hash behavior |
| Development | startup, first transform, HMR latency for TSX/CSS/worker, proxy/SSE behavior |
| Rust embedding | manifest generation, base path, history fallback, immutable asset cache headers, `include_bytes!` integration |
| Code splitting | shell budget and lazy graph/timeline/editor chunks without dynamic public paths |
| Security | CSP without `unsafe-eval`, worker loading, source-map publication, asset path containment |
| Product cutover coexistence | current single-file plugin bundles and Hermes wrapper only behind the bounded per-domain migration flag |
| Tests | Vitest, Playwright, type checking, coverage, deterministic production preview |
| Packaging | fresh `cargo package`, crates.io prebuilt assets, no Node at runtime/docs.rs |
| Product cutover risk | changed scripts/files, rollback path, bounded old/new shell coexistence, atomic removal of stale live routes/names at cutover |

Acceptance validates the existing pipeline's correctness, embedding, CSP, history behavior, determinism, and product-cutover safety. It is not a cross-bundler scorecard or benchmark gate. The V2 extension must:

- emit `dashboard/app/dist/asset-manifest.json` with content hash, content type, byte size, entry/chunk relationship, integrity hash, and source stamp;
- keep the initial shell JS `<= 250 KiB gzip` and CSS `<= 80 KiB gzip`;
- lazy-load graph, timeline, editor, labs, and each domain workspace;
- serve every `/api/*` request to Axum, never history fallback;
- preserve current `build.rs` behavior: missing assets build when Node exists, packaged prebuilt assets avoid Node;
- produce identical public URLs and manifest hashes on two clean builds with fixed toolchain/inputs.

`dashboard/build.mjs`, `build.rs`, `src/dashboard/assets.rs`, `src/dashboard/mod.rs`, `Cargo.toml` package includes, and `tests/dashboard_api_test/api.rs` change together. Keep `dashboard/build.shared.mjs` and old dist emission until the last legacy plugin retires.

## 7. Generated contracts, read-model envelopes, and commands

The plan 10/17 contract generator emits discriminated unions for entity/event/relation kinds, `ScopeSelectorV2`/`ScopeResolutionV2`, `RetrievalAnchorRecordV1`, sink-eligible/redacted content states, query rows, commands, capabilities, privacy status, replay records, `ApiProblem`, and SSE events at root `packages/tracedecay-client/src/generated/schema.ts`. CI runs generation then requires a clean tree. Dashboard `packages/api-client` is only the thin browser-auth binding over that artifact, not a divergent generated client fork.

Every route consumes the generated plan 10/17 `ApiResponse<T>` without redefining its envelope:

```ts
export type ReadModelEnvelope<T> = ApiResponse<T>;
```

`ApiResponse.meta` always includes request/use-case, protocol, catalog digest, `ScopeResolutionV2`, snapshot, coverage, freshness, redactions, retention, applied limits, and warnings. Feature code cannot construct a smaller meta object.

Paged feature data uses the one generated `CursorPage<T> { items, next_cursor, truncation, count_semantics, ordering }` defined in plan 17's contract IR; Brain/graph/timeline data carries its own generated LOD and allowed-action fields. `CoverageStatusLine` renders all nonempty dispositions, sampling/truncation, freshness, redactions, retention, warnings, and a “why?” link. A feature may specialize the copy but cannot return `data` alone or drop `meta`.

Problems use the one generated `application/problem+json` `ApiProblem { problem_type, title, status, code, detail, instance, retry, current_version, restart, current_binding, candidates, invalid, operation }`. `packages/api-client/src/errors.ts` preserves these fields while redacting response/token bodies from logs. Invalid fields bind to form controls; `current_version` opens conflict review; `current_binding` supports cutoff recovery; candidates drive scope disambiguation; `operation` opens durable status; `restart` invalidates cursor/subscription/snapshot as directed. Retry never guesses from HTTP status alone.

Commands use generated types and this interaction contract:

1. Capability advertises action, scope, destructive class, operation-specific inspection/plan requirement, and required version.
2. When required, UI opens the named typed inspection/plan. It lists descendants, redactions, holds, irreversible effects, exact scope, and immutable confirmation; ordinary commands have no preview screen and autonomous curation has none.
3. Confirm submits an opaque idempotency key and `ifVersion`/watermark; it never reuses a timed-out key for different input.
4. `409` presents current-versus-requested state and offers refresh/review, never blind retry. Public bare native session IDs are invalid input; the migration inspector alone may render provider/profile candidate guidance without hydration. A provider-qualified native locator can still return multiple generation/variant candidates and requires explicit canonical selection.
5. Accepted commands return operation/event IDs; UI follows their projection status through the live feed and links to audit evidence.

The dashboard never exposes arbitrary SQL, file path mutation, shell, or policy bytecode execution.

## 8. Cache, snapshot, and live SSE model

### 8.1 Query keys and cache bounds

`packages/data-client/src/keys.ts` uses:

```ts
type QueryKeyV1 = readonly [
  "v2",
  profileId: string,
  capabilityVersion: string,
  operationOrDatasetId: string,
  accessDecisionDigest: string,
  schemaRegistryDigest: string,
  catalogDigest: string,
  queryFingerprint: string,
  scopeFingerprint: string,
  timeFingerprint: string,
  snapshotOrWatermarkFingerprint: string,
  representationVersion: string,
  messageView: MessageView,
  cursor: string | null,
];
```

Sensitive text is represented only by an opaque server-issued fingerprint. Keys use canonical catalog operation/dataset identity, never route or feature names. Domain routes are generated query presets returning the same sealed page envelope; they cannot create feature-local cache adapters. Frozen snapshots are immutable and cacheable until retention/schema invalidates them. Live snapshots have bounded freshness and are changed only by typed deltas or full resync. IndexedDB stores at most the last 20 nonsensitive route snapshots and a configurable protected-cache quota; LRU eviction deletes payload chunks before metadata. Cache entries carry schema/access/retention digests and are rejected, not migrated heuristically, on mismatch.

Abort previous route/brush requests on supersession. Prefetch only the selected entity inspector and adjacent timeline page; never prefetch all shards or payload bodies.

### 8.2 SSE state machine

Subscriptions are created by `POST /api/v2/subscriptions` using a protected request body and return `{ subscription_id, expires_at, snapshot_watermark, replay_retention, stream_path }`. The browser opens the returned `GET /api/v2/subscriptions/{id}/events` with `Last-Event-ID` only on resume and invokes `subscriptions.revoke` through `POST /api/v2/subscriptions/{id}:revoke` on explicit close when reachable. Query literals never appear in the SSE URL, subscription ID, event ID, or logs.

```text
idle → snapshot_loading → live
live + duplicate/out-of-order → live (idempotently ignored)
live + coverage delta → live/partial (visible status change)
live + gap/resync-required → stale_visible → snapshot_loading
live + network loss → reconnecting → live | stale_visible | offline_visible
any + auth/schema/access mismatch → blocked with explicit recovery
```

The generated `ApiStreamEvent` union is `Snapshot | Delta | Operation | Projection | Coverage | Gap | ResyncRequired | ServerNotice`. `delta-reducer.ts` accepts only increasing authenticated stream event IDs, then applies generated per-change stable IDs/vector watermarks idempotently; it does not invent a `(shard, entity)` identity that operation or aggregate streams may lack. It preserves remove/upsert boundaries, applies `Coverage` as first-class state, and batches animation-free DOM commits per frame. `Operation` follows command/job/workflow/export/migration/automation receipts to explicit terminal state and never treats HTTP `202` as completion. A gap freezes the last-known-good snapshot, disables mutation commands tied to its version, and announces the resync. Exponential backoff uses jitter and pauses while the page is hidden or offline. Subscription IDs and `Last-Event-ID` remain page-memory-only capability material. Resume occurs only within advertised retention; otherwise the server sends `ResyncRequired`.

The client treats 15-second SSE comment heartbeats as liveness only; they consume no semantic sequence and never trigger React updates. The server queue is bounded at 256 frames/2 MiB per connection. A slow consumer receives resync/close behavior; the browser cannot continue displaying the stream as complete after that close.

Tests inject duplicate, out-of-order, coalesced, missing, stale, schema-changed, access-changed, slow-consumer, and disconnect/reconnect streams. No test may use time sleeps; fake clocks and explicit event advancement make the state machine deterministic.

## 9. Workbench shell and universal inspector

### 9.1 Command/status bar

Left to right:

- product/home mark and route breadcrumb;
- profile-wide scope control defaulting explicitly to All, with lazy repository/project/worktree/ref hierarchy, multi-selection, disambiguated safe labels, provenance/freshness, and ambiguity retry;
- occurred-time range plus live/frozen/as-of state;
- global query opener and keyboard shortcut;
- compare toggle with explicit A/B periods/entities;
- coverage/health summary;
- save, share, and export actions;
- command palette and settings.

The palette is generated from the versioned capability/tool catalog. Each item shows intent, read/mutate class, evidence source (`local semantic`, `live delivery`, `joined`), required scope, estimated cost, and unavailable reason. Git-intent searches offer the catalog-generated guided inputs for branch listing/search/diff, PR and commit context, changelog, session lookup, and workflow capabilities; the palette enumerates these entries from the versioned catalog at build/run time and never hardcodes a V1 tool-name list that could fossilize. Joined GitHub/local actions display both freshnesses and a reconciliation state; drift never looks like a unified truth.

### 9.2 Universal inspector

The inspector works for entity/set, event, aggregate, path, relation, facet, time range, and comparison selections—each a generated `VisualSelectionV1` variant (section 4), so every supported safe selection kind is shareable and restorable through the URL:

- **Summary:** type, label, time/scope, observed/inferred status, coverage, key measures.
- **Evidence:** supporting observations/events, source position and privacy-domain-keyed fingerprint identity, evidence class, algorithm, and confidence.
- **Relations:** incoming/outgoing typed relations, legal pivots, redacted frontier counts, bounded expansion.
- **Native:** sanitized native source row, normalized observation, canonical event, projection row, schema/privacy versions; authorization and transcript mode apply. This tab never opens protected-quarantine plaintext.
- **History:** versions, valid/observed intervals, supersession, corrections, late arrivals.
- **Actions:** generated allowed commands with preview/audit; no action is inferred from UI entity type alone.

Every inspectable research entity exposes “copy stable anchor” and “re-run retrieval recipe.” Resolution shows identity/version/watermark drift and coverage before navigation. A cursor or old response handle can page a current result but is never displayed as the durable research identifier.

Every message, Turn, session, agent, tool invocation/result, hint, fact/memory version, task/attempt, policy evaluation, code/Git entity, and saved selection also exposes the catalog-generated **Fork to Playground** action when at least one evaluator accepts it. The mapping preserves the stable anchor, scope, occurred/as-of time, snapshot/watermark, recorded component versions, current composition/selection, and a backlink to the source scene. The destination shows every editable overlay as a typed diff from the frozen source; unsupported or unavailable inputs remain visible. Feature code cannot hardcode lab routing or silently paste current state into a historical experiment.

Aggregate selection lists exact or sampled membership, denominator, hidden counts, expansion cursor, watermark, and algorithm version. Relation selection explains endpoint identities and why the connector is causal, structural, inferred, similarity, or temporal. Inspector tabs are keyboard-addressable; closing restores focus to the selected mark/row.

## 10. Brain / All implementation

The default Brain answers one question in this reading order:

1. **First-scan claim:** one server-authored `ConsequenceClaimV1` naming the most consequential current activity or health issue plus scope, time, coverage, and confidence, or explicitly abstaining.
2. **Focal topology:** recent project/workflow/agent plus initiative/plan/task/attempt clusters connected by typed activity, dependencies, blockers, leases, and acceptance evidence, selected to match the claim and current time window.
3. **Aligned activity:** work/attempt, agent, code, delivery, knowledge, and automation lanes below the same time window.
4. **Health guardrail:** compact project × subsystem matrix for ingest, projection, query, storage, privacy, and remote freshness.
5. **Learning loop:** hint/tool/fact/skill/automation candidate→use→outcome funnel with unresolved horizon.
6. **Resume:** unfinished initiatives, plans, work items, attempts, blockers, expiring leases, pending acceptance/review, workflows, goals, agent runs, and saved investigations.

This is a spatial reading path, not six equal cards. On desktop the topology dominates; subordinate sections use open bands. On mobile the claim, one focused cluster, and one activity lane appear first; health/learning/resume live in sheets.

First run never opens an empty analytics wall. With real evidence, a four-step guided investigation moves Brain territory → selected causal path → Turn in Loom → Fork to Playground and can be dismissed or resumed without creating a second state model. With no eligible evidence, the UI explains capture/project enrollment and offers an isolated bundled synthetic tour clearly labeled “example data”; it cannot mix example and real counts, persist example entities into production stores, or present a fabricated health claim. Returning users land on the truthful Brain and may resume any saved scene/experiment from the Resume band.

`ConsequenceClaimV1` is a generated application read model, never freeform browser/model prose. It carries the bounded candidate universe, registered score components and weights, winning entity/relation IDs, evidence anchors, scope/time/watermark, coverage threshold and observed coverage, algorithm/policy version, confidence, and `abstain_reason`. The claim always exposes **Why this?** and the compared candidate set; below-threshold or tied evidence abstains instead of manufacturing importance.

All/Brain is federated across the selected repositories/projects/worktrees/refs. Nodes, aggregate membership, edges, inspector titles, tables, and exports retain canonical repository/snapshot identity and per-shard provenance/freshness/partial state. Same-name projects, branches, files, symbols, sessions, or agents use disambiguated labels and never merge by display text; cross-repository connectors require typed dependency/session/workflow/Git/evidence relations.

### 10.1 Brain aggregate tile contract

`BrainTile` is the generated plan 10/17 aggregate-tile read model re-exported through `app/src/contracts`; the browser never declares it as a local interface. Reference shape:

```ts
export interface BrainTile {
  id: string;
  kind: RegistryEntryId;
  label: SinkEligibleText;
  membership: AggregateMeasureV1;
  activity: ReadonlyMap<RegistryEntryId, AggregateMeasureV1>;
  edgeMeasures: readonly {
    relationKind: PredicateId;
    evidenceClass: EvidenceClass;
    measure: AggregateMeasureV1;
    coverage: CoverageReportV1;
  }[];
  coverage: CoverageReportV1;
  hiddenChildren: AggregateMeasureV1;
  expandable: boolean;
  expansionAnchor: RetrievalAnchorId | null;
  layout: AtlasLayoutRefV1;
}
```

`AggregateMeasureV1` is the one generated discriminated union from plan 09: exact value/optional denominator, sampled estimate/sample/denominator/uncertainty, capped lower-bound/cap/denominator, or unknown/reason. Membership, activity, edges, hidden children, health, and learning-loop aggregates all use it; impossible boolean/null combinations and bare numbers are unrepresentable. Runtime labels arrive only through the plan-18 sink-eligible wrapper.

Semantic zoom:

- L0 profile: projects/workflows/agents/work/plan/task/attempt/lease/acceptance and domain health.
- L1 project/workflow/initiative/plan: work items, blockers, attempts, leases, acceptance/review, worktrees, branches, sessions, runs, memory, code snapshots, and delivery.
- L2 neighborhood: selected entities and bounded typed relations.
- L3 evidence: exact message, visible reasoning artifact, tool event, diff, diagnostic, fact source, policy evaluation, artifact, or delivery record.

The server publishes a versioned profile-atlas tile pyramid independent of request snapshots. Every tile carries zoom band, fixed atlas geometry, parent/child identity, aggregate-to-canonical membership mapping, importance-ranked label candidates, scope contour, entry/exit anchor, generation lineage, coverage, and prefetch neighbors. Viewport requests use hysteresis between zoom bands and a one-ring prefetch so clusters do not flicker at thresholds. A new snapshot updates evidence within the current atlas generation; a new generation supplies an explicit anchor-lineage transition and never silently reshuffles the user's mental map. This follows map-like multiscale graph work such as [GraphMaps](https://arxiv.org/abs/1506.06745) and recent [graph tile pyramids](https://arxiv.org/abs/2605.17498), adapted to TraceDecay's typed identity/evidence rules.

Expansion preserves existing positions, requests only child tiles/neighborhood, and animates no more than 250 ms unless reduced motion is set. The legibility budget is numeric: at most `250` labeled marks in the viewport, at most `2` overlapping label pairs per `100` labels, and no effective label smaller than `11 px` after zoom. If topology exceeds any of these bounds, the UI increases aggregation, folds a dense community into a node-link/matrix hybrid, or switches to matrix/outline; it never merely hides labels. Dense-community matrix thresholds and manual pin/unpin behavior are versioned and tested against temporal volatility, following the hybrid principle demonstrated by [DynTrix](https://onlinelibrary.wiley.com/doi/full/10.1111/cgf.15076). The performance suite asserts the bounds against the fixture corpus.

### 10.2 Graph-of-graphs lens contracts

| Lens | Nodes | Edges | Primary layout | Required coordinated evidence |
|---|---|---|---|---|
| Git | repos, worktrees, refs, commits, PRs, checks, reviews, releases | ancestry, points-to, produced, observed, encountered, delivered | layered + history rail | live/local freshness and drift |
| Code | snapshots, files, stable symbols, occurrences, diagnostics, tests | contains, calls, types, uses, changed-to, affected | layered/radial/matrix | source/diff/test/impact evidence |
| Threads | sessions, Turns, messages, reasoning artifacts, summaries, goals, tools | contains, follows, summarizes, used-context, produced | layered outline | native/representative/audience counts |
| Agents | actors, agent instances, tasks, goals, handoffs | spawned, delegated, messaged, joined, interrupted, completed | stable parent tree | provider/host/workflow meaning |
| Tasks | initiatives, immutable plan/work-item versions, gates, assignments, attempts, executors, packets, artifacts, outcomes | decomposes, requires, verifies, synthesizes, assigned, leased, attempted, handed-off, produced, accepted | plan outline/dependency DAG/critical path | readiness, exact scope, route/lease/packet/evidence versions |
| Plans | initiatives, plans/subplans, work-item DAGs, project sets, repositories, milestones, decisions | expands-to, blocks, supersedes, spans, unlocks, delivers | graph-of-graphs + semantic zoom | immutable version diff and active-attempt impact |
| Turns | Turn, context snapshot, messages, goals, tools/results, code, hints, outcomes | visible-at-start, invoked, produced, affected | compact layered DAG | explicit Turn interval and coverage |
| Causal Loom temporal composition (not `GraphLensV1`) | events/intervals/density bins | source order, causal, correlation, temporal | lane/time | occurred and ingested time |
| Memory | facts/versions, entities, decisions, contradictions, retrievals, feedback | source, supports, contradicts, supersedes, retrieved, rated | provenance DAG/cluster | trust/version/retention |
| Automation | jobs, schedules, runs, actors, candidates, artifacts, skill/memory versions, uses, outcomes | scheduled, spawned, proposed, validated, auto-decided, autonomously-applied, injected, observed, automatically-recovered | waterfall + lineage DAG | config/policy/actor identity |

`GraphCompositionSpecV1` is the single lens-stack contract: one primary lens, zero to two overlays, and an explicit bounded set of registered bridge kinds. The ordinary route selects the primary lens; the lens-stack control can overlay, for example, Git + code + Turns or agents + tasks + memory while retaining per-lens node/edge membership, bridge role, evidence style, legend, LOD, and disablement. It returns the same `GraphSliceViewV1`, not a second “combined graph” endpoint. A path such as PR → commit → symbol → Turn → agent → task → fact is therefore visible as typed cross-domain bridges rather than one semantically flattened edge soup.

Selecting any node changes the common selected entity and reveals cross-lens pivots. Switching or composing lenses preserves time/scope/selection/pins and atlas position. Edge keys and inspector language are lens-specific; shared visual geometry does not erase semantics. A composition exceeding the overlay/bridge/legibility budget is rejected with a suggested filtered query, matrix, or sequence of focused neighborhoods.

## 11. Universal Explorer

Explorer has three synchronized authoring modes:

- plain-language intent compiled by the server into visible `TraceQueryV1`;
- structured builder for scope, kinds, predicates, text, graph/time operators, grouping, ranking, fields, and limits;
- source-form `TraceQueryV1` editor with schema completion and safe validation, never SQL/FTS syntax.

The search builder exposes one versioned evaluated retrieval profile with inspectable stages—not a magical semantic toggle:

- lexical token/field and exact phrase;
- typo-tolerant fuzzy with visible edit/candidate cap;
- entity/symbol/alias resolution;
- optional semantic/vector candidates;
- graph-neighborhood relation candidates;
- recency/activity prior;
- explicit origin/kind/provider/session/agent/project/ref/time/sensitivity filters;
- representative grouping/dedupe with native membership and expansion.

Users may disable stages or choose a benchmark-proven profile. The UI never claims embeddings improve results; vector-disabled/unavailable/regressed is visible, and exact literal/phrase hits cannot be demoted below a profile's locked exact-match floor. Search results state stage matches, score components, candidate universe, caps/exclusions, grouping membership, coverage, and benchmark profile/version.

The result surface pivots between precise table, timeline, graph, matrix, distribution, small multiples, and saved collection. All pivots consume one response snapshot and disclose unsupported encodings. Result rows expose type, primary label, time, scope, evidence class, match reason, score components, coverage, and source mode; selecting opens the inspector.

Memory and self-improvement data are ordinary first-class Explorer data, not content hidden behind an Evolution summary. The catalog-owned generated presets `preset.knowledge.all-memories`, `preset.knowledge.active-project-with-profile`, `preset.skills.all`, and `preset.automation.all` exhaustively enumerate every authorized fact/fact-version, knowledge entity/version, decision, contradiction, relation assertion, retrieval, feedback, automation job/admission/skip-episode/run/artifact/candidate/decision/effect/recovery, skill/package/version/materialization, recorded use, and outcome. **All memories** instantiates the first preset as an explicit cursor-paged `TraceQueryV1` over active-profile All scope with owner, project, kind, state, tag, trust/evidence, updated-time, source-session/run, and retention filters; it never means “the rows currently loaded by this browser.” Active-project recall is visibly the union of exactly `Profile` plus that project, with provenance and per-root coverage; `ZeroProject` is a contextual activity partition, not a synonym for durable user memory. The same result set pivots to graph, table, timeline, similarity/matrix, or collection without changing identity or counts. From a transcript or run, users can follow what memory/skill artifacts it informed; from a memory, association, or skill, they can follow exact source Turns/runs and later retrievals, injections, uses, feedback, outcomes, revisions, or recoveries. A relation inspector exposes predicate, valid/observed time, evidence, confidence/trust inputs, producer/version, and supporting anchors; visual thickness or proximity is never the only explanation. This uses the shared query/graph/inspector contracts—there is no separate memory browser database, client-side association score, or required before/after graph mode.

Brush, lasso, path, facet, time-range, table-row, cluster, and inspector-relation interactions never become hidden renderer filters. For **highlight**, **filter**, **exclude**, **compare**, or **derive lane**, the server `query.compose_from_selection` use case returns the canonical visible `TraceQueryV1` delta, explanation, cost, and reversible breadcrumb. Every coordinated slot applies the same accepted delta or declares it unsupported, so graph, timeline, table, and chart cannot drift into different result universes. **Add to collection** is deliberately separate: it resolves the selected stable anchors and invokes versioned `collections.update`; it is never previewed or recorded as a query delta.

Query Explain shows canonical AST/fingerprint, validation, cost/budget, selected and pruned shards, pushed/residual filters, FTS/vector/graph/time operators, ranking components including absent features, candidate universe, per-stage caps, cap-induced exclusions, stable sort key, cursor/watermark, timing, coverage, truncation, message-origin/view semantics, and retention. Noisy ranking can be diagnosed from exact score components and bounded candidates; capped/ambiguous results never look complete. Export equivalent current CLI/MCP/HTTP requests plus a stable retrieval recipe from the server-generated representation so the UI does not reinvent syntax or preserve stale names.

Explorer shows the active qualified retrieval profile, latest evaluation-report version, promotion state, and concise regression/coverage warning. **Investigate quality** deep-links the exact protected query/profile/snapshot into Search Quality Lab, the sole owner of corpus execution, judgments, metrics, comparisons, and promotion gates. Explorer never embeds a benchmark runner or creates a second evaluation state model.

For optional semantic code search, Explorer, Code, Settings, and Search Quality Lab render one server-owned FastEmbed embedding plus optional BGE rerank state. Settings shows desired, activated, effective, and observed enablement independently; exact embedding/rerank artifact and model; verified-cache/offline/download-consent state; CPU/device/thread/batch/RAM/disk/residency budgets; native rerank toggle/top-N (default 25, hard cap 25); strict versus byte-stable lexical fallback; and generation/rebuild progress and coverage. The browser never lists cache paths, loads a model, computes vectors, probes devices, or infers runtime state. Before benchmark promotion the control is visibly off by default. Missing model, consent, cache, generation, device, memory, deadline, or coverage has a distinct typed state and legal install/import/verify/activate/rebuild action through the existing representation operations.

Code and Explorer Query Explain show a stage waterfall and off/on comparison with lexical candidates, semantic additions, pre/post-rerank rank delta, exclusions/caps, exact artifact/generation provenance, latency, peak RSS, cache state, vector/index coverage, and fallback/error receipts. Search Quality Lab can replay and ablate lexical-only, native semantic, native BGE rerank, and an independently registered optional Codex Spark/app-server-style rerank route. The model-assisted panel shows discovered capability, credential/egress/privacy decision, exact requested/actual model route, cost/tokens/deadline/top-N, and preserves the pre-rerank order on unavailable/denied/timeout/malformed output. It is a separate off-by-default profile toggle, never an embedding backend or silent replacement for the promoted FastEmbed embedding or native BGE reranker, and links its active-hint/scout applicability to plan 22 without adding another lab or operation.

One mandatory frozen historical conformance slice represents Rspack, Rsbuild, and React Router repositories/worktrees/benchmark records with same-name files, symbols, branches, sessions, and known dependency/PR evidence. It verifies repo disambiguation, federated graph links, scope candidates, ranking relevance, provenance, and stale/partial labels across CLI/MCP/API/dashboard outputs. It is retrieval/scenario data only: the test does not require those checkouts to exist, execute their benchmarks, import their packages, or influence TraceDecay's dashboard build.

Collections retain stable research anchors plus entity/event/relation refs, retrieval recipe, snapshot/watermark, and annotation refs — the `CollectionV1` record of section 4.3. Compare aligns by stable entity identity, Turn boundary, commit, or explicit anchor; unaligned items remain visible. Expired page cursors/response handles never invalidate a collection.

## 12. Causal Loom

Causal Loom coordinates:

- month/day/hour density overview;
- stable parent/subagent tree and lane ordering;
- virtualized event waterfall at workflow/session/Turn/event granularity;
- virtualized transcript/code/diff inspector;
- impact ribbon for files, symbols, tests, commits, PRs, facts, skills, and automations;
- as-of reconstruction panel;
- follow and compare controls.

### 12.1 Lane order and event priority

1. Human prompts and user-visible objectives.
2. Assistant output and provider-exposed reasoning artifacts.
3. Parent/subagent lifecycle, delegation, messages, handoffs, goals.
4. Tool calls/results/errors/retries/approvals/latency.
5. Files/symbols/patches/diagnostics/builds/tests/impact.
6. Worktrees/branches/commits/PRs/checks/reviews/releases.
7. Hints/retrieval/memory/facts/feedback/policy decisions.
8. Schedules/runs/skips/artifacts/candidates/autonomy-decisions/automatic-effects/recoveries, plus labeled historical approval/apply events.
9. Context/tokens/compression/latency/cost.

Human prompt, agent spawn/handoff, failed tool, file mutation, diagnostic/test failure, commit/PR/review/release, policy mutation, and privacy events remain discoverable at every zoom. Routine success noise may aggregate but remains in counts/export and expands deterministically.

### 12.2 Time and causality behavior

- Use occurred time for placement and ingested time for a late-arrival marker. Missing occurred time uses a labeled ingest-time fallback.
- Frozen views never reorder. Live late events appear through a visible “new historical event” marker; accepting refresh creates a new snapshot.
- Interval selection uses `[from, to)` and reports clipped lanes/events.
- Direct structured touches and inferred affected entities render separately.
- Causal connectors require evidence; temporal proximity uses a neutral connector and never an arrow.
- The bounded causal chain is `context before → visible rationale/decision → action/tool → result → code/artifact → test/delivery → downstream impact`.

### 12.3 Turn inspection

A committed Turn selection shows:

- start/end, actor/agent/provider/host/session/workflow and parent Turn;
- context snapshot visible at start, including summary/LCM lineage and retrieval/memory/hints;
- summary text renders compact `[S1]`-style provenance chips for consequential claims. Opening a chip explicitly authorizes/resolves the plan-23 manifest entry and shows relation, source range, current/stale/locked/redacted/revoked state, coverage/omissions, and requested-versus-actual model/revision/effort/fallback; the browser never embeds raw opaque IDs throughout prose or auto-hydrates source content;
- direct-user/delegated-agent/tool-result/provider-protocol/unknown origin counts and native/representative expansion;
- provider-exposed reasoning artifacts with `summary`, `analysis_text`, `structured`, `encrypted`, `redacted`, or `unavailable` format;
- goals/tasks at start, updates during Turn, terminal state;
- tools/results, approvals, errors, retries, latency;
- files/symbols/patches/diagnostics/tests and evidence-bearing impact;
- Git/worktree/branch/commit/PR state encountered or produced;
- output messages, cost, tokens, compression, downstream outcomes, and coverage.

Reasoning is excluded from search, embeddings, export, and saved annotations by default and respects shorter retention. Missing or encrypted artifacts render coverage markers, never inferred chain-of-thought.

### 12.4 Follow, compare, and time machine

- Follow one agent: keep its lanes expanded, collaborators summarized, and delivery impact visible.
- Compare sessions/agents/branches/models/policies/time ranges: align on user Turns, commits, goals, or manual anchors; show missing intervals and coverage.
- Scrub as-of: project/worktree/ref/snapshot, visible messages/context, fact/memory versions, hint/policy/config/tool catalog, open goals/tasks/workflows, and observed delivery state.
- Exact replay requires immutable artifact/config/candidate/index/memory/catalog manifest; recorded replay verifies stored result; best-effort lists substitutions.

The Loom is also an experiential replay player. `ReplayFrameViewV1` supplies the current Turn/event, previous/next consequential anchors, before/after state refs, graph delta, code/diff refs, collaborator changes, impact wake, fidelity, and substitutions. Controls are play/pause, speed, step Turn, step event, next error, next mutation, next handoff, and scrub; graph, transcript, code/diff, inspector, and impact ribbon share one playhead and cross-highlight the same stable anchors. Playback never fabricates intermediate state between canonical frames. Reduced motion replaces interpolation with discrete frame changes and an announced change list.

Any compatible `TraceQueryV1` event, interval, or counter result can become a bounded derived lane: name, query fingerprint, grouping facet, color/mark role, snapshot, coverage, and retrieval recipe are explicit; lanes are searchable, pinnable, reorderable, nestable, and saved with the workspace recipe. This adopts the inspectable query-derived-track pattern used by [Perfetto](https://perfetto.dev/docs/analysis/debug-tracks) without importing its trace model. Derived lanes remain server-query results and cannot mutate canonical event order or create client-only counts.

### 12.5 Timeline windowing contract

The Loom's client data contract for large corpora is typed, generated in the plan 10/17 schema (density-bin fields lifted from master plan §14.2 into schema), and re-exported through `app/src/contracts`:

```ts
export interface DensityBinV1 {
  laneId: string;
  bucketStart: string;               // occurred time; half-open [bucketStart, bucketEnd)
  bucketEnd: string;
  exactCount: number | null;         // exactly one of exactCount/sampledCount is set
  sampledCount: number | null;
  denominator: number | null;
  hiddenCount: number;
  lateCount: number;
  coverage: CoverageSummary;
  aggregationVersion: string;
  firstEventCursor: string | null;   // bin → EventPageV1 linkage for drill-down
}

export interface LaneWindowV1 {
  laneId: string;
  window: { from: string; to: string };
  totalLogicalRows: number;          // server total; drives virtualized row count (§16.1)
  loadedPageCursors: readonly string[];
  evictionWatermark: string | null;
}

export interface EventPageV1 {
  pageCursor: string;                // PK within (laneId, window)
  laneId: string;
  events: readonly string[];         // canonical event IDs; rows hydrate via generated read models
  nextCursor: string | null;
}
```

Keys, envelopes, and paging policy:

- A density bin is keyed by `(laneId, bucketStart, aggregationVersion)`. One density request returns at most `2,000` bins; if the requested window/zoom would exceed that, the server raises aggregation and reports it in `coverage` — the client never bins events itself.
- An event page holds at most `500` events and `<= 1 MiB`; the client prefetches at most one page ahead and one behind the viewport and evicts by LRU beyond `12` retained pages per lane, deleting payload chunks before metadata (same policy as section 8.1).
- Virtualized transcript/table row counts and scroll positions derive from `LaneWindowV1.totalLogicalRows` (the accessibility row-count source of section 16.1), never from loaded-page length; unloaded rows render as fetchable placeholders with position preserved.
- Size envelope: the recorded fixture corpus pins `388,000+` messages and the 250k-density-mark budget; at hour buckets that is ~8,760 bins per lane-year, so ~28 lane-years fit the mark budget before the server must raise aggregation. `dashboard/tests/performance/timeline` includes a 388k-message fixture exercising binning, paging, prefetch, and eviction at this scale.

## 13. Domain workspaces

Scope and persistence are visible product semantics, not hidden implementation detail:

- Every fact, memory version, skill, policy, automation, saved investigation, and annotation shows a plain-text owner line (`profile`, `cross-project`, or named project), privacy domain, and source evidence in its summary/history; this is not a decorative badge.
- Human-authored non-curation create/import commands and autonomous curation effects require an explicit generated `DeclaredScope`. Opening a project route or filtering All to one project never preselects ownership silently; no fact/memory/skill item proposal/apply control exists.
- Existing-target actions use the entity's canonical owner and disable with an ownership-conflict explanation if the request state disagrees. Moving ownership opens the dedicated migration inspect/plan workflow; it is not an editable field.
- Cross-project use links to one durable source version through evidence relations. The UI never offers “copy to project” as a shortcut for memory, skill, policy, or automation reuse.
- Mixed All-scope lists group/filter by owner without changing identity. Profile-owned and project-owned histories remain distinguishable in tables, graphs, exports, URLs, and replay manifests.

### 13.0 Work, plans, tasks, and executors

- The canonical selection is initiative/plan-version/work-item/attempt IDs plus frozen/live watermarks. A board is a protected saved `TraceQueryV1` and lens; changing board, repository, agent, executor, or layout never copies or rehomes tasks.
- `SavedViewDefinitionV1::Task(TaskViewSpecV1)` round-trips the complete task-specific contract inside the ordinary `SavedViewV1`: protected canonical query/digest with mandatory explicit `query.scope` and derived scope digest, projection/lens/group/sort/layout, owner/sharing grants, live or exact frozen plan/entity/projection versions and watermark, config/catalog/schema versions, optimistic view version, timestamps, and revocation. It defines no second saved-view ID/store/scope/share lifecycle. Multiple overlapping view instances remain isolated; reopen never falls back from missing frozen inputs to current state. Share revoke invalidates the exact grant/subscriptions without deleting the owner's view.
- Initiative overview shows exact cross-project scope, plan version, dependency/fan-in state, critical-path interval/slack, budgets/deadlines, active agents/executors, costs, outcomes, coverage, and links to Goals/workflows/code/Git/PR/check/release evidence.
- Plan outline, Kanban, dependency DAG, critical path, timeline, causal, workload, executor-fleet, repository-work, initiative, agent-relevant, and All views preserve identical IDs/counts/selection. This is plan 24's projection vocabulary exactly: §0.21's saved projections plus §12.5's Executor Fleet/Repository Work lens names and §12.7's agent-relevant slice; 11 renders no view name outside that set. Drag/drop invokes only generated legal commands; derived readiness cannot be set directly. Workload and Executor Fleet runtime/cost/rate/denominator views consume plan 26/PR 30J generated accounting/liveness projections and retain attempt/work-item/executor/route/model/effort drill-down refs, methodology, watermark, and unknown/capped state; the browser never aggregates its loaded rows into a total.
- One saved view exposes switchable Kanban, DAG, timeline, table, workload, and cross-repository bundle projections over the same authorized `TraceQueryV1`, frozen/live watermark, canonical membership, sort/group specification, and selection. Switching mode preserves filters, selected IDs, exact totals, and per-view camera/column/scroll state; it does not create another saved view or refetch an unbounded board.
- Kanban is server-grouped and cursor-windowed by derived lane/reason. Columns virtualize horizontally; cards/rows virtualize vertically with at most three viewports overscan. Initial hydration includes IDs, safe labels, readiness/reason, assignment/route summary, attention/progress, and versions only; inspector/detail/artifact bodies hydrate on selection. A 388k-item fixture must keep DOM rows below 600, initial payload below the §19 route budget, and keyboard logical row/column positions exact.
- Multi-select is an explicit mode with selected count/scope and one inline generated batch validation/effect view. The catalog-generated result discriminant is either `AtomicAllOrNone` (one committed/rejected transaction with deterministic per-item validations) or `PerItemPartial` (per-canonical-ID accepted/version-conflict/denied/stale/unavailable/failed outcomes). Only `PerItemPartial` may advance accepted rows while failed rows remain selected; an atomic rejection advances none. A partial failure never appears as all-success or as an `AtomicAllOrNone` receipt.
- Drag/drop is optional pointer shorthand for a generated legal command, never raw lane mutation. Keyboard/touch users choose “move/change…” then a legal destination/action list with reason, impact, and confirmation requirement. Dependency-blocked/readiness states cannot be overridden by presentation movement.
- Ordinary edits (title/specification metadata, saved-view filters, assignment/priority when policy permits) commit directly through the cataloged expected-version command and show version history; the UI invents no generic undo/rollback token. Archive is soft presentation retirement: it preserves canonical identity, relations, attempts, anchors, and audit and can be reopened by the cataloged versioned command. Worktree deletion, force-affecting Git, merge/release, protected-data deletion, and other irreversible/external effects use their operation-specific confirmation/compensation contracts. Curation candidates/effects remain fully autonomous and expose history/outcomes, not proposal controls.
- Multi-select route assignment invokes one generated `work_items.assign_set` command, shows the bounded affected set/constraints inline before the direct transaction, and accepts only an `AtomicAllOrNone` receipt with deterministic per-item validation. The renderer rejects a `PerItemPartial` discriminator for `assign_set`; E2E fixtures cover full commit, one-item validation rejection with zero commits, and malformed mixed results. It never loops singular commands client-side. Shared execution renders an aggregate parent plus independently leased child work items; it never depicts two authoritative leases as co-owners of one task.
- Task/attempt inspector covers candidate versus active plan admission, plan-local labels and derived-work provenance, local plus enclosing dependency closure, acceptance/review-successor lineage, assignment/route rationale, lifecycle owner and acting native-CLI/provider/adapter participants, requested versus actual host/provider/model/reasoning effort/tools/skills/grants, typed failure causality, fenced lease state, one-shot lifecycle checkpoint state, packet/omissions, explicit inspect/write/integration workspace authority, Turns/tools/artifacts/handoffs/outcomes, cancellation/reconciliation, costs, audit, and anchors.
- Generated legal actions expose `work_items.record_attestation`, `record_review`, `record_decision`, `record_exception`, `handoff`, `reopen`, and `reverse_transition` exactly. Attestation/review/decision/exception/handoff dialogs require the typed evidence/version/grant fields and cannot set derived readiness/acceptance directly; reopen creates a successor work-item version; reverse-transition shows the exact compensating edge and receipt and is never labeled generic “undo.”
- Work status/diagnostics/live changes use `task_graph.status`, `task_graph.doctor`, and the `task_graph.events` canonical subscription kind. The UI never polls a second board database or invents `/task-events`.
- Attempt navigation is a first-class paginated list/detail/timeline over `attempts.list/get/timeline`, not a hidden work-item expansion. It distinguishes every immutable attempt, state event, retry/defer reason, requested/actual route, start packet, current accepted packet, effective Turn boundary, lease/fence, and imported nonauthoritative execution-observation lane.
- The executor offer inbox uses registration-scoped `task_offers.list/get`. An offer is visibly advisory—never a claim or lease. An authenticated executor may accept (atomically yielding the one attempt/lease/start manifest) or decline; scheduler/admin revoke is separately authorized. Expired/declined/revoked offers never look like failed attempts, and a dashboard observer cannot impersonate an executor.
- Packet history uses `context_packets.list/get` to compare sealed ordinals, omissions, source/access/config/catalog drift, start/accepted/superseded/expired state, and anchors. Only the active executor capability may invoke fenced `context_packets.accept` with the exact prior packet and safe Turn boundary; the UI has no generic current-packet setter and never rewrites the immutable start packet.
- Task inspector maps the Hermes-derived anatomy without inventing another tab union: metadata/specification → `summary`/`specification`; status and legal actions → `summary`/`actions`; `DiagnosticEnvelopeV1` list → `evidence`/`actions`; dependencies → `dependencies`; attachments/comments/handoffs → `decisions`; canonical events → `history`; protected worker log → `evidence` with authorization; run history → `attempts`. Attempt selection reuses the same URL/inspector model. Unknown diagnostic action kinds remain visible and disabled with update guidance.
- Attention is a server-authored ranked strip over plan-06 `AttentionSignalV1`: stale advisory work claim, stale/expiring authoritative lease, aging blocker, repeated retry, unresolved effect, protocol violation, packet invalidation, critical-path risk, or material sibling change. Server supplies tier/age basis/watermark/evidence; the client never diffs clocks or treats attention as task truth. Staleness rings always include a text tier and exact last evidence time. Progress shows completed/total when both are known, indeterminate otherwise, and never derives percent from status or message volume.
- Overlap overlay distinguishes authoritative leases/writable-resource reservations, advisory `WorkClaimV1`, intentional ensemble/shared-work children/parallel roles, canonical query-scope overlap, and weak proximity. It shows exact overlap evidence/TTL without sibling prompt text and never labels a work claim as execution ownership.
- Agent default is the active attempt plus blockers/parents, material siblings, decisions, acceptance, handoffs, packet entries, and workspace conflicts. All work requires an explicit human authorization/scope expansion.
- `/work/notifications` owns the plan 24 §12.7 human notification-subscription UI: explicit saved filters/channels with event classes, quiet hours, dedupe, rate budgets, and authorization, edited through direct validated `task_notifications.create/update/delete` commands with expected version/idempotency. Task state never auto-subscribes the creating profile/channel, and dashboard toasts, gateway messages, hook hints, and task comments share no accidental notification loop.
- Every initiative/plan/query/view with edit authority offers **Edit as Markdown**. It first explains the exact frozen selection, external-stub closure, owner, base versions, size estimate, expiry, and destructive-intent rules, then starts `task_graph.edit_bundles.export`; it never downloads an inferred current board or exposes a server path. Small bundles may use an embedded CodeMirror workspace; large bundles use contained download/upload streams while the route retains only the opaque workspace ID and safe manifest summary.
- `/work/edit-bundles/:id` is a resumable operation cockpit, not a second task editor. Its file tree follows the signed manifest; status shows upload digest, frozen base, TTL, limits, cleanup state, and operation history. Validation renders stable diagnostic code, severity, exact file/range, related spans, and safe fixes in editor, outline, and accessible table. The semantic-diff view groups add/change/retain/explicit-retire plus dependency/gate/assignment/route/budget changes, critical-path effects, and affected active attempts; a raw text diff is supplemental only.
- Stale bases open an explicit three-way semantic conflict resolver backed by `rebase`; canonical/local/base values and evidence are distinct, conflicts cannot be bulk-accepted blindly, and rebase creates a successor workspace. Submit remains disabled until strict validation, secret scan, version checks, impact acknowledgement where required, and complete upload pass. The final dialog names exact owner/counts/retirements/active-attempt implications and invokes one atomic submit—never preview/apply/rollback or per-file CRUD.
- Successful submit shows canonical ID mappings, new plan/version anchors, operation/audit receipt, and verified `Purged` cleanup before leaving the cockpit. Validation failure retains the workspace only until its visible TTL; delete/clean and expired/crash-reaped states are idempotent and recoverable only by a fresh export. Mobile is inspect/validate/diff/conflict-review/submit only; editing a large bundle directs the user to CLI/SDK without hiding cleanup or diagnostics. Offline or locked state never presents local bytes as canonical truth.

### 13.0A Related repositories, worktrees, and cleanup lifecycle

This is a composed Work/Repository Work/Delivery experience, not a new inventory, board, or route. It consumes plan 24's canonical work-item, attempt, workspace-authority, scope, Git/delivery, relation, retention, and receipt views through plan 21's generated bindings and presentation descriptors. Selecting a worktree from a task opens the universal inspector in place; “open in Delivery,” “open Git graph,” “open thread/Turn graph,” and “open timeline” preserve the same opaque IDs, scope, watermark, time, and selection through typed link descriptors. TraceDecay observes worktrees created by users, agents, executors, or external tooling; the UI has no create, provision, clone, adopt, or implicit-write-worktree action.

- **Always-visible association summary:** every task/ticket card, virtualized row, DAG node, and search result exposes a compact, text-equivalent scope line: repository count/name, worktree count and lifecycle severity, branch/ref, PR state, active-agent/attempt count, and nearest retention/expiry transition. Dense modes may collapse this to one line plus counts, but focus/accessible name and the table retain every category. Unknown, partial, denied, and “none observed” are distinct.
- **Task detail header:** before tabs, show canonical task/plan version and a Related Work strip grouped `Repositories → checkouts/worktrees → refs/branches → commits/PRs/checks`, with current attempt/lease and participant topology adjacent. Each item states relation type, freshness, ownership, dirty/shared state when authorized, active agents/attempts, cleanup state, retention deadline, holds, and blocker count. It reuses plan 24 §12.6 `Workspace Authority`, `Execution Topology`, and `Code/Git/delivery impact` descriptors; there is no dashboard-local tab or task-to-worktree join.
- **Evidence, not name matching:** show creator/source provenance when known—user, agent, executor, host integration, or external tool—and keep unknown explicit for worktrees discovered through sanitized host hooks, tool observations, Turn CWD changes, Git/worktree watchers, and later reconciliation. Distinguish declared scope, sealed attempt binding, observed use, produced artifact, and merely related/encountered Git evidence. Every association shows provenance, occurred/observed time, confidence/evidence class, coverage, and whether it is confirmed, proposed, rejected, superseded, or ambiguous. Never infer cleanup authorization from creator, CWD, branch naming, path prefix, repository proximity, a viewed PR, confirmed association, or one task's archive state. Shared worktrees list every authorized live reference; hidden relations contribute an explicit redacted/unknown blocker rather than disappearing.
- **Incremental relation resolution:** canonical subscription deltas update the Related Work strip and task graph when a hook/tool/CWD/Git watcher discovers or strengthens a relation; the browser never runs its own watcher. A proposed or ambiguous material link appears inline with compact generated `confirm`, `reject`, or `reassign relation` legal actions, exact competing task/attempt candidates, and evidence. These actions resolve only the association and cannot adopt the worktree, change workspace authority, assign cleanup ownership, or bypass a running attempt. If the contract does not expose the applicable relation-resolution capability, the proposal remains inspectable and read-only rather than being mapped to a generic task edit.
- **Quiet association hints:** high-confidence nonconflicting discoveries update silently with a transient activity marker and history event. Only one material ambiguous association that blocks routing, acceptance, coordination, or cleanup may produce a deduped hint in the task attention strip; it names why now, links to evidence, supports inspect/resolve/suppress, and obeys per-worktree/task cooldown and acknowledgement. Lower-ranked proposals remain in the inspector/queue and never stack notifications.
- **Workspace/worktree inspector:** the universal inspector presents canonical repository/common-dir, checkout/worktree generation, ref/base/head/snapshot, creator/source provenance, workspace authority mode, cleanup authorization evidence/delegator/scope/expiry, dirty/shared/conflict evidence, active reservations/leases/agents/attempts, producing task/attempt, all referencing tasks/PRs, retention policy/source, expiry, holds, last reconciliation, and cleanup history. “Creator known,” “associated,” “writable by an attempt,” and “cleanup delegated” are four separate facts. Absolute paths appear only when the sealed view authorizes a local display sink; copied/deep-linked state remains opaque.
- **Task-graph overlay:** Repository Work and task/plan DAGs offer one bounded `workspace lifecycle` overlay. Nodes remain canonical tasks/attempts/worktrees/branches/PRs; edges use the generated relation vocabulary; redundant encodings distinguish active, retained, cleanup-eligible, blocked, queued/in-progress, removed-and-verified, failed, stale, and unknown states. The overlay can highlight “what keeps this worktree,” “what becomes eligible if this task archives,” and “what changed after PR merge” from daemon-supplied impact evidence without predicting or executing deletion in the browser. Outline, exact relationship table, and deterministic export are authoritative fallbacks.
- **Archive/merge lifecycle:** archiving a task remains `work_items.archive` and never deletes a worktree. A merged PR, closed attempt, archived task, expired lease, or elapsed retention clock is only daemon-observed eligibility input. The UI shows the triggering evidence, policy/config generation, `eligible_at`, retention countdown, reconciliation state, and remaining blockers. Automatic cleanup, when enabled by effective policy, is an application/daemon workflow with the same preflight and receipt as a manual request; closing the tab cannot cancel, complete, or fabricate it. Dirty/failed worktrees default to retained-for-investigation exactly as plan 24 §9.8 requires.
- **Eligibility and blockers:** “associated,” “creator known,” and “writable by this attempt” are never styled or described as “cleanup-authorized” or “cleanup-eligible.” Cleanup may be presented as eligible only when the sealed daemon view proves explicit still-valid delegated cleanup authority for that exact worktree generation and effect, terminal/reconciled attempts, no active lease/agent/process/reservation or effect-unknown state, no other live task/plan/edit/workspace reference, satisfied retention and holds, and an admissible clean/dirty disposition. Confirming or reassigning a task relation grants no cleanup authority. Blockers are stable codes with evidence links and remediation/legal next actions—cleanup not delegated/expired/out of scope, active attempt, shared/foreign/user-controlled, dirty/untracked, unmerged/unreconciled delivery, live or ambiguous association, hold, retention not elapsed, stale/partial coverage, authority unavailable, or cleanup already operating. The client never computes eligibility from loaded rows or local time.
- **Cleanup queue without a second truth:** “Cleanup queue” is a saved Triage composition over the Repository Work projection, grouped by eligible now, eligible later, blocked, operating, failed, and recently cleaned. Server facets include repository/project/initiative/task, ownership, dirty/shared, PR merged/closed/open/unknown, archived/terminal/active task, active agent/attempt, blocker code, retention bucket, stale, orphan-candidate, and receipt outcome. `orphan-candidate` is a daemon classification with coverage/evidence and never means “no task row loaded.” Saved filters store only canonical `TraceQueryV1`/layout state.
- **Bulk safety:** multi-select freezes opaque workspace IDs/generations, relation/coverage watermark, and expected eligibility revisions. The generated action first returns the exact affected set, retained/skipped set, blockers, paths only where locally authorized, branches/PRs/tasks/agents implicated, policy, retention, and confirmation requirement. Any changed generation or newly active reference is rejected or excluded according to the operation's generated atomicity contract; the dialog cannot downgrade blockers. Consequential confirmation and cancellation/recovery follow plan 09, not a frontend modal token.
- **Receipts:** operation progress and terminal rows show requested/actual disposition, initiator or policy trigger, workspace generation, task/attempt/PR anchors, preflight evidence watermark, policy/config/catalog versions, started/finished time, removed-versus-retained members, Git/worktree reconciliation, verification result, and stable operation/audit receipt links. “Cleaned” is reserved for a daemon receipt that verified removal and canonical reconciliation; disappearance from a list, `404`, archive, PR merge, process exit, or optimistic UI success is insufficient. Failed/unknown outcomes remain retryable or investigable through generated legal actions.
- **Contract gate:** no component invents `worktrees.clean`, overloads `work_items.archive`, shells out, or aliases plan 21's `task-graph edit clean`. Until the catalog owners add the distinct physical-workspace operation and plan 21 generates its CLI/MCP/API/SDK/dashboard binding, the inspector/queue remain truthful read-only eligibility surfaces with an explicit “cleanup capability unavailable in this contract” reason. Once generated, the dashboard consumes its sealed preflight/operation/receipt views unchanged.
- **Live, offline, and race state:** task/worktree/PR/agent/attempt deltas arrive only through canonical subscriptions. Eligibility revision changes invalidate selection and refetch the bounded preflight; gap state disables cleanup until resync. Offline cache may show last verified state/receipt with age but cannot queue a browser-local cleanup. Back/forward, refresh, daemon restart, and another client completing cleanup preserve selection or resolve it to the terminal receipt/tombstone.
- **Mobile/accessibility:** mobile task detail shows the Related Work strip as an ordered summary and opens one full-height inspector/queue sheet; bulk selection and confirmation are stepwise, never a horizontally clipped desktop table. The table exposes real headers for repository, worktree, branch, PR, agents/attempts, eligibility, blocker, retention, and receipt; logical row counts survive virtualization. All status uses icon/shape/text, countdowns expose exact UTC timestamps, blocker/receipt changes announce once, focus returns to the originating task/worktree, and every graph/hover action has keyboard, screen-reader, touch, and reduced-motion parity.

### 13.1 Sessions

- Complete paginated session list and complete sanitized-native message enumeration, lossless for retained non-secret structure/semantics.
- Provider/model/role/kind/origin/time/project/Git/workflow filters.
- Explicit transcript modes and counts from section 4.2.
- Turn graph/outline, parent/subagent tree, Claude workflow/Codex goal links, context compression, cost, and direct code/delivery impact.
- Sanitized-native source, normalized observation, canonical event, and projection tabs with offsets/privacy-domain-bound fingerprints.

### 13.2 Agents

- Actor identity versus agent instance versus provider workflow identity.
- Stable parent/subagent tree, Turn sequence, delegation, inter-agent messages, handoffs/joins/interruptions, goals, tools, outcomes, retry/failure patterns.
- Compare providers/models/sessions/projects without conflating a logical actor with a process/run.

### 13.2A Coordination

- Presence is an expiring evidence claim with agent/provider/host, same or parallel worktree, repository/ref/revision, workflow/goal/Turn, observed/expires time, source, confidence, and unknown-after-expiry state. A missing row never means “no other agent.”
- Nearby ranking separates same worktree, parallel worktree/same repository, overlapping ref, direct file/symbol/test/goal/review overlap, and weak temporal proximity. Direct overlap lists evidence and stable research anchors; temporal-only proximity is neutral and never labeled conflict.
- The main artifact is a ranked overlap ledger synchronized with a compact worktree/agent map. Each row shows recipient-authorized domain `SafeCoordinationSummary` backed by `CatalogSafeText`, exact coverage/freshness, stable retrieval recipe, and legal `inspect/message/handoff/ack/suppress` actions. Prompt injection requires a separate `PromptEligibleText` conversion/policy receipt. The table is the precision/accessibility authority.
- `message`, `handoff`, `ack`, and `suppress` use the generated direct/resumable commands with an inline disclosed-summary/effect view, expected claim version, idempotency, and receipt; there is no generic preview/apply pair. Delivery, acceptance, acknowledgement, suppression, expiry, and resolution are distinct states; a sent message never appears as an acknowledged handoff.
- One dynamic coordination hint may appear in the command/status rail for the highest material actionable overlap. It includes one sentence, one stable anchor, one primary action, “suppress,” and why-now evidence. Per agent/pair/work-claim dedupe, cooldown, acknowledgement, and suppression prevent repeat prompts; lower-ranked overlaps remain in the workspace, not stacked notifications.
- Analytics show eligible/material/selected/delivered/inspected/messaged/handoff/ack/suppressed/expired/resolved/duplicate-prevented/unresolved with denominators, coverage, and horizon. No outcome is inferred from later code proximity alone.

### 13.3 Code

- Repository/snapshot/file/stable-symbol/occurrence graph and lineage.
- Session/agent ownership overlays labeled direct, inferred, or unknown.
- Diff graph, dependency matrix, cycles/coupling, diagnostic/test and affected-test overlays.
- Branch/commit/as-of slider and snapshot comparison; CodeMirror source/diff with exact locations and redaction decorations.
- Semantic search diagnostics reuse Explorer's stage waterfall, representation state, rank/rerank comparison, artifact/generation/rebuild coverage, resource charts, provenance, and typed fallbacks; Code never creates a second settings panel, search request, model session, or client-side vector index.
- `move_symbol` is a generated operation, never a browser rewrite helper. `code.move_symbol.inspect` shows the exact source/destination diff, inserted destination imports, caller/dependency/visibility/collision/module/cycle/orphan/cfg impact, snapshot/version, affected tests, and no caller auto-edit; `code.move_symbol.commit` requires confirmation, revalidation, repository/worktree grant, recovery/reindex operation, and durable receipt.

### 13.4 Knowledge

- `/knowledge` is a complete, cursor-paged inventory of every authorized memory-bearing fact/version, knowledge entity/version, decision, contradiction, and relation assertion across All or an explicit narrower scope. It exposes table, graph, timeline, similarity/matrix, and collection views over one query/snapshot, with kind/state/tag/owner/project/source/trust/evidence/time/retention filters and exact searched/skipped/hidden/redacted/unknown coverage.
- Fact, memory, entity, and relation selection opens the universal inspector with current content/state, version lineage, source transcript/Turn/run anchors, association predicates and evidence, trust changes, retrieval/use/feedback history, conflicts/supersession, retention/holds, and deletion impact. Previous/next/related navigation never loses the active query or scope.
- Graph-resident holographic memory is durable user data, not a disposable code-index cache; graph-generation cleanup and reindex controls must never imply that facts or fact-entity relations will be deleted.
- Retrieval history and candidate explanations.
- Curator/reflection candidates and exact source→candidate→validation/policy→autonomous effect→use/outcome→autonomous revision/recovery chain; imported approval/apply events are labeled historical/provider evidence.
- Similarity projection, provenance graph, version table, and nearest-neighbor table; projection never replaces precise scores.

### 13.5 Delivery

- Worktrees, branches, commits, PRs, checks, reviews, releases, and remotes.
- Separate produced, observed, and merely encountered artifacts.
- Local semantic snapshot and live delivery facts display separate fetched/indexed timestamps, head/base/merge-base, changed-file digests, coverage, and reconciliation.
- Drift blocks joined impact claims and offers refresh-live, reindex-local, or recompute-both actions.

### 13.6 Automations, skills, and autonomous curation

- `/automations` inventories every authorized job, schedule, run, actor, artifact, candidate, validation, autonomy decision, automatic effect/recovery, use, and outcome; `/skills` inventories every skill/package/version/materialization and host state. Both are cursor-paged Explorer-backed views with All/project/type/state/time/source/outcome filters, table/graph/timeline pivots, coverage, and stable anchors—not dashboard samples or status cards.
- Automation status separates cheap scheduler wakeups, dirty scopes, admission/defer decisions, coalesced skip episodes, actual runs, and nonterminal effect reconciliation. The pending-work view groups thread/project/profile dirty keys by task and trigger class, then shows reason, first/newest activity, coalesced delta, per-shard current/considered/consumed/included frontiers, finalized boundary, active-writer/coverage state, quiet/max-debounce countdown, dependency-selector/semantic-input/evaluation-snapshot digests, and next reconsideration. Admission history distinguishes `NoRelevantChange`, `IdenticalTerminalInput`, `QuietPeriodActive`, `BelowMinimumDelta`, dependency, lock, retry, budget, pause, unknown/partial/deferred/reconciliation state, and exposes model/tool/token/cost work avoided. One expandable skip episode shows first/last evaluation, count, exact frontier tuple, policy/config digest, and why the job is dormant; interval/lock noise never renders as thousands of fake runs.
- Run detail includes immutable input-contract/manifest, included evidence, typed trigger and reevaluation policy, per-shard current/considered/consumed/included frontiers, active-writer snapshot and coverage, semantic effective-input versus evaluation-snapshot digests, generic operation attempts, backoff/deadline/circuit, poison quarantine or nonterminal effect-reconciliation state, and last-terminal cursor/effect receipt. An old dirty generation, `current > considered`, `considered > consumed`, unknown writer, repeatedly reopening circuit, or starved scope is unhealthy even if recent wakeups were “skipped successfully.” Selecting a frontier, writer, reconciliation receipt, or coalesced skip episode opens its exact source/evaluation anchors in Explorer and the Loom.
- Run detail renders the exact curator/session-reflector/skill-writer waterfall and lets users traverse source sessions/Turns → artifacts/candidates → validation/evaluation → autonomous decision/effect → memory/skill versions → retrieval/injection/use → feedback/outcome → revision/recovery. Skill detail shows source evidence, content/version diff when authorized, validation/loadability, target hosts/materializations, referenced capabilities, uses/outcomes, drift, and revision/archive lineage. Selecting any step synchronizes the Brain graph, Loom interval, Explorer result, and inspector.
- Capture Claude workflow runs, Codex goals, and Hermes-style curator/session-reflector/skill-writer concepts as typed related entities, not one ambiguous run type.
- Managed skill lifecycle: evidence→candidate→validation/eval→policy decision→autonomous materialization→injection/use→outcome→autonomous revision/recovery/archive.
- Managed memory lifecycle uses the same product spine while preserving fact-specific trust/conflict/supersession/deletion semantics.

### 13.7 Observatory

- Project × subsystem health matrix; ingest lag; rewrite/backfill/parser coverage; identity conflicts. Provider freshness drill-down shows the one daemon operation's source frontier/target watermark, leader/joiners, progress, source-open/sweep/read-byte/RSS/amplification metrics, cancellation boundary, partial failures, and terminal receipt; a page or search refresh never starts hidden ingestion.
- A version-aware Logs/Diagnostics explorer shows the immutable active runtime build set and every record's producer component/version, optional collector version, time, safe code, correlation, and source coverage. Exact build, semantic-version range, include/exclude set, current runtime set, compatible protocol/manifest, and explicit legacy-unknown filters are URL/saved-view state; result headers always show searched, returned, excluded-by-version, and unknown-version counts. It also shows segment age/bytes, total quota pressure, retention horizon, holds, importer disposition, and orphan/dangling lifecycle findings. Version boundary bands can pivot the same records into Loom/Explorer without deleting old evidence or treating collector version as producer version.
- Catalog/activity/project/graph/blob health; migrations; projection lag; query latency/caps/partial results.
- Database Authority shows the sole daemon/store authority, writer/read-pool/checkpoint/snapshot state, latest verified backup/integrity receipt, and `StoreIsolationStatusV1`. Strong mode distinguishes dedicated-service-identity and remote-authority-only and shows proof generation/expiry, service-manager real-client probe health, runtime ACL-drift evidence, source/effect-broker health, and exact remediation state; `SameUserDegraded` is visibly degraded with `database_read_denied_to_clients=false`. No view reveals a store path, SQLite URI, file handle, key, raw backup, grant resource, or “open database” action.
- Every operational trend consumes plan 26's `VisualizationEnvelopeV1<MetricSeriesViewV1>`: descriptor, exact points/denominators, threshold bands, configuration/policy/model/catalog boundaries, incidents/remediations, comparison baseline, uncertainty/coverage, and drill-down anchors. Charts never receive an unannotated point bag or invent browser-local thresholds; selecting a boundary/incident opens the same evidence inspector and Causal Loom interval.
- Doctor findings display severity, observed owner, remediation authority, evidence, and only legal actions. Foreign-owned packages are informational; the UI cannot render an update/repair button for foreign or unknown authority.
- Storage identity split is a named conflict with both safe candidates, evidence, and no initialize action; it is never rendered as “no index.” An administrator may enter the merged-#425 consolidation workflow: inspect two nonempty source identities, review path-plus-file/inode holder/freeze/reservation state and per-table/artifact dispositions, create/verify backups, produce the deterministic plan/confirmation token, start or resume the durable staging/verification/cutover operation, and copy exact recovery. The UI never accepts arbitrary raw paths, performs client-side merge logic, or starts consolidation from Settings.
- Store consolidation is operational administration, visually and permission-wise separate from Evolution/curation. It may require confirmation, pause on open holders or failed verification, and expose rollback/recovery receipts. Autonomous memory/fact/managed-skill/profile curation retains no per-item preview/apply/rollback queue and cannot invoke consolidation.
- Provider integrations show `Detected/Installed/Configured/Healthy/Degraded/Partial/Unsupported/ForeignOwned` with hook/tool/session coverage, missing pieces, last verification, and repair owner. Provider branding never substitutes for health evidence.
- Codex hook inventory is per definition, not one plugin-health badge: source layer and representation, event, matcher behavior, redacted exact-definition digest, managed/project-trust/review/disable/effective/skip state, overlap invocation group, `TraceDecayOwned|HostObserved|Unobservable` run visibility, evidenced last handler run/arbitration only when observable, and `/hooks` remediation. It never infers foreign execution or renders command bodies, paths, stdin/stdout, environment values, transcript locators, prompts, or tool payloads. Additive definitions remain separate rows even when they target the same event; observable concurrent runs and the single advisory TraceDecay effect-arbitration winner remain separately visible.
- Claude hook inventory uses the same per-definition frame with host-native axes: user/project/local/managed/plugin/skill/agent/session/built-in source, component lifetime, exact 30-event/version disposition, matcher/`if`, command/HTTP/MCP/prompt/agent type, sync/async/rewake, timeout/platform class, disable-all/managed-only/exemption, host dedupe, run/completion/delivery Turn, decision/effect coverage, and lag/spill state. `/hooks` is labeled read-only and sensitive; the UI never copies its command/args, URL/header, MCP input, prompt/agent body, path, environment, or output. Foreign definitions are inert, and `Unobservable` never grows a fabricated last-run row.
- Integrations renders host deployment targets separately from TraceDecay data ownership. Every named Hermes profile may have distinct installed/configured/trust health, but all topology edges terminate at the same user-global TraceDecay `ProfileId`, daemon, catalog, and store set; the UI never offers a Hermes-specific TraceDecay profile/database selector.
- Daemon/update rows show lease epoch, accepting/draining/stopped state, in-flight counts, durable progress/receipt, takeover/recovery, and safe retry; process exit alone never renders upgrade success.
- Hook/hint/tool opportunity, emitted, adopted, missed, human-corrected, unresolved and terminal-outcome metrics with denominators/horizons. Hint panels refuse rates when lifecycle/category conservation fails and link the FM-156 incident; impossible partitions never render as plausible funnels.
- Generated Capability Registry for every current use case and MCP/CLI/HTTP/dashboard/skill/hook binding: semantic version, request/result schema, read/mutate/autonomous/confirmation/recovery mode, scope, privacy, cost, local/live/joined evidence, availability/gap, catalog digest, and “open guided action.” Old curation approval/apply names remain operator-only migration evidence, never current help/hints/catalog.
- Storage growth, blob integrity/GC, retention, redaction/privacy, remote freshness, malformed rows.
- Provider/project/domain coverage matrix and direct drill-down to evidence.
- `/observatory/sync` renders plan 28's stable topology graph: Brain nodes, fenced authorities, shard placements, replicas/caches, repository/checkout/worktree bindings, sync links, privacy boundaries, and backup/recovery edges. Shape, stroke, labels, and accessible table duplicate color; it is not an unconstrained force graph.
- Synchronized panels show authority/node epochs, placement generation, per-shard watermarks, cache/replica lag, pending spool bytes/age, gaps/conflicts/quarantine, schema/privacy/grant compatibility, certificate/key age, revocation, backup age, verified recovery point, RPO/RTO, and old-authority fencing. Every alert deep-links to Explorer/Loom/config/audit evidence.
- Failover stays disabled on mere unreachability. The inspector names the required positive fence class/receipt—graceful shutdown, external exclusive-resource revocation, or independent quorum lease—and offers a separate forensic-fork export when same-Brain promotion cannot be proven safe.
- Repository-correlation compare exposes credential-free remote aliases and immutable Git proof, with fork/shallow/rewritten ambiguity and explicit adopt/split action. Same paths/names never render as proof; sensitive paths/addresses remain redacted.

### 13.7A Integrations workspace

`/settings/integrations` is the generated administrative workspace for understanding and operating TraceDecay across supported agent hosts. It is not a plugin card grid or a filesystem editor. Its desktop composition is a synchronized target rail, topology canvas, capability-difference matrix, and inspector/history rail; narrow screens preserve the same selection in ordered sheets. The generated plan-10 client is the sole data/command path. Browser code never probes host processes, reads plugin caches/files, parses provider configuration, guesses installation paths, or constructs install commands.

The topology uses stable layered geometry rather than an unconstrained force graph:

```text
release package set → host target/install scope → package/component
                    → skills · roles · hooks · MCP facade
                    → logical registration/profile → effective capability families
```

Nodes show desired/installed/observed/effective state, version and signed manifest/catalog/profile digest, owner/trust, last-probe cache age, and restart/reconnect requirement. Edges distinguish contains, installs, registers, exposes, depends-on, blocked-by, and drift; shape/line style/text duplicate color. Selecting any node synchronizes the matrix, exact status, current/previous operations, settings provenance, safe evidence anchors, and legal actions. Topology filters cover host, package/component kind, desired/effective/drift/health, owner/trust, stale observation, MCP registration/profile, restart class, and operation state without changing server truth.

The capability-difference matrix is host × component/capability, virtualized and server-paged. Every cell renders plan 09's `HostIntegrationDifferenceRowV1` as separate desired, `HostCapabilityDispositionV1` support, installed, observed, and effective axes plus omissions/fallbacks/evidence/legal actions; it never flattens those axes into one state vocabulary. `Undocumented`, `Stale`, `TrustPending`, absent, disabled, installed-but-unobserved, and observed-but-ineffective remain visually and textually distinct, and none renders as healthy. Users may pivot to package/component inventory, MCP exposure, or compatibility view without issuing a different semantic query. A pinned legend, row/column summaries with exact denominators, compare-two-host mode, and saved filters make large fleets readable; the accessible table is feature-equivalent to the matrix and is the export/copy source.

The inspector includes package/component/registration/profile versions and digests, desired install scope and enablement, generated dependencies/conflicts, ownership and trust authority, observation freshness/stale-cache reason, effective grants/scope/sensitivity/effect ceilings, eager/deferred MCP discovery behavior, inherited/unsupported host caveats, credential-reference status without values, drift, and exact restart/reconnect/reload impact. It links to immutable configuration revision, integration operation, audit/effect/compensation receipts, doctor evidence, and prior install/update/repair/uninstall/verify history. It never renders a raw host path, configuration body, command line, environment value, credential, or arbitrary manifest.

`Install`, `Update`, `Repair`, `Uninstall`, and `Verify` are generated actions visible only when the application returns the corresponding legal admin capability. They submit the typed expected-version/idempotency command, immediately pin the returned `OperationRef`, stream/poll shared operation progress, preserve uncertain-effect reconciliation, and show terminal/restart instructions. `Verify` is the only action that requests a fresh probe; passive page refresh uses persisted observation. Foreign/unknown ownership disables mutation with named remediation authority. Desired-state forms edit plan-20 package/component/install-scope/trust/update/credential-reference settings, then show the separate pending integration operation; save never pretends to install.

Accessibility is a release gate: complete keyboard traversal and roving focus, visible focus at every zoom, screen-reader names that include row/column and full state, matrix/table parity, reduced-motion topology transitions, 200% zoom/reflow, high-contrast and light/dark themes, non-color state encoding, and focus restoration after live deltas or dialogs. Loading, empty, denied, locked, partial, stale, offline, unsupported, foreign-owned, reconnect-required, operation-running, reconciliation, and terminal-error states have designed fixtures and cannot collapse into a spinner or generic red banner.

### 13.8 Costs

- Tokens, latency, model/provider/tool usage, context/compression, dollar cost, estimated savings, and methodology.
- Preserve `actual`, `tokenized`, `estimated`, `mixed`, unknown model, price source/freshness/offline, recording gate, session ledger, model/day aggregates, and legacy lifetime counters.
- Every aggregate drills to sessions/Turns/messages/tools/hints/outcomes and declares confidence/missing denominator.

### 13.9 Privacy

- Privacy Observatory consumes `PrivacyProtectionStatusV1`: configured policy, effective non-disableable floor, source/sink/detector coverage and versions, last verified scan, sanitized/quarantined/legacy-unscanned/unknown counts, and restore eligibility. It never derives “enabled” from historical lossy rows.
- The primary artifact is a source × sink × privacy-domain coverage matrix synchronized with safe finding-class/state counts and descendant remediation lineage. Unknown/locked/corrupt/skipped coverage remains visible and prevents a clean claim.
- Findings show opaque ID, broad class/confidence/state, safe source/sink class, age, remediation/rotation state, and legal actions. No candidate, substring, length, plaintext hash, exact span, secret fingerprint, or raw field path is rendered.
- Scan/remediation/quarantine actions use their generated operation-specific inspect/plan/start/status/verify/hold/release commands and elevated authorization. Rotation/revocation is presented before deletion; restore remains blocked until isolated scan/rebuild/promotion receipts pass.
- The named current gaps—Hermes projection-only ingest, duplicated full-command hook analytics, unscanned bounded MCP failures/summaries, direct response-handle/backup copies, raw unauthenticated dashboard exposure, memory metadata/V11 vectors, and false status inference—each have an inspectable safe regression row.

## 14. Replay labs and Evolution Studio

The playground is one **experiment cockpit**, not fourteen unrelated debug forms. `LabWorkbench` renders catalog-defined evaluator schemas inside the shared `ExperimentSpecV1`/`ExperimentRunV1` lifecycle and owns immutable input selection, source backlink, requested-mode/actual-fidelity banner, typed parameter controls, baseline plus bounded variants, pipeline DAG, run/cancel/resume/retry/minimize, branch history, sweeps/ablations, synchronized trace/output/explanation diff, metrics/cost/coverage, manifest/resource-access rail, annotations, saved recipe, reproducibility export, failure minimization, and the separate fixture-promotion command.

The canonical desktop anatomy is:

```text
┌ source + fidelity + versions + budget + run controls + terminal receipt ┐
├ input/parameter delta rail ┬ pipeline DAG / branch graph ┬ manifest rail ┤
├ baseline + 1..5 variants: synchronized playhead, graph, timeline, output ┤
├ case/sweep matrix · heatmap/small multiples/Pareto · causal drill-down   ┤
└ coverage · substitutions · side-effect receipt · anchors · save/export ┘
```

One run is one operation-backed cohort. Its cursor-paged `ExperimentCellV1` rows explicitly address variant, evaluator, corpus case, repetition, and sweep values; the sweep matrix, Pareto point, trace, output, and cost all resolve to that cell and anchor. `ReplayTraceV1` pages stable stage IDs for one cell across input/context assembly, candidate generation, rule/policy decisions, tool/model calls, output, and evaluation. `ReplayComparisonV1` pages typed `ReplayComparisonCellV1` alignments across one baseline cell and at most five variant cells and records added, removed, substituted, changed, and unaligned stages. Moving the playhead cross-highlights the same comparison cell/stage in pipeline, timeline, graph delta, message/code/config diff, candidate waterfall, metrics, and explanation; missing stages occupy explicit gaps rather than shifting the comparison.

Experiments form an immutable merge-free DAG through the sole `ExperimentBranchRefV1`. Forking creates a child experiment with parent experiment/run/variant, changed-field patch, source scene/anchor, parent manifest, and output relation; variants are coordinates inside a spec and never form a second ancestry. Bounded one-factor, grid, and pairwise sweeps declare typed values and require deterministic seeds, explicit corpus/cases, 2–6 variants, repetition count, evaluators, concurrency, wall/CPU/RSS/overlay/disk/network/output/FD/process/token/cost/egress budgets, and preflight estimate. They support cancel/resume/retry through the shared operation kernel and expose partial coverage. Heatmaps, small multiples, Pareto fronts, regression slices, and per-cell causal drill-down resolve to the same run/cell/stage anchors.

The application, not a badge, proves isolation. Every run executes through a versioned worker protocol in a fresh process with empty environment, closed inherited descriptors, no ambient credentials, frozen clock/RNG, verified read-only input mounts, bounded disposable overlay, no production repository/store/query-counter/lease/cache/write access, and only brokered explicit model/network grants. The full resource budget is visible before launch; timeout/cancel kills and reaps the process tree. `ReplaySideEffectReceiptV1` lists every opened resource, broker call, granted model/cost use, denied attempt, overlay write, resource high-water mark, forced termination, and final zero-live-effect proof. A remote/model-dependent run can be reproducible in manifest and recorded outputs without claiming byte determinism.

Guided, analyst, and expert modes are disclosure presets over the same experiment state, not separate products: guided narrates the next step and pins the minimum panels; analyst exposes comparison/sweep/evaluator controls; expert exposes complete manifests, schemas, receipts, and recipes. Switching modes never changes inputs, query, run, selection, or result and always exposes where a hidden panel moved.

Every experiment, run, cell, stage, comparison, comparison cell, and minimized failure receives a canonical `RetrievalAnchorId`. The shared saved-view experiment variant can reopen the exact experiment/run/cell/stage/comparison/comparison-cell/reduction/playhead; ordinary annotations target those anchors. Reproducibility export contains the redacted manifest, variants, coordinates/cells, outputs, anchors, annotations, environment, CLI/MCP/HTTP/SDK recipe, side-effect receipt, and expiry. Deterministic delta-debugging can minimize typed removable dimensions—events, context blocks, candidate channels, rules, configuration patches, graph edges, or corpus cases—while preserving a named failing predicate, reduction tree, cap, and every attempted reduction. It may produce only a secret-scanned fixture candidate; the one `experiments.fixtures.promote` command remains separately authorized and never automatic.

Observed foreign hook definitions are inert evidence in every lab. Replay may use a recorded sanitized result or TraceDecay's catalog renderer semantics, but it never executes, shells, imports, or trusts an observed command, prompt, agent handler, path, or environment.

The cockpit interaction model follows the useful parts of [Phoenix Playground](https://arize.com/docs/phoenix/prompt-engineering/how-to-prompts/using-the-playground), [LangSmith experiment comparison](https://docs.langchain.com/langsmith/compare-experiment-results), and reactive [Observable notebooks](https://observablehq.com/documentation/notebooks/), while retaining TraceDecay's local-first evidence, privacy, and no-live-side-effect guarantees.

| Lab | Input panels | Required output panels |
|---|---|---|
| Hint | historical event/session position or sanitized synthetic event; host/provider; project/ref/snapshot; deterministic/scout engine, policy/config/index/memory/tool/catalog/model capability; optional observed host definition/source/control snapshot | normalization, trigger/context delta, hypotheses, approved bounded reads/anchors, model/deterministic candidates, suppression/dedupe/cooldown/escalation/budget, exact addressed envelope/payload, delivery timing, tokens/latency/cost, adoption/outcome; Codex replay shows its exact ten-event/trust/concurrency contract, while Claude replay shows the pinned 30-event oracle, five handler kinds, source/component lifecycle, matcher/`if`, host dedupe, sync/async delivery Turn, exit/transport/model output semantics, lag/spill/version/privacy coverage, with all foreign handlers inert |
| Retrieval | query/scope, memory/index/model/ranking versions, candidate snapshot | lexical/entity/vector/recent candidates, exclusions/redaction/dedupe, trust/decay/usage features, final order, coverage, no-counter proof |
| Ingest | source bytes/ref, parser/redaction/identity/projector versions | source→observation→events→projection rows, hashes/offsets/idempotency/externalization/quarantine/unresolved identity, version diff |
| Query | visual/source `TraceQueryV1`, scope, watermark, budget, planner/index/ranking | AST, cost, shards, pushdown, operators, rank/merge, cursor, coverage, equivalent CLI/MCP/HTTP |
| Search Quality | historical query/Turn/task anchor or sanitized synthetic query, current/as-of/evolution/forensic mode, corpus/qrel/cutoff, retrieval profiles, channel/model/index/ranker/summary versions | per-channel waterfall, logical-copy representative, summary-DAG horizon, temporal correction/supersession/conflict lineage, shard fusion/diversity/rerank decisions, labels/agreement, per-stratum nDCG/MRR/recall/precision/temporal/duplicate/no-answer/resource regressions, exact final `RetrievalAnchorId`s |
| Scope/Federation | exact locator/selector or historical anchor, registry/catalog/ref/index snapshots, candidate resolver and shard-plan versions | canonical `ScopeSelectorV2`/`ScopeResolutionV2`, candidates/evidence, selected snapshots, pruned/opened/unavailable shards, one-step retry, cross-transport request/result diff; never changes registry |
| Correlation | session/worktree/ref/commit/PR/code candidates and local/live snapshots | evidence windows/events, features, confidence, alternatives, abstention, Git reconciliation, labeled-case promotion |
| Coordination | historical presence/work claims, agents/worktrees/refs/goals, overlap evidence, policy/catalog/dedupe/suppression state | proximity classes/ranking, material-overlap decision, safe summary, one-hint selection or suppression, stable anchor/recipe, legal action simulation, outcome attribution/coverage; never sends or acknowledges |
| Orchestration | initiative/plan/task/attempt/lease/executor/workspace/packet snapshots, policy/config/catalog/model/index versions, explicit time and fault point | decomposition/plan validation, gates/readiness/critical path, route eligibility/fairness/retry, packet ranking/omissions, sibling materiality, advisory-claim/lease-acquisition/fence/cancel/effect reconciliation, requested-vs-actual route/cost/outcome, exact anchors; never acquires leases/spawns/sends/mutates |
| Scheduler | task, effective config, ledger/activity/lease/policy snapshots, explicit time | due/skip/block tree, config source, watermark, proposed lease/work/effects, revalidation requirements |
| Memory | candidate/source, sensitivity/transience, entity/fact/conflict/trust/retrieval/retention/autonomy-config snapshot | automatic effect/rejection/defer/quarantine/protect/no-change, duplicate/conflict/supersession, trust/retrieval/deletion descendant effects, explanation; never a human decision control |
| Policy Diff | corpus plus two bundles | changed/unchanged/regression/win/unlabeled, case diff, latency/token distributions, affected categories, coverage/digest |
| Evolution | bounded evidence collection or selected skill/memory/policy/automation version and historical use, actor/run/config/policy/catalog/index snapshots, old/current candidate versions | evidence→candidate→validation→autonomous decision/effect→use→outcome→revision/recovery lineage, structured version and decision/tool-route diffs, wins/regressions/unknown horizons, latency/token/cost/privacy/coverage, and zero-live-effect receipt; never applies, rejects, rolls back, or changes live curation |
| Privacy | reserved/invalid synthetic canary, parser/detector/policy versions, bounded sink matrix | parse/decode tree, safe detection classes, overlap/marker/receipt, sink eligibility, latency/coverage/version diff; never loads a real candidate or mutates live findings/policy |

The Search Quality workspace has generated, capability-gated subviews for corpus versions, qrel versions, candidate pools, judgments and supersession chains, adjudications, generic Search Quality experiments/runs/cells/comparisons, aggregate/redacted reports, fixtures, and retrieval profiles. Artifact reads bind the corresponding `retrieval.*.list/get` operations from plan 15 §0.1; experiment reads/runs bind plan 10 §8.5. Direct artifact actions bind only `retrieval.corpus_versions.create/freeze`, `retrieval.qrel_versions.create/freeze`, `retrieval.candidate_pools.create`, `retrieval.judgments.record/supersede`, `retrieval.adjudications.record`, `retrieval.evaluation_reports.publish`, `retrieval.profiles.publish/activate`, and the shared `experiments.fixtures.promote`. The UI shows immutable lineage, exact frozen inputs, authorization, sanitization/secret-scan and side-effect receipts, and operation state; it never rewrites a judgment, edits a frozen artifact, publishes private content, or changes a live query's pinned profile.

### 14.1 Evolution Studio

Evolution Studio treats self-improvement as an inspectable product loop:

```text
usage/session/diagnostic/hint evidence
  → curator/reflector/skill-writer actor and goal
  → candidate or artifact
  → validation/eval and autonomy-policy decision
  → autonomously materialized skill/memory/profile version
  → injection/retrieval/tool use
  → observed or unresolved outcome
  → autonomous revision/recovery, archive, or contradiction
```

Views:

- lineage DAG with exact actors, runs, inputs, artifacts, versions, autonomy decisions/effects, uses, outcomes, and recoveries;
- version diff for skill instructions, policy rules, memory facts/trust, schedules/config, and tool catalogs;
- effectiveness trends with eligible denominator, adoption, terminal horizon, coverage, confidence, and no-outcome state;
- replay selected historical use under old/current version;
- autonomous decision ledger with validation/config evidence, staged scope, monitoring horizon, effect/recovery receipts, and pause/resume/run-now/pin/protect/exclude controls; no item-level apply/reject;
- “why did this evolve?” evidence bundle linking source Turns, failures, corrections, diagnostics, and prior outcomes.

Present self-improvement as autonomous but evidence-bound, not infallible. Automatically rejected/deferred/quarantined candidates, weak evidence, unresolved outcomes, regressions, conflicts, recovery loops, and policy/config drift are first-class states; inspection never becomes a manual approval gate.

## 15. Visualization, LOD, and interaction system

Every substantial visual checks in a mini-brief at `dashboard/app/src/features/<feature>/visual-brief.md` containing analytical question, data grain, exact/sampled semantics, encoding, selection, keyboard/touch behavior, mobile continuation, URL state, synchronized fallback, export scene, benchmark fixture, and accepted desktop/mobile concept reference.

A thin reusable `WorkspaceSlotFrame` owns only slot chrome, snapshot/coverage status, lifecycle, and composition synchronization. Renderer registry entries compose narrow typed capabilities—`ViewportAdapter`, `InteractionAdapter`, `AccessibilityAdapter`, `FallbackAdapter`, and `ExportAdapter`—over the shared visual-semantic ontology and `VisualizationEnvelopeV1<T>`. No universal switch-heavy renderer component owns camera, interaction, accessibility, fallback, and export behavior for every artifact. Domain features provide typed visual specs and inspector callbacks; they do not create chart wrappers, workers, query filters, selection stores, legends, exporters, or accessibility trees. Linked composition state follows the same scene/state principles as [Vega-Lite parameters](https://vega.github.io/vega-lite/docs/parameter.html) and [Grafana Scenes](https://grafana.com/developers/scenes/core-concepts), but the TraceDecay application remains the source of query/data truth.

### 15.1 Renderer choice matrix

| Analytical artifact | Primary implementation | DOM/mark budget | Fallback and export |
|---|---|---|---|
| Brain/topology and large relationship graphs | winner of the PR 26 renderer bakeoff; Sigma.js/Graphology, deck.gl/custom typed-buffer WebGL, and Canvas/worker candidates are evaluated rather than preselected | `50k` loaded nodes/`200k` edges benchmark plus a 10× transfer/layout stress corpus; interactive/labeled subset bounded by legibility | searchable outline, relationship table, adjacency matrix; deterministic SVG/table export |
| Workflow/provenance/Turn DAG | ELK worker + Canvas with DOM labels under budget | `< 2k` visible marks, otherwise collapsed groups | ordered relationship/evidence list; deterministic SVG |
| Causal Loom | Canvas density/marks + virtualized DOM transcript | `250k` density marks benchmark; sanitized native/canonical events requested in bounded pages | chronological table/transcript; fixed-viewport Canvas or table export |
| Time series, bars, heatmaps, distributions | ECharts with custom semantic theme | aggregate bins only | generated directly labeled table; SVG/PNG export |
| Dense dependency/coverage matrix | Canvas matrix with accessible row/column controls | viewport tiles, no unbounded cells | sorted relationship/status table; PNG/SVG/table export |
| Source/message/diff | CodeMirror 6 with virtualized payload slices | bounded lines/bytes per page | semantic preformatted text/download under authorization |
| Small precise lists/trees | DOM + TanStack Virtual | visible rows + overscan <= 3 viewports | same semantic DOM is fallback/export |

Do not create a graph when a ranked list or matrix answers the question more precisely. Do not create a chart for one scalar. The user can switch any graph to outline/table, any chart to exact table, and any timeline to transcript.

PR 26 freezes the graph renderer only after a code-backed bakeoff on the current and 10× corpora measures cold/warm render, incremental typed-buffer append, layout/update cost, GPU and JS memory, picking accuracy/latency, overdraw, label quality/collision, LOD transition churn, context loss/recovery, bundle size, accessibility/fallback integration, deterministic export, and concept fidelity on target hardware. The plan does not equate theoretical loaded-node capacity with usable visual quality; [Sigma's own documentation](https://www.sigmajs.org/docs/) and [deck.gl performance guidance](https://deck.gl/docs/developer-guide/performance) are inputs, not conclusions. The losing prototype/dependency is deleted before the foundation PR merges.

The bakeoff ADR preregisters three reproducible hardware tiers before measurements: constrained 8 GiB/iGPU at DPR 1, reference 16 GiB/iGPU-or-entry-dGPU at DPR 1/2, and mobile 6 GiB/touch at portrait/landscape DPR. Browser/OS/GPU/driver, thermal/power mode, viewport, corpus, atlas generation, and five-run median/p95 are recorded. A candidate is disqualified—regardless of aggregate score—if it fails keyboard/screen-reader outline parity, deterministic export, selected-mark visibility, label/collision limits, context-loss recovery, reduced-motion behavior, CSP/offline constraints, reference-tier memory budgets, mobile fallback, or truthful 10× LOD degradation. Atlas tile first/prefetch transfer and decode must meet the same `first useful evidence <=2 s` and local interaction budgets; 10× may aggregate more but cannot lose selected/consequential entities, coverage, or navigation.

Among candidates passing every hard gate, the committed scorecard weights perceptual legibility/object constancy 30%, interaction/render latency 20%, GPU+JS memory 15%, resilience/context loss 10%, accessibility/export integration 10%, bundle/initialization cost 10%, and implementation/maintenance surface 5%. Each metric has a frozen normalization range in the ADR; no post-result weight changes are allowed. A tie within 2 percentage points selects the smaller dependency/code footprint. The ADR includes raw results, screenshots/recordings, disqualifications, sensitivity analysis, winner, and deleted losing prototypes.

### 15.2 Product visual catalog

| Product question | Interactive visual | Selection/drill-down | Precision fallback |
|---|---|---|---|
| How is the whole profile connected? | semantic-zoom Brain clusters with aligned activity | cluster→project/workflow→neighborhood→evidence | outline + adjacency matrix |
| Which projects/subsystems are unhealthy? | project × ingest/projection/query/storage/privacy/remote heatmap with sparklines | cell→coverage/store/events/diagnostics | directly labeled status table |
| What happened through this workflow? | Causal Loom density, lanes, delegation rail, impact ribbon | bin→Turn→event→evidence chain | chronological transcript table |
| What did one agent or Turn do? | parent tree + Turn DAG + compact tool/code/delivery waterfall | actor/Turn/tool/file/goal/outcome | nested outline + evidence ledger |
| Which nearby agents may overlap? | compact worktree/agent map synchronized to ranked evidence ledger | overlap→agent/worktree/file/symbol/test/goal/review evidence and safe action | exact presence/overlap/action table |
| How does a cross-repository plan execute? | graph-of-graphs with plan outline, dependency DAG, critical path, Kanban/workload/executor projections, and claim-overlap overlay | initiative→plan version→task/gate→attempt/packet/worktree→artifact/outcome/PR | task/dependency/attempt ledger + nested outline |
| What changed across code? | snapshot/symbol evolution DAG, diff viewer, churn small multiples | symbol→occurrences/diff/callers/tests | file/symbol change table |
| Where is coupling/risk? | dependency structure matrix plus cycle/impact overlay | cell/component→edges/symbols/affected tests | sorted coupling/risk table |
| How does work connect to Git/delivery? | commit/ref/PR graph with local/live evidence overlays | revision/PR→sessions/agents/code/checks | Git history/reconciliation table |
| How does knowledge evolve? | fact/version/provenance DAG, trust line, contradiction pairs | version→source/retrieval/feedback/decision | version/provenance ledger |
| Which facts/sessions/code are related? | bounded similarity projection and cluster hulls | point/pair→score components/evidence | nearest-neighbor table |
| How do automations execute? | scheduler swimlane, run waterfall, artifact/candidate/decision lineage | run phase→actor/tool/artifact/decision | run/artifact table |
| How do skills/memory improve? | Evolution evidence→candidate→version→use→outcome DAG plus effectiveness trends | version/use/outcome→source Turns/evals | lifecycle/version ledger |
| Are hints/tools useful? | eligible→suggested→delivered→used→terminal funnel, category matrix, unresolved-horizon survival line | stage/category→evaluation/payload/action evidence | exact denominator/outcome table |
| Where do tokens/costs go? | time series, provider/model/tool heatmap, session small multiples | bin/model→Turn/message/tool ledger | exact cost ledger |
| Is context being compressed safely? | source→summary DAG, depth distribution, compression line, missing-payload markers | node→source ranges/payload/decision | LCM node/source table |
| Is data complete and durable? | storage-growth lines, shard/source coverage matrix, lag/disposition histograms | store/shard/source→health/receipts | exact operational table |
| What would an engine decide and where did variants diverge? | experiment pipeline DAG + synchronized replay playhead + baseline/variant trace/output/explanation diff | run→stage alignment→input/evidence/version/resource receipt | ordered stage/alignment/result ledger |
| Which parameters/cases cause a regression? | branch DAG + sweep heatmap/small multiples/Pareto front + minimization tree | cell/branch/reduction→aligned causal trace and stable anchor | exact case/parameter/metric/reduction table |

Every visual shares inspector, scope/time/selection, coverage status, export manifest, and direct table pivot. Each chart title states the question and data interval, not a vague noun such as “Insights.”

### 15.3 Stable layout contract

`layout-cache.ts` keys stable Brain positions by `(profileAtlasGeneration, queryFamily, graphComposition, layoutAlgorithm, layoutVersion, seed)` and evidence-only layouts by `(snapshotId, queryFingerprint, graphComposition, layoutAlgorithm, layoutVersion, seed)`. Server-provided atlas anchors and generation lineage win; existing territories/nodes keep positions during snapshot updates and expansion; new nodes begin at the registered parent/entry boundary and settle without moving unaffected clusters. A saved camera references the same key and migrates through an explicit atlas-anchor lineage or is discarded with an incompatibility notice—never silently aimed at a different territory.

- Force layout runs in a worker and stops at deterministic iteration/energy limits; it never depends on wall-clock frame count.
- Reduced motion uses the final deterministic coordinates immediately.
- ELK options, community detection, bundling, sampling, and aggregation versions are returned and shown in inspector/export metadata.
- Direct evidence edges do not bundle. Aggregate edges may bundle only when exact counts by kind/evidence remain inspectable.
- Layout workers post progressive positions within `500 ms`; main-thread tasks over `50 ms` fail the performance test.
- WebGL context loss switches to a preserved table/matrix and offers renderer restart; selection and investigation state survive.

### 15.4 Common graph interactions

- Click/tap commits selection; hover/focus previews without changing history.
- Shift-click pins or adds to comparison; Escape returns to the previous committed selection.
- Double-click/Enter expands one bounded neighborhood using the server cursor.
- Lasso operates only in explicit selection mode and announces count; touch uses step-through/add buttons instead.
- Committing a lasso, path, brush, cluster, or relation never changes the query implicitly: the action menu offers highlight/filter/exclude/compare/derive-lane, previews the canonical `TraceQueryV1` delta from `query.compose_from_selection`, then records the accepted breadcrumb across every linked slot. Collect is a separate `collections.update` command over resolved selected anchors and shows its own optimistic-version receipt.
- Path mode requires explicit source and target, legal edge kinds, max depth/cost, and at most 20 alternatives.
- Search results reveal and focus a bounded neighborhood; they do not re-run a whole-graph client filter.
- Zoom-to-fit, reset, previous/next result, parent, expand, collapse, switch fallback, and open inspector are explicit controls and keyboard commands.
- Empty-space click clears preview, not committed selection. Drag threshold prevents accidental clear.

### 15.5 Chart rules

- Axes have units; bars/lines carry direct labels when legible; a detached legend is supplemental.
- Unknown denominators use gaps/hatching and text, never zero-height bars.
- Partial, stale, sampled, and comparison series use the semantic ledger plus line/shape redundancy.
- Truncated axes require an explicit break marker and table values.
- Tooltips duplicate, not replace, essential values. Focus exposes the same content.
- Small multiples share scales unless a labeled independent-scale mode is required.
- Every aggregate drill-down uses the exact filter/watermark that produced it.

## 16. Responsive, accessibility, and input behavior

Target WCAG 2.2 AA and completion of every primary workflow by keyboard and screen reader.

### 16.1 Keyboard and screen reader

- One skip link each for command bar, primary view, outline, inspector, and time brush.
- Roving focus for graph outline/lane headers; DOM focus never enters thousands of Canvas marks.
- Canvas/WebGL consumes the bounded structured `AccessibilitySceneV1` from the same sealed `VisualizationEnvelopeV1`: stable parent/child hierarchy, roles, sink-eligible labels, logical position/set size, selected/expanded state, relation IDs, legal action IDs, exact/sampled/capped/unknown visible and hidden measures, coverage, truncation, and continuation anchor. The synchronized outline and viewport summary derive deterministically from that scene; a free-text summary cannot substitute for it. DOM/Canvas/table/export parity fixtures compare entity/relation ordering and counts.
- Timeline has lane list, previous/next consequential event, next Turn, jump to time, expand noise, and read selected chain commands.
- Keyboard shortcuts are discoverable, remappable, disabled while typing, and never single-character-only without a modifier except standard spatial navigation.
- Focus is restored after route lazy-load, sheet/dialog close, inspector close, mutation result, and renderer fallback.
- Errors and coverage changes announce once; live-region announcements are coalesced to at most one per 2 seconds per region, verified with a fake-SSE burst fixture, so streaming rows cannot flood live regions.
- Tables use real headers, sort state, captions, row labels, and pagination. Virtualization preserves logical row count/position.

### 16.2 Mobile and touch

- All targets are at least `44×44 CSS px`; primary controls target `48 px`.
- `touch-action` and gesture ownership let the page scroll until a graph/timeline explicitly receives two-finger/pan mode. No scroll traps.
- Explicit zoom in/out/reset, previous/next, expand, collapse, and lane-step controls provide gesture alternatives.
- Portrait graph uses focused neighborhoods, not the profile topology overview. Portrait timeline shows one primary lane plus collaborator summary and step-through.
- Sheets have apply/cancel/reset, safe-area padding, focus trap, scroll restoration, and selection persistence.
- Keyboard-open viewport, 200% text zoom, orientation change, and iOS/Android browser chrome do not cover primary controls.
- Landscape graph/timeline supports a resizable inspector and maintains a minimum `320 px` evidence region.

### 16.3 Motion and cognition

- Respect `prefers-reduced-motion`; provide an app override but never force motion on.
- Motion explains expansion, selection, new live evidence, or time travel only; no idle pulsing/particles.
- Live additions do not steal focus or move a frozen selection.
- Use plain evidence language: “observed,” “inferred,” “temporal,” “partial,” “redacted,” “unavailable.” Avoid anthropomorphic success copy.

## 17. Loading, empty, stale, partial, offline, privacy, and failure states

`packages/design-system/src/states/` implements the same state vocabulary across routes:

| State | Required presentation | Legal actions |
|---|---|---|
| Loading first snapshot | stable shell and shape skeleton, request/scope label | cancel if expensive |
| Incremental page/layout | keep existing evidence, localized progress | cancel/continue in background |
| Empty complete | exact scope/time/query/mode and evidence of complete search | clear/adjust filter, inspect source coverage |
| Empty partial | never “no data”; list unavailable/locked/redacted sources | retry, unlock, change scope |
| Stale | last-known-good timestamp/watermark and reason | refresh; read-only navigation |
| Partial | missing source matrix, effect on claim/aggregate | inspect coverage, exclude/refresh source |
| Offline | last verified watermark/cache age and authority identity; pending local overlay separately labeled; canonical commands disabled | retry/sync when online; export cached nonsensitive metadata if allowed |
| Authority fenced | old/current authority epochs, placement and recovery receipt | inspect failover; re-seed old node; never resume writes |
| Sync conflict | gap/collision/policy/schema/placement class without payload | inspect receipt, repair or retain locally |
| Locked | metadata/coverage only, no payload/search leak | unlock profile/store |
| Redacted | redaction class/reason and count without hidden IDs/content | request authorized view if policy permits |
| Incompatible | client/server/schema versions and supported recovery | restart/update/open current route; never stale-name fallback |
| Query budget/deadline | partial results plus operator/cost/truncation | narrow scope, raise authorized budget |
| Fatal renderer | preserve table/outline and state | restart renderer, report diagnostics |
| Fatal route/API | stable error code/request ID, no secret detail | retry, diagnostics, navigate back |

The first-scan Brain claim is suppressed when coverage is insufficient to support it; the UI instead selects the coverage issue. Cached content disappears immediately when retention/access events invalidate it.

### 17.1 Privacy and security

- Use no third-party analytics, CDN, external font, telemetry pixel, or remote visualization service.
- Local mode rejects unprotected non-loopback launch/bind. Plan 28's optional protected-remote profile requires TLS, allowlisted authority/proxy trust, enrolled-node/scoped authentication, and application authorization; Tailscale or another VPN may supply reachability but never replaces these controls. Browser bootstrap exchanges a one-time launch nonce for an `HttpOnly`, `SameSite=Strict` session; the nonce never persists in URL/history/storage/logs.
- Send cookies with `credentials: "same-origin"`. Unsafe cookie-authenticated requests include the in-memory `X-TraceDecay-CSRF` token; logout/profile lock clears it. Nonbrowser clients use bearer auth, never a query token.
- Enforce exact loopback `Host`, exact same-origin `Origin`/fetch metadata, no wildcard CORS, and restrictive nonce CSP without `unsafe-eval`; reject forwarded-host and DNS-rebinding variants.
- Never put raw prompts, queries, file paths, branches classified sensitive, payload text, tokens, or error bodies in `console`, performance marks, route names, DOM data attributes, query keys, or screenshot filenames.
- Search and code/message payloads render text, never unsanitized HTML. Markdown uses an allowlist and strips raw HTML/URLs not explicitly safe.
- Generated view types expose content only as plan 18 sink-eligible wrappers or explicit redacted/denied/unknown variants. Feature code cannot cast raw JSON/metadata/error bodies to a renderable string; a lint/test rejects `dangerouslySetInnerHTML`, unchecked markdown/URL metadata, raw compatibility payloads, and transport error `Display` text.
- Copy/share/export previews state exactly what leaves protected storage, apply redaction, and require confirmation for payloads/reasoning.
- Clipboard deep links contain opaque IDs only. “Copy text” is a separate authorized action.
- Profile lock clears decrypted React state, CodeMirror documents, workers, Canvas text atlases, IndexedDB protected cache, and clipboard warnings; metadata may remain only if policy allows.
- Reasoning is opt-in, excluded from search/export by default, and always carries format/visibility/retention labels.
- Deletion previews show descendant projections/blobs/FTS/vector impact, holds, recovery grace, and non-content audit receipt before confirmation.
- The V1 arbitrary-host/unauthenticated raw dashboard seam is a mandatory negative fixture: first-default startup rejects non-loopback bind, every API view authenticates, and no raw content/metadata path survives V2 route cutover.

## 18. Deterministic export and visual QA

Interactive Canvas/WebGL state is never screenshot directly as the only export. `app/src/shared/renderers/export-scene.ts` builds a separate frozen scene from:

- exact query/snapshot/vector watermark and retention watermark;
- fixed viewport, DPR, font files, locale/time zone, layout seed/version, color theme;
- explicit selection, scope/time/query fingerprint, transcript mode, hidden/sampled counts, coverage and redaction report;
- static labels, axes, relationship/evidence key, caveats, and no hover-only content.

Export waits for `render-ready` after fonts/layout/data settle. It rejects if a live snapshot changes, then offers freeze-and-retry. WebGL export falls back to SVG/table/server rendering on unsupported context or size. JSON/Markdown/SVG/PNG exports share one manifest; canonical JSONL/Parquet remain server export formats.

Visual fixtures use a committed redacted corpus, fixed UTC time, fixed fonts, fixed random/layout seeds, and desktop `1440×1000`, laptop `1280×800`, mobile portrait `390×844`, mobile landscape `844×390`, and 200% text zoom. Each feature PR:

1. captures accepted concept and latest browser screenshot at matching dimensions;
2. uses `view_image` on both in the same QA pass;
3. records at least five comparisons across copy, layout, typography, palette, icon/mark semantics, spacing/container, responsive behavior, or motion;
4. updates `dashboard/design/fidelity-ledger.md` with mismatch and fix;
5. records semantic-zoom, lens-overlay, replay-playhead, live-arrival, comparison, and LOD-transition motion/storyboard captures where relevant;
6. runs machine metrics for label collision, edge crossing/occlusion, overdraw, layout/territory churn, selection visibility, empty pixels versus evidence density, and fallback equivalence;
7. fails on unapproved visible copy, generic substituted icons, clipped content, overflow, unreadable chart text, empty Canvas, feature-local semantic marks, or concept drift.

A mismatch is **material** — and must be fixed or explicitly waived with rationale in the fidelity ledger — when it changes copy text, visual-semantic meaning, hierarchy, typography role, icon/glyph, focal artifact, composition, interaction affordance, responsive continuation, evidence density, or concept-authored motion; adds/removes a UI element; breaks the section 2.2 anatomy; or produces a perceptually obvious spacing/alignment drift. `8 px` at `1440×1000` is an automated tripwire, not a definition of visual quality. Perceptual screenshot review, transition review, and the principal user's design judgment remain release gates.

Manual browser QA uses the in-app Browser first. Playwright Chromium/WebKit/Firefox supplies repeatable CI and mobile emulation, not visual taste approval.

Visual fidelity is insufficient if users misread the data. Before PR 32 signoff, the principal user, an independent visualization reviewer, and an accessibility reviewer run the fixed corpus in guided, analyst, and expert density modes over the same state model. Measure time-to-first-correct-insight, false-causality rate, sampled/partial/redacted-state comprehension, lens-overlay disorientation, atlas-location recall after zoom/reload, replay-stage comprehension, task completion/error/abandonment, and interaction count. A screen that matches its concept but fails these tasks returns to concept/design work; PR 32 cannot waive it as polish.

The study protocol and thresholds are frozen before the selected concept is implemented. It includes the principal user plus at least six independent participants spanning experienced agent-tool users, engineering newcomers, one visualization specialist, and one assistive-technology user; a person may fill two specialist roles but not reduce the independent count below six. Each participant runs a randomized subset of the fixed tasks once without coaching, then one retained-orientation task 24–72 hours later; scripted fixtures use identical data/version/coverage and the incumbent UI is measured for every overlapping task. Release requires 100% completion of privacy/coverage/causality-critical tasks, at least 85% first-attempt completion overall, false-causality answers `<=5%`, sampled/partial/redacted interpretation accuracy `>=90%`, replay-stage interpretation `>=90%`, atlas target recall `>=80%`, abandonment `<=10%` overall and zero on critical tasks, median recoverable errors `<=1` per task, and no overlapping task more than 20% slower or 20% more interactions than the incumbent without a documented capability gain. The principal user's sixteen primary workflows must all pass their section 22.2 time limits. Report per-role results, median/p95, errors, confidence intervals where meaningful, recordings/notes under consent, and every failure/retest; changing a task, corpus, threshold, or participant exclusion after results invalidates that run.

## 19. Performance budgets and degradation

Record reference machine, corpus manifest, build mode, browser/GPU, viewport, and five-run median/p95. Every latency/FPS/heap gate runs against the pinned fixture corpus manifest at `dashboard/tests/performance/corpus-manifest.json` (388,000+ messages, 36,000+ code-graph nodes, 71,000+ edges); changing the corpus requires re-baselining every budget in the same PR, so the gates cannot drift silently release to release.

| Budget | Gate |
|---|---|
| Initial shell JS/CSS | `<= 250 KiB` gzip JS; `<= 80 KiB` gzip CSS |
| Localhost first contentful paint | `<= 1.5 s` on the pinned corpus manifest |
| First useful evidence | `<= 2 s` to the route's `first-evidence` performance mark: the first committed data row/mark painted from a non-cache response; each route registers exactly one such mark, asserted in its e2e test |
| Graph/timeline render-ready | `<= 3 s` on the pinned corpus manifest |
| Local interaction response | `<= 100 ms` excluding fetch |
| Main-thread long task | none `> 50 ms` (PerformanceObserver `longtask` entries) during the sixteen scripted section 22.2 tasks, which define the primary workflows |
| Worker progressive layout | first stable partial `<= 500 ms` |
| Graph | `>= 55 FPS` at 50k loaded nodes/200k edges rendered at `aggregate` LOD, and `>= 55 FPS` at `neighborhood` LOD with 2k visible marks; the benchmark records the LOD level each run passed at |
| Timeline | `>= 55 FPS` at 250k density marks; native/canonical event hydration bounded by `EventPageV1` (section 12.5: `<= 500` events and `<= 1 MiB` per page, one page prefetch each direction) |
| Atlas object constancy | unchanged territories move `0 px` across ordinary snapshot refresh; atlas-generation migration reports mapped/unmapped anchors and keeps p95 mapped-territory displacement within the approved transition budget |
| Visual legibility | zero selected-mark occlusion; `<=2` label collisions per `100` visible labels; edge/mark overdraw, crossing, and LOD-transition churn remain below the committed renderer-bakeoff baselines |
| Default response payload | `<= 1 MiB`; page/stream larger authorized payloads |
| Worktree lifecycle/cleanup queue | 100,000 associated workspace relations with 10,000 eligible/blocker transitions: first exact aggregate plus first 100 rows meets the `<= 2 s` first-evidence and `<= 1 MiB` response gates; DOM rows remain `< 600`; facets, retention, orphan classification, and bulk preflight are server-paged/aggregated; no path, Git scan, relation join, or eligibility computation runs in the browser |
| Mobile route heap | `<= 300 MiB` JS heap measured via CDP `Performance.getMetrics` `JSHeapUsedSize` under Playwright 390×844 emulation, sampled 5 s after render-ready, median of five runs; hidden routes stop work |
| Route lazy chunk | per-route budget recorded in a committed budget file; CI fails any chunk `> 10%` over its recorded budget unless the same PR updates the budget entry with a linked justification — an unamended budget file is the definition of "unexplained" |

Remote-Brain performance gates run the sixteen workflows under pinned high-latency, packet-loss, and bandwidth-throttled profiles on desktop and mobile. Each route declares first-view and atlas-prefetch byte ceilings; superseded viewport requests abort before decode/render; `prefers-reduced-data` or an explicit reduced-data setting defaults to density/table/outline before heavy graph/tile hydration. CI proves stale responses cannot overwrite the committed viewport and that truthful coverage, selection, and navigation survive every degradation profile.

Degradation order is semantic, not merely graphical:

1. pause hidden layouts/live animation;
2. reduce labels while preserving selection/consequential marks;
3. request higher server aggregation/LOD;
4. switch graph to matrix/outline or timeline to density/table;
5. disable decorative transitions;
6. retain last-known-good evidence with explicit partial/stale state.

Never drop prompts, errors, file mutations, policy/privacy events, or coverage markers merely to meet FPS.

## 20. Complete V1 behavior and action parity

Generate `dashboard/tests/fixtures/v1-surface-inventory.json` from manifests/routes/components/tests. Each row records V1 route/tab/view, filters, URL state, keyboard/touch path, loading/empty/error states, read models, actions/mutations, capability gates, V2 owner, parity test, migration-only path, current binding, and retirement status.

| V1 surface | V2 owner | Exact parity gate before migration switch/removal |
|---|---|---|
| Shell project selector + six plugin tabs | Workbench shell | project and All scope, capability states, deep links, back/forward, no lost action, migration-only `?tab=` mapping and post-cutover typed stale-path failure |
| Holographic Inspector | Knowledge + inspector | fact/entity/bank list, search/tags, content, trust components/history, retrieval stats, HRR coverage, categories, growth, provenance |
| Holographic Semantic Map | Knowledge similarity | PCA/projection, category/filter, hover/focus/select, trust/content preview, exact score/table fallback |
| Holographic Association Graph | Memory lens | fact/category/entity/bank nodes, contains/mentions/bundles relations, bounded expansion, evidence/table fallback |
| Holographic Similarity | Knowledge comparison | threshold, pair limit, duplicate/merge/related classes, cosine/lexical overlap/shared tokens, curation handoff |
| Holographic Curation status/activity/history | Knowledge + Automations | scheduler state, pause/resume/run-now, effective autonomy config edit, run/artifact drill-down, candidates/decisions/effects/recovery, oplog, snapshots, activity |
| Curation fact apply | Autonomous curation history | preserve V1 behavior evidence, but V2 exposes no manual apply; show automatic delete/merge/rewrite validation, winner/loser evidence, descendant outcomes, recovery/audit, pin/protect/exclude controls |
| Managed skills | Automations/Evolution | candidate/validation/autonomy-decision/materialization/recovery inspect, pause/protect/exclude/config, artifact/evidence/version/use/outcome; no approve/install item action |
| LCM overview/recent | Sessions + Observatory | messages/sessions/summaries, roles/sources/depth, compression, recent lists, storage scope/path/health |
| LCM search | Explorer/Sessions | internal FTS/LIKE parity receipt, current evaluated hybrid search, origin/source/session/time filters, sanitized-native/summary provenance, pagination/export plus #410 modes/counts |
| LCM session detail | Sessions/Loom | complete messages, order/limit/offset, summary nodes, tokens/metadata, native/representative/audience modes |
| LCM node detail | Sessions/inspector | node metadata, depth/category/compression, message/child-node source expansion, complete reconstruction of retained sanitized structure |
| LCM timeline | Loom | day/hour/session filters and counts plus richer lanes/coverage |
| LCM compression | Sessions/Labs | overall/session/node source/summary token ratios and counts, preview/compress/boundary/status/doctor semantics |
| LCM payload health/GC | Observatory | externalized bytes/counts, reclaimable/orphans/missing/unresolved payload references/tombstones/last outcome, operation-specific plan/start/status/audit |
| Code Graph overview | Code | kind/language/connected/largest-file/edge charts, click-through filters/focus, exact tables |
| Code Graph Canvas | Code/graphs | search, seedless default, kind/language/directory filters, focus/select, progressive neighbors, callers/callees, path mode |
| Savings overview/ledger | Costs | range, net/lifetime totals, recording gate, per-day/tool/project, methodology and confidence labels |
| Savings sessions/models/pricing | Costs | pagination, expand model rows, cost basis blocks, actual/tokenized/estimated/mixed, tokenizer exactness, unknown model, OpenRouter/cache/fallback/offline freshness |
| Code Diagnostics | Observatory + Code | overview, language settings, idle backfill, refresh all/language, diagnostic→symbol/test mapping, capability/error states |
| Settings | Settings | project include/exclude/max size/docstrings/calls/gitignore; user upload/debounce/timeout; source/default/env/storage/version; validation; resync/restart recommendation |
| Automation jobs/scheduler | Automations | CRUD/run/pause/resume, due/skip/lock/lease, effective config/source, run ledger, audit |
| Analytics hints/usage/underused | Observatory/Costs/Hint Lab | exact counts, denominators, sample/caps, policy version, unresolved horizon, emitted/adopted/missed/correction/terminal evidence |
| Hermes wrapper | Unified app host compatibility | capability proxy, base path, CSP, shared React, auth, direct route reload, no duplicated stores/profile after #407 |

Every V1 write has an explicit V2 command parity test before the migration switch. If the target intentionally changes dangerous behavior—such as V1 hard deletion—the inventory records the approved semantic change, migration/rollback, and user-visible warning rather than claiming byte parity.

## 21. TDD implementation and PR sequence

Each numbered task is independently reviewable. A task starts by writing failing tests against fixtures/contracts, implements the minimum complete slice, verifies focused and full frontend gates, updates inventories/visual briefs, and commits. Do not combine domain workspaces or labs into one hairball PR.

The phase-4 PR letters in this section are the authoritative sub-split ledger for dashboard work: the master plan tracks the top-level PR numbers (4A, 24–32, 35–37) and global release gates, and defers letter-level dashboard splits to this plan. Where a master letter and a letter here disagree, this ledger is the tracking truth for dashboard sub-PRs.

### Task 1: PR 4A — V1-backed read-only concept workbench

**Files:**
- Create: `dashboard/design/concepts/*`
- Create: `dashboard/design/extraction-ledger.md`
- Create: `dashboard/design/fidelity-ledger.md`
- Create: `dashboard/app/src/experimental/BrainConcept.tsx`
- Create: `dashboard/app/src/experimental/brain-v1-adapter.ts`
- Test: `dashboard/tests/visual/brain-concept.spec.ts`
- Test: `dashboard/tests/accessibility/brain-concept.spec.ts`

- [ ] Generate all three complete section-2.1 directions from the identical frozen corpus; run the fixed user storyboards, record principal-user critique, select one, and reject until hierarchy, identity, density, graph/timeline/lab anatomy, mobile continuation, state semantics, and light/dark behavior are legible.
- [ ] Record exact tokens, typography, component/container families, icons/marks, allowed copy, viewport composition, responsive continuation, and motion in `extraction-ledger.md`.
- [ ] Write Playwright tests that expect the Brain claim, central bounded topology driven by real V1 aggregate data, activity rail, health strip, inspector selection, keyboard outline, and mobile sheets. Expected before implementation: route/locators fail.
- [ ] Implement a feature-flagged, read-only workbench using existing V1 APIs with explicit unavailable/partial joins; it must not pretend project-scoped APIs are All data.
- [ ] Compare browser screenshots to concepts with `view_image`, fix every material mismatch per the section 18 threshold list, and record the fidelity ledger.
- [ ] Run `cd dashboard && npm test && npm run build && npx playwright test tests/visual/brain-concept.spec.ts tests/accessibility/brain-concept.spec.ts`. Expected: pass.
- [ ] Commit: `docs(ui): lock Brain workbench product contract` for design/reference artifacts and `feat(ui): prototype V1-backed Brain workbench` for the guarded prototype.

### Task 2: PR 25A — Generated client consumption, build-boundary ADR, and application foundation

**Files:**
- Create: `docs/adr/dashboard-v2-build-boundary.md`
- Create: package tree under `dashboard/packages/{api-client,data-client,query-state,design-system,testing}/` plus `dashboard/app/src/{contracts,features,shared}/`
- Create: `dashboard/app/src/{main,app,router,routes,providers,error-boundary}.tsx`
- Modify: `dashboard/{package.json,package-lock.json,build.mjs,tsconfig.json}`
- Modify: existing `dashboard/build.shared.mjs` programmatic Rsbuild/Rspack integration as needed, `build.rs`, `src/dashboard/assets.rs`, `src/dashboard/mod.rs`, `Cargo.toml`; no bundler migration/config authority is introduced
- Test: `dashboard/tests/contract/{generated-drift,asset-manifest,history-fallback}.test.ts`
- Test: `tests/dashboard_api_test/api.rs`

- [ ] Land the current Rsbuild/Rspack build-boundary, determinism, embedding, and rollback record as the first reviewed commit in PR 25A. State that it documents the existing `package.json → build.mjs/build.shared.mjs → dist → build.rs/assets.rs` authority and makes no bundler selection. Do not infer a migration, Vite authority, external-repository dependency, or benchmark requirement from historical cross-project scenarios. PR 24D remains exclusively the API-owned deterministic OpenAPI/generated-TypeScript-client slice consumed here.
- [ ] Write failing generated-client drift, content-hashed asset manifest, CSP, base-path, lazy-chunk, history-fallback, `/api` non-fallback, two-clean-build determinism, and packaged-asset tests.
- [ ] Run `cargo run -p tracedecay --bin generate-openapi -- --check`, then the root client workspace's own generate/test commands for `packages/tracedecay-client` (that workspace's toolchain, per the section 5 package-manager rule — the dashboard itself remains npm-only), and the dashboard browser-binding tests; expose only official typed HTTP/problem/SSE methods plus UI-safe contract aliases.
- [ ] Implement one React root, router, providers, route-lazy error boundary, existing Rsbuild/Rspack build integration, asset manifest, and Axum history fallback.
- [ ] Preserve old shell/plugins only under the migration feature flag while parity work is active; direct old/new URLs work in that mode, and a cutover fixture proves old live routes/names stop resolving afterward.
- [ ] Run `cd dashboard && npm ci && npm test && npm run build`; run `cargo test --test dashboard_api_test`; run `cargo package --allow-dirty --no-verify` followed by the repository package verification command. Expected: pass and no second-build diff.
- [ ] Commit separately inside PR 25A: `docs(adr): lock dashboard V2 build boundary` first, then `build(dashboard): establish generated V2 application shell`; do not reuse PR 24D.

### Task 3: PR 25B — Investigation state, shell, persistence, and design system

**Files:**
- Create: `dashboard/packages/query-state/src/*`
- Create: `dashboard/packages/design-system/src/*`
- Create: `dashboard/app/src/shell/*`
- Create: `dashboard/app/src/migration-paths.ts`
- Test: `dashboard/tests/component/{investigation-state,command-bar,inspector-dock,mobile-sheets}.vitest.tsx`
- Test: `dashboard/tests/e2e/{url-history,saved-state,migration-paths}.spec.ts`

- [ ] Write failing explicit-All default, no-cwd/last-project narrowing, repository/project/worktree/ref canonical URL, same-name disambiguated candidates, one-step retry preserving request, CLI/MCP/API parity, protected literal exclusion, back/forward, per-slot visualization/panel persistence, route selection preservation, stable-anchor/recipe recovery after cursor/handle expiry, six-group switcher pinned/recent navigation, Observe→Configure context preservation, theme/density, focus restoration, mobile sheet, migration-only legacy path, and post-cutover stale-path failure tests.
- [ ] Implement `InvestigationStateV1`, versioned codecs/store/history, protected drafts, local preferences, and IndexedDB ownership exactly as section 4.
- [ ] Implement accepted tokens/type/icons/controls/open-layout shell and all state primitives without feature data.
- [ ] Implement scope default All, time/live/as-of/compare, query opener, health, save/export, persistent six-group workspace switcher, pinned/recent destinations, command palette frame, outline/inspector/time brush docks, status line, and mobile sheets.
- [ ] Run `cd dashboard && npm test && npx playwright test tests/e2e/url-history.spec.ts tests/e2e/saved-state.spec.ts tests/e2e/migration-paths.spec.ts`. Expected: pass with zero sensitive literals in URL/history fixtures and typed stale-path failure after cutover.
- [ ] Commit: `feat(dashboard): add shared investigation workbench`.

### Task 4: PR 25C — Universal inspector, cache, SSE, and capability commands

**Files:**
- Create: `dashboard/app/src/shared/inspector/*`
- Complete: `dashboard/packages/data-client/src/*`
- Test: `dashboard/tests/component/{universal-inspector,coverage-status,command-preview}.vitest.tsx`
- Test: `dashboard/tests/e2e/{sse-reconnect,partial-offline,optimistic-command}.spec.ts`

- [ ] Write failing universal/generated inspector panel, illegal-panel migration, aggregate membership, relation evidence, native/normalized/history, capability action, destructive preview, optimistic conflict, same-query/different-snapshot cache separation, route-preset/generic-operation cache identity, and SSE state-machine tests.
- [ ] Implement canonical operation/dataset query keys with access/schema/catalog/query/scope/time/snapshot/representation/cursor identity, cache bounds/abort, protected offline cache, subscription creation, idempotent delta reducer, coverage deltas, gap/resync, reconnect/backoff, schema/access invalidation, operation-terminal events, and `/operations/{id}` polling recovery after stream loss. Delete route/feature-specific cache adapters.
- [ ] Implement the six universal panels plus catalog-generated entity/capability descriptors and complete/loading/stale/partial/offline/locked/redacted/incompatible/error states.
- [ ] Verify fake SSE duplicates/out-of-order/gaps without sleeps and profile lock clears protected state.
- [ ] Run `cd dashboard && npm test && npx playwright test tests/e2e/sse-reconnect.spec.ts tests/e2e/partial-offline.spec.ts tests/e2e/optimistic-command.spec.ts`. Expected: pass.
- [ ] Commit: `feat(dashboard): connect evidence inspector and live snapshots`.

### Task 5: PR 26A — Shared slot, renderer capabilities, LOD, export, and worker foundation

**Files:**
- Create: `dashboard/app/src/shared/renderers/*`
- Create: `dashboard/app/src/shared/{renderers,charts,code-viewer}/**/*`
- Create: `dashboard/app/src/features/{brain,causal-loom}/visualization/**/*`
- Test: `dashboard/tests/component/{renderer-registry,layout-cache,selection-adapter,accessible-chart}.vitest.tsx`
- Test: `dashboard/tests/e2e/{renderer-context-loss,export-scene}.spec.ts`
- Benchmark: `dashboard/tests/performance/{graph,timeline,main-thread}.spec.ts`

- [ ] Write failing stable layout, deterministic worker, expansion position, independently restorable per-slot viewport/scale/lanes/LOD/playhead/synchronization, selection/camera adapter, hidden-route suspension, reduced motion/data, table fallback, WebGL loss, render-ready, and export-manifest tests.
- [ ] Implement the bakeoff winner behind the renderer registry and thin slot frame, plus typed viewport/interaction/accessibility/fallback/export adapters, ELK worker, dense Canvas, adaptive matrix, relationship table, chart theme/accessibility, and CodeMirror payload-slice primitives; delete losing graph prototypes and dependencies.
- [ ] Implement deterministic export scene with fixed fonts/DPR/layout and fallback.
- [ ] Run unit/E2E/performance tests. Expected: stable hashes across two runs, nonblank exports, fallback retains selection, initial route does not load renderer chunks.
- [ ] Run the section 15.1 renderer bakeoff on current and 10× corpora, record the decision and deletion receipt for losing prototypes/dependencies, then implement one thin `WorkspaceSlotFrame`, renderer registry, typed capability adapters, visual-semantic ontology, linked scale/brush/focus/legend/export machinery, atlas tile client, and accessibility adapter. No switch-heavy universal renderer or feature-local selection store, chart wrapper, worker protocol, legend, or exporter survives review.
- [ ] Commit: `feat(dashboard): add bounded visualization foundation`.

### Task 6: PR 26B — Observatory and non-topology Brain slice

**Files:**
- Create: `dashboard/app/src/features/observatory/src/*`
- Create: `dashboard/app/src/features/brain/src/{BrainPage,FirstScanClaim,HealthStrip,LearningLoop,ResumeWork}.tsx`
- Create: `dashboard/app/src/features/{observatory,brain}/visual-brief.md`
- Test: `dashboard/tests/e2e/{observatory,brain-summary}.spec.ts`

- [ ] Write failing tests for first-scan suppression under partial coverage, federated All/repository/project/worktree/ref scope and per-shard provenance, same-name disambiguation, Work/initiative/plan/task/attempt/blocker/lease/acceptance first-scan clusters, unfinished-work Resume ordering, project × subsystem health drill-down, producer exact/range/exclusion/runtime-set/compatible-protocol/legacy-unknown log filters with truthful excluded counts plus segment/quota/retention/hold state, foreign-owner doctor severity/actions, partial/degraded provider branding, daemon drain/update recovery receipts, hint/tool outcome denominators, storage/privacy/ingest states, complete current generated Capability Registry/guided action, and learning loop.
- [ ] Implement matrix/table/aggregate charts before topology, using exact server read models and inspector pivots.
- [ ] Verify mobile reading order, table parity, offline snapshot, locked store, direct labels, and no equal-weight card grid.
- [ ] Run feature, accessibility, visual, and data-invariant tests. Expected: pass.
- [ ] Commit: `feat(dashboard): ship profile-wide Observatory and Brain summary`.

### Task 7: PR 27 — Universal Explorer

**Files:**
- Create: `dashboard/app/src/features/explorer/src/{ExplorerPage,IntentInput,QueryBuilder,TraceQueryEditor,SearchStagePanel,ResultTable,PivotSwitcher,ExplainPanel,QualityStatus,CollectionPanel,ComparePanel}.tsx`
- Create: `dashboard/app/src/features/explorer/visual-brief.md`
- Test: `dashboard/tests/e2e/{explorer,query-explain,collections-compare}.spec.ts`

- [ ] Write failing plain-language→visible-AST, builder/raw round-trip, All/repository/project/worktree/ref scope, same-name candidate retry, lexical/phrase/fuzzy/entity/semantic/graph/recency stage, origin/kind filter, complete memory/skill/automation kind presets, grouping/dedupe/native expansion, validation/cost, candidate cap, pagination/cursor, ranking explanation, active-profile/latest-qualified-report/regression warning plus exact Search Quality Lab deep link, frozen historical Rspack/Rsbuild/React Router cross-repo retrieval fixture, pivot, selection, stable recipe, collection, compare, export, and exact CLI/MCP/API request/result parity tests. Explorer has no external benchmark execution path, checkout dependency, or dashboard-build implication.
- [ ] Implement the three query authoring modes and pivots without client joins or SQL syntax.
- [ ] Add transcript mode/origin facets and hidden-copy counts; prove every sanitized native row remains reachable.
- [ ] Verify partial shards, unknown denominator, explicit candidate/ranking caps, ambiguous message-origin view, stable cursor plus cursor-independent research recipe, privacy-boundary graph frontier, mobile builder, keyboard results, and table/export parity.
- [ ] Run focused/full frontend tests and fixed-corpus user task “find exact historical direct-user prompt, survive typo/role ambiguity, expand its copied/delegated/native set, and prove stable source identity after cursor expiry <=30 seconds.” Expected: pass; embeddings-on must beat or tie embeddings-off within every declared promotion threshold or remain disabled.
- [ ] Commit: `feat(dashboard): add universal evidence explorer`.

### Task 8: PR 28A/28B — Causal Loom density, lanes, transcript, and inspector

**Files:**
- Complete: `dashboard/app/src/features/causal-loom/timeline/*`
- Create: `dashboard/app/src/features/causal-loom/src/{CausalLoomPage,lane-model,turn-selection}.ts(x)`
- Create: `dashboard/app/src/features/causal-loom/visual-brief.md`
- Test: `dashboard/tests/e2e/{loom-density,loom-turn,loom-transcript-modes}.spec.ts`

- [ ] Write failing density exact/sample/hidden/late counts, bounded refinement, stable lanes, consequential event, transcript mode, Turn evidence, virtualized code/diff, and occurred/ingested tests.
- [ ] Implement density brush and lane LOD, then event waterfall and Turn/transcript inspector using frozen snapshot semantics.
- [ ] Ensure routine aggregation never removes counts/export and frozen late events never silently reorder.
- [ ] Verify table/transcript fallback, mobile single lane, keyboard consequential-event traversal, and reduced motion.
- [ ] Run feature/visual/accessibility/performance suites. Expected: pass at 250k density marks.
- [ ] Commit: `feat(dashboard): render bounded causal event lanes`.

### Task 9: PR 28C/28D/28E — Agent follow, causal evidence, impact, as-of, compare, annotation, export

**Files:**
- Create: `dashboard/app/src/features/causal-loom/src/{AgentFollow,DelegationTree,CausalChain,ImpactRibbon,AsOfPanel,CompareLoom,AnnotationRange}.tsx`
- Test: `dashboard/tests/e2e/{loom-follow,loom-impact-asof,loom-compare-export}.spec.ts`

- [ ] Write failing parent/subagent/handoff, evidence-class connector, touched-versus-affected, time-machine fidelity, aligned comparison, range annotation, deep-link, and deterministic export tests.
- [ ] Implement one sub-PR per capability group; keep lane order and investigation state stable across them.
- [ ] Prove temporal proximity has no causal arrow; unavailable reasoning/tool catalog/policy/input is explicit.
- [ ] Run fixed-corpus user task “follow parent through subagents and code/test/commit/PR impact <=60 seconds.” Expected: pass.
- [ ] Commit each sub-PR: `feat(timeline): add agent follow and evidence chains`; `feat(timeline): add impact and as-of state`; `feat(timeline): add compare and deterministic export`.

### Task 10: PR 29 — Brain topology and seven shared graph route presets

**Files:**
- Complete: `dashboard/app/src/features/brain/visualization/*`
- Create: `dashboard/app/src/features/brain/src/BrainTopology.tsx`
- Create: generated route-preset descriptors under `dashboard/app/src/routes/presets/{git,code,threads,agents,turns,memory,automation}.ts`; extend shared Brain/Explorer renderer specs only
- Test: `dashboard/tests/e2e/{brain-semantic-zoom,graph-lenses,git-drift}.spec.ts`

- [ ] Write failing tile truth-contract, semantic zoom, stable expansion, federated multi-repo/worktree/ref scope, Work/plan/task/attempt/blocker/lease/acceptance cluster membership and cross-domain pivots, same-name node separation, per-shard stale/partial provenance, lens switch, legal edge vocabulary, cross-lens selection, fallback, dense LOD, and Git local/live drift tests.
- [ ] Implement Brain topology only after PR 26 contracts pass; register seven base generated graph presets and their mini-briefs (Task 10A adds `tasks`/`plans`, completing the nine-slug enum), profile-atlas tile pyramid/hysteresis/object-constancy contract, bounded overlay/bridge composition, and adaptive dense-community matrix. No `features/graphs` package or timeline graph destination exists.
- [ ] Add generated Git tool actions and explicit semantic/live evidence requirements to Git inspector/palette.
- [ ] Verify no hairball, aggregate versus evidence edge bundling, mobile focused neighborhood, 50k/200k benchmark, and table/matrix equality.
- [ ] Commit: `feat(dashboard): connect the graph-of-graphs Brain`.

### Task 10A: PR 25G/30K — Canonical Work workspace and advanced task lenses

**Files:**
- Create: `dashboard/app/src/features/work/src/**/*`
- Create: generated route-preset descriptors under `dashboard/app/src/routes/presets/{tasks,plans}.ts`
- Extend: `dashboard/app/src/features/{brain,work,causal-loom}/src/*`
- Test: `dashboard/tests/e2e/{work-initiative-plan,work-kanban-dag,work-attempt-packet,work-executor-critical-path,work-worktree-lifecycle,work-markdown-edit,work-notifications}.spec.ts`

- [ ] Write failing one-canonical-ID/count/selection tests across initiative outline, saved Kanban, dependency DAG, critical path, timeline, causal, workload, executor-fleet, repository-work, initiative, agent-relevant, and All projections.
- [ ] Add generated `tasks` and `plans` route-preset rows, completing the nine-value `GraphLensV1` enum; extend the section 4 lens-slug/enum/composition fixture and prove `/graphs/tasks` and `/graphs/plans` plus legal overlays round-trip through URL state.
- [ ] Implement initiative/plan/task/attempt routes and inspectors from generated views/legal capabilities, using section 4's catalog-owned Work panel descriptors for plan 24 §12.6 content; board/query/layout state never becomes task or dispatch authority.
- [ ] Implement section 13.0A in existing Work/Repository Work/Delivery compositions: every task projection shows related repository/worktree/branch/PR and active agent/attempt summaries; task detail and universal worktree inspector expose creator/source provenance, inferred/confirmed/ambiguous relation evidence, cleanup delegation, blockers, retention, and receipts; canonical deltas update hook/tool/CWD/Git-watcher discoveries; one deduped material ambiguity hint and generated relation-resolution actions never imply adoption or cleanup authority. Do not add a route, inventory, watcher, worktree-creation/provisioning control, filesystem/DB/Git browser path, or client-side eligibility join.
- [ ] Add the workspace-lifecycle overlay, stale/orphan-candidate filters, and virtualized cleanup Triage queue with exact table/mobile/keyboard parity. Gate physical cleanup behind the distinct generated daemon `legal_capabilities` action and plan-09 confirmation/operation receipt; prove `work_items.archive` and PR merge only update daemon eligibility evidence, and prove plan 21's `task-graph edit clean` remains edit-bundle-only. If the authoritative catalog lacks the physical cleanup capability, ship truthful read-only eligibility/blocker UX and fail the action-parity gate rather than inventing an API.
- [ ] Implement the PR 24R **Edit as Markdown** consumer at `/work/edit-bundles/:id`: explicit selection/export, small embedded and large streamed bundle modes, signed manifest tree, source-span diagnostics, semantic graph/active-attempt diff, three-way conflict/rebase, atomic-submit confirmation, canonical-ID receipt, TTL/delete/crash/success cleanup state, and CLI/SDK handoff. Browser state stores only opaque IDs and safe summaries; HTTP never transports a server path.
- [ ] Consume reviewed plan-13 PR 2A Hermes UI ledger rows, retaining required notices and source-to-test links for direct/behavioral ports; prove a redesigned interaction passes the named upstream regression before dropping the port candidate.
- [ ] Implement `/work/notifications` (plan 24 §12.7): saved filters/channels with event classes, quiet hours, dedupe, and rate budgets via generated direct validated `task_notifications.create/update/delete` commands; prove task creation never auto-subscribes a channel.
- [ ] Verify the frozen historical Rspack/Rsbuild/React Router multi-repository scope scenario, one transactional `assign_set` Codex/Claude route partition with all-or-none per-item receipt, fan-out/fan-in/shared-work child gates, attempt list/detail/timeline with immutable start/current accepted packet, registration-scoped offer list/get/accept/decline plus authorized revoke, fenced packet accept at a safe Turn, fully anchored packet entries, typed attestation/review/decision/exception/handoff actions, versioned reopen and exact reverse-transition with no generic undo, advisory-claim-versus-authoritative-lease distinction, query-scope overlap suppression, complete saved-view reopen/share/revoke, workspace/ref/snapshot, requested/actual route, brokered/non-preemptible effects, stale-fence/cancellation status, direct notification subscriptions, task-graph status/doctor, worktree association/lifecycle/cleanup receipts, complete edit-bundle operation/diagnostic/diff/conflict/cleanup parity, and canonical subscription deltas with no `/task-events` stream. The fixture supplies scenario data only and imposes no TraceDecay build dependency.
- [ ] Replay a 100,000-item sharded fixture plus stale-base, partial-upload, explicit-retire, unsafe-archive, secret, concurrent-submit, validation-fix loop, process-death, expiry, and immediate-success-purge cases. Expected: responsive virtualized UI/table parity, precise accessible diagnostics, no omission-driven deletion, no partial canonical write, no leaked path/content, and terminal cleanup truth matches CLI/API/MCP/SDK receipts.
- [ ] Gate Workload/Executor Fleet implementation on plan 26 PRs 22H/30J and verify cost/runtime/rate/denominator/unknown values and drill-down refs match generated application/CLI/MCP/API/SDK fixtures; no client aggregation.
- [ ] Prove drag/drop maps to a legal versioned command, blocked work cannot be dragged ready, large graphs aggregate server-side, and every visual has table/mobile/keyboard/export parity.
- [ ] Commit separately: `feat(dashboard): add canonical work and plan views`; `feat(dashboard): visualize work across the TraceDecay brain`.

### Task 11: PR 30A/30B/30B2 — Sessions, Agents, and Coordination workspaces

**Files:**
- Create: `dashboard/app/src/features/sessions/src/*`
- Create: `dashboard/app/src/features/agents/src/*`
- Create: `dashboard/app/src/features/coordination/src/*`
- Test: `dashboard/tests/e2e/{sessions,session-raw-canonical,agents,coordination}.spec.ts`

- [ ] Generate failing parity tests for every LCM row in section 20 and every #410 transcript mode/count/provenance behavior.
- [ ] Implement complete session list/detail, Turn graph/outline, source observation/native-row/representative tabs, summary lineage, compression/cost, workflow/goal and code/delivery links. Summary DAG nodes include `[S1]` marker chips, source/claim anchor coverage and omissions, requested/actual Terra-or-fallback route, manifest/run receipts, transitive stale/successor state, and explicit authorized source open.
- [ ] Implement actor/instance topology, agent tree/Turns/delegation/handoff/tools/outcomes/compare plus the first-class goal detail route with native Codex plan/status updates, owning agent/session/workflow, linked Turns, and terminal evidence.
- [ ] Implement expiring presence claims, same/parallel-worktree proximity, direct-versus-temporal overlap, safe summaries, stable anchors/recipes, inspect/message/handoff/ack/suppress previews/receipts, one deduped non-spam hint, analytics, table parity, and Coordination Lab deep links.
- [ ] Verify Claude workflow and Codex goal semantics remain labeled and sanitized native copied-subagent rows remain expandable.
- [ ] Switch LCM reads to current V2 routes only after parity; keep old routes migration-only until compression/payload V2 commands pass, then remove stale live names rather than redirecting/falling back.
- [ ] Commit separate PRs: `feat(dashboard): add Sessions workspace`; `feat(dashboard): add Agents workspace`; `feat(dashboard): add agent coordination workspace`.

### Task 12: PR 30C/30D — Code and Delivery workspaces

**Files:**
- Create: `dashboard/app/src/features/code/src/*`
- Create: `dashboard/app/src/features/delivery/src/*`
- Test: `dashboard/tests/e2e/{code-workspace,code-diff-impact,delivery-git-reconciliation}.spec.ts`

- [ ] Generate failing Code Graph/Diagnostics parity tests plus snapshot/symbol-lineage/diff/impact/affected-test cases.
- [ ] Implement code views using the code lens, matrix, charts, CodeMirror, exact source locations, and observed/inferred ownership.
- [ ] Implement worktree/ref/commit/PR/check/review/release views with produced/observed/encountered states and local/live reconciliation.
- [ ] Verify drift blocks joined claims and command palette routes Git intent to generated TraceDecay tools.
- [ ] Redirect graph/diagnostic views independently after parity.
- [ ] Commit separate PRs: `feat(dashboard): add Code workspace`; `feat(dashboard): add Delivery workspace`.

### Task 13: PR 30E/30F/30G — Knowledge, Automations/Evolution, and Costs

**Files:**
- Create: `dashboard/app/src/features/knowledge/src/*`
- Create: `dashboard/app/src/features/{automations,evolution}/src/*`
- Create: `dashboard/app/src/features/costs/src/*`
- Test: `dashboard/tests/e2e/{knowledge,automation-skill-lifecycle,evolution,costs}.spec.ts`

- [ ] Generate failing Holographic, Curation, Automation, managed-skills, analytics, and Savings parity tests from section 20, including a complete cursor-paged All-memory/All-skill inventory, profile/project declared-scope ownership, cross-project reuse without copied durable state, source-transcript/run traversal, typed trigger/input-contract and per-scope dirty/admission navigation, current-versus-considered-versus-consumed frontier drill-down, semantic-input-versus-evaluation-snapshot digests, active-writer/coverage deferral, grouped/coalesced skip episodes, quiet countdown, identical-input work avoided, generic retry/circuit/quarantine/nonterminal-effect-reconciliation state, and stalled/starved-dirty health.
- [ ] Implement Knowledge fact/version/entity/relation/provenance/trust/retrieval/similarity/autonomous-curation flows with complete table/graph/timeline navigation, decision/effect/outcome/recovery history, and config/pause/pin/protect/exclude controls, not item commands.
- [ ] Implement complete schedules/runs/actors/artifacts/candidates/skills inventory/detail plus Evolution autonomy lineage/version/use/outcome/replay views; every source session, produced memory/skill version, later use, and outcome remains traversable through stable anchors.
- [ ] Implement Costs exact tier/methodology/pricing/recording/offline/unknown-model behavior and linked drill-down.
- [ ] Switch each domain to its current V2 route only after read/write parity and rollback drill; remove the old live binding atomically.
- [ ] Commit three reviewable PRs with feature-specific titles.

### Task 13A: PR 25F/30L — Privacy workspace and Context Scout Observatory

**Files:**
- Create: `dashboard/app/src/features/privacy/src/*`
- Create: `dashboard/app/src/features/hints/src/*`
- Create: `dashboard/app/src/features/observatory/src/ContextScoutPage.tsx`
- Create: `dashboard/app/src/features/{privacy,hints}/visual-brief.md`
- Test: `dashboard/tests/e2e/{privacy-observatory,context-scout}.spec.ts`

- [ ] Write failing `/privacy` tests: `PrivacyProtectionStatusV1` rendering, source × sink × privacy-domain coverage matrix, unknown/locked/corrupt/skipped coverage blocking a clean claim, finding rows exposing only opaque ID/class/confidence/state (a fixture with candidate/substring/span/fingerprint fields must fail to render), elevated-auth scan/remediation previews, restore blocked until isolated scan/rebuild/promotion receipts, and each named current-gap regression row from section 13.9.
- [ ] Write failing `/observatory/context-scout` tests: trigger/silence/envelope/delivery/outcome funnel with denominators and horizon, queue/model/tool/host state, suppression/dedupe/cooldown evidence, and deep links to Hint Lab replay and `/settings/context-scout`.
- [ ] Implement `features/privacy` per section 13.9 and plan 18 §14.3 semantics, and `features/hints` plus the Observatory scout page per plan 22's Observatory controls, all from generated read models and commands.
- [ ] Verify direct reload/back-forward, mobile sheets, table parity, keyboard/screen-reader paths, and locked/offline/partial states on both routes.
- [ ] Run `cd dashboard && npm test && npx playwright test tests/e2e/privacy-observatory.spec.ts tests/e2e/context-scout.spec.ts`. Expected: pass.
- [ ] Commit separate PRs: `feat(dashboard): add privacy observatory workspace`; `feat(dashboard): add context scout observatory`.

### Task 14: PR 25D/30H — Activity preset and saved views

**Files:**
- Create: `dashboard/app/src/routes/presets/activity.ts`; reuse shared Explorer/Loom query, timeline, health, and inspector components
- Create: `dashboard/app/src/features/saved-views/src/*`
- Test: `dashboard/tests/e2e/{activity,saved-views}.spec.ts`

- [ ] Write failing activity live/frozen/filter/coverage, generated activity-model parity, all three `SavedViewDefinitionV1::{Investigation,Task,Experiment}` variants plus `CollectionV1`/`AnnotationV1` complete round-trip and size-envelope rejection (section 4.3), proof that no task/experiment-view table/ID/share command fork exists, simultaneous overlapping views, exact experiment/run/cell/stage/comparison/comparison-cell/reduction/playhead restore, exact frozen-input unavailable state, saved protected-query classification/redaction/share-plan/start/revoke/expiry, URL restore, declared-owner conflict, and #425 identity-split operation-link fixtures.
- [ ] Implement Activity as a registered `triage` composition preset over shared Explorer/Loom query, timeline, health, inspector, and table components with consequential-event priority, project/domain facets, bounded live paging, and no duplicate hidden counts. It owns no feature package, data model, cache adapter, or renderer.
- [ ] Implement saved-view create/update/open/delete plus generated `saved_views.share.plan`, `saved_views.share.start`, and `saved_views.share.revoke` commands; protected query literals/annotations remain encrypted, published views expire locally, and sharing requires classification/redaction planning plus explicit confirmation. Generated binding-ID parity tests cover every CLI/MCP/HTTP/SDK/UI spelling.
- [ ] Consume Settings links only through plan 20 PR 25E's generated workspace routes and typed operation descriptors; PR 25D/30H owns no setting form, registry behavior, direct configuration command, or Settings cutover.
- [ ] Verify `/activity` and `/saved/:viewId` direct reload/back/forward/mobile/offline/locked behavior.
- [ ] Prove `/activity` and `/saved/:viewId` reuse canonical query/cache/page/inspector models, legacy `/work/views/:id` redirects only during migration, and no `features/activity` or task-specific saved-view store exists.
- [ ] Commit independently: `feat(dashboard): add cross-domain activity`; `feat(dashboard): add protected saved investigations`.

Plan 20 PR 25E exclusively creates `features/settings`, its E2E suite, complete generated forms, effective-source behavior, direct validated commands, activation/ack/drift views, and V1 Settings cutover. This frontend plan supplies the shared shell, visualization language, navigation, accessibility, and route-composition requirements that PR 25E consumes; it does not assign Settings implementation to PR 25D or 30H.

### Task 14A: PR 25H — Generated Integrations topology and operations workspace

**Files:**
- Create: `dashboard/app/src/features/settings/src/integrations/*`
- Create: `dashboard/app/src/features/settings/src/integrations/visual-brief.md`
- Test: `dashboard/tests/e2e/integrations.spec.ts`

- [ ] Write failing generated-client fixtures for multi-host/package/component topology; base plus zero/one/many companions; skills/roles/hooks/MCP enablement; logical registration/profile exposure; desired/documented/version-gated/absent/unknown/disabled/installed/observed/effective matrix states; version/digest drift; foreign/unknown ownership; trust; stale cache; credential-ref status; omissions/fallbacks; reconnect/reload/restart; and operation history.
- [ ] Implement `/settings/integrations` per §13.7A with stable layered topology, virtualized capability-difference matrix and equivalent table, target/inventory pivots, filters/compare, synchronized inspector, exact counts/coverage, deep links, and preserved URL selection.
- [ ] Bind desired package/component/install-scope/trust/update/credential settings only through plan 20's generated registry client. Prove save creates no host effect and cannot claim observed/effective state.
- [ ] Bind `Install|Update|Repair|Uninstall|Verify` only to generated plan-10 commands and the shared `OperationRef` monitor. Test admin denial, expected-version/idempotency conflict, foreign-owner refusal, uncertain-effect reconciliation, Verify-only fresh probing, and restart/reconnect completion.
- [ ] Add recursive fixtures containing host paths, config bodies, command lines, environment values, credentials, and arbitrary manifests; fail if any field reaches DOM, URL, telemetry, clipboard/export, error, or screenshot artifact. Assert the browser makes no host/filesystem/config probe outside the generated client.
- [ ] Verify keyboard/screen-reader matrix navigation, table parity, focus restoration under deltas, reduced motion, non-color encoding, 200% zoom, high contrast, both themes, mobile sheets, direct reload/back/forward, and every loading/empty/denied/locked/partial/stale/offline/unsupported/reconciliation/error state.
- [ ] Run `cd dashboard && npm test && npx playwright test tests/e2e/integrations.spec.ts`. Expected: pass with no accessibility, console, request-schema, or visual-regression failure.
- [ ] Commit `feat(dashboard): add integrations topology workspace`.

### Task 14B: PR 25I — Shared Brain Settings and Sync Observatory

- [ ] Add generated-client fixtures for standalone, remote authority, cached client, read replica, standby, hybrid placement, partition, revocation, schema/privacy skew, Git identity candidates, restore/promotion, and old-authority reappearance.
- [ ] Implement `/settings/brain` and `/observatory/sync` per §13.7 using the stable node/store/repository/sync topology plus complete accessible table, synchronized inspector, lag/spool/conflict/recovery charts, and exact coverage.
- [ ] Bind enrollment, placement, sync repair, repository adopt/split, replica retirement, restore, and failover only to plan 28/10 generated operations with legal-action grants, expected versions, idempotency, and receipts. UI never opens database paths or assumes Tailscale.
- [ ] Prove stale/offline/pending/fenced/policy-excluded states cannot look canonical or healthy; pass keyboard, screen-reader, mobile, theme, 200% zoom, deterministic export, synthetic secret, and topology comprehension gates.
- [ ] Commit `feat(dashboard): add shared Brain topology and sync observatory`.

### Task 15: PR 31A–31Q — Shared cockpit, evaluator labs, and owned extensions

The canonical mapping is explicit and is the only PR-to-lab truth:

| PR | Owner |
|---|---|
| 31A | Shared hermetic experiment cockpit, operation/run/cell/trace/comparison/branch/sweep/minimize/anchor/save/export lifecycle, and universal Fork to Playground; no evaluator ships before it. |
| 31B | Hint evaluator base. |
| 31C | Retrieval evaluator. |
| 31D | Ingest evaluator. |
| 31E | Query evaluator. |
| 31F | Correlation evaluator. |
| 31G | Scheduler evaluator. |
| 31H | Memory evaluator. |
| 31I | Policy Diff evaluator, including configuration precedence/effect mode. |
| 31J | Search Quality evaluator and qrel review (plan 15). |
| 31K | Coordination evaluator. |
| 31L | Scope/Federation evaluator, including configuration target-resolution mode. |
| 31M | Privacy & Secret Safety evaluator plus Privacy Observatory integration; synthetic canaries only. |
| 31N | Configuration/autonomy extensions to the existing Policy Diff and Scope/Federation evaluators; not a new lab. |
| 31O | Incremental Context Scout extension to the existing Hint evaluator (plan 22), not a new lab. |
| 31P | Temporal/session/LCM extension to the existing Search Quality evaluator (plan 23), not a new lab. |
| 31Q | Orchestration evaluator (plan 24). |

Evolution is the fourteenth evaluator and ships with its product workspace in Task 13/PR 30G over the same 31A cockpit. Thus there are exactly fourteen `LabKindV1` values, thirteen playground slugs plus named Evolution, one shared lifecycle, and no Configuration/Search/Scout extension masquerading as another lab.

**Files:**
- Complete: `dashboard/app/src/features/playgrounds/shared/*`
- Create: `dashboard/app/src/features/playgrounds/src/{HintLab,RetrievalLab,IngestLab,QueryLab,SearchQualityLab,ScopeFederationLab,CorrelationLab,CoordinationLab,OrchestrationLab,SchedulerLab,MemoryLab,PolicyDiffLab,PrivacyLab}.tsx`
- Test: `dashboard/tests/e2e/labs/*.spec.ts`

- [ ] In PR 31A first write shared failing tests for requested/actual fidelity, complete immutable manifest, universal Fork to Playground mapping, source backlink, missing input/substitutions, explicit run-cell coordinates, paged stage/comparison cells, sole branch ancestry, typed sweep values, cancellation/resume/retry/minimize, partial coverage, enforced resource budgets/zero-effect receipt, every anchor, save/share/annotation/playhead, redacted reproducibility export, retention holds, and separately authorized fixture promotion.
- [ ] Implement shared `LabWorkbench` over the generic experiment/operation lifecycle, synchronized cell-scoped `ReplayTraceV1`/paged `ReplayComparisonV1` playhead, schema-generated parameter controls, branch/sweep visualizations, and server-enforced hermetic worker runtime; no lab owns a scheduler, run table, comparison vocabulary, or side-effect guard.
- [ ] Implement one catalog evaluator per PR with the exact panels in section 14 over plan 10 §8.5's generic experiment/run API; Query Lab reuses Explorer AST/editor; Search Quality consumes generated corpus/qrel/pool/judgment/adjudication/report/profile reads and the exact direct publication/activation commands in §14; Scope/Federation reuses selector/resolution/shard-plan models; Correlation reuses Git reconciliation; Coordination has no messaging port; Orchestration has no scheduling/executor/effect port; Memory reuses Knowledge inspector; and Privacy ("Privacy & Secret Safety Lab") accepts synthetic canaries only. Run/cancel/resume/retry is the shared experiment operation, not a domain evaluator command.
- [ ] Run each lab twice and assert exact mode decision/explanation digest equality; recorded mode does not execute; best-effort lists every substitution.
- [ ] Run the no-documentation fixed-corpus task “select any Turn → Fork to Hint Lab → replay then/current → explain exact payload difference → branch one policy field → run a bounded sweep → minimize a failing case → save/share the anchored experiment” within the accepted comprehension/time budget. Expected: pass without a live-state write.
- [ ] Commit according to the table; extension PRs add evaluator modes/corpora only, and no omnibus or duplicate lab lifecycle merges.

### Task 16: PR 32 — Cross-product accessibility, responsive, export, and visual signoff

**Files:**
- Complete: `dashboard/tests/{visual,accessibility,performance}/**/*`
- Complete: `dashboard/design/fidelity-ledger.md`
- Modify: feature/package files only for audited defects

- [ ] Run automated axe (`@axe-core/playwright`, zero serious/critical violations per route) and manual keyboard/screen-reader/contrast/grayscale/color-deficiency/reduced-motion/table-parity audits for every route against a fixed per-route checklist derived from the section 16.1 requirements, on NVDA + Firefox (Windows) and VoiceOver + Safari (macOS and iOS); record pass/fail per checklist row in `dashboard/tests/accessibility/manual-audit.md` — an audit without a completed checklist does not count as passed.
- [ ] Run desktop/laptop/mobile portrait/mobile landscape/200% text zoom fixtures and every sheet/gesture/orientation/focus path in sections 16 and 18.
- [ ] Compare each route screenshot against its accepted concept with `view_image`; fix every reviewable mismatch and close the fidelity ledger.
- [ ] Run section 18's preregistered study with the principal user and at least six independent participants spanning agent-tool, newcomer, visualization, and assistive-technology roles; record incumbent comparison, time/interactions, errors, false-causality, partial-state comprehension, atlas recall, replay comprehension, abandonment, exclusions, and retests. Expected: every frozen release threshold passes.
- [ ] Run deterministic JSON/Markdown/SVG/PNG export twice and compare manifests/hashes; verify WebGL fallback and no hover-only data.
- [ ] Run performance budgets and fixed-corpus user tasks on local plus pinned remote high-latency/packet-loss/bandwidth profiles across desktop/mobile; enforce per-route/atlas byte ceilings, superseded viewport cancellation, reduced-data table/density-first fallback, and truthful selection/coverage before fixing unexplained bundle/chunk/heap/FPS regressions.
- [ ] Run `cd dashboard && npm ci && npm test && npm run build && npx playwright test`; run `cargo test --test dashboard_api_test`; run package verification. Expected: all pass.
- [ ] Commit: `test(dashboard): complete product quality gates`.

### Task 17: PR 35–37 — Per-domain cutover, bounded rollback, and deletion

**Files:**
- Modify: `dashboard/tests/fixtures/v1-surface-inventory.json`
- Modify: `dashboard/app/src/migration-paths.ts`
- Modify: `dashboard/build.mjs`, `src/dashboard/assets.rs`, `Cargo.toml`, `docs/dashboard.md`
- Delete only after gates: old plugin source/dist directories listed below

- [ ] For one domain at a time, run V1/V2 differential read and command fixtures, migration/backfill coverage, direct deep link, history, mobile, export, and rollback drill.
- [ ] Mark inventory rows `parity-proven`, switch the current route/feature flag, and disable the V1 executable binding atomically; rollback is an explicit receipt-bound operator action during migration, not a stale-client fallback.
- [ ] Remove a plugin after zero unresolved inventory rows, no generated capability/route reference, packaged asset proof, migrated non-disposable data, and a closed bounded rollback receipt; no generic release-count grace period applies.
- [ ] Delete in dependency order: `dashboard/graph/`; `dashboard/lcm/`; `dashboard/code-diagnostics/`; `dashboard/savings/`; `dashboard/settings/`; Holographic curation subfeatures then `dashboard/holographic/`; `dashboard/hermes-wrapper/`; old `dashboard/shell/` and V1 shims last.
- [ ] Remove corresponding V1 Rust dashboard routes/services only under the owning backend cutover plan; frontend deletion does not authorize data/service deletion.
- [ ] Rebuild/package from a clean checkout and prove no deleted asset path, old `?tab=` link, wrapper path, command, current help, hint, or catalog entry is orphaned; stale routes return the typed update/restart/current-route failure and never redirect silently.
- [ ] Commit one domain retirement per PR; final commit: `refactor(dashboard): retire V1 plugin shell`.

## 22. Verification matrix

### 22.1 Data correctness before screenshots

- Aggregate membership/count/denominator/sample/hidden equality against API fixture.
- Stable entity/relation/path identity and lens-specific legal edge kinds.
- Timeline occurred/ingested order, half-open windows, late events, Turn bounds, and hidden routine counts.
- Sanitized-native/canonical/human/direct-user/subagent/protocol transcript counts and representative membership.
- Query rows/facets/ranking/coverage/cursor equal API contract.
- Local/live Git head/base/merge-base/changed-file digest and drift behavior.
- Task/ticket related repository/worktree/branch/PR/agent/attempt membership, creator/source provenance, association confidence/disposition, cleanup delegation, eligibility/blockers, retention/expiry, operation state, and cleanup receipt equality against the same sealed API views; association never implies cleanup authorization.
- Fact/skill/policy/config versions and lifecycle links.
- V1 behavior/action inventory status.

### 22.2 User-task gates on the fixed corpus

Each user task has a scripted Playwright scenario with deterministic fixtures for repeatability and a matching human comprehension/orientation protocol from section 18; neither substitutes for the other. Script timing is measured from navigation start to the final assertion on the recorded reference machine, median of five runs. These sixteen scenarios define the "primary workflows" referenced by the section 19 long-task budget. "Survive typo/role ambiguity" is concrete: the script submits a fixed misspelled query and an ambiguous role facet and passes only if the disambiguation flow completes to the target result without a dead end or manual query retyping.

- Find an exact historical direct-user prompt, expand its copied/delegated/native set, and prove sanitized source identity/export in `<= 30 s`.
- Follow a parent agent through subagents and direct code/test/commit/PR impact in `<= 60 s`.
- Inspect an inferred relation and find its evidence/confidence/algorithm in `<= 30 s`.
- From a selected Turn, Fork to Playground, replay one hint then-versus-now, align changed stages, and explain the exact payload difference in `<= 60 s`; the extended manual protocol branches one field, sweeps, minimizes, and saves the anchored experiment.
- Compare two sessions and export complete evidence with coverage/caveats in `<= 90 s`.
- Open All Memory, verify complete cursor/coverage semantics, filter and inspect one memory/association, then find its source transcript/actor/run, validation, uses/outcomes, and any autonomous supersession/recovery in `<= 90 s`; repeat the source/use traversal for one managed skill.
- Find an active nearby agent in a parallel worktree, prove direct overlap, inspect the safe summary/anchor, send or suppress one audited coordination action, and verify no repeat hint in `<= 60 s`.
- Start at All, use the frozen historical Rspack/Rsbuild/React Router scenario to disambiguate same-name scope candidates, traverse a cross-repo graph/search result, and export equivalent CLI/MCP/API retrieval recipes with matching provenance in `<= 60 s`; no external checkout or benchmark execution is permitted.
- Open a cross-repository initiative, trace plan version→dependency/gate→offer→packet→attempt→artifact/outcome, inspect one blocked task, and complete one legal versioned transition without losing scope/selection in `<= 90 s`.
- From one task, identify every related repository/worktree/branch/PR and active agent/attempt, distinguish a high-confidence discovered association from an ambiguous proposal, resolve only the relation, then explain why an archived/PR-merged worktree is retained or cleanup-eligible; open the exact Git/thread/timeline evidence and complete one authorized cleanup preflight/confirmation or verify the truthful capability/blocker state and terminal receipt in `<= 90 s`.
- From Evolution, trace one skill or memory from evidence through autonomous validation/materialization/use/outcome/recovery, replay old-versus-current without live effects, and explain an unresolved denominator or regression in `<= 90 s`.
- Open Context Scout Observatory, distinguish useful silence from unavailable/late/suppressed delivery, inspect one addressed envelope and its anchors/coverage, pause and resume at a safe boundary, and verify no replay or counter mutation in `<= 60 s`.
- Open Privacy, distinguish clean from unknown/locked/unscanned coverage, inspect one safe finding/remediation lineage, start only the authorized operation-specific workflow, and verify no candidate content leaks in `<= 60 s`.
- From `/observatory/sync`, diagnose a stale-cache/authority-unavailable fixture, inspect node/store/placement and pending-spool coverage, follow the operation trace, then verify Settings shows the same desired/activated/observed state in `<= 90 s`.
- Starting from one PR, traverse PR→commit→symbol→Turn→agent→task→memory/managed skill→automation outcome while switching Git, Code, Turns, Agents, Tasks, Memory, and Automation lenses plus Loom without losing scope, time, selection, atlas position, or retrieval anchors; explain every typed bridge and reject one unsupported bridge in `<= 120 s`.
- In Settings, find one redactor policy through task-oriented `privacy` grouping and search, inspect basic/advanced/operator visibility, safety floor, effective source chain, affected runtimes/data and CLI/API equivalent, submit one versioned CAS edit, then follow validation, activation acknowledgement, rescan/reindex progress, and terminal receipt with dashboard/CLI equality in `<= 120 s`.

### 22.3 Required commands

```bash
cd dashboard
npm ci
npm test
npm run build
npx playwright test
cd ..
cargo test --test dashboard_api_test
cargo nextest run --workspace --no-fail-fast
git diff --check
```

Expected: all pass. Before executing Rust compiler/check commands, use TraceDecay diagnostics per repository instructions; the frontend plan does not override workspace test-selection guidance.

## 23. Release gates and definition of done

- `/` opens a truthful All/Brain view across the active profile; project selection is a filter, not another app.
- All/repository/project/worktree/ref scopes are explicit and ambiguity-safe — the section 22.2 scope task and its typo/ambiguity script complete without dead ends — and semantically identical across dashboard/CLI/MCP/API; frozen historical federated Rspack/Rsbuild/React Router scenario fixtures retain same-name disambiguation and per-shard provenance/stale/partial state without becoming product repositories, runtime/build dependencies, or benchmark gates.
- Git, code, thread, agent, Turn, task, plan, memory, and automation/skill graph lenses preserve distinct semantics, compose through bounded typed overlays/bridges, and retain a stable profile-atlas mental map across ordinary refresh. Timeline remains a Loom composition/overlay and is never serialized as `GraphLensV1`.
- Causal Loom follows agents/sessions/Turns through context, visible reasoning, tools, code, tests, Git/delivery, hints/memory, goals, and outcomes with evidence-class connectors.
- Claude workflows, Codex goals, and Hermes-style curator/reflector/skill-writer actors are captured and visible as typed related entities.
- Evolution Studio makes skill, memory, policy, and automation evolution inspectable from evidence through autonomous version/use/outcome/supersession/recovery.
- Agent coordination exposes expiring evidence-backed same/parallel-worktree proximity, direct overlap, safe summaries, stable anchors, audited actions, one deduped hint, and read-only historical replay without claiming presence from silence.
- Every task projection and detail exposes related repositories, observed worktrees, branches, PRs, active agents/attempts, association provenance/confidence, cleanup delegation/eligibility/blockers, retention/expiry, and receipts. TraceDecay never creates/provisions a worktree; association resolution never grants cleanup authority; archive/PR merge never implies deletion; only a distinct generated daemon capability can perform confirmed cleanup, with responsive table/mobile/accessibility parity and no browser filesystem/database/Git access.
- Every lab uses one experiment/run lifecycle, exposes exact/recorded/best-effort fidelity, aligned stage traces, branches/sweeps, canonical anchors, and a resource-access receipt proving it cannot mutate live state.
- #410 native/representative/human-best-effort/direct-user/delegated-agent/tool-result/provider-protocol modes, counts, provenance, and copy membership are explicit; no record is silently omitted.
- Every V1 read, filter, state, action, capability, route, and error behavior is inventoried and parity-proven or documented as an approved semantic change before retirement.
- Every view has loading/empty/stale/partial/offline/locked/redacted/incompatible/error behavior, table/outline parity, desktop/mobile behavior, keyboard/screen-reader support, reduced motion, and deterministic export.
- Initial bundle, response, render, FPS, main-thread, heap, and user-task budgets pass on the recorded corpus/reference machine.
- Three complete concept directions are critiqued against identical data before one is approved; final screenshots and transition recordings pass `view_image`/storyboard fidelity review, perceptual and legibility metrics, principal-user approval, independent visualization/accessibility critique, and section-18 comprehension/orientation gates.
- Legacy plugins retire independently after bounded migration/rollback receipts close; no stale live name/fallback survives V2 default, and no frontend cutover deletes user data or backend evidence.
- No production frontend file exceeds `800` lines and no route/application component becomes a feature, data, and renderer hairball.

## 24. Plan self-review checklist

- [ ] Master-plan routes, Brain, Explorer, Loom, domain workspaces, labs, visualization, privacy, parity, performance, and deletion are each mapped to tasks/tests.
- [ ] Merged #405/#407/#410/#411/#412/#413/#414/#415/#416/#417/#418/#419/#420/#422/#423/#424/#425 semantics and closed #409 history are reflected in identity/profile, message/fact views and ranking explanations, denominator-safe exact analytics, doctor authority, operator-only split-store consolidation/recovery, race-safe move-symbol parity, daemon/proxy/update recovery, generation-scoped inventory refresh, release state, and stale-client behavior.
- [ ] Generated application/API/hook/tool-catalog contracts are consumed without browser-owned business logic.
- [ ] Every graph lens names legal nodes/edges/layout/fallback/evidence.
- [ ] Agent proximity/coordination preserves expiring claim evidence, same/parallel-worktree semantics, safe summary/anchor/recipe, audited actions, one-hint dedupe, Coordination Lab, and analytics.
- [ ] Explorer exposes lexical/phrase/fuzzy/entity/semantic/graph/recency stages, origin/kind filters, grouping/dedupe/native expansion, Query Explain caps, and per-slice benchmark gates without assuming embeddings help.
- [ ] Every V1 mutation has direct or operation-specific command/audit/parity ownership; no generic preview/apply/rollback lifecycle returns through the UI.
- [ ] URL/local/IndexedDB/encrypted storage ownership excludes sensitive literals from unsafe locations.
- [ ] SSE gap/resync/offline/coverage semantics preserve last-known-good evidence.
- [ ] Mobile, keyboard, screen reader, reduced motion, table fallback, deterministic export, and visual fidelity start in feature PRs.
- [ ] No incomplete implementation phrase, generic “write tests,” or unowned implementation step remains.
- [ ] Exact file paths, focused commands, expected outcomes, PR boundaries, and deletion gates are present.
