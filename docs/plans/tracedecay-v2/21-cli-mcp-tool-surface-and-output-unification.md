# TraceDecay V2 CLI, MCP, LSP, and Output Unification

**Delivery:** PR12 core; PR17 Plan 24/32 task/work and internal cross-worktree
extensions; PR18 public-name freeze

**Status:** planned product work
**Depends on:** [08 tool catalog](08-tool-catalog-crate.md), [09 application](09-application-crate.md), [18 privacy](18-secret-detection-redaction-and-private-data-safety.md), and [20 configuration](20-configuration-control-plane.md). PR12 ships Plan 35's explicitly resolved single-project LSP admission; [16 scope](16-cross-project-repository-worktree-scope.md) extends it to canonical multi-root admission in PR15 and is not a PR12 prerequisite.

## Outcome

CLI and MCP are thin clients over the same daemon-owned application use cases.
LSP is a stateful sibling adapter over the same typed code and diagnostic
contracts. CLI and MCP render typed results as compact human output or canonical
JSON; LSP uses protocol-native JSON-RPC responses and diagnostics.

This plan does not create a generated surface inventory, developer-plan parser,
command generator, parity matrix, independent task editor, or workflow
executor. PR17 binds typed Plan 24 task/work and Plan 32 runtime application
operations without moving their semantics into adapters.

## Boundaries

- The routed `tracedecayd` is the sole application and database authority for
  each selected mutable shard; PR16 preserves exactly one fenced daemon
  authority per shard.
- `tracedecayd` owns the LSP gateway, session state, routing, diagnostics, and
  analyzer lifecycle defined by Plan 35. The stdio bridge contains only
  authenticated framing and transport logic.
- CLI and MCP resolve transport input, call the daemon, and render the response. They never open a business database or run query, policy, migration, or repair logic locally.
- The stdio bridge never opens a business database, starts an analyzer, or
  implements admission, routing, merge, privacy, or fallback policy.
- A missing or incompatible daemon fails closed with one actionable problem. No client silently falls back to an embedded writer.
- HTTP routing, extraction, and encoding remain owned by
  [10](10-api-crate.md); public protocol versioning and SDK bindings remain
  owned by [17](17-official-public-api-and-sdks.md). Shared schema and binding
  references do not transfer adapter lifecycle ownership. Dynamic workflow
  semantics remain owned by [32](32-dynamic-workflow-runtime-and-sdk.md).
- Developer plans and Markdown files are documentation, not runtime input.

## Typed application result

Every exposed use case returns a sealed result containing:

- the requested data or command receipt;
- resolved scope and snapshot identity;
- coverage, freshness, redaction, and partial-state information;
- stable pagination or retrieval anchors where the result is resumable;
- typed warnings, failures, retry guidance, and legal next actions.

Renderers cannot query stores, infer missing state, mutate results, or traverse arbitrary `serde_json::Value`. Domain-specific views may define compact presentation, but JSON always serializes the semantic result rather than a presentation document or transport wrapper.

## Output contract

- MCP defaults to compact Markdown in text content and returns schema-valid structured content when the protocol supports it.
- CLI defaults to deterministic human output. `--json` emits canonical JSON only.
- JSON is never embedded as an escaped string inside another JSON result.
- Empty, partial, unavailable, denied, stale, redacted, ambiguous, pending, and failed remain distinct.
- Canonical problems preserve semantic code, layer, terminality, retryability
  and retry scope, legal next actions, cancellation stage, saturation, stream
  gap, resume expiry, provider availability, and partial coverage across
  protocol-native mappings. A transport error never substitutes for one of
  these application states.
- Collections use stable ordering and an opaque cursor. Truncation either returns a resumable anchor or a typed budget error.
- Human output keeps identifiers, coverage, blockers, and next actions needed to continue safely.
- Terminal controls, Markdown, paths, labels, and errors pass the shared output-safety boundary.
- Normal results use stdout; diagnostics use stderr; exit classes are stable and tested.

## CLI adapter

PR 12 consolidates shared input handling for scope, format, pagination, and daemon connection without rebuilding the command tree from Markdown or an inventory file. Commands call explicit application methods and map typed problems to stable exits.

Help stays concise and task-oriented. Deprecated aliases may have a bounded compatibility shim, but aliases do not retain separate semantics or implementations.

## MCP adapter

Use one protocol implementation with isolated per-connection lifecycle, authentication, negotiated capabilities, cancellation, and backpressure. Advertise only features that are implemented and directly tested.

MCP tools and resources call the same explicit application methods as CLI. MCP task IDs, retrieval anchors, sessions, and workflow IDs remain distinct types. MCP clients cannot choose hidden bindings or bypass authorization through a generic invoke tool.

Multiple MCP clients may connect concurrently, but all business reads and writes are brokered through the owning daemon authority established locally by PR4 and generalized per shard by PR16.

## Daemon connection capacity and saturation

Brokering every client through the daemon makes daemon admission a product
surface, not an implementation detail. PR7 dogfooding demonstrated the failure
shape this section forbids: concurrent agent fleets exhausted the daemon's
client capacity, connections were shed at the transport (broken pipe /
connection reset), and Doctor's own diagnostics were among the shed traffic —
so a healthy store was indistinguishable from a corrupt one exactly when an
operator was trying to tell them apart.

- Each client process holds one multiplexed daemon connection; per-request
  connections and per-tool-call CLI one-shots share it through the daemon
  client rather than multiplying sockets.
- Admission is class-aware. Health, doctor, and diagnostics traffic occupies a
  small reserved class that bulk context/query/ingest traffic cannot starve;
  saturation of the bulk class never makes the daemon unobservable.
- Capacity exhaustion is a typed, rendered saturation problem with retry
  guidance — never a raw transport error. Clients render it distinctly from
  daemon-missing and daemon-incompatible.
- Shed and rejected admissions are counted and visible through the Plan 26
  observability surface so saturation is diagnosable after the fact.

## LSP adapter

LSP is stateful and protocol-native; it does not use canonical CLI/MCP
rendering. The daemon gateway owns initialization, shutdown, capability
negotiation, workspace and document lifecycle, document versions, cancellation,
notifications versus responses, bounded queues, and backpressure.

Each supported standard method resolves through an explicit catalog binding to
a typed application/query operation. No adapter or bridge may blindly proxy an
unknown method or arbitrary JSON-RPC payload. Notifications cannot satisfy
pending requests, and cancelled or superseded document versions cannot publish
results.

A missing, incompatible, or failed daemon produces one bounded protocol-native
startup or session failure; the bridge starts no local fallback. Conformance
fixtures compare navigation, diagnostics, lifecycle, cancellation, ordering,
and failure semantics with direct use of each supported upstream analyzer,
allowing only documented TraceDecay provenance, bounds, and exact graph
augmentation.

## Canonical dispatch and tool families

The Plan 08 catalog snapshot's schema-reference index and canonical binding
taxonomy map CLI, MCP, HTTP, and LSP names to cataloged application operations.
The CLI/MCP dispatcher, Plan 10 HTTP router, and Plan 35 LSP gateway consume
those references through their own protocol adapters. Bindings may validate
transport syntax and render protocol-native results; aliases contain zero
authorization, query, mutation, storage, availability, or fallback logic.

- `search`, `find_exact`, qualified-name lookup, similar-symbol lookup, and
  signature search are views over one symbol kernel.
- `read`, `outline`, `module_api`, `signature`, and file views share one
  source/outline kernel.
- `callers`, `callees`, `callers_for`, call chains, file dependents, and impact
  share one graph-traversal kernel; implementation and type-hierarchy names are
  typed graph views, not separate engines.
- `test_map` and `affected` share one test-attribution kernel.
- Exact, symbol, insert, move, and structural rewrites use the one journaled
  application `EditTransaction`; preview/dry-run never means a second edit path.
  Plan 35's `prepareRename` and `rename` bind only to read-only
  candidate/preview UseCaseIds; they never bind directly to
  `tracedecay_rename_symbol`, API-migration apply, another write-effect entry,
  `workspace/applyEdit`, or opaque server commands. Plan 34's immutable
  preview/manifest and `EditTransaction` remain the only apply path. General
  LSP `textDocument/codeAction` is deferred from PR12 and cannot ship until a
  separate owner defines typed candidate consumption, policy classification, a
  canonical preview/`EditTransaction` route, and acceptance fixtures.
- Git index mutation exposes exactly two public operations on both CLI and
  MCP: `git_preview` and `git_apply`. They share one typed schema and call the
  PR11 daemon-owned `GitIndexTransaction`; adapters cannot invoke its internal
  `stage_hunks`, `unstage_hunks`, or `commit_index` steps independently.
  `git_preview` returns selected hunks, intended effect class, CAS evidence,
  and an immutable transaction digest without locking or mutating the index.
  `git_apply` requires that preview identity, acquires the real index lock,
  revalidates CAS state, and returns an idempotent receipt or a typed stale,
  conflict, lock-contended, denied, or invalid-effect result. Neither operation
  is generic Git execution or permits autonomous merge, rebase, cherry-pick,
  branch/tag/ref mutation, or history rewriting.

Literal grep, AST structural match, body source, graph node records, and context
composition remain distinct because their evidence and semantics differ.
`diagnose` and `diagnostics` remain distinct effects. Project/runtime/storage,
memory, LCM, and daemon health views remain distinct evidence domains even when
their bindings share the dispatcher.

The explicit read-only feedback diagnostics surface binds once here at PR12
across CLI/MCP/HTTP and at PR13 through host adapters defined by
[Plan 27](27-cross-host-agent-plugin-bundles.md). Operations are
`feedback_diagnostics`, `feedback_get`, `feedback_expand`, and
`feedback_list` — advisory, read-only views over
[Plan 09](09-application-crate.md)'s PR11 feedback-cycle result and finding
lifecycle; they are not degraded LSP methods and do not gain duplicate
transport-specific implementations. CLI/MCP canonical JSON and compact
Markdown preserve the same semantics, finding IDs, coverage/state, source
provenance, and continuation metadata. Collections use
[Plan 05](05-query-crate.md) stable opaque cursors; durable expansion resolves
[Plan 13](13-research-provenance-and-context-anchors.md)
`RetrievalAnchorId`s. Oversized transport output uses the existing reversible
response-handle path (`tracedecay_retrieve`) with explicit original count,
returned/preview count, handle, expiry, and typed unavailable/budget errors.
Response handles are never durable finding IDs. Ingested GitHub review threads
first surface through PR13 host adapters; PR17 optional workflow composition
does not gate these bindings.

## PR17 task/work graph bindings

CLI and MCP expose a compact, audience-filtered surface for Plan 24
initiative/work-item/version, dependency, context/history, saved projection,
assignment/review, task-shape assessment, decomposition review,
routing recommendation, split/merge/resize/re-route proposal, outcome record,
calibration report, Plan 24 auxiliary-attempt request, provider-capability
inspection, and Plan 32 auxiliary admission/progress/inspect/cancel operations.
PR17 binds these semantic operation families without claiming their command or
tool spellings are the frozen PR18 SDK vocabulary. Human/operator,
orchestrator, reviewer, and active-executor profiles are distinct; an active
executor receives only its addressed work, context, lifecycle, and handoff
capabilities and cannot grade itself or accept its own proposal.

Kanban, DAG, timeline, causal, workload, executor/model, and
repository/delivery reads render the same sealed application selection with
canonical IDs, versions, scope, watermarks, coverage, evidence anchors, and
Plan 32 runtime refs. There is no adapter-local board filter language, status
setter, readiness calculation, model score, scheduler command bus, or generic
task invocation. Mutations require the exact typed application capability,
expected versions, authorization, and idempotency identity. No operation
accepts this roadmap or another Markdown file as executable work input.
Assessment, recommendation, and calibration output always preserves feature
coverage, confidence/intervals, evidence horizon, estimator/policy/config
revisions, exclusions, abstention/fallback reason, and requested-versus-actual
route. Human output cannot shorten an abstention, stale proposal, unknown
outcome, censored failure, or model-version boundary into a score or success.

Auxiliary surfaces submit or inspect typed application requests only. CLI, MCP,
and HTTP clients never execute Claude Code or Codex locally, construct a shell
command, pass raw environment/secrets, choose an unapproved fallback, mint a
lease, or parse provider output into graph state. Results preserve requested
and actual provider/backend/executable/protocol/model/reasoning identity,
negotiated capabilities, sandbox/approval class, parent lineage,
progress/heartbeat and stream coverage, cancellation stage, artifacts, and the
explicit `Unsupported`, `Absent`, `Stale`, `Cancelled`, `TimedOut`, `Failed`,
or `Partial` outcome. These are PR17 semantic surface families, not frozen
PR18 command, MCP-tool, route, or SDK method names.

Every opaque `TaskId`/`WorkItemId` remains the durable authorized retrieval
root across compact lookup/context, current/as-of/evolution/forensic history,
thread/attempt traversal, impact and affected tests, handoff, escalation,
governed experience recall, proposal review, and runtime control. Oversized
output uses reversible response handles, but durable identity remains the
TaskId/finding ID plus Plan 13 anchors. Summaries and handles never replace
exact evidence or widen access.

Task, proposal-decision, and runtime-control operations preserve distinct
capability/effect classes. No binding introduces a generic task DSL,
adapter-local query kernel, status setter, scheduler, Doctor probe, or hidden
alias.

Pinned Hermes CLI/dashboard evidence at `c48d53413aa2c`
(`hermes_cli/kanban.py`, `plugins/kanban/dashboard/plugin_api.py`) establishes
the minimum practical Work UX, not names to copy. CLI/MCP/HTTP must offer
compact, composable outcomes for:

- initiative/work selection, task list/detail, derived lane/readiness,
  dependencies/blockers, assignment/recommendation, comments and artifacts;
- decomposition/proposal review and explicit legal actions;
- attempt/run history, bounded event/output tail, delegation/parent lineage,
  progress/heartbeat, diagnostics, capacity/defer reasons, and terminal
  receipts;
- dispatch watch/status, workload/provider statistics, cancellation/recovery,
  and restart/resume explanation; and
- applicable skill/hint/capability discovery with provenance and availability.

The derived UI may use familiar triage/todo/scheduled/ready/running/blocked/
review/done/archived vocabulary where semantically valid, but no adapter writes
a card status, selects an ambient board/profile, or treats a list rendering as
readiness authority. Plain-text process exit, a terminal-protocol reminder, or
a dragged card is never a successful application result.

## PR17 internal cross-worktree operation family

PR17 adds one paired CLI/MCP **internal profile** over Plan 07's daemon-admitted
worktree events, Plan 24's task/readiness authority, Plan 36's Git identity,
and Plan 37's advisory conflict/proximity findings. These bindings are
developer/conformance names, not public compatibility promises. They remain
absent from default public help, MCP discovery, HTTP routes, and SDKs until
Plan 17 freezes the public names in PR18.

The internal semantic operation IDs and temporary paired bindings are exact:

- `worktree.task_placement.read.v1`: CLI `worktree task-placement`; MCP
  `worktree_task_placement`; effect `Read`.
- `worktree.stack_status.read.v1`: CLI `worktree stack-status`; MCP
  `worktree_stack_status`; effect `Read`.
- `worktree.dependency_ready.read.v1`: CLI `worktree dependency-ready`; MCP
  `worktree_dependency_ready`; effect `Read`.
- `worktree.cross_merge.preview.v1`: CLI
  `worktree cross-merge dry-run`; MCP `worktree_cross_merge_dry_run`; effect
  `Preview`.
- `worktree.cross_merge.apply.v1`: CLI `worktree cross-merge apply`; MCP
  `worktree_cross_merge_apply`; effect `Write`.
- `worktree.cross_merge.status.v1`: CLI `worktree cross-merge status`; MCP
  `worktree_cross_merge_status`; effect `Read`.
- `worktree.cross_merge.cancel.v1`: CLI `worktree cross-merge cancel`; MCP
  `worktree_cross_merge_cancel`; effect `Write` with control capability.
- `worktree.conflict.list.v1`: CLI `worktree conflicts`; MCP
  `worktree_conflicts`; effect `Read`.
- `worktree.proximity.list.v1`: CLI `worktree proximity`; MCP
  `worktree_proximity`; effect `Read`.
- `worktree.receipt.get.v1`: CLI `worktree receipt`; MCP
  `worktree_receipt`; effect `Read`.

PR18 may change every CLI/MCP spelling above. It may not merge operations,
change their effect classes, remove semantic states, weaken authorization, or
reuse one operation ID for different behavior. No alias survives from the
internal profile unless PR18 explicitly freezes it with a compatibility
expiry.

### Typed identity and request contract

Every operation addresses canonical identity. A path, CWD, branch label,
active checkout, first workspace folder, host session, or task title is never
accepted as worktree identity.

```rust
pub struct WorktreeAddress {
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub worktree_id: WorktreeId,
    pub worktree_epoch: WorktreeEpoch,
    pub ref_id: Option<GitRefId>,
    pub head_commit: CommitId,
}

pub struct TaskPlacementRequest {
    pub task_id: TaskId,
    pub work_item_version_id: WorkItemVersionId,
    pub plan_version_id: WorkPlanVersionId,
    pub selection: TaskPlacementSelection,
    pub page: PageRequest,
}

pub struct StackStatusRequest {
    pub source: WorktreeAddress,
    pub task_id: Option<TaskId>,
    pub base_commit: CommitId,
    pub head_commit: CommitId,
    pub page: PageRequest,
}

pub struct DependencyReadyRequest {
    pub task_id: TaskId,
    pub work_item_version_id: WorkItemVersionId,
    pub plan_version_id: WorkPlanVersionId,
    pub worktree: WorktreeAddress,
    pub expected_readiness: Option<ReadinessDigest>,
}

pub struct CrossMergePreviewRequest {
    pub task_id: Option<TaskId>,
    pub source: WorktreeAddress,
    pub source_stack: NonEmptyVec<CommitId>,
    pub destination: WorktreeAddress,
    pub expected_merge_base: CommitId,
    pub expected_destination_generation: CodeGenerationId,
    pub readiness_digest: Option<ReadinessDigest>,
}

pub struct CrossMergeApplyRequest {
    pub preview_id: CrossMergePreviewId,
    pub preview_digest: CrossMergePreviewDigest,
    pub idempotency_key: IdempotencyKey,
    pub authorization_grant_id: AuthorizationGrantId,
}

pub struct CrossMergeStatusRequest {
    pub operation_id: CrossMergeOperationId,
    pub cursor: Option<OperationEventCursor>,
}

pub struct CrossMergeCancelRequest {
    pub operation_id: CrossMergeOperationId,
    pub expected_operation_version: CrossMergeOperationVersion,
    pub idempotency_key: IdempotencyKey,
}
```

Task placement returns exact Plan 24 task/item/plan versions, every authorized
`WorktreeId` and epoch, placement relation/revision, assignment versus
advisory-claim provenance, source/ref/commit identity, freshness, coverage,
and legal actions. It is many-to-many and never chooses a worktree by path or
branch name.

Stack status returns the exact ordered commit set between base and head,
commit-parent topology, ahead/behind/diverged state, per-commit TaskId joins
when independently authorized, changed symbol/file IDs, test and diagnostic
receipt IDs, destination relationship, conflict projection, readiness
projection, omissions, and coverage. It never treats a clean working tree,
process exit, test command text, or commit existence as task completion.

Dependency-ready is a Plan 24-derived view over the pinned plan/item version,
gating dependencies, acceptance prerequisites, Plan 32 runtime compatibility,
and the exact Plan 36 commit stack. It returns
`Ready { readiness_digest }`, `Blocked { reasons }`,
`Stale { expected, observed }`, or `NotExecutable { reason }`. CLI and MCP
cannot compute readiness, suppress an unknown gate, or turn incomplete
evidence into ready.

Conflict and proximity reads consume Plan 37's canonical advisory findings.
They preserve warning/finding identity, current/other authorized
`WorktreeId`, epoch, affected file/symbol IDs, relation paths, risk tier,
observed/expiry time, coverage, anchors, and suppression state. Hidden peers
collapse to a coarse `restricted_overlap` without identity, count, task,
branch, path, or timing disclosure.

### Cross-merge authority and state machine

Plan 21 owns only bindings and rendering. `cross_merge_apply` cannot enter any
callable profile until a coordinated Plan 36 owner change defines and tests
the daemon-owned `CrossMergeTransaction`; Plan 16 scope resolution and Plan 09
authorization/effect receipts are also prerequisites. Until that owner
contract exists, dry-run may return a read-only merge plan, but apply/status/
cancel must be absent rather than advertised as unsupported.
This family does not alias or widen the existing `git_preview`/`git_apply`
index-and-hunk bindings.

The owner contract is constrained here so adapter behavior is unambiguous:

- Dry-run resolves one source commit stack and one destination worktree/ref,
  constructs the merge through an isolated native-Git index, and returns
  exact base/source/destination identities, candidate tree/commit identity,
  conflicts, changed files/symbols, affected tests, required checks,
  policy/hooks/signing requirements, expiry, and an immutable preview digest.
  It mutates no ref, index, worktree, task, or runtime state.
- Apply requires explicit `CrossMergeApply` capability, the same authorized
  source/destination identities, an unexpired preview, exact source commit
  stack, destination epoch/head/ref/generation CAS, readiness digest when
  task-linked, and an idempotency key. The routed daemon serializes the
  effect per repository and destination worktree. Hooks, clients, source
  worktrees, and peer agents never write the destination.
- A preview with conflicts, missing dependency readiness, dirty/untracked or
  native in-progress destination state, stale epochs, incomplete required
  tests, denied source, or unsupported Git capability is not applicable.
  Apply performs no automatic conflict resolution, rebase, force, stash,
  checkout substitution, push, remote write, or history rewrite.
- Apply journals native-Git preparation, commit point, ref/worktree
  reconciliation, and final receipt. A process failure cannot be retried as a
  new mutation until reconciliation proves `Committed`, `NotCommitted`, or
  `EffectUnknown`.

```text
Previewed -> Queued -> Applying -> Committed
Previewed | Queued -> Cancelled
Applying -> Committed | Conflict | Stale | Failed | EffectUnknown
Applying + cancel -> CancelRequested -> Cancelled | TooLate | EffectUnknown
Conflict | Stale | Failed | Cancelled -> terminal
EffectUnknown -> ReconciledCommitted | ReconciledNotCommitted
```

Cancellation before the native commit point leaves destination state
unchanged and returns `Cancelled`. After the commit point the committed or
effect-unknown receipt wins; adapters cannot report cancellation as rollback.
Conflict is a typed terminal result with conflict IDs and safe affected
symbol/file IDs, not a partially successful merge or permission to edit
another worktree.

`CrossMergeReceiptV1` contains operation/preview/idempotency identities,
actor/transport/effect class, source/destination WorktreeIds and epochs,
ordered source commit IDs, base and before/after destination commit/tree/ref
identities, task/readiness identities when present, native Git/hook/signing
outcomes, conflict IDs, changed file/symbol IDs, test/diagnostic receipt IDs,
reconciliation state, timestamps, policy/config/catalog/privacy revisions,
coverage, warnings, and integrity digest. It contains no path, command, source,
diff body, log, prompt, task narrative, or peer-session content.

### Authorization, cursors, payloads, and latency

- Every selection first resolves Plan 16 scope and independently authorizes
  task, source worktree, destination worktree, ref/commit, finding, receipt,
  and operation. Possessing any ID, preview, cursor, or receipt grants nothing.
  Missing and denied resources are externally identical until the caller is
  authorized for the addressed relation.
- Placement, stack, dependency, conflict, proximity, status, and receipt are
  read capabilities. Dry-run is `Preview`; apply is `Write`; cancel is
  `Write` with a distinct `Control` capability. Neither `Read` nor `Preview`
  can be upgraded by a transport flag. Apply and cancel require explicit
  grants and fresh expected versions.
- `PageRequest` remains exactly `{ limit, cursor }`; limit is at most 100. A
  worktree cursor binds operation/capability, request digest, TaskId and
  work-item/plan versions when present, source/destination WorktreeId and
  epoch vector, ref/base/head/commit-stack digests, code generations,
  readiness digest, finding/event watermark, scope/grant and authorization
  epoch, catalog/schema/policy/config/privacy revisions, sort key, profile,
  and 15-minute expiry. Resume reauthorizes before cursor validation.
- Requests are at most 32 KiB. A normal page is at most 100 items and 256 KiB.
  A dry-run result is at most 1 MiB; larger diff/evidence detail returns total/
  preview/omitted counts plus a reversible response handle. Status streams
  queue at most 128 events or 256 KiB per client and expose a signed resume
  cursor; overflow produces an explicit gap and fresh-status action, never a
  silent suffix.
- On the checked-in warm fixture, placement/dependency/status/receipt reads
  are p95 <= 100 ms and p99 <= 250 ms; stack/conflict/proximity reads are p95
  <= 250 ms and p99 <= 750 ms; cross-merge dry-run is p95 <= 2 s and p99 <=
  5 s for a 100-commit/10,000-changed-line stack. Apply admission and cancel
  acknowledgement are p95 <= 100 ms and p99 <= 250 ms; mutation completion is
  reported asynchronously and has no fabricated client deadline guarantee.
- Daemon admission reserves control/receipt capacity so bulk stack or
  proximity reads cannot starve apply reconciliation, status, cancellation,
  diagnostics, or Doctor. Saturation is a typed problem with retry scope.

### Files, tests, and milestones

Owner types and handlers must exist before adapters bind:

- Plan 07:
  `crates/tracedecay-hooks/src/{event,binding,capabilities,transport,spool}.rs`
  and `src/daemon/hook_events/{mod,admission,replay,projector}.rs`.
- Plan 24/09:
  `crates/tracedecay-domain/src/work/{graph,projection}.rs` and
  `crates/tracedecay-application/src/work/{projection,commands}.rs` own
  TaskId placement and dependency-ready semantics.
- Plan 36/09:
  `crates/tracedecay-domain/src/git/cross_merge.rs`,
  `crates/tracedecay-application/src/git/cross_merge.rs`, and the native Git
  adapter own preview, apply, journal, reconciliation, cancellation, and
  receipts. Those owner files require the coordinated Plan 36 change above.
- Plan 37/09:
  `crates/tracedecay-domain/src/feedback/proximity.rs` and
  `crates/tracedecay-application/src/feedback/cycle.rs` own conflict/proximity
  findings.
- Plan 21 adapters:
  `src/cli/worktree.rs`,
  `src/mcp/tools/definitions/worktree.rs`,
  `src/mcp/tools/handlers/worktree.rs`, and the existing dispatch/render
  modules contain transport syntax and presentation only.
- Parity fixtures:
  `tests/mcp_suite/worktree_parity.rs`,
  `tests/core_cli_suite/worktree_output.rs`,
  `tests/worktree_operations_suite/{placement,stack,dependency_ready,cross_merge,conflict_proximity,receipts}.rs`,
  and `tests/fixtures/interface_output/human-v1/worktree/`.

Milestones are:

1. **M21.W1 — read model:** bind placement, stack, dependency-ready,
   conflict, proximity, and receipt reads after owner handlers pass; exit
   requires path-free identity, cursor/auth parity, and no adapter-local
   readiness.
2. **M21.W2 — preview:** bind cross-merge dry-run to the Plan 36 read-only
   plan; exit requires deterministic preview digest, conflict/test coverage,
   zero native mutation, and reversible truncation.
3. **M21.W3 — effect:** after the coordinated Plan 36 owner contract lands,
   bind apply/status/cancel; exit requires CAS, explicit grants, daemon-only
   writes, effect-unknown reconciliation, and cancel/commit race fixtures.
4. **M21.W4 — bounded rollout:** pass payload, cursor, saturation, latency,
   restart, and hidden-peer canaries in the internal profile.
5. **M21.W5 — PR18 freeze:** Plan 17 chooses public names and compatibility
   policy, adds them to discovery/help/SDKs, and removes or explicitly expires
   every internal spelling.

## Exact CLI/MCP primitive parity contract

PR12 uses one compiled binding record per callable application operation:

```rust
pub struct CliMcpBindingV1 {
    pub binding_id: BindingId,
    pub capability_id: CapabilityId,
    pub exposure: BindingExposure,
}

pub enum BindingExposure {
    PairedCliMcp { cli: CliBinding, mcp: McpBinding },
    CliOnly { cli: CliBinding, reason: AsymmetryReason, approval: ReviewId },
    McpOnly { mcp: McpBinding, reason: AsymmetryReason, approval: ReviewId },
}
```

The BindingId resolves UseCaseId, schemas, effect, pagination, cancellation,
deadline, receipt, authority, and privacy from the single Plan 08 capability
snapshot; this record cannot override or copy semantic fields. Plan 21
contributes CLI/MCP spellings and transport syntax, while Plan 08 owns the inert
record type, assembly, and validation.

For every capability selected into a declared paired CLI/MCP profile, a
callable retrieval, preview, command, or control capability has both bindings
or snapshot validation fails. The two bindings decode into the same Plan 09
request and `RequestContext`, call the snapshot's UseCaseId, and receive the
same `ApplicationResult<T>`. A larger CLI profile may omit a capability from an
MCP companion to satisfy that companion's reviewed schema/routing ceilings; the
omission is explicit profile membership, not missing parity. Context, work, and
operator MCP companions are projections of the one catalog, never copied
catalogs.
Every `CliBinding` stores its exact command path and every `McpBinding` stores
its exact tool name; aliases reference the same BindingId and carry an expiry
revision. Compiled tests reject duplicate spellings, empty names, separate
alias schemas/handlers, and unreviewed asymmetry. These CLI/MCP compatibility
names do not determine PR18 SDK method names.

Transport-local `--help`, `--version`, shell-completion generation, MCP stdio
connection lifecycle, MCP resources, and LSP lifecycle/notifications are not
callable application capabilities. No exception permits a business capability
selected into a paired profile to have only one adapter.

The exact PR12 core primitive families are:

- symbol: search, exact/qualified-name, signature, implementations, and type
  hierarchy;
- source: bounded lines, body, outline, module API, and file metadata;
- graph: callers, callees, call chain, file dependents, impact, and dependency
  depth;
- tests: test map and affected-test attribution;
- temporal: authorized session lookup, message search, shipped current/as-of
  session narrative, and Plan 13 anchor expansion; and
- operational: catalog, configuration, project, health, and storage/runtime
  state where the application operation is shipped.

PR17 extends paired profiles with TaskId/WorkItemId lookup and compact context,
current/as-of/evolution/forensic task history, thread/attempt traversal, Plan 24
proposal/context reads, and Plan 32 provider-capability and runtime inspection
only after their typed handlers ship. These bindings are absent from PR12
profiles, help, and discovery.

Each is a narrow mostly LLM-free application call. Context or planning
operations consume returned evidence packets explicitly; no primitive invokes a
model, chooses another binding, performs recursive dispatch, or creates
parallel work. Plan 24 owns declared task/retrieval fan-out and Plan 32 alone
admits and executes parallel branches. CLI and MCP only submit or inspect those
typed operations and receipts.

These family names and `BindingId`s are internal PR12/PR17 semantic identity.
PR18 retains sole authority to choose and freeze the public CLI command, MCP
tool, HTTP route, and Rust/TypeScript/Python SDK names for the PR17
cross-worktree family. PR12 does not derive SDK names from CLI commands or MCP
tools. Core names already public before this family retain their compatibility
policy; no temporary PR17 cross-worktree spelling becomes public merely
because the internal conformance profile exercises it.

## Canonical invocation, output, and controls

Both adapters construct the same transport-neutral invocation:

```rust
pub struct CanonicalInvocation<T> {
    pub request: T,
    pub scope: ScopeSelector,
    pub page: PageRequest,
    pub deadline: Option<Deadline>,
    pub cancellation: CancellationRef,
    pub requested_format: OutputFormat,
}
```

`requested_format` is removed before the application call and affects only
presentation. CLI accepts the common scope/page/deadline fields and maps
SIGINT/explicit cancellation to the invocation cancellation reference. MCP
maps the protocol request ID, cancellation notification, and negotiated
deadline to the same fields. The effective deadline is the earliest authorized
client, daemon, policy, or operation bound. Disconnect does not imply effect
rollback; the client reconnects with request/effect identity to inspect the
canonical receipt.
Cancellation or deadline expiry before daemon admission maps to Plan 09
`Cancelled { stage: BeforeAdmission }` or
`TimedOut { stage: BeforeAdmission }`: CLI uses the stable cancelled/timed-out
exit class and stderr view, while MCP uses the same problem envelope in
structured problem data and text content. After admission, both surfaces render
the canonical operation/effect receipt instead of a pre-admission problem.

`--json` and MCP structured content serialize the same schema-versioned Plan 09
success or problem envelope with stable enum tags, field meanings, number
representation, null rules, and deterministic collection order. Golden
fixtures compare canonical JSON values, not object insertion order.
CLI `--json` emits exactly one UTF-8 JSON object followed by one newline, with
no ANSI, Markdown, progress, log, or diagnostic bytes on stdout.
`HumanViewRevision` identifies one compact Markdown contract; CLI writes it as
terminal-safe Markdown/plain text and MCP returns the same view in text
content. Golden snapshots live under
`tests/fixtures/interface_output/human-v1/`. Human output must preserve result
contract revision, primary identity, temporal state, authorized scope class,
coverage, omissions, score kind/calibration, retriever contributions,
cursor/anchor, effect class, cancellation/deadline stage, receipt identity,
problem code, and legal next action whenever present. It may omit safe
zero/default decoration but cannot turn partial/unknown/denied/stale into empty
or successful.

Collections share Plan 09 `PageRequest` and `PageState`. The opaque
authenticated cursor is bound to capability/use case, request digest,
scope/grant digest and authorization epoch, temporal
horizon/snapshot/generation/watermark, catalog, result-schema, sort/ranking,
privacy, and redaction revisions, last sort key, profile, and expiry.
`PageRequest` is exactly `{ limit, cursor }`; resume permits no query, scope,
ordering, or profile change. CLI `--cursor` and MCP `cursor` carry the same
bytes. Resume reauthorizes before cursor validation and hydration;
authorization narrowing or scope/grant mismatch returns
`NotFoundOrNotAuthorized` before cursor validity or state is disclosed. Invalid,
expired, stale, and wrong-operation cursor codes are distinct only after the
caller is authorized for the addressed resource and operation.

Resource-addressed absent, out-of-scope, and policy-hidden requests are
externally identical across CLI JSON, CLI human output, MCP structured content,
MCP text content, exit class, protocol problem data, retry class, and legal
actions. They expose no count, cursor, provider/anchor/task state, alternative
binding, timing classification, or existence hint. Discovery omits
unauthorized capabilities rather than returning disabled entries.
Direct invocation of an unauthorized or profile-hidden command/tool is
indistinguishable from an unknown operation and returns no alternative binding
or profile hint.
Random, expired-without-authority, and real-but-unauthorized response handles
return the same non-disclosing handle-unavailable shape; only an authorized
handle may expose a distinct expiry or truncation state.

Previews use the cataloged `EffectClass::Preview` and Plan 09
`PreviewResult`/`OperationReceipt`; mutating commands use their cataloged effect
class and Plan 09 `EffectReceipt`. Adapter success is impossible without the
receipt contract named by the manifest. CLI exit and MCP result mapping preserve
`Completed`, `Cancelled`, `TimedOut`, `Failed`, `Partial`, and `EffectUnknown`
plus cancellation stage and reconciliation state. A transport timeout,
disconnect, or MCP cancellation acknowledgement cannot fabricate a terminal
application outcome. When completion, cancellation, and deadline race, the
daemon's committed terminal event wins; adapters never replace it with their
locally observed timeout or cancellation.

## Files, owners, and dependency order

PR12 changes only adapter and shared-client files:

- `Cargo.toml` — no new adapter-to-store dependency and no second
  schema/catalog crate;
- `src/daemon_client.rs` — one multiplexed client, request correlation,
  reconnect/receipt inspection, class-aware admission, and no business logic;
- `src/cli.rs` — command-tree composition and compatibility shims only;
- `src/cli/dispatch.rs` — `BindingId` resolution and canonical invocation;
- `src/cli/args/common.rs` — shared scope, page, cursor, deadline, cancellation,
  and format syntax;
- `src/cli/output/mod.rs` — presenter selection;
- `src/cli/output/json.rs` — canonical Plan 09 JSON serialization;
- `src/cli/output/markdown.rs` — deterministic compact human rendering;
- `src/cli/output/problem.rs` — stable stderr/exit mapping;
- `src/mcp/tools/dispatch.rs` — the same BindingId/request/result dispatch;
- `src/mcp/tools/render.rs` — MCP structured-content and text-content mapping
  from the canonical result;
- `src/mcp/transport.rs` — request ID, cancellation, deadline, lifecycle, and
  backpressure mechanics only;
- `src/mcp/response_handles.rs` — reversible oversized-response handles with
  authorization recheck and no anchor/task identity substitution;
- `src/cli/worktree.rs`,
  `src/mcp/tools/definitions/worktree.rs`, and
  `src/mcp/tools/handlers/worktree.rs` — PR17 internal cross-worktree transport
  decoding/rendering only, with no path resolution, readiness, conflict,
  merge, authorization, or receipt logic;
- `src/mcp/tools/handlers/admin_cli.rs` — deleted after the final family
  migration; and
- `tests/mcp_suite/main.rs` and
  `tests/mcp_suite/mcp_cli_parity_test.rs` — registered public semantic parity,
  paired-profile, cursor, and non-disclosure fixtures;
- `tests/core_cli_suite/main.rs` and
  `tests/core_cli_suite/output_contract.rs` — registered stdout/stderr/exit and
  human/JSON golden fixtures; and
- `tests/fixtures/interface_output/human-v1/` — reviewed human-view snapshots.

Family cutovers remove local orchestration from
`src/mcp/tools/handlers/analysis.rs`, `grep.rs`, `graph.rs`, `info.rs`,
`health.rs`, `admin_project.rs`, `session/message_search.rs`,
`session/sessions_for.rs`, `edit.rs`, `git.rs`, `workflow_query.rs`, and
`workflow.rs`, plus corresponding command branches in `src/cli.rs`,
`src/sessions_cmd.rs`, `src/status_cmd.rs`, `src/doctor.rs`,
`src/agent_cmd.rs`, and `src/automation_cli/mod.rs`. A handler file may remain
as transport decoding only; any migrated query, authorization, error, or
rendering branch is deleted in the same slice.

Plan 08 owns `CliMcpBindingV1`, profile/schema budgets, and snapshot validation.
Plan 09 owns requests, evidence/effect envelopes, cursors, errors, deadlines,
cancellation stages, stream events/frontiers/gaps/resume, and receipts. Plan 05
owns deterministic query ordering;
Plans 13/23 own anchors and temporal history. Plans 18/20 own privacy and
configuration. Plan 24 owns task graph/planning/fan-out intent; Plan 32 owns
workflow/auxiliary parallel runtime and effects; Plan 09 retains
operation-specific edit/Git transaction authority. Plan 21 owns only transport
decoding, daemon-client mechanics, and rendering. No adapter file may import a
store, query provider, planner/model client, scheduler, or effect
implementation. Plan 10 owns HTTP parity in
`tests/api_application_parity.rs`; Plan 35 owns LSP parity in
`tests/lsp_application_parity.rs`. Those dedicated test targets consume the same snapshot
but are not implementations of or exceptions to paired CLI/MCP profiles.

Dependency order is fixed:

1. Plan 08 lands inert IDs, manifest/binding record types, and builder
   validation without importing application.
2. Plan 09 lands packet/problem/cursor/receipt contracts, executable handlers,
   contributions, and fake-port tests against those record types.
3. Root-owned `src/catalog_composition.rs` assembles and validates the snapshot
   against Plan 09's closed validation-only
   `ApplicationHandlerDescriptors`; runtime dispatch continues through
   ordinary typed methods.
4. `src/daemon_client.rs` lands multiplexing, cancellation/deadline
   propagation, saturation mapping, and receipt reinspection.
5. CLI and MCP dispatchers migrate the read-only primitive families in this
   order: symbol, source, graph, tests, temporal, then operational. Each family
   must pass JSON, human, paging, cancellation/deadline, and authorization
   parity before its old handler-local path is deleted.
6. Preview and command families migrate only after preview/effect-receipt,
   idempotency, stale/CAS, cancellation-after-commit, and `EffectUnknown`
   fixtures pass.
7. Feedback diagnostics and PR17 task/work/runtime families bind last, after
   their owning application handlers exist; no stub or advertised unavailable
   method is counted as shipped.
8. Cross-worktree read and preview bindings follow M21.W1–W2. Apply, status,
   and cancel remain absent until the coordinated Plan 36 transaction owner
   passes M21.W3; internal rollout precedes the PR18 public-name freeze.
9. Delete `admin_cli` and duplicate render/query/error helpers, then enforce
   architecture and facade budgets. PR18 SDK bindings remain a later,
   independently reviewed gate.

## Public test matrix and migration gates

The following matrix is acceptance prose and executable fixtures, not a
generated inventory, runtime parity registry, or frozen public-name list.

- **Result states:** complete-empty, complete-nonempty, partial, unknown,
  unavailable, unsupported, stale, redacted, ambiguous, saturated, cancelled,
  timed out, failed, and effect-unknown.
- **Temporal/authority:** current/as-of/evolution/forensic; authorized,
  expired/narrowed grant, absent, out-of-scope, and policy-hidden with
  indistinguishable public failures.
- **Paging:** first/middle/final page, zero results, known/unknown total,
  concatenation equivalence, cursor expiry, scope/grant/schema/sort mismatch,
  authorization-epoch/catalog/ranking/privacy/redaction/profile mismatch,
  snapshot drift, random versus unauthorized response handles, handle expiry,
  and anchor expansion.
- **Evidence:** complete/partial/unknown coverage, every omission reason, each
  score kind and invalid calibration, multiple retriever contributions,
  deterministic ordering, and contribution/aggregate count agreement.
- **Control:** cancel before admission/read/effect, during read/effect,
  after-commit cancellation, deadline at each stage, disconnect/reconnect,
  resume suffix equivalence, suppression of late uncommitted data, and
  continued reconciliation/receipt publication for already-committed effects.
- **Streams:** monotonic event sequence/frontier, bounded resume, explicit
  gap/drop/truncation coverage, token expiry, backpressure, cancellation and
  deadline ordering, exactly one terminal event, and no post-terminal event.
- **Effects:** read/preview/write/admin classes, idempotent replay, mismatched
  idempotency input, stale CAS, conflict, durable receipt identity,
  reconciliation, partial, and unknown effect.
- **Presentation:** CLI JSON, CLI human/stdout/stderr/exit, MCP structured
  content, MCP text content, protocol problem, terminal-control/path/Markdown
  safety, no double encoding, and no irreversible truncation.

PR12 runs:

```bash
cargo test --test mcp_suite
cargo test --test core_cli_suite
cargo test --test api_application_parity
cargo test --test lsp_application_parity
cargo test --test architecture_boundaries
cargo check --all-features
```

Each family cutover is blocked unless every capability selected into a paired
profile has one CLI and one MCP binding to the same CapabilityId/UseCaseId and
schemas; canonical JSON values match; `HumanViewRevision` snapshots preserve
all continuation and safety fields;
concatenated pages and resumed suffixes match the pinned full execution;
cancellation/deadline and effect receipts agree; unauthorized and absent
resources, unknown versus hidden bindings, and stolen/wrong-scope cursors are
indistinguishable; adapters open no store or planner/runtime path; and the
replaced handler/query/renderer is deleted in the same migration slice. The
final gate also rejects any value above the paired profile's checked-in
`maximum_bindings`, `maximum_schema_bytes`, or `maximum_routing_tokens` unless
Plan 08 and Plan 21 owners approve the updated profile record and eager-client
fixture; a second discovery authority; duplicated workflow semantics; SDK
names published before PR18; or any CLI/MCP facade that works only through
deferred tool discovery.

## Rejected-argument telemetry

The catalog snapshot's schema-reference index and dispatcher own one
`interface_argument_rejected.v1` event for CLI, MCP, and HTTP. It is emitted
at the authoritative schema/dispatch rejection boundary, after syntax has
been separated into argument names and values and before the typed problem is
rendered. Adapters do not keep private counters or infer rejection telemetry
from stderr, protocol error text, or logs. A client-side rejection that cannot
reach the daemon is represented in telemetry coverage as unreported rather
than silently counted as zero.

The event contains only:

- the cataloged tool or command identity, or a bounded `unknown_operation`
  class when dispatch could not resolve one;
- normalized rejected argument names and the stable error class, such as
  unknown, misspelled, removed, misplaced, duplicate, or invalid shape;
- schema identifier and version, producer revision, transport, event time,
  trace identifier, and an idempotency key;
- normalized provider, model family, and agent-host kind when explicitly
  available from trusted connection metadata, with absence kept distinct
  from unknown.

Names are extracted without their prefix/value separator and must pass the
bounded argument-name grammar before recording. `--key=value` can record
`key`, never `value`; positional tokens, raw request payloads, error messages,
environment values, paths, hostnames, user identifiers, prompts, and provider
content are never copied. A name that fails privacy or grammar checks becomes
a stable rejection category plus a redacted-name count, not a raw token or a
reversible digest. The event path applies the shared privacy policy before
enqueue and is bounded, non-blocking, and explicit about dropped events.

Aliases are resolved only after the attempted spelling has been safely
classified, so future alias or schema decisions can compare a rejected name
with the active canonical schema without changing dispatch behavior. Event
emission cannot make an invalid request valid, alter its error, add a retry,
or delay the response. Aggregation and product read models are owned by
[26](26-observability-accounting-and-usage.md).

## Direct parity tests

Parity is verified from public behavior, not from a generated inventory:

1. invoke the same use case through CLI and MCP;
2. decode both canonical JSON results and compare semantic fields;
3. verify compact human output preserves identity, coverage, blockers, and continuation;
4. test missing daemon, stale client, denied scope, empty, partial, redacted, paged, oversized, cancelled, and failed states;
5. run concurrent-client tests proving clients never open writable databases;
6. test stdout, stderr, exit codes, MCP lifecycle, framing, cancellation, and reconnect behavior directly;
6a. drive the daemon to bulk-class saturation under concurrent clients and prove diagnostics/health requests still answer, saturated requests receive the typed saturation problem rather than a transport-level disconnect, and shed counts surface in observability;
7. submit equivalent unknown, removed, misplaced, duplicate, and invalid-shape arguments through CLI, MCP, and HTTP and assert one schema-identical rejection event per attempt;
8. prove values, payloads, paths, hostnames, identifiers, prompts, secrets, and unsafe names never enter events, logs, or typed problems, while redacted and dropped-event coverage remains visible;
9. verify replay/retry idempotency, unavailable provider/model/host metadata, bounded name/cardinality limits, and daemon-unavailable client rejection behavior;
10. run LSP lifecycle, negotiation, document-version, cancellation,
    notification/response separation, backpressure, daemon-failure, and direct
    upstream semantic-parity fixtures.
11. invoke `git_preview` and `git_apply` through CLI and MCP and compare their
    canonical semantic results, then prove compact Markdown and JSON preserve
    the same transaction identity, selected hunks, effect class, receipt, and
    typed stale/conflict state;
12. exercise concurrent index changes, real index-lock contention, preview CAS
    drift, retry idempotency, and forbidden generic/history-changing requests;
13. invoke `feedback_diagnostics`, `feedback_get`, `feedback_expand`, and
    `feedback_list` through CLI and MCP and compare canonical JSON semantics,
    then prove compact Markdown preserves finding IDs, coverage/state,
    continuation cursors, Plan 13 anchor references, and typed
    unavailable/budget/truncation outcomes;
14. prove oversized feedback results return reversible `tracedecay_retrieve`
    handles with original/preview counts and expiry while response handles
    remain distinct from durable finding IDs; security fixtures prove handles
    cannot bypass authorization or substitute for anchors.
15. in PR17, compare task/work graph, projection, history, task-shape,
    decomposition review, routing recommendation, live resize/re-route
    proposal, outcome/calibration, and runtime-control results across CLI, MCP,
    and HTTP; preserve confidence/coverage/abstention/censoring and prove lane
    or proposal decisions lower only to legal typed commands and cannot bypass
    dependencies or Plan 32 runtime/effect authority.
16. compare Plan 24 auxiliary-request and Plan 32 provider-adapter
    admission/progress/cancel/receipt results across CLI, MCP, and HTTP using
    fake and supported native streams; prove typed argv/stdin, deterministic
    native Claude Code and Codex app-server/allowed-CLI selection, exhaustive
    outcomes, malformed-output and version-drift handling, secret redaction,
    cancellation/kill escalation, restart/resume, and no client-side execution
    or recursive dispatch.
17. translate the pinned Hermes CLI/dashboard scenarios into surface parity:
    list/detail/dependency and derived-lane reads; proposal review; run/event
    tail; delegation visibility; blocker/capacity explanation; diagnostics,
    watch/statistics, skills/hints, artifacts, terminal receipt, cancellation,
    and restart recovery. Fixtures compare semantic results rather than copying
    Hermes command names, status writes, profile routing, or free-form fields.
18. prove metamorphic parity: concatenated pages equal the bounded full result;
    resume equals the uninterrupted suffix; cancellation suppresses later
    publication while reporting its true execution stage; redaction is
    monotonic; and TaskId summary expansion returns exact anchored evidence
    without granting authority.
19. compare canonical semantic identity, scope/generation, data/receipt,
    ordering/continuation, coverage/freshness/redaction, problem code, legal
    action, cancellation stage, and terminal state across CLI Markdown, MCP
    content, HTTP JSON, and LSP JSON-RPC. Byte-identical envelopes and pixels
    are not required.
20. in the PR17 internal profile, compare task placement, stack status,
    dependency-ready, conflict/proximity, cross-merge dry-run/apply/status/
    cancel, and receipt lookup across CLI and MCP. Exercise stale worktree
    epochs, hidden peers, cursor theft/drift, dirty destinations, conflicting
    stacks, missing tests, authorization revocation, daemon saturation/restart,
    cancel-before-commit, cancel-after-commit, effect-unknown reconciliation,
    and exact duplicate apply. Prove no request accepts path/CWD/active-root
    identity and no client or source worktree writes the destination.
21. hold the 100-commit/10,000-changed-line fixture to the request/result,
    queue, p95/p99, resume-gap, and reversible-truncation budgets above; fail
    if bulk reads starve receipt, cancellation, diagnostics, or Doctor traffic.

## PR12 core and PR17 extension deliverables

- one daemon client shared by CLI and MCP adapters;
- one daemon-owned stateful LSP gateway and transport-only stdio bridge;
- sealed application-result serialization;
- compact Markdown/terminal presenters for shipped use cases;
- canonical JSON and cursor/anchor handling;
- stable problem and exit mapping;
- canonical privacy-safe rejected-argument emission at the shared dispatcher;
- removal of handler-local database/query behavior, raw JSON renderers, double encoding, irreversible truncation, and writable fallback;
- removal of the `admin_cli` registry and session/analytics handler copies;
- focused CLI/MCP parity and concurrency tests.
- exactly `git_preview` and `git_apply` as shared-schema CLI/MCP Git bindings,
  with preview/CAS enforcement, typed receipts, and stale/conflict parity;
- read-only feedback diagnostics bindings (`feedback_diagnostics`,
  `feedback_get`, `feedback_expand`, `feedback_list`) with Plan 05 cursors,
  Plan 13 anchor expansion, and `tracedecay_retrieve` truncation handles.
- PR17 compact task/work graph and Plan 32 runtime bindings over the same typed
  application results, including reversible paging/expansion and explicit
  audience profiles.
- PR17 auxiliary request/provider capability and attempt lifecycle bindings
  with bounded streams/resources; no generic process-execution or raw
  provider-protocol proxy.
- PR17 internal paired task-placement, stack, dependency-ready,
  conflict/proximity, cross-merge preview/apply/status/cancel, and receipt
  bindings with path-free identity, exact effect classes, cursors,
  authorization, backpressure, and receipts; PR18 alone freezes their public
  names.

## Done

- One typed application result drives CLI and MCP output.
- CLI and MCP contain transport and presentation logic only.
- LSP uses typed application/query operations and protocol-native output; no
  bridge or gateway binding exposes a blind JSON-RPC proxy.
- All business access goes through the daemon.
- Compact Markdown and canonical JSON agree semantically.
- Direct tests cover every shipped binding and failure class touched by PR 12.
- Rejected arguments have CLI/MCP/HTTP parity without recording values or private payloads.
- Git preview/apply Markdown and JSON agree semantically, and no generic Git or
  autonomous ref/history mutation surface exists.
- Feedback diagnostics CLI/MCP/HTTP bindings agree semantically with host
  adapter projections at PR13, preserve truncation/cursor/anchor semantics,
  and never treat response handles as durable finding IDs.
- Cross-worktree CLI/MCP results agree semantically; readiness remains Plan 24
  derived, conflict/proximity remains advisory, and cross-merge writes are
  explicit daemon-authorized Plan 36 effects with preview/CAS/reconciliation.
  Paths and direct peer writes are absent from every schema.
- No generated surface inventory, developer-plan parser, independent task
  editor, or second executor is introduced.
