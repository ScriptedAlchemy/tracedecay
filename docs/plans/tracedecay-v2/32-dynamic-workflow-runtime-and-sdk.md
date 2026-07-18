# PR17: Daemon-owned typed workflow runtime and contracts

**Status:** implementation authority for PR17.

## Decision

TraceDecay workflows compose existing typed application operations. The daemon validates
versioned definitions, owns runs, schedules steps, records effects, and exposes controls.
It is also the sole runtime that executes explicitly admitted
[Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md) task steps;
Plan 24 owns task/work graph state and semantics, while this plan owns runtime
scheduling and effects.

The end-to-end handoff is:

`Plan 24 planning request -> Plan 32 leased planner attempt -> normalized
proposal evidence -> Plan 24 proposal decision and accepted attempt set ->
Plan 32 bounded parallel attempts -> normalized attempt evidence -> optional
Plan 32 leased synthesis attempt -> Plan 24 acceptance/query projection`.

Plan 32 never turns planner or provider output directly into child work. A
planner may recommend tasks, routes, topology, and evidence requirements, but
only Plan 24 may assign task identity, create or accept proposals, derive
readiness, or publish an accepted attempt set. Plan 32 alone admits that set,
reserves capacity, issues and fences leases, starts providers, retries,
reconciles effects, and closes attempts. Runtime `Completed` is evidence for
Plan 24; it is never task acceptance or graph completion.

PR17 adds no JavaScript/TypeScript runtime, generated Claude workflow JavaScript,
Markdown parser, progress tracker, rewrite executor, taskgraph compiler, or shell command tape.
Plan files remain prose and are never executable workflow input.

## Definition contract

An immutable workflow definition version contains:

- stable definition/version identity, owner, explicit project/profile scope,
  input/output schema, and retention class;
- typed step IDs referencing cataloged application operation IDs;
- schema-validated literal inputs or typed references to prior step outputs;
- an optional exact Plan 24 work-item/version/readiness/acceptance binding when
  the step executes canonical product work;
- an optional exact Plan 24 auxiliary-attempt request reference whose provider
  recommendation, scope, context, grants, budgets, and fallback constraints
  must be revalidated before admission;
- explicit runtime predecessor edges, bounded fan-out groups, concurrency and
  failure policy, route/capability requirements, budgets, and runtime result
  conditions; and
- configuration/catalog/policy/privacy snapshots and a definition digest.

Definitions are data, not source code. Unknown operations, cycles, dangling references,
incompatible schemas, unbounded fan-out, privilege escalation, or
unsupported effects reject before activation. Editing creates a new version;
admitted runs stay pinned to their exact version and snapshots.

`RuntimePredecessor` and `RuntimeResultCondition` govern only release and
runtime closure inside the admitted definition. They cannot create a Plan 24
dependency, satisfy a Plan 24 acceptance contract, or close a work item.

Lifecycle is `Candidate -> Validated -> Active -> Retired | Rejected`. Names
are scoped aliases only; run admission resolves and records an exact version.
Files may be explicit import/export artifacts, but watchers never auto-import,
activate, or infer authority from CWD or nearest-directory precedence.

## Required implementation units

PR17 creates or modifies only the following runtime-owned units. Names in this
section are contract names, not examples:

- `crates/tracedecay-domain/src/workflow/definition.rs`:
  `WorkflowDefinitionV1`, `WorkflowStepV1`, `RuntimePredecessor`,
  `RuntimeResultCondition`, `FanoutGroupV1`;
- `crates/tracedecay-domain/src/workflow/control.rs`:
  `RunControlEnvelopeV1`, `MonotonicRunDeadline`,
  `RunCancellationV1`, `CancellationState`;
- `crates/tracedecay-domain/src/workflow/budget.rs`:
  `SharedEffectBudgetV1`, `BudgetLedgerV1`, `BudgetReservationV1`,
  `BudgetDimension`, `WorkflowStageKind`;
- `crates/tracedecay-domain/src/workflow/provider.rs`:
  `ProviderAdapterKind`, `ProviderNegotiationV1`,
  `ProviderExecutionEnvelopeV1`, `NativeLaunchPlanV1`,
  `ProviderEventEnvelopeV1`, `ProviderApprovalRequestV1`,
  `ProviderControlV1`, `ProviderRecoveryRequestV1`,
  `ProviderTerminalReceiptV1`, `ProviderOutcome`;
- `crates/tracedecay-domain/src/workflow/evidence.rs`:
  `NormalizedEvidenceEnvelopeV1`, `EvidencePacketSetV1`,
  `SynthesisInputV1`, `EvidenceCoverageV1`, `EvidenceUnknownV1`;
- `crates/tracedecay-domain/src/workflow/state.rs`:
  `WorkflowRunState`, `WorkflowAttemptState`, `EffectState`,
  `IdempotencyState`, `RetryDisposition`;
- `crates/tracedecay-store/src/workflow/events.rs`,
  `leases.rs`, `outbox.rs`, and `recovery.rs`: canonical append,
  fencing, idempotency, delivery, and restart transactions;
- `src/application/workflow/ports.rs`, `admission.rs`, `runtime.rs`,
  `recovery.rs`, and `queries.rs`: application commands, ports, and views;
- `src/workflow_runtime/kernel.rs`, `planner.rs`, `fanout.rs`,
  `synthesis.rs`, and `providers/{native_process,claude_code_cli,codex_app_server,codex_cli}.rs`:
  daemon implementations;
- `src/cli/workflow.rs`, `src/mcp/tools/handlers/workflow.rs`,
  `src/http/workflow.rs`, and `src/dashboard/workflow.rs`: thin surfaces over
  the application ports; and
- root `Cargo.toml`: register
  `[[test]] name = "workflow_runtime_suite"` with
  `path = "tests/workflow_runtime_suite/main.rs"`;
- `crates/tracedecay-domain/tests/workflow_runtime_contract.rs`,
  `crates/tracedecay-store/tests/workflow_runtime_contract.rs`, and
  `tests/workflow_runtime_suite/{main,shared_budget,capability_manifest,deterministic_fallback,parallelism,evidence_handoff,model_routing,native_providers,no_recursive_dispatch,retry_recovery,runtime_metrics,backup_restore,remote_fencing,surface_parity}.rs`:
  the binding contract and integration suites.

Plan 24 owns its planning request, proposal, query, accepted-attempt-set, and
task-evidence-contract types. PR17 consumes those types by exact versioned
reference and does not duplicate them under `workflow`.

## Runtime clock, run, and effect authority

Runs use one daemon
runtime-clock/scheduler/history/lease/attempt/effect/artifact kernel shared
with automations. Typed workflow application operations invoke it directly;
API, CLI, MCP, dashboard, and host bindings contain no private readiness,
dispatch, retry, completion, effect, or artifact logic. There is no workflow
database, journal, clock/timer, scheduler, lease family, retry loop, or worker
authority outside this shared kernel, and Plan 24 defines no competing task
scheduler, clock, lease table, attempt runtime, effect journal, or worker
authority.

Canonical run history records admission, step readiness, attempt dispatch,
delivery/effect observation, validated result, retry decision, cancellation,
checkpoint, and terminal receipt. A step becomes ready only from committed
history. Admission plus outbox, result plus transition, and terminal closure are
atomic owner-shard transactions.

Every effect has stable run/step/attempt/idempotency identity. Idempotent effects
may resume after restart; at-least-once and non-repeatable adapters follow their
declared reconciliation rules. Sent-without-receipt becomes `EffectUnknown` and
blocks automatic retry and successful completion. A replacement attempt is
legal only after the daemon proves the previous effect absent or safely
repeatable.

Pause and cancellation fence new admissions, reconcile in-flight effects, and
then publish a stable state. Cancellation never rewrites completed history.
Retries retain prior evidence and remain bounded by attempt, time, token, cost,
output, and concurrency budgets. Restart rebuilds readiness from canonical
history and cannot duplicate a committed observable effect.

## One run-control envelope and effect budget

Admission creates exactly one durable `RunControlAggregateV1`. Its admitted
limits and snapshots are immutable; authority, cancellation, deadline
checkpoint, and ledger are monotonically versioned state. Planner, operation,
fan-out, provider, retry, recovery, control, and optional synthesis stages
receive a read-only `RunControlEnvelopeV1` view of that aggregate:

```rust
pub struct RunControlAggregateV1 {
    pub run_id: WorkflowRunId,
    pub limits: RunLimitSnapshotV1,
    pub snapshots: RunSnapshotSetV1,
    pub authority: VersionedAuthorityV1,
    pub cancellation: RunCancellationV1,
    pub deadline: MonotonicRunDeadline,
    pub ledger: BudgetLedgerV1,
}

pub struct RunControlEnvelopeV1 {
    pub run_id: WorkflowRunId,
    pub authority_epoch: AuthorityEpoch,
    pub deadline: MonotonicRunDeadline,
    pub cancellation: RunCancellationV1,
    pub budget: SharedEffectBudgetV1,
    pub capabilities: CapabilityManifestSnapshotV1,
    pub routing: ModelRoutingPolicyV1,
}

pub struct MonotonicRunDeadline {
    pub budget: Duration,
    pub remaining_at_checkpoint: Duration,
    pub checkpoint_boot_id: BootId,
    pub checkpoint_monotonic_tick: u64,
    pub checkpoint_utc: Timestamp,
    pub not_after_utc: Timestamp,
}

pub struct SharedEffectBudgetV1 {
    pub max_attempts: u32,
    pub max_provider_calls: u32,
    pub max_effects: u32,
    pub max_parallel_effects: u32,
    pub max_tokens: u64,
    pub max_cost_micros: u64,
    pub max_output_bytes: u64,
    pub max_artifact_bytes: u64,
}

pub enum WorkflowStageKind {
    Planner,
    Operation,
    Fanout,
    Provider,
    Synthesis,
    Retry,
    Recovery,
    Control,
}
```

Admission intersects the Plan 24 request, workflow definition, Plan 06 route,
and Plan 20 provider limits dimension by dimension into one
`RunLimitSnapshotV1`; no stage carries an independent replenishable budget.
Attempts, calls, effects, tokens, cost, output, and artifacts are cumulative.
Parallel effects are a released concurrency gauge, not a consumed counter.

`MonotonicRunDeadline::remaining(now)` subtracts monotonic elapsed time when
`checkpoint_boot_id` matches and otherwise subtracts wall elapsed from the
persisted checkpoint, clamped by `not_after_utc`. Clock rollback, missing boot
identity, or ambiguous elapsed time fails to zero. Remaining time never
increases after pause, human decision time, retry, reconnect, failover, or
daemon restart. An in-run human override may shorten limits or cancel;
extending a deadline or replenishing a consumed dimension requires cancel and
re-admit as a new run linked to the old receipt.

Every stage reserves its upper bound from one persisted `BudgetLedgerV1`
before work. Reservation, idempotency claim, lease/effect record, and outbox
publication are one owner-shard transaction. Settlement returns only proved
unused capacity; retry never refunds consumed effects, calls, tokens, cost, or
time. Exhaustion produces `BudgetExhausted { dimension, stage }`, fences new
reservations, preserves evidence already committed, and either proceeds to a
declared evidence-preserving fallback or terminates explicitly.

The application kernel exposes these exact internal ports:

```rust
pub trait WorkflowRuntimeKernel {
    async fn admit_run(
        &self,
        command: AdmitWorkflowRunV1,
    ) -> Result<RunAdmissionReceiptV1, AdmissionError>;
    async fn apply_plan24_decision(
        &self,
        command: ApplyPlan24DecisionV1,
    ) -> Result<Plan24DecisionApplicationReceiptV1, TransitionError>;
    async fn reserve_effect(
        &self,
        command: ReserveStageEffectV1,
    ) -> Result<EffectPermitV1, ReservationError>;
    async fn record_progress(
        &self,
        command: RecordAttemptProgressV1,
    ) -> Result<ProgressReceiptV1, TransitionError>;
    async fn settle_effect(
        &self,
        command: SettleEffectV1,
    ) -> Result<EffectSettlementReceiptV1, TransitionError>;
    async fn finish_attempt(
        &self,
        command: FinishAttemptV1,
    ) -> Result<AttemptTerminalReceiptV1, TransitionError>;
    async fn request_cancellation(
        &self,
        command: RequestRunCancellationV1,
    ) -> Result<CancellationReceiptV1, TransitionError>;
    async fn pause_run(
        &self,
        command: PauseWorkflowRunV1,
    ) -> Result<RunControlReceiptV1, TransitionError>;
    async fn resume_run(
        &self,
        command: ResumeWorkflowRunV1,
    ) -> Result<RunControlReceiptV1, TransitionError>;
    async fn retry_attempt(
        &self,
        command: RetryWorkflowAttemptV1,
    ) -> Result<RetryReceiptV1, TransitionError>;
    async fn respond_to_provider_approval(
        &self,
        command: RespondToProviderApprovalV1,
    ) -> Result<ProviderApprovalReceiptV1, TransitionError>;
    async fn reconcile_effect(
        &self,
        command: ReconcileEffectV1,
    ) -> Result<ReconciliationReceiptV1, TransitionError>;
}
```

Every mutating command carries run/node/attempt identity as applicable,
expected authority epoch, expected cancellation generation, deadline identity,
idempotency key, actor, and reason. No surface may call a provider adapter
directly. Every adapter method capable of process, protocol, network, or OS I/O
requires an `EffectPermitV1`; only pure `prepare_launch` lowering may run
without one.

## Capability manifest and planner handoff

Admission constructs one immutable `CapabilityManifestSnapshotV1` from the
Plan 08 operation/provider descriptors, the complete Plan 20 configuration
snapshot, Plan 27 observations, and the effective privacy/policy revisions:

```rust
pub struct CapabilityManifestSnapshotV1 {
    pub revision: CapabilityManifestRevision,
    pub digest: Digest,
    pub generated_at: Timestamp,
    pub catalog_revision: CatalogRevision,
    pub configuration_revision: ConfigurationRevision,
    pub observation_frontier: ObservationFrontier,
    pub privacy_revision: PrivacyRevision,
    pub operations: Vec<OperationCapabilityV1>,
    pub providers: Vec<ProviderCapabilityV1>,
    pub permitted_roles: Vec<AuxiliaryRole>,
    pub effective_parallelism: NonZeroU32,
}

pub struct PlannerExecutionInputV1 {
    pub control: RunControlEnvelopeV1,
    pub capabilities: CapabilityManifestSnapshotV1,
    pub planning_request: Plan24PlanningRequestRef,
    pub context: AuthorizedContextManifest,
}
```

This is an input snapshot, not a registry. The planner cannot discover ambient
executables, reread live settings, create task IDs, accept proposals, or submit
children. Its terminal `NormalizedEvidenceEnvelopeV1` contains a typed proposal
payload reference. Plan 24 validates that payload under its own semantics and
returns either an exact `AcceptedAttemptSetRef` or a rejected, expired, or
superseded decision. The run remains `AwaitingPlanDecision` meanwhile; its
deadline continues decreasing and its cancellation token remains live.

Only `ApplyPlan24DecisionV1` may leave `AwaitingPlanDecision`. It carries
expected run version and authority epoch, planning request, actor, reason,
idempotency key, and one tagged payload:

```rust
pub enum Plan24DecisionPayloadV1 {
    Accepted {
        decision: Plan24ProposalDecisionRef,
        accepted_attempt_set: AcceptedAttemptSetRef,
        readiness_digest: Digest,
        manifest_digest: Digest,
    },
    Rejected { decision: Plan24ProposalDecisionRef },
    Expired { decision: Plan24ProposalDecisionRef },
    Superseded { decision: Plan24ProposalDecisionRef },
}
```

`Accepted` transitions to `Queued`; `Rejected` transitions to `Failed`,
`Expired` to `TimedOut`, and `Superseded` to `Cancelled`, without creating a
fan-out lease or attempt. Every downstream attempt must have either a separate
Plan 24 accepted-attempt decision or an explicit contingent-release
authorization in the accepted set; runtime predecessor satisfaction alone
cannot release new canonical work.

Fan-out admission requires the exact accepted set version, readiness digest,
proposal decision, and manifest digest. Plan 32 revalidates them immediately
before the first lease. Stale or changed capability evidence produces
`AdmissionRejected::CapabilityManifestStale` before capacity reservation or
process startup. Plan 32 never recomputes Plan 24 readiness or acceptance.

## Bounded fan-out, backpressure, and no-progress timeout

```rust
pub struct ConcurrencyPolicyV1 {
    pub run_limit: NonZeroU32,
    pub group_limit: NonZeroU32,
    pub provider_limit: NonZeroU32,
    pub capacity_class: CapacityClassKey,
    pub max_queue_depth: u32,
    pub no_progress_timeout: Duration,
}
```

Effective concurrency is the minimum of the run, accepted fan-out group,
provider, capacity-class, capability-manifest, and remaining-budget limits.
Queues are persisted and bounded. Queue overflow returns
`Deferred::Backpressure` or the definition's explicit rejection outcome;
workers wake on committed capacity release, cancellation, or deadline, never
an unbounded polling loop. Fairness is deterministic within a capacity class
by `(admitted_at, run_id, node_id)`.

Heartbeat updates liveness only. `ProgressFrontier` advances only for a newly
committed typed provider event, effect settlement, artifact, or terminal
receipt with a strictly greater sequence/frontier. Only that advancement
resets `no_progress_timeout`. Expiry starts cancellation escalation and closes
as `TimedOut { reason: NoProgress }` unless effect reconciliation requires
`EffectUnknown`.

Fan-out emits one independently sealed evidence envelope per attempt. Failure
policy is one of `FailFast`, `CollectWithinBudget`, or
`RequireAtLeast { successes }`; cancellation of siblings follows that pinned
policy and never discards already sealed envelopes.

## Model routing and deterministic fallback

Plan 24 owns the task-domain recommendation. Plan 06 evaluates policy. Plan 32
consumes their pinned decision and performs only capability negotiation and
execution:

```rust
pub struct ModelRoutingPolicyV1 {
    pub requested: RouteSelectionV1,
    pub ordered_fallbacks: Vec<RouteSelectionV1>,
    pub deterministic_fallback: DeterministicFallbackAction,
    pub decision_revision: PolicyDecisionRevision,
    pub human_override: Option<HumanRouteOverrideV1>,
}

pub enum DeterministicFallbackAction {
    RunOperation { operation_id: OperationId },
    SequentializeAcceptedAttempts,
    PreserveOrderedEvidenceWithoutSynthesis,
    Abstain { reason: FallbackReason },
}

pub enum RouteResolution {
    Primary(RouteSelectionV1),
    ExplicitFallback(RouteSelectionV1),
    DeterministicNonLlm(DeterministicFallbackAction),
    Deferred(DeferralReason),
    Rejected(RouteRejectionReason),
}
```

Resolution order is exact primary, explicit fallbacks in stored order,
declared deterministic non-LLM action, then fail closed. Plan 32 does not
rerank models, infer a provider from a model string, or invent fallback.
`RunOperation` is admissible only when the pinned capability manifest marks
the exact operation version non-LLM, deterministic, and side-effect-free or
idempotently reconciled. Deterministic operations use canonical serialized
inputs and must produce byte-identical payload bytes and `payload_digest` for
the same definition, snapshots, and input digest; run/attempt identities,
timestamps, frontiers, and accounting metadata are outside those payload
bytes.
Evidence without synthesis is ordered by `(fanout_group_id, node_id,
attempt_id)` and preserves failures, unknowns, disagreement, and minority
evidence.

A human route override is a version-checked Plan 09 command with actor, reason,
allowed route, expected run version, and expected policy revision. It can
select only a manifest-eligible route and cannot broaden grants, egress,
budget, or deadline. It is pinned before attempt creation and cannot replace an
adapter after startup.

## Typed auxiliary provider adapters

Plan 32 owns the provider-adapter execution contract and the only runtime
dispatcher for auxiliary attempts. It first revalidates the pinned Plan 24
request and Plan 06 decision against one complete pinned Plan 20 auxiliary-
provider configuration snapshot, then acquires a fenced lease and creates an
attempt before invoking any provider. Discovery or process startup before
lease authority is forbidden.

Plan 08 owns the catalog descriptor schema. Plan 20 alone owns executable
references, allowed ranges, defaults, disclosure/sandbox policy, lifecycle
bounds, resume policy, and fallback configuration. Plan 27 discovers and
probes the configured executables and supplies host conformance evidence
without resolving settings. Plan 32 consumes the Plan 08 descriptor, Plan 27
observations, and one Plan 20 snapshot, owns live negotiation, and never writes
a second catalog, configuration source, or host registry. Every adapter exposes
one typed descriptor and negotiation result covering:

- backend identity and kind, executable identity/path source, executable and
  protocol version, build/revision, availability freshness, and supported
  operating systems;
- supported model/version and reasoning-effort selectors, context/input limits,
  tool/event/artifact capabilities, sandbox and approval modes, network/egress
  controls, cancellation, progress/heartbeat, reconnect/resume, and structured
  protocol features;
- the exact catalog/configuration/privacy revisions and probe evidence used for
  negotiation; and
- an explicit `Supported`, `Unsupported`, `Absent`, `Stale`, or `Failed`
  negotiation outcome with reason and coverage. Capability absence never
  triggers an implicit provider or protocol fallback.

The admitted execution envelope pins:

- project, repository, checkout/worktree generation, branch/ref/commit, code
  generation, work-plan/item/version, parent task/attempt/Session/Turn,
  run/node/lease/attempt, actor, and authority-epoch identities;
- a bounded authorized retrieval-context manifest and resolved payload handles,
  not global task state or direct store access;
- executable identity, an argument vector, and bounded stdin or framed protocol
  input. Adapters never accept a shell command string, interpolation template,
  shell redirection, or ambient command fragment;
- exact requested provider backend, model/version, reasoning effort,
  sandbox/approval mode, capability grants, working directory, deadline,
  cancellation token, budgets, and expected outputs;
- the complete Plan 20 auxiliary-provider configuration revision/digest used
  for executable, version range, sandbox/environment disclosure, defaults,
  deadline/cancellation/kill, reconnect/resume, capacity, and explicit
  fallback decisions;
- an environment allowlist plus opaque secret references resolved just in time
  through the existing secret boundary. Unlisted inherited environment,
  credential values, prompts, and private context never enter events, logs,
  receipts, or process diagnostics; and
- expected event schema, progress/heartbeat cadence, output/artifact limits,
  terminal receipt schema, and effect/reconciliation class.

The native Claude adapter executes the supported Claude Code CLI for
Claude-designated work. Hermes Anthropic is not a Claude execution backend and
cannot satisfy that route. The Codex app-server adapter is preferred for
Codex-designated work because it provides structured session/event/control
semantics. A distinct Codex CLI adapter may be selected only when app-server is
unsupported or unavailable and the pinned policy/configuration explicitly
allows that fallback. Here `configuration` means the one pinned Plan 20
snapshot; adapters and Plan 27 cannot supply a local default. The runtime
records requested and actual adapter,
executable/protocol/model versions, the fallback decision, and its reason;
neither adapter silently invokes the other.

Claude Code CLI means spawning the configured native executable and consuming
its documented versioned CLI protocol. Anthropic APIs, in-process SDK clients,
and Hermes translation are not conforming implementations. Codex app-server
means launching or connecting to the configured native app-server executable
and speaking its negotiated protocol. A direct OpenAI API or in-process client
is not a substitute.

Application callers provide typed provider fields, never arbitrary argv. The
selected adapter deterministically lowers them into `NativeLaunchPlanV1`;
admission pins the executable, argv, framed input, environment allowlist, and
digest before process startup. The daemon-internal `NativeProviderAdapter`
port is:

```rust
pub trait NativeProviderAdapter {
    async fn negotiate(
        &self,
        permit: EffectPermitV1,
        request: ProviderNegotiationRequestV1,
    ) -> Result<ProviderNegotiationV1, ProviderError>;
    async fn prepare_launch(
        &self,
        envelope: ProviderExecutionEnvelopeV1,
    ) -> Result<NativeLaunchPlanV1, ProviderError>;
    async fn start(
        &self,
        permit: EffectPermitV1,
        launch: NativeLaunchPlanV1,
        events: BoundedProviderEventSink,
    ) -> Result<ProviderSessionV1, ProviderError>;
    async fn control(
        &self,
        permit: EffectPermitV1,
        command: ProviderControlV1,
    ) -> Result<ProviderControlReceiptV1, ProviderError>;
    async fn recover(
        &self,
        permit: EffectPermitV1,
        request: ProviderRecoveryRequestV1,
    ) -> Result<ProviderRecoveryReceiptV1, ProviderError>;
    async fn reconcile(
        &self,
        permit: EffectPermitV1,
        request: ProviderReconciliationRequestV1,
    ) -> Result<ProviderReconciliationReceiptV1, ProviderError>;
}
```

There is deliberately no `execute(prompt) -> String` provider abstraction.
Adapters are selected through the closed `ProviderAdapterDispatch` enum, not a
runtime `dyn` trait object; implementations are `Send + Sync`, and the
repository's async-trait convention must make the port compile on supported
toolchains. `BoundedProviderEventSink` applies bounded backpressure and writes
terminal/control events through a non-droppable durable lane.

Candidate order is resolved from pinned snapshots before attempt creation.
Each candidate receives a distinct attempt, lease, and permit. A pre-session
`Unsupported`, `Absent`, or `Stale` result may yield
`RetryDisposition::RetryExplicitFallback` and a new attempt for the next
stored candidate. Failure after provider-session creation terminates or
reconciles that attempt and never switches adapters in place.

Adapters ingest bounded, ordered stdout, stderr, and native protocol events as
separate typed channels with sequence identity, timestamps, truncation/drop
coverage, and safe redaction. Structured native events are authoritative only
for what their protocol proves. Free-form stdout/stderr is evidence, never a
graph mutation or successful terminal receipt. Malformed frames, schema/version
drift, out-of-order terminal events, oversized output, stream loss, and
unexpected process exit produce explicit `Failed` or `Partial` outcomes with
retained safe diagnostics; they never fall back to text scraping as success.

Progress and heartbeat events update only Plan 32 attempt liveness/history.
They do not renew authority after lease loss or prove task completion.
Deadline/cancellation propagates through protocol-native cancellation first,
then bounded interrupt/terminate and kill escalation for the owned process
group where supported. Each stage is timed and recorded. Failure to prove
termination yields an unknown-effect/partial state that blocks replacement or
success until reconciled.

Artifacts are accepted only through declared bounded channels, content/type
validation, privacy policy, and attempt identity. A terminal receipt records
provider/backend/executable/protocol/model identity, requested versus actual
selection, start/end/exit/cancellation state, stream coverage, progress
frontier, artifacts, token/cost evidence, and one exhaustive outcome:
`Completed`, `Unsupported`, `Absent`, `Stale`, `Cancelled`, `TimedOut`,
`Failed`, or `Partial`.

Resume/reconnect is capability-specific and never inferred. A reconnectable
app-server session resumes only from a pinned provider session/frontier and
matching attempt/lease authority. A CLI process without a proven resume
protocol restarts only as a new attempt after reconciliation. Daemon restart
rebuilds adapter state from canonical history, verifies process/session
identity and lease authority, reconnects when proved safe, otherwise
cancels/fences or marks the attempt partial/unknown. It never adopts an ambient
process by PID alone or replays stdin/effects speculatively.

Plan 32 publishes typed lease/attempt/provider liveness, progress, deadline,
cancellation/kill, restart/reconnect/resume, unknown-effect, and terminal
evidence to the Plan 14 Doctor kernel. It does not define provider health
severity, finding identity, diagnosis, or remediation presentation. Doctor may
invoke a separately authorized Plan 32 control, but cannot repair, reclaim, or
cancel runtime state by inference.

Auxiliary execution envelopes omit task-dispatch, graph-write, runtime-control,
lease-minting, and provider-selection capabilities. Provider output requesting
another agent is ordinary evidence and cannot recursively dispatch. Only Plan
09 may submit another human-authorized Plan 24 request to this runtime after a
new graph/proposal decision.

Every provider session is classified before start as `Observational`,
`InterceptedEffects`, or `CompoundNonRepeatable`. An observational session has
no write, shell, network, or daemon-control grants. An intercepted session
stops each native tool/effect request until the kernel reserves a distinct
effect identity and permit, then records its receipt. A compound non-repeatable
session is one compound effect with one permit and one receipt, runs inside
bounded sandbox/network/filesystem authority fixed by that permit, and cannot
claim per-tool reconciliation. Any session requiring shell, network, or
multiple independently writable effects must use `InterceptedEffects`; if the
provider cannot intercept them, admission rejects. Loss of a compound
terminal receipt becomes `EffectUnknown` and is never retried automatically.

The auxiliary-attempt principal is denied Plan 24 proposal/acceptance and Plan
32 admission/control operations at HTTP, MCP, CLI, and local-socket
authorization boundaries and receives no ambient daemon credential. This
principal-level denial, not prompt text or capability omission alone, enforces
no recursive dispatch.

Adapters must negotiate and verify provider-native child, subagent, and
delegation capabilities as disabled for auxiliary attempts. If the native
provider cannot prove disablement, negotiation returns `Unsupported`. Child
activity may be retained only by observation imports; it cannot create a
canonical request, proposal, run, lease, attempt, or graph mutation.

A native approval request is `ProviderApprovalRequestV1`, bound to exact
run/node/attempt/lease, native request ID, grants, deadline, cancellation
generation, and event sequence. A human response is a version-checked
`ProviderControlV1::Approve | Deny` that cannot broaden admitted grants.
Timeout denies or cancels; it never approves.

## Normalized evidence and optional synthesis

All operation and provider adapters terminate through the same envelope:

```rust
pub struct NormalizedEvidenceEnvelopeV1 {
    pub envelope_id: EvidenceEnvelopeId,
    pub run_id: WorkflowRunId,
    pub node_id: WorkflowNodeId,
    pub attempt_id: WorkflowAttemptId,
    pub lease: LeaseFence,
    pub stage: WorkflowStageKind,
    pub producer: EvidenceProducerV1,
    pub requested_route: Option<RouteSelectionV1>,
    pub actual_route: Option<RouteSelectionV1>,
    pub source_frontier: ProgressFrontier,
    pub terminal: AttemptTerminalReceiptV1,
    pub observations: Vec<TypedObservationRef>,
    pub artifacts: Vec<AuthorizedArtifactRef>,
    pub coverage: EvidenceCoverageV1,
    pub unknowns: Vec<EvidenceUnknownV1>,
    pub disagreements: Vec<EvidenceDisagreementV1>,
    pub budget_usage: BudgetUsageV1,
    pub typed_payload: TypedEvidencePayloadRefV1,
    pub source_packet_set: Option<EvidencePacketSetRef>,
    pub cited_envelopes: Vec<EvidenceEnvelopeRef>,
    pub payload_digest: Digest,
}

pub struct EvidencePacketSetV1 {
    pub run_id: WorkflowRunId,
    pub manifest_digest: Digest,
    pub ordered_envelopes: Vec<EvidenceEnvelopeRef>,
    pub set_digest: Digest,
}

pub struct TypedEvidencePayloadRefV1 {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub payload: AuthorizedPayloadRef,
    pub payload_digest: Digest,
}

pub enum EvidencePublicationState {
    Building,
    Sealed,
    Published,
    Acknowledged,
}
```

Sealing validates identity, lease fence, terminal frontier, schemas, artifact
authorization, redaction, and digest. Publication is an atomic
`Building -> Sealed -> Published` transition with the outbox record in the
same owner-shard transaction. Delivery acknowledgement is idempotent and does
not make the packet authoritative for Plan 24 acceptance.

Optional synthesis is a normal `WorkflowStageKind::Synthesis` attempt with its
own lease and reservation from the same deadline, cancellation token, and
budget ledger. `SynthesisInputV1` contains only the ordered immutable envelope
references, authorized payload handles, requested output schema, and an
instruction to preserve disagreement. A synthesis envelope must cite every
source envelope through `source_packet_set`, its set digest, and
`cited_envelopes`; source envelopes remain immutable and queryable. Synthesis
failure, timeout, absence, or budget exhaustion returns the unsynthesized
`EvidencePacketSetV1`; it never deletes, rewrites, or hides source envelopes.
Plan 24 alone evaluates source and synthesis envelopes against its task
evidence and acceptance contracts.

## Exact run, attempt, effect, retry, and recovery states

```rust
pub enum WorkflowRunState {
    Admitted,
    Planning,
    AwaitingPlanDecision,
    Queued,
    Executing,
    Synthesizing,
    Pausing,
    Paused,
    Cancelling,
    Reconciling,
    Completed,
    Partial,
    Failed,
    Cancelled,
    TimedOut,
    EffectUnknown,
}

pub enum WorkflowAttemptState {
    Reserved,
    Leased,
    Starting,
    Running,
    AwaitingDecision,
    Cancelling,
    Terminating,
    Killing,
    Reconciling,
    Completed,
    Cancelled,
    TimedOut,
    Failed,
    Partial,
    EffectUnknown,
}

pub enum EffectState {
    Planned,
    Dispatched,
    Observed,
    Unknown,
    ReconciledApplied,
    ReconciledAbsent,
    ReconciledRepeatable,
}

pub enum IdempotencyState {
    Reserved,
    InFlight,
    Committed,
    ReconciledAbsent,
    ReconciledRepeatable,
    Unknown,
}

pub enum RetryDisposition {
    RetrySameRoute,
    RetryExplicitFallback,
    Reconnect,
    ReconcileFirst,
    Stop,
}

pub enum DecisionWaitKind {
    NativeApproval {
        request_id: NativeApprovalRequestId,
        timeout_action: ApprovalTimeoutAction,
    },
    Plan24Escalation {
        proposal_id: Plan24ProposalId,
    },
}

pub enum ApprovalTimeoutAction {
    DenyRequest,
    CancelAttempt,
}
```

The legal attempt path is `Reserved -> Leased -> Starting -> Running`.
`Running` reaches `Completed` only after a validated terminal receipt and
settled effects. Any active state may enter `Cancelling`; cancellation follows
`Cancelling -> Terminating -> Killing -> Reconciling` until termination and
effects are proved. Sent-without-receipt is `EffectState::Unknown`,
`IdempotencyState::Unknown`, and `WorkflowAttemptState::EffectUnknown`; it
blocks retry, replacement, synthesis success, and run success.

`Running -> AwaitingDecision` records one `DecisionWaitKind`.
`NativeApproval` returns to `Running` only after an exact approve/deny receipt;
its pinned timeout action is deterministic. `Plan24Escalation` returns only
after a version-checked Plan 24 decision command. Run deadline wins over
approval and no-progress deadlines; explicit cancellation wins over every
other nonterminal transition. A run becomes `Paused` only when new effects are
fenced and every active attempt is terminal, reconciled, or provider-proven
suspended.

`RetryPolicyV1::decide(input)` is a total pure function over the pinned policy,
attempt ordinal, provider outcome and phase, effect/idempotency state, route
candidate index, reconnect proof, and remaining deadline/ledger. It returns
the first matching disposition in this exact order:

1. unknown, dispatched-without-receipt, or unproved termination:
   `ReconcileFirst`;
2. exact safe reconnect proof under the same attempt: `Reconnect`;
3. pre-session `Unsupported | Absent | Stale` with a remaining ordered route:
   `RetryExplicitFallback`;
4. retryable `TimedOut | Failed | Partial`, repeatable/reconciled effects, and
   a permitted same-route attempt: `RetrySameRoute`;
5. the same retryable outcomes with no same-route attempt but a remaining
   ordered route: `RetryExplicitFallback`; and
6. every other input, including `Completed` and `Cancelled`: `Stop`.

The selected disposition is committed with its complete input digest before
creating a retry. Deadline or ledger insufficiency makes rules 3 through 5
ineligible and therefore yields `Stop`; no later component reinterprets it.

`run retry` is legal only before terminal run closure. A terminal run never
reopens; later work is a linked re-admission with a new run ID.

Retry always creates a new `WorkflowAttemptId`, preserves the predecessor and
evidence, and consumes the original run deadline and ledger. Replacement is
legal only from `ReconciledAbsent` or `ReconciledRepeatable`. Reconnect
preserves an attempt ID only when provider session ID, progress frontier,
lease fence, cancellation generation, and authority epoch all match. A stale
or fenced late receipt is retained as non-authoritative evidence and cannot
settle an effect, publish an envelope, or advance a run.

Idempotency is exact:

- same key plus same canonical command digest returns the original receipt;
- same key plus different digest returns `IdempotencyConflict`;
- Plan-24-bound run admission is unique by
  `(Plan24RequestId, request_version)`;
- generic definition run admission is unique by
  `(WorkflowDefinitionId, definition_version, idempotency_key)`;
- provider events deduplicate by `(attempt_id, channel, sequence)`;
- effect settlement deduplicates by `(effect_id, provider_receipt_id)`;
- evidence publication deduplicates by
  `(attempt_id, terminal_frontier, payload_digest)`; and
- delivery acknowledgement deduplicates by `(envelope_id, consumer_id)`.

Restart order is fixed: rebuild canonical history and budget consumption;
increment and persist the authority epoch; fence old leases; replay only
committed outbox records; reconcile dispatched or unknown effects; reconnect
only exact matching provider sessions; close or mark unresolved attempts; seal
any proved terminal envelope; then release newly ready runtime predecessors.
A reconnect first proves the old provider session, frontier, attempt, and lease,
then obtains a new recovery lease and requires protocol-level acknowledgement
that the addressed session is rebound to the new authority epoch and fence.
Without that acknowledgement the attempt becomes `Partial` or `EffectUnknown`.
A daemon connected to a shared app-server may cancel only its addressed
session; it never signals or kills the shared server process. Recovery never
reruns Plan 24 planning semantics and never infers a task transition.

## Application and surfaces

Typed application use cases cover definition list/get/create-version/validate/
activate/retire/diff and run list/get/start/pause/resume/cancel/retry/status/
history. Mutations use expected version, authority epoch, actor, reason,
idempotency key, and typed receipts. Protected inputs, outputs, transcripts, and
artifacts resolve through existing authorized payload routes.

PR17 ships internal typed domain/application contracts plus the then-supported
HTTP, CLI, MCP, and dashboard bindings. CLI provides
`tracedecay workflow definition ...` and
`tracedecay workflow run ...` commands with Markdown default and typed JSON.
MCP stays compact: run, inspect, and control tools plus paged resources. No MCP
client executes or schedules locally.

The exact PR17 CLI command set is:

```text
tracedecay workflow definition list|show|create-version|validate|activate|retire|diff
tracedecay workflow run start|admit-work|apply-plan-decision|inspect|history|pause|resume|cancel|retry|reconcile
tracedecay workflow run approval approve|deny
```

`admit-work` requires `--request-id`, `--request-version`,
`--readiness-digest`, `--manifest-digest`, and `--idempotency-key`. Mutating
run controls require `--run-id`, `--expected-version`,
`--expected-authority-epoch`, `--reason`, and `--idempotency-key`. Approval
also requires `--attempt-id`, `--native-request-id`, and
`--expected-cancellation-generation`. JSON output is the corresponding typed
receipt, not a surface-specific shape.

`apply-plan-decision` requires `--planning-request-id`,
`--proposal-decision-id`, and
`--decision-kind accepted|rejected|expired|superseded`. The `accepted` variant
also requires `--accepted-attempt-set-id`,
`--accepted-attempt-set-version`, `--readiness-digest`, and
`--manifest-digest`; non-accepted variants reject those flags. All references,
the variant tag, and their canonical digest enter the idempotency command
digest.

HTTP handlers call the same application commands at
`POST /v1/workflow/definitions:validate`,
`POST /v1/workflow/runs`, `POST /v1/workflow/runs:admit-work`,
`POST /v1/workflow/runs/{run_id}:apply-plan-decision`,
`GET /v1/workflow/runs/{run_id}`,
`GET /v1/workflow/runs/{run_id}/history`, and
`POST /v1/workflow/runs/{run_id}:{pause|resume|cancel|retry|reconcile}`.
MCP exposes compact `workflow_run`, `workflow_inspect`, and
`workflow_control` tools plus paged definition/history/evidence resources.
These are transports over `src/application/workflow`; they do not call
`src/workflow_runtime/providers` directly.

[Plan 17](17-official-public-api-and-sdks.md) exclusively owns PR18 public
contract stabilization, schema/OpenAPI publication, generated or handwritten
Rust/TypeScript/Python clients, SDK documentation, and SDK conformance/parity.
PR17 may expose typed HTTP handlers used by that later publication, but it does
not generate, publish, or gate on an SDK.

The dashboard shows definitions, versions, dependency graph, run timeline,
step/attempt state, inputs/outputs, executor/model route, queue/latency,
tokens/cost, effects, retries, cancellation, coverage, and legal controls from
daemon application views. Plan 24's Work projections join these runtime views
by exact versioned references. Browser code never computes readiness,
completion, assignment quality, or route policy.

## Metrics ownership and emitted source events

Plan 26 owns metric semantics, retention, privacy, dashboards, and alerting.
Plan 32 emits bounded typed source events sufficient to derive:

- `tracedecay_workflow_runs_total{outcome}`;
- `tracedecay_workflow_run_duration_seconds`;
- `tracedecay_workflow_budget_exhaustions_total{dimension,stage}`;
- `tracedecay_workflow_effect_reservations_total{stage,outcome}`;
- `tracedecay_workflow_inflight_effects{stage,capacity_class}`;
- `tracedecay_workflow_queue_depth{capacity_class}`;
- `tracedecay_workflow_queue_wait_seconds{capacity_class}`;
- `tracedecay_workflow_backpressure_deferrals_total{reason,capacity_class}`;
- `tracedecay_workflow_no_progress_timeouts_total{stage}`;
- `tracedecay_workflow_cancellation_escalations_total{stage,action}`;
- `tracedecay_workflow_effect_unknown_total{adapter_kind,effect_class}`;
- `tracedecay_workflow_retries_total{reason,route_change}`;
- `tracedecay_workflow_recovery_total{action,outcome}`;
- `tracedecay_workflow_route_decisions_total{resolution,reason,provider_kind}`;
- `tracedecay_workflow_recursive_dispatch_rejections_total{role}`; and
- `tracedecay_workflow_fanout_width`.

Labels never contain run, step, attempt, project, user, executable path,
prompt, model-version, artifact, or payload identity. Exact high-cardinality
details remain authorized history, not metric labels.

## Task/work graph bridge

An executable Plan 24 work item is admitted only through a typed application
command that pins the active work-plan version, work-item version, readiness
digest, resolved project/repository/worktree/branch scope, acceptance contract,
route decision, grants, budgets, privacy/config/policy/catalog revisions, and
idempotency identity. An auxiliary step additionally pins the exact Plan 24
auxiliary-attempt request and negotiated provider-adapter descriptor. Admission
creates or references one workflow run/node;
that node's lease, attempt, effect, cancellation, artifact, and receipt
identities are projected back into Plan 24 history.

Plan 32 may report validated runtime evidence, but it does not decide task
identity, dependency state, board lane, completion, model grade, or whether an
external issue is canonical work. Plan 24 may derive readiness and legal graph
transitions from committed runtime receipts, but it never dispatches a worker
or applies an effect. Revalidation after graph, scope, policy, or evidence
change uses one explicit cancel/pause/continue decision against the pinned
runtime node; neither side silently rewrites admitted work.

An admission may carry an accepted Plan 24 topology/partition revision. Plan
32 releases a node only when committed runtime predecessors are satisfied and
the exact Plan 24 accepted set separately authorizes that node or its
contingent release. It enforces useful concurrency and capacity classes,
serializes or isolates shared-authority hubs, records requested versus actual
topology/concurrency and defer reasons, and never invents a partition,
recomputes Plan 24 readiness, or treats predecessor satisfaction as task
acceptance.

Plan 24 task intelligence may use committed run/step/attempt/effect/artifact/
receipt evidence to propose split, merge, resize, re-review, or re-route. A
proposal is not a runtime event, queue update, lease change, retry decision, or
cancellation request. If an authorized user accepts a proposal affecting
admitted work, Plan 09 submits a separate typed Plan 32
pause/cancel/continue/re-admit command with expected authority/runtime
versions. Plan 32 applies only that command and records its own receipt; it
never watches recommendations or recalibration outputs for implicit control.

A version-checked command may apply an accepted Plan 24 minimal-repair
proposal: pause and fence the invalidated subgraph, reconcile in-flight
effects, continue only nodes proved unaffected, and create new attempts for
changed nodes only when the repair decision includes a new accepted-attempt
authorization or an already accepted contingent-release authorization. It
preserves old history and fails closed on stale proof or unknown effect. No
proposal directly changes runtime state.

`AwaitingDecision` references an authorized Plan 24 escalation proposal and
records affected node/attempt, effect reconciliation, deadline, human answer
or override provenance, and cancellation semantics. The runtime never infers
an answer, treats timeout as approval, or creates a cron unblock.

Typed checkpoint evidence may be validated for identity, order and grounding,
but cannot renew a lease, prove task completion, or mutate the graph. Reviewer
roles cannot mutate executor work; adversarial hackers and evaluator fixers
run in isolated fixtures; legitimate solvers validate hardened evaluators;
synthesizers preserve disagreements and minority evidence; and no role
recursively dispatches. Generic debate or consensus is not independent review.

## Remote and host behavior

One daemon authority epoch owns each run. Remote hosts receive bounded typed execution
units and return addressed receipts; they never advance history, choose steps, or mint
leases. Failover verifies history/outbox/effect frontiers and fences the old owner.

Codex, Claude Code, Cursor, and Hermes bundles project the same cataloged
workflow operations and Plan 24 task-step bindings. Existing Claude-generated workflow scripts may be retained
only as historical observations or explicit migration evidence; they are not
executed, translated, imported, or installed by PR17.

[Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)'s
already-shipped read-only advisory operations — feedback-cycle findings,
GitHub-ingested review-thread surfacing, CI-failure localization, and
proximity warnings — may appear as typed workflow steps composed through this
same scheduler/history/lease/effect/artifact kernel with the same
idempotent-effect and receipt guarantees. PR17 is not first availability of
those capabilities, performs no GitHub writes, and defines no second workflow
engine, retry loop, or effect authority. Workflow effects remain workflow
authority only.

## Pinned Hermes regression translations

PR17 carries direct conformance scenarios from the pinned
`NousResearch/hermes-agent@c48d53413aa2c` tests, translated to TraceDecay
authority rather than copied:

- `test_kanban_dispatch_lock.py`: two concurrent dispatchers for one mutable
  authority scope cannot both reclaim/spawn/write. TraceDecay tests two daemon
  authority epochs; the fenced loser performs no admission, adapter start, or
  history/effect write. It does not reproduce a per-board file lock.
- `test_kanban_per_profile_cap.py`: existing running work counts against a
  capacity limit, independent capacity classes remain fair, and deferred work
  becomes eligible later. TraceDecay keys limits by stable provider/backend/
  capability and scope identity rather than profile strings, returns an
  explicit deferred/capacity outcome, and rejects invalid limits instead of
  silently treating them as unlimited.
- `test_kanban_reclaim_claim_lock_guard.py`: a stale reclaim snapshot cannot
  reset newly claimed live work. TraceDecay requires matching
  run/node/attempt/lease ID and authority epoch for every reclaim/cancel/
  terminal transition; PID is diagnostic evidence only.
- `test_kanban_stop.py`: heartbeat without a terminal protocol action does not
  complete work, and reminders are bounded separately from persisted violation
  history. TraceDecay treats process exit without its typed terminal receipt as
  `Partial`/`Failed` or unknown-effect, may surface bounded protocol guidance,
  and never lets guidance synthesize a receipt.
- `test_async_delegation.py`: finished-undelivered results restore once and use
  exclusive delivery acknowledgement, while abandoned running work becomes
  unknown after restart. TraceDecay directly tests atomic terminal
  receipt/outbox publication, idempotent one-consumer delivery, restart replay,
  and fenced unknown in-flight reconciliation.
- `test_kanban_redaction.py`: comment bodies, completion summary/result/
  metadata, and block reasons are redacted before persistence. TraceDecay
  applies secret canaries to every corresponding event, blocker, terminal
  receipt, artifact metadata, review, hint, log, and error sink.

## Acceptance

PR17 is complete when definition validation/versioning, shared scheduling,
atomic history/outbox transitions, restart resume, effect reconciliation,
cancellation, bounded retries/fan-out, internal typed-contract and
CLI/MCP/HTTP/dashboard parity, remote fencing, authorization/privacy,
backup/restore, typed Claude Code CLI and Codex app-server/allowed-CLI provider
adapters, and fault-injection tests pass.
Tests must prove no duplicate observable effect, no false terminal success, no
ambient file/CWD authority, and no dependency on JavaScript, Markdown parsing,
developer-roadmap taskgraph materialization, or arbitrary shell execution.
Plan 24 integration tests additionally prove one runtime mapping per admitted
task step, stale graph/readiness rejection, exact versioned runtime projection,
and the absence of any second runtime clock/scheduler/lease/attempt/effect
authority.
Topology fixtures compare admitted single, sequential, parallel, hierarchical
and hybrid revisions; shared-authority serialization, requested-versus-actual
concurrency, capacity deferral, and refusal to invent an unreviewed partition.
Minimal-repair and decision fixtures cover invalidation fencing, unaffected
proof, unknown-effect reconciliation, stale proposals, new-attempt history,
explicit human override, cancellation, and timeout without approval or
implicit graph/runtime change. Checkpoint and role-isolation fixtures prove
checkpoints cannot renew leases or establish completion, minority review
survives synthesis, hacker/fixer/legitimate-solver roles stay isolated, and no
debate, consensus, recursive dispatch, or second scheduler appears.
Task-intelligence integration tests additionally prove runtime evidence can
produce an anchored advisory proposal without changing run state, and that an
accepted proposal still requires a separately authorized, version-checked
runtime command. Stale proposal, lease, route, or authority evidence fails
before any control or effect transition.
Provider-adapter fixtures use fake protocol/process streams plus supported
native conformance runs to cover executable absence, unsupported capability,
version/model drift, deterministic app-server versus allowed CLI selection,
typed argv/stdin and shell-injection canaries, environment/secret canaries,
malformed/out-of-order/oversized output, stream loss, progress/heartbeat,
deadline and every cancellation/kill-escalation stage, artifact validation,
restart/reconnect/resume, wrong worktree or parent identity, stale lease, and
all terminal outcomes. They prove native Claude Code—not Hermes Anthropic—is
used for Claude routes, Codex fallback is explicit, auxiliary agents cannot
recursively dispatch, and no provider output mutates graph/runtime state.
Configuration fixtures prove every admission pins one complete Plan 20
snapshot, live negotiation cannot invent or reread defaults, Plan 27 drift
evidence cannot mutate settings, invalid fallback fails closed, and an admitted
attempt remains on its pinned executable/model/sandbox/deadline/resume policy
until an explicit cancel/re-admit decision.
Pinned-Hermes regression fixtures additionally cover concurrent authority
epochs, capacity deferral and later eligibility, stale reclaim versus a new
lease, bounded protocol guidance without a terminal receipt, one-time durable
terminal delivery after restart, abandoned-running unknown state, and
pre-persistence secret redaction. Test names and source fields remain evidence
citations only; TraceDecay does not copy Hermes database, status, profile,
claim-lock, PID, tool, or environment contracts.
Public SDK publication and Rust/TypeScript/Python parity are PR18 acceptance
under Plan 17 and are not PR17 completion gates.

## PR17 implementation milestones and gates

### PR17A: Domain and authority contracts

Implement `definition.rs`, `control.rs`, `budget.rs`, `evidence.rs`,
`provider.rs`, and `state.rs`. Contract tests must compile the exact enums and
ports above and prove that Plan 32 has no task graph mutation or task
acceptance API and Plan 24 has no lease, attempt, provider-start, retry, or
effect-settlement API.

Gate:

```text
cargo test --all-features -p tracedecay-domain --test workflow_runtime_contract
```

### PR17B: Store kernel, budget, idempotency, and recovery

Implement canonical events, lease fencing, budget reservations, effect
settlement, outbox publication, evidence sealing, and the fixed restart order.
Fault injection covers crashes after reservation, lease commit, provider
start, terminal commit, evidence sealing, publication, and delivery
acknowledgement.

Gate:

```text
cargo test --all-features -p tracedecay-store --test workflow_runtime_contract
cargo test --all-features --test workflow_runtime_suite retry_recovery
```

### PR17C: Plan 24 bridge, bounded fan-out, and evidence handoff

Implement planner-attempt admission, `AwaitingPlanDecision`, accepted-set
revalidation, concurrency/backpressure, no-progress timeout, deterministic
fallback, per-attempt evidence envelopes, and optional synthesis. Tests prove
that planner output alone creates no child attempt and that only an exact
Plan 24 accepted set unlocks fan-out.

Gate:

```text
cargo test --all-features --test workflow_runtime_suite shared_budget
cargo test --all-features --test workflow_runtime_suite capability_manifest
cargo test --all-features --test workflow_runtime_suite parallelism
cargo test --all-features --test workflow_runtime_suite evidence_handoff
cargo test --all-features --test workflow_runtime_suite no_recursive_dispatch
```

### PR17D: Native provider adapters and human controls

Implement native Claude Code CLI, Codex app-server, and policy-allowed Codex
CLI adapters through `NativeProviderAdapter`. Fake protocol/process tests run
on every platform. PR17 records at least one passing supported-host conformance
run for each claimed native adapter/protocol version; a skipped local test is
diagnostic coverage, not certification.

Gate:

```text
cargo test --all-features --test workflow_runtime_suite native_providers
cargo test --all-features --test workflow_runtime_suite model_routing
```

### PR17E: Surfaces, metrics, and aggregate acceptance

Implement CLI, MCP, HTTP, dashboard application-port parity and Plan 26 source
events. Exercise Markdown and JSON rendering without local execution in any
surface.

Gate:

```text
cargo test --all-features --test workflow_runtime_suite
cargo test --all-features --test workflow_runtime_suite backup_restore
cargo test --all-features --test workflow_runtime_suite remote_fencing
cargo test --all-features --test workflow_runtime_suite surface_parity
cargo test --all-features
```

The aggregate acceptance suite must prove:

1. planner, fan-out, every provider call, retry, recovery, and synthesis share
   one deadline, cancellation generation, and effect ledger;
2. pause, human wait, restart, reconnect, failover, and retry never increase
   remaining time or any consumed budget dimension;
3. cancellation fences all new reservations and unknown effects block
   replacement and success;
4. stale capability, readiness, route, configuration, privacy, or authority
   evidence fails before lease acquisition or process startup;
5. effective concurrency never exceeds the strictest declared limit, bounded
   queue overflow defers or rejects deterministically, and heartbeat without
   frontier progress reaches `NoProgress`;
6. deterministic fallback returns byte-identical ordered payload bytes and
   payload digests, excluding execution identity, timestamps, and accounting
   metadata, while optional synthesis failure preserves all source, failure,
   disagreement, unknown, and minority evidence;
7. same-key/same-digest commands return the original receipt,
   same-key/different-digest commands fail with `IdempotencyConflict`, and
   duplicate provider/effect/evidence/delivery receipts settle once;
8. Claude routes start native Claude Code CLI and reject Hermes Anthropic, API,
   and SDK substitutes; Codex starts app-server first and uses CLI only as an
   explicit pre-start configured fallback;
9. post-start provider failure never switches adapters, reconnect requires
   exact provider session/frontier/lease/epoch, and CLI restart never adopts a
   PID or replays stdin;
10. native approval timeout never approves, stale or grant-expanding human
    controls fail closed, and valid human overrides remain versioned,
    attributable, reversible through a later command, and bounded by admitted
    grants and budget;
11. provider requests for child agents plus attempted HTTP, MCP, CLI, and local
    socket recursive ingress create no task, proposal, run, lease, attempt,
    process, or graph mutation; and
12. runtime `Completed` and synthesis output remain evidence until Plan 24
    independently applies its task acceptance and query semantics.
