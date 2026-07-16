# TraceDecay V2 CLI, MCP, LSP, and Output Unification

**Delivery:** PR 12

**Status:** planned product work
**Depends on:** [08 tool catalog](08-tool-catalog-crate.md), [09 application](09-application-crate.md), [18 privacy](18-secret-detection-redaction-and-private-data-safety.md), and [20 configuration](20-configuration-control-plane.md). PR12 ships Plan 35's explicitly resolved single-project LSP admission; [16 scope](16-cross-project-repository-worktree-scope.md) extends it to canonical multi-root admission in PR15 and is not a PR12 prerequisite.

## Outcome

CLI and MCP are thin clients over the same daemon-owned application use cases.
LSP is a stateful sibling adapter over the same typed code and diagnostic
contracts. CLI and MCP render typed results as compact human output or canonical
JSON; LSP uses protocol-native JSON-RPC responses and diagnostics.

This plan does not create a generated surface inventory, plan parser, command generator, parity matrix, task editor, or workflow executor.

## Boundaries

- The routed `tracedecayd` is the sole application and database authority for
  each selected mutable shard; PR16 preserves exactly one fenced daemon
  authority per shard.
- `tracedecayd` owns the LSP gateway, session state, routing, diagnostics, and
  analyzer lifecycle defined by Plan 35. The stdio bridge contains only
  authenticated framing and transport logic.
- CLI and MCP resolve transport input, call the daemon, and render the response. They never open a business database or run query, policy, migration, or repair logic locally.
- The stdio bridge never opens a business database, starts an analyzer, or
  implements admission, routing, merge, privacy, or fallback policy.
- A missing or incompatible daemon fails closed with one actionable problem. No client silently falls back to an embedded writer.
- HTTP routing, extraction, and encoding remain owned by
  [10](10-api-crate.md); public protocol versioning and SDK bindings remain
  owned by [17](17-official-public-api-and-sdks.md). Shared schema and binding
  references do not transfer adapter lifecycle ownership. Dynamic workflow
  semantics remain owned by [32](32-dynamic-workflow-runtime-and-sdk.md).
- Developer plans and Markdown files are documentation, not runtime input.

## Typed application result

Every exposed use case returns a sealed result containing:

- the requested data or command receipt;
- resolved scope and snapshot identity;
- coverage, freshness, redaction, and partial-state information;
- stable pagination or retrieval anchors where the result is resumable;
- typed warnings, failures, retry guidance, and legal next actions.

Renderers cannot query stores, infer missing state, mutate results, or traverse arbitrary `serde_json::Value`. Domain-specific views may define compact presentation, but JSON always serializes the semantic result rather than a presentation document or transport wrapper.

## Output contract

- MCP defaults to compact Markdown in text content and returns schema-valid structured content when the protocol supports it.
- CLI defaults to deterministic human output. `--json` emits canonical JSON only.
- JSON is never embedded as an escaped string inside another JSON result.
- Empty, partial, unavailable, denied, stale, redacted, ambiguous, pending, and failed remain distinct.
- Collections use stable ordering and an opaque cursor. Truncation either returns a resumable anchor or a typed budget error.
- Human output keeps identifiers, coverage, blockers, and next actions needed to continue safely.
- Terminal controls, Markdown, paths, labels, and errors pass the shared output-safety boundary.
- Normal results use stdout; diagnostics use stderr; exit classes are stable and tested.

## CLI adapter

PR 12 consolidates shared input handling for scope, format, pagination, and daemon connection without rebuilding the command tree from Markdown or an inventory file. Commands call explicit application methods and map typed problems to stable exits.

Help stays concise and task-oriented. Deprecated aliases may have a bounded compatibility shim, but aliases do not retain separate semantics or implementations.

## MCP adapter

Use one protocol implementation with isolated per-connection lifecycle, authentication, negotiated capabilities, cancellation, and backpressure. Advertise only features that are implemented and directly tested.

MCP tools and resources call the same explicit application methods as CLI. MCP task IDs, retrieval anchors, sessions, and workflow IDs remain distinct types. MCP clients cannot choose hidden bindings or bypass authorization through a generic invoke tool.

Multiple MCP clients may connect concurrently, but all business reads and writes are brokered through the owning daemon authority established locally by PR4 and generalized per shard by PR16.

## LSP adapter

LSP is stateful and protocol-native; it does not use canonical CLI/MCP
rendering. The daemon gateway owns initialization, shutdown, capability
negotiation, workspace and document lifecycle, document versions, cancellation,
notifications versus responses, bounded queues, and backpressure.

Each supported standard method resolves through an explicit catalog binding to
a typed application/query operation. No adapter or bridge may blindly proxy an
unknown method or arbitrary JSON-RPC payload. Notifications cannot satisfy
pending requests, and cancelled or superseded document versions cannot publish
results.

A missing, incompatible, or failed daemon produces one bounded protocol-native
startup or session failure; the bridge starts no local fallback. Conformance
fixtures compare navigation, diagnostics, lifecycle, cancellation, ordering,
and failure semantics with direct use of each supported upstream analyzer,
allowing only documented TraceDecay provenance, bounds, and exact graph
augmentation.

## Canonical dispatch and tool families

One typed schema registry and canonical binding taxonomy maps CLI, MCP, HTTP,
and LSP names to cataloged application operations. The CLI/MCP dispatcher,
Plan 10 HTTP router, and Plan 35 LSP gateway consume those references through
their own protocol adapters. Bindings may validate transport syntax and render
protocol-native results; aliases contain zero authorization, query, mutation,
storage, availability, or fallback logic.

- `search`, `find_exact`, qualified-name lookup, similar-symbol lookup, and
  signature search are views over one symbol kernel.
- `read`, `outline`, `module_api`, `signature`, and file views share one
  source/outline kernel.
- `callers`, `callees`, `callers_for`, call chains, file dependents, and impact
  share one graph-traversal kernel; implementation and type-hierarchy names are
  typed graph views, not separate engines.
- `test_map` and `affected` share one test-attribution kernel.
- Exact, symbol, insert, move, and structural rewrites use the one journaled
  application `EditTransaction`; preview/dry-run never means a second edit path.
  Plan 35's `prepareRename` and `rename` bind only to read-only
  candidate/preview UseCaseIds; they never bind directly to
  `tracedecay_rename_symbol`, API-migration apply, another write-effect entry,
  `workspace/applyEdit`, or opaque server commands. Plan 34's immutable
  preview/manifest and `EditTransaction` remain the only apply path. General
  LSP `textDocument/codeAction` is deferred from PR12 and cannot ship until a
  separate owner defines typed candidate consumption, policy classification, a
  canonical preview/`EditTransaction` route, and acceptance fixtures.
- Git index mutation exposes exactly two public operations on both CLI and
  MCP: `git_preview` and `git_apply`. They share one typed schema and call the
  PR11 daemon-owned `GitIndexTransaction`; adapters cannot invoke its internal
  `stage_hunks`, `unstage_hunks`, or `commit_index` steps independently.
  `git_preview` returns selected hunks, intended effect class, CAS evidence,
  and an immutable transaction digest without locking or mutating the index.
  `git_apply` requires that preview identity, acquires the real index lock,
  revalidates CAS state, and returns an idempotent receipt or a typed stale,
  conflict, lock-contended, denied, or invalid-effect result. Neither operation
  is generic Git execution or permits autonomous merge, rebase, cherry-pick,
  branch/tag/ref mutation, or history rewriting.

Literal grep, AST structural match, body source, graph node records, and context
composition remain distinct because their evidence and semantics differ.
`diagnose` and `diagnostics` remain distinct effects. Project/runtime/storage,
memory, LCM, and daemon health views remain distinct evidence domains even when
their bindings share the dispatcher.

The explicit read-only feedback diagnostics surface binds once here at PR12
across CLI/MCP/HTTP and at PR13 through host adapters defined by
[Plan 27](27-cross-host-agent-plugin-bundles.md). Operations are
`feedback_diagnostics`, `feedback_get`, `feedback_expand`, and
`feedback_list` — advisory, read-only views over
[Plan 09](09-application-crate.md)'s PR11 feedback-cycle result and finding
lifecycle; they are not degraded LSP methods and do not gain duplicate
transport-specific implementations. CLI/MCP canonical JSON and compact
Markdown preserve the same semantics, finding IDs, coverage/state, source
provenance, and continuation metadata. Collections use
[Plan 05](05-query-crate.md) stable opaque cursors; durable expansion resolves
[Plan 13](13-research-provenance-and-context-anchors.md)
`RetrievalAnchorId`s. Oversized transport output uses the existing reversible
response-handle path (`tracedecay_retrieve`) with explicit original count,
returned/preview count, handle, expiry, and typed unavailable/budget errors.
Response handles are never durable finding IDs. Ingested GitHub review threads
first surface through PR13 host adapters; PR17 optional workflow composition
does not gate these bindings.

## Rejected-argument telemetry

The versioned schema registry and dispatcher own one
`interface_argument_rejected.v1` event for CLI, MCP, and HTTP. It is emitted
at the authoritative schema/dispatch rejection boundary, after syntax has
been separated into argument names and values and before the typed problem is
rendered. Adapters do not keep private counters or infer rejection telemetry
from stderr, protocol error text, or logs. A client-side rejection that cannot
reach the daemon is represented in telemetry coverage as unreported rather
than silently counted as zero.

The event contains only:

- the cataloged tool or command identity, or a bounded `unknown_operation`
  class when dispatch could not resolve one;
- normalized rejected argument names and the stable error class, such as
  unknown, misspelled, removed, misplaced, duplicate, or invalid shape;
- schema identifier and version, producer revision, transport, event time,
  trace identifier, and an idempotency key;
- normalized provider, model family, and agent-host kind when explicitly
  available from trusted connection metadata, with absence kept distinct
  from unknown.

Names are extracted without their prefix/value separator and must pass the
bounded argument-name grammar before recording. `--key=value` can record
`key`, never `value`; positional tokens, raw request payloads, error messages,
environment values, paths, hostnames, user identifiers, prompts, and provider
content are never copied. A name that fails privacy or grammar checks becomes
a stable rejection category plus a redacted-name count, not a raw token or a
reversible digest. The event path applies the shared privacy policy before
enqueue and is bounded, non-blocking, and explicit about dropped events.

Aliases are resolved only after the attempted spelling has been safely
classified, so future alias or schema decisions can compare a rejected name
with the active canonical schema without changing dispatch behavior. Event
emission cannot make an invalid request valid, alter its error, add a retry,
or delay the response. Aggregation and product read models are owned by
[26](26-observability-accounting-and-usage.md).

## Direct parity tests

Parity is verified from public behavior, not from a generated inventory:

1. invoke the same use case through CLI and MCP;
2. decode both canonical JSON results and compare semantic fields;
3. verify compact human output preserves identity, coverage, blockers, and continuation;
4. test missing daemon, stale client, denied scope, empty, partial, redacted, paged, oversized, cancelled, and failed states;
5. run concurrent-client tests proving clients never open writable databases;
6. test stdout, stderr, exit codes, MCP lifecycle, framing, cancellation, and reconnect behavior directly;
7. submit equivalent unknown, removed, misplaced, duplicate, and invalid-shape arguments through CLI, MCP, and HTTP and assert one schema-identical rejection event per attempt;
8. prove values, payloads, paths, hostnames, identifiers, prompts, secrets, and unsafe names never enter events, logs, or typed problems, while redacted and dropped-event coverage remains visible;
9. verify replay/retry idempotency, unavailable provider/model/host metadata, bounded name/cardinality limits, and daemon-unavailable client rejection behavior;
10. run LSP lifecycle, negotiation, document-version, cancellation,
    notification/response separation, backpressure, daemon-failure, and direct
    upstream semantic-parity fixtures.
11. invoke `git_preview` and `git_apply` through CLI and MCP and compare their
    canonical semantic results, then prove compact Markdown and JSON preserve
    the same transaction identity, selected hunks, effect class, receipt, and
    typed stale/conflict state;
12. exercise concurrent index changes, real index-lock contention, preview CAS
    drift, retry idempotency, and forbidden generic/history-changing requests;
13. invoke `feedback_diagnostics`, `feedback_get`, `feedback_expand`, and
    `feedback_list` through CLI and MCP and compare canonical JSON semantics,
    then prove compact Markdown preserves finding IDs, coverage/state,
    continuation cursors, Plan 13 anchor references, and typed
    unavailable/budget/truncation outcomes;
14. prove oversized feedback results return reversible `tracedecay_retrieve`
    handles with original/preview counts and expiry while response handles
    remain distinct from durable finding IDs; security fixtures prove handles
    cannot bypass authorization or substitute for anchors.

## PR 12 deliverables

- one daemon client shared by CLI and MCP adapters;
- one daemon-owned stateful LSP gateway and transport-only stdio bridge;
- sealed application-result serialization;
- compact Markdown/terminal presenters for shipped use cases;
- canonical JSON and cursor/anchor handling;
- stable problem and exit mapping;
- canonical privacy-safe rejected-argument emission at the shared dispatcher;
- removal of handler-local database/query behavior, raw JSON renderers, double encoding, irreversible truncation, and writable fallback;
- removal of the `admin_cli` registry and session/analytics handler copies;
- focused CLI/MCP parity and concurrency tests.
- exactly `git_preview` and `git_apply` as shared-schema CLI/MCP Git bindings,
  with preview/CAS enforcement, typed receipts, and stale/conflict parity;
- read-only feedback diagnostics bindings (`feedback_diagnostics`,
  `feedback_get`, `feedback_expand`, `feedback_list`) with Plan 05 cursors,
  Plan 13 anchor expansion, and `tracedecay_retrieve` truncation handles.

## Done

- One typed application result drives CLI and MCP output.
- CLI and MCP contain transport and presentation logic only.
- LSP uses typed application/query operations and protocol-native output; no
  bridge or gateway binding exposes a blind JSON-RPC proxy.
- All business access goes through the daemon.
- Compact Markdown and canonical JSON agree semantically.
- Direct tests cover every shipped binding and failure class touched by PR 12.
- Rejected arguments have CLI/MCP/HTTP parity without recording values or private payloads.
- Git preview/apply Markdown and JSON agree semantically, and no generic Git or
  autonomous ref/history mutation surface exists.
- Feedback diagnostics CLI/MCP/HTTP bindings agree semantically with host
  adapter projections at PR13, preserve truncation/cursor/anchor semantics,
  and never treat response handles as durable finding IDs.
- No generated surface inventory, plan parser, task editor, or executor is introduced.
