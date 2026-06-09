# TokenSave LCM Session Rewrite Design

Date: 2026-06-09
Branch: `feature/lcm-comparison`
Status: Approved design direction

## Goals

Port Hermes LCM into TokenSave by fully rewriting TokenSave's current simple session internals into an LCM implementation while preserving the useful public session-search surface.

The design goals are:

- Make the existing project-local `sessions.db` the authoritative LCM-capable session database through schema migrations, not by introducing a second parallel LCM database for TokenSave-managed sessions.
- Replace the simple provider-normalized session-message internals with lossless LCM-grade raw-message storage, summary-DAG, lifecycle/frontier, externalized-payload, and lineage state.
- Preserve the useful public behavior of provider-normalized transcript search, especially `tokensave_message_search`, while allowing its internals to be rebuilt on top of LCM-grade storage and indexed snippets.
- Remove authoritative session text caps for new writes. Caps may still exist for display snippets, FTS/index text, MCP response truncation, or safety-bounded rendering, but the authoritative stored raw session content must be lossless and expandable/recoverable.
- Use a hybrid ownership model: Rust owns durable/indexed state, migrations, deterministic query/DAG APIs, storage locality, and path-safety rules; the generated Hermes Python plugin owns Hermes `ContextEngine` lifecycle integration and Hermes auxiliary LLM calls.
- Start with the existing generated Python bridge that shells to `tokensave tool ... --json --args`, then consider PyO3 or native Python bindings later only if the bridge becomes a measured bottleneck.
- Keep TokenSave's codegraph context retrieval and fact memory systems separate from LCM session compression.

## Non-Goals

This design does not implement the port. It defines the target architecture and constraints for a later implementation.

Non-goals are:

- Do not add PyO3, maturin, or native Python bindings as the first milestone.
- Do not replace `src/context/builder.rs` with LCM. Codegraph context building remains graph/search/source retrieval for code intelligence.
- Do not repurpose `src/memory/*` fact storage into a summary DAG. The fact store remains structured, user/project memory; LCM stores transcript lineage and compression state.
- Do not require Hermes users to migrate to project-local storage for non-local installs. Storage locality follows current TokenSave and Hermes install rules.
- Do not keep the existing 256 KiB session-message cap as an authoritative storage limit for new session content. It can only survive as a derived-view limit for search/display/response safety.
- Do not preserve branch-only internal shapes that conflict with the approved design. The durable compatibility boundary is public session search and existing session DB data.

## Architecture

TokenSave should absorb Hermes LCM by turning the session layer into a Rust-owned LCM core with a Hermes-specific Python adapter.

The high-level split:

- Rust session/LCM core:
  - Owns `sessions.db` schema, migrations, WAL/busy-timeout settings, storage path selection, path containment, and externalized-payload metadata.
  - Replaces the simple provider-normalized message table as the authoritative raw-content layer with lossless LCM raw storage, while retaining compatibility projections/indexes for provider-normalized search.
  - Exposes deterministic tool/API operations for search, load, describe, expand, expand-query support, compression status, lifecycle/frontier state, and DAG traversal.
  - Preserves stable JSON outputs where existing MCP/Hermes tools depend on them.
- Generated Hermes Python plugin:
  - Continues to be installed from `src/agents/hermes.rs`.
  - Registers TokenSave tools, the `pre_llm_call` hook, and the `TokensaveMemoryProvider`.
  - Hosts the Hermes `ContextEngine` adapter surface and calls TokenSave through `tokensave tool ... --json --args`.
  - Calls Hermes `agent.auxiliary_client.call_llm` for LCM summarization and query expansion prompts that require an auxiliary model.

This keeps durable correctness, migrations, and storage safety in Rust while avoiding a native extension boundary during the first port.

## Storage/Migration Model

The existing `sessions.db` becomes the LCM-capable session DB, and LCM replaces the simple session internals rather than sitting beside them as a second subsystem.

Today, TokenSave stores project-local sessions at `crate::config::get_tokensave_dir(project_root).join("sessions.db")` through `src/sessions/cursor.rs`. `src/global_db.rs` creates provider-normalized tables:

- `sessions(provider, session_id, project_key, project_path, title, started_at, ended_at, transcript_path, metadata_json, parent_session_id, is_subagent, agent_id, parent_tool_use_id)`
- `session_messages(provider, message_id, session_id, role, timestamp, ordinal, text, kind, model, tool_names, source_path, source_offset, metadata_json)`
- `session_messages_fts` over `text`, `role`, `kind`, `model`, and `tool_names`
- parse offsets and accounting tables used by existing transcript ingestion

LCM migrations should rewrite/evolve this database instead of creating a separate TokenSave LCM DB. The migrated schema should support both existing transcript search behavior and LCM-grade raw storage:

- Preserve `sessions` and provider-normalized message search as compatibility projections, not as the only authoritative session representation.
- Add or migrate to LCM raw-message identity with stable `store_id` ordering, conversation/session linkage, source/provider fields, role, lossless content reference, tool-call fields, timestamps, token estimates, pinned state, and metadata.
- Add summary DAG tables equivalent in capability to Hermes `summary_nodes`: `node_id`, session/conversation, depth, summary content/reference, token counts, source token counts, source ids, source type, source time window, expand hint, and FTS/search metadata.
- Add lifecycle/frontier state equivalent in capability to Hermes `lcm_lifecycle_state`: conversation id, current session, last finalized session, current frontier store id, last finalized frontier store id, debt markers, rollover/reset/maintenance timestamps, and update time.
- Add migration state/schema version tables in the existing DB layer so old `sessions.db` files can be upgraded idempotently.
- Add externalized-payload metadata that links placeholders in indexed text to payload files, content hashes, kind, role/tool metadata, session/conversation ownership, byte/char counts, and creation time.

The current `MAX_SESSION_MESSAGE_TEXT_BYTES` cap of 256 KiB must be removed from authoritative storage for new writes. It may remain appropriate for FTS snippets, compatibility search rows, display previews, MCP response truncation, and safety-bounded rendering, but the LCM store must preserve full raw content either inline in a dedicated full-content column/table or by externalizing payloads and storing safe placeholders plus recoverable payload references. FTS should index bounded, safe text or snippets, not necessarily the full authoritative payload.

Existing rows that were already capped cannot be made lossless retroactively. Migrations should carry those rows forward as best-effort legacy data, mark them as legacy/truncated when detectable, and ensure all new writes use lossless authoritative storage.

Storage locality follows TokenSave install rules:

- `--local` Hermes installs use project-local TokenSave storage under the project `.tokensave`, including the migrated `sessions.db` and local externalized payload directory.
- Non-local Hermes installs store LCM state under the Hermes profile/home location, matching Hermes LCM's current profile-local behavior.
- The path selector must be explicit so the same Rust APIs can open either the project-local DB or the Hermes-profile DB without guessing.

## Rust Session/LCM Core

The Rust core should become the source of truth for LCM state. It should not merely mirror the Python reference implementation table-for-table; it should provide the same semantics through TokenSave-native types and migrations.

Core responsibilities:

- Ingest provider-normalized transcript messages into the LCM raw-message model and derive provider-normalized search records from that model where useful.
- Guarantee lossless authoritative raw storage for new writes. Search snippets, FTS columns, MCP responses, and rendered previews may be capped, but `load`/`expand` APIs must be able to recover full content or the full externalized payload.
- Maintain append-first raw-message ordering and store-id lineage. Existing Hermes LCM allows one narrow rewrite for GC placeholders on externalized tool result rows; TokenSave should model any such mutation explicitly and keep source lineage intact.
- Keep the existing search path for `tokensave_message_search` over provider-normalized transcripts, while making LCM search able to combine raw messages and summary nodes with source/session filters.
- Implement summary-DAG persistence and deterministic traversal/expansion APIs in Rust. Python can request operations, but Rust should own source-id resolution, session ownership checks, FTS queries, pagination, and snippet construction.
- Track lifecycle/frontier state durably so session rollover, reset, maintenance debt, and current compaction frontier survive process restarts.
- Keep codegraph context retrieval independent. `ContextBuilder` continues to combine code search, graph traversal, and source extraction; LCM tools operate on conversation/session state.
- Keep fact memory independent. `MemoryCategory`, fact records, trust scores, entity links, and feedback are not summary nodes and should not be overloaded for transcript compression.

The first Rust API surface can be tool-driven rather than a public library API. The important boundary is deterministic JSON in and out, because the generated Hermes plugin can call it through the existing bridge.

## Hermes Python Context-Engine Adapter

Hermes' current LCM reference owns a `ContextEngine`-style lifecycle:

- `should_compress_preflight(messages)` is not a pure predicate. It ingests messages and can return `true` when the replay-safe view differs because ingest protection sanitized or externalized content.
- `compress(messages, current_tokens, focus_topic)` ingests, selects raw backlog outside the fresh tail, summarizes leaf chunks, optionally condenses summary nodes, advances the lifecycle frontier, assembles active prompt context, and returns sanitized replay messages.
- `on_session_start`, rollover/reset handling, auxiliary-session markers, and foreground session binding maintain lifecycle/frontier state.
- Tool calls can ingest live messages before search so current-turn content is discoverable.

TokenSave's generated Hermes plugin should adapt this lifecycle to Rust-owned state rather than reimplementing storage in Python. The adapter should:

- Register a Hermes context engine or equivalent hook surface for preflight, compression, session start, rollover, reset, and tool calls.
- Call TokenSave tools through the generated `tools.py` subprocess bridge with JSON args and JSON responses.
- Keep only process-local coordination in Python, such as active auxiliary session markers and bridge timeout/error handling, when that state does not need to be durable.
- Delegate all durable raw-message, DAG, frontier, payload, and search mutations to Rust.
- Preserve the existing TokenSave Hermes install/config behavior: generate plugin files, register `pre_llm_call`, register the memory provider, and refuse to overwrite an existing non-TokenSave Hermes memory provider.

The adapter should also preserve Hermes' behavior that auxiliary LLM sessions are stateless for compression, so summarizer/query-expansion calls do not recursively ingest or compact themselves.

## LLM Bridge

Hermes LCM summarization depends on Hermes auxiliary model calls. The initial TokenSave port should keep those calls in generated Python:

- Summary calls use `agent.auxiliary_client.call_llm` with task `compression`, messages containing the summary prompt, temperature, max tokens, optional timeout, and routed model settings.
- Reasoning tags such as `<think>`, `<thinking>`, `<reasoning>`, `<thought>`, and `<REASONING_SCRATCHPAD>` must be stripped before summary text is persisted.
- Model fallback and circuit breaker semantics should be preserved: try the configured primary/fallback route chain, stop hammering a failing route during cooldown, and fall back to deterministic truncation when LLM summarization cannot produce a smaller summary.
- Rust should store the resulting summary, source lineage, token accounting, and failure/status metadata. Python should not be the durable owner of summary nodes.

For tools like `lcm_expand_query` that synthesize an answer from expanded context, Python may keep the auxiliary LLM call initially. Rust should provide deterministic context selection and expansion payloads so the LLM prompt is built from safe, session-authorized material.

## Tool/API Surface

`tokensave_message_search` remains stable or compatibly enhanced.

Existing behavior to preserve:

- Required `query`.
- Default provider of `cursor`.
- Optional `project_key`, `parent_session_id`, `include_subagents`, `scope`, and `limit`.
- Results containing provider-normalized session and message records with scores.
- The handler opens the project-local `sessions.db` and searches `session_messages_fts`.

Enhancements should be additive at the public API boundary. Internally, search may be served from LCM-derived compatibility tables or indexed snippets rather than from the old capped text column. Result metadata may indicate legacy truncation, externalized payload references, or LCM availability, but existing callers should still be able to use the current fields.

New LCM-oriented tools may be added rather than overloading `tokensave_message_search`:

- `tokensave_lcm_status`: current session, lifecycle/frontier, compression status, debt, payload stats, and schema health.
- `tokensave_lcm_load_session`: ordered raw-message pages by explicit session id, with role/time filters, content slicing, and stable cursors.
- `tokensave_lcm_grep`: combined raw-message and summary-node search with current/session/all scope controls.
- `tokensave_lcm_describe`: session DAG overview or summary-node subtree metadata.
- `tokensave_lcm_expand`: expand a raw message, summary node, externalized payload, or source subtree with authorization checks and content pagination.
- `tokensave_lcm_expand_query`: deterministic context selection in Rust plus optional Hermes auxiliary synthesis in Python.
- `tokensave_lcm_compress` or internal equivalents: preflight/ingest/compress operations used by the Hermes adapter.

Names can be finalized during implementation, but the behavioral split should remain: `tokensave_message_search` is compatibility transcript search; LCM tools expose compression-aware session state.

## Ingest Protection/Security

Hermes LCM contains important storage-boundary protections that should carry into TokenSave's Rust-owned store:

- Externalize obvious large or binary-ish payloads, including data URI/base64 media, long base64 runs, oversized raw payloads, and large tool outputs.
- Store compact placeholders in indexed text while preserving recoverable payload content in full-content storage or externalized files with metadata and hashes.
- Keep externalized payload files private where possible, equivalent to `0700` directories and `0600` payload files.
- Reject payload refs that are not basenames. Do not allow `/`, `\`, parent traversal, symlink escape, or arbitrary absolute paths.
- Enforce storage root containment. Local installs stay under project `.tokensave`; non-local profile installs stay under the selected Hermes home/profile root.
- Keep session/conversation ownership checks on payload expansion so a ref from another session cannot be expanded casually.
- Preserve optional sensitive-pattern redaction as metadata-only/lossy when enabled, and make that lossiness visible in status/doctor output.
- Include integrity scans for missing/unreferenced payloads and SQLite/FTS health without previewing sensitive payload contents.

The security model should treat search indexes, snippets, and rendered previews as derived, bounded, and safe-to-display. The authoritative raw content path must be lossless, separately controlled, paginated, and authorized.

## Lifecycle/Compression Behavior

The compression semantics should match Hermes LCM's observable behavior while moving durable state to Rust.

Key behavior to preserve:

- Preflight can ingest. It must be allowed to return compression-needed when ingest protection changes the replay view even if token thresholds alone would not require compression.
- Compression bypasses ignored sessions, stateless sessions, and auxiliary thread contexts.
- Compression respects a cooldown after boundary skips and can force overflow recovery when context pressure is critical.
- The fresh tail stays raw in active context. Raw backlog outside the fresh tail is summarized into leaf DAG nodes, with dynamic leaf chunk behavior where configured.
- Summary nodes preserve source ids, source token counts, time windows, depth, and expand hints so future `describe`/`expand` calls can recover lineage.
- Condensation can summarize lower-depth nodes into higher-depth nodes when needed, preserving a DAG rather than a flat rolling summary.
- Active prompt assembly combines the leading system anchor when present, relevant summary context, preserved objective context when needed, and the fresh tail. It must sanitize tool-call/tool-result pairing before returning replay messages.
- Rollover resets the active frontier for the new session while preserving the last finalized session/frontier for the conversation.
- Maintenance debt records when raw backlog could not yet be compacted, so deferred maintenance can run later instead of losing state.

TokenSave should keep deterministic state transitions in Rust and leave nondeterministic summarization text generation to Hermes auxiliary LLM calls through Python.

## Testing Strategy

Testing should cover the design boundaries rather than only happy-path tool output.

Required test areas:

- Migration tests from existing `sessions.db` files with only `sessions`, `session_messages`, `session_messages_fts`, parse offsets, and parent/subagent columns into the LCM-capable schema.
- Migration tests proving already-capped legacy rows are carried forward as best-effort legacy data, marked as truncated/legacy where detectable, and never treated as newly lossless content.
- Compatibility tests proving `tokensave_message_search` retains current provider/project/subagent filtering and result shape.
- Raw-storage tests proving new content above 256 KiB is preserved authoritatively and recoverable through load/expand APIs while search indexes, snippets, MCP responses, and display renderers use safe bounded views.
- Regression tests proving no new authoritative session write path uses `MAX_SESSION_MESSAGE_TEXT_BYTES` or any replacement cap before durable raw storage/externalization.
- Externalization tests for large tool output, data URI/base64 payloads, basename-only refs, private permissions, root containment, missing payloads, and cross-session expansion denial.
- Lifecycle tests for session start, rollover, reset, frontier advance, last-finalized frontier preservation, maintenance debt, and restart recovery.
- Compression tests with fake Hermes auxiliary LLM calls covering leaf compaction, condensation, fallback chain, circuit breaker, deterministic truncation fallback, reasoning stripping, and active-context assembly.
- Generated Hermes plugin tests proving install layout, config editing, memory provider registration, `pre_llm_call`, JSON bridge calls, and non-overwrite behavior for existing memory providers.
- Tool tests for load/grep/describe/expand/expand-query/status using deterministic fixtures.
- Regression tests that `src/context/builder.rs` and `src/memory/*` remain independent from LCM session compression.

## Rollout/Backward Compatibility

Rollout should be incremental but converge on one session store design.

Compatibility commitments:

- Existing `sessions.db` files are migrated in place with idempotent schema migrations.
- Existing capped rows are preserved as best-effort legacy data. New session writes after migration must be lossless in the LCM raw store.
- Existing provider-normalized transcript ingestion continues to support `sessions` and `session_messages` behavior, but those simple internals become compatibility projections over the LCM-grade session store where practical.
- `tokensave_message_search` remains stable or only compatibly enhanced.
- Existing local installs continue to store under project `.tokensave`; non-local Hermes installs continue to store under the Hermes profile/home.
- Existing Hermes plugin generation remains the installation mechanism, but its generated Python grows the LCM adapter and tool bridge calls.

Rollout shape:

- Introduce schema migrations and read-only LCM inspection APIs first, including proof that new authoritative writes are lossless and old capped rows are flagged as legacy where detectable.
- Add Hermes adapter preflight/ingest with compression disabled or status-only until storage and search compatibility are verified.
- Enable compression in Hermes after raw storage, externalization, lifecycle, and active-context assembly tests pass.
- Measure bridge overhead before considering PyO3/native bindings. A later native milestone should be justified by latency, packaging, or reliability evidence.

## Open Questions

- Should non-local Hermes-profile LCM storage reuse a file named `sessions.db` for consistency, or keep a Hermes-specific filename while using the same migrated schema?
- What is the exact Rust table layout for full raw content: inline full-content table, externalized-by-default payload table, or a hybrid threshold policy?
- Which LCM operations should be public MCP tools versus internal Hermes-plugin-only tools?
- Should TokenSave expose LCM summary nodes to non-Hermes providers once their transcripts are indexed, or initially limit compression lifecycle to Hermes sessions?
- How much of `lcm_expand_query` synthesis should remain Python-only after context selection, and what JSON contract should Rust provide for reproducible prompt construction?

## Self-Review

This spec intentionally describes one plan: fully rewrite the existing TokenSave session internals into a lossless LCM-capable store inside the existing `sessions.db`, with Rust owning durable state and generated Python owning Hermes lifecycle/auxiliary LLM integration. It rejects the earlier separate-DB starting point, rejects capped authoritative storage for new writes, and rejects PyO3 as the first milestone.

No placeholders or TBD markers remain. The open questions are bounded design choices for implementation, not unresolved contradictions in the approved architecture.
