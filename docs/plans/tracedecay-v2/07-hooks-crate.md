# TraceDecay V2 Root Hooks Boundary Implementation Plan

**Plan 32 integration:** hooks may capture native workflow lifecycle and deliver already-authorized `SteeringTargetV1::{WorkflowRun,WorkflowNode}` envelopes only at capability-proven safe boundaries, including at most one bounded Stop/SubagentStop continuation. They never execute workflow source, decide replay/readiness/cache, convert comments/signals/hints into steering, or own run/task completion; Plan 32 owns those semantics and receipts.

> **Accepted-base refresh delta (audit 29 / packet 30):** host-hook registration,
> dispatch, event mapping, error handling, and uninstall are byte-identical
> `B`→`D` (only `src/hooks/steering.rs` changed). Preserve current asymmetries
> (Codex six-hook `config.toml` + trust table; Claude seven-entry bundle with
> unique `PostToolUseFailure`; absent Codex `PreToolUse`; Hermes in-process
> callbacks) until deliberately migrated; **decide** the hook-trust-state owner
> and restore a compact Codex parent-owns-writes token. See
> [`30-baseline-refresh-candidate-packet.md`](30-baseline-refresh-candidate-packet.md)
> §5, §7.5 and FM-169/FM-170.

**Goal:** Build a bounded host-lifecycle runtime that losslessly captures provider events, obtains replayable hint decisions, and acknowledges Codex, Claude Code, Cursor, Hermes, and Kiro without coupling host latency to indexing, projection, cross-project queries, or storage internals. “Hook” is the canonical lifecycle boundary, not a claim that every host exposes a hook file: Hermes plugin/session/tool/gateway/delegation/scheduler callbacks and source-broker catch-up lower into the same request/receipt state machine with an exact capability disposition.

**Architecture:** the private root `v2::hooks` module owns host wire normalization, hot-path orchestration, deadline and durability policy, reply rendering, and provider conformance. It delegates durable frames to `tracedecay-capture`, policy/context work to narrow `tracedecay-application` ports, and capability metadata to `tracedecay-tool-catalog`; it never opens a database, mutates policy state directly, or implements provider transcript parsing twice. It remains a module because root is its only production consumer; plan-19 import lints preserve the boundary without publishing another crate.

**Tech Stack:** Rust 2024; serde/serde_json; bytes; thiserror; async-trait or boxed futures matching workspace convention; tokio only for orchestration/tests; proptest; Criterion; V2 domain/capture/catalog/application contracts. Policy is reached only through the application port.

The module consumes the hook facet of plan 08/27's one canonical `HostIntegrationManifestV1`; capture, installation, MCP tools, skills, roles, and executors consume sibling facets with identical host/version/event/capability codes. The bundle compiler lowers that source IR into unsigned host-specific `HostBundlePayloadV1` artifacts plus capability-difference and release inputs; PR 36R alone creates the signed `HostBundleManifestV1` release envelope. Neither generated representation nor this module copies workflow semantics. `v2::hooks` owns one wire framing/response/conformance implementation. Plan 09's application feature owns lifecycle authorization/idempotency/operation state, while plan 12's root-private deployment adapter performs approved install/update/uninstall/config effects; installer work never enters the hook hot path. The current Cline/Roo exact install/uninstall duplicates and the wider nine-installer/15-integration cluster are explicit deletion seeds, not adapters to preserve.

---

## 1. Contract Lock

This plan owns master-plan PR 24F. It lands after application PR 24A establishes the narrow hook port and may use the commit sequence in Section 15, but remains one hook-runtime boundary in program numbering.

Plan [`24-canonical-task-plan-graph-and-multi-agent-executor.md`](24-canonical-task-plan-graph-and-multi-agent-executor.md) may supply exact task/attempt/context-packet refs to plan-22 suggestions and bounded executor lifecycle signals. Hooks never enumerate boards, schedule/claim/cancel/complete work, widen an executor grant, or inject unaddressed sibling context.

- tracedecay-capture owns spool files, framing, fsync, recovery scans, source continuity, rewrite generations, immutable observation appends, and capture manifests.
- Root `v2::hooks` owns host request decoding, normalization into `HookRequestV1`, deadline/durability selection, application-policy invocation, host response encoding, and acknowledgement receipts.
- tracedecay-policy owns deterministic intent, hint, routing, suppression, dedupe, cooldown, escalation, budget, rendering decisions, and missed-capability/correction outcome proposals.
- tracedecay-tool-catalog owns immutable capability/use-case metadata and host/tool bindings. Hooks may resolve a pinned snapshot; they may not hard-code a second tool catalog.
- tracedecay-application composes captured request facts, authorized query/memory/skill candidates, policy evaluation, evaluation/state recording, and explicit proposed effects.
- [`22-incremental-context-scout-and-suggestion-envelopes.md`](22-incremental-context-scout-and-suggestion-envelopes.md) owns optional asynchronous model/read exploration and durable suggestion envelopes. Hooks only claim/revalidate/render an already prepared envelope through `HookApplicationPort`; they never start or wait for scout work.
- tracedecay-store and tracedecay-projectors are behind capture/application ports. This module has no SQL, connection, migration, projection, blob, Git, network, or filesystem implementation.
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
- No process-global workspace cache: every invocation carries the provider session's logical workspace/root set, host session identity, and explicit projectless state. A long-lived Hermes/Codex/Claude/Cursor process may interleave sessions in different repositories without one session's CWD or cached project affecting another.
- No adapter-local greeting/code regex decides whether TraceDecay speaks. Adapters report bounded evidence and plan 06's canonical `InteractionIntentClassV1`; policy owns eligibility, useful silence, and replayable reason codes.
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
| src/hooks/codex.rs | Historical subset of Codex hooks and workspace/context hints | Replace with the ten-event current Codex contract: `SessionStart`, `SubagentStart`, `PreToolUse`, `PermissionRequest`, `PostToolUse`, `PreCompact`, `PostCompact`, `UserPromptSubmit`, `SubagentStop`, and `Stop`; no direct memory/index/policy calls and no invented `PostToolUseFailure`. |
| src/hooks/claude.rs | Historical six-event subset: pre-tool, session/subagent start, post-tool, prompt submit, stop | Replace with the independent pinned 30-event Claude contract and generated per-event/type dispositions; preserve only fixture-proven semantics and never treat the six aliases as the current denominator. |
| src/hooks/cursor.rs, cursor_compact.rs, cursor_shell.rs | Before prompt, post-tool, file/shell/workspace, precompact, session start/end/stop, bounded ingest | CursorAdapter plus capture scheduling effects; no inline transcript ingest. |
| src/hooks/kiro.rs | Pre-tool, prompt, post-tool and transcript catch-up | KiroAdapter with explicit coverage where the host lacks richer lifecycle events. |
| src/hooks/tool_hints.rs and classifiers/evals | Classification, routing, dedupe, cooldown, payload | Compatibility policy bundle in tracedecay-policy. Hooks only build RequestFacts and render the returned envelope. |
| src/hooks/memory_inject.rs | Prompt recall candidate selection/injection | Application/query candidates plus policy retrieval decision; no store read in adapter. |
| src/hooks/hint_outcomes.rs | Emitted/acted/unresolved attribution | Capture delivery evidence; projectors/policy own terminal attribution. |
| src/hooks/post_tool_use.rs | Host tool-name matching, output/error/edit extraction | Generated tool/host binding plus normalized ToolActivityFacts. Provider source remains referenced only through an opaque privacy-domain-bound locator. |
| src/hooks/steering.rs | Bootstrap/session context, index/project guidance | Versioned policy/catalog templates with host reply rendering. |
| src/mcp/hook_events.rs | FileEdit, Shell, WorkspaceOpen, SessionStart, IncrementalSync notification planning | Compatibility adapter emits canonical hook observations and proposed application effects; MCP notification transport stays thin. |
| daemon hook notification/spool paths | Process routing, sync debounce, branch tracking | Capture/application worker consumes effects asynchronously; hook runtime records route/fallback evidence. |

Accepted-base inputs refreshed through 2026-07-11:

- The inspected base `99ad19bc` contains merged PR #405 legacy identity adoption and #412 daemon/update drain safety. Host requests resolve one adopted identity. Shutdown/update hooks record lifecycle lease, in-flight drain, background-writer stop, checkpoint, and service-state receipts separately and cannot acknowledge safe restart before them.
- PR #407 user-profile Hermes consolidation. Hermes/curator/reflector/skill-writer activity is actor/workflow evidence inside the user's profile, never a separate hook profile.
- PR #410 copied-subagent prompt collapse. Hook normalization records native `PromptOrigin` evidence and projectors map it into `tracedecay-domain::MessageOrigin`; every sanitized native observation is retained, while direct_user/subagent/tool_result filters and parent-representative dedupe remain query/projector behavior.
- PR #411 foreign-skill ownership/remediation. Hook hints and diagnostics must not suggest update/delete when catalog/application says the package is foreign to this installation; the safe route is info/no-action or explicit manual ownership transfer.
- Merged #441/#445 Hermes routing. Hook envelopes keep immutable installed `HostProfileRef` evidence separate from current invocation/session workspace and explicit projectless state; reinitialize/clone/reload resets runtime route/home, and profile/user events reach profile activity without project discovery or a host-home fallback.
- The normative publication snapshot is [master §2.6](../2026-07-09-tracedecay-brain-rewrite.md#26-current-master-accepted-changes) plus [plan 13](13-research-provenance-and-context-anchors.md). Hooks must acquire the lifecycle lease before configuration/store startup, never install or repair while an exclusive lifecycle owner exists, drain already accepted input, and expose typed deferral evidence. Identity, retirement, session variants, read-only search, and peer-safe checkpoint behavior remain hint-context/outcome fixtures.

Before PR 24F begins, refresh open PRs, master, installed host versions, hook manifests, application hook-port schema, and catalog digest. Drift becomes a manifest difference, not an undocumented assumption.

## 5. Exact File and Module Tree

~~~text
src/v2/hooks/
├── mod.rs                        # curated root-private facade
├── error.rs                      # stable failure and host-response codes
├── request.rs                    # HookRequestV1, origin, native identity
├── response.rs                   # HookResponseV1 and host-neutral effects
├── receipt.rs                    # append/evaluation/delivery/ack receipts
├── budget.rs                     # latency, bytes, tokens, candidates, deadlines
├── durability.rs                 # required durability and acknowledgement rules
├── backpressure.rs               # tier selection and typed degraded behavior
├── runtime.rs                    # HookRuntime orchestration only
├── ports.rs                      # capture, application, clock, metrics traits
├── facts/
│   ├── mod.rs
│   ├── prompt.rs                 # direct/subagent/protocol origin facts
│   ├── tool.rs                   # call/result/approval/edit/shell/error facts
│   ├── agent.rs                  # spawn/handoff/join/interrupt/goal facts
│   ├── coordination.rs           # presence, work claim, TTL, scope/redundancy facts
│   ├── workspace.rs              # cwd/ref/worktree hints, never canonical IDs
│   └── lifecycle.rs              # session/compact/stop/workspace lifecycle
├── adapters/
│   ├── mod.rs                    # generated capability-ledger mappings
│   ├── common.rs                 # bounded JSON/wire helpers
│   ├── codex.rs
│   ├── claude.rs
│   ├── cursor.rs
│   └── kiro.rs
├── render/
│   ├── mod.rs                    # host response selection
│   ├── codex.rs
│   ├── claude.rs
│   ├── cursor.rs
│   └── kiro.rs
├── conformance/
│   ├── mod.rs                    # descriptor-driven fixture runner
│   ├── manifest.rs               # host versions/events/coverage/digests
│   └── differential.rs           # V1/V2 normalized/reply comparison
└── telemetry.rs                  # bounded labels and timing summaries

tests/
├── hooks_v2.rs                    # integration-test harness
└── hooks_v2/
    ├── support.rs
    ├── request_contract.rs
    ├── host_conformance.rs
    ├── hot_path.rs
    ├── durability_ack.rs
    ├── concurrency_ordering.rs
    ├── backpressure.rs
    ├── crash_recovery.rs
    ├── hint_replay.rs
    ├── outcome_evidence.rs
    ├── privacy_security.rs
    └── v1_differential.rs

tests/fixtures/hooks_v2/{codex,claude,cursor,kiro,manifests}/
benches/{hooks_v2_notification,hooks_v2_prompt,hooks_v2_concurrent_agents,hooks_v2_host_render}.rs
~~~

Companion files owned elsewhere:

~~~text
crates/tracedecay-domain/src/hooks/{mod,binding,request,receipt}.rs
crates/tracedecay-capture/src/spool/{client,frame,recovery}.rs
crates/tracedecay-policy/src/evaluators/{hint.rs,routing.rs}
crates/tracedecay-tool-catalog/src/{runtime.rs,bindings/hook.rs}
crates/tracedecay-application/src/features/hooks/{ports,queries,commands,views}.rs
src/hooks/v2_compat.rs
src/mcp/hook_events_v2.rs
~~~

Production files target at most 400 lines and may not exceed the 800-line hard default ceiling without a temporary plan-19 waiver. Provider files contain mapping only; shared policy/identity/capture behavior cannot migrate into them.

## 6. Dependency and Ownership Rules

Allowed direction:

~~~text
host executable / MCP notification
                  │
                  ▼
          root::v2::hooks
          ├──→ tracedecay-domain values
          ├──→ tracedecay-capture client
          ├──→ tracedecay-tool-catalog snapshot
          └──→ tracedecay-application hook port
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

The module never produces canonical events, projections, policy state, tool definitions, Git state, memory/facts, or automation mutations. Those effects occur only through the declared capture/application boundaries.

CI runs:

~~~bash
cargo tree -p tracedecay --edges normal
rg -n 'rusqlite|libsql|sqlx|axum|reqwest|octocrab|git2|std::process|Command::|src/dashboard|src/mcp' src/v2/hooks
~~~

Expected: no forbidden dependency or source match. std::fs is also forbidden except a compile-gated conformance-fixture loader in tests; production spool I/O belongs to capture.

## 7. Public Request, Response, and Port Contracts

The domain companion module defines transport-neutral IDs and enums; adapters may add private wire structs. Host identity always uses the domain-owned opaque `HostProfileRef` plus `HostSurfaceKindV1`, backed by the generated host registry. A fixed Codex/Claude/Cursor/Kiro enum is forbidden because it would exclude the remaining registered hosts and leak a root-private type into catalog/scout contracts.

~~~rust
pub enum HookPoint {
    SessionStart,
    Setup,
    InstructionsLoaded,
    UserPromptSubmit,
    UserPromptExpansion,
    MessageDisplay,
    SubagentStart,
    SubagentStop,
    PreToolUse,
    PermissionRequest,
    PermissionDenied,
    PostToolUse,
    PostToolUseFailure,
    PostToolBatch,
    ApprovalObserved,
    Notification,
    TaskCreated,
    TaskCompleted,
    TeammateIdle,
    ConfigChange,
    CwdChanged,
    FileChanged,
    WorktreeCreate,
    WorktreeRemove,
    BeforeFileEdit,
    AfterFileEdit,
    AfterShell,
    WorkspaceOpen,
    ScopeChanged,
    PreCompact,
    PostCompact,
    Stop,
    StopFailure,
    Elicitation,
    ElicitationResult,
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
    pub host_profile: HostProfileRef,
    pub host_surface: HostSurfaceKindV1,
    pub hook_point: HookPoint,
    pub invocation_scope: HookInvocationScopeV1,
    pub definition: HookDefinitionRefV1,
    pub handler_run: HookHandlerRunRefV1,
    pub invocation_group: HookInvocationGroupRefV1,
    pub producer_build: TraceDecayBuildRefV1,
    pub collector_build: Option<TraceDecayBuildRefV1>,
    pub capability_snapshot_digest: ManifestDigest,
    pub trust_state: HostHookTrustStateV1,
    pub source: SourceInstanceId,
    pub requested_scope: ScopeSelectorV2,
    pub native: NativeEventIdentity,
    pub session_hint: Option<AliasRef>,
    pub turn_hint: Option<AliasRef>,
    pub tool_use_hint: Option<AliasRef>,
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

Plan 01 PR 4 owns one canonical `crates/tracedecay-domain::hooks` family: `binding.rs` contains definition/source/provenance/run/representation/trust vocabulary, while `request.rs` and `receipt.rs` contain non-overlapping host-neutral request/durability/result contracts. No `hooks_v1.rs` facade or duplicate enum family exists. Plan 20 consumes the binding vocabulary in configuration views, plan 03 captures request/receipt evidence, plan 08 binds immutable `HostHookBindingId` specs, and this crate adds only host-wire adapters and runtime behavior. `HookDefinitionProvenanceV1` is resolved, ambiguous candidate-set plus coverage, or generated-binding-only; the runtime never fabricates one source when Codex does not identify the launching definition.

`HookInvocationScopeV1` is the plan-01 lifecycle vocabulary covering session/setup/Turn/tool-call/tool-batch/subagent/task/team/worktree/component/elicitation/async/display scopes. Codex `SessionStart` maps to `SessionLifecycle`, `SubagentStart` to `SubagentLifecycle`, and its remaining events to `Turn`; Claude events retain native scope instead of being forced into a Turn. `HookDefinitionRefV1` binds resolved/ambiguous/generated-only source provenance, representation, content digest, host trust hash where applicable, matcher-group ordinal, handler ordinal, managed bit, and installed bundle digest. Configured definitions, host-deduped handlers, actual runs, and invocation groups remain distinct evidence.

### 7.1 Exact Codex wire and matcher contract

The Codex adapter has private closed `CodexHookWireInputV1` variants and losslessly lowers every current release field. All variants receive `session_id`, nullable `transcript_path`, `cwd`, `hook_event_name`, and `model`. `transcript_path` is an unstable convenience locator and `cwd` is an untrusted scope candidate; neither is a canonical session/project identity or permission. `SessionStart`, `PreToolUse`, `PermissionRequest`, `PostToolUse`, `UserPromptSubmit`, `SubagentStart`, `SubagentStop`, and `Stop` additionally carry closed `permission_mode=default|acceptEdits|plan|dontAsk|bypassPermissions`; a forward-unknown value is retained as bounded unknown evidence, never mapped to a known authorization mode. Subagents reuse the parent `session_id`, so `agent_id` is mandatory lineage rather than optional decoration. Merged #447's literal `[hooks.state]` parent is a V1 stock-host probe/import fixture proving semantic TOML equivalence is insufficient when observing trust state. V2's bundle emits hook definitions only and never creates or edits that host trust-state table; installation/enabling leaves every non-managed exact hash for user review in `/hooks`.

| Codex event | Scope and required event fields | Matcher | Supported response semantics |
|---|---|---|---|
| `SessionStart` | Thread; `source=startup|resume|clear|compact` | regex over `source`; `*`, empty, or omitted means all | plain text or JSON developer context; common `systemMessage`/`continue`/`stopReason`; `continue:false` stops before session proceeds |
| `SubagentStart` | Subagent start; `turn_id`, `agent_id`, `agent_type` | regex over `agent_type` | plain text or JSON subagent context and `systemMessage`; `continue:false` is recorded but cannot prevent start |
| `PreToolUse` | Turn; `turn_id`, `tool_name`, `tool_use_id`, `tool_input` | tool name, including `apply_patch` aliases `Edit|Write` and MCP names/regexes | warning/context, allow, deny, or allow plus complete replacement `updatedInput`; plain stdout ignored; exit 2 is legacy block |
| `PermissionRequest` | Turn; `turn_id`, `tool_name`, `tool_input`, optional `tool_input.description: string|null` | same tool-name/alias rules | ignored plain stdout; `systemMessage`; allow, deny+message, or no decision; any host-collected deny wins, otherwise allow wins over no decision; no rewrite/permission expansion/interrupt |
| `PostToolUse` | Turn; `turn_id`, `tool_name`, `tool_use_id`, `tool_input`, `tool_response`; Bash nonzero remains this event | same tool-name/alias rules | warning/additional context, legacy block/exit 2 feedback, or `continue:false`; feedback/stop text replaces the original result before the model continues, but the completed tool effect cannot be undone |
| `PreCompact` | Turn; `turn_id`, `trigger=manual|auto` | regex over trigger | common JSON; `continue:false` stops before compaction; plain stdout ignored |
| `PostCompact` | Turn; `turn_id`, `trigger=manual|auto` | regex over trigger | common JSON; `continue:false` stops after compaction; plain stdout ignored |
| `UserPromptSubmit` | Turn; `turn_id`, `prompt` | matcher ignored | plain text or JSON developer context; block/exit 2 rejects submission; common continuation fields apply |
| `SubagentStop` | Turn; `turn_id`, `agent_id`, `agent_type`, nullable `agent_transcript_path`, `stop_hook_active`, nullable `last_assistant_message` | regex over `agent_type` | exit-0 stdout must be JSON; `decision:block` or exit 2+stderr requests another subagent continuation, but any matching `continue:false` wins |
| `Stop` | Turn; `turn_id`, `stop_hook_active`, nullable `last_assistant_message` | matcher ignored | exit-0 stdout must be JSON; `decision:block` or exit 2+stderr creates a continuation prompt, but any matching `continue:false` wins |

Tool input/response, prompts, last messages, and transcript-derived content enter transient `Unclassified` fields and then payload refs; public facts retain only bounded typed IDs/kinds, classified locators, digests, receipts, and coverage. The adapter accepts unknown forward fields only behind the bounded forensic payload policy and never treats them as trusted tool/compiler facts. Current Codex interception gaps (`unified_exec` rich shell paths, WebSearch, and other non-shell/non-MCP tools) are explicit capability-denominator gaps: hooks are guardrails, not a complete enforcement boundary.

Codex loads every matching active definition and launches matching command handlers concurrently. Config precedence does not replace lower-layer hooks, one denial cannot prevent a sibling hook from starting, and TraceDecay never serializes foreign handlers. The invocation-group receipt records all observed TraceDecay handler runs, result arrival order, aggregation state, and the fact that unseen foreign results remain host-owned. Policy state/hint delivery is compare-and-swap keyed to canonical session/Turn/tool/agent identity so duplicate TraceDecay definitions cannot emit a second hint, while their distinct runs remain auditable.

~~~rust
pub enum HookEffect {
    InjectContext(PromptEligibleText),
    SystemWarning(LogSafeText),
    Allow,
    Block { code: BlockingDecisionCode, message: LogSafeText },
    RewriteToolInput(PayloadRef),
    RewriteToolOutput(PayloadRef),
    AskPermission { reason: LogSafeText },
    DeferTool { reason: LogSafeText },
    PermissionDecision { behavior: PermissionBehaviorV1, message: Option<LogSafeText> },
    ClaudePermissionRequestDecision(ClaudePermissionRequestDecisionV1),
    RetryDeniedTool,
    ReplaceDisplayContent(PromptEligibleText),
    UpdateWatchPaths(WatchPathSetV1),
    ElicitationDecision(ClaudeElicitationDecisionV1),
    ProvideWorktreePath(ValidatedHostDirectoryV1),
    ConfigureSessionBootstrap(ClaudeSessionBootstrapV1),
    ContinueTurn { reason: LogSafeText },
    ContinueSubagent { reason: LogSafeText },
    StopHookFlow { reason: Option<LogSafeText> },
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

HookEffect is a proposal until the host adapter delivers it. A generated per-event output validator makes illegal combinations unrepresentable: `RewriteToolInput` is legal only for `PreToolUse` and must contain a complete string `command` for Bash/`apply_patch` or a complete arguments object for MCP; it accompanies allow only. `PermissionDecision` is legal only for `PermissionRequest`. Continuation effects target only their matching stop event and are always suppressed when `stop_hook_active` proves that host flow was already continued. Blocking is legal only for catalog-declared blocking hook points with a policy decision carrying a blocking rule ID. Notification and ordinary hint failures return an empty response, not denial.

Canonical hook points preserve native semantics rather than pretending similarly named events are equivalent. `ToolActivityFacts` carries a closed native disposition (`Succeeded`, `Failed`, `Interrupted`, `DeniedBeforeExecution`, `Unknown`) plus provider event code, exit/error coverage, and whether the effect may already have occurred. Codex Bash nonzero remains `PostToolUse` with `Failed`; Claude retains distinct `PostToolUse` and `PostToolUseFailure`; an absent/unknown host field remains `Unknown`. Render legality is selected from the exact `(host, surface, version, native event, handler type)` capability row, never from `HookPoint` alone. A host-native event may therefore normalize to shared facts while retaining capabilities that no sibling host has.

Host-specific legality further narrows this union. Claude `RewriteToolOutput`, retry, display replacement, watch-path, elicitation, validated worktree-path, session-bootstrap, ask/defer, typed permission update/destination/interrupt, and continuation effects require their exact event/version schema and policy authorization. MessageDisplay replacement is display-only. Generated TraceDecay bindings never register `WorktreeCreate`: registration would replace Claude's native Git behavior and violate plan 24's no-provisioning invariant. TraceDecay may observe the resulting workspace through later host/Git/CWD evidence, and `WorktreeRemove` remains cleanup evidence only. Elicitation accept/content crosses plan 21's form-secret and URL-auth boundary and cannot manufacture consent. CwdChanged/FileChanged watch updates never imply permission to read paths. Worktree/watch outputs carry expiring authorization-bound `ProtectedHostLocatorHandleV1`s plus privacy digests; only the root effect broker resolves them to strings at final host rendering, and raw paths never cross public/domain/UI serialization. Codex cannot receive these Claude-only effects. Unknown or version-gated output fields fail before publication rather than being emitted optimistically.

Codex fields parsed but unsupported in the current release are rejected by conformance before install: `permissionDecision:"ask"`, legacy approve, PreTool `continue`/`stopReason`/`suppressOutput`, PermissionRequest `updatedInput`/`updatedPermissions`/`interrupt`, PostTool `updatedMCPToolOutput`/`suppressOutput`, and unsupported common/event combinations. TraceDecay emits neither `suppressOutput` nor handler types Codex currently skips. Parsed `suppressOutput` has no implemented effect where documented; PreTool invalid fields fail/report and the tool call continues unchanged, PermissionRequest reserved fields fail closed, and PostTool unsupported fields fail/report while ordinary result processing continues. Each outcome is a distinct receipt-tested handler result, never a generic continuation bucket.

### 7.2 Codex handler execution contract

Generated Codex hooks use exactly one `type:"command"` handler per TraceDecay matcher group. The generator never emits `prompt`, `agent`, or `async:true`; foreign instances of those parsed-but-skipped forms remain observed `UnsupportedHandler` rows and are never called healthy. Every generated command has a catalog-fixed executable/argv, explicit one-second host timeout (the internal 10/25 ms budgets still govern normal completion), optional catalog-owned `statusMessage` only when useful, and an independently escaped `commandWindows`; JSON uses `commandWindows`, while TOML input accepts `command_windows` or `commandWindows`. Omitting timeout and inheriting Codex's 600-second default is a generation failure.

Commands execute with the session `cwd`; the entrypoint therefore never resolves itself or a package resource relative to cwd. Plugin `PLUGIN_ROOT` is contained read-only package state. Although Codex supplies writable `PLUGIN_DATA` and compatibility aliases, generated TraceDecay hooks never write them; no plugin-local spool, cache, identity, or state silo exists. None of these variables is profile/Brain identity, authorization, source scope, database storage, or permission to read a transcript. Repo-local foreign hooks are diagnosed with git-root guidance, but TraceDecay's generated plugin command invokes the separately installed signed `tracedecay` binary directly.

### 7.3 Exact Claude Code wire, event, and handler contract

The Claude adapter consumes a dated, content-digested official-reference oracle independent of generated output. The current oracle contains exactly 30 events:

| Claude event | Cadence, matcher, and decisive fields | Control and TraceDecay disposition |
|---|---|---|
| `SessionStart` | session; `startup|resume|clear|compact`; `source`, optional `model|agent_type|session_title` | context plus `initialUserMessage|sessionTitle|watchPaths|reloadSkills`; command/MCP only; generated command capture/context, never writes `CLAUDE_ENV_FILE` |
| `Setup` | explicit init/maintenance; `init|maintenance`; `trigger` | context only, command/MCP only; generated metadata capture when invoked |
| `InstructionsLoaded` | async component load; `load_reason`; file/memory/load/include/glob metadata | no control; metadata capture with classified path fingerprints |
| `UserPromptSubmit` | Turn; matcher ignored; `prompt` | context/block, session title, suppress-original behavior; generated synchronous capture/hint |
| `UserPromptExpansion` | Turn; command-name matcher; original prompt plus expansion type/name/args/source | context/block; preserve expanded-versus-original lineage |
| `MessageDisplay` | streaming display; matcher ignored; Turn/message/index/final/delta | display-only replacement never changes transcript/model evidence; policy-disabled metadata-only by default |
| `PreToolUse` | tool call; tool matcher plus `if`; tool name/input/use ID | `deny > defer > ask > allow`, complete `updatedInput`, context; generated guardrail only with exact coverage/policy |
| `PermissionRequest` | approval; tool matcher plus `if`; tool/input, suggestions, no guaranteed tool-use ID | allow/deny, updated input/permissions, deny message/interrupt; generated default is no decision |
| `PermissionDenied` | auto-mode denial; tool matcher plus `if`; tool/input/denial evidence | only `retry:true` can affect flow; capture outcome, never auto-retry without policy |
| `PostToolUse` | successful tool; tool matcher plus `if`; output and duration | feedback/block, schema-valid `updatedToolOutput`, context; cannot undo tool effect |
| `PostToolUseFailure` | failed tool; tool matcher plus `if`; error, interrupt, duration | feedback/block only; retain separate failure semantics |
| `PostToolBatch` | completed parallel batch; matcher ignored; complete serialized batch results | block before next model call/context; authoritative fan-out/fan-in boundary |
| `Notification` | async notification; notification-type matcher | no control; bounded metadata capture |
| `SubagentStart` | subagent lifecycle; agent-type matcher | context only; capture exact parent/agent identity |
| `SubagentStop` | subagent terminal candidate; agent-type matcher; last message plus version-gated background tasks/session crons | continuation/context with host cap; missing registry fields are unknown and cannot prove terminal |
| `TaskCreated` | task lifecycle; matcher ignored; native task fields | exit 2 rolls back creation; `continue:false` stops the teammate entirely; provider evidence never replaces plan-24 authority |
| `TaskCompleted` | task lifecycle; matcher ignored; native task fields | exit 2 blocks completion; `continue:false` stops the teammate entirely; provider evidence never replaces plan-24 authority |
| `Stop` | Turn terminal candidate; matcher ignored; last message plus version-gated background tasks/session crons | continue feedback, maximum eight host blocks; missing registry fields are unknown and cannot prove terminal |
| `StopFailure` | API-error Turn end; error-type matcher | output and exit ignored; immutable failure observation only |
| `TeammateIdle` | team lifecycle; matcher ignored; `teammate_name` and deprecated `team_name` | may keep teammate working; advisory evidence related to plan-24 coordination |
| `ConfigChange` | async config lifecycle; config-source matcher | may block except policy settings; foreign config body never retained |
| `CwdChanged` | async scope candidate; matcher ignored; `old_cwd|new_cwd` | no decision; may replace dynamic `watchPaths` and write `CLAUDE_ENV_FILE`; generated TraceDecay emits neither |
| `FileChanged` | async watch event; literal filename watch-list semantics; path/event | no decision; may replace dynamic `watchPaths` and write `CLAUDE_ENV_FILE`; generated TraceDecay emits neither and performs no ambient read |
| `WorktreeCreate` | worktree lifecycle; matcher ignored | registration replaces default Git behavior; generated TraceDecay always omits it and observes externally created worktrees through later host/Git/CWD evidence |
| `WorktreeRemove` | worktree lifecycle; matcher ignored | no decision; cleanup evidence only |
| `PreCompact` | Turn boundary; `manual|auto`; custom instructions | may block compaction; no synchronous transcript parse |
| `PostCompact` | Turn boundary; `manual|auto`; compact summary | no decision; summary is unclassified content until sanitized |
| `Elicitation` | MCP interaction; server-name matcher; requested form/schema | accept/decline/cancel and content override under exact policy; never bypass user authorization |
| `ElicitationResult` | MCP interaction; server-name matcher; response | transform/decline under exact policy; preserve request/response lineage |
| `SessionEnd` | session lifecycle; end-reason matcher; no deadline field in wire input | no decision; runtime budget defaults 1.5 s, settings timeout may raise overall budget to 60 s, plugin timeout does not raise it, and `CLAUDE_CODE_SESSIONEND_HOOKS_TIMEOUT_MS` override is versioned oracle evidence |

Common Claude inputs include `session_id`, optional version-gated `prompt_id`, lagging `transcript_path`, `cwd`, event name, conditional `permission_mode=default|plan|acceptEdits|auto|dontAsk|bypassPermissions`, conditional `effort.level=low|medium|high|xhigh|max`, and conditional `agent_id|agent_type`. Only `SessionStart` may contain optional `model`. Generated schemas accept forward fields as bounded unknown evidence; they never guess authority or completeness.

Claude matcher compilation is versioned catalog data. `*`, empty, or omitted matches all; simple character sets use exact/list matching, other patterns use unanchored JavaScript regex; comma/hyphen behavior is minimum-version gated; `FileChanged` uses literal watch-list semantics; declared no-matcher events silently ignore a matcher. Handler `if` contains one permission rule, applies only to documented tool-event families, has no boolean composition, and fails open on unparseable Bash. Matcher/`if` coverage is a guardrail denominator, never an enforcement claim. Plugin-scoped MCP tool and agent names remain exact native identities.

Claude supports `command`, `http`, `mcp_tool`, `prompt`, and experimental `agent` handlers with event-specific eligibility. Native defaults are versioned oracle data: 600 s for command/HTTP/MCP, 30 s prompt, 60 s agent; UserPromptSubmit lowers command/HTTP/MCP to 30 s and MessageDisplay to 10 s. Generated TraceDecay bindings use only synchronous command exec form (`command:"tracedecay"`, closed `args`, `async:false`) with explicit tighter timeout and optional status text; they never use shell form, `asyncRewake`, HTTP, MCP, prompt, or agent handlers for capture/policy authority. Foreign definitions preserve command exec/shell/PowerShell fields; HTTP URL/header/`allowedEnvVars`; connected MCP server/tool/input substitution; prompt `$ARGUMENTS`/model/`continueOnBlock`; and experimental agent prompt/model/timeout semantics. HTTP non-2xx/network/timeout and disconnected/error MCP hooks are non-blocking, so neither is a durability or security boundary.

Observed environment/path semantics distinguish placeholder substitution from exported process variables: `${CLAUDE_PROJECT_DIR}`, `${CLAUDE_PLUGIN_ROOT}`, `${CLAUDE_PLUGIN_DATA}`, plugin `${user_config.*}`, `CLAUDE_CODE_REMOTE`, version-gated `CLAUDE_CODE_BRIDGE_SESSION_ID`, `CLAUDE_EFFORT`, and event-limited `CLAUDE_ENV_FILE`. None supplies Brain identity, scope, authorization, or a store root. Exec form substitutes each closed argument without a shell. Shell/PowerShell foreign fixtures cover the v2.1.198 placeholder rewrite, `$env:CLAUDE_PROJECT_DIR`, and the unsafe bare `$CLAUDE_PROJECT_DIR` PowerShell spelling; generated TraceDecay avoids every shell/version branch.

All matching Claude handlers launch in parallel. Within one event resolution the host deduplicates identical command handlers by `(command,args)` and HTTP handlers by URL, including async command definitions; async executions from separate event firings are not cross-fire deduplicated. Receipts conserve configured → matched → host-deduped → started → completed/timed-out → decision-applied/context-delivered states without fabricating foreign runs.

Claude processes JSON only on exit 0. Exit 2 behavior is event-specific; other nonzero exits normally fail open, except `WorktreeCreate` where any nonzero aborts. HTTP status alone never blocks. Universal fields are `continue`, `stopReason`, `suppressOutput`, `systemMessage`, and allowlisted `terminalSequence`; event-specific output is schema-validated. Injected `additionalContext` over 10,000 characters spills to a host session file and becomes explicit privacy/coverage degradation—TraceDecay context remains below the cap and never asks a model to open the spill. Other large inputs/errors retain ordinary bounded-decode/privacy rules. Async results record produced-at and later model-visible-at Turns; stale resume replay and `asyncRewake` are not Context Scout delivery channels.

### 7.4 One-shot task lifecycle continuation

TraceDecay may use only the stock same-host continuation semantics at `Stop` and `SubagentStop` to ask the current root agent or subagent to reconcile a bound plan-24 attempt before it exits. The generated synchronous command calls the local daemon; when eligible it returns exit-0 JSON with `decision:"block"` and one compact reason. Codex converts that reason into a new continuation prompt; Claude delivers it to the same agent as the next instruction. This path never invokes an Anthropic API, a Hermes provider profile, HTTP/MCP, or Claude `prompt`/`agent` hooks. It is a lifecycle reminder, not a model route.

The daemon evaluates plan 24's `LifecycleCheckpointNeedV1` only for an unambiguous current `WorkItemVersionId` + `ExecutionAttemptId` + fenced `TaskLeaseId/lease_epoch` + participant binding. Eligibility requires material unreported progress, blocker, handoff, or terminal-candidate evidence. A `LifecycleOwner` continuation says to invoke exactly one canonical `attempts.progress`, `attempts.block`, `attempts.complete`, or explicit handoff command with the pinned refs. An acting/reviewer/provider-internal subagent continuation can request only `attempts.participant_handoff`; it cannot heartbeat or terminalize the attempt. The hook may record terminal-candidate evidence, but it cannot infer completion from prose, mark a task terminal, create work, widen a grant, accept a review, or mutate the graph directly.

One persisted compare-and-swap winner is selected across additive/concurrent hook definitions by:

```text
(profile, host_instance, session_or_thread, turn, root_or_agent,
 work_item_version, attempt, lease_epoch, terminal_candidate,
 lifecycle_protocol_version)
```

Lifecycle checkpoint state is closed: `Ineligible | Reserved | PromptIssued | ContinuedObserved | ConfirmedByLifecycleCommand | SuppressedLoopGuard | Missed | DeliveryUnknown`. When `stop_hook_active=true`, a `PromptIssued`/uncertain-delivery reservation already exists, the lease is stale, the binding is ambiguous, or the daemon/config/trust path is unavailable, the handler returns empty success and allows the agent to stop. TraceDecay deliberately caps this feature at **one** inward continuation even though Claude currently has a larger host block cap and an override. Delivery uncertainty is at-most-once, never an automatic retry. Explicit lifecycle commands and lease reconciliation remain authoritative when hooks are absent, disabled, untrusted, interrupted, or bypassed.

Task lifecycle continuation owns a separate decision/effect slot from hints and Plan 22 suggestion envelopes. A hint cannot consume, trigger, or disguise the one-shot reminder; a reminder cannot carry exploratory context. If foreign hooks return conflicting outcomes, native host precedence still applies and the receipt records that TraceDecay could not prove delivery.

### 7.5 Live attempt steering at host-safe boundaries

An active plan-24 execution attempt is steerable while its Turn is in progress, but a task comment is not implicitly a prompt. A task comment is the shared canonical annotation entity with the registered `TaskComment` presentation role and `CommentsOn` work-item target; `TaskCommentRevisionRefV1` is a validated typed ref over one immutable annotation revision, not a second comment entity/store. An authorized actor must explicitly promote that revision, or submit the same typed payload directly, as Plan 01's `SteeringDirectiveV1` with `SteeringTargetV1::TaskAttempt`. Promotion preserves the annotation/revision/body digest as provenance; editing or tombstoning the annotation cannot mutate, repeat, or revoke the directive. Cancellation/supersession is another fenced steering command.

Plan 01 is the sole owner of `SteeringDirectiveV1`, `SteeringTargetV1`,
`SteeringRevisionV1`, requirements, delivery claims/receipts,
acknowledgements, and terminal dispositions. Plan 24 owns the task-attempt
lifecycle commands and fences; Plan 32 owns workflow-run/node lifecycle after
its integration. This crate imports those exact contracts and owns only host
capability declaration, safe-boundary selection, rendering, and observation.
It defines no hook-local steering enum, wire-domain directive, receipt, or
state machine.

Every delivery claim/receipt, acknowledgement, and terminal disposition carries a canonical `SteeringReceiptBasisDigestV1`. Its hashed basis losslessly includes directive/work-item/work-item-version, attempt, lease, authority/fence epochs, steering sequence, originating actor/authority, requirement/kind, expected packet/graph revision, sanitized payload digest, priority/expiry, idempotency-key digest, and optional promoted-comment revision. A batch receipt additionally binds ordered member basis digests plus first/last sequence and actual host boundary/capability digest. Receipt verification rehydrates those immutable rows and rejects any mismatch; a receipt cannot be replayed across attempts, epochs, controllers, payloads, or graph/packet revisions.

The daemon admits task-targeted directives through Plan 24 and workflow-targeted directives through Plan 32. Hooks cannot admit one. The resulting Plan 01 value has already passed expected target state, authority/fence, accepted-context/history/graph revision, actor authority, expiry, sanitizer, idempotency, monotonic target sequence, and catalog/config limit checks. The hook revalidates the pinned snapshot before claiming but cannot widen scope, grants, payload, priority, expiry, or target.

Delivery uses a daemon-owned per-attempt inbox projected from the canonical task event/outbox stream. Every adapter declares exact safe boundaries and acknowledgement evidence:

1. provider-native current-Turn interrupt, only when the pinned host capability proves addressed interrupt semantics;
2. after a tool result and before the next model call;
3. one `Stop`/`SubagentStop` inward continuation, sharing the persisted one-shot guard in §7.4;
4. otherwise `NextTurnOnly`.

The generated host ledger must publish and test the following conservative
baseline. A versioned capability probe may narrow or add a boundary, but a
similarly named callback never upgrades itself:

| Host surface | Native addressed boundary | After-tool boundary | Terminal/next-Turn fallback |
|---|---|---|---|
| Codex | `Unsupported` until an addressed current-Turn interrupt is present in the exact stock contract | `PostToolUse` only when the pinned host version proves it runs before the next model call | one shared `Stop`/`SubagentStop` continuation; otherwise `NextTurnOnly` |
| Claude Code | `Unsupported` unless the exact installed hook capability proves addressed in-loop context | `PostToolUse`/`PostToolUseFailure` only at the proven pre-model boundary | one shared `Stop`/`SubagentStop` block; async/rewake/notification becomes `NextTurnOnly`, never native delivery |
| Cursor | `Unsupported` by default; UI notification and composer state are not model context | registered post-tool callback only when its conformance row proves after-result/before-model ordering | registered stop/before-submit boundary when addressed; otherwise `NextTurnOnly` |
| Hermes | only an in-process agent-loop callback explicitly registered as addressed and pre-sampling; gateway/chat/Kanban notification is unsupported | in-process post-tool/pre-model callback when proven for the active CLI/delegated/task-worker lane | next agent-loop/Turn callback; gateway delivery, cron, webhook, background process, and board comments are `NextTurnOnly` evidence only |

Each row records `Unsupported` versus `DeferredNextBoundary` versus
`NextTurnOnly` rather than silently choosing the next similarly named hook.
Duplicate delivery callbacks insert-or-read the same receipt. A duplicate or
stale acknowledgement returns the canonical Plan 01 acknowledgement
disposition and cannot advance a target cursor, clear required state, or cause
another model-visible render.

No adapter interrupts an in-flight side-effecting tool, rewrites a tool result, or reports a mid-sampling injection when the host cannot prove one. Notification callbacks may wake the daemon but are not delivery evidence. Before any payload bytes leave the daemon, the adapter must win Plan 02's globally unique active member claim for every `(target, target_sequence)` in one owner-shard transaction. A uniqueness conflict returns an empty successful host response; it cannot render optimistically. The claimed batch is ordered and bounded by Plan 01's pinned limits plus the current Plan-20 lowering. Required directives retain order and are never coalesced across a semantic dependency; advisory directives may be batched only when compatible. Batch overflow leaves a bounded remainder pending rather than truncating or growing the prompt. A newly lowered limit that excludes an admitted unhanded directive records `BlockedByLimitChange`, renders zero bytes, and leaves required state fenced for supersede/cancel remediation. The host receipt records the actual `SteeringDeliveryBoundaryV1` and `SteeringDeliveryDispositionV1`, including explicit `Unsupported`, `DeferredNextBoundary`, `NextTurnOnly`, `DeliveryUnknown`, `BlockedByLimitChange`, duplicate, and stale outcomes.

Required directives are logically exactly-once: one durable claim at a deliverable boundary, zero or one model-visible delivery, explicit acknowledgement, then `Applied | Rejected | Superseded` disposition with evidence. Duplicate hook runs and reconnects insert-or-read the same claim/receipt; `DeliveryUnknown` never re-injects automatically. Every admitted required directive without a terminal disposition fences `attempts.complete`, review/integration admission, and lease-terminal publication even after delivery expiry; expiry stops new delivery attempts but cannot silently waive controller intent. Advisory directives record the same receipts when observable but never fence progress or completion. A late directive racing a terminal transaction either wins the attempt-state CAS and fences that transaction or is atomically rejected `AttemptAlreadyTerminal`; it never attaches to a successor attempt.

Codex/Claude `Stop` and `SubagentStop` can carry steering only through the same single bounded continuation reserved for that root/subagent Turn. Cursor/Hermes use only the exact registered terminal or next-loop boundary in the table above; neither inherits Codex/Claude continuation semantics by analogy. Steering has precedence over an ordinary lifecycle reminder when required steering is pending; the combined continuation still occurs at most once and contains only the bounded claimed steering plus the exact lifecycle command needed to acknowledge/disposition it. `stop_hook_active=true`, an existing `PromptIssued`, uncertain delivery, a stale lease, or any pinned per-Turn/rate/cooldown ceiling returns empty success plus the canonical deferral/limit receipt. Provider-native interrupt support, if added later, must first pass generated adapter conformance; similarly named notification, async, prompt, agent, gateway, or board handlers are not substitutes.

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
    pub requested_catalog: CatalogSnapshotRefV1,
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
    pub request_facts_digest: ManifestDigest,
    pub bundle: PolicyBundleRef,
    pub catalog_snapshot: CatalogSnapshotRefV1,
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

The invocation lifecycle is monotonic and receipt-backed: `Received -> Decoded -> Sanitized -> Appended -> Evaluated? -> Rendered? -> DeliveryAttempted? -> HostAcknowledged`. A typed terminal degradation records the furthest completed stage and actual durability. No transition may claim `Appended` before the capture receipt is recoverable, `Rendered` before output-schema validation, `DeliveryAttempted` before bytes are handed to the host transport, or `HostAcknowledged` without native acknowledgement evidence. Notification transports that expose no acknowledgement terminate as `DeliveredNoAcknowledgementObservable`, not `Acknowledged`. Retry insert-or-reads the invocation/event allocation and resumes only an unperformed stage; it does not replay a delivery whose state is `AttemptedUnknown`.

Defaults:

| Event class | Required before host acknowledgement | Degradation |
|---|---|---|
| Direct/copy/subagent prompt, tool call/result, permission request/decision, file edit, agent start/stop/goal/handoff, Turn stop/continuation, outcome | LocalFsync | If unavailable, return typed degraded acknowledgement and a non-content failure receipt; never claim durable. |
| Session/workspace/compaction lifecycle | LocalFsync through capture's service-owned ingress spool | May coalesce only identical rebuildable lifecycle notifications after one durable representative. |
| Project sync/index notification | DaemonQueue | Can coalesce by project/ref/path digest; canonical source event remains captured. |
| Hint evaluation/delivery | Evaluation record plus state transition committed when budget permits; otherwise append delivery-pending receipt | No hint on uncertain state; never inject twice. |

Idempotency:

- Prefer provider event/call/message IDs plus source generation and content digest.
- When only an offset exists, use source artifact, rewrite generation, [offset,next_offset), and record digest.
- When neither exists, application insert-or-reads a persisted allocation keyed by host/session/hook-point/native digest. Random process-local IDs cannot determine duplicate identity.
- Definition/handler/run identity is preserved separately from canonical event identity. A generated release-manifest-bound `HostHookBindingId` in fixed argv resolves the current catalog definition; stale bindings and foreign/ambiguous copies are capture-only. Because Codex supplies no definition/run ID, application persists run allocation keyed by binding, canonical host-event identity, source candidate set, and attempt evidence; indistinguishable copied definitions remain one partial-coverage group rather than fabricated distinct runs. Exact retry dedupe is handler-run-specific. A CAS/lease arbitrates only advisory context/hints so one deterministic current binding can inject them. Blocking, rewrite, permission, and continuation responses use event-specific host aggregation: security deny is never suppressed by advisory arbitration, `PermissionRequest` defaults to `NoDecision` and may allow only under separately authorized managed policy bound to exact tool/input/grant, and current signed duplicate bindings render identical policy results.
- The host retry of one invocation returns acknowledgement-only plus the stored delivery state when its policy/catalog/environment digest still matches; it never reprints a possibly delivered effect. A digest mismatch returns typed `stale_environment` with no re-evaluation/redelivery. Delivery-unknown is never automatically retried (plan 22 §11 envelope claims follow the same rule).
- Task lifecycle continuation uses §7.4's stricter persisted CAS. Exactly one current signed binding may reserve `PromptIssued`; concurrent losers, retries, a second stop with `stop_hook_active=true`, and uncertain delivery return empty success. A later explicit lifecycle command correlates to the reservation but never causes re-delivery.
- A transcript rewrite increments generation, emits RewriteDetected, and appends superseding observations. It never overwrites old evidence.
- Late records retain occurred/ingested times and source continuity. They do not renumber established Turns or imply causation.
- Continuity is orthogonal, not one mutually exclusive label. A record may be both late and part of a rewrite generation, or fill a previously recorded gap. The receipt preserves duplicate identity, generation/supersession lineage, source-range relation, lateness, and remaining gaps independently; replay cannot erase an earlier uncertainty merely by receiving a later record.

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
- Activation and recovery watermarks, hysteresis, emergency-reserve size, per-source quantum, and maximum source wait are bounded plan-20 configuration generated into the integration manifest; they are not adapter constants. Each transition records the triggering frames/bytes/age/disk measurement and configuration digest. Recovery requires every relevant measurement below its lower watermark for a stable interval, preventing tier flapping. Deficit round-robin over `(privacy domain, source instance)` with a bounded consecutive-frame quantum is the normative fairness algorithm; deterministic scheduler tests prove a noisy source cannot starve a sparse source while preserving each source's known order.
- Read snapshots and policy inputs never hold writer locks. Busy/locked state becomes partial coverage or silence, not an unbounded wait.
- Disk-full reserves a small emergency receipt area for typed manifest/keyed-fingerprint/status fields only; it does not pretend payload durability.

Fallback segments use random contained names and a separately fsynced, privacy-domain-keyed allocation index; filenames never reveal event IDs, scope, host, or idempotency-key prefixes. The index maps a keyed invocation fingerprint to segment/offset/receipt and is itself framed, checksummed, generation-stamped, and recoverable by bounded segment scan. Reconciliation claims a segment by compare-and-swap, verifies ownership/mode/checksum/sanitizer floor, imports it once, and writes a tombstone before deletion. A corrupt index or segment yields quarantined non-content diagnostics and cannot turn an unverified frame into a durable acknowledgement.

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
- provider-session logical workspace source, explicit projectless marker, `InteractionIntentClassV1`, and resolution coverage; process CWD is evidence only and never a fallback;
- tool call/result/error/edit facts with provider field, source event, parser version, and trust class;
- bounded memory/skill/query candidates supplied by application;
- prior hint state snapshot and evaluation horizon;
- current presence/work claim, nearby-claim query snapshot, declared redundancy, and coordination dedupe/cooldown/ack state;
- explicit clock, deadline, access, sensitivity, and vector watermark.

The FM-138/FM-139 conformance corpus includes greetings and acknowledgements that remain silent, ambiguous general chat, projectless memory requests, and two concurrently interleaved sessions in distinct Hermes workspaces. Deep-copy/reload fixtures prove that immutable config may be shared while locks, session/turn state, pending deliveries, and workspace bindings remain invocation/session owned.

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

## 12. Generated Provider Conformance Matrix

The checked-in human-readable table below is a historical fixture inventory, not support truth. The normative matrix is generated from the plan-27 host capability ledger at a pinned host version and assigns every `(host, surface, event, response)` one disposition: documented supported, version-gated, absent, undocumented/unknown, policy-disabled, or trust-pending. Only documented or probe-validated support can become a required adapter row. Unknown support stays unknown; it cannot be inferred from another host, a similarly named hook, an old TraceDecay handler, or a generated bundle field. CI regenerates this table and the plan-27 difference report from the same source, failing on handwritten drift, an uncovered supported row, or a required unknown/absent row.

| Host | Historical V1/probe fixture seeds | Generated V2 coverage target |
|---|---|---|
| Codex | all ten current hidden bindings and stock-wire goldens | Exact `SessionStart`, `SubagentStart`, `PreToolUse`, `PermissionRequest`, `PostToolUse`, `PreCompact`, `PostCompact`, `UserPromptSubmit`, `SubagentStop`, and `Stop`; common/event inputs, matchers/aliases, command-only execution, trust/source/definition lineage, concurrent-handler grouping, stdout/JSON/exit-2 outputs, unsupported-field failures, interception gaps, Windows lowering, and continuation guards. |
| Claude Code | six V1 aliases plus independent 30-event stock-wire oracle | Exact current 30 events from §7.3; common/event fields, versioned matcher/`if` semantics, five handler types, exec/shell/PowerShell, sync/async/rewake, source/frontmatter lifecycle, host dedupe/concurrency, universal/event outputs, exit/HTTP/MCP/prompt-agent behavior, spill/lag/terminal coverage, and explicit generated-versus-foreign dispositions. |
| Cursor | hook_cursor_before_submit_prompt, subagent/post-tool, session start/end/stop, precompact, after file/shell, workspace open | Prompt/subagent/tool/session/compact/edit/shell/workspace, Composer/agent origin, file paths as classified locators, JSON reply; steering goldens prove native interrupt unsupported by default, version-proven post-tool delivery, before-submit/next-Turn fallback, explicit unsupported/deferred truth, and duplicate/stale acknowledgement refusal. |
| Hermes | plugin memory/session/tool callbacks, gateway delivery, delegation, process/cron/webhook/Kanban transitions, compression/session switch, provider failover | Exact callback/source capability rows per CLI/gateway/background/delegated/task-worker surface; canonical session/source/chat/thread provenance without transport-based scope; model-visible context and delivery receipts; non-durable child versus leased durable-attempt distinction; source-broker catch-up with explicit lag where a callback is absent. Steering goldens distinguish an addressed in-process pre-sampling boundary from post-tool/pre-model and next-loop fallback; gateway/chat/Kanban/cron/webhook/background notifications remain unsupported as model delivery and duplicate/stale acknowledgement cannot clear state. |
| Kiro | pre-tool, prompt-submit, post-tool | Delegation/tool/prompt facts, bounded catch-up request, explicit gaps for unsupported lifecycle. |
| MCP/daemon notification | FileEdit, Shell, WorkspaceOpen, SessionStart, IncrementalSync | Canonical hook observation plus async project-sync proposal; branch/worktree hints are candidates until identity/Git evidence resolves them. |

For every generated supported/version-gated row, fixtures cover:

- minimal valid, maximal valid, unknown forward field, malformed, oversized, missing ID/time/path, secret, Unicode, retry, duplicate, late, gap, rewrite;
- direct user, copied parent prompt, subagent instruction, protocol tool result, unknown origin;
- presence/work claims, heartbeat/TTL, every redundancy mode, session/subagent/pre-edit/expensive-research/scope-change coordination gates, planned-overlap acknowledgement, and one-compact-hint maximum;
- multi-repo/project/worktree, generic zero-project, moved/adopted/linked/detached cases, `sessions.project_key` conflict, Claude first-CWD change, active-base-versus-PR-worktree graph mismatch, ignored dependency hint retaining scope, and stale registry/store candidates;
- tool success/error/retry/missing result, approval allow/deny, edit/shell variants;
- Codex all-source concurrency with reordered completion, no sibling-start suppression, advisory-only invocation-group CAS winner/losers, separately aggregated deny/rewrite/permission/continuation precedence, delivery-unknown no-redelivery, and observable handler-run audit conservation;
- Claude configured/matched/host-deduped/started/completed/context-delivered conservation across parallel synchronous handlers and repeated async firings; event-specific exit/output precedence, eight-stop-block cap, lagging transcript, stale resume context, output spill, and no foreign-handler execution in replay;
- one-shot task lifecycle continuation for root and subagent: lifecycle-owner command versus non-owner `participant_handoff`, rejection of subagent terminal authority, duplicate plugin/user definitions, parallel handlers, first stop with `stop_hook_active=false`, second with `true`, persisted CAS winner, explicit confirmation, no task binding, stale lease, daemon timeout/contention, trust/feature absence, user interrupt, Claude `StopFailure`, delivery unknown, and proof that no Anthropic/provider/prompt/agent/MCP route ran;
- active-attempt steering for root and subagent on Codex, Claude, Cursor, and Hermes: native current-Turn interrupt only when independently capability-proven, after-tool/before-model delivery, unsupported/deferred/next-Turn truth, duplicate and reconnect, duplicate/stale acknowledgement, two-controller monotonic CAS, stale lease/fence/packet/graph, in-flight side-effecting tool deferral, late-terminal single-winner race, required completion/integration fence, advisory non-blocking, hard member/byte/token/Turn/rate/cooldown ceilings, and exactly one bounded continuation where the host contract supports it;
- exact V1 normalized fields and host response where compatibility is required;
- no panic and safe empty response for unknown forward event.

## 13. Privacy and Security

- Hook channels, fallback spool segments, payloads, and receipts are mode 0600 under the active profile/privacy domain; directories are 0700.
- Decode into transient `Unclassified` fields, then call the one capture sanitizer before any spool/journal/evaluation. Hooks never scan/redact/mint receipts. Secret-like or incomplete content never enters FTS, vectors, facts, fixtures, metrics, errors, hints, exports, or general spools.
- Request/access digest binds profile, privacy domain, sensitivity grant, host, and installed integration identity. A response cannot be replayed under different access.
- Validate JSON depth, string/array counts, UTF-8, declared lengths, media type, IDs, and all host output escaping.
- Ignore environment variables and paths not in the explicit invocation allowlist. Hash classified locators before telemetry.
- Never synchronously open `transcript_path` or `agent_transcript_path`, whose format is unstable. Persist only a classified locator fingerprint and coverage, and schedule separately authorized source-broker capture when a provider adapter supports the current format. Prompt/tool/last-message bodies are sanitized payloads; scan failure retains only a durable non-content failure receipt.
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
- steering directive/claim/delivery/ack/disposition counts by closed state, actual boundary, requirement, and adapter capability version; never comment/payload text, actor ID, task ID, or retrieval content as labels;
- source/profile/shard/index watermarks and partial/redacted coverage through drill-down receipts, not high-cardinality metric labels.

Release gates:

- notification p95 <=10 ms, p99 <=25 ms; hard deadline breach <0.1%;
- prompt/pre-tool p95 <=25 ms, p99 <=75 ms; hard deadline breach <0.1%;
- 100 concurrent agents at 1,000 events/s for 10 minutes: zero unexplained canonical loss/duplicate, per-source order preserved, projected visibility p95 <=2 s after drain;
- process kill at every Section 10 point: complete commit or safe retry, zero false durable/emitted claims;
- disk/WAL pressure reaches explicit degradation tiers and recovers without unbounded memory;
- secret corpus: zero secret-bearing search/vector/fact/metric/fixture/export hit;
- host conformance: 100% generated documented/probe-validated event/reply rows have fixtures and every absent/unknown/version-gated row has a checked disposition with no fabricated adapter;
- Codex conformance: an independent ten-event required matrix cannot shrink with generated output; every stock-client lane covers exact fields/nullability, matcher semantics, additive sources/concurrency, outputs/exit codes, handler type/timeout/cwd/platform, trust/managed/feature state, and explicit interception denominator;
- Claude conformance: an independent, pinned 30-event × five-handler-type × supported-surface matrix cannot shrink with generated output; it covers all §7.3 fields/matchers/version gates/decisions, source/frontmatter/managed policy, parallel dedupe, sync/async delivery, exec/shell/PowerShell, HTTP/MCP failure, prompt/agent decisions, lag/spill/privacy, and the generated command-only subset;
- Hermes conformance: independent CLI, gateway/chat/thread, delegated child, background process, cron/webhook, Kanban/task-worker, compression, session-switch, provider-failover, projectless, project, multi-root, and multiple named-profile lanes conserve callback/source -> normalized event -> append -> context/delivery -> terminal evidence. Missing callbacks are explicit lag/coverage gaps; gateway delivery is an effect receipt rather than task success; Hermes profile or transport identity never selects TraceDecay profile/project scope.
- outcome: >=90% eligible evaluations terminal within horizon, false attribution <1% on labeled corpus;
- trust/noise: zero adversarial prompt/pasted-log promotion to trusted compiler/tool failure, repeated-hint budget and useful-silence fixtures pass, and every injected hint names its trusted routing evidence or abstains;
- new production files target <=400 lines, remain <=800 lines absent a temporary plan-19 waiver, and contain no provider duplication of policy/capture logic.

## 15. PR 24F TDD and Commit Sequence

Commands run from repository root with the checkout-local target directory. Do not set CARGO_TARGET_DIR or TRACEDECAY_DATA_DIR unless Cargo reports actual target-lock contention.

### Commit 1: Contracts, budgets, and adapter registry

**Files:** root `Cargo.toml`; `src/v2/hooks/{mod,error,request,response,receipt,budget,durability,ports}.rs`; `src/v2/hooks/adapters/{mod,common}.rs`; `tests/hooks_v2.rs`; `tests/hooks_v2/{request_contract,host_conformance,privacy_security}.rs`.

- [ ] Write failing schema/validation tests for every generated supported HookPoint, PromptOrigin, invocation scope, definition/handler-run/invocation-group identity, `ScopeSelectorV2`, missing-time rule, payload sensitivity, budget bound, unsupported blocking/output point, host-bundle/runtime/probe digest binding, capability disposition, and adapter descriptor uniqueness; include the independent ten-event Codex matrix, multi-repo/worktree, empty explicit selector, first-CWD, base-checkout/PR-worktree, and stale-registry cases. Fail if generated Codex coverage omits one required event or adds an invented event.
- [ ] Run `cargo test --test hooks_v2 -- --nocapture`. Expected: fail because the root V2 hook module/types do not exist.
- [ ] Implement the pure contracts and immutable adapter registry; generate JSON Schema fixtures and stable digests.
- [ ] Re-run. Expected: all tests pass and unknown forward host event maps to typed UnsupportedEvent without panic.
- [ ] Commit: feat(hooks): define bounded host hook contracts.

### Commit 2: Capture durability and hot-path runtime

**Files:** `src/v2/hooks/{runtime,backpressure,telemetry}.rs`; capture spool client companion; `tests/hooks_v2/{hot_path,durability_ack,backpressure}.rs`; `benches/{hooks_v2_notification,hooks_v2_prompt}.rs`.

- [ ] Add failing tests ack_never_overstates_durability, invocation_state_is_monotonic, unobservable_transport_ack_is_not_fabricated, canonical_event_never_coalesces, optional_sync_coalesces_after_representative, tier_hysteresis_prevents_flapping, sparse_source_is_not_starved, timeout_returns_silent_degradation, duplicate_returns_same_observation, fallback_index_recovers_fsynced_retry, corrupt_segment_never_claims_durable, and queue_budget_is_bounded.
- [ ] Run focused tests. Expected: fail because HookRuntime/capture client are absent.
- [ ] Implement Sections 7–10 using capture/application fakes; no production file I/O.
- [ ] Re-run tests and Criterion baselines. Expected: correctness passes; benchmark report includes corpus/host/hook/runtime/reference-machine IDs and meets Section 14.
- [ ] Commit: feat(hooks): add durable bounded hook runtime.

### Commit 3: Codex and Claude adapters

**Files:** `src/v2/hooks/adapters/{codex,claude}.rs`; `src/v2/hooks/render/{mod,codex,claude}.rs`; `tests/fixtures/hooks_v2/{codex,claude}/`; `tests/hooks_v2/{host_conformance,v1_differential,outcome_evidence}.rs`.

- [ ] Freeze redacted V1 fixtures and current stock Codex/Claude fixtures for every applicable generated entry point in Section 12. Codex covers all ten events and its complete §7.1/7.2 contract. Claude covers the independent 30-event oracle, exact common/event schemas and matchers, all five handler kinds, generated exec-form command subset, source/frontmatter/managed policy, parallel host dedupe, sync/async/rewake, exit/HTTP/MCP/prompt-agent result semantics, platform lowering, lag/spill/privacy, terminal/task/team/worktree/elicitation lifecycles, and every version gate; preserve unsupported historical entry points only as explicit absent/legacy evidence.
- [ ] Add adapter-declared steering-boundary fixtures for current-Turn native interrupt, after-tool/before-model, one-shot Stop/SubagentStop continuation, and next-Turn-only. Prove similarly named async/notification/prompt/agent facilities remain unsupported until capability-proven and no adapter interrupts an in-flight side-effecting tool.
- [ ] Run differential tests. Expected: fail before adapters exist.
- [ ] Implement mapping/render only; use catalog binding and application policy result.
- [ ] Re-run. Expected: normalized/request/reply parity passes or a fixture records an intentional versioned difference.
- [ ] Commit: feat(hooks): port Codex and Claude host adapters.

### Commit 4: Cursor, Hermes, Kiro, and MCP/daemon notification adapters

**Files:** `src/v2/hooks/adapters/{cursor,hermes,kiro}.rs`; `src/v2/hooks/render/{cursor,hermes,kiro}.rs`; `src/v2/hooks/conformance/*`; root internal-shadow adapters; `tests/fixtures/hooks_v2/{cursor,hermes,kiro}/`; `tests/hooks_v2/{host_conformance,v1_differential}.rs`.

- [ ] Add all Section 12 fixtures, linked-worktree/detached/moved/adopted-store cases, #410 prompt-origin cases, and Hermes CLI/gateway/background/delegation/task-worker/profile/scope lanes. Cursor and Hermes each receive steering goldens for native-boundary capability present/absent, post-tool/pre-model, next-Turn fallback, unsupported versus deferred truth, two-host duplicate claim, duplicate/stale acknowledgement, in-flight side-effect deferral, and bounded batch/Turn/rate/cooldown exhaustion. Gateway/notification/board delivery must never pass as model-visible Hermes steering.
- [ ] Run conformance/differential tests. Expected: fail before mappings exist.
- [ ] Implement adapters and async proposed effects; remove inline ingest/sync from new path.
- [ ] Re-run. Expected: every descriptor event has a fixture and no direct store/index/process call exists.
- [ ] Commit: feat(hooks): port remaining host event adapters.

### Commit 5: Concurrent-agent, crash, replay, and privacy harness

**Files:** `tests/hooks_v2/{concurrency_ordering,crash_recovery,hint_replay,privacy_security}.rs`; `benches/{hooks_v2_concurrent_agents,hooks_v2_host_render}.rs`.

- [ ] Add deterministic scheduler/load tests for 100 parent/subagents, presence/work-claim heartbeat/TTL, same/parallel-worktree overlap, planned redundancy, five coordination gates, one-hint/dedupe/cooldown/ack, required/advisory steering contention, duplicate delivery/reconnect/stale lease/late terminal, exactly one bounded continuation, duplicate/gap/late/rewrite, daemon loss, fallback segment collision, disk full, locked reader, bundle/catalog publication, kill points, exact/recorded/best-effort replay, delivery unknown, human correction, and secret corpus. Replay parent prefix `019f4906`, four PR #359 child agents, and Cursor session `ebc96a27-b046-4c88-865f-b38d76da9d2d` from the shared coordination manifest.
- [ ] Run tests. Expected: at least one ordering/durability/replay assertion fails before final recovery/reconciliation handling.
- [ ] Complete idempotent retry, fair writer scheduling contracts, delivery reconciliation, and no-write replay adapters.
- [ ] Re-run the root hook test target and four hook benches. Expected: all Section 14 gates pass.
- [ ] Commit: test(hooks): prove concurrent capture and replay safety.

### Commit 6: Shadow migration and cutover receipts

**Files:** `src/v2/hooks/conformance/differential.rs`; `src/hooks/v2_compat.rs`; integration manifests/config; compatibility tests/docs.

- [ ] Add shadow tests proving one host invocation yields one V1 effect owner, one non-effecting V2 evaluation, no double hint, comparable normalized/evaluation/reply digests, and explicit uncomparable coverage.
- [ ] Enable v2_hooks_shadow per host/hook point; collect 24-hour parity/latency/privacy/continuity report.
- [ ] Cut over one hook point at a time with profile/source freeze watermark, V1/V2 state digest, bundle/catalog/adapter versions, feature flag, and rollback procedure.
- [ ] Preserve V1 adapters only inside the bounded shadow/rollback harness and V1 evidence through the data rollback window. Once a hook point cuts over, stale installed hooks/daemons/plugins fail the exact protocol/catalog handshake with restart/reinstall/update guidance; they never execute a V1 fallback or old tool name.
- [ ] Commit: refactor(hooks): route host integrations through V2 runtime.

## 16. Cutover, Rollback, and Deletion Criteria

Cutover order: notification-only session/workspace -> post-tool/edit/shell -> prompt submit -> subagent/agent lifecycle -> compaction -> explicit pre-tool blocking. Each step requires:

- refreshed host manifest and accepted base;
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
- [ ] `cargo clippy -p tracedecay-domain -p tracedecay-capture -p tracedecay-policy -p tracedecay-tool-catalog -p tracedecay --all-targets -- -D warnings`. Expected: exit 0.
- [ ] Run the root hook unit/integration/property/conformance suites under all root features. Expected: all pass, none ignored.
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
- Active-attempt steering is bound to exact work item/attempt/lease/authority/fence/sequence/packet/graph revisions and actual host boundary receipts. Required unresolved/unknown delivery fences completion/integration; advisory does not; duplicate/reconnect delivery is logically exactly-once; unsupported mid-Turn delivery is truthfully next-Turn-only; Stop/SubagentStop continues at most once.
- Agent presence/claims remain current through bounded heartbeats; coordination hints occur only at five material workflow gates, are compact/advisory/planned-redundancy-aware, and cannot spam or mutate other agents.
- Every request and durable receipt binds the unchanged `ScopeSelectorV2` digest; multi-repo/project/checkout/worktree/ref/snapshot/generation ambiguity/staleness is explicit and hooks never infer current project/CWD/first CWD/base checkout/current graph.
- Missed Git/tool capability and human correction are observable first-class outcomes.
- #405/#407 identity/profile migration, #410 prompt origin, #411 remediation ownership, and #412 drain/shutdown semantics are preserved; #413 contributes actual protocol version only.
- Hooks contain no database, query, policy implementation, Git, network, process, or product UI logic.
- All hook contracts import the single plan-01 `tracedecay_domain::hooks::{binding,request,receipt}` family; no root adapter or companion crate defines a `hooks_v1` facade or duplicate provenance/request/receipt vocabulary.
- Every persisted/evaluated hook request and rendered hint uses the Plan 18 receipt/sink-eligible contract; raw provider wire exists only during bounded decode and cannot serialize through a hook-owned port.
- Shadow cutover and rollback have been proven separately for every host/hook point.
