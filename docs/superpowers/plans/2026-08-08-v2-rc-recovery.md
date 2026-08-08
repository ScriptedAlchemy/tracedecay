# TraceDecay V2 RC Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Convert the interrupted Claude checkout into a fully wired, truthfully typed V2 release-candidate branch with direct production-journey and aggregate test evidence.

**Architecture:** Preserve the canonical Rust domain/application authorities, finish one production vertical at a time, and derive MCP, HTTP, SDK, host, and dashboard surfaces from those authorities. Stabilize replay, identity, security, and typed-state boundaries first; mount runtime producers and consumers next; regenerate wire artifacts only after Rust contracts stop moving.

**Tech Stack:** Rust 2024 workspace, rusqlite, axum/HTTP, MCP, TypeScript SDK, React/rsbuild, Vitest, schemars contract generation, GitHub Actions.

## Global Constraints

- `docs/plans/tracedecay-v2/00-plan-set-index.md` is the sole roadmap and acceptance authority; `NEXT.md` records current outcomes only.
- Preserve the two pre-recovery local feature commits and all peer-owned dirty files until their owning task either integrates or deliberately removes them.
- Complete every cutover in one delivery slice; do not add compatibility aliases for branch-local or unreleased shapes.
- A capability is available only when a production caller can exercise it; otherwise return its existing typed unavailable/unsupported/denied state.
- Add no production `unwrap`, `expect`, `panic`, silent fallback, fabricated timestamp/default, empty success, swallowed error, dead-code allowance, or test-only production port.
- No new hand-written file may exceed 1,000 lines, and a touched oversized file must not grow; extract a cohesive responsibility where safe.
- Canonical Rust schemas generate dashboard and SDK wire types. Never hand-edit `dashboard/src/contracts/generated.ts`.
- Every behavioral change uses RED → GREEN → REFACTOR and records the focused failing and passing commands. Generated output uses generator drift checks instead of hand-written unit tests.
- Named libtest runs must be non-vacuous; use full module paths or `scripts/require-exact-test.sh` and confirm the executed count is non-zero.
- Preserve byte-exact identity, replay, staleness, denial, isolation, cancellation, and rollback contracts; do not weaken assertions or raise timeouts to mask defects.
- Audit all supported host integrations whenever shared host lifecycle behavior changes.
- Workers share one dirty checkout: re-read before editing, own only named files, never revert peer edits, stage explicit paths, and create coherent conventional commits.
- npm OIDC, live operator-host runs, Plan 15 semantic evaluation, Plan 25 cadence re-observation, and Plan 38 large-store observation are external evidence gates, not permission to fabricate repository evidence.
- Manual screen-reader polish is not an RC blocker; functional keyboard/DOM behavior and existing automated accessibility checks remain in scope.

---

### Task 1: Make work synthesis replay atomic and byte-stable

**Files:**
- Modify: `crates/tracedecay-application/src/work_synthesis.rs`
- Modify: `crates/tracedecay-application/src/work_attempt.rs`
- Modify: `crates/tracedecay-application/tests/work_synthesis_service.rs`
- Modify: `crates/tracedecay-rusqlite-runtime/src/work_attempt.rs`
- Modify: `crates/tracedecay-rusqlite-runtime/tests/work_attempt_storage.rs`

**Interfaces:**
- Consumes: existing synthesis admission/source/draft authorities and work-attempt start port.
- Produces: one durably persisted admitted synthesis result that identical replays return byte-for-byte; changed replay material yields the existing typed conflict.

- [ ] **Step 1: Write the failing replay test**

Add a real service/storage test that admits synthesis while one source is `Unknown`, changes that source to `Succeeded`, repeats the identical request, and asserts the second result is byte-identical to the first. Add a second test that changes request identity material and asserts the typed conflict without mutation.

```rust
let first = service.synthesize(request.clone()).await?;
sources.mark_succeeded(source_id).await?;
let replay = service.synthesize(request.clone()).await?;
assert_eq!(serde_json::to_vec(&replay)?, serde_json::to_vec(&first)?);
assert_eq!(store.committed_result_count(run_id).await?, 1);
```

- [ ] **Step 2: Verify RED**

Run the narrow application test with `scripts/require-exact-test.sh`; expect the replay assertion to fail because source/draft data is recomputed.

- [ ] **Step 3: Persist complete admission atomically**

Move replay authority to the durable synthesis record. Store request identity plus the complete returned result in the same transaction that creates the admitted attempt. On duplicate identity return the stored result; on mismatched identity return a typed conflict. Do not re-read mutable sources during replay.

- [ ] **Step 4: Verify GREEN and regressions**

Run the focused application test, the selected rusqlite adapter tests, and all work-synthesis tests; require non-zero passing counts and pristine output.

- [ ] **Step 5: Commit**

Commit only the synthesis service, port/adapter, and tests as `fix(work): make synthesis replay byte-stable`.

### Task 2: Bind run admission and provider execution to admitted authority

**Files:**
- Modify: `crates/tracedecay-rusqlite-runtime/src/work_run_control.rs`
- Modify: `crates/tracedecay-rusqlite-runtime/tests/work_run_control_storage.rs`
- Modify: `crates/tracedecay-domain/src/work_execution_snapshot.rs`
- Modify: `crates/tracedecay-application/src/work_attempt.rs`
- Modify: `crates/tracedecay-application/tests/work_attempt_service.rs`
- Modify: `src/daemon/service/invocation/work.rs`
- Modify: `src/daemon/service/invocation/work_attempt_exec.rs`
- Modify: `src/daemon/service/invocation/work_attempt_exec/tests.rs`

**Interfaces:**
- Consumes: registered workflow/application topology and daemon environment snapshot authorities.
- Produces: first-admission run deadline/topology persistence, mismatch refusal for every later attempt, and an environment-cleared provider child restored only from admitted variables.

- [ ] **Step 1: Write three failing tests**

Add literal behavior tests for: attempt `attempt-2` admitted with deadline D1 followed by `attempt-10` with D2 returns typed conflict and leaves D1 intact; caller-supplied topology that differs from registered topology is refused before provider launch; and a fake child observes an admitted sentinel while an ambient secret is absent.

```rust
assert_eq!(admit("attempt-2", d1)?.run_deadline, d1);
assert!(matches!(admit("attempt-10", d2), Err(RunControlError::AdmissionConflict { .. })));
assert_eq!(read_run(run_id)?.deadline, d1);
```

- [ ] **Step 2: Verify RED**

Run the exact rusqlite run-control test, application attempt test, and daemon child-process test; expect deadline/topology/environment assertions to fail for their intended reasons.

- [ ] **Step 3: Implement durable admission and clean spawning**

Persist run deadline and topology identity on first admission under the existing transaction/CAS boundary. Compare every later admission to those stored values. Resolve topology from registered authority before `WorkAttemptService::start`. Call `env_clear()` for provider children and add only the admitted snapshot plus unavoidable platform process variables explicitly selected by the existing host policy.

- [ ] **Step 4: Verify GREEN**

Run all three focused families, then `cargo check -p tracedecay --lib --tests --all-features`.

- [ ] **Step 5: Commit**

Commit as `fix(work): bind attempts to admitted run authority`.

### Task 3: Replace fabricated operational states with typed truth

**Files:**
- Modify: `src/tracedecay/lifecycle/mod.rs`
- Modify: `src/daemon/project_open_handshake.rs`
- Modify: `src/daemon/core_doctor.rs`
- Modify: `crates/tracedecay-global-db/src/session_temporal/operations/sources.rs`
- Modify: `crates/tracedecay-global-db/src/session_temporal/operations/message_anchor.rs`
- Modify: `crates/tracedecay-global-db/src/session_temporal/retrieval/records/relations.rs`
- Modify: `crates/tracedecay-global-db/src/session_temporal/retrieval/tests/relation_graph_tests.rs`
- Test: lifecycle, Doctor, session-temporal, and graph projection suites

**Interfaces:**
- Consumes: canonical store schema, metadata, owner-store hydration, and generation authorities.
- Produces: typed reset-required/unobserved/unavailable/absent-owner/stale-generation outcomes without synthetic schema, size, timestamp, or anchor values.

- [ ] **Step 1: Write failing boundary tests**

Cover: a nonempty wrong-schema project open returns the reset-required typed state before normal I/O; a route-live Doctor request with failed metadata/integrity observation returns unavailable fields rather than compiled schema or size `0`; missing message ownership and malformed timestamps do not insert a legacy anchor; stale graph generation cannot satisfy a current read.

```rust
assert!(matches!(open_result, Err(ProjectOpenError::ResetRequired { .. })));
assert_eq!(doctor.database.observed_size_bytes, None);
assert!(matches!(source_result, Err(SourceError::OwnerUnavailable { .. })));
```

- [ ] **Step 2: Verify RED**

Run one exact test in each family and confirm each fails on the fabricated current behavior.

- [ ] **Step 3: Route through canonical observations**

Remove defaulting branches. Propagate typed states through daemon/API serialization. Hydrate temporal sources only through each message's owning store and parse authoritative timestamps. Require the read generation to match the canonical current generation.

- [ ] **Step 4: Verify GREEN**

Run the full lifecycle/Doctor/session-temporal/relation-graph focused suites and their crate checks.

- [ ] **Step 5: Commit**

Commit as `fix(runtime): preserve truthful operational states`.

### Task 4: Mount the complete Work MCP surface

**Files:**
- Modify: `src/mcp/tools/definitions.rs`
- Integrate: `src/mcp/tools/definitions/work.rs`
- Modify: `src/mcp/tools/binding.rs`
- Modify: `src/mcp/tools/handlers/mod.rs`
- Create: `src/mcp/tools/handlers/work.rs`
- Modify: `src/application_surface.rs`
- Modify: `crates/tracedecay-application/src/work_catalog.rs`
- Modify: `plugin/README-cursor.md`
- Test: MCP definition, binding, dispatch, API catalog, and live daemon Work tests

**Interfaces:**
- Consumes: canonical `work_executable_binding_registry` and existing application Work dispatcher.
- Produces: exactly 26 advertised Work tools with definitions, lifecycle annotations, bindings, deadlines, dispatch handlers, typed results, and discovery parity; exactly 11 read-only operations including `Topology`.

- [ ] **Step 1: Write failing registry and live-dispatch tests**

Extend existing maximal-registry, every-definition-has-binding, every-binding-has-dispatch, and read-only tests to include the 26 canonical Work operations. Add a live MCP test that invokes one read and one mutation through the daemon and observes the same typed Work result as HTTP.

```rust
assert_eq!(work_definitions.len(), 26);
assert_eq!(work_definitions.iter().filter(|d| d.read_only).count(), 11);
assert_eq!(mcp_result, http_result);
```

- [ ] **Step 2: Verify RED**

Run the exact catalog/definition/binding tests; expect missing module, binding, or dispatch coverage rather than a compile-only failure.

- [ ] **Step 3: Derive Work mounting from the canonical registry**

Register definitions, add a distinct Work dispatch group or reusable Work adapter, and route all tools through the existing application owner. Do not duplicate request/result DTOs or maintain a second operation list.

- [ ] **Step 4: Verify GREEN**

Run all MCP tool-definition/binding/handler tests, API application parity, runtime surface acceptance, and the live Work MCP/HTTP journey.

- [ ] **Step 5: Commit**

Commit as `feat(mcp): mount the canonical Work surface`.

### Task 5: Finish worktree, fan-out, checkpoint, and handoff runtime journeys

**Files:**
- Integrate/refactor: `crates/tracedecay-application/src/worktree_catalog.rs`
- Integrate/refactor: `crates/tracedecay-application/src/worktree_inventory.rs`
- Integrate/refactor: `crates/tracedecay-application/src/worktree_cleanup.rs`
- Integrate/refactor: `crates/tracedecay-application/src/workflow_fan_out.rs`
- Integrate or delete: `crates/tracedecay-domain/src/work_checkpoint.rs`
- Modify: `src/daemon/service/invocation/handoff.rs`
- Modify: `src/daemon/service/invocation/work.rs`
- Modify: `src/application_surface.rs`
- Modify: `crates/tracedecay-application/src/work_catalog.rs`
- Test: application service tests and `tests/work_loop_journey.rs`

**Interfaces:**
- Consumes: registered project/worktree identity, workflow run-control fence, provider placement, handoff token, and durable Work storage.
- Produces: explicit-root inventory/cleanup, fenced bounded fan-out, durable checkpoint retrieval/handoff when justified, and task-token redemption.

- [ ] **Step 1: Split oversized dirty modules before adding behavior**

Extract DTO validation, inventory projection, cleanup planning, and cleanup execution into focused modules so no new hand-written file exceeds 1,000 lines. Replace static-constructor `unwrap`/`expect` with typed initialization results.

- [ ] **Step 2: Write failing real-journey tests**

Add one daemon journey covering explicit roots, present/stale/partial/foreign inventory, cleanup denial, inspect→confirm→remove→reconcile, bounded fan-out where observed concurrent children never exceed `max_parallel`, lease/fence loss, restart/replay, and task handoff redemption. Assert stale+present coverage is partial, not complete.

```rust
assert_eq!(inventory.coverage.completeness, Completeness::Partial);
assert!(max_observed_children <= request.topology.max_parallel);
assert_eq!(redeemed.task_id, issued.task_id);
```

- [ ] **Step 3: Verify RED**

Run exact service tests and the selected work-loop journey; expect unmounted adapter, missing bound, stale coverage, and unavailable handoff failures.

- [ ] **Step 4: Mount canonical adapters**

Implement real filesystem/Git worktree adapters with registered identity, use run-control for fan-out admission and concurrency, validate request fences and placement, and make checkpoint state durable only if the handoff/retrieval journey consumes it. If no production consumer is justified by the V2 authority, delete the checkpoint contract and tests instead of staging dead code.

- [ ] **Step 5: Verify GREEN and commit**

Run application, rusqlite, daemon Work, host handoff, and work-loop journeys. Commit as `feat(work): complete workflow and worktree journeys`.

### Task 6: Emit and project execution topology through one generation

**Files:**
- Refactor/integrate: `crates/tracedecay-application/src/execution_topology_metrics.rs`
- Modify: canonical run/attempt/fan-out/handoff event producers
- Modify: canonical observability persistence/query adapter
- Modify: Work topology HTTP/application catalog route
- Test: application projector and live topology route tests

**Interfaces:**
- Consumes: 11 canonical execution-topology event kinds from actual owner transitions.
- Produces: persisted, scope-isolated, generation-bound metrics/read model with explicit unknown/partial coverage.

- [ ] **Step 1: Split the 2,500-line dirty projector**

Separate event normalization, aggregation, pagination, and public projection into focused modules under 1,000 lines without changing behavior.

- [ ] **Step 2: Write failing emission and read-model tests**

Exercise a real provider run with fan-out/retry/handoff and assert expected event kinds are emitted exactly once, the route rejects a mismatched `scope_ref`, two source generations cannot be joined, and unavailable storage returns a typed unavailable state rather than empty metrics.

- [ ] **Step 3: Verify RED**

Run the exact application projector and daemon route tests; expect no production emission/adapter/route.

- [ ] **Step 4: Mount producers and query authority**

Emit at owner transitions, persist through the canonical observability store, bind one generation into the query envelope and payload, and expose one application/HTTP Work topology read.

- [ ] **Step 5: Verify GREEN and commit**

Run focused projection, route, scope-isolation, and work-loop tests. Commit as `feat(observability): publish execution topology`.

### Task 7: Complete retained context, Context Scout, policy replay, and remote surfaces

**Files:**
- Modify: `crates/tracedecay-application/src/context_scout.rs` and its production executor/bindings
- Modify: retained application services and root MCP/CLI callers
- Modify: `crates/tracedecay-policy/src/replay.rs` and its production authorization caller
- Modify: remote operation catalog, CLI, MCP, Rust SDK, TypeScript SDK schema authority, and dashboard route ownership
- Modify: `crates/tracedecay-application/src/sdk_catalog.rs`
- Modify: `crates/tracedecay-sdk/src/operations.rs` through its generator
- Modify: `sdks/typescript/src/operations.ts` through its generator
- Test: API/MCP/CLI parity plus retained, Scout, policy, remote, handoff, and multi-root journeys

**Interfaces:**
- Consumes: canonical retained-content hydration/redaction, source authorization, remote protocol, and mounted application registries.
- Produces: equivalent typed behavior through CLI/MCP/HTTP/SDK/dashboard and accurate SDK availability for all promised V2 operations.

- [ ] **Step 1: Write failing production-parity tests**

Cover: the same retained fact and temporal message through CLI/MCP/HTTP/dashboard; Context Scout saved-edit→stop→restart/dedupe/overlay; replay authorization exact/recorded/best-effort unavailable input without mutation; remote offline→replay→backup→restore→fenced failover; SDK reachability for Work, Handoff, Multi-root, Scout, retained, and remote operations.

- [ ] **Step 2: Verify RED**

Run each narrow journey and record missing executors/routes or truthful unavailable states.

- [ ] **Step 3: Mount canonical services**

Replace direct root handlers with typed application services, mount Scout through durable runtime receipts, call replay authorization from the production authorization path, and add transport adapters over the existing remote protocol. For each of the 38 currently unavailable SDK entries, either mount the canonical request/result schema and production executor or retain a typed unavailable entry with a specific roadmap-sanctioned reason; do not resurrect superseded legacy aliases.

- [ ] **Step 4: Verify GREEN and commit**

Run application/API parity, MCP, CLI, SDK generation/conformance, retained, Scout, policy, remote, handoff, and multi-root tests. Commit as `feat(application): complete V2 service parity`.

### Task 8: Harden shared host lifecycle, privacy, and LSP advisory behavior

**Files:**
- Modify: shared host CLI runner and `crates/tracedecay-agent-hosts/src/agents/kiro.rs`
- Modify: `crates/tracedecay-agent-hosts/src/agents/kiro/tests.rs`
- Modify: `docs/KIRO-INTEGRATION.md`
- Refactor/modify: `crates/tracedecay-runtime-core/src/privacy/detect.rs`
- Integrate: `crates/tracedecay-runtime-core/src/privacy/structured_text.rs`
- Modify: GitHub/fact/session metadata sinks
- Modify: `crates/tracedecay-lsp/src/capabilities.rs`, `context.rs`, `gateway.rs`, protocol controller, and daemon LSP source
- Test: all-host lifecycle, privacy ingress, and LSP saved-edit journeys

**Interfaces:**
- Consumes: shared declarative host bundle install/update/uninstall contract, structured sanitizer, and daemon projection authority.
- Produces: operator-state isolation and rollback for every host, MCP-only Kiro IDE/CLI registration without a separate CLI or plugin lifecycle, structured metadata privacy, real advisory registrations, and distinct absent/unsupported/denied/scope-denied states.

- [ ] **Step 1: Write failing host tests**

Use an isolated HOME plus an ambient operator `KIRO_HOME`; assert the operator sentinel is unchanged, peer MCP entries survive install/update/uninstall, and failure rolls back byte-for-byte. Assert Kiro global and workspace registrations are rendered by the same shared `mcpServers` merge authority, install/update/uninstall require no `kiro-cli` binary, and no Kiro Power, OpenVSX extension, managed agent, steering, or hook artifact is installed. Apply the shared preservation assertions to Claude, Codex, Cursor, Kimi, OpenCode, and every supported host target.

- [ ] **Step 2: Write failing privacy and LSP tests**

Ingress nested JSON/YAML-like provider metadata containing `vault_passphrase` through real GitHub, fact, and session sinks and assert sanitized/quarantined output. Exercise saved-edit→LSP advisory projection/clear/authorized expansion and assert negotiated-but-denied maps to a denial reason distinct from `CapabilityNotNegotiated`.

- [ ] **Step 3: Verify RED**

Run focused host, privacy, and LSP tests and confirm the expected isolation, leakage, registration, and typed-state failures.

- [ ] **Step 4: Implement shared boundaries**

Replace Kiro's dirty `kiro-cli mcp add/remove` path with the shared declarative JSON bundle merge, backup, rollback, registration-state, and uninstall helpers already used by file-managed hosts. Resolve global and workspace Kiro roots only from the admitted install context, preserve peer configuration, and delete TraceDecay's Kiro Power/extension/managed-agent/steering/hook lifecycle so Kiro-specific code is limited to MCP paths and schema selection. Parse before sanitizing provider metadata, and derive LSP registrations/snapshots from production daemon sources. Extract cohesive privacy/Kiro/LSP responsibilities rather than growing files already above 1,000 lines.

- [ ] **Step 5: Verify GREEN and commit**

Run every supported host's lifecycle/bundle tests, runtime privacy suite, LSP crate tests, and the saved-edit integration journey. Commit as `fix(hosts): preserve isolated V2 integration state`.

### Task 9: Regenerate contracts and mount the final dashboard V2 surface

**Files:**
- Modify generator authority: `crates/tracedecay-dashboard-api/src/contract_schema.rs`
- Generate: `dashboard/codegen/schemas/dashboard-contracts.schema.json`
- Generate: `dashboard/src/contracts/generated.ts`
- Generate: `crates/tracedecay-sdk/src/operations.rs`
- Generate: `sdks/typescript/src/operations.ts`
- Modify: `dashboard/src/test/workAttemptFixture.ts`
- Integrate: recovered Observatory adoption/outcome/retrieval/family-ledger files
- Integrate: Work topology accounting files and routes
- Modify: dashboard navigation/page owners and DOM tests

**Interfaces:**
- Consumes: stable canonical Rust Work/topology/retained/remote schemas and one generation-bound topology route.
- Produces: drift-free generated contracts and reachable dashboard views whose partial/unknown semantics match backend coverage.

- [ ] **Step 1: Add failing dashboard behavior tests**

Add DOM tests that navigate to each recovered Observatory view and Work topology accounting, render real generated fixture shapes, refuse mixed-generation joins, and display a partial/floor label when a denominator is capped.

- [ ] **Step 2: Verify RED**

Run the targeted Vitest files; expect unmounted views, obsolete topology fixture shape, missing generated route/types, and mixed-generation behavior.

- [ ] **Step 3: Export and regenerate canonical artifacts**

Register every mounted dashboard contract including the complete synthesis result and execution topology view. Run `npm run contracts:generate`, SDK generation, and formatters. Update fixtures to embed `topology: WorkTopologyPolicyV1`; never hand-edit generated outputs.

- [ ] **Step 4: Mount views and enforce snapshot semantics**

Route recovered Observatory and Work components through the existing workspace owners, bind every joined query to one generation, and make capped denominators explicitly partial. Keep automated user-visible behavior in scope; do not add manual screen-reader release bureaucracy.

- [ ] **Step 5: Verify GREEN and commit**

Run `npm run contracts:check`, `npm run typecheck`, targeted tests, full `npm test`, and `npm run build` from `dashboard/`; run SDK conformance from the repository root. Commit as `feat(dashboard): complete the V2 product surface`.

### Task 10: Complete architectural cutovers and remove dead scaffolding

**Files:**
- Remove after migration: `src/application.rs`
- Modify: every remaining `crate::application::` caller
- Move root HTTP/SPA ownership into `crates/tracedecay-api` where required by Plan 10
- Refactor touched oversized modules identified by `scripts/check-handwritten-file-size.sh` or repository equivalent
- Remove stale flags, aliases, unmounted declarations, source-shape gates, and docs contradicted by production behavior
- Test: API ownership, SSE resume/terminal, package/bundle, and repository hygiene checks

**Interfaces:**
- Consumes: the mounted services and stable contracts from Tasks 1–9.
- Produces: one application/API authority without root compatibility façades or dead branch-local contracts.

- [ ] **Step 1: Write failing ownership tests**

Exercise API-owned `/`, static assets/cache, API boundary, and SSE resume/terminal behavior. Add behavior tests at the first real consumer for each migrated root caller; do not add source-string scans that merely assert a file or symbol is absent.

- [ ] **Step 2: Verify RED**

Run API/dashboard route tests and confirm ownership remains in the root shim where the plan requires the crate boundary.

- [ ] **Step 3: Migrate callers and delete superseded code**

Move every root caller to explicit application/API crate imports, then delete the shim and branch-local compatibility. Split touched oversized modules by cohesive responsibility, remove unused dependencies with their last caller, and update durable capability documentation.

- [ ] **Step 4: Verify GREEN and commit**

Run API/dashboard/SSE tests, `cargo check --workspace --all-targets --all-features`, dead-code/size/dependency hygiene scripts, and bundle checks. Commit as `refactor(application): complete the V2 cutover`.

### Task 11: Converge all local tests and produce RC evidence

**Files:**
- Modify: only source/tests required to root-cause fresh failures
- Modify: `docs/plans/tracedecay-v2/NEXT.md` with current measured outcomes only
- Create: one current RC evidence document under `docs/reports/`
- Modify: release metadata only if the existing release authority requires a repository-side change

**Interfaces:**
- Consumes: completed Tasks 1–10.
- Produces: zero unclassified repository failures and an exact ledger of external gates without publishing or tagging.

- [ ] **Step 1: Run formatting and generated-drift gates**

Run `cargo fmt --all -- --check`, dashboard contract check, SDK generation/conformance, plugin bundle validation, and release-drift checks. Fix root causes and rerun each failing gate to exit 0.

- [ ] **Step 2: Run compiler and lint gates**

Run `cargo check --workspace --all-targets --all-features` and the repository's CI-equivalent clippy command with warnings denied. Fix every fresh failure without suppressing lints or adding dead-code allowances.

- [ ] **Step 3: Run dashboard and focused production journeys**

Run dashboard typecheck/tests/build; Work/workflow/provider/cancellation/retry/review/replan/no-Git/Git/lease-loss/restart journeys; retained/Scout/remote/LSP/privacy journeys; and every supported host bundle/lifecycle journey. Require direct exit 0 and non-zero test counts.

- [ ] **Step 4: Run aggregate Rust verification**

Run `cargo nextest run --workspace --all-features --no-fail-fast --no-tests=fail` or the exact CI-equivalent shards when platform constraints require it. Classify every failure by root cause, fix it with a failing regression test, and repeat until the local supported matrix is green.

- [ ] **Step 5: Run final review and current CI**

Dispatch a whole-branch semantic review against the integration floor, fix its complete load-bearing finding set in one wave, rerun scoped review, push the branch, and watch the final commit's CI/SDK/plugin workflows to terminal status. Do not reuse CI from an older SHA.

- [ ] **Step 6: Record RC evidence and external gates**

Update `NEXT.md` only with measured current outcomes. Record command, timestamp, commit SHA, executed counts, and exit status for every gate. List npm OIDC, live semantic evaluation, operator host runs, cadence re-observation, and large-store GC separately with no fabricated pass.

- [ ] **Step 7: Commit**

Commit evidence and any final root-cause fixes as coherent conventional commits; do not create a tag, GitHub Release, or package publication without explicit user authorization.
