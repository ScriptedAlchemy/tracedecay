# TraceDecay V2 Incremental Context Scout and Suggestion Envelope Plan

**Plan 32 integration:** native workflow/run/node/history evidence may inform one bounded advisory suggestion under the existing authorization, relevance, dedupe, silence, and anchor gates. A suggestion never becomes workflow input, signal, required steering, scheduling/readiness authority, taskgraph compilation, or completion evidence; only an explicit authorized Plan-32 steering command can cross that boundary.

> Status: implementation-grade design; no production code is changed by this plan.
>
> Product rule: the scout is optional, asynchronous, advisory, evidence-bound, and silent by default. It may prepare context while an agent works, but it never makes a hook wait for a model or invents generic TraceDecay availability text.

## 1. Contract lock

1. `IncrementalContextScout` is an application-owned daemon workflow, not a second hint engine, a hook-embedded agent, or an MCP client recursively calling TraceDecay.
2. Canonical capture and projection complete independently of scouting. The scout consumes immutable canonical events and frozen read snapshots; it never becomes an ingest acknowledgement dependency.
3. The synchronous hook path performs no model call, search, graph traversal, remote request, or unbounded read. It only performs a bounded pending-envelope lookup, revalidation, rendering, and delivery within the existing hook budget.
4. No suggestion is the expected result when relevance, authority, freshness, privacy, novelty, timing, or budget is insufficient.
5. A deliverable suggestion is addressed to exactly one `ThreadId`, `TurnId`, `SessionId`, and `AgentId`. Unresolved or ambiguous identity is a suppression reason, never permission to deliver to the current or first session.
6. Every suggestion cites durable `RetrievalAnchorId`s and evidence/provenance. Model prose without anchors, tool receipts, and a pinned input manifest cannot enter a host prompt.
7. Model assistance is optional and capability-selected. A Codex app-server adapter may use a low-latency model such as Spark when the app server advertises it and configuration selects it; no model name, provider, executable, or fallback is hardcoded in domain or application code.
8. The model proposes bounded structured exploration and candidate semantics. The application authorizes and executes tools; pure policy ranks, deduplicates, suppresses, budgets, and selects; root `v2::hooks` renders and delivers. The model cannot call arbitrary shell, MCP, network, filesystem, mutation, or curation operations.
9. Read-only is the default and V2 launch maximum effect class. Local query/search/memory/LCM/code/Git/coordination reads are eligible only through cataloged application use cases. Remote read/egress requires an explicit grant and remains separately labeled.
10. Suggestions are compact, specific, and non-overbearing. Static capability boilerplate, repeated categories, restated user prompts, vague advice, and uncited generated claims are invalid output.
11. Late, stale, expired, superseded, redacted, or already-observed suggestions are recorded with a disposition and never silently injected into a later logical message.
12. The scout shares the canonical hint state (plan 6's `HintStateSnapshot`, fields in [06-policy-crate.md](./06-policy-crate.md) §9.1.2), policy, catalog, configuration, presentation, delivery receipt, and outcome pipeline. It does not create parallel counters, renderers, model settings, or dashboard-only state.
13. Replay, shadow, A/B evaluation, and “what would happen now?” are read-only. They never deliver, mutate counters, affect dedupe, or create per-item approval/apply/rollback flows.
14. Existing deterministic hint classifiers remain a candidate source during migration, then move behind the same policy contract. There is one delivery selector at cutover: plan 6's `DeliveryArbiterV1` ([06-policy-crate.md](./06-policy-crate.md) §9.1.3), which arbitrates deterministic and scout `DeliveryCandidateV1` submissions under one `HintStateSnapshot` version compare-and-swap.
15. Canonical initiative/task/ticket/dependency/claim/context-packet events are eligible evidence, not a broadcast feed. A sibling-task change reaches an exact Thread/Turn only when evidence proves material overlap, a blocker, a handoff, or an invalidated assumption.
16. Scout wakeups/coalescing/backoff/fairness use plan 09's one `SchedulerKernelV1`; each admitted run wraps `OperationKernelV1<ScoutRunKind>` for epoch, heartbeat, steps, progress, cancellation, retry/takeover, and terminal receipts. Scout owns trigger/materiality/model/exploration/envelope policy only and cannot add another scheduler or job ledger.
17. A scout suggestion is never steering. Plan 01's `SteeringDirectiveV1` is an authorized command with an explicit `TaskAttempt`, `WorkflowRun`, or `WorkflowNode` target; Plan 24 owns task lifecycle fences and Plan 32 owns workflow lifecycle. It has independent delivery, acknowledgement, disposition, revision, limit, and required-fence semantics. The scout cannot create, promote, rewrite, rank, suppress, acknowledge, resolve, deflect, or masquerade as a steering directive. A human/controller comment remains a shared annotation until explicitly promoted through the owning lifecycle command.

This plan extends the contracts in [01-domain-crate.md](./01-domain-crate.md), [06-policy-crate.md](./06-policy-crate.md), [07-hooks-crate.md](./07-hooks-crate.md), and [09-application-crate.md](./09-application-crate.md). Configuration is exclusively owned by [20-configuration-control-plane.md](./20-configuration-control-plane.md); plan 09 owns semantic response views while capability, binding, rendering, and format parity are exclusively owned by [21-cli-mcp-tool-surface-and-output-unification.md](./21-cli-mcp-tool-surface-and-output-unification.md); every message/LCM/context read and current/as-of decision is owned by [23-session-lcm-temporal-retrieval-and-evaluation.md](./23-session-lcm-temporal-retrieval-and-evaluation.md) through the sole `TraceQueryV1` path; canonical task refs/events are owned by [24-canonical-task-plan-graph-and-multi-agent-executor.md](./24-canonical-task-plan-graph-and-multi-agent-executor.md).

## 2. Product objective and non-goals

### 2.1 Objective

TraceDecay should behave like an ambient context brain without flooding the working agent. As events accumulate during a Turn, the daemon incrementally asks a bounded question: “Is there one new, well-supported piece of context that would materially improve the next safe action?” It may:

- recover a directly relevant prior Turn, memory, fact, LCM summary, or research anchor;
- find a code symbol, call/impact path, diagnostic, Git change, branch, worktree, PR, or delivery fact that resolves the current uncertainty;
- notice a nearby agent or parallel worktree already doing materially overlapping work;
- connect the exact current task to a cross-repository initiative, dependency, sibling-task claim, blocker, handoff, or newly published context packet;
- connect the current Turn to parent/subagent/session/workflow/goal evidence;
- identify that the semantic index, scope resolution, or live/local Git join is stale or incomplete and propose one exact recovery action;
- recommend one cataloged TraceDecay capability when its use has high expected value and has not already been used;
- prepare an authorized retrieval bundle before the next provider injection boundary.

The result is a compact typed envelope with exact addressee, expiry, evidence, retrieval anchors, reason, and delivery policy. A custom suggestion may be model-assisted, but its truth comes from authorized evidence and receipts rather than model confidence alone.

### 2.2 Non-goals

- No autonomous edit, commit, branch, PR, message, curation, schedule, configuration, or external-system mutation.
- No second general-purpose agent runtime or unrestricted tool loop.
- No attempt to inject continuously while a host has no supported steering boundary.
- No authoritative attempt steering, comment promotion, acknowledgement, disposition, completion fence, or steering retry. Those are canonical task commands, not generated suggestions.
- No replacement for host compaction, memory curation, search, or the deterministic policy engine.
- No inference that temporal adjacency proves causation, adoption, or agent ownership.
- No raw transcript mirror, secret-bearing prompt cache, or model-output search index.
- No global “helpfulness” stream. A suggestion must be materially related to the exact current Turn.
- No manual queue where operators approve, apply, reject, or roll back individual scout results.
- No requirement that an app server, Spark, a network, or any model is available. Deterministic-only and fully disabled modes are first-class.

## 3. Canonical ownership and dependency DAG

Do not create a new crate. Scouting has one deployment consumer, and its invariants already belong to domain, store, query, policy, hooks, catalog, application, API, and root composition. A `tracedecay-scout` crate would hide cycles rather than create a coherent independent boundary.

| Concern | Semantic owner | Runtime owner | Forbidden duplication |
|---|---|---|---|
| Scout/suggestion IDs, states, schemas, reason codes | `tracedecay-domain` | pure validation | root/daemon string enums |
| Trigger source evidence | capture/projectors | canonical event journal/outbox | hook-private event history |
| Checkpoints, runs, candidates, envelopes, receipts | `tracedecay-store` repositories | activity-owner shard | dashboard JSON files |
| Search/LCM/memory/code/Git reads | query/application use cases | injected application ports | recursive local MCP calls |
| Model-neutral candidate policy | `tracedecay-policy` | pure pinned evaluation | model prompt thresholds |
| Capability eligibility/effect class | tool catalog | immutable catalog snapshot | handwritten allowlists in daemon |
| Incremental orchestration | `tracedecay-application` | daemon worker calls application service | hook or API business logic |
| Model gateway contract | application consumer-owned port | root adapter, initially Codex app-server optional | automation-backend reuse by import |
| Safe delivery handshake | hooks/application | host adapter plus daemon session | fake user messages |
| HTTP/SSE/OpenAPI | API | thin generated transport | private dashboard routes |
| CLI/MCP/SDK presentation | plan 21 generated bindings/renderers over plan 09 typed views | thin root/public adapters | scout-specific renderer |
| Dashboard interaction | Brain frontend | generated client and plan 09 typed views | browser-side ranking |

```mermaid
flowchart LR
    Host["Host events"] --> Hooks["root v2::hooks adapter"]
    Hooks --> Capture["tracedecay-capture"]
    Capture --> Store["tracedecay-store journal"]
    Store --> Projectors["tracedecay-projectors"]
    Projectors --> Outbox["immutable event outbox"]
    Outbox --> App["application ContextScoutService"]
    App --> Query["query and retrieval ports"]
    App --> Catalog["tool catalog snapshot"]
    App --> Gateway["ModelGatewayPort"]
    Root["root composition"] --> Gateway
    App --> Policy["pure scout and hint policy"]
    Policy --> Envelope["durable suggestion envelope"]
    Envelope --> Delivery["bounded host delivery lookup"]
    Delivery --> Hooks
    App --> API["HTTP/SSE/generated clients"]
    API --> UI["Brain Observatory, Loom, Hint Lab, Settings"]
```

Allowed edges remain those in [19-system-defragmentation-convergence-and-extensibility.md](./19-system-defragmentation-convergence-and-extensibility.md): application composes store/query/policy/catalog ports; hooks depend only on narrow application ports; root supplies concrete process/model adapters. Policy never imports a model SDK, query implementation, store, clock, or network. The model gateway never imports hook rendering or repositories.

## 4. Domain contracts

Add pure contracts under `crates/tracedecay-domain/src/scout/` and export them through the versioned schema registry. IDs follow the deterministic encoding and privacy-domain rules in [01-domain-crate.md](./01-domain-crate.md), and `CoverageReportV1` is the canonical shared coverage type owned there.

```text
crates/tracedecay-domain/src/scout/
├── mod.rs
├── ids.rs
├── trigger.rs
├── address.rs
├── run.rs
├── tool.rs
├── candidate.rs
├── envelope.rs
├── delivery.rs
├── outcome.rs
├── checkpoint.rs
├── status.rs
└── reason_codes.rs
```

```rust
pub struct ScoutRunId(pub uuid::Uuid);
pub struct SuggestionCandidateId(pub uuid::Uuid);
pub struct SuggestionEnvelopeId(pub uuid::Uuid);
pub struct SuggestionDeliveryId(pub uuid::Uuid);
pub struct ModelInvocationId(pub uuid::Uuid);
pub struct LogicalMessageId(pub EntityId);
pub struct ScoutConsumerId(pub NativeKindCode);

// Imported unchanged from tracedecay-domain; this plan defines no task-local refs:
// WorkItemVersionRefV1, WorkClaimRefV1, ContextPacketManifestRefV1.

pub struct SuggestionAddressV1 {
    pub profile_id: ProfileId,
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub logical_message_id: LogicalMessageId,
    pub host_profile: HostProfileRef,
    pub host_surface: HostSurfaceKindV1,
    pub source_instance_id: SourceInstanceId,
}
```

All address fields are mandatory for `Deliverable`. Provider aliases and path strings are never used as delivery addresses. A projector may retain an unresolved trigger for coverage, but the application returns `SuggestionSuppressionReason::UnresolvedAddressee` until exact entities resolve.

`LogicalMessageId` deduplicates provider events that represent the same semantic instruction or result without collapsing legitimate repetitions:

- direct provider message ID is the strongest key;
- copied parent prompt/subagent instruction uses the captured origin relation plus parent `MessageId`, target agent, and delegation generation;
- tool-call/result protocol uses invocation ID and direction;
- transcript rewrites retain the logical ID but create a new observation generation and superseding event evidence;
- digest-only fallback is privacy-domain-keyed over normalized origin, bounded sanitized content digest, address, and source generation;
- identical text in different Turns or agents is not automatically one logical message;
- provider protocol wrappers around one event do not each consume a hint budget.

### 4.1 Trigger and snapshot

```rust
pub enum ScoutTriggerKindV1 {
    TurnOpened,
    UserMessageObserved,
    DelegatedPromptObserved,
    AgentSpawned,
    GoalChanged,
    ToolPlanned,
    ToolCompleted,
    ToolFailed,
    FileRead,
    FileEdited,
    DiagnosticObserved,
    GitStateChanged,
    WorkClaimChanged,
    TaskChanged,
    TaskDependencyChanged,
    TaskOfferChanged,
    TaskLeaseChanged,
    ContextPacketPublished,
    TaskHandoffObserved,
    ScopeChanged,
    CompactionBoundary,
    TurnIdle,
    TurnClosing,
}

pub struct ScoutTriggerV1 {
    pub trigger_event_id: EventId,
    pub kind: ScoutTriggerKindV1,
    pub address: Option<SuggestionAddressV1>,
    pub scope: ScopeSelectorV2,
    pub source_sequence: Option<u64>,
    pub occurred_at: Option<UtcMicros>,
    pub ingested_at: UtcMicros,
    pub event_watermark: VectorWatermark,
    pub continuity: SourceContinuity,
    pub sensitivity: DataSensitivity,
    pub sanitization_receipt: SanitizationReceiptId,
}

pub struct ScoutInputManifestV1 {
    pub run_id: ScoutRunId,
    pub trigger_ids: Vec<EventId>,
    pub address: SuggestionAddressV1,
    pub scope_resolution_id: ScopeResolutionId,
    pub frozen_snapshot: FrozenSnapshot,
    pub access_policy_digest: AccessPolicyDigest,
    pub config_snapshot_id: EffectiveConfigSnapshotId,
    pub policy_bundle_id: PolicyBundleId,
    pub tool_catalog: CatalogSnapshotRefV1,
    pub hint_state_version: EntityVersionId,
    pub model_capability: Option<ModelCapabilityRefV1>,
    pub evaluation_time: UtcMicros,
    pub deadline: UtcMicros,
}
```

`WorkClaimChanged` is advisory agent-presence evidence; `TaskOfferChanged` is non-authoritative routing; `TaskLeaseChanged` is fenced execution authority. No trigger or classifier uses the ambiguous name `TaskClaimChanged`. Schema generation and replay exhaustively map every closed plan-24 offer/lease/claim event class or fail the build.

The input manifest references authorized, sanitized data; it does not inline prompts, tool arguments/results, paths, environment values, or model credentials. Every input lane reports complete/partial/stale/locked/redacted/unavailable coverage.

### 4.2 Bounded model/tool protocol

```rust
pub enum ScoutToolIntentV1 {
    SearchMessages,
    ExpandLcm,
    RetrieveMemory,
    SearchCode,
    TraceFunctions,
    AssessImpact,
    InspectGit,
    InspectDelivery,
    FindSessions,
    InspectWorkflow,
    FindNearbyWork,
    InspectTaskContext,
    InspectTaskDependencies,
    FindMaterialSiblingChanges,
    ResolveScope,
}

pub struct ScoutToolProposalV1 {
    pub ordinal: u16,
    pub intent: ScoutToolIntentV1,
    pub capability_id: CapabilityId,
    pub typed_request: SchemaBoundValueRef,
    pub expected_information_gain: ScoreMicros,
    pub rationale_code: ScoutRationaleCode,
}

pub struct ScoutToolReceiptV1 {
    pub ordinal: u16,
    pub capability_id: CapabilityId,
    pub request_digest: PrivacyDomainBoundLocatorDigest,
    pub result_anchor_ids: Vec<RetrievalAnchorId>,
    pub coverage: CoverageReportV1, // canonical shared coverage type from 01-domain-crate.md
    pub started_at: UtcMicros,
    pub completed_at: UtcMicros,
    pub status: ScoutToolStatusV1,
    pub bytes_read: u64,
    pub tokens_exposed_to_model: u32,
}
```

The model returns a schema-validated proposal. The application rejects unknown capability IDs, mutation effects, widened scope, raw string selectors, excessive result fields, unavailable catalogs, or tools outside the active grant. A proposal is not execution authority.

### 4.3 Candidate and envelope

```rust
pub enum SuggestionKindV1 {
    Retrieval,
    PriorContext,
    CapabilityRoute,
    CodeOrImpactEvidence,
    GitOrDeliveryEvidence,
    NearbyAgentWork,
    TaskDependencyOrHandoff,
    ScopeOrFreshnessGap,
    ContradictionOrRisk,
}

pub struct SuggestionCandidateV1 {
    pub candidate_id: SuggestionCandidateId,
    pub run_id: ScoutRunId,
    pub address: SuggestionAddressV1,
    pub kind: SuggestionKindV1,
    pub intent: IntentId,
    pub primary_anchors: Vec<RetrievalAnchorId>,
    pub supporting_evidence: Vec<EvidenceRef>,
    pub proposed_text: Option<PromptEligibleText>,
    pub proposed_capability: Option<CapabilityId>,
    pub proposed_scope: Option<ScopeSelectorV2>,
    pub source_confidence: Confidence,
    pub model_confidence: Option<Confidence>,
    pub created_at: UtcMicros,
    pub not_before: UtcMicros,
    pub expires_at: UtcMicros,
}

pub struct SuggestionEnvelopeV1 {
    pub envelope_id: SuggestionEnvelopeId,
    pub address: SuggestionAddressV1,
    pub sequence: u64,
    pub state: SuggestionEnvelopeStateV1,
    pub version: EntityVersionId, // claim CAS fields; the Section 16 store row transacts on state/version
    pub kind: SuggestionKindV1,
    pub category: HintCategoryId,
    pub payload: PromptEligibleText,
    pub payload_digest: SanitizedOutputDigest,
    pub retrieval_anchor_ids: Vec<RetrievalAnchorId>,
    pub evidence_refs: Vec<EvidenceRef>,
    pub policy_evaluation_id: PolicyEvaluationId,
    pub scout_run_id: ScoutRunId,
    pub input_manifest_id: ManifestId,
    pub provenance_id: ProvenanceId,
    pub delivery_policy: SuggestionDeliveryPolicyV1,
    pub created_at: UtcMicros,
    pub eligible_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub input_watermark: VectorWatermark,
    pub explanation: DecisionExplanation,
    pub diagnostic: Option<DiagnosticEnvelopeV1>,
}
```

`model_confidence` is diagnostic only and never substitutes for evidence authority. `source_confidence`, scope coverage, anchor freshness, and policy score govern delivery. Model text passes the plan 18 output firewall, bounded-text validation, citation coverage, prohibited-content scan, and compactness check. Prefer deterministic rendering from structured facts; retain custom text only when it adds specific anchored meaning.

When the suggestion carries a recovery/remediation/coordination action, `diagnostic` uses the domain type defined by plan 01 and the shared semantics in plan 24 §4.11. It may contain at most three actions after scout narrowing; each capability/effect/input schema must already be in the exact addressee's current grant/catalog and remains subject to application revalidation. The compact prompt renders at most one enabled action label plus anchors; unknown/disabled kinds remain dashboard/inspector evidence and are never emitted as executable free text. Suggestions without a diagnostic remain valid evidence/retrieval envelopes; no empty diagnostic is fabricated.

### 4.4 Lifecycle and reason codes

```rust
pub enum ScoutRunStateV1 {
    Coalescing,
    Snapshotting,
    DeterministicPlanning,
    ModelPlanning,
    ExecutingReads,
    Ranking,
    Persisting,
    CompletedSilent,
    CompletedWithEnvelope,
    Cancelled,
    Failed,
}

pub enum SuggestionEnvelopeStateV1 {
    Pending,
    Eligible,
    ClaimedForDelivery,
    Delivered,
    Suppressed,
    Expired,
    Superseded,
    DeliveryUnknown,
}

pub enum SuggestionSuppressionReasonV1 {
    NoMaterialOpportunity,
    UnresolvedAddressee,
    AmbiguousScope,
    PartialUnsafeCoverage,
    PrivacyDenied,
    BelowRelevanceThreshold,
    BelowEvidenceThreshold,
    AlreadyObserved,
    DuplicateLogicalMessage,
    RepeatedCategory,
    Cooldown,
    TurnBudgetExhausted,
    SessionBudgetExhausted,
    TokenBudgetExhausted,
    CostBudgetExhausted,
    Stale,
    Late,
    Expired,
    Superseded,
    HostCannotDeliverSafely,
    ScoutPaused,
    ModelUnavailableAndNoDeterministicCandidate,
    Overloaded,
}
```

Reason codes are closed, cataloged, safely rendered, and metric dimensions. Arbitrary model explanations never become status codes. `SuggestionSuppressionReasonV1` is the one suppression registry for both delivery engines: plan 6's `SuppressionReason` re-exports exactly this set, so deterministic hints and scout envelopes suppress with the same codes and a new variant is a versioned enum revision recorded in [06-policy-crate.md](./06-policy-crate.md).

`ScoutRunStateV1` is a domain phase projection over a linked `OperationKernelV1<ScoutRunKind>`, not an independent lifecycle. Coalescing through Persisting are scout phase codes; generic running/waiting/cancelling/succeeded/failed/blocked state, epoch/heartbeat, current step, progress, cancel intent, retry/takeover, and receipts come only from the operation kernel. `CompletedSilent` and `CompletedWithEnvelope` are typed scout terminal dispositions on a generically succeeded operation.

## 5. Event stream, checkpoints, and incremental scheduler

The scout registers one independent outbox consumer group, `context_scout.v1`. It never shares checkpoint state with projectors, hint outcome jobs, automation, or dashboard subscriptions, but it reuses plan 01/02's sole mechanical `OutboxConsumerLeaseV1<K>`/`OutboxConsumerCheckpointV1<K>` epoch/CAS codec and tables. Outbox wakeups enter plan 09's `SchedulerKernelV1`, whose scout policy supplies coalescing windows, priorities, per-address fairness, backpressure/silence, and admission budgets. The kernel supplies the queue, timers, backoff, checkpoint wakeups, and fenced admission; no scout-local poller or fairness queue exists.

```rust
pub struct ScoutConsumerKeyV1 {
    pub consumer_id: ScoutConsumerId,
    pub consumer_version: ComponentVersion,
    pub shard_id: ShardId,
    pub generation: ProjectionGenerationId,
}

pub struct ScoutCheckpointExtensionV1 {
    pub config_snapshot_id: EffectiveConfigSnapshotId,
}

pub struct ScoutTurnStateV1 {
    pub address: SuggestionAddressV1,
    pub last_trigger_sequence: u64,
    pub coalesce_generation: u64,
    pub active_run_id: Option<ScoutRunId>,
    pub queued_trigger_ids: Vec<EventId>,
    pub prior_category_state: HintStateSnapshot,
    pub last_envelope_fingerprint: Option<PrivacyDomainKeyedFingerprintV1>,
    pub closed_at: Option<UtcMicros>,
    pub version: EntityVersionId,
}
```

The persisted checkpoint is `OutboxConsumerCheckpointV1<ScoutConsumerKeyV1>` plus `ScoutCheckpointExtensionV1` in the same transaction. `prior_category_state` is the shared `HintStateSnapshot` of [06-policy-crate.md](./06-policy-crate.md) §9.1.2 — it carries the full logical/semantic/anchor/coordination-pair fingerprint sets, per-category cooldown clocks, turn/session/scout/token ledgers, pending-suggestion slot, and CAS version token. `last_envelope_fingerprint` is only a fast-path check on the most recent payload, never the dedupe state itself.

### 5.1 Trigger eligibility

Deterministic trigger rules run before snapshot/model allocation:

- high-value immediate: user/delegated prompt, tool failure, diagnostic, material scope change, agent spawn, work-claim overlap, task blocker/handoff, or dependency change that invalidates an active assumption;
- medium-value coalesced: tool completion, file read/edit, Git state change, goal update;
- task-graph coalesced: ordinary task/claim/context-packet updates remain silent unless a bounded relevance join proves an exact current-task dependency, overlap, blocker, handoff, or invalidated assumption;
- boundary: turn idle, pre-compaction/post-compaction, closing;
- non-triggering by default: heartbeats, progress ticks, repeated reads, renderer activity, analytics writes, scout/hint outcome events, and the scout's own tool activity.

The registry declares whether an event family may trigger and its default debounce class. Self-generated event exclusion is based on producer/correlation IDs, not fragile name prefixes.

### 5.2 Coalescing and cancellation

Partition state by `(ProfileId, ThreadId, AgentId)` and serialize scheduling per partition while allowing profile-wide bounded concurrency.

1. Append every canonical event normally.
2. Record its scout eligibility/disposition without blocking the originating outbox consumer.
3. Coalesce eligible events for the same Turn over a configurable 75–300 ms quiet window, capped by a 750 ms maximum wait.
4. Merge only rebuildable trigger summaries; retain all source event IDs in the run manifest.
5. A newer user message, scope generation, Turn closure, or access/config/policy generation invalidates an older queued run.
6. Cancel active model/tool work at declared boundaries. A cancellation cannot retract a delivered envelope; it may supersede only pending envelopes.
7. Maintain at most one active run plus one coalesced queued generation per address.
8. Fair scheduling uses per-profile, project, worktree, and agent token buckets so one noisy agent cannot starve others.

Cancellation checks occur before snapshot acquisition, before and after every model request, before each tool execution, every query page, before policy evaluation, before persistence, and before delivery claim.

### 5.3 Backpressure

| Tier | Condition | Behavior |
|---|---|---|
| 0 normal | queue/age/cost below targets | deterministic plus configured model path |
| 1 coalesce | transient burst | extend within max debounce; collapse rebuildable triggers by address |
| 2 supersede | old runs behind current Turn | cancel obsolete generations; record `Superseded` |
| 3 deterministic-only | model queue, provider, or cost pressure | skip model; evaluate deterministic candidates only |
| 4 optional-work shed | disk/CPU/store/query pressure | record `Overloaded`; remain silent; preserve canonical capture |
| 5 paused/circuit-open | operator/config/health decision | advance examined checkpoint with explicit paused disposition; no scouting |

The scout must never retain an outbox transaction or writer lock while waiting. It may advance `last_examined_sequence` only after a durable disposition exists. Runs accepted for work are idempotently keyed by address, coalesce generation, trigger-set digest, config/policy/catalog digests, and frozen watermark.

### 5.4 Restart and gap recovery

- Lease takeover compares epoch and resumes only committed checkpoints.
- An incomplete run resumes from a durable phase receipt or is deterministically cancelled and recreated; it never repeats a delivered envelope.
- Source gaps or rewrites block confident delivery until reconciliation. Historical runs remain evidence and acquire supersession links.
- A large lag is not replayed as current advice. Catch-up records historical eligibility for evaluation, expires old envelopes without model work, and begins live scouting at a declared watermark.
- Checkpoint corruption isolates the scout consumer; it does not block capture/projectors. Repair verifies run/envelope/delivery references before promotion.
- Disk full disables optional work before canonical ingest reserves are consumed.

## 6. Incremental context construction

The application builds a bounded, typed context ledger rather than repeatedly submitting the whole Turn.

```rust
pub struct ScoutContextDeltaV1 {
    pub address: SuggestionAddressV1,
    pub new_event_refs: Vec<EventId>,
    pub current_goal_refs: Vec<EntityRef>,
    pub observed_capability_events: Vec<EventId>,
    pub changed_scope_refs: Vec<ScopeResolutionId>,
    pub current_work_claim_refs: Vec<EntityRef>,
    pub current_task_refs: Vec<WorkItemVersionRefV1>,
    pub relevant_task_claim_refs: Vec<WorkClaimRefV1>,
    pub context_packet_refs: Vec<ContextPacketManifestRefV1>,
    pub prior_suggestion_fingerprints: Vec<PrivacyDomainKeyedFingerprintV1>,
    pub coverage: CoverageReportV1,
}
```

Construction order:

1. Resolve the exact address and immutable `ScopeResolutionV2` at the trigger watermark.
2. Load only the Turn delta since the prior successful run plus a bounded state summary of goals, tool intents/results, touched symbols/files, Git/worktree state, active claims, and prior delivered categories.
3. Apply authorization and plan 18 sink policy before content hydration.
4. Produce deterministic hypotheses from event kind, catalog routes, missed-capability rules, scope/freshness health, nearby-work rules, and unresolved context gaps.
5. Retrieve candidate anchors only when a hypothesis needs evidence.
6. Invoke the optional model only when deterministic policy predicts enough information gain to justify model cost and no suppression already applies.
7. Expose to the model a structured evidence digest plus bounded sanitized anchor excerpts, never the unbounded transcript or full tool response.
8. After each approved read, add only novel typed facts/anchors to the run context and recompute whether another step has positive value.

Default run limits:

- 3 planning rounds, including the final candidate round;
- 4 tool reads total, at most 2 from one capability family;
- 8 retrieval anchors considered, 3 eligible in a final envelope;
- 8,192 model-input tokens and 256 model-output tokens per run;
- 2 seconds soft and 8 seconds hard wall time in low-latency mode;
- no more than one model run per Turn unless a materially new direct-user message or tool failure opens a new logical opportunity.

All values are configuration descriptors with floors/ceilings, not constants duplicated in the worker.

## 7. Tool sandbox and effect policy

Catalog metadata from [08-tool-catalog-crate.md](./08-tool-catalog-crate.md) adds the following struct; plan 8 owns and reserves `ScoutCapabilityPolicyV1` in its catalog crate, and this section states its required fields:

```rust
pub struct ScoutCapabilityPolicyV1 {
    pub capability_id: CapabilityId,
    pub eligibility: ScoutEligibilityV1,
    pub effects: EffectSpec,
    pub evidence_source: EvidenceSourceRequirement,
    pub allowed_sensitivity: BTreeSet<DataSensitivity>,
    pub max_requests_per_run: u16,
    pub max_result_items: u32,
    pub max_result_bytes: u64,
    pub timeout: Duration,
    pub egress: EgressClassV1,
}
```

Launch rules:

- only a `UseCaseDefinition.effects` contract validated as read-only is scout-eligible;
- local application/query reads are eligible by default;
- live Git/delivery reads require `scout.tools.remote_reads.enabled`, repository authorization, egress grant, credential-reference availability, and freshness need;
- shell, file mutation, Git mutation, PR/comment/message mutation, config mutation, curation, automation, credential access, exports, restore, repair, and arbitrary MCP invocation are ineligible;
- model-requested scope must be equal to or narrower than the resolved run scope; cross-project expansion requires a preauthorized declared scope from [16-cross-project-repository-worktree-scope.md](./16-cross-project-repository-worktree-scope.md);
- result fields are generated projections approved for model input, not transport Markdown/JSON;
- timeout, cancellation, coverage, redaction, truncation, retrieval anchors, and cost become receipts available to policy;
- tool errors become typed evidence gaps. Their literal messages are not echoed into suggestions.

The model never receives CLI syntax or raw MCP schemas as its tool authority. `ScoutToolExecutorPort` accepts a catalog capability plus its generated typed application request. This prevents rendering drift, ambient-CWD mistakes, missing project scope, and recursive daemon calls.

Recommended launch families:

| Need | Eligible application capability | Typical output |
|---|---|---|
| prior intent | message/session search, LCM expand/load | Turn/session anchors and temporal coverage |
| durable knowledge | memory/fact retrieval | versioned knowledge anchors, trust, contradiction |
| code relationship | context/search/callers/callees/impact/affected | symbol/snapshot/evidence anchors |
| Git/worktree | branch/commit/session/workflow context | local semantic generation and joined freshness |
| parallel agents | presence/work claims/nearby work | safe summaries and overlap evidence |
| sibling tasks/tickets | task/dependency/claim/context-packet reads | task/claim/packet anchors, material relation, version/freshness |
| search gap | status/coverage/scope resolver | one exact recovery action |

The search and ranking implementation remains owned by [05-query-crate.md](./05-query-crate.md) and evaluated under [15-search-quality-evaluation-and-retrieval-research.md](./15-search-quality-evaluation-and-retrieval-research.md); the scout does not implement its own lexical/vector search. When plan 05's separately gated model-assisted rerank profile is enabled, a scout query may consume its canonical ranked result and explanation exactly like any other `TraceQueryV1` result. The scout never sends candidate pairs through `ScoutModelRequestV1`, invents another reranker, or treats model prose as rank evidence. A query rerank failure preserves the declared pre-rerank order and typed coverage before scout policy evaluates whether any suggestion remains useful.

## 8. Model gateway and optional App Server Spark path

Application owns a provider-neutral consumer port:

```rust
pub enum ModelPurposeV1 { IncrementalContextScout }

// ModelCapabilityRefV1 and ModelResidencyV1 are generic domain/catalog
// contracts from plan 01. Scout consumes them unchanged; plan 24 executor
// routing and future model-backed subsystems never depend on this scout plan.

pub struct ScoutModelRequestV1 {
    pub invocation_id: ModelInvocationId,
    pub run_id: ScoutRunId,
    pub address: SuggestionAddressV1,
    pub session_mode: ModelSessionModeV1,
    pub session_generation: u64,
    pub input_sequence: u64,
    pub system_prompt_version: SchemaVersion,
    pub context_digest: PrivacyDomainKeyedFingerprintV1,
    pub context_sections: Vec<ModelContextSectionV1>,
    pub allowed_intents: BTreeSet<ScoutToolIntentV1>,
    pub remaining_reads: u8,
    pub max_output_tokens: u32,
    pub deadline: UtcMicros,
}

pub enum ScoutContextSectionKindV1 {
    Goal,
    TurnDelta,
    ToolReceipts,
    Anchors,
    PriorSuggestions,
    ScopeHealth,
}

pub struct ModelContextSectionV1 {
    pub section: ScoutContextSectionKindV1,
    pub anchors: Vec<RetrievalAnchorId>,
    pub sanitized_excerpt: Option<PromptEligibleText>,
    pub tokens: u32,
}

// Closed response schema (Section 8.1): silent, request_reads, or candidate. Nothing else parses.
pub enum ScoutModelResponseV1 {
    Silent { rationale_code: ScoutRationaleCode },
    RequestReads { proposals: Vec<ScoutToolProposalV1> },
    Candidate { candidate: SuggestionCandidateV1, planning_tokens_used: u32 },
}

pub trait ModelGatewayPort: Send + Sync {
    fn capabilities<'a>(
        &'a self,
        purpose: ModelPurposeV1,
    ) -> BoxFuture<'a, Result<ModelCapabilitySnapshotV1, ModelGatewayError>>;

    fn plan<'a>(
        &'a self,
        request: ScoutModelRequestV1,
        deadline: UtcMicros,
    ) -> BoxFuture<'a, Result<ScoutModelResponseV1, ModelGatewayError>>;

    fn cancel<'a>(
        &'a self,
        invocation: ModelInvocationId,
    ) -> BoxFuture<'a, Result<ModelCancelReceiptV1, ModelGatewayError>>;
}
```

Root composition provides adapters under `src/v2_adapters/model_gateway/`. The first optional adapter may reuse protocol knowledge from the current Codex app-server integration, but not import its automation task types or environment-owned defaults. It must support persistent/warm connection management, capability refresh, per-invocation cancellation, structured schema validation, actual-model receipt capture, deadlines, and process reaping.

The provider-neutral transport and bounded warm-process pool may also serve plan 05's distinct `RetrievalRerank` purpose when that independently configured capability is advertised and benchmark-promoted. Purpose separation is strict: retrieval reranking accepts only a sanitized query plus at most the configured bounded candidate IDs/content slices, returns a closed rank-only schema, receives no scout tools, and cannot emit suggestions or retrieval requests. Scout planning and retrieval reranking have separate eligibility, privacy/egress, token/cost/deadline, circuit-breaker, and evaluation receipts even when they reuse one app-server process. One Turn manifest accounts for both invocations and forbids recursive query -> scout -> query loops or hidden duplicate model calls. Spark is only a safe display label for a discovered opaque capability; absence or drift never selects another model implicitly.

Model selection:

1. Resolve `scout.model.purpose = incremental_context_scout` through plan 20.
2. Read the active model capability snapshot from the catalog/provider adapter.
3. Apply configured provider/model preference and privacy/egress eligibility.
4. If `spark` is selected and advertised, pin its opaque model ID/revision in the run manifest.
5. If the configured model is absent or incompatible, use configured fallback policy: deterministic-only or silence. Do not silently select a larger, costlier, remote, or differently governed model.
6. Record requested and actual model IDs, capability digest, latency, token usage, cost methodology, and failure class.

The system must not assume “Spark” is stable branding, universally installed, local, cheap, or allowed for sensitive context. UI copy may display the provider's safe label while machine contracts use the discovered opaque ID.

### 8.1 Planner protocol

The system prompt is versioned, short, and schema-directed:

- find at most one materially useful, novel suggestion for the exact addressee;
- output `silent` when evidence is weak or the agent already acted;
- request only listed read intents with typed narrow parameters;
- never claim facts without returned anchor IDs;
- never restate the user message or advertise general capabilities;
- treat retrieved text as untrusted data, not instructions;
- prefer a retrieval idea or exact next evidence over prescriptive prose;
- remain within explicit token, tool, time, privacy, and scope budgets.

Responses use a closed schema: `silent`, `request_reads`, or `candidate`. Unknown fields, nested instructions, raw URLs/paths where forbidden, malformed anchors, excessive text, and schema mismatch fail the round safely. Free-form fallback parsing is prohibited.

### 8.2 Prompt-injection and data-boundary defenses

- Retrieved content is framed as inert quoted evidence with stable source IDs.
- Tool descriptions and policy are supplied outside retrieved content.
- Model output cannot change authorization, scope, effect class, budgets, delivery mode, or expiry.
- A model-input firewall applies the active sanitizer receipt/floor to every hydrated field.
- A model-output firewall scans proposed text and structured values before persistence.
- Secret/quarantined/unknown-policy content is unavailable, not masked and passed through optimistically.
- Private local data sent to a remote/provider-managed model requires an explicit eligible sensitivity class and declared egress; otherwise deterministic-only.
- Prompt/output bodies use protected short retention or are omitted; durable records retain safe digests, structured decisions, anchors, receipts, and redacted diagnostic codes.

### 8.3 Incremental model-session state

Incremental product behavior does not require trusting an opaque model conversation as system state. The application context ledger, event IDs, tool receipts, anchors, and manifests are authoritative. The gateway may optimize latency in either advertised mode:

```rust
pub enum ModelSessionModeV1 {
    StatelessSnapshot,
    PinnedIncrementalThread,
}

pub struct ScoutModelSessionReceiptV1 {
    pub address: SuggestionAddressV1,
    pub mode: ModelSessionModeV1,
    pub provider_thread_locator: Option<PrivacyDomainBoundLocatorDigest>,
    pub generation: u64,
    pub last_input_sequence: u64,
    pub last_input_digest: PrivacyDomainKeyedFingerprintV1,
    pub model_capability: ModelCapabilityRefV1,
    pub created_at: UtcMicros,
    pub last_used_at: UtcMicros,
    pub expires_at: UtcMicros,
}
```

- `StatelessSnapshot` sends the bounded current ledger each run and is the deterministic/privacy-conservative default.
- `PinnedIncrementalThread` reuses one provider thread only for the exact profile/thread/session/agent address and supplies monotonically sequenced deltas plus the prior digest.
- A provider thread is never shared between agents, worktrees, scopes, profiles, experiments, or live and replay modes.
- The application does not assume the provider retained state. Sequence/digest mismatch, reconnect, eviction, missing capability, or uncertain cancellation resets to a fresh snapshot generation.
- Scope/access/privacy/config/policy/model-revision changes close the old generation. Compaction may start a new generation from an authorized TraceDecay summary plus anchors; it does not ask the model to remember hidden history.
- Warm app-server process pooling is separate from conversation reuse. Pools are bounded by process count, idle TTL, memory, profile, provider credential, and privacy residency.
- Provider thread IDs are protected opaque locators. Dashboard/CLI show generation and health, never raw provider locators.
- Exact replay uses recorded inputs/results or stateless reconstruction. It never depends on a live provider conversation still existing.
- Cancellation receipts determine whether a thread can be reused. Unknown cancellation closes it rather than risking cross-run output.

## 9. Pure ranking, relevance, and silence policy

`tracedecay-policy/src/scout.rs` (`EvaluatorKind::Scout`, reserved in plan 6's module tree and evaluator registry) consumes only the pinned input manifest, candidates, receipts, prior hint state, configuration values, and an explicit clock. It returns a `ScoutDecisionV1` with selected envelope proposal or silence plus reason codes, following plan 6's evaluator patterns (registered input/output schemas, fixed-point scores, decision/explanation digests):

```rust
pub struct ScoutDecisionV1 {
    pub run_id: ScoutRunId,
    pub evaluation_id: PolicyEvaluationId,
    pub input_manifest_id: ManifestId,
    pub scored: Vec<ScoredCandidate>, // plan 6 shape; one entry per SuggestionCandidateV1
    pub selected: Option<SuggestionCandidateId>,
    pub envelope_proposal: Option<SuggestionEnvelopeV1>, // Pending state; unpersisted until the CAS commits
    pub suppressions: Vec<(SuggestionCandidateId, SuggestionSuppressionReasonV1)>,
    pub silence: Option<SuggestionSuppressionReasonV1>,
    pub state_proposal: HintStateProposal, // plan 6 §9.1.2; the single CAS on the hint-state version
    pub explanation: DecisionExplanation,
    pub decision_digest: ManifestDigest,
}
```

Fixed-point feature groups:

| Group | Positive evidence | Penalties/abstention |
|---|---|---|
| relevance | exact current goal/intent/entity/tool/error/symbol match | broad topical similarity only |
| evidence | direct/provider/derived facts; complete scope; fresh anchor | heuristic-only, partial, stale, ambiguous |
| expected value | resolves blocker, prevents duplicated work, recovers missing context | advice agent already followed |
| novelty | unseen anchor/relationship/capability for this logical message | repeated category, payload, anchor, or work claim |
| urgency | useful before next tool/edit/compaction boundary | useful only after Turn closes |
| specificity | exact retrieval, symbol, session, agent, branch, or recovery action | generic “consider searching” text |
| cost | low tokens/latency/tool/model cost | budget pressure or uncertain value |
| privacy | fully authorized and sanitizer-current | redacted/locked/unknown/egress mismatch |

Policy ordering:

1. validate exact identity, scope, access, snapshot, time, and host capability;
2. remove stale, expired, unauthorized, unanchored, already-observed, and generic candidates;
3. compute logical-message, semantic, category, anchor, and coordination-pair fingerprints;
4. apply dedupe, cooldown, escalation, turn/session/global budgets, and quiet-mode thresholds;
5. compare top candidate against the explicit silence utility;
6. render at most one envelope with no more than three anchors;
7. propose one atomic hint-state transition and envelope write.

Default presentation budget is 96 tokens; hard maximum is 160. Default delivery is at most one scout suggestion per Turn, four per session, one initial category plus one later evidence-strengthened escalation, and one coordination advisory per unchanged claim pair. Configuration may be stricter. Safety and compactness floors prevent looser values beyond hard caps. The turn/session/token budgets here are the shared plan 6 `HintStateSnapshot` ledgers debited by `DeliveryArbiterV1` for both engines; the four-per-session scout quota is the dedicated scout session ledger inside that same snapshot, not a parallel counter.

Semantic fingerprint inputs are category, intent, exact address/logical message, resolved scope digest, primary entity/anchor set, proposed capability, and normalized bounded payload meaning. Every output is a plan-01 `PrivacyDomainKeyedFingerprintV1` bound to the address privacy domain and active key epoch; it is never a generic/public content digest, and key rotation rebuilds or safely forgets state rather than comparing epochs. Model wording changes alone cannot evade dedupe.

## 10. Many agents, worktrees, repositories, and projects

Scouting uses the exact scope and temporal attribution contracts in [16-cross-project-repository-worktree-scope.md](./16-cross-project-repository-worktree-scope.md).

- Repository, project, checkout, worktree, ref, commit, `CodeSnapshotId`, graph generation, and index watermark remain distinct.
- Same repository or path basename does not imply same worktree, ref, snapshot, or task.
- Parent/subagent, sibling-agent, handoff, goal, and work-claim relations require captured/provider evidence.
- Nearby work uses authorized `AgentPresenceV1`, `WorkClaimV1`, TTL, evidence-bearing overlap, and a `CatalogSafeText` summary. It never passes another agent's raw prompt or reasoning.
- Cross-worktree code/Git reads name both source and target immutable snapshots; live PR/delivery state remains separately fresh and joined.
- Cross-project expansion is allowed only when the run's declared scope permits the immutable project-set version. A model cannot turn `Current` into `AllAuthorized`.
- Scheduler fairness is keyed by profile/project/worktree/agent while delivery dedupe is keyed by exact addressee. One agent's receipt never marks another agent as having seen a suggestion.
- Planned redundant ensemble work is suppressed as a duplicate-work warning; diversity/review roles are not treated as accidental overlap.
- When overlapping agents are found, the envelope gives one safe summary plus retrieval anchors to agent/work-claim/session context. It does not auto-message, cancel, lock, or reassign either agent.
- When coverage cannot prove proximity, remain silent and expose the coverage gap in Observatory, not in the model prompt.

The Rspack/Rsbuild/React Router scenario pack from plan 16 must include sibling worktrees, same basename, copied subagent prompts, planned duplicate benchmarking, accidental duplicate research, and live/local PR drift.

### 10.1 Canonical task/ticket graph integration

[`24-canonical-task-plan-graph-and-multi-agent-executor.md`](./24-canonical-task-plan-graph-and-multi-agent-executor.md) owns `InitiativeId`, `WorkItemId`, `WorkItemVersionId`, task/ticket presentation aliases, dependency predicates, task claims, context packets, and their repository/project/worktree/agent/Thread relations. The scout consumes its immutable events and application reads; it does not create a private task table or infer canonical tickets from prompt text.

One initiative may span Rspack, Rsbuild, plugins, and React Router repositories while Codex, Claude, Cursor, or other agents own sibling tasks in separate worktrees. A task event is deliverable context only after a bounded relevance join establishes one of:

- the current `WorkItemId` directly depends on, blocks, supersedes, duplicates, or shares a declared deliverable with the changed sibling work item;
- both task claims cover the same repository/ref/file/symbol/test/PR/retrieval anchor with material overlap above policy threshold;
- a sibling task changed an assumption explicitly referenced by the current task/context packet;
- a blocker was added, cleared, or reassigned on the current dependency path;
- a handoff names the current agent/task/Thread or transfers an evidence packet required by it;
- a new context packet contains authorized anchors selected for this task or resolves a recorded evidence gap;
- a sibling outcome invalidates a pinned code/Git/API/config/version fact used by the current Turn.

Project membership, common initiative membership, chronological proximity, shared labels, same repository, or “nearby” work alone are insufficient. The scout never streams a global board, sprint activity, ticket comments, or all sibling progress into prompts.

```rust
pub struct MaterialTaskChangeV1 {
    pub current_task: WorkItemVersionRefV1,
    pub changed_task: WorkItemVersionRefV1,
    pub relation: TaskRelevanceKindV1,
    pub dependency_path: Vec<EvidenceRef>,
    pub current_claim: Option<WorkClaimRefV1>,
    pub sibling_claim: Option<WorkClaimRefV1>,
    pub context_packets: Vec<ContextPacketManifestRefV1>,
    pub retrieval_anchor_ids: Vec<RetrievalAnchorId>,
    pub materiality: ScoreMicros,
    pub coverage: CoverageReportV1,
    pub watermark: VectorWatermark,
    pub expires_at: UtcMicros,
}

pub enum TaskRelevanceKindV1 {
    DirectDependencyChanged,
    BlockerChanged,
    MaterialClaimOverlap,
    HandoffToCurrentWork,
    ContextPacketForCurrentWork,
    AssumptionInvalidated,
    DeliverableSuperseded,
}
```

Delivery still targets the exact `SuggestionAddressV1`, not a task, initiative, project, or agent type. The envelope states the changed task, its material relation to current work, one safe consequence or retrieval idea, and task/claim/context-packet anchors. It does not dump ticket bodies or another agent's reasoning.

Task notification fingerprints include current/changed `WorkItemId`, relation kind, dependency/claim version, primary anchors, current logical message, and target address. Unchanged versions suppress; a stronger blocker or new invalidating evidence may produce one evidence-strengthened escalation. Pair/category cooldown and per-Turn/session budgets apply across providers so duplicate Codex/Claude capture cannot produce repeated notices.

Authorization is evaluated independently for the current task, sibling task, claim, packet, repository, and target agent. If only the existence of a sibling change is visible, the scout does not leak its title, summary, owner, repository, or anchors. It either emits an authorized generic relation code with one legal retrieval action when materially useful, or remains silent. Global task-board visibility never implies prompt-delivery authority.

## 11. Delivery handshake and timing

The daemon and each host adapter negotiate a versioned capability session:

```rust
pub struct SuggestionHostHelloV1 {
    pub host_profile: HostProfileRef,
    pub host_surface: HostSurfaceKindV1,
    pub host_version: ComponentVersion,
    pub source_instance_id: SourceInstanceId,
    pub host_capabilities: HostCapabilitySnapshotV1,
    pub offered_modes: BTreeSet<SuggestionDeliveryModeV1>,
    pub offered_hook_points: BTreeSet<HookPoint>,
    pub tool_catalog: CatalogSnapshotRefV1,
    pub max_payload_tokens: u32,
    pub max_payload_bytes: u32,
    pub protocol_version: SchemaVersion,
}

pub enum SuggestionDeliveryModeV1 {
    NextPromptContext,
    PreToolContext,
    ProviderSteer,
    NotificationOnly,
}

pub struct PendingSuggestionRequestV1 {
    pub invocation_id: HookInvocationId,
    pub address: SuggestionAddressV1,
    pub hook_point: HookPoint,
    pub last_seen_sequence: u64,
    pub remaining_budget: Duration,
    pub access_policy_digest: AccessPolicyDigest,
}
```

The hello is accepted only when its `HostCapabilitySnapshotV1.subject` is `Installed` and that `HostIntegrationRuntimeRefV1` matches the active plan-01 runtime handshake: host profile/instance/surface, integration manifest, component set, bundle/component, install receipt/generation, and adapter version all bind; the independently carried snapshot digest pins the current probe without a reverse reference. `offered_modes` and `offered_hook_points` are a mechanically derived intersection of the plan-27 host capability ledger, the installed components, current probe, trust/policy state, catalog generation, and host version; they are not self-asserted support. `Supported` and fresh may negotiate, `VersionGated` returns the exact remediation, and absent/undocumented/policy-disabled/stale/trust-pending entries cannot deliver. A reconnect after install, trust, probe, catalog, or profile change creates a new session; an in-flight envelope remains pinned to the old digest and is safely expired or reconciled rather than widened.

`HookApplicationPort` gains a narrow `claim_pending_suggestion` operation. The claim is a `DeliveryArbiterV1` operation ([06-policy-crate.md](./06-policy-crate.md) §9.1.3), not a parallel scout-side CAS: one transactional compare-and-swap on the `HintStateSnapshot` version token that revalidates address, logical message, scope/access digest, expiry, current hint state, payload budget, host mode, and envelope sequence, and claims the pending-suggestion slot. Deterministic candidates arbitrated in the same invocation ride the same version token, so at most one engine delivers. It performs no query or model work.

Timing rules:

- ready before Codex `UserPromptSubmit` (or the host's exact equivalent): inject through the event-supported developer/additional-context field, never by wrapping a normal prompt in a fake `UserPromptSubmit hook (completed)` user message;
- ready after prompt submission but before a declared `PreToolUse` boundary: deliver only if the category remains relevant to that exact Turn and the host supports that context field;
- ready mid-generation: use `ProviderSteer` only when explicitly advertised, semantically safe, and the envelope is urgent enough under policy; otherwise do not interrupt;
- Claude `async` completion and `asyncRewake` are not provider-steer or suggestion channels: async context arrives on a later Turn, rewake surfaces exit-2 failure text, and neither preserves the addressed envelope's timing/decision semantics. Generated TraceDecay hooks remain synchronous; model/search work stays daemon-side and the next legal native context boundary claims the envelope.
- ready after the useful boundary: mark `Late`, optionally reconsider as a new candidate against the next Turn only when the underlying evidence is independently relevant and a new envelope/address is created;
- `NotificationOnly` is dashboard/host-visible status, not presumed model context;
- no supported mode: suppress with `HostCannotDeliverSafely`;
- timeout or store contention: return no suggestion and preserve the existing hook response.

Delivery adds no wait for an active run. Target budget: pending lookup/revalidation p95 <=2 ms, p99 <=5 ms, and zero model/network/tool latency on the hook critical path. Existing plan 7 total deadlines remain authoritative.

Claim and delivery are separate:

1. application atomically claims envelope with invocation/lease and returns exact bytes/digest;
2. host adapter renders and attempts delivery;
3. adapter records delivered, failed, rejected, or unknown receipt;
4. claim timeout returns envelope to eligible only if no host acknowledgement could have occurred; uncertain delivery is never retried automatically;
5. retries by the same invocation return the stored receipt while its policy/catalog/environment digest still matches and do not duplicate injection; a digest mismatch returns a typed stale-environment error rather than re-evaluating or redelivering (the plan 7 §8 retry rule).

When additive Codex sources launch several matching TraceDecay handlers concurrently, every handler run is captured, but `claim_pending_suggestion` uses plan 07's invocation-group identity and the same hint-state CAS. Exactly one winner may render a model-visible suggestion; losers return an empty successful response and cannot suppress sibling start. Arrival order never changes the chosen envelope. `Stop`/`SubagentStop` continuation is a separate policy effect, never an incremental-scout delivery channel, and `stop_hook_active` prevents a suggestion from creating a continuation loop.

Plan 07 §7.4's task lifecycle checkpoint is a separate delivery-arbiter lane with its own persisted CAS, eligibility denominator, one-shot cap, and receipt. The scout may contribute evidence that task state materially changed, but it cannot reserve, spend, render, or trigger the continuation. The checkpoint may ask only for an explicit plan-24 lifecycle command and cannot carry retrieval ideas. Both lanes share the Turn attention budget and native host capability/trust snapshot so they cannot each inject competing text at the same boundary; task reconciliation wins only when lifecycle debt is material, otherwise the ordinary hint arbiter proceeds. The dashboard and replay harness display the two decisions side by side rather than combining their outcomes.

Plan 07 §7.5's live steering inbox is a third, authoritative command lane—not a `DeliveryCandidateV1`, suggestion envelope, hint-state slot, or scout outcome. Its directive is already selected by an authorized controller and ordered by the target's fenced monotonic sequence. The host delivery coordinator checks steering before the ordinary hint arbiter: pending required steering wins the next safe boundary; fresh advisory steering wins over generated advice at that boundary but never gains a completion fence. A steering batch may share §7.4's single Stop continuation only under its one-shot receipt, while the scout is suppressed with `AuthoritativeSteeringPending` and spends no hint/scout quota. Steering uses Plan 01's separate hard payload/batch/Turn/rate/cooldown limits and canonical delivery/ack/disposition receipts; no steering overflow may borrow scout/hint tokens or grow the prompt, and no scout cooldown can waive a required steering fence. Conversely, an ignored or rejected suggestion cannot acknowledge or resolve steering.

Adapter capability names remain explicit. `SuggestionDeliveryModeV1::ProviderSteer` means an advisory scout envelope may use a provider-native model-context boundary proven by the host ledger; it does **not** mean steering or authorize delivery of a `SteeringDirectiveV1`. Steering separately records `NativeInterrupt | AfterToolBeforeModel | StopContinuation | NextTurnOnly`, never interrupts an in-flight side-effecting tool, and falls back truthfully with `Unsupported`, `DeferredNextBoundary`, or `NextTurnOnly`. The shared Codex/Claude/Cursor/Hermes conformance fixture must prove that each lane records its actual boundary, duplicate/stale acknowledgement cannot advance steering, and no suggestion receipt is accepted as a steering delivery receipt.

For Claude, one generated exec-form handler is eligible for advisory delivery even when foreign handlers also match. Host parallelism and identical-handler dedupe remain observed facts, not scout arbitration. `PostToolBatch` is the preferred bounded fan-out/fan-in reconsideration boundary; per-tool completion may update evidence but cannot emit several sibling suggestions for one batch. Produced-at, eligible-at, host-hook completion, model-visible Turn, transcript-resume replay, and spill/coverage states are distinct timestamps/dispositions.

## 12. Feedback and outcome attribution

The lifecycle is:

```text
trigger -> run -> reads -> candidates -> policy/silence -> envelope
        -> claim -> host delivery -> observed action/retrieval/feedback
        -> terminal outcome with evidence and coverage
```

Outcomes extend the shared plan 6/7 hint contract through plan 6's recorded outcome enum v2 revision ([06-policy-crate.md](./06-policy-crate.md) §10 — the only legal extension path for those closed enums) and persist as plan 6 `HintOutcomeRecordV1` rows:

- `Observed`: directly linked retrieval-anchor open, suggested capability invocation, scope recovery, handoff/coordination action, or accepted host acknowledgement plus corroborating action;
- `Unobserved`: horizon closed with adequate capture coverage but no linked action; not automatically “bad”;
- `Unresolvable`: capture gap, ambiguous causation, missing provider event, delivery unknown, or horizon loss;
- `HumanHelpful`, `HumanNotHelpful`, `HumanIncorrect`, `HumanTooLate`, `HumanRepeated`, and `HumanTooVerbose` are explicit feedback evidence, not silent model training labels;
- `PreventedDuplicateWork` requires evidence that a work claim, handoff, or task changed after delivery; temporal proximity alone is insufficient;
- correction records the corrected intent/scope/entity/route and exact suggestion ID;
- dismissal/mute affects future policy state at the declared scope but never deletes historical evidence.

Every numerator has an eligible denominator, terminal horizon, coverage threshold, and delivery-state exclusion. Failed/unknown delivery cannot count as emitted, ignored, or adopted. Outcome projectors never inspect raw model reasoning.

## 13. Dashboard product surfaces

Plan 11 exclusively owns routes, workspace composition, layout, panels, and interaction. This section specifies only Context Scout read models, legal actions, states, and acceptance data consumed by that owner; its screen descriptions are non-normative inputs and cannot create a second frontend contract.

The dashboard extends [11-dashboard-frontend.md](./11-dashboard-frontend.md) through generated API/client contracts only.

### 13.1 Observatory: Context Scout

Add a `Context Scout` subsystem card and workspace containing:

- enabled/paused/shadow/deterministic/model-assisted state and effective config source;
- daemon worker lease/epoch, accepting/draining state, queue depth/bytes/age, per-shard checkpoint/watermark, lag, cancellation, backpressure tier, and circuit breakers;
- trigger funnel: observed -> eligible -> coalesced -> run -> model -> reads -> candidate -> silent/envelope -> delivered -> terminal outcome;
- suppression distribution by stable reason, with denominators and scope/host/provider strata;
- model capability/provider/actual model, health, residency/egress class, token/latency/cost, fallback-to-deterministic rate, schema rejection, and timeout;
- tool-read waterfall by capability, latency, coverage, information gain, redaction, timeout, and result-anchor count;
- relevance/precision/recall/noise/duplication/late/expired/token metrics with confidence intervals and evaluation version;
- host handshake coverage and safe delivery modes by Codex/Claude/Cursor/Kiro/provider version;
- privacy/sanitizer/config/policy/catalog/index generation coverage and blocked lanes;
- per-project/worktree/agent fairness, nearby-work opportunities, planned redundancy, duplicate-work prevention, and unresolved scope;
- task-graph opportunities by material relation, current/sibling task pair, blocker/handoff/invalidated-assumption outcome, suppressed nonmaterial board events, and cross-repository coverage without task titles in metrics;
- safe doctor findings and one legal remediation action.

Metrics and logs contain IDs/reason codes/counts, not prompt text, paths, tool payloads, secret candidates, model prompts, or unrestricted model outputs.

### 13.2 Causal Loom and Turn inspector

Add a `Scout` lane aligned with exact Turn time and causality:

- trigger markers grouped into one coalesce generation;
- run span with snapshot/policy/catalog/config/model refs;
- model planning and approved tool-read child spans;
- returned retrieval-anchor nodes and provenance edges;
- candidate/suppression decision tree;
- pending/late/expired/superseded/delivered envelope marker;
- host claim/delivery receipt and terminal outcome edge;
- nearby-agent/worktree evidence connected to the Agent and Git graph lenses;
- exact current/sibling task, dependency, claim, handoff, and context-packet nodes connected to the Task, Agent, Git, and Thread graph lenses;
- “why now?”, “why this?”, “why silent?”, and “what was unavailable?” explanations from typed reasons.

The inspector may show authorized sanitized evidence snippets on demand. It must distinguish occurred, ingested, projected, run, eligible, delivery, and outcome times. A model run cannot be displayed as part of the agent's own reasoning trace.

### 13.3 Hint Lab and playground

Hint Lab accepts any authorized message, Turn, session position, event, suggestion, or retrieval anchor and supports:

- exact historical replay with original artifacts — for model-assisted runs, `ExactDeterministic` scopes to policy-over-recorded-candidates only; the model-planning stage replays solely as `RecordedResult` (a live model has no counterpart to plan 6's evaluation seed), and a mixed run reports its weakest stage fidelity, never a blanket exact label;
- recorded-result verification;
- current-best-effort “what would the scout suggest now?” with every substituted config/policy/catalog/index/model/memory/tool snapshot listed;
- A/B deterministic-only versus model-assisted, old versus new bundle, model capability, budget, threshold, debounce, and tool-grant variants;
- immutable branch-at-any-parameter, bounded sweeps/ablations, aligned stage playhead, cost/quality Pareto views, and typed failure minimization through the shared experiment cockpit;
- shadow replay of an entire session with cumulative dedupe/cooldown/budget state;
- step view: trigger -> context delta -> hypotheses -> requested reads -> returned anchors -> feature scores -> suppression/selection -> rendered envelope;
- exact payload/token count and host-specific rendering preview;
- Codex definition/source replay with all ten exact events, source/trust/managed/effective state, matcher aliases or ignored matcher, concurrent handler-run grouping and arrival order, output/exit-code decode, deny/continuation precedence, unsupported-field failure, and explicit `unified_exec`/WebSearch/non-MCP interception gaps;
- counterfactual delivery-time simulation for on-time/late/next-boundary behavior;
- outcome relabeling workflow with adjudicator identity and agreement, never production delivery;
- fixture promotion as a separate explicit test-artifact command.

The evaluator is production-read-only by construction: plan 6 exposes immutable evaluator ports only, while plan 9's one hermetic experiment runner freezes clock/RNG, mounts immutable inputs plus a disposable overlay, denies production write/counter/cache/lease/effect ports, and records opened/denied resources in `ReplaySideEffectReceiptV1`. Results, relabeling evidence, variants, traces, and comparisons persist only through plan 2's generic experiment/run/stage family, never a scout/lab/replay-artifact store. It cannot claim/deliver an envelope, mutate live hint state, invoke mutation tools, or approve/apply/rollback curation. A current-best-effort live-model stage requires an explicit metered egress/model grant and budget; the manifest can reproduce inputs and recorded output but cannot claim byte determinism.

Observed foreign hook definitions remain inert: Hint Lab may replay their recorded sanitized result or compare against TraceDecay's catalog renderer, but never executes a foreign command, handler, script, prompt, agent, path, or environment and never changes host trust.

### 13.4 Controls and feedback

Operators may enable/disable/pause/resume the subsystem, cancel active optional runs, select deterministic/shadow/model-assisted mode, configure budgets/grants/scope, mute a category/scope for a horizon, and submit feedback. These are system controls or policy evidence, not item approval.

There is no “send this suggestion now” button. A historical envelope can be copied as a citation only through normal authorized export/copy rules; it cannot be re-injected as if timely.

## 14. Configuration control plane

Every control is a `ConfigDescriptorV1` from [20-configuration-control-plane.md](./20-configuration-control-plane.md), visible with effective source, constraints, impact, history, status, and CLI/MCP/API/SDK parity.

Minimum registry:

| Prefix | Representative keys |
|---|---|
| `scout.runtime` | `enabled`, `mode=off\|shadow\|deterministic\|model_assisted`, `pause`, worker concurrency, queue bytes/age, lease TTL |
| `scout.trigger` | event-family eligibility, debounce min/max, idle threshold, self-event exclusion, per-Turn run cap |
| `scout.model` | purpose binding, provider/backend preference, model capability reference, deterministic fallback, stateless/pinned-incremental mode, warm session pool/idle TTL, soft/hard timeout, input/output tokens, daily token/cost caps |
| `scout.tools` | generated eligible capability set, per-family limits, timeout, result items/bytes, local-only default, remote-read/egress grant |
| `scout.context` | delta horizon, max goals/events/symbols/anchors, excerpt tokens, allowed sensitivity/residency, retention |
| `scout.policy` | relevance/evidence/novelty/urgency thresholds, silence utility, category cooldown, logical dedupe, escalation, terminal horizon |
| `scout.delivery` | enabled host modes, max payload tokens/bytes, per-Turn/session budgets, expiry, late policy, claim TTL |
| `scout.coordination` | proximity threshold, claim TTL, planned-redundancy classes, summary budget, pair cooldown |
| `scout.tasks` | eligible material relation kinds, dependency-path depth, overlap threshold, packet/claim freshness, pair cooldown, cross-repository and provider boundaries; no global-board mode |
| `scout.evaluation` | shadow sample, experiment assignment, corpus/version, metric gates, feedback retention |
| `scout.privacy` | model residency/egress eligibility, sanitized prompt/output retention, audit detail; all constrained by plan 18 floor |
| `scout.observability` | safe sampling, tracing, low-cardinality metrics, status retention |

Rules:

- global default is `off` until shadow gates pass; rollout may then choose deterministic or model-assisted per profile/project/host;
- privacy floor can forbid model assistance for a sensitivity/residency combination regardless of a lower layer;
- model credentials are opaque `CredentialRefId`s; Settings never reveals or stores secret material;
- a model/backend name is selected from discovered capabilities, not an unconstrained text field;
- hot-reload applies to thresholds/budgets/grants at the next run boundary; in-flight runs pin the old snapshot or cancel;
- host protocol/model adapter changes may require daemon restart or new-agent-session impact, shown before direct save;
- disabling or pausing prevents new runs and cancels optional in-flight work at safe boundaries; it does not delete evidence;
- config import/export carries capability references and unresolved credential aliases, never secrets;
- there is no preview/apply ceremony. Validate and save directly; revision conflicts and safety-floor violations are typed.

Settings route: `/settings/context-scout`. It shows target/layer/effective provenance, model capability health, tool grants, privacy/egress, budgets, delivery compatibility, evaluation status, and linked Observatory evidence.

## 15. CLI, MCP, API, SDK, and rendering contract

All bindings originate in the catalog/application manifest and follow [21-cli-mcp-tool-surface-and-output-unification.md](./21-cli-mcp-tool-surface-and-output-unification.md): plan 09 owns semantic typed views; plan 21 renders them with human CLI default, Markdown MCP default, explicit JSON/NDJSON, stable coverage, retrieval anchors, and no scattered renderers.

### 15.1 Semantic use cases

| Use case | Effect | Purpose |
|---|---|---|
| `scout.status.get` | read | worker/model/tool/host/config/coverage status |
| `scout.runs.list` | read | cursor-bounded runs and phase summary receipts |
| `scout.runs.get` | read | one exact run and its phase receipts |
| `scout.envelopes.list` | read | cursor-bounded authorized envelope lifecycle and anchors |
| `scout.envelopes.get` | read | one exact addressed envelope lifecycle and anchors |
| `scout.decision.explain` | read | feature/suppression/dedupe explanation |
| `scout.evaluation.get` | read | corpus/experiment metrics and regressions |
| `scout.feedback.record` | evidence append | explicit helpful/incorrect/late/repeated feedback |
| `scout.runtime.pause` | system control | stop optional work at a safe boundary |
| `scout.runtime.resume` | system control | restart optional work from durable state |
| `scout.runtime.cancel` | system control | cancel selected active run; no envelope deletion |

These eleven rows are the complete canonical family imported by plans 08, 09, 10, and 21. CLI `suggestions` and HTTP `/scout/suggestions` are presentation bindings of `scout.envelopes.*`; no `scout.suggestions.*` semantic alias exists. Configuration reads/writes use the generic `config.*` use cases and `scout.*` descriptors. Do not add scout-specific configuration files or configuration endpoints.

Historical/current-best-effort scout replay uses the generic `experiments.draft_from_selection`, `experiments.create`, and `experiment_runs.create/get/cancel/resume/retry/minimize` family with `LabKindV1::Hint` plus the scout evaluator mode. No `scout.replay.*` use case, table, scheduler, or cancel path exists.

### 15.2 CLI

```text
tracedecay scout status [scope flags] [--format human|markdown|json]
tracedecay scout runs list|get ...
tracedecay scout suggestions list|get|explain ...
tracedecay experiment fork --turn <id>|--message <id>|--session <id> --lab hints --set evaluator_mode=scout --mode exact|recorded|current-best-effort
tracedecay scout evaluation show [--corpus <id>] [--experiment <id>]
tracedecay scout feedback <envelope-id> --rating helpful|not-helpful|incorrect|too-late|repeated|too-verbose [--reason-code <code>]
tracedecay scout pause|resume [--target <scope>]
tracedecay scout cancel --run <id>
tracedecay config get|set|history|status scout.<key> ...
```

Streams use NDJSON only when explicit. Human output includes exact scope, freshness, status, coverage, applied budgets, next cursor/anchor, and one safe remediation. It never prints model prompt/output bodies by default.

### 15.3 MCP

Generated MCP bindings expose concise agent-oriented reads such as status, pending/history lookup, explanation, generic experiment fork/run/status, and feedback. Default Markdown begins with result/silence and the exact addressee/scope, followed by bounded anchors and coverage. `format=json` returns the same canonical typed view. No tool returns the entire scout queue or model transcript in prompt-sized output; pagination and retrieval anchors apply.

MCP replay requires an exact message/Turn/session anchor and defaults to `RecordedResult` or current best effort as declared. It cannot deliver or alter live counters. Agents receive typed late/stale/partial flags rather than prose warnings.

### 15.4 HTTP/SSE and SDKs

```text
GET  /api/v2/scout/status
GET  /api/v2/scout/runs
GET  /api/v2/scout/runs/{id}
GET  /api/v2/scout/suggestions
GET  /api/v2/scout/suggestions/{id}
GET  /api/v2/scout/suggestions/{id}/explanation
GET  /api/v2/scout/evaluation
POST /api/v2/experiments:draft-from-selection       # LabKindV1::Hint + scout evaluator mode
POST /api/v2/experiments:create
POST /api/v2/experiments/{id}/runs:create
GET  /api/v2/experiment-runs/{id}
POST /api/v2/commands/scout/{pause,resume,cancel}
POST /api/v2/commands/scout/feedback
POST /api/v2/subscriptions
GET  /api/v2/subscriptions/{id}/events
```

SSE event families include safe `scout_status_changed`, `scout_run_progress`, `scout_envelope_state_changed`, `scout_delivery_recorded`, `scout_outcome_recorded`, and `scout_resync_required`. Slow consumers reload a frozen snapshot. TypeScript, Rust, and Python clients share generated schemas/pagers/streams and never parse Markdown.

## 16. Store and projector design

[02-store-crate.md](./02-store-crate.md) owns activity-shard repositories and migrations:

- one scout extension row keyed to plan-02 `outbox_consumer_checkpoints`; no `scout_consumer_checkpoints` or scout-local lease table exists;
- `scout_turn_states` keyed by exact address/logical message;
- `scout_runs` as a scout-specific extension keyed one-to-one to `OperationKernelV1<ScoutRunKind>` plus immutable input manifests; generic phase-step/progress/cancel/takeover/terminal receipts use plan 02's shared `operations`/`operation_steps` family;
- `scout_tool_receipts` referencing catalog capabilities and retrieval anchors;
- `suggestion_candidates` with bounded protected payload refs;
- `suggestion_envelopes` with sequence/state/expiry and privacy-domain/key-epoch-bound fingerprint;
- `suggestion_claims` and `suggestion_delivery_receipts` with uniqueness on invocation/envelope;
- `suggestion_feedback` and projected outcome refs;
- canonical foreign keys/refs to task, claim, dependency evidence, and context-packet anchors without copying task bodies;
- retention/tombstone/hold ownership and referential integrity to anchors/evidence.

Owner-shard rule: records live with the activity/session owner. The catalog may hold safe routing/progress summaries, never canonical payloads or delivery state. Cross-shard reads route by address and return vector coverage.

Transactions:

- run creation plus accepted trigger disposition is atomic;
- final policy evaluation, hint-state compare-and-swap, envelope insert, and Turn-state version advance are atomic;
- claim uses envelope state/version/address/access/expiry compare-and-swap;
- delivery receipt plus terminal state is idempotent by host invocation and attempt;
- cleanup never deletes anchors/evidence still retained by envelope/outcome/audit holds.

[04-projectors-crate.md](./04-projectors-crate.md) adds deterministic handlers for trigger eligibility views, logical-message origin relations, runs/tools/envelopes/delivery/outcomes, and safe rollups. Projectors never schedule model work or rank candidates. Dead-letter/gap/rebuild semantics remain the shared projector design.

[03-capture-crate.md](./03-capture-crate.md) captures provider-native Turn, message, tool, agent, Git/worktree, and lifecycle evidence with origin and continuity. It does not parse model suggestions or synthesize missing causation. Scout model/tool/delivery records return through declared sanitized application/capture events, not direct projector writes.

## 17. Privacy, authorization, retention, and audit

[18-secret-detection-redaction-and-private-data-safety.md](./18-secret-detection-redaction-and-private-data-safety.md) governs every source and sink.

- Authorization occurs before resolving scope, hydrating anchors, reading model capability details, constructing prompts, executing tools, listing runs, or rendering envelopes.
- Every hydrated field must have a sanitizer receipt compatible with the active floor. Legacy-unscanned, quarantined, locked, secret-like, or unknown content is unavailable.
- Model inputs and outputs are independent sinks with detector/version receipts.
- A retrieval anchor is bound to privacy domain, principal/access digest, scope, snapshot, schema/catalog/config, expiry, and retention; it is reauthorized on open.
- Prompt/tool/model raw bodies are protected short-retention artifacts only where explicitly enabled; otherwise retain safe digests and typed receipts.
- Suggestion text and model explanations are not indexed into general search by default, preventing self-echo retrieval loops.
- Cross-agent summaries use `CatalogSafeText`; reasoning trace/raw prompts are never shared merely because agents occupy nearby worktrees.
- Remote model/live Git egress is explicit, separately metered, and visible in Settings/Observatory.
- Audit records actor/system, purpose, target IDs, policy/config/catalog/model refs, reason codes, counts, state transitions, and digests without content literals.
- Deletion propagates tombstones to derived candidates/envelopes and invalidates affected retrieval anchors; delivered evidence history retains only lawful safe receipts.

Threat tests cover prompt injection in retrieved sessions/code/Git, secret canaries in every field/result/model output, cross-project/worktree authorization, malicious model capability metadata, oversized output, anchor swapping, replay privilege changes, and renderer escape attacks.

## 18. Failure recovery, status, and doctor

Typed health components:

| Failure | Live behavior | Recovery/status |
|---|---|---|
| model/backend unavailable | deterministic-only or silence | capability/adapter status; bounded retry/circuit breaker |
| configured Spark/model absent | no implicit model substitution | `model_capability_unavailable`; choose discovered capability or fallback policy |
| app-server protocol/schema drift | reject response, cancel/reap session | protocol/version finding and host update action |
| invalid/malformed model output | silence; safe schema-rejection count | fixture/detail anchor for authorized lab, no raw error in hint |
| query/tool timeout | stop step or run within budget | tool receipt and coverage; no generic failure hint |
| scope identity conflict | suppress delivery | exact candidates and migration/reconciliation action |
| stale index/Git join | only anchored freshness-gap candidate if valuable | reindex/reconcile action; never false current claim |
| privacy/authorization denial | silence | safe denied/blocked coverage count |
| queue lag/overload | coalesce, cancel obsolete, deterministic-only, shed | tier, age, checkpoints, capacity action |
| store busy/locked | no hook wait; run retry or silence | owner/lease/status without unbounded retry |
| worker crash/daemon upgrade | resume checkpoint/phase receipts | lease epoch, draining, takeover, recovered/cancelled counts |
| host lacks safe injection | persist/suppress or notification-only | handshake matrix and host integration action |
| delivery unknown | never retry automatically | reconcile host acknowledgement or terminal unresolvable |
| config/policy/catalog change | cancel or finish pinned generation | generation status and new-run boundary |

`tracedecay doctor` and `SystemStatusSnapshot` use the same model as Observatory/CLI/MCP. Checks include:

- checkpoint continuity, lease ownership, queue age, disk reserve, stuck phases, orphaned claims, expired pending envelopes;
- model adapter executable/protocol/capability/schema/cancellation/token-accounting health;
- selected model and credential-reference availability without secret metadata;
- tool eligibility catalog completeness and absence of mutation capabilities;
- sanitizer receipt/floor/egress compatibility;
- host handshake/delivery-mode/version coverage;
- outcome denominator and feedback projection health;
- config registry/drift/impact and experiment assignment health.

Doctor does not run a model or inject a suggestion by default. An explicit synthetic canary check uses no repository/session content and records cost.

## 19. Evaluation, replay, and promotion gates

Use the chronological research anchors and session IDs in [13-research-provenance-and-context-anchors.md](./13-research-provenance-and-context-anchors.md), the historical failures in [14-historical-failure-regression-matrix.md](./14-historical-failure-regression-matrix.md), and the time-safe corpus methodology in [15-search-quality-evaluation-and-retrieval-research.md](./15-search-quality-evaluation-and-retrieval-research.md).

### 19.1 Corpus construction

Stratify real sanitized local sessions by:

- host/provider; direct user/subagent/copied prompt/tool protocol origin;
- simple no-hint chat, code exploration, debugging, implementation, review, Git/PR, cross-project, worktree, memory/LCM, compaction, automation, and failed TraceDecay lookup;
- single agent, parent-child tree, sibling agents, parallel worktrees, planned redundant review, accidental duplicate research;
- independent and dependency-linked sibling tasks across Rspack/Rsbuild/React Router, blocker/handoff/context-packet/invalidated-assumption cases, and high-volume nonmaterial task-board negatives;
- short/long Turn, fresh/stale/missing index, complete/partial scope, local/live/joined Git;
- suggestion opportunity category and correct silence negatives;
- model-assisted eligibility/privacy/egress class;
- emitted, not emitted, corrected, repeated, too late, adopted, and unresolvable historical outcomes.

Apply causal time cutoffs: replay sees only observations, indexes, memory/facts, Git/delivery snapshots, work claims, configuration, and catalog versions available at the trigger time. Current-best-effort is labeled separately and cannot be scored as historical prediction.

Create pooled candidate judgments from deterministic baseline, current static hints, multiple scout policies/models, human-proposed anchors, and hard negatives. Two adjudicators label material usefulness, correct addressee/scope, evidence sufficiency, novelty, timing, compactness, privacy, best action/anchor, and whether silence is correct. Preserve disagreement and Cohen's kappa.

### 19.2 Experiments

Required modes:

1. current V1/static hint behavior baseline;
2. V2 deterministic candidate/policy only;
3. model-assisted shadow with no tools;
4. model-assisted shadow with bounded local reads;
5. optional remote-read grant stratum;
6. host rendering/delivery timing simulation;
7. live local A/B only after offline and shadow gates, with stable assignment by profile/session and instant disable.

Replay entire sessions, not isolated prompts, so dedupe, cooldown, budgets, logical-message copies, agent spawning, and late results are measured correctly. Synthetic unit cases cover every reason code and fault; real transcripts protect product precision.

### 19.3 Metrics and launch goals

| Metric | Promotion goal |
|---|---|
| delivered material precision | >=0.80 overall, 95% CI lower bound >=0.75; no major host/origin category below 0.65 |
| high-value opportunity recall | >=0.65 at fixed precision; report by category rather than optimize broad hint frequency |
| correct-silence rate on no-hint negatives | >=0.95 |
| harmful/misdirected/incorrect-scope suggestions | <=0.01; privacy/secret/cross-authority violation = 0 |
| generic availability boilerplate | 0 accepted cases |
| repeated logical-message/category envelope | <=0.005 of deliveries; more than one envelope per Turn = 0 outside explicit evidence-strengthened escalation contract |
| on-time usefulness | >=0.90 of selected envelopes reach a supported boundary before expiry; late envelopes are not delivered |
| payload size | median <=64 tokens, p95 <=96, hard <=160 |
| hook incremental overhead | pending lookup/revalidation p95 <=2 ms, p99 <=5 ms; no model/tool/network wait |
| warm deterministic trigger-to-envelope | p50 <=250 ms, p95 <=1 s |
| warm model-assisted trigger-to-envelope | p50 <=1.5 s, p95 <=5 s, p99 <=12 s (trigger-to-envelope includes up to 750 ms coalesce wait, queue delay, and at most one superseded re-run; a single run's wall clock stays within the Section 6 2 s soft/8 s hard limits); host/provider strata reported |
| bounded exploration | <=4 reads and <=8,192 input/256 output tokens per run; zero mutation effects |
| cost | within configured per-profile/day cap; cost per materially useful delivery and silent run reported |
| crash/idempotency | zero duplicate delivery across retry/restart/lease takeover fault matrix |

Precision gates dominate recall. A variant that increases recall by emitting more irrelevant hints does not promote. Low-support strata remain shadowed. Confidence intervals, missing labels, unresolved outcomes, coverage, and cost methodology accompany every result.

### 19.4 Online safety

- Start `off`, then deterministic shadow, then model shadow, then opt-in delivery by host/scope.
- Shadow work is budgeted and can be disabled independently of canonical hints.
- Experiment assignment pins policy/config/catalog/model generations for a session.
- Dashboard and CLI expose one kill switch that hot-disables new runs and delivery; hooks continue normal capture.
- Automatic circuit breakers pause model assistance on privacy failure, invalid-output spike, latency/cost breach, duplicate delivery, cross-scope mismatch, or precision guardrail regression.
- Promotion is an autonomous rollout-policy decision over predeclared aggregate gates, not per-item curation approval. Operators configure/pause/inspect; individual suggestions are never approve/apply items.

## 20. Migration and convergence

1. Freeze current source inventories: hook points, deterministic hint classifiers, dedupe state, analytics, app-server adapters, automation model configuration, daemon queues, MCP/CLI/API/dashboard surfaces, and relevant incoming-master changes.
2. Add domain/store/projector contracts and replay-only ingestion of historical hint/delivery/outcome evidence.
3. Move deterministic `tool_hints` classifier outputs behind pure plan 6 policy as the `DeliveryCandidateV1::Deterministic(ScoredCandidate)` arm of `DeliveryArbiterV1` without changing delivery; scout candidates enter the same union as `DeliveryCandidateV1::Scout(SuggestionCandidateV1)`.
4. Implement the scout worker in shadow against canonical outbox events. It writes runs/candidates/suppression but no deliverable envelope.
5. Add provider-neutral model gateway and Codex app-server adapter behind capability/config selection. Do not reuse automation config or `TRACEDECAY_CODEX_SUMMARY_*` as scout defaults.
6. Run frozen/offline and rolling shadow evaluation; fix logical-message origin, scope, privacy, latency, and dedupe regressions.
7. Add durable envelopes and host handshake. Differential-test render bytes/state receipts while keeping V1 as sole delivery owner.
8. Enable V2 deterministic delivery for one host/scope; enforce plan 6's `DeliveryArbiterV1` as the single delivery selector and compare outcome denominators.
9. Enable optional model-assisted delivery only for eligible strata that pass gates.
10. Migrate CLI/MCP/API/UI to generated use cases and typed views; preserve bounded aliases only where plan 21 requires them.
11. Backfill historical scout-evaluation evidence for labs only. Never create or deliver historical pending suggestions.
12. Retire duplicate V1 classifier/dedupe/render/analytics paths after one read-only release and deletion receipts. Keep deterministic rules as versioned policy inputs, not a second engine.

Cutover rollback disables new V2 scout delivery and reselects the prior single deterministic delivery owner during the bounded compatibility window. It does not delete canonical events or rewrite outcomes. After final V1 deletion, recovery is forward-fix/config-disable; there is no permanent dual-write/dual-delivery architecture.

Compatibility work belongs to [12-root-compatibility-migration.md](./12-root-compatibility-migration.md); public API/SDK stability follows [17-official-public-api-and-sdks.md](./17-official-public-api-and-sdks.md).

## 21. Reviewable PR slices

Numbers extend the existing program without colliding with plans 1–24. Canonical dependencies determine order; no scout PR defines a temporary task ref or temporal retrieval implementation.

### PR 4F — Scout and suggestion domain contracts

- **Ordering:** after plan 01/24 PR 4E publishes canonical task refs and plan 01 publishes generic model-capability refs.
- Add IDs, address/logical-message, trigger, run, tool, candidate, envelope, delivery, outcome, checkpoint, status, and reason schemas.
- Add registry fixtures, deterministic ID tests, bounded-text/privacy compile gates, and dependency rules.

### PR 6F — Scout repositories, transactions, checkpoints, and retention

- Add activity-shard repositories/migrations and transaction/idempotency/fault tests.
- Add lease/checkpoint/claim/delivery recovery and retention/tombstone integrity.

### PR 10D — Scout projections and safe rollups

- Project trigger eligibility, logical origins, lifecycle, delivery/outcome, status, and low-cardinality metrics.
- Add rebuild/dead-letter/determinism and self-event exclusion fixtures.

### PR 10F — Canonical task-materiality projection integration

- **Ordering:** after plan 24 PR 10E and PR 4F; consumes canonical task/dependency/claim/packet refs and materiality candidates without copying task state.
- Join task materiality to exact active Agent/Thread/Turn addresses, emit bounded scout triggers, and prove nonmaterial board traffic stays silent.
- Add cross-repository dependency/handoff/overlap and missing/partial task-projection fixtures.

### PR 22D — Scout capability eligibility and generated bindings

- Extend catalog effect/egress/model/tool eligibility and inventory every allowed/forbidden capability.
- Generate application/transport/docs/config manifests and drift tests.

### PR 23H — Pure scout ranking, silence, dedupe, and replay policy

- Implement fixed-point features, suppression, logical/category/anchor dedupe, cooldown, budgets, expiry, and explanation in `tracedecay-policy/src/scout.rs` (`EvaluatorKind::Scout`, reserved in plan 6's module tree behind its extension seam).
- Add deterministic replay fixtures from current hint evals and multi-agent scenarios.

### PR 24O — Application worker, incremental context, and model gateway

- **Ordering:** after plan 23 PR 24L and plan 24 PRs 24M–24N; consumes their application reads/refs and never creates a scout-local retrieval or executor service.
- Implement outbox scheduling, coalescing/cancellation/backpressure/fairness, snapshots, bounded tool executor, model port, run receipts, and status.
- Add provider-neutral fake gateway and Codex app-server adapter with capability selection, Spark-if-advertised support, structured output, cancellation, and circuit breaker.

### PR 24P — Host suggestion handshake and single delivery selector

- **Ordering:** after PR 24O and the plan-07 hook delivery port.
- Add pending claim/revalidation (a plan 6 `DeliveryArbiterV1` operation), runtime/component/probe-bound host hello, capability-derived delivery modes/hook points, timing/expiry, receipts, provider conformance, and no-hook-wait benchmarks.
- Test every typed capability disposition, stale/mismatched/reinstalled bundle and probe, reconnect-on-change, and a host-advertised mode absent from the generated ledger; none may silently widen delivery or fall back to a similarly named hook.
- Shadow current delivery before selecting one V2 owner.

### PR 25F — Context Scout Observatory and Turn timeline

- Add subsystem status/funnels, queue/model/tool/host/privacy/cost/quality views and Loom Scout lane/inspector.
- Add Settings navigation using plan 20 generated descriptors, plan 09 configuration views, and plan 21 presentation components.

### PR 31O — Incremental Scout Hint Lab and evaluation harness

- Register the Hint/scout evaluator in the generic experiment catalog; add exact/recorded/current-best-effort replay, session-level state, immutable branches, bounded sweeps/ablations, aligned stage playback, shadow/counterfactual timing, qrels, corpus metrics, minimization, saved/exported reproducibility, and fixture promotion.
- Verify plan 6's immutable evaluator ports plus plan 9's hermetic resource receipt and zero live counter/delivery/cache/lease/effect mutations; assert every artifact lands only in the generic experiment/run/stage family and no scout-specific lifecycle/store/route exists.

### PR 33D — Historical evidence migration and shadow parity

- Import historical hint/delivery/outcome evidence, run time-safe corpus, and generate parity/coverage receipts; V1 JSONL `emitted/followed/ignored/suppressed` evidence maps into plan 6 `HintOutcomeRecordV1` rows with the legacy-heuristic attribution class per [12-root-compatibility-migration.md](./12-root-compatibility-migration.md)'s migration inventory.
- Do not backfill live pending suggestions.

### PR 37H — Scout convergence and V1 deletion gate

- Delete duplicate classifier routing, model config, delivery selection, renderer, counter, and analytics paths.
- Require architecture/import scans, generated inventory parity, one delivery owner, and complete deletion receipts.

## 22. Cross-plan integration map

| Plan | Required integration |
|---|---|
| [01-domain-crate.md](./01-domain-crate.md) | IDs, exact Thread/Turn/session/agent address, immutable event/provenance/evidence, watermarks, privacy types |
| [02-store-crate.md](./02-store-crate.md) | owner-shard repositories, outbox consumer, checkpoints, atomic state/envelope/claim receipts, retention/recovery |
| [03-capture-crate.md](./03-capture-crate.md) | provider-native origin, Turn/tool/file/Git/agent/worktree events, continuity, sanitizer receipts |
| [04-projectors-crate.md](./04-projectors-crate.md) | trigger/logical-message/lifecycle/outcome/status projections and rebuild determinism |
| [05-query-crate.md](./05-query-crate.md) | canonical bounded query/search/graph/time execution; no scout-local search |
| [06-policy-crate.md](./06-policy-crate.md) | pure pinned selection via `DeliveryArbiterV1`/`DeliveryCandidateV1`, shared `HintStateSnapshot`/`HintStateProposal` state, silence, dedupe/cooldown/budgets, write-free replay ports, `HintOutcomeRecordV1` outcome semantics |
| [07-hooks-crate.md](./07-hooks-crate.md) | bounded pending claim/render/delivery, provider handshake, zero model/tool wait, host conformance |
| [08-tool-catalog-crate.md](./08-tool-catalog-crate.md) | model/tool capability eligibility, effect/egress/cost/privacy metadata, generated binding inventory |
| [09-application-crate.md](./09-application-crate.md) | workflow ownership, authorization, frozen snapshots, tool/model ports, transactions, status/use cases |
| [10-api-crate.md](./10-api-crate.md) | typed routes, jobs, SSE snapshot/delta/gap/backpressure, loopback auth/OpenAPI |
| [11-dashboard-frontend.md](./11-dashboard-frontend.md) | Observatory, Causal Loom Scout lane, inspector, Hint Lab, Settings, visual/performance/accessibility contracts |
| [12-root-compatibility-migration.md](./12-root-compatibility-migration.md) | root model/daemon/host adapters, V1 anti-corruption boundaries, cutover/deletion receipts |
| [13-research-provenance-and-context-anchors.md](./13-research-provenance-and-context-anchors.md) | exact planning/session/research corpus anchors and future implementation handoff |
| [14-historical-failure-regression-matrix.md](./14-historical-failure-regression-matrix.md) | noisy/repeated/missed hints, wrong project/worktree, daemon/model/hook/output failures as regression fixtures |
| [15-search-quality-evaluation-and-retrieval-research.md](./15-search-quality-evaluation-and-retrieval-research.md) | causal corpus, qrels, candidate pooling, search metrics, hard negatives, retrieval-anchor quality |
| [16-cross-project-repository-worktree-scope.md](./16-cross-project-repository-worktree-scope.md) | exact federation scope, project-set versions, worktree/ref/snapshot identity, nearby-agent coordination |
| [17-official-public-api-and-sdks.md](./17-official-public-api-and-sdks.md) | stable public use cases/events/clients, pagination/retrieval anchors, auth and compatibility |
| [18-secret-detection-redaction-and-private-data-safety.md](./18-secret-detection-redaction-and-private-data-safety.md) | model/tool/hook/UI sink firewalls, egress, quarantine, retention, secret canaries |
| [19-system-defragmentation-convergence-and-extensibility.md](./19-system-defragmentation-convergence-and-extensibility.md) | one engine/policy/catalog/application/delivery path, allowed DAG, deletion and entropy gates |
| [20-configuration-control-plane.md](./20-configuration-control-plane.md) | all settings, model references, grants, budgets, floors, history/status/UI/CLI/API parity |
| [21-cli-mcp-tool-surface-and-output-unification.md](./21-cli-mcp-tool-surface-and-output-unification.md) | generated bindings/views, Markdown/JSON defaults, status/errors/cursors/anchors/render parity |
| [`24-canonical-task-plan-graph-and-multi-agent-executor.md`](./24-canonical-task-plan-graph-and-multi-agent-executor.md) | `WorkItemId`/initiative/task-ticket presentation/dependency/claim/context-packet ownership, cross-repository work-item relations, safe task views, and event schemas consumed here |

## 23. Verification strategy

### 23.1 Domain/store/projector

- property-test deterministic IDs, logical-message identity, address validation, fingerprints, ordering, expiry, and schema round trips;
- concurrency-test simultaneous events/runs for one Turn and many agents/worktrees;
- fault-inject every transaction/lease/checkpoint/claim/delivery kill point;
- rebuild projectors twice from the same journal and compare canonical digests;
- verify deletion/retention/anchor referential integrity and no cross-shard payload copy.

### 23.2 Worker/model/tool

- fake clock/catalog/query/model gateways for deterministic coalescing, cancellation, fairness, backpressure, budgets, and circuit breaking;
- capability tests prove every eligible tool is read-only and every mutation/egress path is rejected by type/catalog/application layers;
- app-server fake covers capability discovery, configured/actual model, Spark advertised/missing, structured output, malformed output, timeouts, disconnect, cancellation, restart, token/cost omission, and child reaping;
- prompt-injection fixtures ensure retrieved content cannot widen tools/scope/effects or alter delivery policy;
- secret canaries traverse every input/result/output/error/trace path with zero forbidden sink matches;
- model-unavailable and disabled modes produce deterministic candidates or clean silence without error hints.

### 23.3 Hook/host

- existing deterministic hint evals score relevance, silence, compactness, rotation/dedupe, and generic-boilerplate absence;
- Codex/Claude/Cursor/Kiro adapter tests verify address, native context channel, payload bytes, late/expiry, claim retry, unknown delivery, and unsupported modes;
- render raw model-visible hook input from real transcripts and verify ordinary prompts are not wrapped as noisy hook messages;
- benchmark hook path with no pending envelope, one eligible, one stale, store contention, daemon reconnect, and many agents; assert no model/query/network calls;
- replay copied parent/subagent prompts and assert one logical opportunity without suppressing distinct agents incorrectly.
- run adapter fixtures with required steering, advisory steering, lifecycle checkpoint, deterministic hint, and scout envelope contending at the same boundary; prove the declared precedence, one bounded Stop continuation, no in-flight side-effecting-tool interruption, truthful next-Turn fallback, separate receipts, and zero hint-budget debit for suppressed scout work.

### 23.4 Product/API/output

- generated inventory proves every use case has CLI/MCP/API/SDK/dashboard disposition, effect class, request/result schema, format, pagination, anchors, auth, and errors;
- every CLI command passes human/Markdown/JSON/NDJSON applicability, stdout/stderr/exit, scope/freshness/coverage, and TTY determinism fixtures;
- every MCP tool defaults to compact Markdown and explicit JSON decodes to the same typed view; no double encoding or giant model transcripts;
- SSE tests snapshot/resume/gap/backpressure/resync and never drop semantic state silently;
- cross-surface fixtures prove comments are historical annotations, steering promotion is an explicit task command, and no Scout control/tool/view can promote, deliver, acknowledge, resolve, retry, or clear a required steering fence;
- browser tests cover Observatory, Loom, Hint Lab, Settings, keyboard/screen-reader, responsive, empty/silent/stale/partial/offline/overload/privacy states;
- Hint Lab tests prove plan 6's immutable evaluator ports admit zero envelope claims, deliveries, counters, feedback, config, curation, or tool mutations; plan 9's side-effect receipt reports zero production effects; persistence reaches only generic experiment/run/stage rows.

### 23.5 Evaluation artifacts

Each run publishes a sanitized manifest with corpus/query labels, cutoff, inclusion digest, config/policy/catalog/model/index/memory versions, host strata, code revision, metrics/confidence intervals, cost methodology, failures, and retrieval anchor IDs. Raw private corpus remains outside Git under the plan 13 security rules.

## 24. Definition of done

- Optional scout modes `off`, `shadow`, `deterministic`, and `model_assisted` work with no app-server/model dependency in the first three.
- One application workflow consumes immutable events with durable checkpoints, bounded coalescing/cancellation/backpressure/fairness, and crash-safe receipts.
- Deliverable envelopes require exact Thread/Turn/session/agent/logical-message address, authorized retrieval anchors, provenance, expiry, and pinned versions.
- Model selection is capability/config based; Spark works when explicitly selected and advertised, and its absence never causes implicit substitution.
- The model can request only cataloged bounded reads; all mutation, arbitrary shell/MCP, ungranted network, scope widening, and curation effects are impossible.
- Pure policy produces useful silence, compact selection, logical/category/anchor/coordination dedupe, cooldown, budgets, explanations, and one atomic state transition.
- Hooks never wait for model/search/tools/network and inject only through a negotiated safe host context boundary.
- Scout suggestions and human/controller steering remain different typed lanes: suggestions stay optional advice under hint dedupe/budgets; steering stays attempt/lease/fence/sequence bound with its own delivery/ack/disposition receipts, required completion fence, advisory non-blocking behavior, and truthful unsupported-boundary fallback.
- Late/stale/expired/superseded/unknown-delivery behavior is deterministic, visible, and duplicate-safe.
- Many-agent/multi-worktree/cross-project scenarios retain exact identity and surface only authorized evidence-bearing nearby-work summaries.
- Canonical task/ticket graph changes reach an exact Turn only for evidence-backed dependencies, overlaps, blockers, handoffs, context packets, or invalidated assumptions; task/claim anchors explain relevance and high-volume global-board activity remains silent.
- Outcome attribution uses linked evidence and correct denominators; adjacency alone never means adoption or prevented work.
- Observatory, Causal Loom, Hint Lab/playground, Settings, CLI, MCP, API, SDKs, status, and doctor expose the same typed state and controls.
- Plan 20 owns every setting and safety floor; plan 09 owns semantic typed views; plan 21 owns every binding/renderer/format rule and may not duplicate those views.
- Offline, session replay, shadow, and controlled A/B gates meet the precision/silence/noise/token/latency/cost/privacy/idempotency targets before staged delivery.
- There is no generic availability boilerplate, no per-item curation approval/apply/rollback UI, no historical delivery backfill, and no permanent second hint/delivery engine.
- Migration ends with generated inventory parity, one delivery owner, V1 deletion receipts, and architecture tests proving the dependency DAG.
