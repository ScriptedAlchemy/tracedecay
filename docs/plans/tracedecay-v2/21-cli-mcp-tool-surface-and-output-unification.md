# TraceDecay V2 CLI, MCP, LSP, and output unification

**Delivery scope:** PR12 core requirements; PR17 executable work loop; PR18
public SDK/name freeze.

## Status / role

CLI, MCP, and HTTP are thin clients over daemon-owned application use cases.
Dashboard uses the same application results only when its first binding ships
in PR14. LSP remains a stateful sibling adapter over the same typed code and
diagnostic operations; it is not a generic workflow transport.

Completion and activity status is owned solely by
[the plan-set index](00-plan-set-index.md). This component plan defines
retained delivery requirements and does not infer milestone status from branch
artifacts.

PR11 requires the canonical application surface; PR12 requires every CLI, MCP,
HTTP/SSE, feedback, Git, and LSP binding to route through it while preserving
output, cursor, cancellation, daemon-capacity, and semantic parity. Dispatcher,
binding, schema, and fixture names visible on a branch are implementation
evidence rather than a spine to reconstruct. A missing callable operation or
lost semantic is a gap; a renamed/deleted scaffold is not. PR17 adds only the
surface needed to complete the Plan 24/32 user journey.

Only actually independently released public CLI/MCP/HTTP names and protocol
shapes retain compatibility. Pure source-only/internal PR12/PR17 bindings,
branch-era V2 callable names, and V2 generated request/response revisions
change in place. Persisted cursors, cancellation records, idempotency keys,
journals, checkpoints, and receipts accept only their exact final shape; any
other database, store, spool, file, or projection returns typed
`ResetRequired` and requires explicit reset or recreation. No storage reader,
migration, backfill, dual write, or census path exists; tests and operation
inventories alone are not publication evidence.

**Cursor-parity correction (2026-07-27).** The cursor half of that parity
requirement was stated as delivered while no shipped code-read surface could
supply a continuation: CLI, MCP, and HTTP each pinned the page to
`PageRequest::first(DEFAULT_PAGE_SIZE)`, so a caller received a `next_cursor` it
had no way to spend, and HTTP discarded `?cursor=` for code reads. `97d6499ce`
closes it by carrying the continuation on `CallableCodeSurfaceMeta`, which all
fourteen code operations pass through, and by advertising `cursor` in the MCP
schema only now that it is honored. Page size deliberately remains a fixed
invocation control at ten; there is no advertised page-size parameter, and its
absence is not a parity gap. Verification is the pr12 reachability test resuming
page two through MCP and the installed CLI; like every commit from that date it
has scoped local evidence only, because no CI has run since 01:24 UTC.

## Retained and required surface capabilities

The delivery-first rewrite changes sequencing and removes duplicate
implementation prose; it does not remove or narrow retained or required
surface capabilities:

- paired CLI/MCP bindings remain for symbol, source, graph, test attribution,
  temporal/session, project, configuration, health, storage/runtime, memory,
  LCM, edit, Git, feedback, task/work, workflow, and provider/runtime
  operations as their owning application handlers ship;
- search, exact and qualified lookup, signature, implementation and type
  hierarchy; bounded source lines/body/outline/module API; callers, callees,
  call chain, dependents, impact and dependency depth; test map and affected
  tests; session/message/current-as-of-evolution-forensic reads; and exact
  anchor expansion retain their distinct semantics and evidence;
- source edits retain preview/apply through the one journaled edit
  transaction, and Git index changes retain the paired preview/apply contract
  with no independently callable internal hunk/commit steps;
- feedback diagnostics, get, list, and exact expansion retain read-only
  diagnostics/impact, CI localization, ingested GitHub review, and agent
  proximity evidence with reversible oversized-response handling;
- task placement, local/GitHub stack status, dependency readiness,
  conflict/proximity, native-integration preview/apply/status/cancel, and
  receipt lookup retain path-free canonical identity and distinct read,
  preview, write, and control capabilities. The GitHub-stack request remains an
  explicit local handoff record with zero GitHub or Git invocation until its
  separately owned external operation exists;
- LSP retains explicit admitted standard methods, workspace/document
  lifecycle, document versions, cancellation, backpressure, analyzer
  conformance, diagnostics, navigation, and typed failure; no blind JSON-RPC
  proxy or local analyzer fallback is introduced;
- configuration, Doctor, rejected-argument observations, daemon health and
  saturation, progress streams, cursors, receipts, and safe output retain
  CLI/MCP/HTTP semantic compatibility; and
- PR17 retains every task graph, Kanban/DAG/timeline/causal/workload,
  decomposition/sizing/routing, workflow definition/run/control,
  provider-execution, topology, placement, integration, evidence, and
  SDK-facing semantic operation listed by Plans 24 and 32.

Rejected-argument observation remains one bounded dispatch event across
CLI, MCP, and HTTP. It may record only a cataloged or bounded unknown operation
class, normalized argument name that passes the safe grammar, stable rejection
class, schema/producer/transport/time/trace/idempotency metadata, and trusted
coarse provider/model-family/host kind when available. Values, positional
tokens, payloads, paths, hostnames, user identity, prompts, secrets, raw
errors, and reversible digests never enter the event. A client-side rejection
that cannot reach the daemon remains unreported coverage, not a fabricated
zero.

## PR12 user outcome

From CLI, MCP, or HTTP, a user can call the same authorized TraceDecay
operation and receive the same semantic result, stable problem, cursor,
receipt, progress, cancellation, and expansion behavior. Feedback diagnostics
and impact are real canonical reads rather than placeholders. In a negotiated
LSP host, the user also receives compact current diagnostics, impact, affected
tests, and test results through standard LSP methods and the owned TraceDecay
context extension without turning LSP into a generic tool proxy.

## PR12 end-to-end production path

1. A CLI command, MCP tool, HTTP operation, standard LSP method, or negotiated
   TraceDecay context request resolves the applicable Plan 08 binding and
   decodes into the same typed application request and `RequestContext`.
2. The daemon checks capability, exact project/user scope, generation,
   deadline, cancellation, and capacity, then calls the one cataloged
   application handler. No adapter opens a writable business store or supplies
   handler-local query, policy, or fallback behavior.
3. Canonical feedback diagnostics, impact, affected-test, test-result,
   get/list, and exact-expansion reads return real typed results. CLI, MCP, and
   HTTP bind the applicable readers in PR12; no advertised PR12 route delegates
   to a placeholder or permanent unavailable handler. Indexing is never a
   transport-level wait condition: operations use the latest complete
   compatible generation, preserve exact/lexical/graph behavior, and return
   typed provider freshness/coverage. Semantic results are omitted while their
   generation is indexing rather than delaying or failing the whole operation.
4. Plan 35 projects standard diagnostics and serves its versioned TraceDecay
   context extension over ordinary LSP/JSON-RPC framing after explicit
   experimental capability negotiation. Compact responses retain exact scope,
   content/graph generation, provider and coverage state, omissions, and
   opaque expansion handles; expansion reauthorizes against the canonical
   read. The gateway neither forwards arbitrary methods/payloads nor owns the
   underlying feedback, test, graph, or evidence data. CLI/MCP/HTTP/LSP parity
   includes the same non-blocking indexing semantics and selected-generation
   identity.
5. Each adapter renders the canonical result for its protocol. HTTP feedback
   parity is part of PR12. Dashboard binding and dashboard parity begin in
   PR14, not PR12.

## PR12 implementation slices

1. Bind representative read, write, administrative, streaming, and
   long-running application operations through the revisioned catalog and the
   canonical application invocation path, first proving CLI/MCP/HTTP semantic
   parity.
2. Bind the canonical diagnostics/impact feedback readers, affected tests,
   test results, get/list, and exact expansion as callable operations and
   delete placeholder/unavailable PR12 handlers.
3. Bind retained standard LSP methods and the negotiated, versioned TraceDecay
   context extension to the same handlers, including bounded envelopes and
   opaque authorized expansion.
4. Converge Markdown/JSON, semantic problems, stable cursors, progress,
   cancellation, receipts, daemon saturation, discovery, and rejected-argument
   observation across the callable surfaces.
5. Delete adapter-local business logic, shadow registries, blind JSON-RPC
   forwarding, and duplicate output/error contracts only after direct parity
   passes.

## PR12 direct acceptance

- Invoke representative canonical operations through CLI, MCP, and HTTP and
  compare typed results before rendering, including stable error, cursor,
  progress, cancellation, saturation, receipt, and Markdown/JSON semantics.
- Invoke real feedback diagnostics and impact readers through CLI, MCP, and
  HTTP; prove every advertised PR12 binding reaches the canonical application
  handler and no placeholder/unavailable implementation remains.
- Negotiate the TraceDecay experimental LSP capability in a real client,
  project current diagnostics, impact, affected tests, and test results, and
  expand an omitted item through its opaque handle. Assert exact scope,
  generation, provider, coverage, omissions, cancellation, stale-state
  suppression, and authorization recheck.
- Reject an unnegotiated extension call, unknown version, arbitrary method or
  payload forwarding, cross-scope handle reuse, hidden-resource enumeration,
  and any adapter attempt to persist or become authority for LSP/feedback data.
- Compare HTTP feedback semantics with CLI/MCP in PR12 while proving no
  dashboard adapter or dashboard-only handler is required before PR14.

## PR12 future boundaries

- PR13 registers callable GitHub-review, CI-localization, and proximity
  producers/contributions behind the same feedback readers and LSP provider
  extension point. It does not change reader transport, framing, output, or
  expansion semantics, and an uncallable producer remains a typed unavailable
  contribution rather than a reserved field.
- PR14 first binds dashboard actions and feedback reads and proves dashboard
  parity over the already callable application results.
- PR17 adds the task/work and runtime loop below through the same canonical
  application path;
  PR18 freezes supported public names and SDK bindings. Neither milestone
  turns LSP into arbitrary forwarding or makes a surface the data owner.

## PR17 user outcome

From a supported surface, a user can:

1. create versioned work;
2. retrieve exact TaskId-rooted evidence;
3. inspect an explained decomposition and provider recommendation;
4. explicitly accept or reject the proposal;
5. separately admit one provider step;
6. watch and resume progress, inspect artifacts and the terminal outcome, and
   cancel when legal; and
7. review a later replan that remains unapplied until another explicit
   decision.

CLI, MCP, HTTP, and dashboard present the same identities, evidence, legal
actions, and receipts. No client executes a provider, computes readiness,
chooses a hidden model, or mutates graph/runtime state locally.

## End-to-end production path

- Surface input is decoded into the same typed application request and
  RequestContext, including scope, capability, page, deadline, cancellation,
  expected version, and idempotency identity.
- The routed daemon is the sole mutable authority. A missing, incompatible, or
  saturated daemon returns a typed actionable problem; clients never open a
  writable business store or fall back to an embedded writer.
- The application performs graph/evidence/policy/configuration/runtime work and
  returns the canonical evidence, preview, effect, progress, or problem result.
- Each surface renders that result without querying another store, inferring
  missing state, changing terminality, or adding a fallback.
- Disconnect is not cancellation or rollback. A caller resumes with the
  canonical task/run/attempt/effect identity and inspects the daemon receipt.

## Surface contract

PR17 exposes one compact work family covering:

- work creation, detail, dependencies, derived readiness, comments/artifacts,
  assignment, handoff, and legal actions;
- Kanban, DAG, timeline, causal, critical-path, workload/capacity,
  executor/model, repository/delivery, evidence, and history projections over
  the same canonical selection;
- exact current/as-of/evolution/forensic evidence retrieval and anchor
  expansion;
- task-shape, calibrated sizing, decomposition/topology/route proposal,
  independent-review and optional-synthesis shape, and explicit decision;
- provider capability explanation and explicit run admission;
- workflow definition/version validation and lifecycle, progress/status/
  history/artifact/outcome inspection, approval, pause/resume/cancel/retry/
  reconcile, placement and integration control; and
- split/merge/resize/reroute/re-review/minimal-repair/restart/escalation replan
  review and explicit accept/reject/supersede.

Names used during PR17 are internal product bindings, not frozen SDK or
long-term public vocabulary. PR18 may improve names, but it cannot merge
distinct effect classes, hide semantic states, weaken authorization, or turn a
review into an implicit effect.

There is no generic invoke tool, task DSL, status setter, board filter
language, scheduler command bus, provider protocol proxy, shell/process tool,
arbitrary JSON payload, or Markdown-plan execution entry point.

## Canonical output behavior

- MCP defaults to compact Markdown and returns schema-valid structured content
  when supported. CLI defaults to deterministic human output; `--json` emits
  one canonical JSON object and newline.
- Human and structured output preserve the primary identity, versions,
  authority/scope class, temporal state, coverage, omissions, score kind and
  calibration validity, route explanation, requested/actual provider,
  continuation, effect class, cancellation stage, receipt, problem code, and
  legal next actions.
- Complete empty, partial, unknown, unavailable, unsupported, stale, redacted,
  denied, ambiguous, saturated, cancelled, timed out, failed, and effect
  unknown remain distinct.
- Pagination uses stable ordering and authenticated opaque cursors. Resume
  reauthorizes before cursor validation or hydration. Oversized responses use
  reversible handles, but TaskId and exact evidence anchors remain the durable
  identity.
- Missing, out-of-scope, policy-hidden, and profile-hidden resources or
  operations are externally indistinguishable until independently authorized.
  Responses reveal no hidden identity, count, cursor, provider state, timing,
  existence, or alternative binding.
- Terminal controls, Markdown, labels, paths, errors, provider output,
  artifacts, and logs contain no credentials, prompts, private source, or
  provider payloads. JSON is never
  double-encoded, and truncation is never irreversible.

## Progress, cancellation, and daemon capacity

Progress streams are bounded, ordered, resumable, and tied to the canonical
run/attempt frontier. Backpressure exposes a gap and fresh-status action rather
than silently dropping a suffix. Heartbeat is liveness, not completion.

Cancellation before admission returns the common pre-admission problem. After
admission, the daemon's operation/effect receipt is authoritative. If an effect
may have crossed its commit point, cancellation or a client timeout cannot
replace committed, partial, reconciling, or effect-unknown state.

Each client process reuses one multiplexed daemon connection. Admission
reserves capacity for health, diagnostics, status, cancellation, and receipt
inspection so bulk evidence reads cannot make the daemon unobservable.
Capacity exhaustion is a typed saturation result, never a raw broken pipe.

## Provider admission and control safety

Surfaces submit typed provider/backend/model/reasoning, sandbox/approval class,
bounded context references, budgets, deadline, cancellation, and opaque secret
references. They never submit a shell command, raw environment, arbitrary
executable, unbounded prompt, provider-local fallback, lease, or native
provider protocol frame.

The result shows the requested and actual provider/backend/executable/protocol/
model/reasoning selection and the explicit fallback decision. `Unsupported`,
`Absent`, `Stale`, `Cancelled`, `TimedOut`, `Failed`, `Partial`, and
`EffectUnknown` remain truthful. Free-form provider output cannot update a task
or recursively dispatch another provider.

Native provider approval is a distinct version-checked control tied to one
attempt and request. Timeout denies or cancels; it never approves.

## Explicit Git effects

Existing Git index mutation remains preview then explicit apply with immutable
CAS evidence and an idempotent receipt. PR17 optional integration follows the
same user-controlled shape: preview shows exact source/destination identities,
candidate effect, checks, conflicts, and expiry; apply requires an explicit
grant, current expected versions, and the native Git owner.

No surface accepts arbitrary Git arguments or exposes force update, rebase,
history rewrite, branch deletion, stash/clean/reset, automatic semantic
conflict resolution, or direct peer-worktree writes. A conflict or dirty/stale
destination is a typed non-success, not permission to improvise.

## Implementation slices

1. Bind work creation and exact evidence retrieval through one real surface,
   then prove CLI/MCP/HTTP/dashboard render the same application semantics.
2. Bind proposal review and explicit decision without introducing a
   surface-local task editor or route evaluator.
3. Bind provider admission, progress/history inspection, cancellation, and one
   supported real provider outcome.
4. Bind non-auto-applied replan review and, where supported, safe explicit Git
   preview/apply.

Each slice adds only the binding and renderer needed by its live application
operation. A binding record, profile, resource, schema, dispatcher entry, or
fixture does not land before its operation is callable.

## Replacement and deletion

- Delete handler-local store/query/policy/readiness/provider/retry/error logic
  as each application operation becomes live.
- Remove PR17 internal operation inventories, temporary cross-worktree
  registries, exact command/tool/type/file catalogs, generated parity
  manifests, shadow profiles, and declaration-only gates.
- Compatibility aliases with release evidence delegate to the canonical
  operation or return an actionable negotiated upgrade, but own no schema or
  behavior. Pure source-only and branch-era aliases are removed in place after
  internal callers move.
- Remove any client-side provider launch, hidden fallback, task status write,
  scheduler, or Git implementation.

## Direct acceptance

One end-to-end test drives the production loop through the selected surfaces:
create work, retrieve and expand exact evidence, receive an explained proposal,
explicitly accept it, separately admit a real provider step, resume bounded
progress, inspect its requested/actual route and terminal receipt, and review
an unapplied replan.

Focused failures cover missing/incompatible/saturated daemon, unauthorized or
hidden resources, stale graph/proposal/configuration/provider evidence, cursor
theft and expiry, partial and oversized evidence, cancellation before and
after effect commit, disconnect/reconnect, stream gaps, invalid fallback,
credential leakage and terminal-control cases, recursive-dispatch attempts, dirty or
conflicting Git preview, CAS drift, duplicate apply, and effect-unknown
reconciliation.

Direct parity tests run through the ordinary aggregate repository checks,
compare semantic JSON and required human fields across CLI, MCP, HTTP, and
dashboard, prove adapters open no business store or provider/runtime path, and
prove old handler logic is deleted. They create no separate acceptance gate and
do not require byte-identical prose, a generated inventory, giant Cartesian
fixture matrix, or PR17 public SDK names.

## Not in PR17

- PR18 freezes public API, CLI/MCP compatibility, and SDK names.
- PR20 optimizes measured surface and daemon latency.
- A blind LSP/JSON-RPC proxy, generic process execution, developer-plan
  executor, and JavaScript workflow runtime remain out of scope.
