# PR14 core / PR17 residual daemon-owned typed workflow runtime

**Status:** implementation authority for PR14's minimal real-provider runtime
and PR17's residual advanced workflow behavior. PR18 publishes SDK names for
the same callable operations; it does not add a second runtime or defer missing
lifecycle semantics.

**Approved delivery split (2026-07-28).** PR14 owns one supported native
provider path, leases/attempts, bounded artifacts/progress, cancel/resume,
restart fencing, sealed terminal evidence, and projection into Work. PR17 owns
workflow definitions, multi-step/fan-out/synthesis/recovery, advanced
placement, expertise/calibration, automation controls, and host/LSP handoff.
Both use one runtime authority.

Earlier runtime type/file inventories, operation registries, fixture corpora,
phase names, and packet gates are historical evidence, not prerequisites or
features that PR17 must recreate. Published operations and persisted
definition, run, attempt, effect, and receipt records retain compatibility and
migration obligations; all other retention is judged by the direct runtime,
provider, recovery, platform, and regression behavior below.

No Plan 32 workflow/lease/attempt contract is established on `origin/master` or
in a published package/release. Pure source-only/internal request helpers take
their final shape in place. Wire-visible request revisions retain negotiation
until an authorized installed-client/host census proves absence. Definition,
run, lease, attempt, effect, journal, and receipt records are persisted product
data and may exist in dogfood stores; their
backward-read/migration/recovery obligations remain fail-closed until the
registered-store census proves absence. Definition and authority versions
below remain product-data history and fencing identities, not by themselves
evidence that a second wire-contract version shipped.

## Decision

TraceDecay workflows compose existing typed application operations. The daemon
validates versioned definitions, admits runs, schedules steps, records history
and effects, and exposes controls. The same daemon kernel is the sole runtime
for explicitly admitted [Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md)
work.

Plan 24 owns task identity, dependencies, readiness, proposals, accepted
attempt sets, and acceptance. Plan 32 owns runtime clocks, queues, capacity,
leases, attempts, providers, progress, effects, retries, recovery,
cancellation, artifacts, placement, and integration execution. Runtime
`Completed` is evidence for Plan 24, never automatic task completion.

PR17 adds no JavaScript/TypeScript workflow runtime, generated workflow source,
Markdown parser, shell command tape, developer-plan executor, or recursive
generic execution.

## PR14/PR17 implementation defaults

- Use `petgraph` for task/workflow DAG traversal, topological order, and SCC
  rejection; `tokio-util` for cancellation; Serde plus `schemars` and
  `jsonschema` for immutable definition validation; rusqlite transactions for
  atomic claims, effects, and publication; `process-wrap` for admitted
  provider process-tree containment; and `d3-dag` for dashboard layout. These
  replace bespoke graph algorithms, cancellation tokens, schema walkers,
  transaction choreography, child-process cleanup, and DAG layout.
- Use existing Tokio timers and `DelayQueue` only for mechanical waiting.
  TraceDecay owns retry eligibility, attempt creation, caps, jitter, exact
  retry directives, cumulative deadline/budget, cancellation, attempt
  identity, and receipts. Add no retry library or library-owned retry policy.
- These libraries do not own Plan 24 task meaning, admission, readiness,
  scheduling, runtime clocks, authority, lease fences, provider selection,
  effect reconciliation, evidence, or replanning. Reject an integration that
  requires a second scheduler, state machine, journal, or authority model.
  Add no workflow platform, PTY layer, `statig`, or outbox framework; retain
  the existing atomic outbox publication semantics in the owner transaction.

## PR14 core and PR17 residual user outcome

After reviewing an evidence-backed Plan 24 proposal, a user can explicitly
admit one real provider-backed step, see the exact provider/model and fallback
used, watch bounded resumable progress, inspect artifacts and a truthful
terminal receipt, cancel or recover safely, and then review a Plan 24 replan
that the runtime never auto-applies.

The same runtime also retains product workflow definition versioning,
validation, activation, execution, history, pause/resume/cancel/retry,
approval, effect reconciliation, bounded fan-out, optional synthesis,
placement, safe Git integration, remote fencing, and backup/restore behavior.

## End-to-end production path

1. A Plan 24 accepted work version supplies exact readiness, evidence,
   decomposition/topology, route, grants, budgets, and acceptance references.
2. The user separately admits execution. The application reauthorizes every
   reference and one complete policy/configuration/capability snapshot
   before the runtime reserves capacity or starts a provider.
3. The runtime creates one durable run-control aggregate, acquires fenced
   leases, creates an attempt, and negotiates the selected provider. No
   provider or discovery adapter can inject a default.
4. The provider runs under a bounded typed execution envelope. Ordered events,
   progress, artifacts, approvals, effects, requested/actual route, and
   resource usage enter canonical history.
5. Cancellation, retry, reconnect, daemon restart, and owner failover use the
   same deadline, budget, authority epoch, lease fences, effect journal, and
   reconciliation rules.
6. A sealed terminal evidence envelope is projected into Plan 24. Plan 24
   independently evaluates acceptance and may generate a replan.
7. A replan changes no runtime state. If the user accepts it, the application
   sends a distinct version-checked pause/cancel/continue/retry/re-admit or
   integration command.

Definitions, persistence, configuration, policy, surfaces, metrics, provider
adapters, placement, and effect recovery land as slices of this runnable path,
not contract-only phases.

## Typed workflow definitions

An immutable definition version retains owner and scope, input/output schemas,
typed steps referencing cataloged application operations, validated references
to prior outputs, runtime predecessor edges, bounded fan-out groups,
concurrency/failure policy, route/capability requirements, budgets, result
conditions, retention, and pinned policy/configuration/catalog
snapshots.

Definitions are product data, not source code. Unknown operations, cycles,
dangling references, incompatible schemas, unbounded fan-out, privilege
expansion, unsupported effects, or recursive generic execution reject before
activation. Editing creates a new version; admitted runs remain pinned.
Lifecycle retains candidate, validate, activate, retire, reject, list, get,
diff, and history operations through the same application surfaces.

Runtime predecessor and result conditions release and close only admitted
workflow nodes. They cannot create a Plan 24 dependency, accepted child work,
or task completion. Files may be explicit import/export artifacts, but watchers
never auto-import or activate them and CWD never supplies scope.

## One runtime, run control, and effect budget

Every run has one durable control aggregate containing immutable admitted
limits and snapshots plus monotonically versioned authority, cancellation,
deadline checkpoint, and shared budget ledger. Planning, operation, fan-out,
provider, synthesis, placement, verification, integration, publication,
cleanup, retry, recovery, and control stages consume that aggregate.

- Remaining time never increases after pause, human wait, retry, reconnect,
  failover, clock rollback, or daemon restart.
- Attempts, provider calls, effects, tokens, cost, output, and artifacts are
  cumulative. Retry does not refund consumed work. Parallelism is a bounded
  released gauge.
- Every stage reserves its upper bound before work. Reservation, idempotency
  claim, lease/effect journal, and outbox publication commit atomically on the
  owner shard.
- Exhaustion fences new work, preserves committed evidence, and follows only
  the declared evidence-preserving fallback or a truthful terminal outcome.
- Pause and cancellation fence new reservations and reconcile active effects
  before publishing a stable state.

Queues are durable and bounded. Effective concurrency is the minimum of run,
fan-out group, provider, capacity class, capability snapshot, topology, and
remaining-budget limits. Overflow defers or rejects deterministically.
Fairness is stable within a capacity class. Heartbeat updates liveness only;
only a newly committed event, effect, artifact, or terminal frontier resets
the no-progress deadline.

## Plan 24 handoff and bounded multi-agent execution

Planner execution is itself a leased bounded attempt over authorized exact
context and the immutable capability snapshot. Planner output is evidence, not
child work. Plan 24 validates it and returns a versioned accepted attempt set,
rejection, expiry, or supersession decision.

Only a version-checked Plan 24 decision can release canonical work. Fan-out
revalidates the exact accepted set, readiness, manifest, authority, scope, and
budget before the first lease. Every child has an independently fenced attempt
and sealed evidence envelope. Failure policy remains fail-fast, collect within
budget, or require-at-least a declared success count.

Optional synthesis is another admitted attempt under the same deadline,
cancellation, and ledger. It consumes immutable ordered source envelopes,
preserves failures, unknowns, disagreement, and minority evidence, and cites
every source. Synthesis failure returns the unsynthesized evidence set; it
cannot erase or rewrite evidence.

Auxiliary providers cannot request children, call a generic task/workflow
ingress, or receive task admission, graph mutation, runtime control, lease
minting, provider selection, or ambient daemon credentials. Provider-native
subagent/delegation capability must be provably disabled or negotiation is
unsupported. Observed external child activity may be retained as evidence but
cannot create canonical work or execution.

## Model routing and deterministic fallback

Plan 24 owns the task-domain recommendation and Plan 06 owns the policy
decision. Plan 32 performs only capability negotiation and execution against
the pinned result.

Resolution order is the exact requested route, stored explicit fallbacks in
order, the declared deterministic non-model action, then fail closed.
Deterministic actions may run only when the capability snapshot proves the
exact operation version and effect/reconciliation class. Ordered evidence
without synthesis preserves all failures and disagreement.

Plan 32 never reranks models, infers a provider from a model string, discovers
ambient executables, or invents fallback. A human override is version checked,
attributable, limited to eligible routes, and unable to widen grants, project
scope, egress, budget, or deadline. No route can change after provider startup; any
later eligible fallback is a new revalidated attempt.

## Native provider execution

PR17 retains typed adapters for the supported native Claude Code CLI, Codex
app-server, and explicitly allowed Codex CLI fallback.

- Claude-designated work uses the configured native Claude Code CLI. Hermes
  Anthropic, direct API calls, and in-process SDK clients do not satisfy it.
- Codex-designated work prefers the configured app-server. Codex CLI is
  eligible only when app-server is unsupported or absent before session start
  and the pinned Plan 20 snapshot explicitly allows that fallback.
- Negotiation records provider/backend/executable/protocol/model/reasoning,
  versions, availability freshness, capabilities, sandbox/approval, network/
  egress, progress, cancellation, reconnect/resume, and every source revision.
- Supported, unsupported, absent, stale, and failed capability states are
  explicit and never trigger an implicit route change.

The admitted envelope binds project/repository/worktree/branch/ref/commit and
code generation, work/parent/session lineage, run/node/lease/attempt, actor and
authority epoch, bounded context handles, requested route, sandbox and approval
class, capabilities, budgets, deadline, cancellation, output contracts,
environment allowlist, and opaque secret references.

Callers provide typed fields, never shell strings, arbitrary argv, raw
environment, executable paths, interpolation, redirects, or ambient prompts.
The selected adapter lowers those fields into a pinned launch plan. Secret
values are resolved just in time and never enter events, logs, errors,
receipts, or diagnostics.

Provider stdout, stderr, and protocol events are distinct bounded ordered
channels with redaction and coverage. Structured events prove only their
protocol semantics; free text is evidence, not graph mutation or terminal
success. Malformed, out-of-order, oversized, lost, or version-drifted streams
return failed or partial evidence, never text-scraped success.

Provider approval is bound to one run/node/attempt/lease, native request,
grants, sequence, deadline, and cancellation generation. A version-checked
human response cannot broaden authority; timeout denies or cancels.

Sessions are classified as observational, intercepted effects, or one bounded
compound non-repeatable effect. Independently writable effects require
interception and a separate permit/receipt; if interception is unavailable,
admission rejects. Loss of a non-repeatable receipt becomes EffectUnknown and
is never retried automatically.

## Progress, artifacts, terminal evidence, and task acceptance

Progress and heartbeat update only attempt liveness/history and never renew a
lost lease or prove completion. Artifacts enter only declared bounded channels
with identity, type/content validation, exact project authorization, coverage,
and retention.

Every terminal envelope preserves run/node/attempt/lease, producer, requested
and actual route, source frontier, terminal receipt, typed observations,
artifacts, coverage, unknowns, disagreements, budget use, source packets, and
payload digest. Sealing validates fence, frontier, schema, exact project
authorization, redaction, and digest. Seal plus outbox publication is atomic,
and delivery acknowledgement is idempotent.

Completed, unsupported, absent, stale, cancelled, timed out, failed, partial,
and effect unknown remain distinct. Process exit or provider summary without a
valid terminal receipt cannot produce Completed. Plan 24 independently applies
task acceptance after receiving this evidence.

## Retry, cancellation, recovery, and fencing

Retries are bounded pure decisions over pinned policy, attempt ordinal,
provider outcome/phase, effect and idempotency state, route position, reconnect
proof, and remaining deadline/ledger.

Unknown or dispatched-without-receipt effects reconcile first. Exact reconnect
preserves an attempt only when provider session, frontier, lease fence,
cancellation generation, and authority epoch all match. A new attempt is legal
only after prior effects are proved absent or repeatable. Terminal runs never
reopen; later work is a linked re-admission.

Cancellation proceeds through protocol-native control, bounded interrupt/
terminate/kill for owned process groups, and effect reconciliation. Failure to
prove termination remains partial or effect unknown and blocks replacement.
Cancellation never rewrites committed history or a crossed effect commit point.

Restart/failover rebuilds history and consumed budget, increments authority
epoch, fences old leases, replays only committed outbox records, reconciles
unknown effects, reconnects only proved sessions, seals proved terminal
evidence, and releases newly ready nodes. PID, path existence, branch name,
process exit, task state, or elapsed time is never recovery proof.

Remote workers receive bounded addressed execution units and return receipts;
they never advance history, select work, mint leases, or choose routes. One
authority epoch owns a run. Backup/restore preserves history, idempotency,
leases/effects, outbox, artifact references, and reconciliation frontiers
before admissions resume.

## Placement, topology, and safe Git effects

Plan 32 retains execution of accepted Plan 24 placement and integration
proposals. Placement, branch, stack, commit, pull request, and integration
identities are evidence relations layered onto TaskId and never redefine it.

Supported placement remains no managed placement, explicitly acknowledged
strictly clean in-place, linked worktree, or isolated local clone. Linked and
isolated placements are canonical, exclusive, fenced, network-free where
declared, and retained/quarantined rather than cleaned when dirty, conflicted,
unknown, or uniquely valuable.

Placement, branch topology, review topology, and integration strategy remain
independent. Task DAG and branch-stack DAG are never conflated. Stack execution
uses stable parent-before-child order only after exact accepted relations,
required commit frontiers, checks, and integration receipts.

Plan 36 remains the native Git evidence and operation owner. Plan 32 reserves
the effect, invokes typed preflight/apply, and journals the receipt. Providers
never receive Git permits. Candidate preparation and all required cataloged
verification run on an isolated exact generation before target movement.

Local ref updates are exact compare-and-swap. Remote publication is an ordinary
verified fast-forward of the accepted candidate. Standard-PR base retarget is
version checked and can change only the accepted base; it cannot create,
merge, close, edit, or comment on a PR. GitHub review ingress remains read-only.

At or after a local/remote/provider commit point, automatic rollback is
forbidden. Partial or unknown state requires forward repair or reconciliation.
The runtime never stashes, cleans, resets, rebases, squashes, amends, reverts,
deletes branches, moves refs backward, force-pushes, bypasses hooks, accepts
arbitrary Git, or resolves semantic conflicts.

Retention expiry is eligibility for a fresh cleanup preflight, not delete
authority. Dirty/untracked data, unique commits or bytes, active holders,
unresolved effects, unacknowledged receipts, uncertain PRs, shared refs,
missing anchors, stale scope, or authorization loss block removal.

## Application operations and surfaces

PR17 retains callable typed operations for:

- workflow definition list/get/create-version/validate/activate/retire/diff;
- work-backed and general typed run start/admit/inspect/status/history;
- Plan 24 proposal-decision application and accepted-attempt release;
- pause/resume/cancel/retry/reconcile and native approval/deny;
- provider capability and requested/actual route inspection;
- progress/event/artifact/evidence paging and resume;
- placement preflight/admit/status/release and safe cleanup;
- integration preflight/admit/status/cancel/reconcile and receipt inspection;
  and
- runtime observations, capacity/defer reasons, costs, recovery state, and
  backup/restore evidence.

CLI, MCP, HTTP, dashboard, and host bindings call the same application
operations. They contain no readiness, scheduling, provider, retry, effect, or
Git logic. PR17 names may remain pre-stabilization bindings, but every
SDK-facing semantic operation and lifecycle state is callable and tested.
PR18 freezes Rust, TypeScript, HTTP, CLI, and MCP compatibility names
without changing this behavior.

## Work projections and observations

Plan 32 emits bounded typed source events for run outcome/duration, queue and
capacity, budget, provider route, retries, cancellation escalation, no-progress
timeout, effect unknown, recovery, recursive-dispatch rejection, fan-out,
placement, quarantine, Git preflight, integration, conflicts, ref updates, PR
retarget, and safety rejection. Plan 26 owns metric meaning,
retention, dashboards, and calibration.

High-cardinality task/run/project/user/path/prompt/model-version/artifact
identity remains authorized history, never a metric label. Plan 24 Kanban, DAG,
timeline, causal, workload, executor/model, repository/delivery, evidence, and
history projections join runtime state only through exact versioned references.

Plan 32 also publishes typed provider availability, lease/attempt liveness,
progress, deadline, cancellation escalation, reconnect/resume, unknown-effect,
placement/integration, and terminal evidence to the existing Doctor kernel.
Doctor owns finding severity and remediation presentation; it may invoke only a
separately authorized runtime control and cannot reclaim, retry, cancel, repair,
or change configuration by inference.

Already-shipped read-only feedback diagnostics, CI localization, GitHub review
ingest, and proximity reads may be composed as typed workflow steps through
this same kernel. They remain read-only and gain no GitHub write, task
acceptance, or provider-dispatch authority from workflow composition.

## Implementation slices

1. Admit a reviewed Plan 24 step into the shared durable run-control, history,
   lease, budget, idempotency, and recovery kernel.
2. Execute it through one supported real native provider with complete
   negotiation, progress, artifact, approval, cancellation, retry, and terminal
   evidence.
3. Project the result into every retained Plan 24 Work view and feed it into
   non-auto-applied replanning.
   These first three slices are PR14 core.
4. In PR17, exercise bounded fan-out, independent review, optional synthesis, capacity,
   pause/resume, restart/failover, and backup/restore through the same journey.
5. In PR17, exercise each retained placement, stack, review, and safe integration mode
   through the same TaskId and explicit effect path.
6. In PR17, bind residual lifecycle and SDK-facing operations across the selected
   surfaces and emit production observations.

Each slice includes its minimal contracts, persistence, configuration/policy
use, adapter behavior, surface, and direct tests. No contract-only phase,
standalone port/registry, exact file/type inventory, fixture framework, or
shadow gate counts as delivery.

## Replacement and deletion

- Remove duplicate workflow databases, task schedulers, clocks, queues, lease
  families, retry loops, provider dispatchers, effect journals, artifact
  stores, and surface-local runtime logic.
- Remove scaffold-only PR17A–F phases, operation/type/file inventories, giant
  fixture corpora, generated provider/catalog registries, declaration parity,
  and repeated authority prose.
- Keep every capability above in the production loop. Compatibility aliases
  backed by release evidence or retained pending the installed-client/host
  census delegate to canonical operations and own no logic. Pure source-only
  aliases are removed in place; branch-era callable aliases remain until the
  census proves absence. PR18 freezes rather than invents lifecycle behavior.

## Direct acceptance

The PR14 core journey must create work, retrieve exact evidence, receive
and explicitly accept an explained proposal, separately admit a supported real
native provider step, watch and resume progress, inspect requested/actual route,
artifacts and truthful outcome, and review an unapplied replan across CLI, MCP,
HTTP, dashboard, and host/application bindings.

PR17 expands the same journey for the residual capabilities below. Together
the journeys prove:

- one deadline, cancellation generation, authority epoch, budget ledger,
  history, scheduler, lease/attempt/effect authority, and idempotent receipt
  path across planning, fan-out, provider, synthesis, placement, Git,
  integration, retry, recovery, and control;
- bounded concurrency/backpressure, no-progress timeout, deterministic
  fallback, independent evidence envelopes, minority-preserving synthesis, and
  no recursive execution;
- native Claude Code routing, Codex app-server and explicitly configured CLI
  fallback, no provider-local defaults or hidden model choice, typed argv/stdin,
  direct credential/environment handling, malformed stream handling, approvals,
  cancellation escalation, reconnect/resume, and every terminal state;
- stale graph/readiness/evidence/route/config/provider/authority/lease
  rejection before effect, same-key replay and changed-input conflict, no
  duplicate observable effect, and truthful partial/unknown recovery;
- all retained workflow definition/run/control/history/evidence/provider/
  placement/integration/observation and SDK-facing operations;
- no-Git, clean in-place, linked worktree, isolated clone, unbranched,
  independent branch, local stack, independent review, standard PR, optional
  GitHub stack observation, fast-forward, two-parent merge, and exact ordered
  cherry-pick where authorized, all preserving TaskId; and
- pre-target verification, ref CAS, ordinary fast-forward publication,
  version-checked PR retarget, stack order, retention/quarantine, cancellation
  races, crash points, remote fencing, and backup/restore without force,
  history rewrite, ambiguity replay, or false success.

Focused fixtures may use disposable stock-Git repositories, fake protocol
streams, and checked-in real provider evidence, but at least one supported
native conformance run is required for every adapter/protocol claimed as
production. A skipped native run is diagnostic coverage, not certification.
Compact direct regressions replace giant declarative fixture catalogs and
contract-only gates.

## Not in PR14/PR17

- PR18 publishes and freezes the public Rust and TypeScript SDKs and
  compatibility policy for these already complete operations.
- PR20 optimizes measured queue, provider, evidence, projection, and effect
  latency.
- JavaScript workflow execution, arbitrary shell/process tools, generic
  recursive dispatch, autonomous task/replan application, unsafe Git, and a
  second runtime remain out of scope.
