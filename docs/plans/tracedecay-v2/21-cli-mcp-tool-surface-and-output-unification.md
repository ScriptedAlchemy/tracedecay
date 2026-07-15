# TraceDecay V2 CLI, MCP, and Output Unification

**Delivery:** PR 12

**Status:** planned product work
**Depends on:** [08 tool catalog](08-tool-catalog-crate.md), [09 application](09-application-crate.md), [16 scope](16-cross-project-repository-worktree-scope.md), [18 privacy](18-secret-detection-redaction-and-private-data-safety.md), and [20 configuration](20-configuration-control-plane.md).

## Outcome

CLI and MCP are thin clients over the same daemon-owned application use cases. Each use case returns one typed result. Human output is compact Markdown or terminal text; machine output is canonical JSON serialized from that same result.

This plan does not create a generated surface inventory, plan parser, command generator, parity matrix, task editor, or workflow executor.

## Boundaries

- `tracedecayd` is the sole application and database authority.
- CLI and MCP resolve transport input, call the daemon, and render the response. They never open a business database or run query, policy, migration, or repair logic locally.
- A missing or incompatible daemon fails closed with one actionable problem. No client silently falls back to an embedded writer.
- HTTP and SDK bindings remain owned by [17](17-official-public-api-and-sdks.md). Dynamic workflow semantics remain owned by [32](32-dynamic-workflow-runtime-and-sdk.md).
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

Multiple MCP clients may connect concurrently, but all business reads and writes are brokered through the one daemon authority established by PR 4.

## Direct parity tests

Parity is verified from public behavior, not from a generated inventory:

1. invoke the same use case through CLI and MCP;
2. decode both canonical JSON results and compare semantic fields;
3. verify compact human output preserves identity, coverage, blockers, and continuation;
4. test missing daemon, stale client, denied scope, empty, partial, redacted, paged, oversized, cancelled, and failed states;
5. run concurrent-client tests proving clients never open writable databases;
6. test stdout, stderr, exit codes, MCP lifecycle, framing, cancellation, and reconnect behavior directly.

## PR 12 deliverables

- one daemon client shared by CLI and MCP adapters;
- sealed application-result serialization;
- compact Markdown/terminal presenters for shipped use cases;
- canonical JSON and cursor/anchor handling;
- stable problem and exit mapping;
- removal of handler-local database/query behavior, raw JSON renderers, double encoding, irreversible truncation, and writable fallback;
- focused CLI/MCP parity and concurrency tests.

## Done

- One typed application result drives CLI and MCP output.
- CLI and MCP contain transport and presentation logic only.
- All business access goes through the daemon.
- Compact Markdown and canonical JSON agree semantically.
- Direct tests cover every shipped binding and failure class touched by PR 12.
- No generated surface inventory, plan parser, task editor, or executor is introduced.
