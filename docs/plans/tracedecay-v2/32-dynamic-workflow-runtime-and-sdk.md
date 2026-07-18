# PR17: Daemon-owned typed workflow runtime and contracts

**Status:** implementation authority for PR17.

## Decision

TraceDecay workflows compose existing typed application operations. The daemon validates
versioned definitions, owns runs, schedules steps, records effects, and exposes controls.
It is also the sole runtime that executes explicitly admitted
[Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md) task steps;
Plan 24 owns task/work graph state and semantics, while this plan owns runtime
scheduling and effects.

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
- explicit dependency edges, bounded fan-out groups, concurrency and failure
  policy, route/capability requirements, budgets, and acceptance conditions;
- configuration/catalog/policy/privacy snapshots and a definition digest.

Definitions are data, not source code. Unknown operations, cycles, dangling references,
incompatible schemas, unbounded fan-out, privilege escalation, or
unsupported effects reject before activation. Editing creates a new version;
admitted runs stay pinned to their exact version and snapshots.

Lifecycle is `Candidate -> Validated -> Active -> Retired | Rejected`. Names
are scoped aliases only; run admission resolves and records an exact version.
Files may be explicit import/export artifacts, but watchers never auto-import,
activate, or infer authority from CWD or nearest-directory precedence.

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

Plan 24 task intelligence may use committed run/step/attempt/effect/artifact/
receipt evidence to propose split, merge, resize, re-review, or re-route. A
proposal is not a runtime event, queue update, lease change, retry decision, or
cancellation request. If an authorized user accepts a proposal affecting
admitted work, Plan 09 submits a separate typed Plan 32
pause/cancel/continue/re-admit command with expected authority/runtime
versions. Plan 32 applies only that command and records its own receipt; it
never watches recommendations or recalibration outputs for implicit control.

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
