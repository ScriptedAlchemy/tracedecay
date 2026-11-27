# Dashboard outstanding issues implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace Explorer's dashboard-owned query jobs with a cancellable progressive response, make retained dashboard workspaces easier to navigate, decide the Work/Workflow default footprint from complete measurements without losing qualified journeys, and retire the dashboard's PR autotrack settings with L5's #4419 cutover.

**Architecture:** A single admitted HTTP request owns Explorer's three parallel source futures and emits source-completion frames followed by one final frame. The dashboard consumes that response with one abortable reader. Existing routes and renderers remain mounted. Navigation changes reuse canonical channel metadata; Work footprint changes stay at the dashboard activation boundary and consume, rather than redefine, L5's Work scope semantics.

**Tech Stack:** Rust 2024, Axum SSE, Tokio, schemars, Bazel, React, TypeScript, TanStack Query, Vitest, pnpm, browser accessibility tooling.

**Spec:** `docs/superpowers/specs/2026-11-16-outstanding-issues-delivery-design.md`

## Global constraints

- Work in this checkout only. These are future execution steps, not authorization to change branches, run tests, start a daemon, modify issues, or touch operator data while writing this plan.
- Execute #4432, then #4220, then #4223. #4223 can measure early, but cannot merge until L5 #4215 merges; rebase, regenerate any affected contracts, and re-measure after that merge. Coordinate shared generated SDK output with L5.
- Task 4 (#4419 dashboard slice) starts when the coordinator reports L5 Task 4's pushed tip, and it may run while the active task's PR is in review. It is the one cross-lane stack: its PR targets L5's branch, and the coordinator folds it into L5's PR (design, Across lanes). L4 owns every hand-written dashboard path that #4419 changes; L5 owns none.
- **Green and red runs.** A green named Rust run is `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test <one target> --test_output=all --test_arg=<full module path> --test_arg=--exact`. The helper proves only a pass: when the command fails it exits with that status and checks no signature. An expected Rust red invokes Bazel directly, `bazel test <one target> --test_output=all --test_arg=<full module path> --test_arg=--exact`, and counts only when Bazel exits nonzero and the log shows `test <full module path> ... FAILED`, the stated assertion text, and `test result: FAILED. 0 passed; 1 failed`. Vitest runs name evidence through `--reporter=verbose`. A named Vitest red is `pnpm exec vitest run --reporter=verbose <file> -t '<test name>'`, and counts only when it exits nonzero, the verbose log marks that test failed with the stated message, and the summary counts `1 failed` with the file's other tests skipped. A green Vitest run exits 0, the verbose log marks each named test passed, and the summary counts `0 failed`. No substring filter is evidence. Bazel prints a passing test's log only with `--test_output=all`, so every Bazel run whose counts or test names are cited uses that flag.
- Use the exact project gateway for selected-project Explorer reads; no active-project fallback. Preserve source-local ordering and source-specific pages, coverage, freshness, denial, and stale states. Do not merge independent pagination cursors.
- Delete old Explorer registry, polling endpoints, run ID, duplicated finality, and unsupported planning metadata with their last callers. No compatibility route or old executor remains unless a released consumer is proved and the coordinator expressly resolves that conflict with the no-legacy policy. Existing persisted shapes get typed refusal or `ResetRequired`, except the documented released relational-store admission exception; no backups or migration readers.
- The HTTP admission guard currently completes when headers return. A streamed response must own cancellation until the body is dropped; do not rely on the header-phase guard to cancel downstream work.
- Use Bazel for all Rust build, test, check, run, and codegen; use dashboard pnpm commands for its TypeScript checks. Do not use the blocked installed daemon. Production journeys use an isolated project, home/profile, and daemon, only after execution is authorized.
- Regenerate generated dashboard contracts, TypeScript SDK, and Rust SDK descriptors with `cd dashboard && pnpm run contracts:generate`; check with `pnpm run contracts:check`. Never edit generated files directly.
- Retain #4480's Loom Weave/Strata/Rails (Weave default), Brain Neural/Luminous/Points/Atlas (Neural default, project scope), and Code Luminous/Plate/Relief (Luminous default). Keep all Work projections, production data, view controls, selected identities, inspectors, replay/window state, camera/minimap, and failure recovery.
- Commits are proposed per task. Stage only task-owned paths, use conventional headers, and do not claim issue closure before merged `master` has evidence for every criterion. If a requirement needs a path owned by L5, stop and hand the seam to the coordinator.

## Review focus

1. **Stream lifetime under disconnect or supersession:** Task 1's `disconnect_drops_unfinished_explorer_sources` and `superseded_explorer_stream_cannot_replace_new_results` must prove both downstream cancellation and stale-frame exclusion.
2. **Foreign or unresolved project selection:** Task 1's `foreign_project_explorer_stream_is_refused` and `selected_project_stream_does_not_use_active_project` must prove exact isolation.
3. **A fast result beside a stale, partial, or failed source:** Task 1's `fast_explorer_source_precedes_held_source` and `explorer_source_outcomes_remain_distinct` must prove progressive ordering and truthful per-source evidence.
4. **Deep links and assistive interaction after navigation changes:** Task 2's `workspace_navigation_retains_view_deep_links` and `workspace_navigation_supports_keyboard_and_forced_colors` must cover view selection, focus, zoom, and contrast.
5. **Footprint change invalidating pending Work or Remote Brain state:** Task 3's `work_resume_and_remote_brain_survive_activation_choice` must exercise histories, receipts, retry, isolation, and replay after a restart. Its measured no-change branch must cite this same preserved journey, not infer safety from source shape.
6. **Settings saves after the autotrack retirement:** Task 4's `settings_page_offers_no_pr_autotrack_controls` must prove that no control or message names a retired key, that a saved patch carries no `sync` field, and that the remaining project settings still save.

---

### Task 1: #4432 cut over the Explorer API and dashboard together

**Files:**
- Modify: `crates/tracedecay-dashboard-api/src/explorer_api.rs`
- Modify: `crates/tracedecay-dashboard-api/src/explorer_api/lifecycle.rs`
- Modify: `crates/tracedecay-dashboard-api/src/routes.rs`
- Modify: `crates/tracedecay-dashboard-api/src/contract_schema.rs`
- Test: `crates/tracedecay-dashboard-api/src/explorer_api.rs` (existing module tests)
- Test: `crates/tracedecay-dashboard-api/src/authority_tests.rs` (router with selected-project gateway)

**Interfaces consumed:** `RequestControl(DashboardHttpRequestControlV1)`, `DashboardState`, `ExplorerQueryRequestV1 { query, limit, offset }`, `graph_service::search_payload`, existing session and knowledge read adapters, and `DashboardCoverageV1`.

**Interfaces produced:** `pub async fn stream_query(State(state): State<DashboardState>, RequestControl(control): RequestControl, Json(request): Json<ExplorerQueryRequestV1>) -> Response`; `#[serde(tag = "kind", rename_all = "snake_case")] enum ExplorerQueryEventV1 { Source { source: ExplorerSourceProgressV1 }, Final { state: ExplorerRunStateV1, elapsed_micros: i64 } }`. One SSE `data` JSON object per source, then one terminal object; `ExplorerRunStateV1` has only terminal variants (`Completed`, `Partial`, `Cancelled`, `TimedOut`, `Error`). Dropping the response body drops the pending `FuturesUnordered` and cancels the request-owned signal. Admission/invalid-request denial remains an HTTP error before any SSE body.

- [ ] **Step 1: Add `fast_explorer_source_precedes_held_source`** to `mod native_tests` in `explorer_api.rs`: hold one real source read behind a barrier, read the first SSE frame from `POST /api/explorer/queries`, and assert the fast source's rows, coverage, and scope arrive before releasing the held read. It reads the body as text and `serde_json::Value`, so it compiles on the unmodified tree, and its first assertion carries the message `the first explorer frame was not a source frame`.
- [ ] **Step 2: Run directly `bazel test //crates/tracedecay-dashboard-api:unit_test --test_output=all --test_arg=explorer_api::native_tests::fast_explorer_source_precedes_held_source --test_arg=--exact`.** Expected red: Bazel exits nonzero, and the log shows `test explorer_api::native_tests::fast_explorer_source_precedes_held_source ... FAILED`, the text `the first explorer frame was not a source frame`, and `test result: FAILED. 0 passed; 1 failed`. The old POST returns only a pending JSON job, not a source frame.
- [ ] **Step 3: Add `disconnect_drops_unfinished_explorer_sources`** to `mod native_tests` with a held source and a dropped HTTP response body; assert the admitted cancellation signal trips and the held read is dropped without a detached worker.
- [ ] **Step 4: Add `explorer_source_outcomes_remain_distinct`** to `mod native_tests` with one ready-empty source, one stale/partial source, and one unavailable source; assert each emits separate source-local evidence and exactly one final `Partial` frame, never an invented merged rank.
- [ ] **Step 5: Add `foreign_project_explorer_stream_is_refused`** at file level in `authority_tests.rs`, the gateway authority suite; assert a foreign or unresolved selected project cannot emit an active project's rows and a denied request creates no read.
- [ ] **Step 6: Replace `create_query` with `stream_query`** in `explorer_api.rs` and the POST mount in `routes.rs`; poll the existing three source futures inside the SSE response stream, emit completed source frames immediately, and emit one terminal frame on deadline or source completion.
- [ ] **Step 7: Bind cancellation to the stream body** in `explorer_api.rs`: keep the admitted signal in a drop-owned stream state and cancel it on body drop, including client disconnect; remove detached `NativeTask` execution and the `StoredExplorerRun`, `QUERY_RUNS`, and capacity/owner lookup code.
- [ ] **Step 8: Remove GET/DELETE `/api/explorer/queries/{run_id}`** from `routes.rs`; delete `query_status`, `cancel_query`, run ID, duplicate `ExplorerFinalityV1`, offset-free planning labels, and their obsolete lifecycle tests in `explorer_api.rs` and `lifecycle.rs`. Preserve per-source offset/limit only where the underlying read actually paginates.
- [ ] **Step 9: Change `contract_schema.rs`** to export `ExplorerQueryEventV1` in place of `ExplorerQueryRunV1`; retain shared source progress and page types and remove old run-specific schema members.
- [ ] **Step 10: Run each route test through the helper:** `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay-dashboard-api:unit_test --test_output=all --test_arg=explorer_api::native_tests::fast_explorer_source_precedes_held_source --test_arg=--exact`, and the same command for `explorer_api::native_tests::disconnect_drops_unfinished_explorer_sources`, `explorer_api::native_tests::explorer_source_outcomes_remain_distinct`, and `authority_tests::foreign_project_explorer_stream_is_refused`. Expect `1 passed; 0 failed` each: fast frame before barrier release, terminal frame once, dropped-body cancellation, and cross-project refusal. Then run `bazel test //crates/tracedecay-dashboard-api:unit_test --test_output=all`; expect a nonzero passed count and `0 failed`.
**Browser files in the same cutover:**
- Modify: `dashboard/src/workspaces/explorer/controller.ts`
- Modify: `dashboard/src/workspaces/explorer/laneModel.ts`
- Modify: `dashboard/src/workspaces/explorer/absence.ts`
- Modify: `dashboard/src/workspaces/explorer/panels.tsx`
- Modify: `dashboard/src/workspaces/explorer/ExplorerPage.tsx`
- Modify: `dashboard/src/workspaces/explorer/controllerPolling.dom.test.tsx` (replace polling fixtures with streaming fixtures)
- Modify: `dashboard/src/workspaces/explorer/ExplorerPage.dom.test.tsx`
- Modify: `dashboard/src/workspaces/explorer/laneModel.test.ts`
- Modify: `dashboard/src/workspaces/explorer/absence.test.ts`
- Generated by script only: `dashboard/codegen/schemas/dashboard-contracts.schema.json`, `dashboard/src/contracts/generated.ts`, `dashboard/src/contracts/types.ts`, `dashboard/src/contracts/decoders.ts`, `sdks/typescript/src/`, `crates/tracedecay-sdk/src/operations.rs` (only files the canonical generator changes)

**Interfaces consumed:** `ExplorerQueryEventV1Schema` from generated contracts, `scopedUrl(scope, '/api/explorer/queries')`, `ExplorerSourceProgressV1`, `AbortSignal`, and existing `laneFromSourceProgress`.

**Interfaces produced:** `readExplorerStream(scope: DashboardScope, query: string, signal: AbortSignal, onEvent: (event: ExplorerQueryEventV1) => void): Promise<void>` in `controller.ts`; `ExplorerStreamSnapshot { query: string; state: ExplorerRunStateV1 | 'pending'; elapsed_micros: number; sources: readonly ExplorerSourceProgressV1[] }` in `laneModel.ts`. `searchLane(lane: LaneId, snapshot: ExplorerStreamSnapshot | undefined, transport: { state: DashboardDomainStateV1; detail?: string } | undefined, submittedQuery: string, terms: readonly string[]): ExplorerLaneReadModel` replaces the old `EnvelopeResult<ExplorerQueryRunV1>` signature; compare `snapshot.query` to `submittedQuery`. `absenceVerdict(snapshot: ExplorerStreamSnapshot | undefined): AbsenceVerdict` requires `completed` and complete coverage from all three sources before confirming absence. `RunRegister` consumes the same snapshot and transport state, with no run ID, finality, or polling labels; `runStateKind` accepts the snapshot's pending/terminal union.

- [ ] **Step 11: Replace polling tests with `superseded_explorer_stream_cannot_replace_new_results`** in `controllerPolling.dom.test.tsx`: hold the old response reader, submit a second query, release the old frame, and assert the first signal aborts and its rows never replace the second query. Its first assertion checks the request sequence, with the message `controller polled a run id instead of reading the stream`.
- [ ] **Step 12: Run `cd dashboard && pnpm exec vitest run --reporter=verbose src/workspaces/explorer/controllerPolling.dom.test.tsx -t superseded_explorer_stream_cannot_replace_new_results`.** Expected red: nonzero exit, the verbose log marks `superseded_explorer_stream_cannot_replace_new_results` failed with `controller polled a run id instead of reading the stream`, and the summary counts `1 failed`. The controller still POSTs then polls by run ID.
- [ ] **Step 13: Add `selected_project_stream_does_not_use_active_project`** to `ExplorerPage.dom.test.tsx`: answer the selected project's scoped POST with real source rows, and assert the page does not issue an unscoped POST or a GET status request.
- [ ] **Step 14: Add `explorer_partial_source_remains_visible_during_other_read`** to `ExplorerPage.dom.test.tsx`: emit a ready code frame while session/knowledge frames are held, then a stale source and the terminal frame; assert rows, progress, distinct stale/unavailable chips, and query-level final state.
- [ ] **Step 15: Regenerate contracts with `cd dashboard && pnpm run contracts:generate`;** check the diff includes `ExplorerQueryEventV1Schema`, removes the retired run schema, and regenerates SDK descriptors/sources without hand edits.
- [ ] **Step 16: Replace `createPlannerQuery`, `readPlannerQuery`, `cancelPlannerQuery`, and `runStatus`** in `controller.ts` with the one `readExplorerStream` fetch/reader: parse complete SSE frames with the generated event schema, dispatch source frames into the snapshot, and report premature EOF as a transport error, never a successful final state.
- [ ] **Step 17: Own one `AbortController` per submitted query** in `useExplorerController`; abort it on cancel, reset, unmount, scope change, and supersession; reject late frames using the existing request generation before updating a lane.
- [ ] **Step 18: Update `laneModel.ts`, `absence.ts`, `panels.tsx`, and `ExplorerPage.tsx`** to show stream progress, elapsed time, source-local pages, explicit transport failure, and cancellation without run ID/finality or the polling retry ladder; migrate `laneModel.test.ts` and `absence.test.ts` to the new snapshot signature.
- [ ] **Step 19: Run `cd dashboard && pnpm exec vitest run --reporter=verbose src/workspaces/explorer/controllerPolling.dom.test.tsx src/workspaces/explorer/ExplorerPage.dom.test.tsx src/workspaces/explorer/laneModel.test.ts src/workspaces/explorer/absence.test.ts`;** require exit 0, the verbose log marking `superseded_explorer_stream_cannot_replace_new_results`, `selected_project_stream_does_not_use_active_project`, and `explorer_partial_source_remains_visible_during_other_read` passed, `0 failed`, visible early source, truthful stale/empty/error, abort on supersession, no status polling, and no confirmed absence before terminal complete coverage.
- [ ] **Step 20: Run `cd dashboard && pnpm run contracts:check`;** expect no generated drift after the canonical Rust schema change.
- [ ] **Step 21: Run `cd dashboard && pnpm run typecheck`;** expect no TypeScript errors after the contract cutover.
- [ ] **Step 22: Run `cd dashboard && pnpm test`;** expect exit 0 with a nonzero passed count and `0 failed`.
- [ ] **Step 23: Run `cd dashboard && pnpm run build`;** expect an embedded-ready bundle.
- [ ] **Step 24: Exercise isolated dashboard `/explorer` against a real test daemon** with one fast and one delayed source, cancel via the UI, then select another project; observe partial frames before final, a stopped unfinished read, and no cross-project rows. Do not use the installed daemon.
- [ ] **Step 25: Commit the Rust route, browser consumer, tests, and generated outputs together** with `fix(explorer): stream request-owned progressive search`; do not land a server-only intermediate state that breaks the dashboard.

### Task 2: #4220 make retained workspace journeys findable

**Files:**
- Modify: `dashboard/src/app/channels.ts`
- Modify: `dashboard/src/app/shell/NavRail.tsx`
- Modify: `dashboard/src/app/shell/CommandPalette.tsx`
- Modify: `dashboard/src/app/shell/ShellChrome.dom.test.tsx`
- Modify: `dashboard/src/app/shell/CommandPalette.dom.test.tsx`
- Modify: `dashboard/src/workspaces/work/WorkPage.tsx`
- Modify: `dashboard/src/workspaces/workflows/WorkflowsPage.tsx`
- Test: `dashboard/src/workspaces/loom/LoomPage.dom.test.tsx`, `dashboard/src/workspaces/brain/BrainPage.dom.test.tsx`, `dashboard/src/workspaces/code/CodePage.cortex.dom.test.tsx`, `dashboard/src/workspaces/work/views/WorkViews.dom.test.tsx`

**Interfaces consumed:** `CHANNELS: readonly Channel[]`, `scopedWorkspacePath`, route query parameters `loomRenderer`, `field` (including `sigma`), `render`, and Work `view`/`task`.

**Interfaces produced:** Keep `Channel { path: string; label: string; taskHint: string }` as the sole navigation metadata interface. Update its task hints and the existing `NavRail`/`CommandPalette` affordances; do not add a second registry or remove a route or renderer.

- [ ] **Step 1: Add `workspace_navigation_retains_view_deep_links`** to `ShellChrome.dom.test.tsx`; navigate from a selected project via the planned cross-links to Loom, Brain, Code, and Work, then switch their visible renderers. Assert `loomRenderer`, `field=sigma`, `render`, `view`, and `task` select their supported view and entity.
- [ ] **Step 2: Run `cd dashboard && pnpm exec vitest run --reporter=verbose src/app/shell/ShellChrome.dom.test.tsx -t workspace_navigation_retains_view_deep_links`.** Expected red: nonzero exit, the verbose log marks `workspace_navigation_retains_view_deep_links` failed with Testing Library's `Unable to find an accessible element with the role "link"` for the first planned cross-link, and the summary counts `1 failed`. The task-specific cross-links do not exist yet.
- [ ] **Step 3: Add `workspace_navigation_supports_keyboard_and_forced_colors`** to `CommandPalette.dom.test.tsx`; assert keyboard focus/Enter finds and activates all retained workspace names under forced-colors and 200% zoom without losing the selected scope.
- [ ] **Step 4: Record the task-to-surface map in `channels.ts`** by updating task hints: code search/Code, session replay/Loom and Sessions, task tracking/Work, and workflow definitions/Workflows; name the concrete ambiguity between each pair.
- [ ] **Step 5: Update `NavRail.tsx` and `CommandPalette.tsx`** to surface the canonical task hints and direct cross-links through `scopedWorkspacePath`; leave all fourteen route entries, renderer defaults, visible view switches, and Work's shared query owner unchanged.
- [ ] **Step 6: Update `WorkPage.tsx` and `WorkflowsPage.tsx`** with reciprocal, scope-preserving entry links and distinct descriptions of task graph versus definition/run ledger; keep selected-task deep links and all Work projections intact.
- [ ] **Step 7: Run `cd dashboard && pnpm exec vitest run --reporter=verbose src/app/shell/ShellChrome.dom.test.tsx src/app/shell/CommandPalette.dom.test.tsx src/workspaces/loom/LoomPage.dom.test.tsx src/workspaces/brain/BrainPage.dom.test.tsx src/workspaces/code/CodePage.cortex.dom.test.tsx src/workspaces/work/views/WorkViews.dom.test.tsx`;** require exit 0, the verbose log marking `workspace_navigation_retains_view_deep_links` and `workspace_navigation_supports_keyboard_and_forced_colors` passed, and `0 failed` across the affected routes and defaults.
- [ ] **Step 8: Run `cd dashboard && pnpm run typecheck && pnpm test && pnpm run build`;** expect type safety, a nonzero passed count with `0 failed`, and a built dashboard.
- [ ] **Step 9: Inspect the real isolated dashboard visually** at desktop and mobile widths, keyboard-only, 200% zoom, and forced-colors; confirm real data, selection/inspectors, focus, contrast, minimap/replay, and renderer recovery rather than fixture-only mockups.
- [ ] **Step 10: Commit only navigation, cross-link, and test paths** with `fix(dashboard): clarify retained workspace journeys`.

### Task 3: #4223 measure Work/Workflow defaults against the qualified journeys

**Files:**
- Modify if measurement warrants a dashboard activation change: `dashboard/src/workspaces/work/WorkPage.tsx`
- Modify if measurement warrants a dashboard activation change: `dashboard/src/workspaces/work/workViewsQueries.ts`
- Modify if measurement warrants a dashboard activation change: `dashboard/src/workspaces/workflows/WorkflowsPage.tsx`
- Test: `dashboard/src/workspaces/work/WorkPage.dom.test.tsx`
- Test: `dashboard/src/workspaces/work/workQuerySignals.dom.test.tsx`
- Test: `dashboard/src/workspaces/workflows/WorkflowsPage.dom.test.tsx`
- Test: `dashboard/src/workspaces/brain/ScopedBrain.dom.test.tsx`
- Test (existing backend journey): `crates/tracedecay/tests/daemon_suite/advanced_workflow_journey_test.rs`

**Interfaces consumed:** L5 #4215's merged scope/authorization behavior; `WorkPage`, `useWorkGraphViews`, `useWorkAttempts`, `useWorkTopology`, `useWorkflowDefinitions`, and existing scope-selected dashboard routes.

**Interfaces produced:** Only if measured and justified, lazy activation of existing Work secondary reads when their named projection or inspector becomes visible; no new store, scheduler, configuration switch, operation, or generated contract. If no change is justified, produce a complete evidence-backed no-change disposition rather than a speculative code diff.

- [ ] **Step 1: Gate on L5 #4215 merged to `master`**; rebase onto it and identify the exact Work task, attempt, workflow effect, and derived-projection owners. Do not implement backend scope semantics in L4.
- [ ] **Step 2: Measure the baseline on an isolated fixture and stated machine** for the default code-intelligence and qualified Work journeys: record initialization spans, sampled RSS at stable checkpoints, exact dashboard request routes, and operator steps before choosing a redundant read.
- [ ] **Step 3: Add `work_default_does_not_fetch_hidden_secondary_projections`** to `WorkPage.dom.test.tsx` only if Step 2 observes a redundant default read; assert its actual route, not a hard-coded request count, with the message `default Work view fetched <route>` naming that route.
- [ ] **Step 4: Run `cd dashboard && pnpm exec vitest run --reporter=verbose src/workspaces/work/WorkPage.dom.test.tsx -t work_default_does_not_fetch_hidden_secondary_projections`.** Expected red: nonzero exit, the verbose log marks the test failed with `default Work view fetched` and the Step 2 route, and the summary counts `1 failed`. If Step 2 found none, record the observed route trace instead of inventing a failing test.
- [ ] **Step 5: Complete the baseline journey on that isolated fixture** for propose/admit/place/parallel-attempt/pause-restart-resume/synthesize and Remote Brain isolation/replay; record canonical task/attempt/effect owners, request counts, receipts, and user steps.
- [ ] **Step 6: Add `work_resume_and_remote_brain_survive_activation_choice`** to `WorkPage.dom.test.tsx`: navigate scoped Work and Remote Brain views over returned histories and pending attempts, then assert saved selections, refused stale/foreign writes, and receipt/replay presentation across a remount.
- [ ] **Step 7: Run the existing advanced workflow journey through the helper:** `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay:daemon_suite --test_output=all --test_arg=advanced_workflow_journey_test::mounted_fan_out_recovers_then_synthesizes_and_hands_off --test_arg=--exact`; expect `1 passed; 0 failed`, covering its real restart, retry, and durable receipt behavior. Then run `bazel test //crates/tracedecay:daemon_suite --test_output=all`; expect a nonzero passed count and `0 failed`.
- [ ] **Step 8: If a secondary read is measured as redundant, defer only that read** in `WorkPage.tsx` and `workViewsQueries.ts` until its projection is active; keep the Board graph, all eight projection controls, canonical Work query owner, and exact selected task. If there is no measured redundant read, make no production edit.
- [ ] **Step 9: If workflow initialization is measured as redundant, defer only the non-visible workflow detail read** in `WorkflowsPage.tsx` until a definition or run is selected; do not defer the mounted definition registry or qualified run controls. If there is no measured redundant read, make no production edit.
- [ ] **Step 10: Run `cd dashboard && pnpm exec vitest run --reporter=verbose src/workspaces/work/WorkPage.dom.test.tsx src/workspaces/work/workQuerySignals.dom.test.tsx src/workspaces/workflows/WorkflowsPage.dom.test.tsx src/workspaces/brain/ScopedBrain.dom.test.tsx`;** require exit 0, the verbose log marking `work_resume_and_remote_brain_survive_activation_choice` (and `work_default_does_not_fetch_hidden_secondary_projections` if Step 3 added it) passed, `0 failed`, and preserved refusal, pending, resume, deep-link, and remote isolation states.
- [ ] **Step 11: Run `cd dashboard && pnpm run typecheck`;** expect clean types after any activation change.
- [ ] **Step 12: Run `cd dashboard && pnpm test`;** expect exit 0 with a nonzero passed count and `0 failed`.
- [ ] **Step 13: Run `cd dashboard && pnpm run build`;** expect a complete dashboard bundle for the chosen branch.
- [ ] **Step 14: Re-measure both journeys after the chosen change (or against the unchanged baseline)** on the same fixture/machine and report before/after span time, sampled RSS, request counts, operator steps, the retained user jobs, and why any measured cost does or does not justify a changed default.
- [ ] **Step 15: Inspect the real isolated dashboard visually** for Work's Board/DAG/Matrix/Timeline/Causal/Workload/Topology/Concurrent Attempts, Workflows, and project-scoped Remote Brain at desktop, keyboard, zoom, and forced-colors; confirm all data/selection/recovery surfaces remain real.
- [ ] **Step 16: If code changed, commit only measured activation and tests** with `perf(dashboard): defer unused Work detail reads`; otherwise submit the complete measurement/evaluation and preserved-journey evidence for coordinator-approved completed no-change closure, without an empty commit. If any acceptance evidence is missing, leave #4223 open.

### Task 4: #4419 retire the PR autotrack settings from the dashboard

This is the dashboard half of L5's #4419 cutover. L5 Task 4 retires both autotrack settings and regenerates the contracts; this task removes every hand-written dashboard consumer of the retired shapes. L5 edits none of these paths, and this task edits nothing else.

**Start gate:** The coordinator reports L5 Task 4's pushed tip and its Step 15 typecheck error list (L5 plan, Gate G3). Branch from that exact tip, not from `master`.

**Files:**
- Modify: `dashboard/src/workspaces/settings/settingsModel.ts`
- Modify: `dashboard/src/workspaces/settings/settingsRows.ts`
- Modify: `dashboard/src/workspaces/settings/settingsEditorMachine.ts`
- Modify: `dashboard/stories/fixtures/data.ts` (the `sync` autotrack line and `pr_autotrack: { tracked: [] }` in `settingsPayload`)
- Refresh by command only: `dashboard/src/test/fixtures/settings-patch.json`
- Test: `dashboard/src/workspaces/settings/SettingsPage.dom.test.tsx`, `dashboard/src/workspaces/settings/settingsModel.test.ts`, `dashboard/src/workspaces/settings/settingsRows.test.ts`, `dashboard/src/workspaces/settings/settingsMutation.test.ts`

**Interfaces consumed:** L5 Task 4's regenerated contracts. `ProjectSettingsPayloadV1` has no `pr_autotrack`, `ProjectEditableSettingsV1` and `ProjectSettingsPatch` have no `sync`, and `SyncSettingsV1`, `SyncSettingsPatch`, `PrAutoTrackPayloadV1`, and `PrAutoTrackEntryV1` no longer exist. The captured settings wire fixture comes from `runtime_surface_acceptance::dashboard_project_settings_commit_through_the_daemon_control_plane`, which writes `settings-patch.json` to Bazel's undeclared test outputs.

**Interfaces produced:** None. The settings editor has no PR autotrack rows, form values, patch fields, equality terms, or poll-interval validation, and the local `MIN_AUTO_TRACK_PR_POLL_SECS` in `settingsModel.ts` is deleted.

- [ ] **Step 1: Confirm the hand-off** by running `cd dashboard && pnpm run typecheck` on the branch; expect exactly L5's Step 15 error list, all in this task's paths. Return any other error to L5 through the coordinator.
- [ ] **Step 2: Add `settings_page_offers_no_pr_autotrack_controls`** to `SettingsPage.dom.test.tsx`: render the page over the served fixture, assert that no row, control, or validation message names `auto_track_pr_branches`, `auto_track_pr_poll_secs`, or `pr_autotrack`, edit one remaining project setting, and assert the saved patch has no `sync` field. Remove the three autotrack keys from `FIXTURE_SETTINGS` and drop the poll-interval validation assertions.
- [ ] **Step 3: Run** `cd dashboard && pnpm exec vitest run --reporter=verbose src/workspaces/settings/SettingsPage.dom.test.tsx -t settings_page_offers_no_pr_autotrack_controls`. Expected red: nonzero exit, the verbose log marks `settings_page_offers_no_pr_autotrack_controls` failed on its first assertion, which names `auto_track_pr_branches`, and the summary counts `1 failed`. The editor still reads the removed `sync` and `pr_autotrack` fields.
- [ ] **Step 4: Remove the autotrack consumers**: form values, `MIN_AUTO_TRACK_PR_POLL_SECS`, patch building, and validation in `settingsModel.ts`; both row definitions and their editor cases in `settingsRows.ts`; and the equality terms in `settingsEditorMachine.ts`. Migrate `settingsModel.test.ts`, `settingsRows.test.ts`, and `settingsMutation.test.ts` to the remaining settings, and delete assertions whose only subject was an autotrack setting.
- [ ] **Step 5: Update the story fixture** by deleting `sync: { auto_track_pr_branches: true, auto_track_pr_poll_secs: 120 }` and `pr_autotrack: { tracked: [] }` from `settingsPayload` in `dashboard/stories/fixtures/data.ts`.
- [ ] **Step 6: Refresh the captured wire fixture.** Run `REQUIRE_EXACT_TEST_COUNT=1 scripts/require-exact-test.sh bazel test //crates/tracedecay:runtime_acceptance_suite --test_output=all --test_arg=runtime_surface_acceptance::dashboard_project_settings_commit_through_the_daemon_control_plane --test_arg=--exact`, then copy `settings-patch.json` from that run's undeclared outputs (`bazel-testlogs/crates/tracedecay/runtime_acceptance_suite/test.outputs/`, inside `outputs.zip` when Bazel zips them) over `dashboard/src/test/fixtures/settings-patch.json`. Never hand-edit the fixture. The diff drops the autotrack fields and also carries the new run's identifiers and digests, which is expected for a real capture.
- [ ] **Step 7: Run** `cd dashboard && pnpm exec vitest run --reporter=verbose src/workspaces/settings/`; expect exit 0, the verbose log marking `settings_page_offers_no_pr_autotrack_controls` passed, and `0 failed`.
- [ ] **Step 8: Run** `cd dashboard && pnpm run contracts:check && pnpm run typecheck && pnpm test && pnpm run build`; expect exit 0, no generated drift, and a nonzero Vitest passed count with `0 failed`.
- [ ] **Step 9: Inspect the isolated dashboard Settings page** at desktop width, keyboard-only, and 200% zoom, served by an isolated daemon built from this branch. Confirm that the remaining project settings save and that no autotrack control remains. Do not use the installed daemon.
- [ ] **Step 10: Commit** only this task's paths with `feat(dashboard): drop retired PR autotrack settings`. Open the PR against L5 Task 4's branch with `Refs #4419`, and state that it is the Gate G3 child that the coordinator folds into L5's PR. L4 does not merge it. If a later L5 rebase conflicts with this commit, re-cut it on the rebased tip and rerun Steps 7 and 8.
