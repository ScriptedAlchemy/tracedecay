# TraceDecay V2 Brain Dashboard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the project-tab dashboard with one profile-wide investigative workbench whose default Brain, Explorer, Causal Loom, domain workspaces, graph lenses, and replay labs make every captured agent, turn, tool, code, Git, delivery, memory, skill, automation, policy, and outcome relationship inspectable while preserving complete sanitized-native evidence and approved V1 semantics.

**Architecture:** A route-lazy React/TypeScript application consumes generated HTTP V2 read-model and command contracts; one URL-addressable `InvestigationStateV1` coordinates scope, time, query, selection, comparison, renderer, and inspector across every route. Server-side aggregation and frozen vector watermarks bound data, TanStack Query owns snapshot caches, an explicit SSE state machine applies typed deltas, and focused Sigma/ELK/Canvas/ECharts/CodeMirror renderers expose synchronized outline/table fallbacks instead of attempting a universal graph hairball.

**Tech Stack:** React 19; TypeScript 5.9; bundler selected by the measured Rsbuild-versus-Vite ADR; React Router; TanStack Query and Virtual; Sigma.js/Graphology; ELK.js; ECharts; D3 scales; CodeMirror 6; Web Workers; Vitest/Testing Library; Playwright and `@axe-core/playwright`; Rust/Axum embedded assets and generated OpenAPI/JSON Schema client contracts.

[`20-configuration-control-plane.md`](20-configuration-control-plane.md) is authoritative for `/settings`: its registry generates every form, source chain, validation rule, impact, CLI/API recipe, and drift state. The frontend cannot retain dashboard-only toggles, hidden config, or its own defaults/precedence.

[`21-cli-mcp-tool-surface-and-output-unification.md`](21-cli-mcp-tool-surface-and-output-unification.md) is authoritative for the capability command palette, guided actions, typed output/error/coverage views, and cross-surface examples. The dashboard consumes generated typed views and field descriptors; it never parses MCP Markdown, runs CLI commands, or recreates pagination/rendering semantics.

[`22-incremental-context-scout-and-suggestion-envelopes.md`](22-incremental-context-scout-and-suggestion-envelopes.md) owns Context Scout state, delivery/outcome semantics, Observatory controls, Loom lane, and Hint Lab replay. [`23-session-lcm-temporal-retrieval-and-evaluation.md`](23-session-lcm-temporal-retrieval-and-evaluation.md) owns the Search Quality Lab's temporal/current/as-of/copy/summary-DAG explanations and evaluation contracts; its formerly separate "Search Lab" is folded into the Search Quality Lab and is not a distinct lab or route. The browser only renders those typed views.

[`24-canonical-task-plan-graph-and-multi-agent-executor.md`](24-canonical-task-plan-graph-and-multi-agent-executor.md) owns the Work product semantics: initiative/plan/task/attempt inspectors, boards as saved queries, dependency DAG, critical path, timeline/causal/workload/executor/repository/agent/All lenses, claim-overlap evidence, context-packet inspection, and Orchestration Lab. The frontend never stores board-local tasks, derives readiness, selects routes, or treats drag/drop as arbitrary status mutation.

---

## 1. Contract lock

This plan refines master-plan PRs 4A and 25–32. It depends on the V2 domain, query, policy, application, API, hook, and tool-catalog plans; it does not move their business logic into the browser.

1. `/` means active-profile **All/Brain**, not the most recently selected project. A project page is a saved filtered investigation over the same models and components.
2. There is one logical Brain and several typed graph lenses. Git ancestry, code calls, thread/Turn membership, agent delegation, time order, memory similarity, and automation lineage remain distinct edge vocabularies and visual encodings.
3. A `Turn` is a first-class interval with the context visible at its start and the messages, provider-visible reasoning artifacts, goals, tools/results, files/code, hints/retrieval/memory, costs, and state produced by its end.
4. Every aggregate states exact versus sampled counts, denominator, hidden count, coverage, watermark, and aggregation/layout version. Unknown is never displayed as zero; inferred is never displayed as observed; correlation is never styled as causation.
5. Sanitized native records are never silently discarded. Merged PR #410's copied-subagent-prompt dedupe and domain `MessageOrigin`/`MessageView` become explicit UI modes with representative and hidden-copy counts plus source provenance; protected plaintext, if retained, is available only through the separate elevated quarantine workflow.
6. Canonical V2 read models and commands are the application/API responsibility. Feature modules do not join raw endpoints, synthesize source-of-truth counts, interpret tool schemas, or write stores directly.
7. All replay labs are read-only by default. Exact, recorded-result, and current best-effort are distinct, persistent labels; unavailable historical inputs are shown as unavailable, not substituted silently.
8. The app is local-only and loopback-secured by default. Sensitive literals never enter URLs, browser history, analytics, SSE subscription URLs, cache keys persisted in clear text, clipboard links, or catalog rows.
9. Accessibility, mobile behavior, table parity, partial/offline behavior, deterministic export, and visual QA are acceptance gates in every feature PR. PR 32 audits and polishes them; it does not introduce them for the first time.
10. Existing plugins keep their complete read and write behavior until the owning V2 workspace passes behavior/action parity. There is no blanket read-only transition.
11. No decorative dashboard-card grid, fake metric, gratuitous badge/pill, particle field, 3D graph, perpetual force animation, or color-only evidence encoding is allowed. Use open canvases, rails, lists, matrices, tables, timelines, and one clear focal artifact.
12. The shipped UI is code-native. Image-generated concepts are implementation references, not rasterized application screens.
13. Doctor/provider/daemon branding is evidence-driven: foreign-owned packages are informational, partial integrations are labeled partial, and upgrade completion requires a durable drain/recovery receipt rather than a green icon inferred from process exit.

Publication refresh (2026-07-10): `origin/master` was `6c4b8b91`; #407/#410/#411/#413/#414/#415/#416/#417/#419/#420/#422/#423/#424 were merged and #418 was open. The implementation lead must refresh again. The UI inventory treats ordinary-profile Hermes, #410 message views, #411 doctor authority, #415 release integrity, #417 identity split, #414/#419 race-safe `move_symbol` execution/conflict receipts, #420 proxy-before-store/reconnect/no-write-replay behavior, #422 negotiated tool-list refresh, #423 rank direction/explanation/counters, and #424 exact aggregate-before-sample analytics as current.

## 2. Product model and design direction

The product is an evidence workbench for one interconnected system:

```text
profile
  ├─ projects / repositories / worktrees / refs / commits / PRs / releases
  ├─ threads / sessions / Turns / messages / visible reasoning / context / goals
  ├─ actors / agents / subagents / delegations / handoffs / workflows
  ├─ tools / code symbols / files / patches / diagnostics / tests / impact
  ├─ facts / decisions / contradictions / retrieval / trust / memory versions
  └─ schedules / curator-reflector-skill-writer runs / proposals / skills / outcomes

identity + time + scope + evidence + provenance + coverage connect every lane
```

The interface has one visual thesis: **a quiet investigative instrument with a living but stable map**. Dense evidence is revealed through semantic zoom, selection, and coordinated views rather than by surrounding the user with equally weighted cards.

### 2.1 Concept contract before implementation

PR 4A/25A must use the frontend design workflow to create and approve these code-native UI reference images before production component work:

- Brain desktop at `1440×1000`: first-scan claim, central profile topology, aligned activity rail, compact health strip, inspector.
- Brain mobile portrait at `390×844`: claim, focused neighborhood, single activity lane, command and inspector sheets.
- Causal Loom desktop at `1440×1000`: density overview, agent tree, lanes, selected Turn, code/diff inspector.
- Universal Explorer desktop and mobile: query, result table, graph pivot, explain drawer, comparison collection.
- Labs desktop: shared replay frame plus Hint Lab and Evolution Studio detail states.
- Error-state board: loading, empty, stale, partial, offline, locked, redacted, incompatible, and fatal states.

Each concept must preserve the route and data anatomy in this plan, use the exact semantic color ledger, show realistic fixed-corpus data, and include all code-native labels. Review rejects generic card mosaics, illegible miniature charts, detached legends, hover-only values, and concepts that invent metrics or actions. Save approved concept images and an extraction ledger under `dashboard/design/concepts/`; implementation verification compares browser screenshots against them with `view_image`.

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

### 2.3 Semantic color and mark ledger

Create one ledger in `dashboard/packages/design-system/src/semantic-ledger.ts` and CSS variables in `tokens.css`:

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

The ledger is validated in contrast, grayscale, deuteranopia, protanopia, and tritanopia screenshots. Domain categories may choose shapes/icons but cannot repurpose state colors.

## 3. Information architecture and route ownership

`dashboard/app/src/routes.tsx` defines route metadata once: path, label, feature owner, required capabilities, lazy import, default renderer, keyboard help, and migration-only legacy paths. Current route metadata/help never advertises a stale name.

| Route | Primary question | Feature owner | Default artifact |
|---|---|---|---|
| `/` | What is TraceDecay doing, learning, changing, and failing? | `features/brain` | clustered profile topology + aligned activity |
| `/activity` | What is active or unhealthy now? | `features/activity` | generated activity event/facet model + health matrix |
| `/explore` | Where is this evidence and how is it connected? | `features/explorer` | result table + selected pivot |
| `/timeline` | What happened, in what order, and what did it affect? | `features/causal-loom` | density + virtualized causal lanes |
| `/sessions`, `/sessions/:id`, `/turns/:id` | What context and work occurred in this thread/Turn? | `features/sessions` | session list / Turn evidence outline |
| `/agents`, `/agents/:id` | Which agents collaborated and with what outcomes? | `features/agents` | delegation tree + Turn sequence |
| `/work`, `/work/initiatives/:id`, `/work/plans/:id/versions/:version`, `/work/tasks/:id`, `/work/attempts/:id`, `/work/executors`, `/work/scheduler`, `/work/views/:id`, `/work/notifications` | What work exists, how is it gated/routed/executed, and what context/outcomes connect it to the Brain? | `features/work` | initiative/plan outline + saved Kanban/DAG/task/attempt projection |
| `/coordination` | Which nearby agents may overlap, and what safe action is warranted? | `features/coordination` | evidence-ranked presence/overlap ledger + worktree map |
| `/goals/:id` | How did this Codex goal or provider-native objective evolve and finish? | `features/agents` | versioned goal/plan/Turn evidence ledger |
| `/workflows/:id`, `/automation/runs/:id` | How did the captured workflow/run execute? | `features/automations` | run waterfall + artifact lineage |
| `/code`, `/code/entities/:id`, `/code/compare` | What code changed, depends on it, and is affected? | `features/code` | symbol/snapshot graph + code viewer |
| `/graphs/:lens` | Show one graph vocabulary over shared state | `features/graphs` | lens-specific renderer |
| `/knowledge`, `/knowledge/facts/:id`, `/knowledge/entities/:id` | What does TraceDecay know and why? | `features/knowledge` | fact/version/provenance views |
| `/delivery`, `/projects/:id`, `/projects/:id/branches/:branch`, `/pulls/:id` | What was produced, observed, or encountered in Git/delivery? | `features/delivery` | Git/PR graph + evidence ledger |
| `/automations`, `/skills`, `/evolution` | How is the system autonomously curating and improving itself? | `features/automations` | autonomy decision/effect/outcome ledger + effectiveness views |
| `/observatory` | Is capture, storage, projection, privacy, and query healthy? | `features/observatory` | project × subsystem matrix |
| `/observatory/context-scout` | Is incremental suggestion preparation useful, timely, quiet, private, and healthy? | `features/observatory` + `features/hints` | trigger/silence/envelope/delivery/outcome funnel + queue/model/tool/host state |
| `/privacy` | Is the mandatory sanitizer effective across every source/sink, and what is blocked or needs remediation? | `features/privacy` | coverage/unknown matrix + safe remediation lineage |
| `/costs` | Where do tokens, latency, and cost go? | `features/costs` | precise ledger + trends |
| `/playgrounds/:lab` | What would this versioned engine decide, and why? | `features/playgrounds` | shared replay workbench |
| `/playgrounds/evolution` | How do skills, memories, policies, and automations evolve? | `features/evolution` | version/use/outcome lineage |
| `/saved/:viewId` | Reopen a classified saved investigation | `features/saved-views` | saved route + state |
| `/settings`, `/settings/context-scout` | Which effective settings and capabilities govern behavior? | `features/settings` | scoped settings form + source labels; the context-scout subroute renders plan 22's scout target/layer/effective-provenance controls through plan 20's generated registry forms |

Legal `:lens` values are `git`, `code`, `threads`, `agents`, `turns`, `tasks`, `plans`, `timeline`, `memory`, and `automation`. Legal `:lab` values are `hints`, `retrieval`, `ingest`, `query`, `search-quality`, `scope-federation`, `correlation`, `coordination`, `orchestration`, `scheduler`, `memory`, `policy-diff`, and `privacy`; together with the named `/playgrounds/evolution` route these thirteen slugs form the canonical fourteen-lab inventory: Hint, Retrieval, Search Quality, Coordination, Orchestration, Ingest, Query, Correlation, Scheduler, Memory, Policy Diff, Evolution, Scope/Federation, and Privacy. The `privacy` slug displays as "Privacy & Secret Safety Lab" and supersedes the retired `secret-safety` route name; plan 23's temporal "Search Lab" content is folded into the Search Quality Lab, not a separate lab. Hint Lab includes deterministic and incremental-scout replay, Search Quality includes temporal session/LCM retrieval, and Orchestration replays plan/task/executor/context/lease decisions at `/playgrounds/orchestration` (the only route spelling) against plan 10 §8.5's generated `POST /api/v2/labs/orchestration:replay` endpoint. Evolution has a named route because it is both a lab and a product workspace.

Route changes do not clear scope/time/query/selection unless the destination cannot represent the selected entity. In that case, the selection stays pinned in the inspector and the main view explains the unsupported relation. Browser back/forward restores complete committed investigation states, not only route names.

Superseded route names are recorded here so no ledger dangles: the master plan's `/threads` and bare `/turns` list views are served by `/sessions` and the `threads`/`turns` lenses; `/agents/nearby` became `/coordination`; `/proposals` is retired by the no-approval-queue autonomy model and its content lives under `/evolution`; `/secret-safety` became `/playgrounds/privacy`. Bounded migration-only mappings live in `migration-paths.ts` and disappear at cutover; no current route metadata advertises the stale names.

## 4. Shared investigation state

Create the only cross-feature state model in `dashboard/packages/query-state/src/investigation.ts`:

```ts
export type TranscriptMode =
  | "native_rows"
  | "normalized_representative"
  | "human_best_effort"
  | "direct_user"
  | "delegated_agents"
  | "tool_results"
  | "provider_protocol";

export type ComparableSelectionV1 =
  | { kind: "entity"; id: string }
  | { kind: "event"; id: string }
  | { kind: "relation"; edgeKind: string; sourceId: string; targetId: string }
  | { kind: "path"; nodeIds: readonly string[] }
  | { kind: "aggregate"; tileId: string; memberCursor: string | null }
  | { kind: "time_range"; from: string; to: string; laneIds: readonly string[] };

export type SelectionV1 =
  | ComparableSelectionV1
  | { kind: "comparison"; a: ComparableSelectionV1; b: ComparableSelectionV1 };

export type InspectorTabV1 =
  // universal tabs (every workspace)
  | "summary" | "evidence" | "relations" | "native" | "history" | "actions"
  // Work-workspace extension tabs (plan 24 §12.6 task/attempt inspectors)
  | "specification" | "dependencies" | "acceptance" | "assignments" | "attempts"
  | "packets" | "decisions" | "impact" | "costs" | "audit";

export interface InvestigationStateV1 {
  version: 1;
  profileId: string;
  scope: {
    selector: ScopeSelectorV2;
    resolution: ScopeResolutionV2 | null;
  };
  time: {
    occurred: { from: string; to: string };
    knowledgeAsOf: string | null;
    live: boolean;
    compare: null | { a: { from: string; to: string }; b: { from: string; to: string } };
  };
  query: {
    queryFingerprint: string | null;
    protectedDraftId: string | null;
    facets: Readonly<Record<string, readonly string[]>>;
    transcriptMode: TranscriptMode;
  };
  focus: {
    selected: SelectionV1 | null;
    retrievalAnchors: readonly string[];
    retrievalRecipeId: string | null;
    pinned: readonly string[];
    path: readonly string[];
    collectionId: string | null;
  };
  view: {
    renderer: "graph" | "timeline" | "table" | "matrix" | "distribution" | "small_multiples";
    graphLens: "git" | "code" | "threads" | "agents" | "turns" | "tasks" | "plans" | "timeline" | "memory" | "automation";
    layout: string; // registered "<algorithm>@<layoutVersion>" from the renderer registry
    visibleLanes: readonly string[]; // generated per-lens lane-vocabulary IDs only
    levelOfDetail: "auto" | "aggregate" | "neighborhood" | "evidence";
  };
  inspector: { tab: InspectorTabV1 };
}
```

Route lens slugs map explicitly to generated application enums: `git→Git`, `code→Code`, `threads→Thread`, `agents→Agent`, `turns→Turn`, `tasks→Task`, `plans→Plan`, `timeline→Timeline`, `memory→Memory`, and `automation→AutomationSkill`. This mapping is generated/fixture-tested; the fixture asserts that the route `:lens` slug list, the `view.graphLens` union members, and the generated application enum stay identical (ten entries each) so a lens can never be routable but URL-unrepresentable. Feature code never title-cases or guesses an enum.

Selection, comparison, and inspector semantics:

- Every selection kind the inspector supports (section 9.2) is a `SelectionV1` variant, so relation, aggregate, time-range, and comparison selections are URL-encodable, shareable, and restorable like entity selections. Selections serialize with sorted keys and default elision and carry only opaque IDs.
- Period comparison lives in `time.compare` as explicit A/B ranges; entity/aggregate/path/relation/time-range pair comparison is a `comparison` selection. The section 9.1 compare toggle populates exactly one of these two homes; there is no third comparison shape.
- `InspectorTabV1` is owned here; plan 24 cites it. Plan 24 §12.6's eleven task-inspector tabs map onto the union by a checked table: Overview/specification/constraints→`specification` (`summary` remains the universal landing tab), Dependencies/gates/critical path→`dependencies`, Acceptance/evaluations/exceptions→`acceptance`, Assignments/eligible executors/routing→`assignments`, Attempts/retries/cancellation→`attempts`, Context packets/omissions→`packets`, Decisions/handoffs/artifacts/outcomes→`decisions`, Thread/session/Turn/agent/goal/tool evidence→`evidence`, Code/Git/delivery impact→`impact`, Costs/budgets→`costs`, Audit/provenance/anchors→`audit`. The attempt inspector reuses the same union. Workspaces cannot invent tab values outside this union; non-Work routes ignore Work extension tabs with an explicit "tab unavailable here" state, never a silent reset.
- `view.layout` values are registered `<algorithm>@<layoutVersion>` identifiers from the renderer registry; `view.visibleLanes` values come from the generated per-lens lane vocabulary, which is versioned with the application enums above. Restoring a URL whose layout or lane IDs are no longer registered shows an explicit "layout/lanes reset to default" notice and falls back to route defaults — a restored URL never silently no-ops.

### 4.1 State ownership

- **URL:** route; opaque profile/repository/project/worktree/ref/entity/saved-view IDs; non-sensitive time bounds; renderer/lens/layout; the committed `SelectionV1` (every variant, carrying only opaque IDs); facet IDs; transcript mode. Serialize arrays in stable sorted order and omit defaults.
- **Encrypted profile storage:** query literals, annotations, collections, protected saved views, replay input payload references, redaction decisions. URLs hold only an opaque `protectedDraftId` scoped to the local profile.
- **IndexedDB:** versioned bounded response cache, local uncommitted annotation drafts, route recovery checkpoint, deterministic layout coordinates keyed by snapshot/query/layout version. Payloads obey server sensitivity and retention metadata; locked/profile-sign-out purges protected records.
- **Local preferences:** theme, density, keyboard layout, panel geometry, last nonsensitive route, reduced-motion override. Never store entity payloads here.
- **Renderer-local:** hover, lasso-in-progress, drag state, provisional camera, worker job, GPU buffers. Commit selection/camera only on interaction end.

`ScopeSelectorV2` (defined in plan 16 §4) and `ScopeResolutionV2` (defined in plan 01) are imported from the generated contract transported per plans 10/17; query state does not define another `mode/include/projectIds` selector. URL serialization retains the canonical selector's nonsensitive opaque roots/exclusions/time/policies/limits and the resolution ID; candidate details and safe aliases stay in the bounded cache. `retrievalAnchors` are server-issued domain `RetrievalAnchorId`s for session/thread/Turn/message/agent/subagent/workflow/goal/Git evidence; authorized resolution returns `RetrievalAnchorRecordV1`. They survive cursor, SSE, and migration response-handle expiry. Sensitive retrieval inputs remain behind `protectedDraftId`; copied links, saved views, collections, annotations, exports, and route recovery carry a `RetrievalRecipeV1` (defined in plan 01) or protected recipe ref, never an ephemeral response handle alone.

Scope defaults and ambiguity behavior are fixed:

- A new investigation starts at explicit active-profile `All`; cwd, last route, recent project, and selected entity never narrow it silently.
- The chooser is a lazy tree: All → repository → project → checkout/worktree → ref/snapshot, with explicit multi-select and an entity collection overlay. Project routes apply a visible filter over the same state.
- Every selected/candidate scope shows kind, canonical disambiguated `owner/repository/project/worktree/ref` label, authorized provenance, index generation, and fresh/stale/partial/unavailable state. Same-name items are never distinguished by color or truncation alone.
- Name/path/alias input calls generated scope resolution. Ambiguity opens a keyboard/touch-accessible candidate list; choosing one resubmits the preserved canonical request with its signed retry token in one step. The UI does not rebuild the query, guess by cwd, or ask the user to retype it.
- CLI/MCP/API equivalents exported from the workbench use the same opaque IDs and resolution token; scope semantics, candidates/order, provenance, coverage, and errors must match exactly.

`dashboard/packages/query-state/src/url.test.ts` must prove canonical round trips (including every `SelectionV1` variant and A/B compare ranges), default elision, back/forward, unknown-version refusal, the lens-slug/union/enum equality fixture, unknown layout/lane-ID reset behavior, and absence of sensitive literal fixtures. `history.ts` debounces replace-state during brushing but pushes selection, route, compare, and committed time changes.

### 4.2 Transcript modes and #410 seam

Every transcript/session/Turn/search surface shows a persistent mode control with the seven `TranscriptMode` values. Each response includes the generated `TranscriptVisibility` block — a plan 10/17 schema type re-exported through `packages/contracts`, reproduced here for reference and never redefined as a local interface:

```ts
export interface TranscriptVisibility {
  mode: TranscriptMode;
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

These are the shareability deliverables, so they have explicit shapes. All three are generated in the plan 10/17 schema (routes in plan 10 §8.6) and re-exported through `packages/contracts`; the browser defines no parallel record. Reference shapes:

```ts
export interface SavedViewV1 {
  id: string;                          // PK: opaque server-issued ID
  version: number;                     // optimistic-concurrency token
  name: string;                        // unique per (ownerScope, name)
  ownerScope: DeclaredScope;           // profile | cross-project | named project
  classification: "private" | "profile" | "shareable";
  redactionState: "none" | "redacted" | "pending_review";
  route: string;
  state: InvestigationStateV1;         // embedded; stateVersion checked on restore
  recipeRef: string | null;            // RetrievalRecipeV1 ID (plan 01), never a response handle
  watermark: string;                   // frozen snapshot/vector watermark
  shareBundleDigest: string | null;    // set by share_apply; cleared by share_revoke
  expiresAt: string | null;            // local share expiry
  createdAt: string;
  updatedAt: string;
}

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
  classification: "private" | "profile" | "shareable";
  redactionState: "none" | "redacted" | "pending_review";
  createdAt: string;
  updatedAt: string;
}
```

Keys, indexes, retention, and size envelopes: primary key is `id` for all three; uniqueness is `(ownerScope, name)` for saved views and collections; the server indexes saved views by owner/route/expiry, collections by owner, and annotations by target anchor. The owning store is the server-side saved-view/collection/annotation store behind plan 10 §8.6; the browser holds them only in the bounded IndexedDB cache (section 8.1), with annotation bodies and protected query literals confined to encrypted profile storage. Size envelopes: a serialized `SavedViewV1` is `<= 32 KiB` (state plus refs; no payload text); a `CollectionV1` holds `<= 10,000` member anchors/refs and serializes to `<= 256 KiB`; an `AnnotationV1` body is `<= 4 KiB`. Records over-envelope are rejected by the command preview with the exceeded bound, not truncated silently. Embedded `InvestigationStateV1` restores through the same versioned codec as URLs: unknown versions are refused with an explicit incompatible state.

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
│       └── migration-paths.ts           # bounded pre-cutover mappings; empty after cutover
├── packages/
│   ├── api-client/                       # generated from tracedecay-api OpenAPI
│   │   ├── package.json
│   │   ├── src/generated/schema.ts
│   │   ├── src/client.ts
│   │   ├── src/errors.ts
│   │   ├── src/sse.ts
│   │   └── test/{contract,sse}.test.ts
│   ├── contracts/src/                    # UI-safe aliases/compositions over generated API types
│   │   ├── read-models.ts
│   │   ├── commands.ts
│   │   └── contract-version.ts
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
│   │   ├── semantic-ledger.ts
│   │   ├── icons.tsx
│   │   ├── controls/
│   │   ├── layout/
│   │   ├── table/
│   │   ├── states/
│   │   └── a11y/
│   ├── inspector/src/
│   │   ├── UniversalInspector.tsx
│   │   ├── SummaryTab.tsx
│   │   ├── EvidenceTab.tsx
│   │   ├── RelationsTab.tsx
│   │   ├── RawTab.tsx
│   │   ├── HistoryTab.tsx
│   │   └── ActionsTab.tsx
│   ├── brain/src/
│   │   ├── BrainViewport.tsx
│   │   ├── AggregateTileLayer.tsx
│   │   ├── NeighborhoodLayer.tsx
│   │   ├── BrainOutline.tsx
│   │   ├── BrainMatrix.tsx
│   │   ├── lod.ts
│   │   ├── layout-cache.ts
│   │   └── workers/layout.worker.ts
│   ├── timeline/src/
│   │   ├── CausalLoom.tsx
│   │   ├── DensityBrush.tsx
│   │   ├── LaneViewport.tsx
│   │   ├── EventMark.tsx
│   │   ├── AgentTreeRail.tsx
│   │   ├── TranscriptWindow.tsx
│   │   ├── ImpactRibbon.tsx
│   │   ├── timeline-lod.ts
│   │   └── workers/timeline.worker.ts
│   ├── renderers/src/
│   │   ├── registry.ts
│   │   ├── RendererFrame.tsx
│   │   ├── selection-adapter.ts
│   │   ├── camera-adapter.ts
│   │   ├── export-scene.ts
│   │   ├── graph/SigmaRenderer.tsx
│   │   ├── dag/ElkRenderer.tsx
│   │   ├── canvas/DenseMarksRenderer.tsx
│   │   ├── matrix/MatrixRenderer.tsx
│   │   └── fallback/RelationshipTable.tsx
│   ├── charts/src/
│   │   ├── EChart.tsx
│   │   ├── direct-labels.ts
│   │   ├── descriptions.ts
│   │   ├── accessible-table.ts
│   │   └── chart-theme.ts
│   ├── code-viewer/src/
│   │   ├── CodeViewer.tsx
│   │   ├── DiffViewer.tsx
│   │   ├── MessageViewer.tsx
│   │   ├── SourceLocation.tsx
│   │   └── redaction-decorations.ts
│   ├── labs/src/
│   │   ├── LabWorkbench.tsx
│   │   ├── ReplayModeBanner.tsx
│   │   ├── InputManifest.tsx
│   │   ├── DecisionTree.tsx
│   │   ├── VersionPicker.tsx
│   │   ├── SideEffectGuard.tsx
│   │   ├── ComparisonDiff.tsx
│   │   └── FixturePromotionDialog.tsx
│   └── testing/src/
│       ├── fixtures.ts
│       ├── render.tsx
│       ├── a11y.ts
│       ├── fake-sse.ts
│       └── deterministic-time.ts
├── features/
│   ├── brain/
│   ├── activity/
│   ├── explorer/
│   ├── causal-loom/
│   ├── graphs/
│   ├── sessions/
│   ├── agents/
│   ├── coordination/
│   ├── work/
│   ├── code/
│   ├── knowledge/
│   ├── delivery/
│   ├── automations/
│   ├── observatory/
│   ├── hints/
│   ├── privacy/
│   ├── costs/
│   ├── playgrounds/
│   ├── evolution/
│   ├── saved-views/
│   └── settings/
├── tests/
│   ├── contract/
│   ├── component/
│   ├── e2e/
│   ├── visual/
│   ├── accessibility/
│   ├── performance/
│   └── fixtures/
├── build.mjs
├── rsbuild.config.ts or vite.config.ts   # exactly one, selected by ADR
├── vitest.config.mts
├── playwright.config.ts
├── tsconfig.json
├── package.json
└── package-lock.json
```

Rules:

- A production file targets `<= 500` lines; no new file may exceed `800` lines. Route modules contain composition and data loading, not renderer algorithms or endpoint joins.
- Root `packages/tracedecay-client` is the only OpenAPI-generated TypeScript HTTP/problem/SSE schema/runtime. Dashboard `packages/api-client` is a small browser cookie/CSRF/bootstrap binding and re-export layer over that official package; it contains no generated schema or competing pager/event/problem types. `packages/contracts` provides UI-safe aliases/compositions over the official generated schema and may not duplicate transport/domain types. Tool-catalog generation owns `app/src/generated/{catalog,commands}.ts`; `packages/contracts` re-exports their typed IDs/schemas rather than generating a second catalog.
- `packages/data-client` is the only owner of TanStack Query keys, snapshot caching, SSE, IndexedDB cache, and capability gating.
- `packages/query-state` is the only owner of URL/history/persistence semantics.
- `packages/renderers`, `brain`, `timeline`, and `charts` own drawing; feature modules supply typed models and callbacks.
- Each renderer creates at most one Canvas/WebGL context and one worker pool. Hidden routes suspend workers and animation frames and release large GPU buffers after a bounded idle period.
- `features/*` may depend on packages, never another feature's internal files. Shared functionality moves to a package after two proven consumers.
- No package imports V1 plugin source. Migration adapters live at the route boundary only while the explicit migration flag is active and disappear at cutover.
- The `dashboard/` workspace uses exactly one package manager: npm with the committed `package-lock.json` (`npm ci` in CI). Root `packages/tracedecay-client` is a separately managed workspace whose generation/test commands run under its own toolchain; the dashboard consumes only its built artifact. No pnpm lockfile, script, or command appears under `dashboard/`.

## 6. Bundler ADR and embedded-asset boundary

Create `docs/adr/dashboard-v2-bundler.md` before choosing a config filename. Measure the current Rsbuild/Rspack pipeline against a Vite prototype using the same shell route and lazy graph chunk. The ADR (benchmark evidence, decision, rollback path) lands as its own reviewable PR and merges before any bundler config file or implementation commit; Task 2's application-shell work starts from the merged decision so the ADR gate stays independent of the winning bundler's implementation.

The ADR matrix records:

| Dimension | Required evidence |
|---|---|
| Production build | cold/warm duration, peak RSS, emitted chunks, gzip/Brotli sizes, deterministic hash behavior |
| Development | startup, first transform, HMR latency for TSX/CSS/worker, proxy/SSE behavior |
| Rust embedding | manifest generation, base path, history fallback, immutable asset cache headers, `include_bytes!` integration |
| Code splitting | shell budget and lazy graph/timeline/editor chunks without dynamic public paths |
| Security | CSP without `unsafe-eval`, worker loading, source-map publication, asset path containment |
| Migration coexistence | current single-file plugin bundles and Hermes wrapper only behind the bounded per-domain migration flag |
| Tests | Vitest, Playwright, type checking, coverage, deterministic production preview |
| Packaging | fresh `cargo package`, crates.io prebuilt assets, no Node at runtime/docs.rs |
| Migration risk | changed scripts/files, rollback path, bounded old/new shell build, atomic removal of stale live routes/names at cutover |

Acceptance weights correctness/embedding/CSP/history/migration safety above raw build speed. The selected tool must:

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

1. Capability advertises action, scope, destructive class, preview requirement, and required version.
2. UI opens a typed preview. Destructive previews list descendants, redactions, holds, irreversible effects, and exact scope.
3. Confirm submits an opaque idempotency key and `ifVersion`/watermark; it never reuses a timed-out key for different input.
4. `409` presents current-versus-requested state and offers refresh/review, never blind retry.
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
  routeModel: string,
  queryFingerprint: string,
  scopeFingerprint: string,
  timeFingerprint: string,
  transcriptMode: TranscriptMode,
  cursor: string | null,
];
```

Sensitive text is represented only by an opaque server-issued fingerprint. Frozen snapshots are immutable and cacheable until retention/schema invalidates them. Live snapshots have bounded freshness and are changed only by typed deltas or full resync. IndexedDB stores at most the last 20 nonsensitive route snapshots and a configurable protected-cache quota; LRU eviction deletes payload chunks before metadata. Cache entries carry schema/access/retention digests and are rejected, not migrated heuristically, on mismatch.

Abort previous route/brush requests on supersession. Prefetch only the selected entity inspector and adjacent timeline page; never prefetch all shards or payload bodies.

### 8.2 SSE state machine

Subscriptions are created by `POST /api/v2/subscriptions` using a protected request body and return `{ subscription_id, expires_at, snapshot_watermark, replay_retention, stream_path }`. The browser opens the returned `GET /api/v2/subscriptions/{id}/events` with `Last-Event-ID` only on resume and deletes the resource through `DELETE /api/v2/subscriptions/{id}` on explicit close when reachable. Query literals never appear in the SSE URL, subscription ID, event ID, or logs.

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

The inspector works for entity, event, aggregate, path, relation, time range, and comparison selections — each a `SelectionV1` variant (section 4), so every supported selection kind is shareable and restorable through the URL:

- **Summary:** type, label, time/scope, observed/inferred status, coverage, key measures.
- **Evidence:** supporting observations/events, source offsets/hashes, evidence class, algorithm and confidence.
- **Relations:** incoming/outgoing typed relations, legal pivots, redacted frontier counts, bounded expansion.
- **Native:** sanitized native source row, normalized observation, canonical event, projection row, schema/privacy versions; authorization and transcript mode apply. This tab never opens protected-quarantine plaintext.
- **History:** versions, valid/observed intervals, supersession, corrections, late arrivals.
- **Actions:** generated allowed commands with preview/audit; no action is inferred from UI entity type alone.

Every inspectable research entity exposes “copy stable anchor” and “re-run retrieval recipe.” Resolution shows identity/version/watermark drift and coverage before navigation. A cursor or old response handle can page a current result but is never displayed as the durable research identifier.

Aggregate selection lists exact or sampled membership, denominator, hidden counts, expansion cursor, watermark, and algorithm version. Relation selection explains endpoint identities and why the connector is causal, structural, inferred, similarity, or temporal. Inspector tabs are keyboard-addressable; closing restores focus to the selected mark/row.

## 10. Brain / All implementation

The default Brain answers one question in this reading order:

1. **First-scan claim:** one server-authored, evidence-linked sentence naming the most consequential current activity or health issue plus scope, time, coverage, and confidence.
2. **Focal topology:** recent project/workflow/agent clusters connected by typed activity, selected to match the claim and current time window.
3. **Aligned activity:** agent/code/delivery/knowledge/automation lanes below the same time window.
4. **Health guardrail:** compact project × subsystem matrix for ingest, projection, query, storage, privacy, and remote freshness.
5. **Learning loop:** hint/tool/fact/skill/automation candidate→use→outcome funnel with unresolved horizon.
6. **Resume:** unfinished workflows, goals, agent runs, and saved investigations.

This is a spatial reading path, not six equal cards. On desktop the topology dominates; subordinate sections use open bands. On mobile the claim, one focused cluster, and one activity lane appear first; health/learning/resume live in sheets.

All/Brain is federated across the selected repositories/projects/worktrees/refs. Nodes, aggregate membership, edges, inspector titles, tables, and exports retain canonical repository/snapshot identity and per-shard provenance/freshness/partial state. Same-name projects, branches, files, symbols, sessions, or agents use disambiguated labels and never merge by display text; cross-repository connectors require typed dependency/session/workflow/Git/evidence relations.

### 10.1 Brain aggregate tile contract

`BrainTile` is the generated plan 10/17 aggregate-tile read model re-exported through `packages/contracts`; the browser never declares it as a local interface. Reference shape:

```ts
export interface BrainTile {
  id: string;
  kind: "profile" | "project" | "workflow" | "agent_cluster" | "domain_cluster";
  label: string;
  membership: { exact: boolean; count: number; denominator: number | null; sampled: number | null };
  activity: Readonly<Record<string, number | null>>;
  edgeCounts: readonly { kind: string; evidenceClass: string; count: number }[];
  coverage: CoverageSummary;
  hiddenChildren: number;
  expandable: boolean;
  expansionCursor: string | null;
  layout: { algorithm: string; version: string; seed: string; anchor: [number, number] | null };
}
```

Semantic zoom:

- L0 profile: projects/workflows/agents/domain health.
- L1 project/workflow: worktrees, branches, sessions, runs, memory, code snapshots, delivery.
- L2 neighborhood: selected entities and bounded typed relations.
- L3 evidence: exact message, visible reasoning artifact, tool event, diff, diagnostic, fact source, policy evaluation, artifact, or delivery record.

Expansion preserves existing positions, requests only child tiles/neighborhood, and animates no more than 250 ms unless reduced motion is set. The legibility budget is numeric: at most `250` labeled marks in the viewport, at most `2` overlapping label pairs per `100` labels, and no effective label smaller than `11 px` after zoom. If topology exceeds any of these bounds, the UI increases aggregation or switches to matrix/outline; it never merely hides labels. The performance suite asserts the bounds against the fixture corpus.

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
| Timeline | events/intervals/density bins | source order, causal, correlation, temporal | lane/time | occurred and ingested time |
| Memory | facts/versions, entities, decisions, contradictions, retrievals, feedback | source, supports, contradicts, supersedes, retrieved, rated | provenance DAG/cluster | trust/version/retention |
| Automation | jobs, schedules, runs, actors, candidates, artifacts, skill/memory versions, uses, outcomes | scheduled, spawned, proposed, validated, auto-decided, autonomously-applied, injected, observed, automatically-recovered | waterfall + lineage DAG | config/policy/actor identity |

Selecting any node changes the common selected entity and reveals cross-lens pivots. Switching lenses preserves time/scope/selection/pins. Edge keys and inspector language are lens-specific; shared visual geometry does not erase semantics.

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

Query Explain shows canonical AST/fingerprint, validation, cost/budget, selected and pruned shards, pushed/residual filters, FTS/vector/graph/time operators, ranking components including absent features, candidate universe, per-stage caps, cap-induced exclusions, stable sort key, cursor/watermark, timing, coverage, truncation, message-origin/view semantics, and retention. Noisy ranking can be diagnosed from exact score components and bounded candidates; capped/ambiguous results never look complete. Export equivalent current CLI/MCP/HTTP requests plus a stable retrieval recipe from the server-generated representation so the UI does not reinvent syntax or preserve stale names.

Explorer's checked benchmark panel runs the versioned redacted corpus by slice: exact literal, phrase, misspelling, symbol/entity/alias, origin ambiguity, cross-project concept, graph-related, recency, no-result, capped, adversarial noise, and embedding regression. It reports MRR, nDCG, Recall@k, Precision@k, zero-result rate, p50/p95 latency, candidate counts, coverage, and per-slice deltas. It is an evaluation artifact, not a vanity score; any exact-match/origin-filter regression blocks profile promotion.

One mandatory cross-repo slice spans Rspack, Rsbuild, and React Router repositories/worktrees/benchmarks with same-name files, symbols, branches, sessions, and known dependency/PR evidence. It verifies repo disambiguation, federated graph links, scope candidates, ranking relevance, provenance, and stale/partial labels across CLI/MCP/API/dashboard outputs.

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

### 12.5 Timeline windowing contract

The Loom's client data contract for large corpora is typed, generated in the plan 10/17 schema (density-bin fields lifted from master plan §14.2 into schema), and re-exported through `packages/contracts`:

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
- Existing-target actions use the entity's canonical owner and disable with an ownership-conflict explanation if the request state disagrees. Moving ownership opens the dedicated migration preview; it is not an editable field.
- Cross-project use links to one durable source version through evidence relations. The UI never offers “copy to project” as a shortcut for memory, skill, policy, or automation reuse.
- Mixed All-scope lists group/filter by owner without changing identity. Profile-owned and project-owned histories remain distinguishable in tables, graphs, exports, URLs, and replay manifests.

### 13.0 Work, plans, tasks, and executors

- The canonical selection is initiative/plan-version/work-item/attempt IDs plus frozen/live watermarks. A board is a protected saved `TraceQueryV1` and lens; changing board, repository, agent, executor, or layout never copies or rehomes tasks.
- Initiative overview shows exact cross-project scope, plan version, dependency/fan-in state, critical-path interval/slack, budgets/deadlines, active agents/executors, costs, outcomes, coverage, and links to Goals/workflows/code/Git/PR/check/release evidence.
- Plan outline, Kanban, dependency DAG, critical path, timeline, causal, workload, executor-fleet, repository-work, initiative, agent-relevant, and All views preserve identical IDs/counts/selection. This is plan 24's projection vocabulary exactly: §0.21's saved projections plus §12.5's Executor Fleet/Repository Work lens names and §12.7's agent-relevant slice; 11 renders no view name outside that set. Drag/drop invokes only generated legal commands; derived readiness cannot be set directly.
- Task/attempt inspector covers versions, gates, acceptance, assignment/route rationale, requested versus actual host/provider/model/reasoning effort/tools/skills/grants, fenced lease state, packet/omissions, workspace/ref/snapshot, Turns/tools/artifacts/handoffs/outcomes, cancellation/reconciliation, costs, audit, and anchors.
- Claim-overlap overlay distinguishes authoritative writable-resource reservations, advisory work claims, intentional ensemble/parallel roles, and weak proximity. It shows exact overlap evidence/TTL without sibling prompt text.
- Agent default is the active attempt plus blockers/parents, material siblings, decisions, acceptance, handoffs, packet entries, and workspace conflicts. All work requires an explicit human authorization/scope expansion.
- `/work/notifications` owns the plan 24 §12.7 human notification-subscription UI: explicit saved filters/channels with event classes, quiet hours, dedupe, rate budgets, and authorization, edited only through generated preview/apply commands. Task state never auto-subscribes the creating profile/channel, and dashboard toasts, gateway messages, hook hints, and task comments share no accidental notification loop.

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
- `message`, `handoff`, `ack`, and `suppress` use generated preview/apply commands. Delivery, acceptance, acknowledgement, suppression, expiry, and resolution are distinct states; a sent message never appears as an acknowledged handoff.
- One dynamic coordination hint may appear in the command/status rail for the highest material actionable overlap. It includes one sentence, one stable anchor, one primary action, “suppress,” and why-now evidence. Per agent/pair/work-claim dedupe, cooldown, acknowledgement, and suppression prevent repeat prompts; lower-ranked overlaps remain in the workspace, not stacked notifications.
- Analytics show eligible/material/selected/delivered/inspected/messaged/handoff/ack/suppressed/expired/resolved/duplicate-prevented/unresolved with denominators, coverage, and horizon. No outcome is inferred from later code proximity alone.

### 13.3 Code

- Repository/snapshot/file/stable-symbol/occurrence graph and lineage.
- Session/agent ownership overlays labeled direct, inferred, or unknown.
- Diff graph, dependency matrix, cycles/coupling, diagnostic/test and affected-test overlays.
- Branch/commit/as-of slider and snapshot comparison; CodeMirror source/diff with exact locations and redaction decorations.
- `move_symbol` is a generated command, never a browser rewrite helper. Preview is default and shows exact source/destination diff, inserted destination imports, caller/dependency/visibility/collision/module/cycle/orphan/cfg impact, snapshot/version, affected tests, and no caller auto-edit; apply requires confirmation, revalidation, repository/worktree grant, rollback/reindex operation, and durable receipt.

### 13.4 Knowledge

- Facts, versions, entities, decisions, contradictions, provenance, trust changes, feedback, retention, holds, supersession, and deletion lineage.
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

- Schedules, effective config/policy source, locks/leases, skip reasons, run waterfall, actors, artifacts, candidates, validation, autonomy decisions, automatic effects/recovery, and downstream adoption.
- Capture Claude workflow runs, Codex goals, and Hermes-style curator/session-reflector/skill-writer concepts as typed related entities, not one ambiguous run type.
- Managed skill lifecycle: evidence→candidate→validation/eval→policy decision→autonomous materialization→injection/use→outcome→autonomous revision/recovery/archive.
- Managed memory lifecycle uses the same product spine while preserving fact-specific trust/conflict/supersession/deletion semantics.

### 13.7 Observatory

- Project × subsystem health matrix; ingest lag; rewrite/backfill/parser coverage; identity conflicts.
- Catalog/activity/project/graph/blob health; migrations; projection lag; query latency/caps/partial results.
- Doctor findings display severity, observed owner, remediation authority, evidence, and only legal actions. Foreign-owned packages are informational; the UI cannot render an update/repair button for foreign or unknown authority.
- Storage identity split is a named conflict with both safe candidates, evidence, backup/consolidation preview, and no initialize action; it is never rendered as “no index.”
- Provider integrations show `Detected/Installed/Configured/Healthy/Degraded/Partial/Unsupported/ForeignOwned` with hook/tool/session coverage, missing pieces, last verification, and repair owner. Provider branding never substitutes for health evidence.
- Daemon/update rows show lease epoch, accepting/draining/stopped state, in-flight counts, durable progress/receipt, takeover/recovery, and safe retry; process exit alone never renders upgrade success.
- Hook/hint/tool opportunity, emitted, adopted, missed, human-corrected, unresolved and terminal-outcome metrics with denominators/horizons.
- Generated Capability Registry for every current use case and MCP/CLI/HTTP/dashboard/skill/hook binding: semantic version, request/result schema, read/mutate/autonomous/confirmation/recovery mode, scope, privacy, cost, local/live/joined evidence, availability/gap, catalog digest, and “open guided action.” Old curation approval/apply names remain operator-only migration evidence, never current help/hints/catalog.
- Storage growth, blob integrity/GC, retention, redaction/privacy, remote freshness, malformed rows.
- Provider/project/domain coverage matrix and direct drill-down to evidence.

### 13.8 Costs

- Tokens, latency, model/provider/tool usage, context/compression, dollar cost, estimated savings, and methodology.
- Preserve `actual`, `tokenized`, `estimated`, `mixed`, unknown model, price source/freshness/offline, recording gate, session ledger, model/day aggregates, and legacy lifetime counters.
- Every aggregate drills to sessions/Turns/messages/tools/hints/outcomes and declares confidence/missing denominator.

### 13.9 Privacy

- Privacy Observatory consumes `PrivacyProtectionStatusV1`: configured policy, effective non-disableable floor, source/sink/detector coverage and versions, last verified scan, sanitized/quarantined/legacy-unscanned/unknown counts, and restore eligibility. It never derives “enabled” from historical lossy rows.
- The primary artifact is a source × sink × privacy-domain coverage matrix synchronized with safe finding-class/state counts and descendant remediation lineage. Unknown/locked/corrupt/skipped coverage remains visible and prevents a clean claim.
- Findings show opaque ID, broad class/confidence/state, safe source/sink class, age, remediation/rotation state, and legal actions. No candidate, substring, length, plaintext hash, exact span, secret fingerprint, or raw field path is rendered.
- Scan/remediation/quarantine actions use generated preview/apply commands and elevated authorization. Rotation/revocation is presented before deletion; restore remains blocked until isolated scan/rebuild/promotion receipts pass.
- The named current gaps—Hermes projection-only ingest, duplicated full-command hook analytics, unscanned bounded MCP failures/summaries, direct response-handle/backup copies, raw unauthenticated dashboard exposure, memory metadata/V11 vectors, and false status inference—each have an inspectable safe regression row.

## 14. Replay labs and Evolution Studio

`LabWorkbench` owns immutable input selection, mode/fidelity banner, version pickers, A/B setup, run/cancel, input/output manifests, decision/explanation tree, diff, coverage/substitutions, export, and separate fixture-promotion command. Its `SideEffectGuard` shows `read-only` from a server capability; UI labeling is not the enforcement mechanism.

| Lab | Input panels | Required output panels |
|---|---|---|
| Hint | historical event/session position or sanitized synthetic event; host/provider; project/ref/snapshot; deterministic/scout engine, policy/config/index/memory/tool/catalog/model capability | normalization, trigger/context delta, hypotheses, approved bounded reads/anchors, model/deterministic candidates, suppression/dedupe/cooldown/escalation/budget, exact addressed envelope/payload, delivery timing, tokens/latency/cost, adoption/outcome |
| Retrieval | query/scope, memory/index/model/ranking versions, candidate snapshot | lexical/entity/vector/recent candidates, exclusions/redaction/dedupe, trust/decay/usage features, final order, coverage, no-counter proof |
| Ingest | source bytes/ref, parser/redaction/identity/projector versions | source→observation→events→projection rows, hashes/offsets/idempotency/externalization/quarantine/unresolved identity, version diff |
| Query | visual/source `TraceQueryV1`, scope, watermark, budget, planner/index/ranking | AST, cost, shards, pushdown, operators, rank/merge, cursor, coverage, equivalent CLI/MCP/HTTP |
| Search Quality | historical query/Turn/task anchor or sanitized synthetic query, current/as-of/evolution/forensic mode, corpus/qrel/cutoff, retrieval profiles, channel/model/index/ranker/summary versions | per-channel waterfall, logical-copy representative, summary-DAG horizon, temporal correction/supersession/conflict lineage, shard fusion/diversity/rerank decisions, labels/agreement, per-stratum nDCG/MRR/recall/precision/temporal/duplicate/no-answer/resource regressions, exact final `RetrievalAnchorId`s |
| Scope/Federation | exact locator/selector or historical anchor, registry/catalog/ref/index snapshots, candidate resolver and shard-plan versions | canonical `ScopeSelectorV2`/`ScopeResolutionV2`, candidates/evidence, selected snapshots, pruned/opened/unavailable shards, one-step retry, cross-transport request/result diff; never changes registry |
| Correlation | session/worktree/ref/commit/PR/code candidates and local/live snapshots | evidence windows/events, features, confidence, alternatives, abstention, Git reconciliation, labeled-case promotion |
| Coordination | historical presence/work claims, agents/worktrees/refs/goals, overlap evidence, policy/catalog/dedupe/suppression state | proximity classes/ranking, material-overlap decision, safe summary, one-hint selection or suppression, stable anchor/recipe, legal action simulation, outcome attribution/coverage; never sends or acknowledges |
| Orchestration | initiative/plan/task/attempt/lease/executor/workspace/packet snapshots, policy/config/catalog/model/index versions, explicit time and fault point | decomposition/plan validation, gates/readiness/critical path, route eligibility/fairness/retry, packet ranking/omissions, sibling materiality, claim/lease/fence/cancel/effect reconciliation, requested-vs-actual route/cost/outcome, exact anchors; never claims/spawns/sends/mutates |
| Scheduler | task, effective config, ledger/activity/lease/policy snapshots, explicit time | due/skip/block tree, config source, watermark, proposed lease/work/effects, revalidation requirements |
| Memory | candidate/source, sensitivity/transience, entity/fact/conflict/trust/retrieval/retention/autonomy-config snapshot | auto-apply/auto-reject/defer/quarantine/protect/no-change, duplicate/conflict/supersession, trust/retrieval/deletion descendant effects, explanation; never a human decision control |
| Policy Diff | corpus plus two bundles | changed/unchanged/regression/win/unlabeled, case diff, latency/token distributions, affected categories, coverage/digest |
| Privacy | reserved/invalid synthetic canary, parser/detector/policy versions, bounded sink matrix | parse/decode tree, safe detection classes, overlap/marker/receipt, sink eligibility, latency/coverage/version diff; never loads a real candidate or mutates live findings/policy |

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

Every substantial visual checks in a mini-brief at `dashboard/features/<feature>/visual-brief.md` containing analytical question, data grain, exact/sampled semantics, encoding, selection, keyboard/touch behavior, mobile continuation, URL state, synchronized fallback, export scene, benchmark fixture, and accepted desktop/mobile concept reference.

### 15.1 Renderer choice matrix

| Analytical artifact | Primary implementation | DOM/mark budget | Fallback and export |
|---|---|---|---|
| Brain/topology and large relationship graphs | Sigma.js + Graphology/WebGL | `50k` loaded nodes/`200k` edges benchmark; interactive/labeled subset bounded by legibility | searchable outline, relationship table, adjacency matrix; deterministic SVG/table export |
| Workflow/provenance/Turn DAG | ELK worker + Canvas with DOM labels under budget | `< 2k` visible marks, otherwise collapsed groups | ordered relationship/evidence list; deterministic SVG |
| Causal Loom | Canvas density/marks + virtualized DOM transcript | `250k` density marks benchmark; sanitized native/canonical events requested in bounded pages | chronological table/transcript; fixed-viewport Canvas or table export |
| Time series, bars, heatmaps, distributions | ECharts with custom semantic theme | aggregate bins only | generated directly labeled table; SVG/PNG export |
| Dense dependency/coverage matrix | Canvas matrix with accessible row/column controls | viewport tiles, no unbounded cells | sorted relationship/status table; PNG/SVG/table export |
| Source/message/diff | CodeMirror 6 with virtualized payload slices | bounded lines/bytes per page | semantic preformatted text/download under authorization |
| Small precise lists/trees | DOM + TanStack Virtual | visible rows + overscan <= 3 viewports | same semantic DOM is fallback/export |

Do not create a graph when a ranked list or matrix answers the question more precisely. Do not create a chart for one scalar. The user can switch any graph to outline/table, any chart to exact table, and any timeline to transcript.

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
| How do automations execute? | scheduler swimlane, run waterfall, artifact/proposal lineage | run phase→actor/tool/artifact/decision | run/artifact table |
| How do skills/memory improve? | Evolution evidence→proposal→version→use→outcome DAG plus effectiveness trends | version/use/outcome→source Turns/evals | lifecycle/version ledger |
| Are hints/tools useful? | eligible→suggested→delivered→used→terminal funnel, category matrix, unresolved-horizon survival line | stage/category→evaluation/payload/action evidence | exact denominator/outcome table |
| Where do tokens/costs go? | time series, provider/model/tool heatmap, session small multiples | bin/model→Turn/message/tool ledger | exact cost ledger |
| Is context being compressed safely? | source→summary DAG, depth distribution, compression line, missing-payload markers | node→source ranges/payload/decision | LCM node/source table |
| Is data complete and durable? | storage-growth lines, shard/source coverage matrix, lag/disposition histograms | store/shard/source→health/receipts | exact operational table |
| What would an engine decide? | lab decision tree, candidate score waterfall, A/B diff matrix | rule/candidate/diff→input/evidence/version | ordered explanation/result table |

Every visual shares inspector, scope/time/selection, coverage status, export manifest, and direct table pivot. Each chart title states the question and data interval, not a vague noun such as “Insights.”

### 15.3 Stable layout contract

`layout-cache.ts` keys positions by `(snapshotId, queryFingerprint, lens, layoutAlgorithm, layoutVersion, seed)`. Server-provided cluster anchors win; existing nodes keep positions during expansion; new nodes begin at the parent boundary and settle without moving unaffected clusters. A saved camera references the same key and is discarded with an explicit notice on incompatible layout version.

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
- Canvas/WebGL exposes a synchronized semantic outline with selected item, visible/hidden counts, relations, and viewport summary in an `aria-live="polite"` region.
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
| Offline | last-known-good snapshot, commands disabled | retry when online; export cached nonsensitive metadata if allowed |
| Locked | metadata/coverage only, no payload/search leak | unlock profile/store |
| Redacted | redaction class/reason and count without hidden IDs/content | request authorized view if policy permits |
| Incompatible | client/server/schema versions and supported recovery | restart/update/open current route; never stale-name fallback |
| Query budget/deadline | partial results plus operator/cost/truncation | narrow scope, raise authorized budget |
| Fatal renderer | preserve table/outline and state | restart renderer, report diagnostics |
| Fatal route/API | stable error code/request ID, no secret detail | retry, diagnostics, navigate back |

The first-scan Brain claim is suppressed when coverage is insufficient to support it; the UI instead selects the coverage issue. Cached content disappears immediately when retention/access events invalidate it.

### 17.1 Privacy and security

- Use no third-party analytics, CDN, external font, telemetry pixel, or remote visualization service.
- Reject non-loopback launch/bind configuration in the first V2 default. Browser bootstrap exchanges a one-time launch nonce for an `HttpOnly`, `SameSite=Strict` session; the nonce never persists in URL/history/storage/logs.
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

Interactive Canvas/WebGL state is never screenshot directly as the only export. `packages/renderers/src/export-scene.ts` builds a separate frozen scene from:

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
5. fails on unapproved visible copy, generic substituted icons, clipped content, overflow, unreadable chart text, empty Canvas, or concept drift.

A mismatch is **material** — and must be fixed or explicitly waived with rationale in the fidelity ledger — when it changes copy text, changes a semantic color/mark meaning from the section 2.3 ledger, adds or removes a UI element, breaks the stated screen anatomy (section 2.2), or shifts spacing/alignment by more than `8 px` at `1440×1000`. Anything below those thresholds is recordable but non-blocking; the reviewer applies the threshold list, not taste.

Manual browser QA uses the in-app Browser first. Playwright Chromium/WebKit/Firefox supplies repeatable CI and mobile emulation, not visual taste approval.

## 19. Performance budgets and degradation

Record reference machine, corpus manifest, build mode, browser/GPU, viewport, and five-run median/p95. Every latency/FPS/heap gate runs against the pinned fixture corpus manifest at `dashboard/tests/performance/corpus-manifest.json` (388,000+ messages, 36,000+ code-graph nodes, 71,000+ edges); changing the corpus requires re-baselining every budget in the same PR, so the gates cannot drift silently release to release.

| Budget | Gate |
|---|---|
| Initial shell JS/CSS | `<= 250 KiB` gzip JS; `<= 80 KiB` gzip CSS |
| Localhost first contentful paint | `<= 1.5 s` on the pinned corpus manifest |
| First useful evidence | `<= 2 s` to the route's `first-evidence` performance mark: the first committed data row/mark painted from a non-cache response; each route registers exactly one such mark, asserted in its e2e test |
| Graph/timeline render-ready | `<= 3 s` on the pinned corpus manifest |
| Local interaction response | `<= 100 ms` excluding fetch |
| Main-thread long task | none `> 50 ms` (PerformanceObserver `longtask` entries) during the eight scripted section 22.2 tasks, which define the primary workflows |
| Worker progressive layout | first stable partial `<= 500 ms` |
| Graph | `>= 55 FPS` at 50k loaded nodes/200k edges rendered at `aggregate` LOD, and `>= 55 FPS` at `neighborhood` LOD with 2k visible marks; the benchmark records the LOD level each run passed at |
| Timeline | `>= 55 FPS` at 250k density marks; native/canonical event hydration bounded by `EventPageV1` (section 12.5: `<= 500` events and `<= 1 MiB` per page, one page prefetch each direction) |
| Default response payload | `<= 1 MiB`; page/stream larger authorized payloads |
| Mobile route heap | `<= 300 MiB` JS heap measured via CDP `Performance.getMetrics` `JSHeapUsedSize` under Playwright 390×844 emulation, sampled 5 s after render-ready, median of five runs; hidden routes stop work |
| Route lazy chunk | per-route budget recorded in a committed budget file; CI fails any chunk `> 10%` over its recorded budget unless the same PR updates the budget entry with a linked justification — an unamended budget file is the definition of "unexplained" |

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
| LCM payload health/GC | Observatory | externalized bytes/counts, reclaimable/orphans/missing/unresolved payload references/tombstones/last outcome, dry-run token preview/apply/audit |
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

- [ ] Generate the six concept/state reference sets in section 2.1 from the fixed information architecture; reject until copy, density, graph/timeline anatomy, mobile continuation, and state semantics are legible.
- [ ] Record exact tokens, typography, component/container families, icons/marks, allowed copy, viewport composition, responsive continuation, and motion in `extraction-ledger.md`.
- [ ] Write Playwright tests that expect the Brain claim, central bounded topology driven by real V1 aggregate data, activity rail, health strip, inspector selection, keyboard outline, and mobile sheets. Expected before implementation: route/locators fail.
- [ ] Implement a feature-flagged, read-only workbench using existing V1 APIs with explicit unavailable/partial joins; it must not pretend project-scoped APIs are All data.
- [ ] Compare browser screenshots to concepts with `view_image`, fix every material mismatch per the section 18 threshold list, and record the fidelity ledger.
- [ ] Run `cd dashboard && npm test && npm run build && npx playwright test tests/visual/brain-concept.spec.ts tests/accessibility/brain-concept.spec.ts`. Expected: pass.
- [ ] Commit: `docs(ui): lock Brain workbench product contract` for design/reference artifacts and `feat(ui): prototype V1-backed Brain workbench` for the guarded prototype.

### Task 2: PR 24D/25A — Generated client, bundler ADR, and application foundation

**Files:**
- Create: `docs/adr/dashboard-v2-bundler.md`
- Create: package tree under `dashboard/packages/api-client/`, `contracts/`, `data-client/`, and `testing/`
- Create: `dashboard/app/src/{main,app,router,routes,providers,error-boundary}.tsx`
- Modify: `dashboard/{package.json,package-lock.json,build.mjs,tsconfig.json}`
- Modify: selected bundler config, `build.rs`, `src/dashboard/assets.rs`, `src/dashboard/mod.rs`, `Cargo.toml`
- Test: `dashboard/tests/contract/{generated-drift,asset-manifest,history-fallback}.test.ts`
- Test: `tests/dashboard_api_test/api.rs`

- [ ] Benchmark Rsbuild and Vite with the exact matrix in section 6 and land measurements/decision/rollback in the ADR as its own PR (PR 24D) that merges before any bundler config file or implementation commit in this task.
- [ ] Write failing generated-client drift, content-hashed asset manifest, CSP, base-path, lazy-chunk, history-fallback, `/api` non-fallback, two-clean-build determinism, and packaged-asset tests.
- [ ] Run `cargo run -p tracedecay-api --bin generate-openapi -- --check`, then the root client workspace's own generate/test commands for `packages/tracedecay-client` (that workspace's toolchain, per the section 5 package-manager rule — the dashboard itself remains npm-only), and the dashboard browser-binding tests; expose only official typed HTTP/problem/SSE methods plus UI-safe contract aliases.
- [ ] Implement one React root, router, providers, route-lazy error boundary, selected bundler, asset manifest, and Axum history fallback.
- [ ] Preserve old shell/plugins only under the migration feature flag while parity work is active; direct old/new URLs work in that mode, and a cutover fixture proves old live routes/names stop resolving afterward.
- [ ] Run `cd dashboard && npm ci && npm test && npm run build`; run `cargo test --test dashboard_api_test`; run `cargo package --allow-dirty --no-verify` followed by the repository package verification command. Expected: pass and no second-build diff.
- [ ] Commit separately: `docs(adr): select dashboard V2 bundler` (PR 24D, first) and `build(dashboard): establish generated V2 application shell` (PR 25A).

### Task 3: PR 25B — Investigation state, shell, persistence, and design system

**Files:**
- Create: `dashboard/packages/query-state/src/*`
- Create: `dashboard/packages/design-system/src/*`
- Create: `dashboard/app/src/shell/*`
- Create: `dashboard/app/src/migration-paths.ts`
- Test: `dashboard/tests/component/{investigation-state,command-bar,inspector-dock,mobile-sheets}.vitest.tsx`
- Test: `dashboard/tests/e2e/{url-history,saved-state,migration-paths}.spec.ts`

- [ ] Write failing explicit-All default, no-cwd/last-project narrowing, repository/project/worktree/ref canonical URL, same-name disambiguated candidates, one-step retry preserving request, CLI/MCP/API parity, protected literal exclusion, back/forward, panel persistence, route selection preservation, stable-anchor/recipe recovery after cursor/handle expiry, theme/density, focus restoration, mobile sheet, migration-only legacy path, and post-cutover stale-path failure tests.
- [ ] Implement `InvestigationStateV1`, versioned codecs/store/history, protected drafts, local preferences, and IndexedDB ownership exactly as section 4.
- [ ] Implement accepted tokens/type/icons/controls/open-layout shell and all state primitives without feature data.
- [ ] Implement scope default All, time/live/as-of/compare, query opener, health, save/export, command palette frame, outline/inspector/time brush docks, status line, and mobile sheets.
- [ ] Run `cd dashboard && npm test && npx playwright test tests/e2e/url-history.spec.ts tests/e2e/saved-state.spec.ts tests/e2e/migration-paths.spec.ts`. Expected: pass with zero sensitive literals in URL/history fixtures and typed stale-path failure after cutover.
- [ ] Commit: `feat(dashboard): add shared investigation workbench`.

### Task 4: PR 25C — Universal inspector, cache, SSE, and capability commands

**Files:**
- Create: `dashboard/packages/inspector/src/*`
- Complete: `dashboard/packages/data-client/src/*`
- Test: `dashboard/tests/component/{universal-inspector,coverage-status,command-preview}.vitest.tsx`
- Test: `dashboard/tests/e2e/{sse-reconnect,partial-offline,optimistic-command}.spec.ts`

- [ ] Write failing inspector tab, aggregate membership, relation evidence, native/normalized/history, capability action, destructive preview, optimistic conflict, and SSE state-machine tests.
- [ ] Implement query keys/cache bounds/abort, protected offline cache, subscription creation, idempotent delta reducer, coverage deltas, gap/resync, reconnect/backoff, schema/access invalidation, operation-terminal events, and `/operations/{id}` polling recovery after stream loss.
- [ ] Implement the six inspector tabs and complete/loading/stale/partial/offline/locked/redacted/incompatible/error states.
- [ ] Verify fake SSE duplicates/out-of-order/gaps without sleeps and profile lock clears protected state.
- [ ] Run `cd dashboard && npm test && npx playwright test tests/e2e/sse-reconnect.spec.ts tests/e2e/partial-offline.spec.ts tests/e2e/optimistic-command.spec.ts`. Expected: pass.
- [ ] Commit: `feat(dashboard): connect evidence inspector and live snapshots`.

### Task 5: PR 26A — Shared renderer, LOD, chart, export, and worker foundation

**Files:**
- Create: `dashboard/packages/renderers/src/*`
- Create: `dashboard/packages/{brain,timeline,charts,code-viewer}/src/*` foundations
- Test: `dashboard/tests/component/{renderer-registry,layout-cache,selection-adapter,accessible-chart}.vitest.tsx`
- Test: `dashboard/tests/e2e/{renderer-context-loss,export-scene}.spec.ts`
- Benchmark: `dashboard/tests/performance/{graph,timeline,main-thread}.spec.ts`

- [ ] Write failing stable layout, deterministic worker, expansion position, selection/camera adapter, hidden-route suspension, reduced motion, table fallback, WebGL loss, render-ready, and export-manifest tests.
- [ ] Implement renderer registry/frame, Graphology/Sigma, ELK worker, dense Canvas, matrix, relationship table, chart theme/accessibility, and CodeMirror payload slice primitives.
- [ ] Implement deterministic export scene with fixed fonts/DPR/layout and fallback.
- [ ] Run unit/E2E/performance tests. Expected: stable hashes across two runs, nonblank exports, fallback retains selection, initial route does not load renderer chunks.
- [ ] Commit: `feat(dashboard): add bounded visualization foundation`.

### Task 6: PR 26B — Observatory and non-topology Brain slice

**Files:**
- Create: `dashboard/features/observatory/src/*`
- Create: `dashboard/features/brain/src/{BrainPage,FirstScanClaim,HealthStrip,LearningLoop,ResumeWork}.tsx`
- Create: `dashboard/features/{observatory,brain}/visual-brief.md`
- Test: `dashboard/tests/e2e/{observatory,brain-summary}.spec.ts`

- [ ] Write failing tests for first-scan suppression under partial coverage, federated All/repository/project/worktree/ref scope and per-shard provenance, same-name disambiguation, project × subsystem health drill-down, foreign-owner doctor severity/actions, partial/degraded provider branding, daemon drain/update recovery receipts, hint/tool outcome denominators, storage/privacy/ingest states, complete current generated Capability Registry/guided action, learning loop, and resume.
- [ ] Implement matrix/table/aggregate charts before topology, using exact server read models and inspector pivots.
- [ ] Verify mobile reading order, table parity, offline snapshot, locked store, direct labels, and no equal-weight card grid.
- [ ] Run feature, accessibility, visual, and data-invariant tests. Expected: pass.
- [ ] Commit: `feat(dashboard): ship profile-wide Observatory and Brain summary`.

### Task 7: PR 27 — Universal Explorer

**Files:**
- Create: `dashboard/features/explorer/src/{ExplorerPage,IntentInput,QueryBuilder,TraceQueryEditor,SearchStagePanel,ResultTable,PivotSwitcher,ExplainPanel,BenchmarkPanel,CollectionPanel,ComparePanel}.tsx`
- Create: `dashboard/features/explorer/visual-brief.md`
- Test: `dashboard/tests/e2e/{explorer,query-explain,collections-compare}.spec.ts`

- [ ] Write failing plain-language→visible-AST, builder/raw round-trip, All/repository/project/worktree/ref scope, same-name candidate retry, lexical/phrase/fuzzy/entity/semantic/graph/recency stage, origin/kind filter, grouping/dedupe/native expansion, validation/cost, candidate cap, pagination/cursor, ranking explanation, Rspack/Rsbuild/React Router cross-repo benchmark, pivot, selection, stable recipe, collection, compare, export, and exact CLI/MCP/API request/result parity tests.
- [ ] Implement the three query authoring modes and pivots without client joins or SQL syntax.
- [ ] Add transcript mode/origin facets and hidden-copy counts; prove every sanitized native row remains reachable.
- [ ] Verify partial shards, unknown denominator, explicit candidate/ranking caps, ambiguous message-origin view, stable cursor plus cursor-independent research recipe, privacy-boundary graph frontier, mobile builder, keyboard results, and table/export parity.
- [ ] Run focused/full frontend tests and fixed-corpus user task “find exact historical direct-user prompt, survive typo/role ambiguity, expand its copied/delegated/native set, and prove stable source identity after cursor expiry <=30 seconds.” Expected: pass; embeddings-on must beat or tie embeddings-off within every declared promotion threshold or remain disabled.
- [ ] Commit: `feat(dashboard): add universal evidence explorer`.

### Task 8: PR 28A/28B — Causal Loom density, lanes, transcript, and inspector

**Files:**
- Complete: `dashboard/packages/timeline/src/*`
- Create: `dashboard/features/causal-loom/src/{CausalLoomPage,lane-model,turn-selection}.ts(x)`
- Create: `dashboard/features/causal-loom/visual-brief.md`
- Test: `dashboard/tests/e2e/{loom-density,loom-turn,loom-transcript-modes}.spec.ts`

- [ ] Write failing density exact/sample/hidden/late counts, bounded refinement, stable lanes, consequential event, transcript mode, Turn evidence, virtualized code/diff, and occurred/ingested tests.
- [ ] Implement density brush and lane LOD, then event waterfall and Turn/transcript inspector using frozen snapshot semantics.
- [ ] Ensure routine aggregation never removes counts/export and frozen late events never silently reorder.
- [ ] Verify table/transcript fallback, mobile single lane, keyboard consequential-event traversal, and reduced motion.
- [ ] Run feature/visual/accessibility/performance suites. Expected: pass at 250k density marks.
- [ ] Commit: `feat(dashboard): render bounded causal event lanes`.

### Task 9: PR 28C/28D/28E — Agent follow, causal evidence, impact, as-of, compare, annotation, export

**Files:**
- Create: `dashboard/features/causal-loom/src/{AgentFollow,DelegationTree,CausalChain,ImpactRibbon,AsOfPanel,CompareLoom,AnnotationRange}.tsx`
- Test: `dashboard/tests/e2e/{loom-follow,loom-impact-asof,loom-compare-export}.spec.ts`

- [ ] Write failing parent/subagent/handoff, evidence-class connector, touched-versus-affected, time-machine fidelity, aligned comparison, range annotation, deep-link, and deterministic export tests.
- [ ] Implement one sub-PR per capability group; keep lane order and investigation state stable across them.
- [ ] Prove temporal proximity has no causal arrow; unavailable reasoning/tool catalog/policy/input is explicit.
- [ ] Run fixed-corpus user task “follow parent through subagents and code/test/commit/PR impact <=60 seconds.” Expected: pass.
- [ ] Commit each sub-PR: `feat(timeline): add agent follow and evidence chains`; `feat(timeline): add impact and as-of state`; `feat(timeline): add compare and deterministic export`.

### Task 10: PR 29 — Brain topology and eight graph lenses

**Files:**
- Complete: `dashboard/packages/brain/src/*`
- Create: `dashboard/features/brain/src/BrainTopology.tsx`
- Create: `dashboard/features/graphs/src/{GraphLensPage,lens-registry,git-lens,code-lens,thread-lens,agent-lens,turn-lens,timeline-lens,memory-lens,automation-lens}.ts(x)`
- Test: `dashboard/tests/e2e/{brain-semantic-zoom,graph-lenses,git-drift}.spec.ts`

- [ ] Write failing tile truth-contract, semantic zoom, stable expansion, federated multi-repo/worktree/ref scope, same-name node separation, per-shard stale/partial provenance, lens switch, legal edge vocabulary, cross-lens selection, fallback, dense LOD, and Git local/live drift tests.
- [ ] Implement Brain topology only after PR 26 contracts pass; implement each of the eight base lens registry rows and its mini-brief (Task 10A's `tasks`/`plans` lenses complete the ten-slug union).
- [ ] Add generated Git tool actions and explicit semantic/live evidence requirements to Git inspector/palette.
- [ ] Verify no hairball, aggregate versus evidence edge bundling, mobile focused neighborhood, 50k/200k benchmark, and table/matrix equality.
- [ ] Commit: `feat(dashboard): connect the graph-of-graphs Brain`.

### Task 10A: PR 25G/30I — Canonical Work workspace and advanced task lenses

**Files:**
- Create: `dashboard/features/work/src/**/*`
- Create: `dashboard/features/graphs/src/{tasks-lens,plans-lens}.ts(x)`
- Extend: `dashboard/features/{brain,graphs,causal-loom}/src/*`
- Test: `dashboard/tests/e2e/{work-initiative-plan,work-kanban-dag,work-attempt-packet,work-executor-critical-path,work-notifications}.spec.ts`

- [ ] Write failing one-canonical-ID/count/selection tests across initiative outline, saved Kanban, dependency DAG, critical path, timeline, causal, workload, executor-fleet, repository-work, initiative, agent-relevant, and All projections.
- [ ] Add the `tasks` and `plans` lens-registry rows (`tasks-lens`/`plans-lens`), completing the ten-slug `graphLens` union; extend the section 4 lens-slug/union/enum fixture and prove `/graphs/tasks` and `/graphs/plans` round-trip through URL state.
- [ ] Implement initiative/plan/task/attempt routes and inspectors from generated views/legal capabilities, using the section 4 `InspectorTabV1` Work extension tabs for plan 24 §12.6's task/attempt inspector content; board/query/layout state never becomes task or dispatch authority.
- [ ] Implement `/work/notifications` (plan 24 §12.7): saved filters/channels with event classes, quiet hours, dedupe, and rate budgets via generated preview/apply commands; prove task creation never auto-subscribes a channel.
- [ ] Verify exact Rspack/Rsbuild/React Router scope, Codex/Claude route partitions, fan-out/fan-in gates, context-packet omissions, claim-versus-lease distinction, workspace/ref/snapshot, requested/actual route, and stale-fence/cancellation status.
- [ ] Prove drag/drop maps to a legal versioned command, blocked work cannot be dragged ready, large graphs aggregate server-side, and every visual has table/mobile/keyboard/export parity.
- [ ] Commit separately: `feat(dashboard): add canonical work and plan views`; `feat(dashboard): visualize work across the TraceDecay brain`.

### Task 11: PR 30A/30B/30B2 — Sessions, Agents, and Coordination workspaces

**Files:**
- Create: `dashboard/features/sessions/src/*`
- Create: `dashboard/features/agents/src/*`
- Create: `dashboard/features/coordination/src/*`
- Test: `dashboard/tests/e2e/{sessions,session-raw-canonical,agents,coordination}.spec.ts`

- [ ] Generate failing parity tests for every LCM row in section 20 and every #410 transcript mode/count/provenance behavior.
- [ ] Implement complete session list/detail, Turn graph/outline, source observation/native-row/representative tabs, summary lineage, compression/cost, workflow/goal and code/delivery links.
- [ ] Implement actor/instance topology, agent tree/Turns/delegation/handoff/tools/outcomes/compare plus the first-class goal detail route with native Codex plan/status updates, owning agent/session/workflow, linked Turns, and terminal evidence.
- [ ] Implement expiring presence claims, same/parallel-worktree proximity, direct-versus-temporal overlap, safe summaries, stable anchors/recipes, inspect/message/handoff/ack/suppress previews/receipts, one deduped non-spam hint, analytics, table parity, and Coordination Lab deep links.
- [ ] Verify Claude workflow and Codex goal semantics remain labeled and sanitized native copied-subagent rows remain expandable.
- [ ] Switch LCM reads to current V2 routes only after parity; keep old routes migration-only until compression/payload V2 commands pass, then remove stale live names rather than redirecting/falling back.
- [ ] Commit separate PRs: `feat(dashboard): add Sessions workspace`; `feat(dashboard): add Agents workspace`; `feat(dashboard): add agent coordination workspace`.

### Task 12: PR 30C/30D — Code and Delivery workspaces

**Files:**
- Create: `dashboard/features/code/src/*`
- Create: `dashboard/features/delivery/src/*`
- Test: `dashboard/tests/e2e/{code-workspace,code-diff-impact,delivery-git-reconciliation}.spec.ts`

- [ ] Generate failing Code Graph/Diagnostics parity tests plus snapshot/symbol-lineage/diff/impact/affected-test cases.
- [ ] Implement code views using the code lens, matrix, charts, CodeMirror, exact source locations, and observed/inferred ownership.
- [ ] Implement worktree/ref/commit/PR/check/review/release views with produced/observed/encountered states and local/live reconciliation.
- [ ] Verify drift blocks joined claims and command palette routes Git intent to generated TraceDecay tools.
- [ ] Redirect graph/diagnostic views independently after parity.
- [ ] Commit separate PRs: `feat(dashboard): add Code workspace`; `feat(dashboard): add Delivery workspace`.

### Task 13: PR 30E/30F/30G — Knowledge, Automations/Evolution, and Costs

**Files:**
- Create: `dashboard/features/knowledge/src/*`
- Create: `dashboard/features/{automations,evolution}/src/*`
- Create: `dashboard/features/costs/src/*`
- Test: `dashboard/tests/e2e/{knowledge,automation-skill-lifecycle,evolution,costs}.spec.ts`

- [ ] Generate failing Holographic, Curation, Automation, managed-skills, analytics, and Savings parity tests from section 20, including profile/project declared-scope ownership and cross-project reuse without copied durable state.
- [ ] Implement Knowledge fact/version/entity/provenance/trust/retrieval/similarity/autonomous-curation flows with decision/effect/outcome/recovery history and config/pause/pin/protect/exclude controls, not item commands.
- [ ] Implement schedules/runs/actors/artifacts/candidates/skills and Evolution autonomy lineage/version/use/outcome/replay views.
- [ ] Implement Costs exact tier/methodology/pricing/recording/offline/unknown-model behavior and linked drill-down.
- [ ] Switch each domain to its current V2 route only after read/write parity and rollback drill; remove the old live binding atomically.
- [ ] Commit three reviewable PRs with feature-specific titles.

### Task 13A: PR 25F/30J — Privacy workspace and Context Scout Observatory

**Files:**
- Create: `dashboard/features/privacy/src/*`
- Create: `dashboard/features/hints/src/*`
- Create: `dashboard/features/observatory/src/ContextScoutPage.tsx`
- Create: `dashboard/features/{privacy,hints}/visual-brief.md`
- Test: `dashboard/tests/e2e/{privacy-observatory,context-scout}.spec.ts`

- [ ] Write failing `/privacy` tests: `PrivacyProtectionStatusV1` rendering, source × sink × privacy-domain coverage matrix, unknown/locked/corrupt/skipped coverage blocking a clean claim, finding rows exposing only opaque ID/class/confidence/state (a fixture with candidate/substring/span/fingerprint fields must fail to render), elevated-auth scan/remediation previews, restore blocked until isolated scan/rebuild/promotion receipts, and each named current-gap regression row from section 13.9.
- [ ] Write failing `/observatory/context-scout` tests: trigger/silence/envelope/delivery/outcome funnel with denominators and horizon, queue/model/tool/host state, suppression/dedupe/cooldown evidence, and deep links to Hint Lab replay and `/settings/context-scout`.
- [ ] Implement `features/privacy` per section 13.9 and plan 18 §14.3 semantics, and `features/hints` plus the Observatory scout page per plan 22's Observatory controls, all from generated read models and commands.
- [ ] Verify direct reload/back-forward, mobile sheets, table parity, keyboard/screen-reader paths, and locked/offline/partial states on both routes.
- [ ] Run `cd dashboard && npm test && npx playwright test tests/e2e/privacy-observatory.spec.ts tests/e2e/context-scout.spec.ts`. Expected: pass.
- [ ] Commit separate PRs: `feat(dashboard): add privacy observatory workspace`; `feat(dashboard): add context scout observatory`.

### Task 14: PR 25D/30H — Activity, saved views, and Settings

**Files:**
- Create: `dashboard/features/activity/src/*`
- Create: `dashboard/features/saved-views/src/*`
- Create: `dashboard/features/settings/src/*`
- Test: `dashboard/tests/e2e/{activity,saved-views,settings}.spec.ts`

- [ ] Write failing activity live/frozen/filter/coverage, generated activity-model parity, `SavedViewV1`/`CollectionV1`/`AnnotationV1` round-trip and size-envelope rejection (section 4.3), saved protected-query classification/redaction/share-preview/apply/revoke/expiry, URL restore, declared-owner conflict, and full effective-source Settings parity tests (including the `/settings/context-scout` subroute rendering plan 22's scout controls through plan 20's registry forms).
- [ ] Implement cross-domain activity with consequential-event priority, project/domain facets, bounded live paging, inspector, table fallback, and no duplicate hidden counts.
- [ ] Implement saved-view create/update/open/delete plus generated `share_preview`, `share_apply`, and `share_revoke` commands; protected query literals/annotations remain encrypted, published views expire locally, and sharing requires classification/redaction preview.
- [ ] Implement profile/project/integration/automation/storage settings reads and commands with explicit `DeclaredScope`, effective source/default, immutable environment source, validation, optimistic conflict, migration/resync/restart impact, and audit receipt.
- [ ] Verify `/activity`, `/saved/:viewId`, and `/settings` direct reload/back/forward/mobile/offline/locked behavior.
- [ ] Switch Settings to the current V2 route only after read/write parity and rollback drill; remove the old live binding atomically.
- [ ] Commit independently: `feat(dashboard): add cross-domain activity`; `feat(dashboard): add protected saved investigations`; `feat(dashboard): add effective Settings workspace`.

### Task 15: PR 31A–31M — One replay lab per PR

Thirteen slug labs ship as PR 31A–31M in the section 14 table order; Evolution Studio (the fourteenth canonical lab) ships its product workspace in Task 13 and reuses the generated `labs/evolution:inspect`/`labs/evolution:simulate` bindings, so it needs no letter here. This letter mapping follows the section 21 tracking-truth rule.

**Files:**
- Complete: `dashboard/packages/labs/src/*`
- Create: `dashboard/features/playgrounds/src/{HintLab,RetrievalLab,IngestLab,QueryLab,SearchQualityLab,ScopeFederationLab,CorrelationLab,CoordinationLab,OrchestrationLab,SchedulerLab,MemoryLab,PolicyDiffLab,PrivacyLab}.tsx`
- Test: `dashboard/tests/e2e/labs/*.spec.ts`

- [ ] First write shared failing tests for fidelity label, immutable manifest, missing input, substitutions, comparison, cancellation, coverage, read-only proof, redaction, export, and separately authorized fixture promotion.
- [ ] Implement shared `LabWorkbench` and server-enforced side-effect guard.
- [ ] Implement one lab per PR with the exact panels in section 14; Query Lab reuses Explorer AST/editor, Search Quality owns qrel/benchmark evidence, Scope/Federation reuses the generated selector/resolution/shard-plan models, Correlation reuses Git reconciliation, Coordination reuses proximity/overlap models but has no messaging port, Orchestration reuses plan/task/route/lease/packet views against plan 10 §8.5's generated `labs/orchestration:replay` endpoint but has no scheduling/executor/effect port, Memory reuses Knowledge inspector, and Privacy ("Privacy & Secret Safety Lab") accepts synthetic canaries only.
- [ ] Run each lab twice and assert exact mode decision/explanation digest equality; recorded mode does not execute; best-effort lists every substitution.
- [ ] Run Hint Lab fixed-corpus user task “replay then-versus-now and explain exact payload difference <=60 seconds.” Expected: pass.
- [ ] Commit one feature PR per lab; no omnibus labs merge.

### Task 16: PR 32 — Cross-product accessibility, responsive, export, and visual signoff

**Files:**
- Complete: `dashboard/tests/{visual,accessibility,performance}/**/*`
- Complete: `dashboard/design/fidelity-ledger.md`
- Modify: feature/package files only for audited defects

- [ ] Run automated axe (`@axe-core/playwright`, zero serious/critical violations per route) and manual keyboard/screen-reader/contrast/grayscale/color-deficiency/reduced-motion/table-parity audits for every route against a fixed per-route checklist derived from the section 16.1 requirements, on NVDA + Firefox (Windows) and VoiceOver + Safari (macOS and iOS); record pass/fail per checklist row in `dashboard/tests/accessibility/manual-audit.md` — an audit without a completed checklist does not count as passed.
- [ ] Run desktop/laptop/mobile portrait/mobile landscape/200% text zoom fixtures and every sheet/gesture/orientation/focus path in sections 16 and 18.
- [ ] Compare each route screenshot against its accepted concept with `view_image`; fix every reviewable mismatch and close the fidelity ledger.
- [ ] Run deterministic JSON/Markdown/SVG/PNG export twice and compare manifests/hashes; verify WebGL fallback and no hover-only data.
- [ ] Run performance budgets and fixed-corpus user tasks; fix unexplained bundle/chunk/heap/FPS regressions.
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
- Fact/skill/policy/config versions and lifecycle links.
- V1 behavior/action inventory status.

### 22.2 User-task gates on the fixed corpus

Each user task is a scripted Playwright scenario with a fixed step script and deterministic fixtures — not a human trial. Timing is measured from navigation start to the scenario's final assertion pass on the recorded reference machine, median of five runs. These eight scenarios define the "primary workflows" referenced by the section 19 long-task budget. "Survive typo/role ambiguity" is concrete: the script submits a fixed misspelled query and an ambiguous role facet and passes only if the disambiguation flow completes to the target result without a dead end or manual query retyping.

- Find an exact historical direct-user prompt, expand its copied/delegated/native set, and prove sanitized source identity/export in `<= 30 s`.
- Follow a parent agent through subagents and direct code/test/commit/PR impact in `<= 60 s`.
- Inspect an inferred relation and find its evidence/confidence/algorithm in `<= 30 s`.
- Replay one hint then-versus-now and explain the exact payload difference in `<= 60 s`.
- Compare two sessions and export complete evidence with coverage/caveats in `<= 90 s`.
- Find why a managed skill or memory version changed, the actor/run that changed it, validation, uses, outcomes, and any autonomous supersession/recovery in `<= 90 s`.
- Find an active nearby agent in a parallel worktree, prove direct overlap, inspect the safe summary/anchor, send or suppress one audited coordination action, and verify no repeat hint in `<= 60 s`.
- Start at All, disambiguate same-name Rspack/Rsbuild/React Router scope candidates, traverse a cross-repo graph/search result, and export equivalent CLI/MCP/API retrieval recipes with matching provenance in `<= 60 s`.

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
- All/repository/project/worktree/ref scopes are explicit and ambiguity-safe — the section 22.2 scope task and its typo/ambiguity script complete without dead ends — and semantically identical across dashboard/CLI/MCP/API; federated Rspack/Rsbuild/React Router fixtures retain same-name disambiguation and per-shard provenance/stale/partial state.
- Git, code, thread, agent, Turn, timeline, memory, and automation/skill graph lenses preserve distinct semantics and coordinated state.
- Causal Loom follows agents/sessions/Turns through context, visible reasoning, tools, code, tests, Git/delivery, hints/memory, goals, and outcomes with evidence-class connectors.
- Claude workflows, Codex goals, and Hermes-style curator/reflector/skill-writer actors are captured and visible as typed related entities.
- Evolution Studio makes skill, memory, policy, and automation evolution inspectable from evidence through autonomous version/use/outcome/supersession/recovery.
- Agent coordination exposes expiring evidence-backed same/parallel-worktree proximity, direct overlap, safe summaries, stable anchors, audited actions, one deduped hint, and read-only historical replay without claiming presence from silence.
- Every lab exposes exact/recorded/best-effort fidelity and cannot mutate live state by default.
- #410 native/representative/human-best-effort/direct-user/delegated-agent/tool-result/provider-protocol modes, counts, provenance, and copy membership are explicit; no record is silently omitted.
- Every V1 read, filter, state, action, capability, route, and error behavior is inventoried and parity-proven or documented as an approved semantic change before retirement.
- Every view has loading/empty/stale/partial/offline/locked/redacted/incompatible/error behavior, table/outline parity, desktop/mobile behavior, keyboard/screen-reader support, reduced motion, and deterministic export.
- Initial bundle, response, render, FPS, main-thread, heap, and user-task budgets pass on the recorded corpus/reference machine.
- Approved concepts and final browser screenshots pass `view_image` fidelity review with no material mismatch, as defined by the section 18 threshold list (copy, semantic color/mark, element add/remove, anatomy, `> 8 px` spacing at `1440×1000`).
- Legacy plugins retire independently after bounded migration/rollback receipts close; no stale live name/fallback survives V2 default, and no frontend cutover deletes user data or backend evidence.
- No production frontend file exceeds `800` lines and no route/application component becomes a feature, data, and renderer hairball.

## 24. Plan self-review checklist

- [ ] Master-plan routes, Brain, Explorer, Loom, domain workspaces, labs, visualization, privacy, parity, performance, and deletion are each mapped to tasks/tests.
- [ ] Merged #405/#407/#410/#411/#412/#413/#414/#415/#416/#417/#419/#420/#422/#423/#424 semantics, open #418 input, and closed #409 history are reflected in identity/profile, message/fact views and ranking explanations, denominator-safe exact analytics, doctor authority, race-safe move-symbol parity, daemon/proxy/update recovery, generation-scoped inventory refresh, release state, and stale-client behavior.
- [ ] Generated application/API/hook/tool-catalog contracts are consumed without browser-owned business logic.
- [ ] Every graph lens names legal nodes/edges/layout/fallback/evidence.
- [ ] Agent proximity/coordination preserves expiring claim evidence, same/parallel-worktree semantics, safe summary/anchor/recipe, audited actions, one-hint dedupe, Coordination Lab, and analytics.
- [ ] Explorer exposes lexical/phrase/fuzzy/entity/semantic/graph/recency stages, origin/kind filters, grouping/dedupe/native expansion, Query Explain caps, and per-slice benchmark gates without assuming embeddings help.
- [ ] Every V1 mutation has preview/command/audit/parity ownership.
- [ ] URL/local/IndexedDB/encrypted storage ownership excludes sensitive literals from unsafe locations.
- [ ] SSE gap/resync/offline/coverage semantics preserve last-known-good evidence.
- [ ] Mobile, keyboard, screen reader, reduced motion, table fallback, deterministic export, and visual fidelity start in feature PRs.
- [ ] No incomplete implementation phrase, generic “write tests,” or unowned implementation step remains.
- [ ] Exact file paths, focused commands, expected outcomes, PR boundaries, and deletion gates are present.
