# TraceDecay V2 application crate

## Status / role

`tracedecay-application` is the transport-neutral use-case layer between
product adapters and the domain, query, store, policy, and runtime owners. The
existing application core continues to provide request context, authorization,
scope, evidence envelopes, cursors, cancellation, idempotency, receipts, and
stable problems for shipped operations.

Completion and activity status is owned solely by
[the plan-set index](00-plan-set-index.md). This component plan defines
retained delivery requirements and does not infer milestone status from branch
artifacts.

**Request-context correction (2026-07-26).** Two live `RequestContext` models
remain. `crates/tracedecay-application/src/context.rs` carries the required
`ResolvedScope`; the legacy root model in `src/application/context.rs` carries
session identity and digests but no scope field. That root model therefore
cannot satisfy this plan's scope contract. Convergence on the scope-carrying
application context remains application/surface work, not a frontend
`ResolvedScope` invention or a dashboard-only gap.

(Update 2026-08-07: converged. The legacy root model no longer competes:
`src/application/context.rs` does not exist, relocated into the usecases crate
by `refactor(usecases): move src/application into tracedecay-usecases`
(8946d412f5) and since reshaped into the session-identity type that *maps into*
the application scope rather than carrying a rival context. Exactly one
`RequestContext` is defined repo-wide — the scope-carrying one at
`crates/tracedecay-application/src/context.rs:429`, whose
`scope: ResolvedScope` is a required field. Scope resolution is
fail-closed on profile identity:
`crates/tracedecay-usecases/src/context/mod.rs:182` (`application_scope`)
returns `ApplicationScopeError::ProfileIdentityWithoutProject` rather than
fabricating project/repository/worktree fields from a path or the CWD, and
`:217` (`session_request_scope`) resolves profile-owned session requests only
from identifiers the identity already carries, under a reserved prefix that
cannot compare equal to a real project scope. Direct regressions at `:664`
(`application_scope_maps_project_identity_and_git_route`) and `:684`
(`application_scope_fails_closed_for_profile_identity`) cover both directions.
The two-model correction this paragraph describes is therefore no longer a
live condition.)

**Source-access correction (2026-07-26).** Temporal snapshot composition no
longer hard-codes a participant as `Authorized`. Authorization is a separate,
fail-closed field derived from the authenticated `TemporalAuthorizedRoot` and
the exact participant project. Persisted source metadata and retention expiry
independently produce `Available`, `Locked`, `RetentionWithheld`, `Deleted`,
`Redacted`, or `Unavailable`; invalid or ambiguous source state denies the
snapshot instead of becoming a clean unavailable result.

The application delivery requires this boundary to be the canonical owner of typed operation
semantics. Root composition and surface adapters may wire those operations but
do not become alternate use-case owners. Feedback-read, retrieval,
configuration, edit, and Git handlers visible on a branch and their direct
parity tests are implementation evidence; their present module, port, or type
names are not a contract inventory. The surface delivery requires every required
surface to invoke the canonical operation and preserve its result.

The work loop extends that core only through the user-visible journey shared with
[Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md) and
[Plan 32](32-dynamic-workflow-runtime-and-sdk.md).

## Retained callable application behavior

The rewrite retains every existing application use case and boundary,
including capture, search, source/graph/test retrieval, context, sessions,
memory, code, delivery, automation, Doctor, configuration, edit transactions,
Git preview/apply, workflows, analyzer-backed semantic evidence, and the
read-only branch-aware feedback cycle. Diagnostics/impact, CI localization,
ingested GitHub review threads, and proximity findings remain one-shot
advisory application results with exact branch/worktree/generation identity,
coverage, lifecycle, cursors, and anchors; no second finding store or GitHub
write path is introduced.

Navigation, type hierarchy, impact, affected tests, diagnostics, and refactor
preview retain provider identity, document/generation freshness, coverage,
provenance, conflict, and explicit unsupported/indexing/stale/cancelled/
timed-out/failed/partial states. A provider can enrich evidence but cannot
replace stable graph identity, prove test execution or delivery, or turn
unavailable evidence into a clean empty result.

Application search and feedback handlers never join repository parsing,
lexical projection, or FastEmbed work. They freeze the latest complete
compatible generation for the exact worktree, return exact/lexical/graph and
other ready evidence immediately, and omit semantic contribution while it is
indexing, stale, failed, cancelled, or incompatible. The result retains the
selected generation plus provider freshness and coverage so every transport
can render the same partial-but-usable outcome.

## Doctor application kernel

Plan 09 is the implementation and use-case composition authority for the one
Doctor application kernel. Plan 14 supplies the historical regression and
observable-behavior contract; Plan 20 supplies desired/effective configuration;
Plans 27, 32, and the owning runtime/storage/query components supply typed
health evidence; Plan 26 supplies denominator-safe
observations; and Plan 11 renders the resulting findings, coverage, and evidence
without evaluating health.

The kernel composes those inputs into stable Doctor finding families,
distinguishes unsupported, absent, stale, degraded, partial, unknown, denied,
and healthy-with-complete-coverage states. Doctor exposes no generic action
registry, mutation preview, apply route, cleanup, GC, retention, relink, repair,
or recovery dispatcher. Recovery and retention remain separately entered owner
operations or bounded daemon maintenance, with their own authorization, lease,
fencing, and durable receipts. Doctor never executes those operations, invents
a generic health score, treats dispatch as recovery, or
collapses unknown/partial evidence into healthy or clean.

Direct tests start from real findings, inject source disagreements and
operational failures, call the canonical Doctor use case, and independently
re-observe the owning authority. Focused cases cover unavailable providers,
executable/protocol/configuration drift, invalid fallback, sandbox/capability
mismatch, stuck or unknown runtime state, incomplete telemetry, authorization
loss, cancellation, and truthful no-change outcomes. They prove Plan 11
performs no Doctor evaluation and Plan 14 contributes no runtime kernel.

The work loop adds all Plan 24/32 semantic operations to the same layer: graph and
history commands; Kanban, DAG, timeline, causal, critical-path, workload,
executor/model, repository/delivery, evidence, and history projections;
task-shape, sizing, decomposition, topology, route, independent-review,
handoff/experience, outcome/calibration, and live-replan operations; workflow
definition/version lifecycle; provider capability/admission/progress/history/
artifact/control; placement; integration; and runtime recovery. This list is
capability coverage, not a second operation registry.

## Work loop user outcome

An authorized user can create versioned work, retrieve its exact evidence,
review an explained proposal, explicitly admit one executable step, inspect
progress and outcome, and review a later replan. CLI, MCP, HTTP, dashboard, and
host adapters invoke the same application behavior and cannot bypass review,
admission, or safety checks.

## End-to-end production path

1. **Create work.** A command validates typed owner scope, task identity,
   dependency legality, acceptance requirements, expected versions, and an
   idempotency key, then commits one immutable graph version through Plan 24
   and returns all authorized saved Work projections.
2. **Retrieve evidence.** A TaskId-rooted read assembles bounded authorized
   evidence from owning application/query operations. It preserves exact
   anchors, source authority, temporal mode, coverage, omissions, and
   continuation; summaries never replace source evidence.
3. **Explain a proposal.** The application gives Plan 06 immutable evidence,
   eligible capabilities, and one complete Plan 20 snapshot. It returns
   task-shape, calibrated sizing, decomposition, topology, independent-review,
   synthesis, and route alternatives plus deterministic fallback or abstention
   without mutating the graph or runtime.
4. **Review and admit.** A user explicitly accepts or rejects the proposal with
   graph/proposal/evidence CAS. A separate command reauthorizes the accepted
   work version, readiness, scope, route, grants, budgets, and
   configuration immediately before Plan 32 acquires a lease or starts a
   provider.
5. **Inspect execution.** Reads expose the requested and actual provider/model,
   lease and attempt, ordered progress frontier, artifacts, partial or unknown
   state, cancellation, and terminal receipt from Plan 32.
6. **Review outcome and replan.** Committed runtime, test, review, cost,
   handoff, experience, and outcome evidence can produce a split, merge,
   resize, reroute, re-review, minimal-repair, restart, or escalation proposal.
   The application presents legal actions; nothing applies automatically.

Contracts, store calls, policy evaluation, configuration snapshot resolution,
surface binding, and provider admission land as slices of this path, not as
independent contract-only phases.

## Application responsibilities

- Define one direct typed entry point for each operation in the loop. Prefer
  ordinary Rust calls over a command bus, query bus, service locator, generic
  invoke operation, or runtime registry.
- Preserve actor, ProjectId, repository/worktree/branch scope, capability
  grant, request identity, deadline, cancellation, and disclosure constraints
  through every call.
- Revalidate exact project/user authorization, configuration, catalog/provider
  capability, evidence, graph versions, readiness, budgets, and
  expected runtime state immediately before every read expansion or effect.
- Use operation-specific idempotency keys. Same key and canonical input return
  the original receipt; a changed input fails with an idempotency conflict.
- Keep graph semantics in Plan 24 and runtime clocks, scheduling, queues,
  leases, attempts, effects, retries, artifacts, recovery, and cancellation in
  Plan 32. The application authorizes and maps between them; it duplicates
  neither authority.
- Keep concrete stores, transports, provider processes, model clients,
  dashboard assets, and runtime implementations outside this crate.

## Evidence and result behavior

Existing application result semantics remain the single surface contract:

- admitted reads carry temporal state, authority, evidence authorities,
  coverage, omissions, scores, contributions, paging, execution state, and a
  typed payload;
- every canonical application problem carries exactly one retry directive:
  `Never | SameRequest | AfterDelay | AfterRevalidate | AfterReconcile`.
  Application handlers select it from authoritative operation state. CLI,
  MCP, HTTP, LSP, dashboard, SDK, and host adapters preserve that directive
  exactly and never infer, strengthen, weaken, combine, or replace it from a
  status code, transport failure, effect class, or local retry policy;
- previews pin expected state and effect class without mutating;
- admitted effects return durable idempotent receipts with reconciliation and
  exactly one truthful terminal outcome;
- complete zero results are distinct from partial, unavailable, unsupported,
  stale, redacted, cancelled, timed out, failed, or unknown results; and
- absent, out-of-scope, and policy-hidden addressed resources return the same
  non-disclosing public shape before counts, cursors, provider state, timing,
  or existence are revealed.

Task evidence retrieval delegates to the canonical current/as-of/evolution/
forensic session kernel and the owning code, Git, CI, diagnostic, impact,
affected-test, review, artifact, and outcome authorities. Every page,
hydration, and exact expansion reauthorizes. A TaskId, cursor, response handle,
run ID, or anchor grants nothing by possession.

Scores preserve their declared kind and producer. Incomparable ordinal,
heuristic, probability, or interval values are not averaged or silently
ordered. Invalid or stale calibration remains explicit.

## Proposal and admission behavior

- Proposal generation is read-only. Every proposal pins the work and graph
  versions, evidence watermarks and anchors, code/Git generation, scope,
  policy/configuration/catalog revisions, and runtime observations it
  used.
- Accept, reject, and supersede are separate commands with expected versions,
  authorization, actor, reason, and idempotency identity. Stale or illegal
  proposals change nothing.
- Provider admission accepts typed provider/backend/model/reasoning, sandbox
  and approval class, capabilities, bounded context, budgets, deadline,
  cancellation, and opaque secret references. It never accepts shell strings,
  arbitrary argv or environment, unbounded prompts, executable paths supplied
  by the caller, or an adapter-local fallback flag.
- Requested-versus-actual provider, backend, model, protocol, sandbox, or
  approval mismatch fails closed unless the exact pinned policy explicitly
  permits the fallback.
- Runtime `Completed` is execution evidence, not task acceptance. Plan 24
  applies acceptance semantics in a separate versioned graph command.
- A replan proposal cannot pause, cancel, retry, reroute, resize, or repair a
  run. If accepted, the application sends a distinct version-checked Plan 32
  control or re-admission command.

## Safe effects

Source edits continue through the journaled application edit transaction.
Git index mutation continues through immutable preview then explicit apply
with repository/worktree/HEAD/index/content CAS and a durable receipt. The work loop
may request only Plan 36-owned typed native Git effects admitted by Plan 32
under an accepted work proposal. No application operation exposes arbitrary
Git, force update, rebase, history rewrite, branch deletion, implicit merge,
or semantic conflict resolution.

Cancellation and deadline checks surround admission and every expensive or
multi-stage operation. Once an effect may have crossed its commit point, the
application reports committed or unknown/reconciling state; it never rewrites
the result as a local timeout or cancellation.

## Implementation slices

1. Extend the existing work command/read path so one real surface can create a
   versioned work item and retrieve exact bounded evidence.
2. Add proposal generation and explicit decision to that path, calling the
   existing policy and configuration authorities directly.
3. Add one admitted Plan 32 provider step and progress/outcome inspection,
   including cancellation and restart recovery.
4. Feed the resulting evidence into non-auto-applied replanning and expose the
   same legal actions through all selected surfaces.
5. Keep every workflow definition/run/control, Work projection, provider,
   placement, integration, handoff, experience, outcome/calibration, and
   SDK-facing semantic operation callable through this same layer; the SDK delivery freezes
   compatibility names without adding missing behavior.

Each slice includes its minimal domain/store/query behavior and direct
surface test. A schema, trait, port, handler descriptor, or catalog
contribution does not land without the callable use case that consumes it.

## Replacement and deletion

- Delete any handler-local authorization, task mutation, evidence assembly,
  readiness, route selection, retry, completion, or error path after its
  application operation is live.
- Remove work-loop-only operation registries, speculative handler catalogs,
  standalone contract phases, exact file inventories, and declaration-parity
  fixtures.
- Do not preserve a shadow task store, scheduler, provider dispatcher, model
  default, or surface-specific result shape.

## Direct acceptance

One production journey, exercised through the shared application path, must:

1. create versioned work;
2. retrieve exact authorized evidence and expand at least one source anchor;
3. show an explained decomposition and provider recommendation;
4. require explicit proposal acceptance and separate provider admission;
5. run one supported real provider step;
6. stream and resume progress, inspect its truthful terminal receipt and
   requested-versus-actual route; and
7. generate a justified replan that leaves graph and runtime state unchanged
   until another explicit command.

Focused failures cover cycle and stale-version rejection, authorization
narrowing, partial evidence, missing provider capability, invalid
fallback, idempotent replay/conflict, cancellation before and after an effect
commit point, restart recovery, stale lease/attempt receipts, unknown effects,
no recursive provider dispatch, and no false completion. Those direct tests
plus ordinary aggregate repository checks prove CLI/MCP/HTTP/dashboard
semantic parity for this journey and the absence of transport, concrete-store,
provider, or runtime dependencies from the application crate; they create no
separate acceptance gate. Every problem fixture asserts exactly one of `Never`,
`SameRequest`, `AfterDelay`, `AfterRevalidate`, or `AfterReconcile` at the
canonical application boundary and byte/semantic preservation of that
directive by every exercised adapter, with no adapter-side inference.

## Not in the work loop

- The SDK delivery chooses and stabilizes public API and SDK names.
- Measured performance optimization covers the loop after production evidence exists.
- Developer-plan parsing, Markdown execution, JavaScript workflow execution,
  generic provider invocation, and autonomous task or Git mutation remain out
  of scope.
