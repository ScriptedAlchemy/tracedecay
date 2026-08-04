# Official Public API and SDKs

## Status / role

PR12 ships the official daemon API. PR17 adds accepted task/work graph and
workflow-runtime application operations. PR18 stabilizes every supported
public operation, including the PR12 base and all later accepted additions, and
publishes working Rust and TypeScript SDKs. No operation family is deferred. The
originally planned Python SDK was dropped: delivery is TypeScript-first plus a
retained Rust SDK for native consumers, with no Python package.

Earlier operation inventories, compatibility matrices, generated declarations,
package fixtures, and conformance packets are historical evidence, not
prerequisites or artifacts that PR18 must recreate. Only actually
independently released public operation names, wire schemas, and SDK APIs may
retain protocol compatibility. Persisted cursors and receipts use the V2
fresh-store rule; all other retention is judged by the direct cross-language,
lifecycle, platform, and regression behavior below.

PR18's first Rust and TypeScript package shapes are not yet published;
branch acceptance and generated schemas do not create an older supported SDK
contract. Branch-local V2 request/response APIs change in place. Persisted
cursors, idempotency keys, journals, checkpoints, and receipts accept only
their exact final shape; any other database, store, spool, file, or projection
returns typed `ResetRequired` and requires explicit reset or recreation. No
storage reader, migration, backfill, dual write, or census path exists. Public
package protocol compatibility starts only at an actual independent release.

## User outcome

An external developer can install any supported SDK, connect to the local
daemon, call every supported public operation with behavioral/lifecycle parity,
and complete the same accepted PR17 journeys:

- use project, source, symbol, graph, retrieval, session/LCM, memory,
  configuration, health/Doctor, feedback, test, diagnostics, edit, Git,
  lifecycle, observability, and every other supported public family;

- create and version initiatives and work items, manage dependencies, page
  projections and history, and inspect assignment/review state;
- request and review task-shape, decomposition, routing, resize/re-route,
  independent-review, outcome, and calibration results, including explicit
  abstention or insufficient-evidence outcomes; and
- inspect provider capabilities, admit an authorized attempt, consume ordered
  progress and artifacts, cancel it, reconnect or resume when supported, and
  obtain its canonical terminal receipt.

The documentation examples perform these journeys against a real daemon. The
SDKs are not generated-type packages that leave users to construct raw
requests.

### Supported public operation coverage

Every Rust and TypeScript SDK exposes every supported public
operation, not only the PR17 additions, with distinct IDs and typed legal
actions. The base includes all callable PR12 families and every operation
accepted before the PR18 freeze; the PR17 additions include:

- initiative, work-item, and version creation/update/read;
- dependency mutation and readiness, history, projection, Kanban/DAG/timeline/
  causal/workload paging;
- assignment, review, task-shape assessment, decomposition proposal and review,
  routing recommendation, live split/merge/resize/re-route proposal,
  independent-review grade, outcome attribution, and calibration;
- auxiliary-attempt request and its relationship to parent work;
- provider capability/discovery/negotiation, requested-versus-actual provider/
  backend/model/protocol identity, admission, lease/attempt state, ordered
  progress/events/artifacts, cancellation escalation, receipt, reconnect,
  resume, and restart recovery; and
- deterministic fallback, abstention, unavailable, partial, censored, unknown,
  and insufficient-evidence outcomes carried by the owning operation.

No SDK computes readiness, scoring, proposal acceptance, provider selection,
scheduling, completion, or calibration independently.

## End-to-end production path

1. An SDK negotiates protocol and capability revisions, authenticates, selects
   an authorized project/profile, and submits a typed operation.
2. The public daemon boundary maps the request to the same canonical
   application operation used by CLI, MCP, and HTTP. The daemon remains the
   only process that authorizes, reads or writes product storage, schedules
   work, and records receipts.
3. The SDK exposes the operation through an idiomatic façade and returns the
   canonical value, page, stream event, legal action, structured error, or
   unavailable/partial outcome without adding business decisions.
4. Long-running calls preserve ordered progress, bounded backpressure,
   timeouts, cancellation, reconnect/resume, and one canonical terminal
   outcome. Every problem preserves exactly the Plan 09 retry directive
   `Never | SameRequest | AfterDelay | AfterRevalidate | AfterReconcile`; SDKs
   never infer retry from transport or status.
5. Responses, stream items, errors, diagnostics, logs, and fixtures contain no
   credentials, prompts, private source, or provider payloads. Credential
   references stay opaque and are resolved only by the daemon for the
   authorized operation.

Equivalent SDK, CLI, MCP, and HTTP calls have equivalent authorization,
meaning, stable error codes, redaction, effects, and lifecycle behavior even
when their syntax is idiomatic to the surface.

### Compatibility and lifecycle behavior

Each accepted operation proves semantic and lifecycle parity across CLI, MCP,
HTTP, Rust, and TypeScript. Compatibility with an older shape is tested when
actual independent release evidence establishes that public protocol shape;
the documented release support policy governs its conformance.
Conformance covers supported syntax,
protocol/capability range, required authorization, paging or stream shape,
retry class, cancellation support, reconnect/resume behavior, stable errors,
and explicit transport limitations through callable adapters. A generated or
manually maintained matrix is optional implementation evidence, not a product
requirement or acceptance artifact.

After the first evidenced publication, additive changes within a major
protocol version preserve compatibility and breaking changes negotiate a new
major version with an actionable error. Before publication, a transient
request/response API changes in place. Persisted V2 state accepts only its
exact final shape; any other shape returns `ResetRequired` and requires
explicit reset or recreation, never a reader or conversion.
The policy retains the full required/optional field, default, nullability, open
object, union/enum, numeric narrowing, identifier, error, stream-event, cursor,
operation rename/removal, retry-class, and capability-removal behavior.
Unknown fields, enums, errors, and events are handled consistently for the
negotiated version; retired identifiers and codes remain reserved.

Cancellation retains typed requested, accepted, before-start,
publication-suppressed, upstream-acknowledged, execution-stopped,
too-late/committed, unsupported, failed, and terminal outcomes. It never
promises rollback of committed effects or exactly-once transport delivery.
Reconnect/resume proves operation and stream identity before continuing and
cannot duplicate a committed mutation.

Effective capability remains the intersection of client support, gateway
guarantee, upstream capability, admitted project/language, policy/configuration,
and active profile, all bound to explicit revisions. The authenticated
bidirectional daemon session used by Plan 35 negotiates protocol, catalog,
project, and client revisions before document content; preserves ordered
events, cancellation, backpressure, reconnect, and bounded terminal errors;
and does not expose an arbitrary LSP or daemon invocation tunnel through SDKs.

PR18 freezes two distinct public handoff-token consumption operations and
exposes each through Rust and TypeScript:

- `open_investigation_handoff` consumes a feedback/diagnostic-cue token and
  delegates to Plan 09's owning investigation application operation/result;
- `open_task_handoff` consumes a ready-commit/cross-worktree/task-cue token and
  delegates to the owning application operation over Plan 24 task identity,
  version, authorization, and context semantics.

After explicit capability negotiation, Plan 35 may project an LSP action for
either kind. Each opaque token is 60-second, single-use, kind- and
destination-bound, and bound to the exact session, project/root, cue/finding
or task version, authorization/policy epoch, and local daemon authority
identity. Consumption reauthenticates, reauthorizes exact scope, checks kind,
destination, expiry, use state, and current owner version, then returns only
the owning operation's open-surface result.

Missing, wrong-kind, wrong-destination, wrong-scope, expired, already-used,
revoked, unauthorized, and policy-hidden tokens use a policy-safe
non-enumerating unavailable shape unless the caller is independently
authorized to receive a narrower reason. Possession grants nothing. Tokens
carry no edit, task body, source, raw path/ID/query/arguments, credential, or
durable evidence. Consumption cannot invoke `workspace/applyEdit`, Git,
provider execution, work mutation, or an arbitrary daemon/LSP method.

## PR18 implementation defaults

- Use Serde plus `schemars` as the accepted wire-model source. Admit Aide only
  after typed DTOs exist and only when it deletes route/OpenAPI glue without
  creating parallel request, response, error, or lifecycle models.
- Generate TypeScript wire models, then build reviewed handwritten
  lifecycle façades over Rust `reqwest` and browser/Node `fetch`.
  Generation replaces hand-copied DTOs; the façades retain
  idiomatic paging, streaming, backpressure, cancellation, retry directives,
  idempotency, reconnect/resume, problems, and receipts rather than generating
  client-side product behavior.
- Use `oasdiff` and ecosystem semver tooling to detect structural and published
  compatibility changes, then keep direct behavioral/lifecycle tests as the
  authority. If Aide or generated bindings lose an accepted union, error,
  stream, or cancellation semantic, reject that path or repair the handwritten
  façade; do not weaken the operation or accept schema/compilation parity as
  conformance.

## Implementation slices

### Stabilize every supported public operation

- Choose public names and request/response shapes at the daemon application
  boundary for every supported operation, including all PR12 base families and
  the accepted PR17 additions.
- Preserve names proven by an actual independent public release as
  compatibility aliases that delegate to the canonical operation. Pure
  source-only and branch-era callable names are replaced in place. A
  compatibility name may translate syntax but
  owns no readiness, scoring, routing, scheduling, provider selection,
  storage, or lifecycle logic.
- Define pagination cursors, stream events, cancellation and retry classes,
  structured errors, version negotiation, and unavailable/partial outcomes in
  the operation that uses them. Schema/OpenAPI generation is package input,
  not an independently accepted deliverable.
- Bind public schema/OpenAPI, routes, MCP tools, CLI commands, adapters, and
  generated bindings directly to the one daemon application contract source.
  Syntax may differ; semantics and lifecycle may not.
- Bind both `open_investigation_handoff` and `open_task_handoff` to their
  owning application operations and expose the same authorized,
  non-enumerating result in Rust and TypeScript without a raw LSP
  tunnel. Plan 35 owns transport projection, Plan 09 owns investigation
  results, and Plan 24 owns task semantics; PR18 owns only the public names,
  token-consumption contract, SDK bindings, and compatibility.

### Publish two usable SDKs

- Ship Rust and TypeScript packages with authenticated connection
  setup, typed operation calls, pagination iterators, streaming, cancellation,
  timeout, retry/idempotency, reconnect/resume, and structured errors idiomatic
  to each ecosystem.
- Generated low-level bindings may be used internally, but reviewed façades
  provide complete journeys and contain no generic invocation tunnel or local
  product logic.
- SDKs never accept shell strings or raw process environments and never execute
  Claude Code, Codex, or provider binaries locally.

### Prove and document real use

- Publish executable quickstarts that cover every public capability family,
  plus complete work-graph, admitted-runtime, investigation-handoff, and
  task-handoff journeys in both languages.
- Test installed packages against one released daemon build on Linux and
  Windows, including current and oldest-supported client/daemon combinations.
- Publish package versions only after the examples and lifecycle tests pass
  against the same daemon artifact.

## Replacement and deletion

PR18 removes source-only temporary PR17 spellings directly. Callable PR17
spellings change in place unless release or live-host evidence establishes a
predecessor; aliases then follow that explicit compatibility disposition.
Branch sequencing alone creates no public compatibility window.
It also removes duplicate surface-specific models, generated-type-only sample
packages, and any SDK-side business or retry decision that competes with the
daemon. Stable compatibility aliases remain thin delegates.

Contract drift is detected by running real operations through adapters and
SDKs. Generated inventories, declaration-parity scorecards, schema-only test
matrices, and compilation-only conformance are not retained as release gates.

## Direct acceptance

- Every supported public operation is callable from Rust and TypeScript
  against the same production daemon boundary; no operation family is
  omitted because it predates PR17, and no SDK operation bypasses daemon
  authorization or opens product storage.
- Each Rust and TypeScript SDK runs representative read, paged, streamed,
  cancellable, and effect/receipt operations against the local daemon.
- Local calls exercise authentication, disconnect, reconnect/resume, paging,
  streaming/backpressure, cancellation before and after an effect commit point,
  and partial/unavailable authority as applicable.
- In each language, an executable journey creates and pages work, observes
  legal proposal/review outcomes, admits a provider-backed attempt, consumes
  progress, exercises cancellation, reconnects or resumes where supported, and
  reads the terminal receipt.
- Cross-surface assertions compare semantic values, stable errors, legal
  actions, redaction, effects, and terminal outcomes rather than generated
  declarations.
- Behavioral/lifecycle conformance for every operation covers its applicable
  request defaults, authorization, scope, paging/streaming, coverage,
  redaction, stable problem plus exact retry directive, idempotency/effect
  receipt, cancellation, reconnect/resume, and unavailable/partial states
  through Rust and TypeScript.
- Stateful fixtures cover task/work versioning, dependencies, paged
  projections/history, assignment/review, assessment/proposal/recommendation/
  outcome/calibration semantics, abstention and deterministic fallback,
  auxiliary request/provider negotiation, requested-versus-actual identity,
  runtime mapping, SSE/stream history, artifacts, and every terminal outcome.
- Pagination, streaming, backpressure, cancellation before and after commit,
  bounded retry, durable idempotency, reconnect/resume, stale revision,
  authentication failure, provider absence, partial coverage, and
  insufficient evidence are exercised through every SDK.
- Current and oldest-supported client/daemon combinations run schema-derived
  positive/negative cases, hand-authored stateful lifecycle fixtures,
  installed-package smoke tests, and executable documentation. Generated
  bindings and idiomatic façades share those fixtures.
- The Plan 35 session journey covers negotiation, ordered bidirectional
  delivery, cancellation, backpressure, reconnect, stale revisions, and
  authentication without exposing a raw LSP tunnel. Each Rust and TypeScript
  SDK produces and consumes both token kinds through the local daemon: a
  feedback/diagnostic cue opens the owning investigation surface, and a
  ready-commit/cross-worktree/task cue opens the owning task surface.
- Handoff failures cover wrong kind/destination/session/project/root/cue/task
  version/authority, expiry, replay, authorization or policy revocation,
  partial/unavailable authority, and unsupported clients. They preserve
  policy-safe non-enumeration and never return task bodies through LSP, mutate
  work, apply edits, invoke arbitrary methods, or create a fallback authority.
- Linux and Windows package-install and documentation examples pass for both
  languages, followed by the ordinary aggregate repository checks rather
  than a separate acceptance gate.

## Not in PR18

- New task/work or provider-runtime semantics; PR17 owns them.
- A JavaScript workflow runtime, arbitrary daemon/LSP payload tunnel, local
  provider executor, or direct database API.
- Generated compatibility inventories, standalone conformance services,
  planning ledgers, placeholder baselines, or publication based only on schema
  generation or package compilation.
