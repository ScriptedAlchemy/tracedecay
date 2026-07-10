# TraceDecay V2 Hooks Crate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (- [ ]) syntax for tracking.

**Goal:** Build a bounded host-hook runtime that losslessly captures provider events, obtains replayable hint decisions, and acknowledges Codex, Claude Code, Cursor, and Kiro without coupling host latency to indexing, projection, cross-project queries, or storage internals.

**Architecture:** tracedecay-hooks owns host wire normalization, hot-path orchestration, deadline and durability policy, reply rendering, and provider conformance. It delegates durable frames to tracedecay-capture, policy/context work to narrow tracedecay-application ports, and capability metadata to tracedecay-tool-catalog; it never opens a database, mutates policy state directly, or implements provider transcript parsing twice.

**Tech Stack:** Rust 2024; serde/serde_json; bytes; thiserror; async-trait or boxed futures matching workspace convention; tokio only for orchestration/tests; proptest; Criterion; V2 domain/capture/catalog/application contracts. Policy is reached only through the application port.

---

## 1. Contract Lock

This plan owns master-plan PR 24F. It lands after application PR 24A establishes the narrow hook port and may use the commit sequence in Section 15, but remains one hook-runtime boundary in program numbering.

Plan [`24-canonical-task-plan-graph-and-multi-agent-executor.md`](24-canonical-task-plan-graph-and-multi-agent-executor.md) may supply exact task/attempt/context-packet refs to plan-22 suggestions and bounded executor lifecycle signals. Hooks never enumerate boards, schedule/claim/cancel/complete work, widen an executor grant, or inject unaddressed sibling context.

- tracedecay-capture owns spool files, framing, fsync, recovery scans, source continuity, rewrite generations, immutable observation appends, and capture manifests.
- tracedecay-hooks owns host request decoding, normalization into HookRequestV1, deadline/durability selection, application-policy invocation, host response encoding, and acknowledgement receipts.
- tracedecay-policy owns deterministic intent, hint, routing, suppression, dedupe, cooldown, escalation, budget, rendering decisions, and missed-capability/correction outcome proposals.
- tracedecay-tool-catalog owns immutable capability/use-case metadata and host/tool bindings. Hooks may resolve a pinned snapshot; they may not hard-code a second tool catalog.
- tracedecay-application composes captured request facts, authorized query/memory/skill candidates, policy evaluation, evaluation/state recording, and explicit proposed effects.
- [`22-incremental-context-scout-and-suggestion-envelopes.md`](22-incremental-context-scout-and-suggestion-envelopes.md) owns optional asynchronous model/read exploration and durable suggestion envelopes. Hooks only claim/revalidate/render an already prepared envelope through `HookApplicationPort`; they never start or wait for scout work.
- tracedecay-store and tracedecay-projectors are behind capture/application ports. This crate has no SQL, connection, migration, projection, blob, Git, network, or filesystem implementation.
- Exact replay mode names are domain `ReplayMode::ExactDeterministic`, `ReplayMode::RecordedResult`, and `ReplayMode::CurrentBestEffort`.
- A host acknowledgement is not an observation commit, hint emission, or acted outcome. Each has a separate typed receipt/event.
- Deterministic candidates and incremental-scout candidates enter one application/policy delivery selector — `DeliveryArbiterV1` in [`06-policy-crate.md`](06-policy-crate.md) §9.1.3, which arbitrates both as `DeliveryCandidateV1` submissions under one `HintStateSnapshot` version compare-and-swap — plus one dedupe/cooldown/budget state and one outcome model. A host invocation cannot receive both engines' duplicate advice and receives at most one `InjectContext`.
- Provider source rows remain provider-owned and unchanged at their native source. TraceDecay hooks retain privacy-domain-bound locators/fingerprints plus sanitized observations; query-time human-message classification from merged PR #410 is a projection/filter concern, and hooks never delete sanitized copied-subagent observations.

## 2. Goals

- Keep notification-only hook added latency p95 at or below 10 ms and prompt-evaluation hook p95 at or below 25 ms on the versioned reference corpus.
- Capture direct user prompts, copied parent prompts, subagent instructions, protocol tool results, model output notifications, tool calls/results, approvals, edits, shell events, compaction, workspace/session lifecycle, agent lifecycle, handoffs, goals, and host errors with explicit origin/coverage.
- Capture and refresh privacy-safe agent presence/work claims with parent/goal, repo/worktree/ref/PR/file/symbol/query scopes, intent, optional <=160-character classified summary (a character cap, distinct from the 160-token hint payload cap), anchors, TTL/status, and declared redundancy.
- Use deterministic observation/idempotency inputs when the host exposes native IDs/offsets and persisted allocation when it does not.
- Make durability explicit: accepted in memory, queued, fsynced locally, committed to the observation journal, and projected are different states.
- Never silently drop canonical prompt, tool, approval, edit, reasoning-visibility, agent, goal, or outcome events under concurrency or backpressure.
- Preserve one order per source/session/agent where evidence exists; never fabricate a total order across concurrent agents.
- Handle duplicate delivery, retry, missing sequence, late records, transcript rewrite/truncation, host restart, daemon restart, disk-full, permission, corruption, and timeout deterministically.
- Pin policy bundle, tool catalog, config, index, memory, skill, profile, project-resolution, and vector-watermark references for every prompt evaluation.
- Preserve evidence origin/trust separately from payload text. Only host/provider-declared typed tool/compiler/result fields can become trusted failure facts; prompt text, pasted logs, and arbitrary tool output remain untrusted content unless independently verified.
- Preserve the exact sanitized injected payload and host response envelope by receipt-bound digest; retain only a locator/digest for provider-owned raw input.
- Support then-versus-now Hint Lab replay without invoking a host or mutating counters/state.
- Make provider support a generated conformance matrix, not scattered match statements.

## 3. Non-Goals

- No transcript history scan, LCM compression, graph sync, Git refresh, repository indexing, embedding, cross-project fan-out, projection rebuild, automation run, or remote API call on the synchronous path.
- No hidden chain-of-thought capture. Only provider-exposed reasoning artifacts/visibility markers pass through capture.
- No direct fact, skill, scheduler, automation, query, or policy-state mutation.
- No direct use of rusqlite, libsql, sqlx, Axum, MCP server/rendering, dashboard, GitHub, git2, reqwest, std::process, or arbitrary filesystem paths.
- No implicit retry that can inject the same hint twice. Retry requires an idempotent invocation and delivery receipt.
- No assumption that cwd identifies one project or that a session has a primary project.
- No current-project fallback: hooks carry domain `ScopeSelectorV2` plus zero-to-many workspace candidates. Missing/ambiguous/stale scope becomes explicit coverage or deliberate `AllAuthorized`, never first CWD/base checkout/current branch graph.
- No security-product expansion. Existing explicit blocking pre-tool decisions retain parity; ordinary guidance remains fail-open and silent on internal failure.

### 3.1 Convergence boundary

Hooks own only host wire adaptation and bounded orchestration in [`19-system-defragmentation-convergence-and-extensibility.md`](19-system-defragmentation-convergence-and-extensibility.md). Capture/Plan [`18`](18-secret-detection-redaction-and-private-data-safety.md) owns sanitization and durability; policy owns decisions; application owns composition/effects; catalog owns capabilities.

| Boundary | Contract |
|---|---|
| Enters | Bounded provider wire bytes in transient memory, invocation/access/deadline context, generated host descriptor, capture/application/catalog ports. |
| Exits | Receipt-bound sanitized hook request, actual durability/ack receipts, one application evaluation request, sink-eligible host response, delivery evidence, safe degradation. |
| Upstream owners | Domain owns values; capture parses/sanitizes/persists; application supplies resolved scope/snapshots; policy/catalog own decision/routing metadata. |
| Downstream owners | Host adapter delivers; capture records; projectors derive outcomes. Hooks never query stores, rank, project, scan secrets, or mutate policy. |
| Extension seam | New host/hook point requires generated capability/descriptor, bounded decoder/renderer mapping, privacy field map, origin/trust mapping, conformance/fuzz/latency fixtures, and cutover receipt. |
| Scale/concurrency | Stateless adapters, explicit deadlines, per-source idempotency, bounded capture/application calls, fair per-agent spools behind capture, silence on uncertain optional guidance. |
| Migration/retirement | V1 host handlers shadow one hook point at a time. After parity/delivery/privacy/latency receipts, delete that live handler; retain only redacted fixtures and recorded evidence. |

## 4. V1 Seams and Future-Master Inputs

| V1 seam | Behavior to preserve or replace | V2 disposition |
|---|---|---|
| src/hooks/mod.rs | Shared JSON reading, project/session lookup, analytics, hint formatting/dedupe | Split wire/common adapters, application ports, and generated host descriptors. Delete only after all host cutovers. |
| src/hooks/codex.rs | Session start, user prompt, subagent start, post-tool, post-compact, workspace/context hints | CodexAdapter conformance rows; no direct memory/index/policy calls. |
| src/hooks/claude.rs | Pre-tool block, session/subagent start, post-tool, prompt submit, stop | ClaudeAdapter; preserve explicit block/allow semantics and tool matcher parity. |
| src/hooks/cursor.rs, cursor_compact.rs, cursor_shell.rs | Before prompt, post-tool, file/shell/workspace, precompact, session start/end/stop, bounded ingest | CursorAdapter plus capture scheduling effects; no inline transcript ingest. |
| src/hooks/kiro.rs | Pre-tool, prompt, post-tool and transcript catch-up | KiroAdapter with explicit coverage where the host lacks richer lifecycle events. |
| src/hooks/tool_hints.rs and classifiers/evals | Classification, routing, dedupe, cooldown, payload | Compatibility policy bundle in tracedecay-policy. Hooks only build RequestFacts and render the returned envelope. |
| src/hooks/memory_inject.rs | Prompt recall candidate selection/injection | Application/query candidates plus policy retrieval decision; no store read in adapter. |
| src/hooks/hint_outcomes.rs | Emitted/acted/unresolved attribution | Capture delivery evidence; projectors/policy own terminal attribution. |
| src/hooks/post_tool_use.rs | Host tool-name matching, output/error/edit extraction | Generated tool/host binding plus normalized ToolActivityFacts. Provider source remains referenced only through an opaque privacy-domain-bound locator. |
| src/hooks/steering.rs | Bootstrap/session context, index/project guidance | Versioned policy/catalog templates with host reply rendering. |
| src/mcp/hook_events.rs | FileEdit, Shell, WorkspaceOpen, SessionStart, IncrementalSync notification planning | Compatibility adapter emits canonical hook observations and proposed application effects; MCP notification transport stays thin. |
| daemon hook notification/spool paths | Process routing, sync debounce, branch tracking | Capture/application worker consumes effects asynchronously; hook runtime records route/fallback evidence. |

Base/future-master inputs refreshed on 2026-07-10:

- The inspected base `99ad19bc` contains merged PR #405 legacy identity adoption and #412 daemon/update drain safety. Host requests resolve one adopted identity. Shutdown/update hooks record lifecycle lease, in-flight drain, background-writer stop, checkpoint, and service-state receipts separately and cannot acknowledge safe restart before them.
- PR #407 user-profile Hermes consolidation. Hermes/curator/reflector/skill-writer activity is actor/workflow evidence inside the user's profile, never a separate hook profile.
- PR #410 copied-subagent prompt collapse. Hook normalization records native `PromptOrigin` evidence and projectors map it into `tracedecay-domain::MessageOrigin`; every sanitized native observation is retained, while direct_user/subagent/tool_result filters and parent-representative dedupe remain query/projector behavior.
- PR #411 foreign-skill ownership/remediation. Hook hints and diagnostics must not suggest update/delete when catalog/application says the package is foreign to this installation; the safe route is info/no-action or explicit manual ownership transfer.
- Publication master `6c4b8b91` includes #407/#410/#411/#413/#414/#415/#416/#417/#419/#420/#422/#423/#424. Open #418 is refreshed before PR 24F; #414/#419 affect generated edit-tool descriptors, #417 makes split identity explicit coverage/silence rather than first-candidate routing, and #423/#424 retrieval/accounting behavior is accepted hint-context/outcome-measurement input. PR #409 remains historical.

Before PR 24F begins, refresh open PRs, master, installed host versions, hook manifests, application hook-port schema, and catalog digest. Drift becomes a manifest difference, not an undocumented assumption.

## 5. Exact File and Module Tree

~~~text
crates/tracedecay-hooks/
├── Cargo.toml
├── src/
│   ├── lib.rs                    # curated runtime/adapter/public contracts
│   ├── error.rs                  # stable failure and host-response codes
│   ├── request.rs                # HookRequestV1, origin, native identity
│   ├── response.rs               # HookResponseV1 and host-neutral effects
│   ├── receipt.rs                # append/evaluation/delivery/ack receipts
│   ├── budget.rs                 # latency, bytes, tokens, candidates, deadlines
│   ├── durability.rs             # required durability and acknowledgement rules
│   ├── backpressure.rs           # tier selection and typed degraded behavior
│   ├── runtime.rs                # HookRuntime orchestration only
│   ├── ports.rs                  # capture, application, clock, metrics traits
│   ├── facts/
│   │   ├── mod.rs
│   │   ├── prompt.rs             # direct/subagent/protocol origin facts
│   │   ├── tool.rs               # call/result/approval/edit/shell/error facts
│   │   ├── agent.rs              # spawn/handoff/join/interrupt/goal facts
│   │   ├── coordination.rs       # presence, work claim, TTL, scope/redundancy facts
│   │   ├── workspace.rs          # cwd/ref/worktree hints, never canonical IDs
│   │   └── lifecycle.rs          # session/compact/stop/workspace lifecycle
│   ├── adapters/
│   │   ├── mod.rs                # HostHookAdapter and immutable registry
│   │   ├── common.rs             # bounded JSON/wire helpers
│   │   ├── codex.rs
│   │   ├── claude.rs
│   │   ├── cursor.rs
│   │   └── kiro.rs
│   ├── render/
│   │   ├── mod.rs                # host response selection
│   │   ├── codex.rs
│   │   ├── claude.rs
│   │   ├── cursor.rs
│   │   └── kiro.rs
│   ├── conformance/
│   │   ├── mod.rs                # descriptor-driven fixture runner
│   │   ├── manifest.rs           # host versions/events/coverage/digests
│   │   └── differential.rs       # V1/V2 normalized/reply comparison
│   └── telemetry.rs              # bounded labels and timing summaries
├── tests/
│   ├── support/mod.rs
│   ├── request_contract.rs
│   ├── host_conformance.rs
│   ├── hot_path.rs
│   ├── durability_ack.rs
│   ├── concurrency_ordering.rs
│   ├── backpressure.rs
│   ├── crash_recovery.rs
│   ├── hint_replay.rs
│   ├── outcome_evidence.rs
│   ├── privacy_security.rs
│   └── v1_differential.rs
├── fixtures/
│   ├── codex/
│   ├── claude/
│   ├── cursor/
│   ├── kiro/
│   └── manifests/
└── benches/
    ├── notification.rs
    ├── prompt.rs
    ├── concurrent_agents.rs
    └── host_render.rs
~~~

Companion files owned elsewhere:

~~~text
crates/tracedecay-domain/src/hooks/{mod.rs,request.rs,receipt.rs}.rs
crates/tracedecay-capture/src/spool/{client.rs,frame.rs,recovery.rs}.rs
crates/tracedecay-policy/src/evaluators/{hint.rs,routing.rs}
crates/tracedecay-tool-catalog/src/{runtime.rs,bindings/hook.rs}
crates/tracedecay-application/src/ports/hooks.rs
crates/tracedecay-application/src/use_cases/hooks/{capture.rs,evaluate.rs,deliver.rs}.rs
src/hooks/v2_compat.rs
src/mcp/hook_events_v2.rs
~~~

No production file may exceed 800 lines. Provider files contain mapping only; shared policy/identity/capture behavior cannot migrate into them.

## 6. Dependency and Ownership Rules

Allowed direction:

~~~text
tracedecay-domain
  ↑          ↑                         ↑
capture      tool-catalog              tracedecay-application
  ↑          ↑                         ↑
  └── capture client ──├── catalog snapshot ─────────┘
                         tracedecay-hooks
                               ↑
                   host executable/MCP notification
~~~

Hooks may depend on domain request/receipt value types, capture client contracts, catalog snapshots, and narrow application hook ports. It may not import `tracedecay-policy` directly; application owns policy/query/memory/skill composition and returns one pinned result. It also may not depend on store/projectors/query implementations, root McpServer/DashboardState, provider session parsers, or V1 global singletons.

### Consumes and produces

| Boundary | Consumes | Produces |
|---|---|---|
| `tracedecay-domain` | Hook/request/origin/durability/receipt IDs, payload refs, sensitivity, continuity, watermarks | No domain writes; normalized value instances only |
| `tracedecay-capture` client | Bounded append contract and actual durability receipt | `HookRequestV1` capture frames, deadlines, idempotency keys; no spool I/O implementation |
| `tracedecay-tool-catalog` | Pinned capability/host-binding snapshot and catalog digest | Binding lookups/availability refs only; no route classification or catalog mutation |
| `tracedecay-application` | One pinned authorized evaluation/result/delivery-recording port | Request facts, captured observation ref, deadline, delivery receipt; no direct policy/query/memory/skill call |
| Host executable/MCP notification | Bounded provider wire request and invocation context | Host wire response, explicit acknowledgement/degradation, safe diagnostics |
| Observability | Safe clock/metric sink | Low-cardinality stage timings, durability/coverage/reason codes; never payload literals |

The crate never produces canonical events, projections, policy state, tool definitions, Git state, memory/facts, or automation mutations. Those effects occur only through the declared capture/application boundaries.

CI runs:

~~~bash
cargo tree -p tracedecay-hooks --edges normal
rg -n 'rusqlite|libsql|sqlx|axum|reqwest|octocrab|git2|std::process|Command::|src/dashboard|src/mcp' crates/tracedecay-hooks/src
~~~

Expected: no forbidden dependency or source match. std::fs is also forbidden except a compile-gated conformance-fixture loader in tests; production spool I/O belongs to capture.

## 7. Public Request, Response, and Port Contracts

The domain companion module defines transport-neutral IDs and enums; adapters may add private wire structs.

~~~rust
pub enum HostKind { Codex, ClaudeCode, Cursor, Kiro }

pub enum HookPoint {
    SessionStart,
    PromptSubmit,
    SubagentStart,
    PreToolUse,
    PostToolUse,
    Approval,
    BeforeFileEdit,
    AfterFileEdit,
    AfterShell,
    WorkspaceOpen,
    ScopeChanged,
    PreCompact,
    PostCompact,
    Stop,
    SessionEnd,
    IncrementalSync,
}

pub enum PromptOrigin {
    DirectUser,
    CopiedParentPrompt { parent_message: Option<EntityRef> },
    SubagentInstruction { parent_agent: Option<EntityRef> },
    ToolResultProtocol { invocation: Option<EntityRef> },
    ProviderProtocol { native_kind: Option<NativeKindCode> },
    Unknown,
}

// Fixture-locked projector mapping:
// DirectUser -> MessageOrigin::DirectUser
// CopiedParentPrompt | SubagentInstruction -> MessageOrigin::DelegatedAgentPrompt
// ToolResultProtocol -> MessageOrigin::ToolResultProtocol
// ProviderProtocol -> MessageOrigin::ProviderProtocol
// Unknown -> MessageOrigin::Unknown

pub struct NativeEventIdentity {
    pub native_event_id: Option<NativeEventLocatorDigest>,
    pub source_offset: Option<u64>,
    pub source_next_offset: Option<u64>,
    pub rewrite_generation: Option<u64>,
    pub record_fingerprint: KeyedSourceRecordFingerprint,
}

pub struct HookRequestV1 {
    pub invocation_id: HookInvocationId,
    pub profile_id: ProfileId,
    pub host: HostKind,
    pub hook_point: HookPoint,
    pub source: SourceInstanceId,
    pub requested_scope: ScopeSelectorV2,
    pub native: NativeEventIdentity,
    pub session_hint: Option<AliasRef>,
    pub actor_hint: Option<AliasRef>,
    pub agent_hint: Option<AliasRef>,
    pub parent_agent_hint: Option<AliasRef>,
    pub prompt_origin: Option<PromptOrigin>,
    pub occurred_at: Option<UtcMicros>,
    pub received_at: UtcMicros,
    pub facts: HookFacts,
    pub payload: PayloadRef,
    pub sensitivity: DataSensitivity,
    pub sanitization_receipt: SanitizationReceiptId,
    pub access: HookAccess,
    pub budget: HookBudget,
}
~~~

Raw paths, tokens, credentials, environment maps, query literals, prompts, arguments, and results are absent from structured fields. Authorized content resides behind PayloadRef. `requested_scope` uses the shared domain selector unchanged. Workspace facts carry privacy-domain digests plus zero-to-many candidate aliases and freshness; identity resolution occurs in application/projectors and never selects the first/current candidate silently.

~~~rust
pub enum HookEffect {
    InjectContext(PromptEligibleText),
    Allow,
    Deny { code: BlockingDecisionCode, message: LogSafeText },
    ScheduleCaptureCatchUp(CaptureRequest),
    ScheduleProjectSync(ProjectSyncRequest),
    RecordDeliveryAttempt(DeliveryAttempt),
}

pub struct HookResponseV1 {
    pub invocation_id: HookInvocationId,
    pub effects: Vec<HookEffect>,
    pub evaluation: Option<PolicyEvaluationId>,
    pub response_digest: SanitizedOutputDigest,
    pub degraded: Vec<HookDegradation>,
}

pub struct HookExecutionReport {
    pub append: HookAppendReceipt,
    pub evaluation: Option<EvaluationReceipt>,
    pub delivery: Option<DeliveryReceipt>,
    pub acknowledgement: HostAcknowledgementReceipt,
    pub timings: HookTimings,
}

pub struct HookCaptureResult {
    pub request: HookRequestV1,
    pub append: HookAppendReceipt,
}
~~~

HookEffect is a proposal until the host adapter delivers it. Deny is legal only for catalog-declared blocking hook points with a policy decision carrying a blocking rule ID. Notification and ordinary hint failures return an empty response, not denial.

The remaining boundary values are explicit:

~~~rust
pub struct HookAccess {
    pub profile_id: ProfileId,
    pub privacy_domain: PrivacyDomainId,
    pub allowed_sensitivity: BTreeSet<DataSensitivity>,
    pub access_digest: AccessPolicyDigest,
}

pub struct HookDeadline {
    pub received_at: Instant,
    pub hard_deadline: Instant,
}

pub struct HookEvaluationRequest {
    pub invocation_id: HookInvocationId,
    pub request_facts: HookFacts,
    pub captured_observation: ObservationId,
    pub access: HookAccess,
    pub requested_catalog: ToolCatalogRef,
    pub deadline: HookDeadline,
}

pub struct HookEvaluationResponse {
    pub evaluation: EvaluationRecord,
    pub response: HookResponseV1,
    pub state_transition: Option<HintStateProposal>,
    pub input_vector: VectorWatermark,
    pub coverage: CoverageReportV1,
}

pub struct HostInvocationContext {
    pub profile_id: ProfileId,
    pub source_id: SourceInstanceId,
    pub received_at: UtcMicros,
    pub budget: HookBudget,
    pub access: HookAccess,
}

pub struct HostWireResponse {
    pub media_type: &'static str,
    pub bytes: Bytes,
    pub digest: SanitizedOutputDigest,
}

pub struct EvaluationReceipt {
    pub evaluation: PolicyEvaluationId,
    pub request_facts_digest: Digest,
    pub bundle: PolicyBundleRef,
    pub catalog_digest: Digest,
    pub state_version_before: EntityVersionId,
    pub state_version_after: Option<EntityVersionId>, // None when no transition was proposed or the CAS lost
    pub committed: bool,
    pub recorded_at: UtcMicros,
}

pub struct HostAcknowledgementReceipt {
    pub invocation_id: HookInvocationId,
    pub durability: AppendState,
    pub response_digest: Option<SanitizedOutputDigest>,
    pub degraded: Vec<HookDegradation>,
    pub acknowledged_at: UtcMicros,
}
~~~

`HintStateProposal`/`HintStateSnapshot` field definitions and the version compare-and-swap token are owned by [`06-policy-crate.md`](06-policy-crate.md) §9.1.2; `CoverageReportV1` is the canonical shared coverage type owned by [`01-domain-crate.md`](01-domain-crate.md).

HookFacts is a tagged union of PromptFacts, ToolActivityFacts, AgentFacts, CoordinationFacts, WorkspaceFacts, and LifecycleFacts from src/facts. `CoordinationFacts` carries presence/claim/heartbeat/TTL/status/redundancy and safe scope anchors; raw prompt/task text is never a coordination summary. DeliveryReceipt records invocation/evaluation/response digest, attempt ordinal, provider acknowledgement ID when available, status, timestamp, and error code without raw payload text.

~~~rust
pub trait HostHookAdapter: Send + Sync {
    fn descriptor(&self) -> &'static HostConformanceDescriptor;
    fn decode(
        &self,
        wire: &[u8],
        context: &HostInvocationContext,
    ) -> Result<Unclassified<RawHookRequestV1>, HookWireError>;
    fn render(
        &self,
        response: &HookResponseV1,
    ) -> Result<HostWireResponse, HookWireError>;
}

pub trait HookCapturePort: Send + Sync {
    fn sanitize_and_append<'a>(
        &'a self,
        request: Unclassified<RawHookRequestV1>,
        durability: RequiredDurability,
        deadline: HookDeadline,
    ) -> BoxFuture<'a, Result<HookCaptureResult, HookCaptureError>>;
}

pub trait HookApplicationPort: Send + Sync {
    fn evaluate<'a>(
        &'a self,
        request: HookEvaluationRequest,
        deadline: HookDeadline,
    ) -> BoxFuture<'a, Result<HookEvaluationResponse, HookApplicationError>>;
    fn record_delivery<'a>(
        &'a self,
        receipt: DeliveryReceipt,
    ) -> BoxFuture<'a, Result<(), HookApplicationError>>;
}
~~~

HookApplicationPort returns one pinned application result containing RequestFacts digest, policy bundle, catalog digest, config/index/memory/skill snapshots, vector watermark, decision/explanation digests, state-transition proposal, exact rendered payload reference, coverage, and substitutions. Hooks never assemble these by reading services separately. `RequestFacts` is the typed digestable snapshot defined in [`06-policy-crate.md`](06-policy-crate.md) §9.1.1; `evaluate` routes deterministic candidates and any pending scout envelope through plan 06's `DeliveryArbiterV1` (§9.1.3), so one invocation yields at most one `InjectContext` under one hint-state compare-and-swap.

## 8. Durability, Acknowledgement, and Idempotency

~~~rust
pub enum RequiredDurability {
    ProcessMemory,
    DaemonQueue,
    LocalFsync,
    JournalCommit,
}

pub enum AppendState {
    Accepted,
    Queued { queue_sequence: u64 },
    Fsynced { spool: tracedecay_domain::SpoolReceipt },
    Committed { append: tracedecay_domain::AppendReceipt },
}

pub struct HookAppendReceipt {
    pub observation_id: ObservationId,
    pub idempotency_key: ObservationKey,
    pub requested_scope_digest: ScopeSelectorDigest,
    pub state: AppendState,
    pub duplicate: bool,
    pub continuity: SourceContinuity,
    pub acknowledged_at: UtcMicros,
}
~~~

Domain `SpoolReceipt` is the one spool-receipt vocabulary: capture's spool client returns it directly, so `AppendState::Fsynced` embeds it without an adapter type (there is no separate hook spool receipt).

Defaults:

| Event class | Required before host acknowledgement | Degradation |
|---|---|---|
| Direct/copy/subagent prompt, tool call/result, approval, file edit, agent/goal/handoff, outcome | LocalFsync | If unavailable, return typed degraded acknowledgement and emergency per-invocation capture receipt; never claim durable. |
| Session/workspace/compaction lifecycle | DaemonQueue; LocalFsync when daemon unavailable | May coalesce only identical rebuildable lifecycle notifications after one durable representative. |
| Project sync/index notification | DaemonQueue | Can coalesce by project/ref/path digest; canonical source event remains captured. |
| Hint evaluation/delivery | Evaluation record plus state transition committed when budget permits; otherwise append delivery-pending receipt | No hint on uncertain state; never inject twice. |

Idempotency:

- Prefer provider event/call/message IDs plus source generation and content digest.
- When only an offset exists, use source artifact, rewrite generation, [offset,next_offset), and record digest.
- When neither exists, application insert-or-reads a persisted allocation keyed by host/session/hook-point/native digest. Random process-local IDs cannot determine duplicate identity.
- The host retry of one invocation returns the stored render/delivery receipt when its policy/catalog/environment digest still matches; a digest mismatch returns a typed `stale_environment` error with no re-evaluation and no redelivery (plan 22 §11 envelope-claim retries follow the same rule).
- A transcript rewrite increments generation, emits RewriteDetected, and appends superseding observations. It never overwrites old evidence.
- Late records retain occurred/ingested times and source continuity. They do not renumber established Turns or imply causation.

## 9. Hot Path and Deadline Contract

The runtime executes these timed stages:

1. Decode bounded host input: 1 MiB default, 16 MiB only for declared compaction/tool-result hooks.
2. Normalize native IDs, origin, typed facts, payload reference, sensitivity, and access.
3. Append at required durability through HookCapturePort.
4. For evaluative hook points only, request one application-owned immutable evaluation snapshot.
5. Render at most one bounded host response envelope.
6. Record delivery attempt/result and host acknowledgement independently.
7. Enqueue slow capture catch-up, project sync, projection, correlation, outcome, and analytics work after acknowledgement.

~~~rust
pub struct HookBudget {
    pub total: Duration,
    pub capture: Duration,
    pub evaluation: Duration,
    pub render: Duration,
    pub max_wire_bytes: u64,
    pub max_hint_tokens: u32,
    pub max_candidates: u32,
}
~~~

Budget defaults:

- notification: total 10 ms target, 50 ms hard deadline; capture 8 ms, no evaluation;
- prompt: total 25 ms target, 100 ms hard deadline; capture 8 ms, evaluation 14 ms, render 3 ms;
- explicit pre-tool block: 25 ms target, 100 ms hard deadline;
- compaction/session catch-up: synchronous envelope remains 25 ms; heavy work is scheduled;
- hint tokens: `max_hint_tokens` defaults to 96 rendered tokens with a 160-token hard cap — the same token ledger plan 06's `DeliveryArbiterV1` debits for scout payloads (plan 22 §9), so sync hints and scout envelopes share one budget.

Hard timeout behavior:

- ordinary guidance: no hint, HookDegradation::DeadlineExceeded, durable non-content receipt;
- explicit blocking rule: use the catalog-declared host security fallback, never an accidental blanket deny;
- capture timeout: acknowledgement states the actual durability reached;
- delivery timeout: no emitted outcome; record DeliveryUnknown for later reconciliation.

No stage may begin if its remaining deadline is below the descriptor minimum. Cancellation is checked before capture, snapshot acquisition, policy evaluation, render, and delivery recording.

## 10. Many-Agent Ordering, Backpressure, and Crash Semantics

Many hook processes may arrive for the same profile, session, worktree, or shard.

- The daemon/capture service is the normal single writer per shard. Hook processes send bounded frames over a private local channel.
- If the daemon is unavailable, capture writes a uniquely named O_EXCL fallback segment, fsyncs the file and containing private directory, and returns its exact durability. Multiple processes never append to a shared unlocked fallback file.
- The writer assigns shard outbox sequence transactionally. Source sequence comes only from provider/native evidence or the capture source ledger; arrival order is not source order.
- Each source/session/agent stream exposes contiguous, duplicate, gap, late, rewrite, and unknown continuity. Cross-stream display uses occurred time, ingested time, producer, source sequence, and event ID only as deterministic presentation order.
- Parent-child/spawn/handoff/tool-result/goal causation requires provider/native references or a later evidence assertion. Same worktree or close timestamps are correlation candidates only.
- Queue thresholds are measured in frames, bytes, age, and disk budget. Tier 1 coalesces rebuildable sync/status notifications; Tier 2 spills all canonical frames durably; Tier 3 disables optional enrichment; Tier 4 returns typed overload for new optional work while preserving canonical capture.
- Prompts, tool activity, approvals, edits, visible reasoning markers, agent lifecycle, goals, hint delivery, corrections, and outcomes are never coalesced or dropped.
- Writer batching is bounded by 1,000 frames, 4 MiB, or 5 ms transaction time. It preserves per-source order while interleaving sources fairly.
- Read snapshots and policy inputs never hold writer locks. Busy/locked state becomes partial coverage or silence, not an unbounded wait.
- Disk-full reserves a small emergency receipt area for non-content hashes/status; it does not pretend payload durability.

Crash matrix:

| Kill point | Required recovery |
|---|---|
| Before frame creation | No acknowledgement; host retry is new/duplicate-resolved. |
| After frame write, before fsync | Recovery verifies framing/checksum and discards torn tail; no durable claim. |
| After fsync, before acknowledgement | Retry finds same idempotency key and returns existing receipt. |
| After observation commit, before outbox commit | Impossible: one transaction. |
| After evaluation record, before injection | Delivery state remains pending; retry uses existing decision and delivers at most once. |
| After injection, before delivery record | Host/provider receipt reconciliation yields Delivered or Unresolvable; never guesses ignored. |
| During rewrite/gap repair | Old generation remains queryable; checkpoint does not advance over unexplained gap. |
| During WAL checkpoint/backup | Committed observations survive; repair emits a manifest/receipt. |

## 11. Hint Request Facts, Replay, and Outcomes

Hook RequestFacts are immutable, minimal, and content-referenced; the typed shape is [`06-policy-crate.md`](06-policy-crate.md) §9.1.1's `RequestFacts`, and this list is its field inventory:

- provider/host/hook point/version;
- prompt origin and direct-user/subagent/protocol evidence from #410;
- session/actor/agent/parent aliases and resolution coverage;
- available capability/catalog digest and host-installed availability;
- workspace/index/project/ref candidates with freshness;
- tool call/result/error/edit facts with provider field, source event, parser version, and trust class;
- bounded memory/skill/query candidates supplied by application;
- prior hint state snapshot and evaluation horizon;
- current presence/work claim, nearby-claim query snapshot, declared redundancy, and coordination dedupe/cooldown/ack state;
- explicit clock, deadline, access, sensitivity, and vector watermark.

The live path records:

Candidate -> rejected/eligible -> category/route -> privacy -> repetition/dedupe/cooldown -> latency/token budget -> rendered payload -> delivery -> terminal outcome.

Every transition records a stable reason code. The exact sanitized payload, response envelope, provider result, and relevant source events are receipt-bound and content-addressed inside their privacy domain; provider-owned raw input contributes only a locator/digest. Metrics store category/digests, never raw prompt/path/tool arguments.

Outcomes:

- suggested_before_action links evaluation, delivery, recommended capability/tool, and later directly/inferred action evidence;
- missed_capability is created by the versioned policy/projector after an alternative observed action, not by the adapter;
- human_correction references the exact user event, corrected intent/route/scope/target and prior evaluation when present; it is evidence, not automatically a negative label;
- acted requires a linked invocation/capability event; temporal adjacency alone is heuristic;
- ignored is not emitted merely because the horizon ended; terminal names remain Observed, Unobserved, or Unresolvable with evidence/coverage;
- delivery_failed and delivery_unknown cannot enter the emitted denominator;
- each eligible evaluation persists as exactly one plan 06 `HintOutcomeRecordV1` row keyed by evaluation, carrying horizon, denominator-eligibility flags, and attribution evidence joins.

Hint Lab receives the stored HookRequestV1 ref, RequestFacts snapshot, bundle/catalog/config/index/memory/skill refs, exact delivery record, and outcome refs. ExactDeterministic refuses missing/redacted artifacts; RecordedResult verifies stored digests without running; CurrentBestEffort lists every substitution and performs no write.

Coordination evaluation runs only at session start, subagent start, `BeforeFileEdit` (or a catalog-declared edit pre-tool equivalent), catalog-declared expensive-research `PreToolUse`, or `ScopeChanged`. It may add at most one compact advisory context item. Planned redundancy, acknowledgement, cooldown, unchanged material overlap, or partial/unsafe claims suppress it. It cannot cancel, reassign, lock, message another agent, or mutate claims on the synchronous path.

## 12. Provider Conformance Matrix

| Host | Required V1 entry points/events | V2 required normalized coverage |
|---|---|---|
| Codex | hook_codex_session_start, hook_codex_user_prompt_submit, hook_codex_subagent_start, hook_codex_post_tool_use, hook_codex_post_compact | Session/prompt/subagent/tool/compact; response-item tool kinds; goal create/update; parent/subagent IDs; additional_context render; explicit absent coverage for unsupported approvals/events. |
| Claude Code | hook_pre_tool_use/evaluate_hook_decision, hook_claude_session_start, hook_claude_subagent_start, hook_claude_post_tool_use, hook_prompt_submit, hook_stop | Pre-tool allow/deny, session/subagent/prompt/tool/stop, tool_use/tool_result pairing, parent tool-use IDs, workflow/handoff evidence, context render. |
| Cursor | hook_cursor_before_submit_prompt, subagent/post-tool, session start/end/stop, precompact, after file/shell, workspace open | Prompt/subagent/tool/session/compact/edit/shell/workspace, Composer/agent origin, file paths as classified locators, JSON reply. |
| Kiro | pre-tool, prompt-submit, post-tool | Delegation/tool/prompt facts, bounded catch-up request, explicit gaps for unsupported lifecycle. |
| MCP/daemon notification | FileEdit, Shell, WorkspaceOpen, SessionStart, IncrementalSync | Canonical hook observation plus async project-sync proposal; branch/worktree hints are candidates until identity/Git evidence resolves them. |

For every row, fixtures cover:

- minimal valid, maximal valid, unknown forward field, malformed, oversized, missing ID/time/path, secret, Unicode, retry, duplicate, late, gap, rewrite;
- direct user, copied parent prompt, subagent instruction, protocol tool result, unknown origin;
- presence/work claims, heartbeat/TTL, every redundancy mode, session/subagent/pre-edit/expensive-research/scope-change coordination gates, planned-overlap acknowledgement, and one-compact-hint maximum;
- multi-repo/project/worktree, generic zero-project, moved/adopted/linked/detached cases, `sessions.project_key` conflict, Claude first-CWD change, active-base-versus-PR-worktree graph mismatch, ignored dependency hint retaining scope, and stale registry/store candidates;
- tool success/error/retry/missing result, approval allow/deny, edit/shell variants;
- exact V1 normalized fields and host response where compatibility is required;
- no panic and safe empty response for unknown forward event.

## 13. Privacy and Security

- Hook channels, fallback spool segments, payloads, and receipts are mode 0600 under the active profile/privacy domain; directories are 0700.
- Decode into transient `Unclassified` fields, then call the one capture sanitizer before any spool/journal/evaluation. Hooks never scan/redact/mint receipts. Secret-like or incomplete content never enters FTS, vectors, facts, fixtures, metrics, errors, hints, exports, or general spools.
- Request/access digest binds profile, privacy domain, sensitivity grant, host, and installed integration identity. A response cannot be replayed under different access.
- Validate JSON depth, string/array counts, UTF-8, declared lengths, media type, IDs, and all host output escaping.
- Ignore environment variables and paths not in the explicit invocation allowlist. Hash classified locators before telemetry.
- Blocking messages use catalog-owned safe templates; provider text cannot inject terminal control sequences or response-envelope fields.
- Fuzz every wire adapter, framed receipt, host renderer, and retry record. Add malicious nested JSON, decompression/large-string, duplicate-key, path traversal, symlink, control-character, and schema-forward cases.
- A lab/conformance run uses read-only ports and write sentinels; fixture promotion is a separate reviewed application command with redaction scan.

## 14. Observability and Performance Gates

Metrics:

- hook_invocations_total by host/hook point/result only;
- stage latency distributions for decode/normalize/capture/snapshot/evaluate/render/record/ack;
- actual durability, queue depth/bytes/oldest age, spill/coalesce/overload, recovery/torn frames;
- continuity duplicate/gap/late/rewrite/unknown counts;
- policy candidate/suppression/delivery/terminal-outcome categories with catalog/policy version;
- coordination eligible/emitted/suppressed/acted/handoff/duplicate-avoided/false-positive/unresolved with policy/query versions, never agent/task text as metric labels;
- source/profile/shard/index watermarks and partial/redacted coverage through drill-down receipts, not high-cardinality metric labels.

Release gates:

- notification p95 <=10 ms, p99 <=25 ms; hard deadline breach <0.1%;
- prompt/pre-tool p95 <=25 ms, p99 <=75 ms; hard deadline breach <0.1%;
- 100 concurrent agents at 1,000 events/s for 10 minutes: zero unexplained canonical loss/duplicate, per-source order preserved, projected visibility p95 <=2 s after drain;
- process kill at every Section 10 point: complete commit or safe retry, zero false durable/emitted claims;
- disk/WAL pressure reaches explicit degradation tiers and recovers without unbounded memory;
- secret corpus: zero secret-bearing search/vector/fact/metric/fixture/export hit;
- host conformance: 100% declared event/reply rows have fixture and parity disposition;
- outcome: >=90% eligible evaluations terminal within horizon, false attribution <1% on labeled corpus;
- trust/noise: zero adversarial prompt/pasted-log promotion to trusted compiler/tool failure, repeated-hint budget and useful-silence fixtures pass, and every injected hint names its trusted routing evidence or abstains;
- new production files <=800 lines and no provider duplication of policy/capture logic.

## 15. PR 24F TDD and Commit Sequence

Commands run from repository root with the checkout-local target directory. Do not set CARGO_TARGET_DIR or TRACEDECAY_DATA_DIR unless Cargo reports actual target-lock contention.

### Commit 1: Contracts, budgets, and adapter registry

**Files:** Cargo.toml/workspace; crate Cargo.toml; src/{lib,error,request,response,receipt,budget,durability,ports}.rs; src/adapters/{mod,common}.rs; tests/{request_contract,host_conformance,privacy_security}.rs.

- [ ] Write failing schema/validation tests for every HookPoint, PromptOrigin, `ScopeSelectorV2`, missing-time rule, payload sensitivity, budget bound, unsupported blocking point, and adapter descriptor uniqueness; include multi-repo/worktree, empty explicit selector, first-CWD, base-checkout/PR-worktree, and stale-registry cases.
- [ ] Run cargo test -p tracedecay-hooks --test request_contract --test host_conformance --test privacy_security. Expected: fail because crate/types do not exist.
- [ ] Implement the pure contracts and immutable adapter registry; generate JSON Schema fixtures and stable digests.
- [ ] Re-run. Expected: all tests pass and unknown forward host event maps to typed UnsupportedEvent without panic.
- [ ] Commit: feat(hooks): define bounded host hook contracts.

### Commit 2: Capture durability and hot-path runtime

**Files:** src/{runtime,backpressure,telemetry}.rs; capture spool client companion; tests/{hot_path,durability_ack,backpressure}.rs; benches/{notification,prompt}.rs.

- [ ] Add failing tests ack_never_overstates_durability, canonical_event_never_coalesces, optional_sync_coalesces_after_representative, timeout_returns_silent_degradation, duplicate_returns_same_observation, and queue_budget_is_bounded.
- [ ] Run focused tests. Expected: fail because HookRuntime/capture client are absent.
- [ ] Implement Sections 7–10 using capture/application fakes; no production file I/O.
- [ ] Re-run tests and Criterion baselines. Expected: correctness passes; benchmark report includes corpus/host/hook/runtime/reference-machine IDs and meets Section 14.
- [ ] Commit: feat(hooks): add durable bounded hook runtime.

### Commit 3: Codex and Claude adapters

**Files:** src/adapters/{codex,claude}.rs; src/render/{mod,codex,claude}.rs; fixtures/codex; fixtures/claude; tests/{host_conformance,v1_differential,outcome_evidence}.rs.

- [ ] Freeze redacted V1 fixtures for every entry point in Section 12, including tool/result/error, subagent parent IDs, pre-tool deny, compact, direct/copy/subagent/protocol origins.
- [ ] Run differential tests. Expected: fail before adapters exist.
- [ ] Implement mapping/render only; use catalog binding and application policy result.
- [ ] Re-run. Expected: normalized/request/reply parity passes or a fixture records an intentional versioned difference.
- [ ] Commit: feat(hooks): port Codex and Claude host adapters.

### Commit 4: Cursor, Kiro, and MCP/daemon notification adapters

**Files:** src/adapters/{cursor,kiro}.rs; src/render/{cursor,kiro}.rs; src/conformance/*; root internal-shadow adapters; fixtures/{cursor,kiro}; tests/{host_conformance,v1_differential}.rs.

- [ ] Add all Section 12 fixtures, linked-worktree/detached/moved/adopted-store cases, and #410 prompt-origin cases.
- [ ] Run conformance/differential tests. Expected: fail before mappings exist.
- [ ] Implement adapters and async proposed effects; remove inline ingest/sync from new path.
- [ ] Re-run. Expected: every descriptor event has a fixture and no direct store/index/process call exists.
- [ ] Commit: feat(hooks): port remaining host event adapters.

### Commit 5: Concurrent-agent, crash, replay, and privacy harness

**Files:** tests/{concurrency_ordering,crash_recovery,hint_replay,privacy_security}.rs; benches/{concurrent_agents,host_render}.rs.

- [ ] Add deterministic scheduler/load tests for 100 parent/subagents, presence/work-claim heartbeat/TTL, same/parallel-worktree overlap, planned redundancy, five coordination gates, one-hint/dedupe/cooldown/ack, duplicate/gap/late/rewrite, daemon loss, fallback segment collision, disk full, locked reader, bundle/catalog publication, kill points, exact/recorded/best-effort replay, delivery unknown, human correction, and secret corpus. Replay parent prefix `019f4906`, four PR #359 child agents, and Cursor session `ebc96a27-b046-4c88-865f-b38d76da9d2d` from the shared coordination manifest.
- [ ] Run tests. Expected: at least one ordering/durability/replay assertion fails before final recovery/reconciliation handling.
- [ ] Complete idempotent retry, fair writer scheduling contracts, delivery reconciliation, and no-write replay adapters.
- [ ] Re-run all crate tests/benches. Expected: all Section 14 gates pass.
- [ ] Commit: test(hooks): prove concurrent capture and replay safety.

### Commit 6: Shadow migration and cutover receipts

**Files:** src/conformance/differential.rs; src/hooks/v2_compat.rs; integration manifests/config; compatibility tests/docs.

- [ ] Add shadow tests proving one host invocation yields one V1 effect owner, one non-effecting V2 evaluation, no double hint, comparable normalized/evaluation/reply digests, and explicit uncomparable coverage.
- [ ] Enable v2_hooks_shadow per host/hook point; collect 24-hour parity/latency/privacy/continuity report.
- [ ] Cut over one hook point at a time with profile/source freeze watermark, V1/V2 state digest, bundle/catalog/adapter versions, feature flag, and rollback procedure.
- [ ] Preserve V1 adapters only inside the bounded shadow/rollback harness and V1 evidence through the data rollback window. Once a hook point cuts over, stale installed hooks/daemons/plugins fail the exact protocol/catalog handshake with restart/reinstall/update guidance; they never execute a V1 fallback or old tool name.
- [ ] Commit: refactor(hooks): route host integrations through V2 runtime.

## 16. Cutover, Rollback, and Deletion Criteria

Cutover order: notification-only session/workspace -> post-tool/edit/shell -> prompt submit -> subagent/agent lifecycle -> compaction -> explicit pre-tool blocking. Each step requires:

- refreshed host manifest and future-master base;
- zero unexplained normalization/capture/reply gaps;
- p95/p99 and queue/disk gates;
- exact durability and duplicate evidence;
- shadow mode with V1 sole effect owner;
- host-native diagnostic;
- rollback drill.

Rollback flips one host/hook-point feature flag to V1, restores V1 hint-state ownership from the receipt, leaves V2 observations/evaluations immutable for diagnosis, and prevents shadow from applying effects.

Delete a V1 hook function/file only when:

1. its every wire event/reply appears in the generated conformance manifest;
2. the bounded shadow/cutover/rollback receipt is accepted and the rollback window is formally closed;
3. one release of read-only compatibility evidence remains available;
4. replay and outcome records no longer reference executable V1 code without an archived bundle/adapter;
5. no installer/plugin manifest emits the V1 command;
6. host diagnostics pass after removal;
7. rollback window is formally closed.

Do not delete sanitized native copied-subagent prompt rows under #410; only retire duplicate query/render paths after parent-representative parity.

## 17. Final Verification

- [ ] cargo fmt --check. Expected: exit 0.
- [ ] cargo clippy -p tracedecay-domain -p tracedecay-capture -p tracedecay-policy -p tracedecay-tool-catalog -p tracedecay-hooks --all-targets -- -D warnings. Expected: exit 0.
- [ ] cargo test -p tracedecay-hooks --all-features. Expected: all unit/integration/property tests pass, none ignored.
- [ ] Run all existing src/hooks, installer/plugin, MCP hook-event, session ingest/search, analytics/hint outcome, automation, and provider fixture suites. Expected: compatibility passes.
- [ ] Run four hook benchmarks and 100-agent load/crash matrix on the recorded reference machine. Expected: every Section 14 gate passes.
- [ ] Run secret/fuzz/permission/path/symlink corpus. Expected: zero secret-bearing index/metric/fixture/export and no escape.
- [ ] Run the forbidden-import/dependency commands in Section 6. Expected: no production violations.
- [ ] Run the placeholder scan using split regex atoms: rg -n 'TB[D]|TO[D]O|\bimplement lat[e]r\b|\bfill i[n]\b|\bappropriate erro[r]\b|\bsimilar to Tas[k]\b' docs/plans/tracedecay-v2/07-hooks-crate.md. Expected: no matches.
- [ ] Inspect every generated host conformance row and deletion receipt. Expected: no unowned event, reply, effect, state, or fallback.

## 18. Definition of Done

- Every supported host event is normalized, durably classified, fixture-locked, and visible with explicit coverage.
- Concurrent agents cannot silently lose, duplicate, reorder within a known source, or falsely causally link canonical activity.
- Hook latency is independent of graph/projector/network/background work and meets the recorded gates.
- Every hint is tied to exact request facts, policy/catalog/environment digests, payload, delivery, and terminal evidence.
- Agent presence/claims remain current through bounded heartbeats; coordination hints occur only at five material workflow gates, are compact/advisory/planned-redundancy-aware, and cannot spam or mutate other agents.
- Every request and durable receipt binds the unchanged `ScopeSelectorV2` digest; multi-repo/project/checkout/worktree/ref/snapshot/generation ambiguity/staleness is explicit and hooks never infer current project/CWD/first CWD/base checkout/current graph.
- Missed Git/tool capability and human correction are observable first-class outcomes.
- #405/#407 identity/profile migration, #410 prompt origin, #411 remediation ownership, and #412 drain/shutdown semantics are preserved; #413 contributes actual protocol version only.
- Hooks contain no database, query, policy implementation, Git, network, process, or product UI logic.
- Every persisted/evaluated hook request and rendered hint uses the Plan 18 receipt/sink-eligible contract; raw provider wire exists only during bounded decode and cannot serialize through a hook-owned port.
- Shadow cutover and rollback have been proven separately for every host/hook point.
