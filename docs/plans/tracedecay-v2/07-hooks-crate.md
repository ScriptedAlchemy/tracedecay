# V2 host hooks boundary

## Status / Role

- Status: current host behavior is captured and hardened in PR6; the V2 hook cutover lands in PR13.
- PR11 supplies application and policy decisions. PR12 supplies stable daemon/API surfaces.
- Hooks are thin host adapters. They emit bounded events or signals to tracedecayd and render its bounded response.

## Outcome

Codex, Claude Code, Cursor, Hermes, Kiro, and supported daemon/MCP notifications feed one daemon-owned event path without opening databases or running synchronization inside the host hook process.

## Owns

- Provider-specific wire decoding, event-name mapping, matcher handling, and legal response rendering.
- A small canonical HookEvent envelope containing provider/session/event identity, ordering/idempotency keys, safe scope hints, bounded payload references, deadline, and schema version.
- Bounded IPC to tracedecayd, strict local serialization limits, and provider-safe behavior when the daemon is unavailable.
- Direct fixtures for each supported host event and response contract.
- Hook-process latency, payload-size, and privacy telemetry that contains no prompt or tool payload.

## Does not own

- Database reads or writes, transcript ingestion, sync, catch-up, project discovery, source parsing, sanitization, indexing, projection, query, policy evaluation, or hint selection.
- A local durable queue, secondary writer, embedded daemon, or direct fallback store.
- Task plans, boards, workflow execution, attempts, leases, agent steering, or end-of-turn task completion.
- Host installation/bundle management, tool catalogs, config mutation, network services, or generic command execution.
- Generated provider inventories, generated conformance matrices, source parsers, workflow JavaScript, or plan-derived code.

## Required behavior

- **PR6 — baseline:** preserve current supported Codex, Claude Code, Cursor, Hermes, and Kiro event semantics in direct redacted fixtures. Unknown events remain explicit and harmless.
- **PR6 — measurements:** record real hook wall time, daemon round-trip time, payload bytes, timeout, and disposition without recording message content.
- **PR6 — failure:** prove existing hooks do not corrupt state when duplicated, reordered, interrupted, or invoked while the daemon is unavailable.
- **PR13 — signal path:** decode one host event, validate bounds, assign an idempotency/order key, send HookEvent to tracedecayd, and stop. Session-start and file-change hooks signal required work; they do not perform sync.
- **PR13 — daemon authority:** tracedecayd owns durable capture, sanitization, scope resolution, sync, database transactions, projections, query freshness, policy evaluation, and receipts.
- **PR13 — acknowledgement:** a successful hook acknowledgement means only that the daemon accepted or durably handled the event according to its typed disposition. It never claims downstream work completed unless the daemon says so.
- **PR13 — unavailable daemon:** optional guidance fails open with no injected text. Capture relies on later daemon catch-up from authoritative host transcripts; the hook must not create another writer or spool database.
- **PR13 — deadlines:** enforce provider-specific hard budgets with cancellation. Late daemon responses are discarded and cannot be injected into a later turn.
- **PR13 — idempotency:** duplicate delivery of the same provider/session/event/order key produces one daemon-side logical event and a stable response disposition.
- **PR13 — ordering:** preserve provider sequence when available; otherwise mark ordering unknown. Never manufacture total order from arrival time.
- **PR13 — response:** render only application-approved, sensitivity-safe guidance supported by that host event. No hook-local reranking or policy fallback.
- **PR13 — payloads:** inline only bounded, eligible metadata. Large or sensitive source content remains in the authoritative host transcript for daemon catch-up.
- **PR13 — isolation:** one busy session cannot block another. Bound request concurrency and memory; overload returns an explicit no-guidance disposition.
- **PR13 — migration:** shadow the daemon event path against current host behavior, cut over one provider/event family at a time, and retain a direct rollback switch until parity receipts pass.

## Acceptance

- PR6 fixtures assert exact supported event mappings and provider response legality against current host probes.
- PR13 tests cover duplicate, reordered, concurrent, malformed, oversized, unknown, timed-out, cancelled, daemon-down, daemon-restart, and slow-consumer cases.
- Integration tests prove hooks never open TraceDecay databases, run sync/catch-up, scan project files, load models, invoke Git, or start child workflows.
- Crash tests cover failure before send, during send, after daemon acceptance, and during response rendering without duplicate daemon effects.
- Privacy tests prove prompts, tool payloads, credentials, private paths, and reasoning text do not enter hook telemetry or error output.
- Performance tests report cold/warm p50/p95/p99 and enforce the provider budgets defined by PR6 measurements.
- Cutover tests compare current and V2 event dispositions, daemon receipts, and rendered guidance before each provider family switches.
- Architecture tests reject database, store-adapter, query, policy-runtime, executor, workflow-JavaScript, and generated-inventory imports from hook adapters.
