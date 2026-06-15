# Holographic Memory Replacement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the current `tokensave` cross-session memory implementation with a Rust-native holographic fact memory backed by `amari-holographic`, SQLite, FTS5, trust scoring, and MCP tools inspired by Hermes Holographic.

**Architecture:** Keep memory local to `.tokensave/tokensave.db`, but replace `memory_decisions` / `memory_code_areas` as the active model with a fact/entity store. Use `amari-holographic` for the VSA/HRR algebra, SQLite/FTS5 for durable metadata and lexical recall, and a Rust retrieval layer that blends FTS5, token overlap, holographic similarity, trust, and optional temporal decay. Existing memory tools become compatibility wrappers over the new fact store, while new first-class MCP tools expose add/search/probe/related/reason/contradict/update/remove/list/feedback/status behavior.

**Tech Stack:** Rust 2021, `libsql`, `serde`, `serde_json`, `sha2`, `amari-holographic`, `tokio`, FTS5, existing `tokensave` MCP handler patterns. Future Python interop can use PyO3/maturin for Rust-to-Python exports and `pyo3_bindgen` only when Rust needs generated bindings to existing Python modules.

---

## Deep-Dive Findings

The current memory system is intentionally small: `DecisionRecord` and `CodeAreaRecord` live in `src/tokensave.rs`; migrations create `memory_decisions`, `memory_code_areas`, and `memory_decisions_fts`; MCP definitions expose `tokensave_record_decision`, `tokensave_record_code_area`, and `tokensave_session_recall`; tests live in `tests/memory_test.rs` and `tests/mcp_handler_test.rs`.

Hermes Holographic is broader than the current `tokensave` memory model. It stores facts, entities, fact-entity links, trust scores, retrieval counters, optional HRR vectors, category memory banks, and FTS5 indexes. Its tool surface is `fact_store` with actions `add`, `search`, `probe`, `related`, `reason`, `contradict`, `update`, `remove`, and `list`, plus `fact_feedback` for helpful/unhelpful scoring.

Hermes' Python HRR implementation uses deterministic SHA-256 phase atoms, phase-add binding, phase-subtract unbinding, circular-mean bundling, and phase cosine similarity. `amari-holographic` gives Rust-native binding, bundling, unbinding, similarity, `HolographicMemory`, `RetrievalResult`, `CapacityInfo`, and resonator cleanup, so the math should be adopted rather than ported.

The plan deliberately avoids a feature flag. Holographic memory becomes the only active memory implementation. Existing public memory tool names are preserved as wrappers where practical to avoid breaking agents that already call them, but their backing storage and retrieval semantics become holographic fact memory.

---

## File Structure

- Modify `Cargo.toml` and `Cargo.lock`: add `amari-holographic` as a normal dependency, not an optional feature.
- Create `src/memory/mod.rs`: module boundary and public exports for the new memory subsystem.
- Create `src/memory/types.rs`: `FactRecord`, `EntityRecord`, `MemoryCategory`, retrieval result structs, feedback structs, and status structs.
- Create `src/memory/encoding.rs`: deterministic symbol/text/entity/path encoding on top of `amari-holographic`.
- Create `src/memory/entities.rs`: entity extraction, normalization, alias matching, and code-aware entity extraction.
- Create `src/memory/store.rs`: SQLite CRUD, migrations-facing helpers, vector persistence, bank rebuilds, and legacy backfill.
- Create `src/memory/retrieval.rs`: hybrid search, probe, related, reason, contradiction detection, and ranking.
- Create `src/memory/trust.rs`: trust clamping, feedback deltas, retrieval counters, and temporal decay math.
- Modify `src/lib.rs`: export `memory`.
- Modify `src/db/migrations.rs`: add v11 schema and fresh-schema tables/triggers.
- Modify `src/tokensave.rs`: remove active decision/code-area memory methods, add fact memory facade methods, and keep compatibility wrappers.
- Modify `src/mcp/tools/definitions.rs`: add new holographic memory tool schemas and update old memory tool descriptions.
- Modify `src/mcp/tools/handlers/memory.rs`: replace current handlers with fact-store handlers and wrappers.
- Modify `src/mcp/tools/handlers/mod.rs`: dispatch new tool names.
- Modify `src/tool_command.rs`: group new tools under `memory & session`.
- Modify `README.md`, `docs/USER-GUIDE.md`, and `src/mcp/server.rs`: document the new memory schema and MCP usage.
- Replace `tests/memory_test.rs`: unit/integration coverage for facts, entities, vectors, trust, retrieval, wrappers, and legacy backfill.
- Modify `tests/mcp_handler_test.rs`: MCP coverage for every new action and compatibility wrapper.
- Modify `tests/migration_test.rs`: v11 schema and migration/backfill coverage.

---

## Data Model

Use these active tables:

```sql
memory_facts(
  fact_id INTEGER PRIMARY KEY AUTOINCREMENT,
  content TEXT NOT NULL UNIQUE,
  category TEXT NOT NULL DEFAULT 'general',
  tags TEXT NOT NULL DEFAULT '[]',
  trust_score REAL NOT NULL DEFAULT 0.5,
  retrieval_count INTEGER NOT NULL DEFAULT 0,
  helpful_count INTEGER NOT NULL DEFAULT 0,
  unhelpful_count INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  last_retrieved_at INTEGER,
  last_feedback_at INTEGER,
  source TEXT NOT NULL DEFAULT 'manual',
  metadata TEXT NOT NULL DEFAULT '{}',
  hrr_vector BLOB,
  hrr_algebra TEXT NOT NULL DEFAULT 'amari_fhrr',
  hrr_dim INTEGER NOT NULL DEFAULT 2048
);

memory_entities(
  entity_id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL UNIQUE,
  entity_type TEXT NOT NULL DEFAULT 'unknown',
  aliases TEXT NOT NULL DEFAULT '[]',
  created_at INTEGER NOT NULL
);

memory_fact_entities(
  fact_id INTEGER NOT NULL REFERENCES memory_facts(fact_id) ON DELETE CASCADE,
  entity_id INTEGER NOT NULL REFERENCES memory_entities(entity_id) ON DELETE CASCADE,
  PRIMARY KEY (fact_id, entity_id)
);

memory_banks(
  bank_id INTEGER PRIMARY KEY AUTOINCREMENT,
  bank_name TEXT NOT NULL UNIQUE,
  vector BLOB NOT NULL,
  hrr_algebra TEXT NOT NULL,
  hrr_dim INTEGER NOT NULL,
  fact_count INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL
);

memory_feedback_events(
  event_id INTEGER PRIMARY KEY AUTOINCREMENT,
  fact_id INTEGER NOT NULL REFERENCES memory_facts(fact_id) ON DELETE CASCADE,
  action TEXT NOT NULL CHECK(action IN ('helpful', 'unhelpful')),
  trust_delta REAL NOT NULL,
  old_trust REAL NOT NULL,
  new_trust REAL NOT NULL,
  created_at INTEGER NOT NULL,
  source TEXT NOT NULL DEFAULT 'mcp',
  note TEXT
);
```

Keep `memory_decisions` and `memory_code_areas` during migration as legacy tables, but stop using them for new writes. Backfill their rows into `memory_facts` once, then have old MCP tools call the new fact APIs.

---

## Trust, Feedback, And Ranking Contract

Trust is first-class state, not just a sorting hint. Every fact starts at `default_trust` (`0.5` unless configuration later says otherwise), is clamped to `[0.0, 1.0]`, and is included in every ranked result. `helpful` feedback applies `+0.05`, increments `helpful_count`, writes a `memory_feedback_events` row, sets `last_feedback_at`, and returns old/new trust. `unhelpful` feedback applies `-0.10`, increments `unhelpful_count`, writes the same audit row, and never deletes the fact automatically.

Retrieval scoring must expose its components so callers can understand why a memory appeared:

```text
relevance = (0.40 * fts_score) + (0.30 * jaccard_score) + (0.30 * holographic_score)
score = relevance * trust_score * temporal_decay
```

`retrieval_count` and `last_retrieved_at` update only for facts returned to the MCP caller. `min_trust` defaults to `0.3`, matching Hermes. `tokensave_memory_status` must report trust distribution buckets (`0.0-0.25`, `0.25-0.5`, `0.5-0.75`, `0.75-1.0`), feedback counts, and facts below the default recall threshold so later Hermes integration can decide when to prune, ask for confirmation, or mark memories stale.

---

## Rust API Contract

The implementation should expose these typed request/response structs from `src/memory/types.rs` and facade methods on `TokenSave`:

```rust
pub struct AddFactRequest {
    pub content: String,
    pub category: MemoryCategory,
    pub tags: Vec<String>,
    pub entities: Vec<String>,
    pub source: String,
    pub metadata: serde_json::Value,
}

pub struct SearchFactsRequest {
    pub query: String,
    pub category: Option<MemoryCategory>,
    pub min_trust: f64,
    pub limit: usize,
}

pub struct FeedbackRequest {
    pub fact_id: i64,
    pub action: FeedbackAction,
    pub source: String,
    pub note: Option<String>,
}
```

Required `TokenSave` facade methods:

```rust
add_fact(request) -> FactRecord
search_facts(request) -> Vec<FactSearchResult>
probe_entity(entity, category, limit) -> Vec<FactSearchResult>
related_facts(entity, category, limit) -> Vec<FactSearchResult>
reason_facts(entities, category, limit) -> Vec<FactSearchResult>
contradict_facts(category, threshold, limit) -> Vec<ContradictionResult>
update_fact(fact_id, patch) -> FactRecord
remove_fact(fact_id) -> bool
list_facts(category, min_trust, limit) -> Vec<FactRecord>
record_fact_feedback(request) -> FeedbackResult
memory_status() -> MemoryStatus
```

These APIs are intentionally close to Hermes' provider methods, but use typed Rust requests so future Hermes bridge code can translate directly without inspecting SQL.

---

## MCP Surface

Add these first-class tools:

- `tokensave_fact_store`: action-based tool matching Hermes semantics with actions `add`, `search`, `probe`, `related`, `reason`, `contradict`, `update`, `remove`, and `list`.
- `tokensave_fact_feedback`: rate a fact as `helpful` or `unhelpful`.
- `tokensave_memory_status`: report fact counts, entity counts, bank status, algebra name, dimension, estimated capacity, missing-vector count, and migration/backfill status.

`tokensave_fact_store` input must accept the Hermes-compatible fields below. The tool name remains `tokensave_`-prefixed because the current tool registry asserts that all exposed MCP tools use that prefix.

```json
{
  "action": "add|search|probe|related|reason|contradict|update|remove|list",
  "content": "fact text for add/update",
  "query": "search query",
  "entity": "single entity for probe/related",
  "entities": ["entity A", "entity B"],
  "fact_id": 123,
  "category": "user_pref|project|tool|general|decision|code_area",
  "tags": ["rust", "memory"],
  "min_trust": 0.3,
  "trust_delta": -0.1,
  "threshold": 0.3,
  "limit": 10,
  "source": "mcp|compat|migration|hermes",
  "metadata": {}
}
```

`tokensave_fact_store` output must include `results` or `fact`, `count`, and, for ranked retrieval actions, per-result `score`, `trust_score`, `fts_score`, `jaccard_score`, `holographic_score`, and `why`. This is required for later Hermes integration and for debugging trust/ranking behavior.

`tokensave_fact_feedback` input must accept:

```json
{
  "action": "helpful|unhelpful",
  "fact_id": 123,
  "source": "mcp|hermes",
  "note": "optional reason"
}
```

`tokensave_fact_feedback` output must include `fact_id`, `action`, `old_trust`, `new_trust`, `trust_delta`, `helpful_count`, `unhelpful_count`, and `event_id`.

Keep these compatibility wrappers:

- `tokensave_record_decision`: writes a `decision` category fact.
- `tokensave_record_code_area`: writes or updates a `code_area` category fact.
- `tokensave_session_recall`: delegates to hybrid search/list and can include code-area facts.

---

## Hermes Integration Contract

Future Hermes integration should not read `.tokensave/tokensave.db` directly. It should call MCP tools or the typed Rust facade. The mapping is:

- Hermes `fact_store(action="add")` -> `tokensave_fact_store { "action": "add", "source": "hermes", ... }`
- Hermes `fact_store(action="search")` / provider `prefetch(query)` -> `tokensave_fact_store { "action": "search", "query": query, "min_trust": 0.3, "limit": 5 }`
- Hermes `fact_store(action="probe")` -> `tokensave_fact_store { "action": "probe", "entity": entity }`
- Hermes `fact_store(action="related")` -> `tokensave_fact_store { "action": "related", "entity": entity }`
- Hermes `fact_store(action="reason")` -> `tokensave_fact_store { "action": "reason", "entities": entities }`
- Hermes `fact_store(action="contradict")` -> `tokensave_fact_store { "action": "contradict", "threshold": 0.3 }`
- Hermes `fact_feedback(action="helpful"|"unhelpful")` -> `tokensave_fact_feedback`
- Hermes `system_prompt_block()` -> `tokensave_memory_status` summarized by the integration layer.
- Hermes `on_memory_write(action="add", target, content)` -> `tokensave_fact_store { "action": "add", "category": "user_pref"|"general", "source": "hermes" }`

The MCP outputs must be JSON-serializable, must not expose raw vector bytes, and must include stable `fact_id` values so Hermes can call feedback after using a memory. This is the key integration invariant: every recalled fact can be rated later.

---

## Future Python Interop Contract

Python interoperability should remain outside the core memory replacement path so `tokensave` does not require a Python runtime to build or run. If future Hermes integration needs Python packaging, add a separate wrapper crate or package rather than putting PyO3 into the main binary.

There are two distinct directions:

- Rust exposed to Python: use PyO3 plus maturin in a future `tokensave-py` wrapper that calls the typed Rust memory facade (`add_fact`, `search_facts`, `record_fact_feedback`, `memory_status`). This is the right path if Hermes wants to import `tokensave` as a Python module.
- Python exposed to Rust: use `pyo3_bindgen` as a build dependency only in a bridge/test crate when Rust needs generated bindings to an existing Python module, such as a Hermes provider shim or compatibility harness. `pyo3_bindgen` generates Rust bindings to Python modules; it does not by itself package Rust APIs for Python callers.

Future wrapper APIs must preserve the same contract as MCP: stable `fact_id`, ranked score components, trust fields, feedback event IDs, and no raw vector bytes in normal responses.

Potential future files:

- `crates/tokensave-py/Cargo.toml`: PyO3/maturin wrapper crate.
- `crates/tokensave-py/src/lib.rs`: Python module exports for fact store, feedback, and status.
- `crates/hermes-bridge/build.rs`: optional `pyo3_bindgen` generation for importing Hermes Python contracts during compatibility tests.
- `tests/hermes_bridge_contract_test.rs`: verifies Python-facing and MCP-facing payloads stay equivalent.

---

## 60-Item Execution Plan

### Phase 1: Dependency And API Spike

- [ ] 1. Add `amari-holographic` to `Cargo.toml` as a normal dependency and run `cargo check` to confirm version compatibility with the existing Rust 2021 crate.
- [ ] 2. Create a temporary compile-only test in `tests/memory_test.rs` that imports `HolographicMemory`, `BindingAlgebra`, `FHRRAlgebra`, and `AlgebraConfig`, then remove the test once the exact public paths are confirmed.
- [ ] 3. Decide the concrete algebra alias in `src/memory/encoding.rs`; prefer `FHRRAlgebra<2048>` because it is closest to Hermes phase-vector HRR, and document why Product Clifford is not the first implementation target.
- [ ] 4. Verify that the chosen algebra supports `from_coefficients`, `to_coefficients`, `bind`, `unbind`, `bundle`, `similarity`, and `normalize` without requiring random runtime state.
- [ ] 5. Add `pub mod memory;` to `src/lib.rs` with an empty `src/memory/mod.rs`, then run `cargo check` to establish the module boundary.

### Phase 2: Core Types

- [ ] 6. Create `src/memory/types.rs` with `MemoryCategory` variants `General`, `UserPref`, `Project`, `Tool`, `Decision`, and `CodeArea`.
- [ ] 7. Add `FactRecord` with fields matching `memory_facts`, using JSON-array `Vec<String>` tags in Rust and `fact_id` as `i64`.
- [ ] 8. Add `EntityRecord` with `entity_id`, `name`, `normalized_name`, `entity_type`, `aliases`, and `created_at`.
- [ ] 9. Add `FactSearchResult` with fact fields plus `score`, `fts_score`, `jaccard_score`, `holographic_score`, `trust_score`, and optional `why`.
- [ ] 10. Add `ContradictionResult`, `FeedbackAction`, `FeedbackRequest`, `FeedbackResult`, `MemoryStatus`, `AddFactRequest`, `SearchFactsRequest`, and `UpdateFactRequest` structs/enums with `serde` derives for typed facade and MCP output.

### Phase 3: Deterministic Holographic Encoding

- [ ] 11. Create `src/memory/encoding.rs` with a `HolographicEncoder` type that owns algebra name, dimension, and reserved role labels.
- [ ] 12. Implement deterministic atom generation from SHA-256 counter blocks so the same token maps to the same algebra value across processes and machines.
- [ ] 13. Implement `encode_text(text)` with Hermes-compatible tokenization as the baseline: lowercase, split on whitespace, strip leading/trailing punctuation, skip empty tokens.
- [ ] 14. Implement `encode_entity(entity)` using normalized entity text and a reserved `__hrr_role_entity__` binding role.
- [ ] 15. Implement `encode_fact(content, entities)` by bundling role-bound content and role-bound entities.
- [ ] 16. Implement vector serialization as `Vec<f64>` coefficients encoded with `bincode`, and deserialization back into the chosen algebra type.
- [ ] 17. Add unit tests proving atom determinism, text determinism, entity normalization stability, fact encoding stability, and serialize/deserialize round trips.

### Phase 4: Entity Extraction

- [ ] 18. Create `src/memory/entities.rs` with `normalize_entity_name` that lowercases, trims whitespace, collapses repeated spaces, and preserves code identifiers.
- [ ] 19. Implement Hermes-style entity extraction for capitalized multi-word names, double-quoted strings, single-quoted strings, and `aka` / `also known as` patterns.
- [ ] 20. Add code-aware extraction for project memory: file paths like `src/foo.rs`, symbol-like tokens like `TokenSave::init`, and tool names like `tokensave_context`.
- [ ] 21. Deduplicate extracted entities by normalized name while preserving first-seen display names.
- [ ] 22. Add tests for quoted entities, aliases, capitalized names, file paths, Rust symbols, MCP tool names, and deduplication order.

### Phase 5: Schema Migration

- [ ] 23. Bump `LATEST_VERSION` in `src/db/migrations.rs` from `10` to `11`.
- [ ] 24. Add `migrate_v11` that creates `memory_facts`, `memory_entities`, `memory_fact_entities`, `memory_banks`, `memory_feedback_events`, indexes, `memory_facts_fts`, and FTS triggers.
- [ ] 25. Update `create_schema` so brand-new databases get the v11 memory schema without replaying old migrations.
- [ ] 26. Backfill `memory_decisions` into `memory_facts` with category `decision`, preserving `text`, `reason` as appended context, `files` as entities/tags where possible, and `created_at`.
- [ ] 27. Backfill `memory_code_areas` into `memory_facts` with category `code_area`, content derived from path plus description, and path extracted as an entity.
- [ ] 28. Record a metadata key such as `holographic_memory_backfill_v1` so backfill does not duplicate rows if migration is retried.
- [ ] 29. Add migration tests for fresh schema, v10-to-v11 migration, decision backfill, code-area backfill, feedback event schema, FTS trigger creation, and idempotence.

### Phase 6: SQLite Store Layer

- [ ] 30. Create `src/memory/store.rs` with `MemoryStore<'a>` or a lightweight facade around `libsql::Connection` obtained from `TokenSave`.
- [ ] 31. Implement `add_fact(AddFactRequest, default_trust)` with duplicate detection by `content`, explicit `source`, JSON `metadata`, explicit entities, extracted entities, and initial `trust_score`.
- [ ] 32. Implement entity resolution by `normalized_name`, alias JSON matching, and insertion into `memory_fact_entities`.
- [ ] 33. Implement `update_fact(UpdateFactRequest)` for content, category, tags, source, metadata, trust delta, and entity relinking when content changes.
- [ ] 34. Implement `remove_fact`, `list_facts`, `get_fact`, `increment_retrieval_counts`, and `record_feedback_event` with old/new trust audit data.
- [ ] 35. Implement `compute_missing_vectors` that encodes facts whose `hrr_vector` is null or whose dimension/algebra is stale.
- [ ] 36. Implement `rebuild_bank(category)` and `rebuild_all_banks()` using bundled fact vectors, with empty-category bank deletion.
- [ ] 37. Add store tests for add, duplicate add, update, remove, list, entity links, vector persistence, missing-vector rebuild, bank rebuild, feedback event writes, `last_feedback_at`, and `last_retrieved_at`.

### Phase 7: Trust And Ranking

- [ ] 38. Create `src/memory/trust.rs` with constants `HELPFUL_DELTA = 0.05`, `UNHELPFUL_DELTA = -0.10`, `TRUST_MIN = 0.0`, `TRUST_MAX = 1.0`, `DEFAULT_TRUST = 0.5`, and `DEFAULT_MIN_TRUST = 0.3`.
- [ ] 39. Implement `clamp_trust`, `apply_feedback`, `trust_bucket`, `trust_distribution`, and `temporal_decay(timestamp, half_life_days)` with deterministic tests.
- [ ] 40. Implement weighted scoring defaults: FTS5 `0.40`, Jaccard `0.30`, holographic `0.30`, multiplied by `trust_score` and temporal decay, while preserving each component score in `FactSearchResult`.
- [ ] 41. Add tests for helpful feedback, unhelpful feedback, feedback event output, old/new trust values, trust clamping, trust buckets, retrieval count increments, and temporal decay disabled/enabled.

### Phase 8: Retrieval Layer

- [ ] 42. Create `src/memory/retrieval.rs` with `FactRetriever` over `MemoryStore` and `HolographicEncoder`.
- [ ] 43. Implement `search(query, category, min_trust, limit)` as FTS5 candidates plus sanitized fallback query, Jaccard rerank, holographic similarity, trust, and optional temporal decay.
- [ ] 44. Implement `probe(entity, category, limit)` using entity-role binding and fact-vector scoring, with FTS fallback only when no vectorized facts exist.
- [ ] 45. Implement `related(entity, category, limit)` by scoring structural adjacency to the bare entity atom and known role vectors.
- [ ] 46. Implement `reason(entities, category, limit)` with AND semantics by taking the minimum per-entity structural score.
- [ ] 47. Implement `contradict(category, threshold, limit)` using entity overlap plus low holographic/content similarity, capped at a safe comparison limit.
- [ ] 48. Add retrieval tests for FTS operator sanitization, Jaccard reranking, holographic probe, multi-entity reason, related, contradiction, min-trust filtering, trust-weighted ordering, score-component output, and retrieval counters.

### Phase 9: TokenSave Facade

- [ ] 49. Add new typed `TokenSave` methods: `add_fact(AddFactRequest)`, `search_facts(SearchFactsRequest)`, `probe_entity`, `related_facts`, `reason_facts`, `contradict_facts`, `update_fact(UpdateFactRequest)`, `remove_fact`, `list_facts`, `record_fact_feedback(FeedbackRequest)`, and `memory_status`.
- [ ] 50. Replace `record_decision` so it writes a `decision` fact, with `reason`, `files`, and `tags` incorporated into tags/entities/content in a predictable way.
- [ ] 51. Replace `record_code_area` so it writes or updates a `code_area` fact keyed by path and preserves touch-count semantics through tags or metadata if needed for old callers.
- [ ] 52. Replace `session_recall` so it delegates to `search_facts` when `query` exists and `list_facts` when omitted, preserving `since` and `limit` behavior where practical.
- [ ] 53. Add facade tests that cover typed request/response APIs, trust/feedback output, status trust buckets, and the three compatibility APIs against the same database.

### Phase 10: MCP Tooling

- [ ] 54. Add `tokensave_fact_store`, `tokensave_fact_feedback`, and `tokensave_memory_status` definitions in `src/mcp/tools/definitions.rs` with exact JSON schemas matching the MCP Surface section, Hermes-compatible field names, and clear guidance to use `probe` or `reason` before answering user/project-memory questions.
- [ ] 55. Rewrite `src/mcp/tools/handlers/memory.rs` to parse the action enum, validate required fields per action, call the new `TokenSave` facade, and format JSON output through the existing `ToolResult` envelope.
- [ ] 56. Update `src/mcp/tools/handlers/mod.rs` dispatch and `src/tool_command.rs` grouping for all new memory tools.
- [ ] 57. Add MCP handler tests for every `tokensave_fact_store` action, `tokensave_fact_feedback` helpful/unhelpful paths, feedback event fields, `tokensave_memory_status` trust distribution fields, malformed input, Hermes-compatible payload names, and the three compatibility wrappers.

### Phase 11: Documentation And Validation

- [ ] 58. Update `README.md`, `docs/USER-GUIDE.md`, and `src/mcp/server.rs` schema notes to describe the holographic fact store, trust scoring, entity recall, compositional reasoning, contradiction detection, compatibility wrappers, the Hermes integration mapping from `fact_store` / `fact_feedback` to `tokensave_` MCP tools, and the future Python interop split between PyO3/maturin and `pyo3_bindgen`.
- [ ] 59. Run focused validation: `cargo test memory_test`, `cargo test mcp_handler_test`, `cargo test migration_test`, and `cargo check`.
- [ ] 60. Run full validation with `cargo test`, review output for performance or flaky timing issues, then prepare a follow-up implementation summary and migration notes for users.

---

## Risk Notes

- The exact `amari-holographic` type paths must be confirmed by compilation because docs pages expose some items through re-exports and some through modules.
- `FHRRAlgebra<2048>` best matches Hermes phase HRR, but if the crate API makes deterministic phase construction awkward, use the nearest algebra that supports deterministic `from_coefficients` and document the deviation.
- SQLite migrations should not perform expensive vector rebuilds. Backfill text rows during migration, then compute missing vectors lazily on first memory use or through `memory_status`.
- Existing stable MCP callers should not break. Keep old tool names as wrappers even though the old tables stop receiving writes.
- Avoid Hermes' known failure modes: no silent HRR downgrade, sanitize FTS5 input, and include status reporting for missing vectors or bank rebuild needs.

## Execution Handoff

Plan complete. Recommended execution is subagent-driven, one phase at a time, with focused tests after each phase and no commits unless explicitly requested.
