# V2 implementation audit — 2026-08-14

## Method

- Date: 2026-08-14.
- Audited revision: `f478c323a` (the checker reports were produced across the
  immediately preceding `42acf504b` snapshot; the only intervening commit
  surfaces semantic-evaluation rejection detail and is reflected below).
- Inputs: all 45 Markdown witness reports present in
  `/tmp/v2-impl-check/`: 39 distinct plan shards, one production-mount census,
  and five duplicate-plan witnesses.
- Checker models: GPT-5.6 Luna and GPT-5.6 Terra. Consolidation and conflict
  adjudication: GPT-5.6 Sol.
- Method: merge every non-superseded gap; deduplicate cross-plan themes; require
  a non-test caller for a production mount; adjudicate disputed call edges with
  `tracedecay tool callers` and targeted current-tree reads. No Cargo, npm, or
  journey command was run during consolidation.
- RC authority: `NEXT.md:692-700`. `RC-BLOCKING` means code or a production
  mount/typed state is absent. `RC-REQUIRED-EVIDENCE` means the implementation
  is present but the required current-tree journey or measurement is not.
  `POST-RC` means an active-plan ambition beyond the stated RC bar.

The register excludes explicitly superseded items. In particular, it does not
reopen the Delivery failure-localization owner decision
(`NEXT.md:493-502`), unsafe `commit_index` publication, removed public Work
snapshot/delta/replan/accept-task routes, Plan 39's historical procedure, or
operator-owned npm trusted-publisher setup.

## Counts

| Section | Entries |
|---|---:|
| Mountless surfaces | 20 |
| Missing deliverables | 17 |
| Partial work themes | 17 |
| Adjudicated conflicts | 10 |
| Evidence-only gaps | 18 |

Across the 72 unique gap/evidence entries (conflict rows only reference those
entries): 34 are `RC-BLOCKING`, 15 are `RC-REQUIRED-EVIDENCE`, 21 are
`POST-RC`, and 2 are `RECORDED` (A2, A3).

## A. MOUNTLESS SURFACES

Per repository policy, each implemented-but-unmounted item below must be wired
to a real production caller or deleted.

| ID | Rank | Consolidated item | Evidence and witness verdicts |
| A21 | RC-BLOCKING | Advisory host-delivery consume path | Added post-consolidation (hawk rerun triage, 2026-08-14): `crates/tracedecay-usecases/src/advisory/host_delivery.rs` delivery/consume/hook-notice surface is registered at daemon startup (`src/daemon/service/invocation/registrars.rs`) but the production call site uses only `.runtime().run_once()`; the consume/deliver half has zero production callers. Wire the delivery consumption or retire that half. |
|---|---|---|---|
| A1 | RC-BLOCKING | Generalized external-source acquisition, canonical refetch, correction, and tombstone production | Host-observation specialization is mounted, but `GitHubExternalSourceAcquisitionV1` and the generalized owner remain uncalled (`crates/tracedecay-usecases/src/external_source_github.rs:213-425`; `external_source_acquisition.rs:341-510`). Plans 02 and 03 independently verdict this `IMPLEMENTED-UNMOUNTED`. |
| A2 | RECORDED | Context Scout suggestion producer parked; neither remounted nor demolished | See [A2 ruling](#a2-ruling-2026-08-14). `1caf016e5` deliberately unmounted the saved-edit/stop producer and set `claim_authority = None`. `c2c11956d6` mounted the distinct advisory/feedback successor. Remount is not a small slice; deleting only the entry points leaves `prepare_controlled` dead. Owner must choose remount or retire. |
| A3 | RECORDED | V3 evidence assembly persist contract; unused `publish_or_replay` trait deleted | See [A3 ruling](#a3-ruling-2026-08-14). The unimplemented `EvidenceAssemblyStore` façade was deleted. Persisted schema, `RepositoryWritePayloadV1::EvidenceAssembly`, rusqlite publish-or-replay, and V3 exact/Git-topology targets remain sanctioned-pending contract authority. |
| A4 | RC-BLOCKING | Exact generation-bound Git and diagnostic joins | `GitReadAuthorityV1::join_generation` has only the regression caller (`crates/tracedecay-usecases/src/git_reads.rs:429`; `tests/git_intelligence_regression.rs:679`). `GenerationDiagnosticJoinV1` is referenced by contracts/composite types and tests, not a producer (`crates/tracedecay-code-index/src/diagnostics.rs:108-315`). `25-code-intelligence.md` says both are `IMPLEMENTED-UNMOUNTED`; `25-code-index.md` inferred `IMPLEMENTED+MOUNTED` from broader Git/LSP routes. Exact caller evidence resolves both as unmounted. |
| A5 | RC-BLOCKING | Canonical index and retrieval-pipeline observability emission | `record_index` exists at `crates/tracedecay-usecases/src/observability/emit.rs:560`; `emit_retrieval_pipeline` exists at `observability/retrieval_emit.rs:466`, but its only caller is an in-module test. Query execution computes an observation without calling the canonical emitter. |
| A6 | RC-BLOCKING | Conflict-prediction and linked-outcome emitters | `WorkConflictPredictionObservedV1` and `WorkConflictOutcomeLinkedV1` are closed payload/projection contracts (`crates/tracedecay-domain/src/observability/payload.rs:41-42`) with no production producer, so their confusion matrices cannot be truthful. |
| A7 | RC-BLOCKING | Adoption/consent observability emitters | Eligibility/outcome/consent payloads and helpers exist (`crates/tracedecay-domain/src/observability/retrieval.rs:194-285`; `crates/tracedecay-usecases/src/observability/emit.rs:488-513`) with no production caller. |
| A8 | RC-BLOCKING | `NoProgressObservedV1` | The type and validation exist (`crates/tracedecay-domain/src/observability/runtime.rs:42-54`; payload variant at `observability/payload.rs:35`), but there is no deadline/frontier producer. |
| A9 | RC-BLOCKING | GitHub stack capability/drift canonical emitters | Helpers exist in `crates/tracedecay-usecases/src/observability/github_stack_emit.rs:124-542`; `record_github_stack_drifts` has only its focused test caller. The stack/advisory runtime itself is mounted, but not these canonical observations. |
| A10 | POST-RC | Independent-review/task-outcome label emission | The closed vocabulary is implemented and tested (`crates/tracedecay-domain/src/observability/review_labels.rs:93-432`) without a root emitter or Plan 24 consumer. |
| A11 | RC-BLOCKING | Remote Brain node sender and operational plane | `EnrolledRemoteClient::capture` and transfer/query/recovery methods have zero production callers (`crates/tracedecay-sdk/src/remote_client.rs:197-355`). Inbound authority routes are mounted, but project composition supplies `RemoteOperationalReadV1::Unavailable` (`src/daemon/project_composition.rs:567`); no live Settings/Dashboard/Doctor state, replica/cache refresh, or remote clean-diagnostic publisher is mounted. |
| A12 | RC-BLOCKING | Supported-host registration breadth | The independent witnesses converge on a mixed staged/mountless lane: Codex hook registration is empty (`plugin/hooks/hooks-codex.json:1-3`); Kiro exposes only prompt-boundary native support (`crates/tracedecay-hooks/src/lib.rs:135-138`); Codex Core and Kimi native plugin assets are staged but not registered (`crates/tracedecay-agent-hosts/src/agents/codex.rs:395-426`; `kimi.rs:1-22,370-428`); and the production-mount census finds Kimi/OpenCode commands take the capture-only fast path before their live handlers (`src/hook_capture_cmd.rs:108-172`; live handlers at `src/hooks/mod.rs:329-389`). Plans 07/27 and the census are independent witnesses. |
| A13 | RC-BLOCKING | General typed workflow step executor | `WorkflowStepExecutionService::execute_ready_step` is implemented at `crates/tracedecay-application/src/workflow_run.rs:546`, but all callers are in `crates/tracedecay-application/tests/workflow_dag_execution.rs`. Production workflow start uses the narrower Work fan-out path. |
| A14 | POST-RC | Nineteen per-verb LSP gateway façade methods | Methods such as `DaemonLspGateway::declaration` (`crates/tracedecay-lsp/src/gateway.rs:2377-2599`) have zero callers; the live protocol correctly uses `semantic_request` (`gateway.rs:2649-2681`). Both Plan 35 witnesses agree this parallel façade should be folded in or deleted. |
| A15 | POST-RC | Derived HTTP route documents | `http_route_documents` derives catalog-backed route documentation (`crates/tracedecay-api/src/http.rs:489-532`) but has only a catalog test caller. |
| A16 | POST-RC | Policy source-authorization replay | Exact/recorded/current-best-effort replay exists at `crates/tracedecay-policy/src/replay.rs:17-174`; all callers are policy tests. |
| A17 | POST-RC | Dashboard Work topology accounting read | `dashboard/src/workspaces/work/workTopologyAccounting.ts` explicitly states that its read model is not published; there is no generated contract or route for that advanced accounting sub-surface. |
| A18 | RC-BLOCKING | Dashboard code-index-generation event | `DashboardEventKindV1::CodeIndexGenerationPublished` is explicitly “Declared but unfed” (`crates/tracedecay-dashboard-api/src/events_api.rs:109-112`) and is only constructed in a serialization test. The scheduler's internal publication bus is a different mounted event. |
| A19 | RC-BLOCKING | Plan 24 branch-stack/integration Work surface | Integration observation/contracts exist, but `WorkOperation::ALL` has no Work integration apply/review/stack operation (`crates/tracedecay-api/src/work.rs:62-345`). Native integration exists as a separate family, so it does not mount this Work-context requirement. |
| A20 | RC-BLOCKING | Exact PR-head/manual branch activation | The daemon background PR tracker is mounted, but the exact branch-add production response returns `code_index_scheduler_unavailable` unconditionally (`src/daemon/branch_add.rs:43-70`), and public PR reconcile/track activation paths remain fail-closed in `src/daemon/pr_autotrack.rs:545-565,760-783`. |

## B. MISSING DELIVERABLES

| ID | Rank | Consolidated item | Evidence |
|---|---|---|---|
| B1 | RC-BLOCKING | Real `ResetRequired` settlement and final-shape cutover | Project-open does not settle a real reset-required store (`src/daemon/project_open_handshake.rs:229`; ignored journey `tests/typed_terminal_restart_acceptance.rs:351-415`). Live observation schema migration/backfill remains (`crates/tracedecay-global-db/src/observation/schema.rs:67-154,238-372`) and registered `migrate_and_attach*` paths remain (`registered.rs:349-388`), contradicting the final-V2 no-migration rule. |
| B2 | RC-BLOCKING | Typed auxiliary-provider catalog descriptors | The tool catalog has no descriptor model for executable/version/protocol/model/sandbox/approval/stream/resume/fallback evidence; native Claude/Codex provider runtime exists elsewhere but is not catalog-described (`crates/tracedecay-tool-catalog/src/`). |
| B3 | RC-BLOCKING | Workflow activation semantic admission | Activation calls structural `definition.validate()` (`crates/tracedecay-application/src/workflow_coordination.rs:462-480`), but the validator does not validate operation existence, schemas, capabilities, privilege/effect compatibility, or recursive execution (`crates/tracedecay-domain/src/workflow.rs:158-296`). |
| B4 | RC-BLOCKING | Provider discovery negotiated against the pinned configuration snapshot | Configuration exposes the current snapshot but does not perform the Plan 20/32 executable-capability negotiation or typed unavailable-executable decision (`crates/tracedecay-usecases/src/configuration/runtime.rs:466-483`). |
| B5 | RC-BLOCKING | Legacy-data privacy remediation authority | No production owner performs at-rest rescan, quarantine overlay, derivative rebuild, resumable checkpoints, or backup/restore replay of newer deletion/quarantine policy. Privacy-specific Doctor/UI state is consequently absent (`crates/tracedecay-sessions/src/runtime/lcm/raw.rs:463-553` only protects new ingest). |
| B6 | RC-BLOCKING | Saved multi-root collection/default/source-binding resolver | The mounted substrate is `AuthorizedScopeSet`; named `QueryCollection`/`WorkspaceCollection`, optional defaults, and Plan 20 source bindings are absent. Dashboard truthfully reports no mounted multi-root set (`crates/tracedecay-dashboard-api/src/lib.rs:1829-1841`). |
| B7 | RC-BLOCKING | Native-integration Dashboard handoff and LSP notification | CLI/MCP operations are mounted, but no Dashboard consumer or LSP notification exists (`tests/native_integration_surface_mount.rs:82-116`; no matching source under `dashboard/src` or `crates/tracedecay-lsp`). |
| B8 | RC-BLOCKING | Dashboard workflow-definition/run-control journey | HTTP, CLI, MCP, and SDK workflow routes exist, but Dashboard has no `/application/workflow` consumer or workflow definition/run-control UI. |
| B9 | POST-RC | Canonical benchmark/comparison observability events | `BenchmarkRunAttemptedV1`, `BenchmarkRunTerminalV1`, and `BenchmarkComparisonRecordedV1` (or an equivalent production family) do not exist. |
| B10 | POST-RC | Bounded policy exploration evaluator | The policy crate has no allowlist/floor/ceiling/share/rollback/circuit-breaker/propensity evaluator. |
| B11 | POST-RC | Code co-change source and Disagreement field | No file-pair co-change endpoint, provider-attributed source, or rendered disagreement field exists for Plan 11b. |
| B12 | POST-RC | Advanced Work checkpoint/discovery and controls | No Work-specific checkpoint/skill/hint/provider-discovery operation, task-title Command-K provider, expertise/calibration view, or host/LSP handoff control is mounted. |
| B13 | POST-RC | Quantitative secret-detector corpus and calibration | Assessment contracts exist, but no checked-in positive/negative corpus, precision/recall/FP/FN runner, or held-out calibration artifact exists. |
| B14 | POST-RC | Typed external extension framework | Plan 19's typed capability/revision/canonical-operation extension declaration has no production implementation. |
| B15 | POST-RC | Whole-store dashboard history | Current storage telemetry deliberately returns growth/history as unknown until a daemon-owned history exists (`crates/tracedecay-dashboard-api/src/storage_telemetry_api.rs:1-21,146-148`). |
| B16 | RC-BLOCKING | Public SDK bidirectional/enrolled-remote parity | Public SDKs provide local HTTP/SSE and a separate remote-protocol subset, not one generated operation set over an enrolled remote authority; the Plan 35 bidirectional negotiated session and complete two-token handoff journey are absent (`crates/tracedecay-sdk/src/client.rs:27-73`; `remote_client.rs:197-291`). |
| B17 | POST-RC | Multi-worktree artifact reuse and move/delete lifecycle authority | No Plan 16 production authority proves content reuse without logical identity sharing, move-preserved identity, or delete/recreate identity replacement. |

## C. PARTIAL ITEMS BY WORK THEME

| ID | Rank | Theme | Remaining coherent slice |
|---|---|---|---|
| P1 | RC-BLOCKING | Typed terminals and stream lifecycle | Core `ResetRequired`/`PartialEffect` validation is strict, but HTTP/MCP/SDK transport paths, SSE resume-expiry/duplicate/disconnect/drop accounting, post-commit cancellation, and reset settlement are not one complete mounted behavior. |
| P2 | RC-BLOCKING | Work and TaskSession | Dashboard positive continuation/rank-final revocation requires an activated federated authority; `Anchor` continuation is unconditionally unavailable (`src/daemon/work_evidence_retrieval.rs:225-230`). `ProfileOwnedNoGit` selection poisoning remains an unresolved owner decision (`NEXT.md:429-437`). |
| P3 | RC-BLOCKING | Multi-root federation | Scope-set CAS, LSP folders, inventory, and generic fanout are mounted, but full Plan 05 fusion/hydration, immutable distributed pagination, per-member coverage, dashboard/CLI parity, and stack-to-Git receipts are incomplete. |
| P4 | RC-BLOCKING | Code-index lifecycle and shared descriptor authority | Markdown structure is admitted but normal `.md` grep visibility remains open (`NEXT.md:582-585`); descriptor-to-analyzer/LSP authority is unproven; rewrite still shells out to host ast-grep; combined cancelled-refresh/restart/branch-split publication and disk-full/concurrent-build paths are incomplete. |
| P5 | RC-BLOCKING | Workflow ownership and control | `admit_workflow_child` internally accepts proposals/admission (`src/daemon/service/invocation/work/workflow_fan_out.rs:583-712`); the durable aggregate lacks a shared deadline/cancellation generation/budget ledger; fairness/no-progress, native approval/EffectUnknown, backup/remote-worker fencing, and registry-derived CLI deadline are incomplete. |
| P6 | RC-BLOCKING | Observability read models and production accounting | Topology metrics/rollup bounds, LSP events, provider pricing provenance, stack observations, review labels, and card-by-card population/coverage parity remain incomplete even where the canonical envelope/read service is mounted. |
| P7 | RC-BLOCKING | Hook/advisory semantics | Hook event-family breadth, revision quarantine, exact debounce/timing, rollback switch, complete failure matrix, Scout feedback delivery, and one shared suggestion channel remain incomplete; advisory successor notices do not make Scout production-ready. |
| P8 | POST-RC | Dashboard workspace product depth | Core routes/pages are mounted, but Sessions replay/raw boundaries, Agents tree/handoff frontier, Knowledge contradictions, Observatory flow/latency, Costs latency, advanced Work controls, CORTEX channels, Loom controls, and full renderer parity remain plan ambitions beyond basic RC usability. |
| P9 | RC-BLOCKING | SDK lifecycle parity | Generated schemas are current, but complete local/enrolled-remote operation parity, handoff failure cases, post-commit cancellation/reconnect, and cross-surface semantic comparison are incomplete. |
| P10 | RC-BLOCKING | Privacy sinks and analyzer isolation | New ingest is sanitized, but full taint propagation and every logs/metrics/API/UI/export/diagnostic sink are not proven; LSP authorized-analyzer and remote capability/disclosure enforcement is incomplete. |
| P11 | RC-BLOCKING | Git/native-integration product contract | Core preview/apply and native owner are mounted, but exact PR thread/comment contract equivalence, checked-out destination variants, approval-to-receipt public journey, failure injection, and Plan 16/LSP-originated selection are incomplete. |
| P12 | RC-BLOCKING | Semantic evaluation, hydration, and rollback | Runtime/vector/query fallback is mounted. `f478c323a` now preserves `SearchEvalError` detail in the typed rejection, but accepted-profile Linux evidence, correction of the surfaced evaluation failure, rollback drill, federated hydration/revocation, and split-store conformance remain incomplete. |
| P13 | RC-BLOCKING | Configuration protected changes and execution snapshot | Direct CAS/configuration is mounted, but protected preview/apply/rollback lacks a production journey; complete provider/work snapshot, mid-attempt no-reread, adapter-default rejection, unsafe-Git combinations, and requested/actual drift views are incomplete. |
| P14 | POST-RC | Policy outcome/replan/calibration | Provider-admission re-evaluation, committed-outcome-driven unapplied replan, complete cohort/horizon/error/drift calibration, self-grading separation, and exploration remain incomplete. |
| P15 | POST-RC | Storage retention operations | Retention is mounted, but the historical orphan backlog is operator-unverified and code-generation retention lacks the separately promised semantic-publication trigger. |
| P16 | POST-RC | Defragmentation and compatibility cleanup | Canonical application routing is real, but release evidence for retained delegates, broad duplicate-wrapper deletion, wildcard parent/child imports, and all ownership-boundary negatives are not complete. |
| P17 | POST-RC | Grafeo breadth after superseded landing plan | Core graph domains are mounted. Remaining historical ambitions are workflow run/attempt/handoff topology, exhaustive old graph-shaped-row deletion proof, aggregate cross-domain rerun, and a pre-Grafeo performance comparison. |

## D. CONFLICTS ADJUDICATED

| ID | Rank | Conflict | Ruling |
|---|---|---|---|
| D1 | RECORDED | Scout contracts/hooks “mounted” versus zero selection/owner callers | Controls, address registry, and advisory successor remain mounted; Scout envelope production remains parked. See [A2 ruling](#a2-ruling-2026-08-14). |
| D2 | RC-BLOCKING | Plan 25 duplicate witnesses disagree on Git/diagnostic joins | Broader Git and LSP routes are mounted, but the exact generation join entrypoints are not. Caller/type evidence resolves A4 as unmounted. |
| D3 | POST-RC | One Plan 35 witness reported missing host/feedback mounts; the other found the production route | Current project-open source mounts feedback/advisory and LSP semantic authorities (`src/daemon/project_open_owners/advisory_runtime.rs:291-308,423-628`; LSP protocol uses `semantic_request`). Those product paths are mounted. Only the 19 convenience façades remain unmounted (A14). |
| D4 | RC-REQUIRED-EVIDENCE | Plan 36 witnesses disagree on native approval/fanout mounting | Current source mounts the owner, six operations, exact topology, and coordinator preflight. Approval is structurally mounted; the gap is the public approval-to-receipt/restart journey, not an absent handler. Manual branch activation remains independently unavailable (A20). |
| D5 | RC-BLOCKING | Plan 37 witnesses disagree on PR auto-track and CI localization | Background discovery/stack/advisory CI localization are mounted. Delivery failure localization is intentionally `NotConfigured` and superseded by decision. Exact PR-head/manual activation still returns unavailable, so A20 remains. |
| D6 | RC-BLOCKING | Semantic path called “implemented-unmounted” versus Plan 31 mounted runtime | Runtime, vector publication, and query fallback are mounted. The live acceptance failure is evaluation/activation rejection, not absence of a semantic production caller. P12/E3 govern. |
| D7 | RC-BLOCKING | Plan 27 calls Kimi/OpenCode plugin artifacts mounted; mount census calls live hooks capture-only | Artifact generation/install and handler functions exist, but the pre-main capture fast path prevents live handler dispatch for those command forms. The census's executable-path evidence wins; A12 remains. |
| D8 | POST-RC | `task_activity` listed as an unmounted conformance exception | Current daemon code publishes `ActivityFamilyV1::Task` after committed Work mutation (`src/daemon/service/invocation/work.rs:107-123`), and Dashboard subscribes. The exception is stale test/ledger maintenance, not a product gap. |
| D9 | POST-RC | Plan 34 requests read-only LSP rename candidate/preview; Plan 35 explicitly keeps rename unavailable | The current gateway intentionally returns unavailable (`crates/tracedecay-lsp/src/gateway.rs:2683-2693`) and never applies edits. Plan 35 is the more specific current LSP authority; treat Plan 34's read-only rename binding as a post-RC plan-authority reconciliation, not an RC edit-safety defect. |
| D10 | RC-BLOCKING | Plan 19 reports fresh final shape complete; Plan 12 finds live migrations/backfills | Direct current source shows live observation migration/backfill and `migrate_and_attach*`. The narrow fresh-store/read-only paths do not satisfy the universal cutover. B1 governs. |

## E. EVIDENCE-ONLY GAPS

These entries require current-tree runs, not new contract inventories.

| ID | Rank | Required run | Success criterion |
|---|---|---|---|
| E1 | RC-REQUIRED-EVIDENCE | Typed-terminal physical-restart matrix | Drive real `PartialEffect` and `ResetRequired` through HTTP, MCP, Rust SDK, and TypeScript SDK; preserve receipt/legal action across daemon kill/respawn. CLI `PartialEffect` is already proven. |
| E2 | RC-REQUIRED-EVIDENCE | Retained-surfaces restart parity | One real project/profile-store journey covering memory reads/effects, session-refresh begin/status/cancel, LCM retrieval, unavailable families, reconciliation, and CLI/MCP/HTTP/both SDKs. |
| E3 | RC-REQUIRED-EVIDENCE | Plan 15 Linux semantic evaluation/activation | Run the pinned 1x/10x sanitized corpus offline; retain raw resource/quality evidence; produce a diagnosable pass/fail; activate only a passing profile; execute rollback. |
| E4 | RC-REQUIRED-EVIDENCE | Incremental-index lifecycle | Save, rename, delete, ref switch, overflow, cancellation, physical restart, serve-during-refresh, and exact compatible republish in one non-vacuous journey. |
| E5 | RC-REQUIRED-EVIDENCE | Supported-host lifecycle fleet | Fresh binary install/update/repair/Doctor/stock journeys for Claude, Codex, Cursor agents/in-composer, Kimi, Kiro, and OpenCode. Hermes is already closed and must not be rerun as an open gap. |
| E6 | RC-REQUIRED-EVIDENCE | Plan 16 same-name multi-root journey | Same-name repositories, linked worktrees, nested folders, denied sibling, immutable pagination across restart, exact anchors, and CLI/MCP/HTTP/UI/LSP parity. |
| E7 | RC-REQUIRED-EVIDENCE | Plan 36 public native-integration journey | Start from Plan 16/LSP selection; exercise pair and declared edge, checked-out/unoccupied destinations, preview/approval/apply/status/cancel, all three modes, daemon restart, and final native receipt. |
| E8 | RC-REQUIRED-EVIDENCE | Remote Brain multi-machine journey | Offline capture, authority change, transfer/replay/duplicate receipt, query coverage, diagnostics, backup, isolated restore, promotion, and old-authority rejection. |
| E9 | RC-REQUIRED-EVIDENCE | Observability topology/settlement journey | Current-tree execution-topology sampling, fanout/dedupe/drop settlement, rollup, compaction, retry/leak, blocked intervals, cancellation, restart, and cross-transport read parity. |
| E10 | RC-REQUIRED-EVIDENCE | Performance refresh | Same-host release `scripts/perf-gate.sh`; clean Linux session-temporal `--refresh-contract`; Work-rollup latency/throughput. Current CI does not mount perf-gate and no fresh artifacts exist. |
| E11 | RC-REQUIRED-EVIDENCE | Real LSP host clients | Live Claude and OpenCode negotiated lifecycle/navigation/diagnostics/cancel/reconnect plus Cursor native-diagnostic merge and exactly-one-analyzer install/repair/rollback/uninstall. |
| E12 | RC-REQUIRED-EVIDENCE | Basic real-Chrome Dashboard usability | Exercise all published routes at required viewport families, keyboard/focus, reduced motion, fallback rendering, and truthful partial/unavailable states. |
| E13 | RC-REQUIRED-EVIDENCE | Default package/install/start smoke | Fresh package artifact, isolated profile/project, daemon start, installed SDK/client operation, and uninstall/cleanup; npm publication itself remains operator-owned. |
| E14 | RC-REQUIRED-EVIDENCE | Doctor authority audit and Cursor lifecycle | Re-run the Doctor remediation/re-observation journey and clean Cursor agents/in-composer install → version bump → Doctor, preserving drift versus ownership-conflict states. |
| E15 | RC-REQUIRED-EVIDENCE | Final RC aggregate gate | Build Dashboard assets, then non-vacuous workspace all-feature nextest, Dashboard typecheck/tests/build, contract and SDK drift checks, host bundle/stock checks, commitlint, release drift, and packaging smoke. |
| E16 | POST-RC | Manual assistive-technology and usability study | Manual NVDA/VoiceOver and the plan's multi-participant study are not part of the basic RC usability bar. |
| E17 | POST-RC | Oldest-supported/Windows SDK matrix | Installed Rust and TypeScript package combinations against current and oldest supported daemons on Linux/Windows. |
| E18 | POST-RC | Grafeo aggregate/performance provenance | Full cross-domain Grafeo journey plus p50/p95/p99, RSS, bytes, write amplification, and reopen comparison; Plan 39's historical task list is superseded, so this is diagnostic/post-RC evidence. |

## Recommended next fix dispatches

Each task is deliberately single-concern.

1. Make project-open settle one incompatible store as typed `ResetRequired`;
   unignore only that exact restart regression.
2. Closed: A2. Do not reconstruct the deleted hook-cycle mount and do not
   demolish the parked Scout runtime. Owner chooses remount (restore
   `run_production_hook_cycle` + lifecycle + claim authority) or retire
   (delete producer entry points and `prepare_controlled` together).
3. Register one generalized GitHub external-source acquisition owner at
   project-open; prove one canonical refetch reaches the existing store.
4. Closed: A3. Do not invent a V3 evidence-assembly producer. Plan 23 owns
   retriever-contribution publication; Git-topology V3 producers remain the
   owning plans in Plan 13's pending-producer inventory.
5. Mount an `EnrolledRemoteClient` node-side capture/transfer scheduler without
   changing the authority-side protocol.
6. Route production workflow ready-step execution through
   `WorkflowStepExecutionService`, or delete that parallel executor and its
   advertised general-step claim.
7. Add pre-activation workflow operation/schema/capability/effect validation
   against the executable catalog.
8. Remove the Kimi/OpenCode capture-fast-path bypass so their installed native
   events reach the existing live handlers.
9. Produce and persist one conflict-prediction plus linked-outcome observation
   from existing Work/native evidence.
10. Run the Plan 15 Linux evaluation and fix the first surfaced
    `SearchEvalError`, rather than widening deadlines or weakening the oracle.

## A2 ruling (2026-08-14)

Decision: **RECORD-RULING**. The producer mount was deliberately parked, not
abandoned mid-build. **WIRE** and **DELETE** are both rejected without an
owner remount-or-retire choice. A2 stays parked, not silently green.

### Caller-intent evidence

- Before `1caf016e5` (`refactor(runtime): complete V2 authority cutovers`,
  2026-08-09) the producer was mounted. `run_production_hook_cycle` in
  `src/daemon/project_open_owners.rs` mapped SavedEdit/Stop/Explicit to
  `ContextScoutCanonicalInputV1::selection_input` and
  `ProjectContextScoutOwnerV1::prepare_configured`, then mounted claim
  authority on enqueue. `src/daemon/project_open_owners/scout_journey_tests.rs`
  (378 lines) covered that journey.
- `1caf016e5` deleted that mount on purpose: `context_scout_lifecycle.rs`
  (−1303), the scout journey tests, and ~2203 lines from
  `project_open_owners.rs` including `run_production_hook_cycle`. The same
  commit replaced live claim-authority resolution
  (`resolve_current_context_scout_claim_authority`) with
  `let claim_authority = None` in
  `src/mcp/tools/handlers/hook_runtime/admission.rs`. That assignment is
  why the ready-guidance branch is unreachable: it is a cutover disable,
  not a forgotten `None`.
- Current `admit_hook_orchestration`
  (`src/daemon/service/invocation/types.rs`) has no success path. SavedEdit,
  session End/TurnComplete, and explicit prepare all return `Unavailable`;
  every other event returns `UnsupportedTrigger`. `hook_v2_scout_prepare`
  only calls that stub. This is a parking brake, not a missing wire.
- `c2c11956d6` (`feat(advisory): mount production feedback runtime`,
  2026-08-12) mounted the hook-notice successor. Admission now peeks
  `peek_advisory_hook_notice`. That surface is distinct from Scout
  suggestion envelopes (D1).
- `tracedecay tool callers` on `prepare_configured`
  (`method:db3b71de8fa41fdfb4e5aa1f22ab09e9`) and `selection_input`
  (`method:a3e64357a89b62413d0774fe9ef8d57b`) returns none.
  `bind_and_assemble` and `ContextScoutCanonicalInputAssemblerV1` have no
  production callers. `run_production_hook_cycle` no longer exists.
  `resolve_current_context_scout_claim_authority` no longer exists.
- NEXT.md has no Scout remount or retire ruling. Plan 22 still describes a
  saved-edit/stop envelope journey; a plan ambition is not a mount.

### Why not WIRE

The “smallest honest slice” is not a hook-to-`prepare_configured` call.
`selection_input` requires a fully assembled canonical packet (address
registry bind, authority pin, `RequestContext`, lifecycle, committed
publication, candidates). That assembler was fed by the deleted lifecycle
and `run_production_hook_cycle`. Reconstructing those is a product remount,
not a one-call connect. Wiring a thinner envelope from the hook alone
would fabricate Scout evidence.

### Why not DELETE

The stated delete set (`prepare_configured`, `selection_input`, dead
assembler ports) is not warning-free. The only non-test caller of
`ContextScoutDurableRuntimeV1::prepare_controlled` is `prepare_configured`.
Removing the entry points leaves the deterministic runtime unused under
`cargo check --lib`. Deleting `prepare_controlled` as well demolishes the
Plan 22 producer while mounted controls, durable store, address registry,
and claim/delivery/feedback remain. That is a retire decision, not a dead-
port cleanup. `allow(dead_code)` is forbidden.

### Action taken

- No producer, advisory, hook-cmd, or daemon-reset code was changed.
- Parking is now an explicit register ruling: owner must remount the
  deleted hook-cycle/lifecycle/claim-authority path, or retire the
  producer entry points together with `prepare_controlled`.
- Advisory/feedback (`c2c11956d6`) remains the live hook-notice successor
  and is not a Scout envelope producer.

## A3 ruling (2026-08-14)

Decision: **RECORD** the persist/V3-target contract; **DELETE** the unused
`EvidenceAssemblyStore` trait. **WIRE** is rejected.

### Caller-intent evidence

- Plan 13 names `PublishEvidenceAssembly::execute` as
  `EvidenceAssemblyStore::publish_or_replay` and says Plan 23 emits
  `RetrieverContributionRecordV1` after it freezes scope, temporal mode, and
  watermarks. That producer does not exist. Plan 23's live
  `RetrieverContribution` / `RetrieverContributionV1` types are application
  and temporal-query ranking records, not `EvidenceAssemblyWriteV1`.
- The only historical production-shaped caller was
  `RuntimeEvidenceAssemblyStore` in `crates/tracedecay-usecases/src/evidence_assembly.rs`.
  It shipped under `#![allow(dead_code)]` and was removed in `a2fea0e7a`
  (`refactor(evidence): remove unmounted duplicate adapters`). The usecases
  seam note kept "canonical store and runtime capabilities" and deleted the
  adapter "until a production journey needs them."
- `tracedecay tool callers` on `publish_or_replay` and on
  `RepositoryWritePayloadV1::EvidenceAssembly` returns no production
  constructor. The only write-payload construction site is
  `crates/tracedecay-rusqlite-runtime/src/writer/tests/authority.rs`.
- The trait had **zero implementors**. Real publish-or-replay is
  `EvidenceAssemblyExecutor::execute_write`
  (`crates/tracedecay-rusqlite-runtime/src/repository/evidence_assembly/mod.rs`),
  already covered by `publish_replay_conflict_and_drilldown_are_atomic`.
- Work evidence retrieval uses a different application
  `RetrieverContribution`. GitHub stack publication uses the mounted V2 path
  in `stack_anchors.rs`. Observation, diagnostic, CI, resolution, tombstone,
  and UI anchors are mounted separately. None of those paths publish
  equivalent V3 evidence assemblies, so the persist contract is not
  superseded-in-place.
- Git-topology V3 targets are already **SANCTIONED-PENDING** in Plan 13
  (2026-08-07 pending-producer inventory). Owning plans: 36/27/37/24/32/16/03.
  Plan 13 forbids deleting those targets as unused breadth.

### Why not WIRE

No current retrieval, stack, or work path can construct a honest
`EvidenceAssemblyWriteV1` (occurrence set, verified ordering proof, dual
sanitization receipts, catalog binding, retriever contribution). Wiring a
call without that producer would fabricate evidence.

### Why not delete the persist stack

Deletion of the unused trait does **not** destroy schema authority. Deletion
of the persist stack would. Other code reads that authority:

- `EVIDENCE_ASSEMBLY_SCHEMA` / `EVIDENCE_ASSEMBLY_IMMUTABILITY` are part of
  the final-shape expected schema
  (`crates/tracedecay-runtime-core/src/db/migrations/final_shape.rs`).
- `RepositoryWritePayloadV1::EvidenceAssembly` and
  `ProjectReadOperationV1::EvidenceAssembly` are live store-protocol variants
  dispatched by rusqlite `execute`.
- `RetrievalAnchorTargetV3::{ExactSourceOccurrence, ExactEvidenceSpan,
  RetrieverContribution}` and the Git-topology target family are the
  contracts later producers must bind to.

### Action taken

- Deleted the unimplemented `EvidenceAssemblyStore` trait and the
  trait-only `EvidenceAssemblyPublicationOutcomeV1` enum.
- Left write/read types, rusqlite executor, final-shape tables, and V3
  target contracts in place for the owning plans.
