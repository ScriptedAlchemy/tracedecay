# Official Public API and SDKs

## Status / Role

- Required V2 product surface.
- PR12 delivers the official daemon API.
- PR17 extends that API with Plan 24 task/work graph and Plan 32 runtime
  operations through internal typed contracts and then-supported adapters; it
  does not publish generated clients or SDKs.
- PR18 stabilizes that API and fully delivers supported Rust, TypeScript, and Python SDKs.
- The end state is complete across all three SDKs; no language binding is deferred or skipped.

## Outcome

Agents and applications use one supported daemon API to access TraceDecay capabilities.
CLI, MCP, HTTP, and SDK adapters expose the same operations, validation, errors, and privacy behavior.
The daemon remains the only process that reads or writes product storage.

## Owns

- The public daemon protocol, versioning policy, and compatibility rules.
- Executable request, response, pagination, streaming, and error contracts.
- Rust, TypeScript, and Python client libraries.
- Authentication and connection mechanics for local and remote daemon clients.
- Cancellation, backpressure, idempotency, and retry-safe operation metadata.
- The versioned authenticated bidirectional daemon-session contract used by
  [Plan 35's](35-daemon-lsp-gateway-and-universal-diagnostics.md) transport-only
  LSP bridge.
- Direct parity tests across CLI, MCP, HTTP, and all SDKs.

## Does not own

- Domain rules, query semantics, privacy policy, configuration semantics, or storage implementation.
- Direct database access from clients or language bindings.
- LSP JSON-RPC framing, document lifecycle, analyzer supervision, or an
  SDK-visible arbitrary LSP payload tunnel.
- A generated compatibility inventory or a second model of the product.
- Markdown/developer-roadmap parsers, rewrite trackers, independent task
  executors, or workflow JavaScript. Typed Plan 24/32 product operations are
  part of the supported API.
- Dynamic workflow execution. PR17 stores typed workflow definitions and invokes existing daemon
  operations; it does not introduce a JavaScript SDK or runtime.

## Required behavior

1. One actual contract source
   - Public request and response types live at the daemon application boundary.
   - Routes, MCP tools, CLI commands, schemas, and SDK bindings map directly to those types.
   - An adapter may change syntax, never meaning.

2. Complete operation parity
   - Every supported public operation declares its availability across CLI, MCP, HTTP, and SDKs.
   - Unsupported transport behavior is an explicit contract decision, not an accidental omission.
   - Equivalent calls return equivalent values, stable error codes, and the same redaction outcome.

3. Daemon authority
   - Clients connect to the daemon and never open TraceDecay databases.
   - The daemon owns authorization, transaction boundaries, migrations, concurrency, and recovery.
   - Connection loss, cancellation, and retries cannot duplicate committed mutations.

4. Stable protocol
   - Additive changes preserve compatibility within a major version.
   - Breaking changes require a new major protocol version and an actionable negotiation error.
   - Unknown fields are handled consistently and documented per protocol version.
   - Compatibility policy classifies required/optional fields, defaults,
     nullability, open objects, unions/enums, numeric narrowing, identifiers,
     errors, stream events, cursors, operation rename/removal, retry class, and
     capability removal. Unknown enum, error, and event behavior is explicit;
     retired identifiers and codes remain reserved.
   - The LSP bridge session negotiates protocol, catalog, project, and client
     revisions before document content is accepted, and preserves ordered
     bidirectional events, cancellation, backpressure, and bounded terminal
     errors without exposing arbitrary daemon invocation.

5. Executable conformance and retry contracts
   - Structural conformance covers schemas and generated types. Semantic
     conformance covers operation identity, authorization, redaction, coverage,
     legal actions, and receipts. Lifecycle conformance covers negotiation,
     ordering, progress, backpressure, cancellation, reconnect/resume,
     saturation, and exactly one canonical terminal outcome.
   - Each operation declares `Never`, `SafeRead`, `IdempotentWithKey`, or
     `ResumeOnly`. SDKs auto-retry only the declared classes under bounded
     policy and retain durable idempotency receipts; non-idempotent mutations
     are never silently retried.
   - Cancellation exposes typed requested, accepted, before-start,
     publication-suppressed, upstream-acknowledged, execution-stopped,
     too-late/committed, unsupported, failed, and terminal outcomes. It never
     promises rollback of committed effects or exactly-once transport
     delivery.
   - Effective capability is the intersection of client support, gateway
     guarantee, upstream capability, admitted project/language,
     policy/configuration, and active profile, all bound to explicit revisions.

6. Usable SDKs
   - Rust, TypeScript, and Python expose typed sync or async APIs idiomatic to each ecosystem.
   - Pagination, streaming, cancellation, timeouts, and structured errors are first-class.
   - SDKs provide connection setup and operation calls, not independent business logic.
   - PR18 SDKs expose Plan 24 initiative/work-item/version, dependency,
     history/projection, assignment/review, task-shape assessment,
     decomposition proposal/review, routing recommendation, live
     resize/re-route proposal, independent-review grade, outcome/calibration,
     Plan 24 auxiliary-attempt request, Plan 32 provider capability,
     admission/progress/cancel/receipt, and other runtime operations with
     distinct IDs and typed legal actions.
     PR18 chooses and stabilizes idiomatic public names over PR17 application
     semantics; planning prose does not freeze generated method spellings. No
     SDK computes readiness, scoring, proposal acceptance, provider selection,
     scheduling, or completion; accepts shell strings/raw environment; or
     executes Claude Code/Codex locally.

7. Safe output
   - Privacy enforcement runs before every public response, stream item, log, and diagnostic payload.
   - Credential material remains opaque and is never returned by read APIs.

## Acceptance

- PR12 ships a versioned daemon API backed by the real application contracts.
- CLI, MCP, and HTTP parity tests cover every public operation and stable error code.
- PR18 ships usable, documented, tested Rust, TypeScript, and Python SDKs.
- PR17 API fixtures cover task/work graph versioning, paged projections,
  assessment/proposal/recommendation/outcome/calibration semantics, abstention
  and deterministic fallback, auxiliary request/provider negotiation,
  requested-versus-actual backend/model identity, typed progress/events/
  artifacts/terminal outcomes, runtime mapping, cancellation, resume/reconnect,
  and SSE history; PR18 runs the same fixtures through all three SDKs.
- Plan 32 PR17 completion never depends on SDK generation or parity. PR18 owns
  public schema/OpenAPI stabilization, client generation/publication,
  documentation, and Rust/TypeScript/Python conformance for workflow and
  task/work and auxiliary-provider operations. PR17 semantic concepts and
  internal IDs do not freeze public method/tool/route names before this gate.
- The three SDK suites pass the same contract fixtures against one daemon build.
- Release gates run current and oldest-supported client/daemon combinations,
  schema-derived positive and negative cases, hand-authored stateful lifecycle
  fixtures, generated-package smoke tests, and executable Rust, TypeScript, and
  Python documentation examples. Schema generation or compilation alone is
  not semantic conformance.
- Generated low-level bindings and reviewed idiomatic façades share the same
  contract fixtures; façades adapt paging, streams, cancellation, and errors
  but contain no product decisions or generic invocation tunnel.
- Cancellation, reconnect, idempotent retry, pagination, and streaming tests pass.
- LSP bridge contract tests cover negotiation, ordered bidirectional delivery,
  cancellation, backpressure, reconnect, stale revisions, and authentication
  without adding a raw LSP tunnel to the public SDKs.
- A client cannot open product storage or bypass daemon authorization and privacy enforcement.
- Contract drift is detected by executable adapter and SDK tests, not generated inventory files.
