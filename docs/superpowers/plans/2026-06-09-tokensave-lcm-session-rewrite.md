# TokenSave LCM Session Rewrite Implementation Plan

> **Rebrand note:** The project has since been renamed **TraceDecay** (binary/crate `tracedecay`, MCP tools `tracedecay_*`). This dated planning artifact keeps the TokenSave-era names it was written with; read `tokensave` / `tokensave_*` as `tracedecay` / `tracedecay_*` when applying it to the current codebase.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rewrite TokenSave's existing session internals into a lossless, LCM-grade session store inside the existing `sessions.db`, while preserving compatible `tokensave_message_search` behavior and adding deterministic LCM load/expand/status/compression APIs.

**Architecture:** Rust owns the durable LCM store, idempotent schema migrations, raw-message and summary-DAG state, bounded derived indexes/snippets, storage path containment, and public CLI/MCP JSON contracts. The generated Hermes Python plugin owns Hermes `ContextEngine` lifecycle registration and Hermes auxiliary LLM calls, calling Rust through the existing `tokensave tool ... --json --args` subprocess bridge. Codegraph `ContextBuilder` and `src/memory/*` fact storage stay separate from session compression and are only regression-tested for non-coupling.

**Tech Stack:** Rust 1.95, `libsql`/SQLite FTS5, `tokio`, `serde`/`serde_json`, generated Hermes Python plugin files, `python3 -m py_compile`, `cargo test`, `cargo fmt --all -- --check`, and `cargo clippy --all-targets`.

---

## Source Anchors

- Approved design: `docs/superpowers/specs/2026-06-09-tokensave-lcm-session-rewrite-design.md`.
- Current DB/session write path: `src/global_db.rs`, `src/sessions/mod.rs`, `src/sessions/source.rs`, `src/sessions/cursor.rs`.
- Current search tool: `src/mcp/tools/handlers/session.rs`, `src/mcp/tools/definitions.rs`, `src/mcp/tools/handlers/mod.rs`.
- Current Hermes generator and subprocess bridge: `src/agents/hermes.rs`, `src/tool_command.rs`.
- Existing tests to evolve: `tests/session_global_db_test.rs`, `tests/mcp_handler_test.rs`, `tests/agent_test.rs`.
- Current authoritative cap to remove from new writes: `MAX_SESSION_MESSAGE_TEXT_BYTES` in `src/global_db.rs`; after this plan, any cap belongs only to derived snippets, FTS/index text, MCP response truncation, or bounded rendering.

## File Structure Map

Create these focused Rust modules:

- `src/sessions/lcm/mod.rs` - public module boundary, exports types and store helpers.
- `src/sessions/lcm/types.rs` - stable Rust structs/enums for raw messages, payload refs, summary nodes, lifecycle state, query inputs, and JSON outputs.
- `src/sessions/lcm/schema.rs` - schema version constants and idempotent migration DDL for LCM tables inside the existing `sessions.db`.
- `src/sessions/lcm/store.rs` - `LcmStore<'db>` wrapper that binds `GlobalDb` plus an explicit storage root and exposes raw ingest, query, DAG, payload, and compression operations.
- `src/sessions/lcm/raw.rs` - lossless raw-message ingest, compatibility projection writes, legacy-row carry-forward, and authoritative content loading.
- `src/sessions/lcm/payload.rs` - externalized payload creation, hashing, permissions, basename-only refs, root containment, and paginated expansion.
- `src/sessions/lcm/dag.rs` - summary DAG persistence, source lineage, subtree expansion, and depth/window metadata.
- `src/sessions/lcm/query.rs` - search/load/describe/status query assembly, stable cursors, bounded snippets, and JSON result types.
- `src/sessions/lcm/compression.rs` - deterministic compression lifecycle primitives, fake summarizer injection, frontier/debt state transitions, and active-context assembly.
- `src/sessions/lcm/security.rs` - ingest-protection classification for large/binary-ish payloads, data URI/base64 detection, sensitive-redaction metadata, and integrity scan helpers.
- `src/sessions/lcm/hermes.rs` - Rust JSON request/response contracts used by the generated Hermes `ContextEngine` adapter.

Modify these existing Rust files:

- `src/sessions/mod.rs` - add `pub mod lcm;` and re-export the public LCM types required by handlers/tests.
- `src/global_db.rs` - call the LCM schema migration during `GlobalDb::open_at`, expose a crate-private connection accessor for LCM modules, and change `upsert_session_message` so new authoritative content is lossless while compatibility/index fields remain bounded.
- `src/sessions/source.rs` - route parsed provider messages through the LCM raw ingest path while preserving incremental parse offsets and provider-normalized session metadata.
- `src/sessions/cursor.rs` - keep `project_session_db_path(project_root).join("sessions.db")` behavior and pass the project-local `.tokensave` storage root into `LcmStore`.
- `src/mcp/tools/definitions.rs` - add additive LCM tool schemas for `tokensave_lcm_status`, `tokensave_lcm_load_session`, `tokensave_lcm_grep`, `tokensave_lcm_describe`, `tokensave_lcm_expand`, `tokensave_lcm_expand_query`, `tokensave_lcm_preflight`, and `tokensave_lcm_compress`.
- `src/mcp/tools/handlers/session.rs` - keep `handle_message_search` compatible and add LCM handlers that call deterministic Rust APIs.
- `src/mcp/tools/handlers/mod.rs` - register the new LCM handler names in `handle_tool_call`.
- `src/agents/hermes.rs` - generate the Hermes `ContextEngine` adapter, LCM bridge helpers, local/profile storage configuration, auxiliary summarizer calls, reasoning stripping, and Python tests embedded in Rust fixtures.
- `src/main.rs` - only if install flags need to pass explicit Hermes profile/locality metadata into `HermesIntegration`; otherwise leave unchanged.
- `src/tool_command.rs` - only if LCM bridge tests need stronger `--project` coverage for subprocess calls; keep the existing `--json --args` contract.

Create these tests:

- `tests/session_lcm_schema_test.rs` - schema migration, idempotency, legacy carry-forward, and single-DB assertions.
- `tests/session_lcm_raw_test.rs` - lossless raw ingest, capped derived snippets, compatibility projections, and authoritative load.
- `tests/session_lcm_payload_test.rs` - payload externalization, path containment, permissions, hashes, missing/unreferenced scans, and cross-session denial.
- `tests/session_lcm_dag_test.rs` - summary DAG persistence, source lineage, subtree expansion, and restart recovery.
- `tests/session_lcm_query_test.rs` - load/grep/describe/status APIs and deterministic pagination.
- `tests/session_lcm_compression_test.rs` - lifecycle/frontier/debt primitives with deterministic fake summarizers.
- `tests/session_lcm_ingest_protection_test.rs` - data URI/base64/large tool output protection and bounded index text.
- `tests/hermes_lcm_bridge_test.rs` - generated Python context engine, subprocess bridge calls, no-op/fake summarizer behavior, auxiliary LLM routing, reasoning stripping, and fallback behavior.

Modify these tests:

- `tests/session_global_db_test.rs` - replace capped-authoritative assertions with lossless storage assertions and keep search/filter compatibility.
- `tests/mcp_handler_test.rs` - assert new LCM tool definitions, handler dispatch, and unchanged `tokensave_message_search` provider/scope schema.
- `tests/agent_test.rs` - extend Hermes generated-plugin tests for `ContextEngine` registration, local/profile storage locality, Python compile checks, and non-overwrite memory provider behavior.

Do not modify these production areas except for regression tests proving separation:

- `src/context/builder.rs` - codegraph context retrieval remains independent from LCM.
- `src/memory/*` - fact memory remains independent from summary DAG state.

## Cross-Cutting Invariants

- The existing `sessions.db` is the only primary TokenSave-managed session database. Migrations evolve it in place; tests must fail if a second primary LCM DB path is introduced.
- New authoritative session content is lossless. `session_messages.text` may become a bounded compatibility projection, but the raw message store must preserve full content inline or through an externalized payload ref.
- Existing rows that were already capped remain best-effort legacy data and must be marked `legacy_truncated = true` when carried into the LCM raw store.
- FTS/index text, snippets, MCP response text, and display previews are derived and bounded. They must never be the only copy of new raw content.
- Storage roots are explicit: project-local installs use `crate::config::get_tokensave_dir(project_root)`; non-local Hermes profile installs use the selected Hermes profile directory plus `.tokensave`.
- Python bridge calls use `tokensave tool <name> --json --args <object>` and optional `--project <path>` when the context engine knows the project root. Do not introduce PyO3 or native bindings in this implementation.
- LCM summaries are not memory facts. `summary_nodes` and lifecycle/frontier rows live in the session DB, not in `src/memory/*`.

---

## Task 1: Session Schema Migration Foundation

**Files:**
- Create: `src/sessions/lcm/mod.rs`
- Create: `src/sessions/lcm/types.rs`
- Create: `src/sessions/lcm/schema.rs`
- Modify: `src/sessions/mod.rs`
- Modify: `src/global_db.rs`
- Create: `tests/session_lcm_schema_test.rs`

- [ ] **Step 1: Write failing schema migration tests**

Add tests that open an old-style `sessions.db`, run `GlobalDb::open_at`, and assert the LCM tables exist in that same file.

```rust
#[tokio::test]
async fn lcm_schema_migrates_legacy_sessions_db_in_place() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join(".tokensave").join("sessions.db");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    let old_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = old_db.connect().unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
            provider TEXT NOT NULL,
            session_id TEXT NOT NULL,
            project_key TEXT NOT NULL,
            project_path TEXT NOT NULL,
            title TEXT,
            started_at INTEGER,
            ended_at INTEGER,
            transcript_path TEXT,
            metadata_json TEXT,
            PRIMARY KEY(provider, session_id)
        );
        CREATE TABLE session_messages (
            provider TEXT NOT NULL,
            message_id TEXT NOT NULL,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL,
            timestamp INTEGER,
            ordinal INTEGER NOT NULL,
            text TEXT NOT NULL,
            kind TEXT,
            model TEXT,
            tool_names TEXT,
            source_path TEXT,
            source_offset INTEGER,
            metadata_json TEXT,
            PRIMARY KEY(provider, message_id)
        );
        INSERT INTO sessions(provider, session_id, project_key, project_path)
        VALUES ('cursor', 'legacy-session', '/tmp/project', '/tmp/project');
        INSERT INTO session_messages(provider, message_id, session_id, role, ordinal, text)
        VALUES ('cursor', 'legacy-message', 'legacy-session', 'assistant', 1, 'legacy text');",
    ).await.unwrap();
    drop(conn);
    drop(old_db);

    let db = tokensave::global_db::GlobalDb::open_at(&db_path).await.unwrap();
    assert_eq!(db.lcm_schema_version().await.unwrap(), tokensave::sessions::lcm::LCM_SCHEMA_VERSION);

    let legacy = db
        .lcm_load_raw_message("cursor", "legacy-message")
        .await
        .expect("legacy message should be carried into raw store");
    assert_eq!(legacy.session_id, "legacy-session");
    assert_eq!(legacy.content, "legacy text");
    assert!(legacy.legacy_source);
    assert!(!legacy.legacy_truncated);
}
```

- [ ] **Step 2: Run the red test**

Run: `cargo test --test session_lcm_schema_test lcm_schema_migrates_legacy_sessions_db_in_place -- --nocapture`

Expected: FAIL with missing `tokensave::sessions::lcm`, missing `GlobalDb::lcm_schema_version`, or missing `GlobalDb::lcm_load_raw_message`.

- [ ] **Step 3: Add LCM module boundary and schema version types**

Sketch:

```rust
// src/sessions/lcm/mod.rs
pub mod schema;
pub mod types;

pub use schema::LCM_SCHEMA_VERSION;
pub use types::{LcmRawMessage, LcmStorageKind};
```

```rust
// src/sessions/lcm/types.rs
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmRawMessage {
    pub provider: String,
    pub message_id: String,
    pub session_id: String,
    pub store_id: i64,
    pub role: String,
    pub ordinal: i64,
    pub timestamp: Option<i64>,
    pub content: String,
    pub content_hash: String,
    pub storage_kind: LcmStorageKind,
    pub payload_ref: Option<String>,
    pub legacy_source: bool,
    pub legacy_truncated: bool,
    pub metadata_json: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LcmStorageKind {
    Inline,
    External,
}
```

- [ ] **Step 4: Add idempotent schema migration inside `GlobalDb::open_at`**

Sketch:

```rust
// src/sessions/lcm/schema.rs
pub const LCM_SCHEMA_VERSION: i64 = 1;

pub(crate) async fn ensure_lcm_schema(conn: &libsql::Connection) -> Option<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS session_schema_migrations (
            name TEXT PRIMARY KEY,
            version INTEGER NOT NULL,
            applied_at INTEGER NOT NULL DEFAULT (unixepoch())
        );
        CREATE TABLE IF NOT EXISTS lcm_raw_messages (
            provider TEXT NOT NULL,
            message_id TEXT NOT NULL,
            session_id TEXT NOT NULL,
            store_id INTEGER PRIMARY KEY AUTOINCREMENT,
            role TEXT NOT NULL,
            ordinal INTEGER NOT NULL,
            timestamp INTEGER,
            content TEXT,
            content_hash TEXT NOT NULL,
            storage_kind TEXT NOT NULL CHECK(storage_kind IN ('inline', 'external')),
            payload_ref TEXT,
            snippet_text TEXT NOT NULL,
            index_text TEXT NOT NULL,
            legacy_source INTEGER NOT NULL DEFAULT 0,
            legacy_truncated INTEGER NOT NULL DEFAULT 0,
            metadata_json TEXT,
            UNIQUE(provider, message_id),
            FOREIGN KEY(provider, session_id) REFERENCES sessions(provider, session_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_lcm_raw_session_order
            ON lcm_raw_messages(provider, session_id, store_id);
        CREATE VIRTUAL TABLE IF NOT EXISTS lcm_raw_messages_fts USING fts5(
            index_text, role, metadata_json,
            content='lcm_raw_messages',
            content_rowid='store_id'
        );",
    ).await.ok()?;

    carry_forward_legacy_messages(conn).await?;
    conn.execute(
        "INSERT INTO session_schema_migrations(name, version)
         VALUES ('lcm', ?1)
         ON CONFLICT(name) DO UPDATE SET version = excluded.version, applied_at = unixepoch()",
        libsql::params![LCM_SCHEMA_VERSION],
    ).await.ok()?;
    Some(())
}
```

Call `crate::sessions::lcm::schema::ensure_lcm_schema(&conn).await?;` immediately after the existing `ensure_session_parent_columns(&conn).await?;` in `GlobalDb::open_at`.

- [ ] **Step 5: Add introspection helpers used by tests**

Sketch:

```rust
impl GlobalDb {
    pub async fn lcm_schema_version(&self) -> Option<i64> {
        let mut rows = self.conn.query(
            "SELECT version FROM session_schema_migrations WHERE name = 'lcm'",
            (),
        ).await.ok()?;
        rows.next().await.ok()??.get::<i64>(0).ok()
    }

    pub async fn lcm_load_raw_message(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Option<crate::sessions::lcm::LcmRawMessage> {
        crate::sessions::lcm::schema::load_raw_message(&self.conn, provider, message_id).await
    }
}
```

- [ ] **Step 6: Prove migration idempotency**

Add `lcm_schema_migration_is_idempotent` in `tests/session_lcm_schema_test.rs` that calls `GlobalDb::open_at(&db_path)` twice and asserts:

```rust
assert_eq!(count_rows(&db_path, "lcm_raw_messages").await, 1);
assert_eq!(schema_version(&db_path).await, tokensave::sessions::lcm::LCM_SCHEMA_VERSION);
```

Run: `cargo test --test session_lcm_schema_test -- --nocapture`

Expected: PASS.

- [ ] **Step 7: Commit checkpoint**

```bash
git add src/sessions/mod.rs src/sessions/lcm/mod.rs src/sessions/lcm/types.rs src/sessions/lcm/schema.rs src/global_db.rs tests/session_lcm_schema_test.rs
git commit -m "Add LCM session schema migrations"
```

## Task 2: Lossless Raw-Message Ingest Model

**Files:**
- Create: `src/sessions/lcm/raw.rs`
- Create: `src/sessions/lcm/store.rs`
- Modify: `src/sessions/lcm/mod.rs`
- Modify: `src/sessions/lcm/types.rs`
- Modify: `src/global_db.rs`
- Modify: `src/sessions/source.rs`
- Modify: `tests/session_global_db_test.rs`
- Create: `tests/session_lcm_raw_test.rs`

- [ ] **Step 1: Replace the capped-authoritative regression test with a lossless red test**

In `tests/session_global_db_test.rs`, replace `upsert_session_message_truncates_oversized_text_deterministically` with:

```rust
#[tokio::test]
async fn upsert_session_message_preserves_oversized_text_losslessly() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    let session = sample_session("cursor", "session-1", "project-a");
    db.upsert_session(&session).await;

    let oversized = format!("{}{}", "x".repeat(300_000), "::lossless-tail");
    let message = sample_message("cursor", "message-1", "session-1", &oversized);
    assert!(db.upsert_session_message(&message).await);

    let compatibility = db
        .get_session_message("cursor", "message-1")
        .await
        .expect("compatibility message should exist");
    assert!(compatibility.text.len() <= tokensave::sessions::lcm::MAX_DERIVED_TEXT_CHARS);
    assert!(compatibility.text.contains("[derived snippet truncated by tokensave]"));

    let raw = db
        .lcm_load_raw_message("cursor", "message-1")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.content, oversized);
    assert!(raw.content.ends_with("::lossless-tail"));
    assert!(!raw.legacy_source);
    assert!(!raw.legacy_truncated);
}
```

- [ ] **Step 2: Run the red test**

Run: `cargo test --test session_global_db_test upsert_session_message_preserves_oversized_text_losslessly -- --nocapture`

Expected: FAIL because `GlobalDb::upsert_session_message` still calls `capped_session_message_text` before storing the only authoritative text copy.

- [ ] **Step 3: Add explicit derived-text helpers**

Sketch:

```rust
// src/sessions/lcm/types.rs
pub const MAX_DERIVED_TEXT_CHARS: usize = 64 * 1024;
pub const DERIVED_TRUNCATION_MARKER: &str = "\n[derived snippet truncated by tokensave]";
```

```rust
// src/sessions/lcm/raw.rs
pub fn derived_text_for_index(raw: &str) -> String {
    if raw.chars().count() <= MAX_DERIVED_TEXT_CHARS {
        return raw.to_string();
    }
    let mut out = raw.chars().take(MAX_DERIVED_TEXT_CHARS).collect::<String>();
    out.push_str(DERIVED_TRUNCATION_MARKER);
    out
}
```

- [ ] **Step 4: Add raw ingest API and make `upsert_session_message` write both layers**

Sketch:

```rust
impl GlobalDb {
    pub async fn upsert_session_message(&self, message: &SessionMessageRecord) -> bool {
        let Some(raw) = crate::sessions::lcm::raw::prepare_raw_message(message) else {
            return false;
        };
        if !crate::sessions::lcm::raw::upsert_raw_message(&self.conn, &raw).await {
            return false;
        }
        let derived = crate::sessions::lcm::raw::derived_text_for_index(&message.text);
        self.upsert_session_message_projection(message, &derived).await
    }
}
```

Keep the existing `session_messages` table and FTS triggers as compatibility projections. Move the previous SQL body into a private `upsert_session_message_projection`.

- [ ] **Step 5: Route transcript ingestion through the same write path**

`src/sessions/source.rs` already calls `db.upsert_session_message(message)`. Keep that call so all providers inherit lossless behavior through one path. Add a test in `tests/session_lcm_raw_test.rs` using a fake `TranscriptSource` that emits a 300 KiB assistant message and assert the raw message tail survives.

Run: `cargo test --test session_lcm_raw_test transcript_ingest_preserves_lossless_raw_content -- --nocapture`

Expected before implementation: FAIL with missing fake-source helpers or missing raw content. Expected after implementation: PASS.

- [ ] **Step 6: Prove search still uses bounded compatibility text**

Add `search_uses_bounded_projection_but_load_recovers_raw`:

```rust
let results = db.search_session_messages("cursor", Some("project-a"), "unique-search-token", 10).await;
assert_eq!(results.len(), 1);
assert!(results[0].message.text.len() <= tokensave::sessions::lcm::MAX_DERIVED_TEXT_CHARS + 64);
assert_eq!(
    db.lcm_load_raw_message("cursor", "message-1").await.unwrap().content,
    oversized
);
```

Run: `cargo test --test session_lcm_raw_test -- --nocapture`

Expected: PASS.

- [ ] **Step 7: Commit checkpoint**

```bash
git add src/global_db.rs src/sessions/source.rs src/sessions/lcm/mod.rs src/sessions/lcm/types.rs src/sessions/lcm/raw.rs src/sessions/lcm/store.rs tests/session_global_db_test.rs tests/session_lcm_raw_test.rs
git commit -m "Make session raw ingest lossless"
```

## Task 3: External Payload Containment and Derived Index Separation

**Files:**
- Create: `src/sessions/lcm/payload.rs`
- Create: `src/sessions/lcm/security.rs`
- Modify: `src/sessions/lcm/mod.rs`
- Modify: `src/sessions/lcm/types.rs`
- Modify: `src/sessions/lcm/schema.rs`
- Modify: `src/sessions/lcm/raw.rs`
- Create: `tests/session_lcm_payload_test.rs`
- Create: `tests/session_lcm_ingest_protection_test.rs`

- [ ] **Step 1: Write failing externalization tests**

Add `externalizes_large_tool_payload_with_recoverable_ref`:

```rust
#[tokio::test]
async fn externalizes_large_tool_payload_with_recoverable_ref() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = open_lcm_db(tmp.path()).await;
    insert_session(&db, "cursor", "session-1").await;

    let payload = format!("tool output\n{}", "A".repeat(900_000));
    let message = raw_message("cursor", "tool-1", "session-1", "tool", &payload)
        .with_kind("tool_result");
    db.lcm_store(tmp.path().join(".tokensave"))
        .ingest_raw_message(&message)
        .await
        .unwrap();

    let raw = db.lcm_load_raw_message("cursor", "tool-1").await.unwrap();
    assert_eq!(raw.storage_kind, LcmStorageKind::External);
    assert!(raw.payload_ref.as_deref().unwrap().ends_with(".payload"));
    assert_eq!(
        db.lcm_expand_payload("cursor", "session-1", raw.payload_ref.as_deref().unwrap(), 0, payload.len())
            .await
            .unwrap()
            .content,
        payload
    );
}
```

Run: `cargo test --test session_lcm_payload_test externalizes_large_tool_payload_with_recoverable_ref -- --nocapture`

Expected: FAIL because payload externalization APIs do not exist.

- [ ] **Step 2: Define payload metadata and storage root API**

Sketch:

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmPayloadRef {
    pub payload_ref: String,
    pub provider: String,
    pub session_id: String,
    pub message_id: String,
    pub kind: String,
    pub content_hash: String,
    pub byte_count: u64,
    pub char_count: u64,
    pub created_at: i64,
}
```

```rust
pub fn payload_dir(storage_root: &Path) -> PathBuf {
    storage_root.join("lcm-payloads")
}
```

For project-local Cursor/Hermes installs, `storage_root` is project `.tokensave`. For non-local Hermes profile installs, `storage_root` is the selected Hermes profile root joined with `.tokensave`.

- [ ] **Step 3: Add schema for payload refs**

Add to `ensure_lcm_schema`:

```sql
CREATE TABLE IF NOT EXISTS lcm_external_payloads (
    payload_ref TEXT PRIMARY KEY,
    provider TEXT NOT NULL,
    session_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    byte_count INTEGER NOT NULL,
    char_count INTEGER NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    metadata_json TEXT,
    UNIQUE(provider, message_id, payload_ref),
    FOREIGN KEY(provider, session_id) REFERENCES sessions(provider, session_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_lcm_external_payloads_owner
    ON lcm_external_payloads(provider, session_id);
```

- [ ] **Step 4: Implement basename-only refs, containment, permissions, and hashes**

Sketch:

```rust
pub fn validate_payload_ref(payload_ref: &str) -> Result<&str> {
    let path = std::path::Path::new(payload_ref);
    let is_basename = path.components().count() == 1
        && !payload_ref.contains('/')
        && !payload_ref.contains('\\')
        && payload_ref != "."
        && payload_ref != "..";
    if is_basename { Ok(payload_ref) } else { Err(LcmError::InvalidPayloadRef) }
}

pub async fn write_external_payload(
    root: &Path,
    provider: &str,
    session_id: &str,
    message_id: &str,
    content: &str,
) -> Result<LcmPayloadRef> {
    let hash = sha256_hex(content.as_bytes());
    let payload_ref = format!("{provider}-{session_id}-{message_id}-{hash}.payload");
    validate_payload_ref(&payload_ref)?;
    let dir = payload_dir(root);
    create_private_dir(&dir)?;
    let path = dir.join(&payload_ref);
    ensure_contained(&dir, &path)?;
    write_private_file(&path, content.as_bytes())?;
    Ok(LcmPayloadRef { payload_ref, provider: provider.to_string(), session_id: session_id.to_string(), message_id: message_id.to_string(), kind: "message".to_string(), content_hash: hash, byte_count: content.len() as u64, char_count: content.chars().count() as u64, created_at: unixepoch(), metadata_json: None })
}
```

- [ ] **Step 5: Add protection tests for unsafe refs and cross-session expansion**

Tests:

```rust
#[test]
fn rejects_payload_ref_path_traversal() {
    for bad in ["../secret", "/tmp/secret", "nested/file", r"nested\file", ".", ".."] {
        assert!(tokensave::sessions::lcm::payload::validate_payload_ref(bad).is_err());
    }
}
```

```rust
#[tokio::test]
async fn denies_cross_session_payload_expansion() {
    let ref_for_a = insert_external_payload(&db, "cursor", "session-a", "message-a", "secret").await;
    let denied = db.lcm_expand_payload("cursor", "session-b", &ref_for_a, 0, 100).await;
    assert!(matches!(denied, Err(LcmError::PayloadNotOwnedBySession)));
}
```

Run: `cargo test --test session_lcm_payload_test -- --nocapture`

Expected: PASS after containment implementation.

- [ ] **Step 6: Add data URI/base64/large-output classification tests**

`tests/session_lcm_ingest_protection_test.rs`:

```rust
#[test]
fn classifies_data_uri_and_long_base64_for_externalization() {
    let data_uri = format!("data:image/png;base64,{}", "A".repeat(20_000));
    assert!(should_externalize("assistant", Some("tool_result"), &data_uri));

    let base64_run = "Q".repeat(80_000);
    assert!(should_externalize("assistant", Some("message"), &base64_run));

    assert!(!should_externalize("assistant", Some("message"), "short useful text"));
}
```

Run: `cargo test --test session_lcm_ingest_protection_test -- --nocapture`

Expected: PASS after `src/sessions/lcm/security.rs` classifies large/binary-ish content.

- [ ] **Step 7: Commit checkpoint**

```bash
git add src/sessions/lcm/mod.rs src/sessions/lcm/types.rs src/sessions/lcm/schema.rs src/sessions/lcm/raw.rs src/sessions/lcm/payload.rs src/sessions/lcm/security.rs tests/session_lcm_payload_test.rs tests/session_lcm_ingest_protection_test.rs
git commit -m "Add LCM payload containment"
```

## Task 4: Summary DAG Tables, Types, and Lineage Expansion

**Files:**
- Create: `src/sessions/lcm/dag.rs`
- Modify: `src/sessions/lcm/mod.rs`
- Modify: `src/sessions/lcm/types.rs`
- Modify: `src/sessions/lcm/schema.rs`
- Create: `tests/session_lcm_dag_test.rs`

- [ ] **Step 1: Write failing DAG persistence test**

```rust
#[tokio::test]
async fn summary_node_preserves_source_lineage_and_expands_sources() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = open_lcm_db(tmp.path()).await;
    insert_raw_messages(&db, "cursor", "session-1", ["alpha", "beta", "gamma"]).await;

    let node = db.lcm_insert_summary_node(LcmSummaryNodeDraft {
        provider: "cursor".into(),
        conversation_id: "conversation-1".into(),
        session_id: "session-1".into(),
        depth: 0,
        summary_text: "alpha through gamma".into(),
        source_refs: vec![
            LcmSourceRef::RawMessage { store_id: 1 },
            LcmSourceRef::RawMessage { store_id: 2 },
            LcmSourceRef::RawMessage { store_id: 3 },
        ],
        source_token_count: 30,
        summary_token_count: 4,
        source_time_start: Some(1_715_000_000),
        source_time_end: Some(1_715_000_030),
        expand_hint: Some("3 raw messages".into()),
        metadata_json: None,
    }).await.unwrap();

    let expanded = db.lcm_expand_summary_node("cursor", "session-1", &node.node_id).await.unwrap();
    assert_eq!(expanded.sources.len(), 3);
    assert_eq!(expanded.sources[0].content, "alpha");
    assert_eq!(expanded.summary.summary_text, "alpha through gamma");
}
```

Run: `cargo test --test session_lcm_dag_test summary_node_preserves_source_lineage_and_expands_sources -- --nocapture`

Expected: FAIL with missing `LcmSummaryNodeDraft`, `LcmSourceRef`, and DAG APIs.

- [ ] **Step 2: Add DAG schema**

Add:

```sql
CREATE TABLE IF NOT EXISTS lcm_summary_nodes (
    node_id TEXT PRIMARY KEY,
    provider TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    depth INTEGER NOT NULL,
    summary_text TEXT NOT NULL,
    summary_hash TEXT NOT NULL,
    summary_token_count INTEGER NOT NULL,
    source_token_count INTEGER NOT NULL,
    source_time_start INTEGER,
    source_time_end INTEGER,
    expand_hint TEXT,
    metadata_json TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE TABLE IF NOT EXISTS lcm_summary_sources (
    node_id TEXT NOT NULL,
    source_kind TEXT NOT NULL CHECK(source_kind IN ('raw_message', 'summary_node')),
    source_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    PRIMARY KEY(node_id, ordinal),
    FOREIGN KEY(node_id) REFERENCES lcm_summary_nodes(node_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_lcm_summary_nodes_session
    ON lcm_summary_nodes(provider, session_id, depth, created_at);
CREATE VIRTUAL TABLE IF NOT EXISTS lcm_summary_nodes_fts USING fts5(
    summary_text, expand_hint, metadata_json,
    content='lcm_summary_nodes',
    content_rowid='rowid'
);
```

- [ ] **Step 3: Define stable DAG types**

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LcmSourceRef {
    RawMessage { store_id: i64 },
    SummaryNode { node_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmSummaryNode {
    pub node_id: String,
    pub provider: String,
    pub conversation_id: String,
    pub session_id: String,
    pub depth: i64,
    pub summary_text: String,
    pub source_refs: Vec<LcmSourceRef>,
    pub summary_token_count: i64,
    pub source_token_count: i64,
    pub source_time_start: Option<i64>,
    pub source_time_end: Option<i64>,
    pub expand_hint: Option<String>,
    pub metadata_json: Option<String>,
}
```

- [ ] **Step 4: Implement deterministic node IDs and source expansion**

Use a deterministic ID to make tests stable:

```rust
pub fn summary_node_id(provider: &str, session_id: &str, depth: i64, source_refs: &[LcmSourceRef], summary_text: &str) -> String {
    let input = serde_json::json!({
        "provider": provider,
        "session_id": session_id,
        "depth": depth,
        "source_refs": source_refs,
        "summary_hash": sha256_hex(summary_text.as_bytes()),
    });
    format!("sum_{}", sha256_hex(input.to_string().as_bytes()))
}
```

Expansion must check `(provider, session_id)` ownership before returning raw messages or child summary nodes.

- [ ] **Step 5: Add restart recovery test**

Create `summary_dag_survives_reopen` that inserts a node, drops `GlobalDb`, reopens the same `sessions.db`, and expands the node.

Run: `cargo test --test session_lcm_dag_test -- --nocapture`

Expected: PASS.

- [ ] **Step 6: Commit checkpoint**

```bash
git add src/sessions/lcm/mod.rs src/sessions/lcm/types.rs src/sessions/lcm/schema.rs src/sessions/lcm/dag.rs tests/session_lcm_dag_test.rs
git commit -m "Add LCM summary DAG storage"
```

## Task 5: Search, Load, Expand, Describe, and Status Rust APIs

**Files:**
- Create: `src/sessions/lcm/query.rs`
- Modify: `src/sessions/lcm/mod.rs`
- Modify: `src/sessions/lcm/types.rs`
- Modify: `src/sessions/lcm/raw.rs`
- Modify: `src/sessions/lcm/dag.rs`
- Modify: `src/sessions/lcm/payload.rs`
- Create: `tests/session_lcm_query_test.rs`

- [ ] **Step 1: Write failing query API tests**

```rust
#[tokio::test]
async fn load_session_returns_ordered_raw_pages_with_stable_cursor() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = open_lcm_db(tmp.path()).await;
    insert_raw_messages(&db, "cursor", "session-1", ["one", "two", "three"]).await;

    let page = db.lcm_load_session(LcmLoadSessionRequest {
        provider: "cursor".into(),
        session_id: "session-1".into(),
        after_store_id: None,
        limit: 2,
        role: None,
        start_time: None,
        end_time: None,
        content_slice: None,
    }).await.unwrap();

    assert_eq!(page.messages.iter().map(|m| m.content.as_str()).collect::<Vec<_>>(), ["one", "two"]);
    assert_eq!(page.next_cursor.as_deref(), Some("2"));

    let second = db.lcm_load_session(LcmLoadSessionRequest {
        after_store_id: Some(2),
        ..page.request_for_next()
    }).await.unwrap();
    assert_eq!(second.messages[0].content, "three");
    assert!(second.next_cursor.is_none());
}
```

Run: `cargo test --test session_lcm_query_test load_session_returns_ordered_raw_pages_with_stable_cursor -- --nocapture`

Expected: FAIL because query request/response types do not exist.

- [ ] **Step 2: Add query request/response types**

Sketch:

```rust
pub struct LcmLoadSessionRequest {
    pub provider: String,
    pub session_id: String,
    pub after_store_id: Option<i64>,
    pub limit: usize,
    pub role: Option<String>,
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    pub content_slice: Option<LcmContentSlice>,
}

pub struct LcmGrepRequest {
    pub provider: String,
    pub query: String,
    pub scope: LcmScope,
    pub session_id: Option<String>,
    pub include_summaries: bool,
    pub limit: usize,
}

pub enum LcmScope {
    Current,
    Session,
    All,
}
```

- [ ] **Step 3: Implement load and expand with pagination**

Rules:

- `limit` clamps to `1..=100`.
- `content_slice` returns bounded `content` plus `content_range`, never silently discards raw content.
- External payload expansion uses `LcmPayloadRef` ownership checks from Task 3.
- Summary expansion uses DAG source ownership checks from Task 4.

Run: `cargo test --test session_lcm_query_test load_session_returns_ordered_raw_pages_with_stable_cursor -- --nocapture`

Expected: PASS.

- [ ] **Step 4: Write and implement combined grep/status/describe tests**

Tests:

```rust
#[tokio::test]
async fn grep_searches_raw_snippets_and_summary_nodes() {
    let hits = db.lcm_grep(LcmGrepRequest {
        provider: "cursor".into(),
        query: "billing migration".into(),
        scope: LcmScope::Session,
        session_id: Some("session-1".into()),
        include_summaries: true,
        limit: 10,
    }).await.unwrap();
    assert!(hits.iter().any(|hit| hit.kind == "raw_message"));
    assert!(hits.iter().any(|hit| hit.kind == "summary_node"));
    assert!(hits.iter().all(|hit| hit.snippet.len() <= MAX_DERIVED_TEXT_CHARS + 64));
}
```

```rust
#[tokio::test]
async fn status_reports_schema_frontier_payload_and_debt_counts() {
    let status = db.lcm_status("cursor", Some("session-1")).await.unwrap();
    assert_eq!(status.schema_version, LCM_SCHEMA_VERSION);
    assert_eq!(status.raw_message_count, 3);
    assert_eq!(status.summary_node_count, 1);
    assert_eq!(status.missing_payload_count, 0);
    assert_eq!(status.maintenance_debt_count, 0);
}
```

Run: `cargo test --test session_lcm_query_test -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Commit checkpoint**

```bash
git add src/sessions/lcm/mod.rs src/sessions/lcm/types.rs src/sessions/lcm/raw.rs src/sessions/lcm/dag.rs src/sessions/lcm/payload.rs src/sessions/lcm/query.rs tests/session_lcm_query_test.rs
git commit -m "Add LCM query APIs"
```

## Task 6: MCP Tool Definitions, Handlers, and Message Search Compatibility

**Files:**
- Modify: `src/mcp/tools/definitions.rs`
- Modify: `src/mcp/tools/handlers/session.rs`
- Modify: `src/mcp/tools/handlers/mod.rs`
- Modify: `tests/mcp_handler_test.rs`
- Modify: `tests/session_global_db_test.rs`

- [ ] **Step 1: Write failing tool-definition tests**

In `tests/mcp_handler_test.rs`:

```rust
#[test]
fn lcm_tool_schemas_are_registered_with_stable_names() {
    let tools = get_tool_definitions();
    let names = tools.iter().map(|tool| tool.name.as_str()).collect::<std::collections::BTreeSet<_>>();

    for expected in [
        "tokensave_lcm_status",
        "tokensave_lcm_load_session",
        "tokensave_lcm_grep",
        "tokensave_lcm_describe",
        "tokensave_lcm_expand",
        "tokensave_lcm_expand_query",
        "tokensave_lcm_preflight",
        "tokensave_lcm_compress",
    ] {
        assert!(names.contains(expected), "missing {expected}");
    }
}
```

Run: `cargo test --test mcp_handler_test lcm_tool_schemas_are_registered_with_stable_names -- --nocapture`

Expected: FAIL because definitions are not registered.

- [ ] **Step 2: Add additive definitions with precise JSON schemas**

Sketch for one tool:

```rust
fn def_lcm_load_session() -> ToolDefinition {
    def(
        "tokensave_lcm_load_session",
        "LCM Load Session",
        "Load ordered lossless raw session messages with pagination and bounded response slicing.",
        json!({
            "type": "object",
            "properties": {
                "provider": {"type": "string", "description": "Provider id, default cursor."},
                "session_id": {"type": "string", "description": "Provider-local session id."},
                "after_store_id": {"type": "number", "description": "Return rows after this raw store id."},
                "limit": {"type": "number", "description": "Maximum rows, clamped to 100."},
                "role": {"type": "string", "description": "Optional role filter."},
                "content_offset": {"type": "number", "description": "Character offset for content slice."},
                "content_limit": {"type": "number", "description": "Maximum characters returned per message."}
            },
            "required": ["session_id"]
        }),
    )
}
```

Use `def_rw` for `tokensave_lcm_preflight` and `tokensave_lcm_compress` because they ingest or mutate lifecycle state.

- [ ] **Step 3: Add handler dispatch and JSON output contracts**

Sketch:

```rust
pub(super) async fn handle_lcm_load_session(cg: &TokenSave, args: Value) -> Result<ToolResult> {
    let provider = string_arg(&args, "provider").unwrap_or("cursor");
    let session_id = required_string_arg(&args, "session_id")?;
    let db = open_project_session_db_or_unavailable(cg.project_root()).await?;
    let page = db.lcm_load_session(LcmLoadSessionRequest::from_args(provider, session_id, &args)?).await?;
    Ok(tool_json(&json!({
        "status": "ok",
        "provider": provider,
        "session_id": session_id,
        "messages": page.messages,
        "next_cursor": page.next_cursor,
    })))
}
```

Keep `truncate_response` at the outer MCP rendering layer, not in raw storage.

- [ ] **Step 4: Preserve `tokensave_message_search` behavior**

Add a compatibility test that reuses current assertions:

```rust
#[tokio::test]
async fn message_search_preserves_provider_project_parent_scope_shape_after_lcm() {
    let (cg, _dir) = setup_project().await;
    seed_lcm_and_projection_rows(cg.project_root()).await;

    let result = handle_tool_call(
        &cg,
        "tokensave_message_search",
        json!({
            "query": "orchard dispatch",
            "provider": "cursor",
            "project_key": cg.project_root().to_string_lossy(),
            "scope": "subagents_only",
            "parent_session_id": "parent",
            "limit": 10
        }),
        None,
        None,
    ).await.unwrap();

    let payload: serde_json::Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["scope"], "subagents_only");
    assert_eq!(payload["results"].as_array().unwrap().len(), 1);
    assert!(payload["results"][0]["message"].get("text").is_some());
}
```

Run: `cargo test --test mcp_handler_test message_search_preserves_provider_project_parent_scope_shape_after_lcm -- --nocapture`

Expected: PASS after handler compatibility is wired.

- [ ] **Step 5: Add CLI bridge smoke test for `tokensave tool ... --json --args`**

In `tests/mcp_handler_test.rs` or a new CLI integration test:

```rust
let output = std::process::Command::new(env!("CARGO_BIN_EXE_tokensave"))
    .current_dir(cg.project_root())
    .args([
        "tool",
        "tokensave_lcm_status",
        "--json",
        "--args",
        r#"{"provider":"cursor"}"#,
    ])
    .output()
    .unwrap();
assert!(output.status.success());
let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
assert_eq!(json["content"][0]["type"], "text");
```

Run: `cargo test --test mcp_handler_test lcm_tool_schemas_are_registered_with_stable_names message_search_preserves_provider_project_parent_scope_shape_after_lcm -- --nocapture`

Expected: PASS.

- [ ] **Step 6: Commit checkpoint**

```bash
git add src/mcp/tools/definitions.rs src/mcp/tools/handlers/session.rs src/mcp/tools/handlers/mod.rs tests/mcp_handler_test.rs tests/session_global_db_test.rs
git commit -m "Expose LCM session tools"
```

## Task 7: Hermes Context Engine Registration and Storage Locality

**Files:**
- Modify: `src/agents/hermes.rs`
- Modify: `tests/agent_test.rs`
- Create: `tests/hermes_lcm_bridge_test.rs`

- [ ] **Step 1: Write failing generated-plugin registration test**

In `tests/agent_test.rs`:

```rust
#[test]
fn test_hermes_generated_python_registers_lcm_context_engine() {
    let home = TempDir::new().unwrap();
    HermesIntegration.install(&make_install_ctx(home.path())).unwrap();
    let init_py = std::fs::read_to_string(home.path().join(".hermes/plugins/tokensave/__init__.py")).unwrap();

    assert!(init_py.contains("class TokenSaveContextEngine"));
    assert!(init_py.contains("ctx.register_context_engine"));
    assert!(init_py.contains("tools.call_tokensave_tool(\"tokensave_lcm_preflight\""));
    assert!(init_py.contains("tools.call_tokensave_tool(\"tokensave_lcm_compress\""));
}
```

Run: `cargo test --test agent_test test_hermes_generated_python_registers_lcm_context_engine -- --nocapture`

Expected: FAIL because generated Python does not register a context engine yet.

- [ ] **Step 2: Generate explicit storage locality metadata**

Add helper output in `plugin_init()`:

```python
def _storage_args(project_root=None, hermes_home=None):
    args = {}
    if project_root:
        args["storage_scope"] = "project_local"
        args["project_root"] = str(project_root)
    elif hermes_home:
        args["storage_scope"] = "hermes_profile"
        args["hermes_home"] = str(hermes_home)
    else:
        args["storage_scope"] = "hermes_profile"
    return args
```

Local Hermes install (`tokensave install --local --agent hermes` without `--profile`) continues to write under `project/.hermes/plugins/tokensave` and instructs users to launch with `HERMES_HOME=project/.hermes`. The LCM storage args still point Rust at the project `.tokensave/sessions.db` when `project_root` is known.

Non-local/profile Hermes install uses the active Hermes home/profile as the storage root; Rust resolves the session DB at `<selected-hermes-profile>/.tokensave/sessions.db`.

- [ ] **Step 3: Add generated `TokenSaveContextEngine` skeleton**

Sketch:

```python
class TokenSaveContextEngine:
    def __init__(self):
        self.hermes_home = None
        self.project_root = None
        self.active_session_id = None

    def initialize(self, session_id=None, hermes_home=None, project_root=None, **kwargs):
        self.active_session_id = session_id
        self.hermes_home = hermes_home
        self.project_root = project_root or kwargs.get("cwd")

    def should_compress_preflight(self, messages, **kwargs):
        args = _storage_args(self.project_root, self.hermes_home)
        args.update({"session_id": self.active_session_id, "messages": messages})
        return json.loads(tools.call_tokensave_tool("tokensave_lcm_preflight", args))

    def compress(self, messages, current_tokens=None, focus_topic=None, **kwargs):
        args = _storage_args(self.project_root, self.hermes_home)
        args.update({
            "session_id": self.active_session_id,
            "messages": messages,
            "current_tokens": current_tokens,
            "focus_topic": focus_topic,
        })
        return json.loads(tools.call_tokensave_tool("tokensave_lcm_compress", args))
```

In `register(ctx)`, call `ctx.register_context_engine(TokenSaveContextEngine())` only when the method exists, matching the current optional registration style for commands and memory providers.

- [ ] **Step 4: Add Python compile and fake context tests**

In `tests/hermes_lcm_bridge_test.rs`, install the plugin into a temp home and run Python that imports `__init__.py`, supplies a fake `ctx`, and asserts `register_context_engine` receives a `TokenSaveContextEngine`.

Run: `cargo test --test hermes_lcm_bridge_test generated_context_engine_registers_when_supported -- --nocapture`

Expected: PASS after generated Python changes.

- [ ] **Step 5: Commit checkpoint**

```bash
git add src/agents/hermes.rs tests/agent_test.rs tests/hermes_lcm_bridge_test.rs
git commit -m "Register Hermes LCM context engine"
```

## Task 8: Python Bridge Calls and Deterministic No-Op/Fake Summarizer Tests

**Files:**
- Modify: `src/agents/hermes.rs`
- Modify: `src/sessions/lcm/hermes.rs`
- Modify: `src/sessions/lcm/types.rs`
- Modify: `tests/hermes_lcm_bridge_test.rs`

- [ ] **Step 1: Write failing bridge-call argument test**

In `tests/hermes_lcm_bridge_test.rs`, generate plugin files and run a Python script that monkeypatches `tools.subprocess.run`:

```python
calls = []
def fake_run(argv, check, capture_output, text, timeout, shell):
    calls.append(argv)
    payload = {"content": [{"type": "text", "text": "{\"status\":\"ok\",\"should_compress\":false,\"messages\":[]}"}]}
    return Result(0, json.dumps(payload), "")

tools.subprocess.run = fake_run
engine = plugin.TokenSaveContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project")
result = engine.should_compress_preflight([{"role": "user", "content": "hello"}])

assert result["status"] == "ok"
assert calls[0][0] == tools.TOKENSAVE_BIN
assert calls[0][1:4] == ["tool", "tokensave_lcm_preflight", "--json"]
assert "--args" in calls[0]
assert '"storage_scope": "project_local"' in calls[0][-1]
```

Run: `cargo test --test hermes_lcm_bridge_test context_engine_preflight_uses_tokensave_tool_json_args -- --nocapture`

Expected: FAIL until the generated context engine uses `tools.call_tokensave_tool` with the correct names and storage args.

- [ ] **Step 2: Normalize nested MCP JSON text in generated Python**

Current `tools.call_tokensave_tool` returns the full JSON-RPC result string. Add a helper:

```python
def call_tokensave_json(name: str, args: dict, **kwargs):
    raw = call_tokensave_tool(name, args, **kwargs)
    outer = json.loads(raw)
    text = outer.get("content", [{}])[0].get("text", "{}")
    return json.loads(text)
```

Use this helper inside `TokenSaveContextEngine`.

- [ ] **Step 3: Add deterministic fake summarizer contract**

Rust request/response sketch:

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LcmCompressionRequest {
    pub provider: String,
    pub session_id: String,
    pub messages: Vec<serde_json::Value>,
    pub current_tokens: Option<i64>,
    pub focus_topic: Option<String>,
    pub summarizer: LcmSummarizerMode,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum LcmSummarizerMode {
    Noop,
    Fake { summary_text: String },
    HermesAuxiliary,
}
```

The generated Python uses `HermesAuxiliary` in production. Rust tests use `Noop` or `Fake`.

- [ ] **Step 4: Add no-op preflight/compress tests**

Tests:

```rust
#[tokio::test]
async fn noop_summarizer_ingests_messages_without_summary_nodes() {
    let response = db.lcm_compress(LcmCompressionRequest {
        provider: "cursor".into(),
        session_id: "session-1".into(),
        messages: vec![json!({"role": "user", "content": "fresh"})],
        current_tokens: Some(100),
        focus_topic: None,
        summarizer: LcmSummarizerMode::Noop,
    }).await.unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.summary_nodes_created, 0);
    assert_eq!(db.lcm_load_session(raw_load("cursor", "session-1")).await.unwrap().messages.len(), 1);
}
```

Run: `cargo test --test hermes_lcm_bridge_test context_engine_preflight_uses_tokensave_tool_json_args -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Commit checkpoint**

```bash
git add src/agents/hermes.rs src/sessions/lcm/hermes.rs src/sessions/lcm/types.rs tests/hermes_lcm_bridge_test.rs
git commit -m "Add Hermes LCM bridge contracts"
```

## Task 9: Compression Lifecycle Primitives and Deterministic Summarizer Injection

**Files:**
- Create: `src/sessions/lcm/compression.rs`
- Modify: `src/sessions/lcm/mod.rs`
- Modify: `src/sessions/lcm/types.rs`
- Modify: `src/sessions/lcm/schema.rs`
- Modify: `src/sessions/lcm/dag.rs`
- Modify: `src/sessions/lcm/query.rs`
- Create: `tests/session_lcm_compression_test.rs`

- [ ] **Step 1: Write failing lifecycle schema tests**

```rust
#[tokio::test]
async fn lifecycle_frontier_survives_reopen() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join(".tokensave/sessions.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    db.lcm_update_lifecycle(LcmLifecycleUpdate {
        provider: "cursor".into(),
        conversation_id: "conversation-1".into(),
        current_session_id: "session-1".into(),
        current_frontier_store_id: Some(42),
        last_finalized_session_id: Some("session-0".into()),
        last_finalized_frontier_store_id: Some(40),
        maintenance_debt: vec![LcmMaintenanceDebt::RawBacklog { from_store_id: 41, to_store_id: 42 }],
    }).await.unwrap();
    drop(db);

    let reopened = GlobalDb::open_at(&db_path).await.unwrap();
    let state = reopened.lcm_lifecycle_state("cursor", "conversation-1").await.unwrap();
    assert_eq!(state.current_frontier_store_id, Some(42));
    assert_eq!(state.last_finalized_session_id.as_deref(), Some("session-0"));
    assert_eq!(state.maintenance_debt.len(), 1);
}
```

Run: `cargo test --test session_lcm_compression_test lifecycle_frontier_survives_reopen -- --nocapture`

Expected: FAIL with missing lifecycle table/types.

- [ ] **Step 2: Add lifecycle/frontier/debt schema**

```sql
CREATE TABLE IF NOT EXISTS lcm_lifecycle_state (
    provider TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    current_session_id TEXT NOT NULL,
    last_finalized_session_id TEXT,
    current_frontier_store_id INTEGER,
    last_finalized_frontier_store_id INTEGER,
    rollover_at INTEGER,
    reset_at INTEGER,
    maintenance_at INTEGER,
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY(provider, conversation_id)
);
CREATE TABLE IF NOT EXISTS lcm_maintenance_debt (
    provider TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    debt_id TEXT NOT NULL,
    debt_kind TEXT NOT NULL,
    from_store_id INTEGER,
    to_store_id INTEGER,
    metadata_json TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY(provider, conversation_id, debt_id),
    FOREIGN KEY(provider, conversation_id)
        REFERENCES lcm_lifecycle_state(provider, conversation_id) ON DELETE CASCADE
);
```

- [ ] **Step 3: Add fake summarizer trait and deterministic compression API**

Sketch:

```rust
#[async_trait::async_trait]
pub trait LcmSummarizer: Send + Sync {
    async fn summarize(&self, request: LcmSummarizeRequest) -> Result<LcmSummarizeOutput>;
}

pub struct FakeSummarizer {
    pub summary_text: String,
}

#[async_trait::async_trait]
impl LcmSummarizer for FakeSummarizer {
    async fn summarize(&self, request: LcmSummarizeRequest) -> Result<LcmSummarizeOutput> {
        Ok(LcmSummarizeOutput {
            summary_text: self.summary_text.clone(),
            source_token_count: request.source_token_count,
            summary_token_count: estimate_tokens(&self.summary_text),
        })
    }
}
```

If the repo avoids `async-trait`, define the trait as synchronous and keep Hermes auxiliary calls in generated Python. The Rust fake summarizer is enough for deterministic compression tests.

- [ ] **Step 4: Implement preflight-ingests behavior**

Test:

```rust
#[tokio::test]
async fn preflight_can_request_compression_when_ingest_protection_changes_replay() {
    let response = db.lcm_preflight(LcmPreflightRequest {
        provider: "cursor".into(),
        session_id: "session-1".into(),
        messages: vec![json!({"role": "assistant", "content": format!("data:image/png;base64,{}", "A".repeat(100_000))})],
        current_tokens: Some(100),
    }).await.unwrap();

    assert!(response.should_compress);
    assert_eq!(response.reason, "ingest_protection_changed_replay");
    assert!(response.replay_messages[0]["content"].as_str().unwrap().contains("[externalized payload"));
}
```

- [ ] **Step 5: Implement frontier advance, fresh tail preservation, and DAG node creation**

Test:

```rust
#[tokio::test]
async fn fake_summarizer_compacts_backlog_and_preserves_fresh_tail() {
    insert_raw_messages(&db, "cursor", "session-1", ["old-1", "old-2", "fresh-1", "fresh-2"]).await;
    let response = db.lcm_compress_with_summarizer(
        compress_request("cursor", "session-1").fresh_tail_count(2),
        &FakeSummarizer { summary_text: "old summary".into() },
    ).await.unwrap();

    assert_eq!(response.summary_nodes_created, 1);
    assert_eq!(response.replay_messages.last().unwrap()["content"], "fresh-2");
    assert_eq!(response.frontier.current_frontier_store_id, Some(2));
}
```

Run: `cargo test --test session_lcm_compression_test -- --nocapture`

Expected: PASS.

- [ ] **Step 6: Commit checkpoint**

```bash
git add src/sessions/lcm/mod.rs src/sessions/lcm/types.rs src/sessions/lcm/schema.rs src/sessions/lcm/dag.rs src/sessions/lcm/query.rs src/sessions/lcm/compression.rs tests/session_lcm_compression_test.rs
git commit -m "Add deterministic LCM compression lifecycle"
```

## Task 10: Hermes Auxiliary LLM Bridge, Reasoning Stripping, and Fallbacks

**Files:**
- Modify: `src/agents/hermes.rs`
- Modify: `src/sessions/lcm/hermes.rs`
- Modify: `src/sessions/lcm/types.rs`
- Modify: `tests/hermes_lcm_bridge_test.rs`

- [ ] **Step 1: Write failing reasoning-strip test for generated Python**

Python fixture in `tests/hermes_lcm_bridge_test.rs`:

```python
class Aux:
    def __init__(self):
        self.calls = []
    def call_llm(self, **kwargs):
        self.calls.append(kwargs)
        return "<think>hidden chain</think>\nUseful compact summary"

agent = type("Agent", (), {"auxiliary_client": Aux()})()
engine = plugin.TokenSaveContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project", agent=agent)
summary = engine._call_auxiliary_summary("Summarize", [{"role": "user", "content": "raw"}])

assert summary["status"] == "ok"
assert summary["text"] == "Useful compact summary"
assert agent.auxiliary_client.calls[0]["task"] == "compression"
```

Run: `cargo test --test hermes_lcm_bridge_test auxiliary_summary_strips_reasoning_tags -- --nocapture`

Expected: FAIL until auxiliary bridge helper exists.

- [ ] **Step 2: Add generated Python reasoning stripper**

Sketch:

```python
REASONING_TAGS = ["think", "thinking", "reasoning", "thought", "REASONING_SCRATCHPAD"]

def _strip_reasoning(text: str) -> str:
    output = text or ""
    for tag in REASONING_TAGS:
        output = re.sub(fr"<{tag}>.*?</{tag}>", "", output, flags=re.DOTALL | re.IGNORECASE)
    return output.strip()
```

- [ ] **Step 3: Add auxiliary summary route chain and cooldown state**

Keep state process-local in Python:

```python
class TokenSaveContextEngine:
    def __init__(self):
        self._route_failures = {}
        self._cooldown_until = {}

    def _call_auxiliary_summary(self, prompt, messages, **kwargs):
        routes = kwargs.get("routes") or [{"model": kwargs.get("model"), "temperature": 0.1}]
        for route in routes:
            key = route.get("model") or "default"
            if self._cooldown_until.get(key, 0) > time.time():
                continue
            try:
                text = self.agent.auxiliary_client.call_llm(
                    task="compression",
                    messages=[{"role": "system", "content": prompt}, *messages],
                    temperature=route.get("temperature", 0.1),
                    max_tokens=route.get("max_tokens", 2048),
                    timeout=route.get("timeout", 60),
                    model=route.get("model"),
                )
                stripped = _strip_reasoning(str(text))
                if stripped:
                    return {"status": "ok", "text": stripped, "route": key}
            except Exception as exc:
                self._route_failures[key] = self._route_failures.get(key, 0) + 1
                self._cooldown_until[key] = time.time() + min(300, 2 ** self._route_failures[key])
        return {"status": "fallback", "text": _deterministic_truncation(messages)}
```

- [ ] **Step 4: Pass auxiliary summaries back to Rust**

`compress()` flow in generated Python:

1. Call `tokensave_lcm_compress` with `summarizer.mode = "request_auxiliary"`.
2. If Rust returns `status = "needs_summary"` with a prompt and source messages, call `_call_auxiliary_summary`.
3. Call `tokensave_lcm_compress` again with `summarizer.mode = "provided"` and `summary_text`.
4. Return the final replay messages and frontier/status JSON to Hermes.

The Rust contract:

```rust
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum LcmSummarizerMode {
    Noop,
    Fake { summary_text: String },
    RequestAuxiliary,
    Provided { summary_text: String, route: Option<String> },
}
```

- [ ] **Step 5: Add fallback behavior tests**

Tests:

```python
class FailingAux:
    def call_llm(self, **kwargs):
        raise RuntimeError("route unavailable")

engine.initialize(session_id="session-1", project_root="/tmp/project", agent=type("Agent", (), {"auxiliary_client": FailingAux()})())
summary = engine._call_auxiliary_summary("Summarize", [{"role": "user", "content": "A" * 10_000}])
assert summary["status"] == "fallback"
assert len(summary["text"]) < 10_000
assert summary["text"].endswith("[deterministic compression fallback]")
```

Run: `cargo test --test hermes_lcm_bridge_test -- --nocapture`

Expected: PASS.

- [ ] **Step 6: Commit checkpoint**

```bash
git add src/agents/hermes.rs src/sessions/lcm/hermes.rs src/sessions/lcm/types.rs tests/hermes_lcm_bridge_test.rs
git commit -m "Integrate Hermes auxiliary LCM summaries"
```

## Task 11: Ingest Protection, Diagnostics, Doctor, and Regression Tests

**Files:**
- Modify: `src/sessions/lcm/security.rs`
- Modify: `src/sessions/lcm/query.rs`
- Modify: `src/mcp/tools/definitions.rs`
- Modify: `src/mcp/tools/handlers/session.rs`
- Modify: `src/agents/mod.rs`
- Modify: `src/agents/hermes.rs`
- Modify: `tests/session_lcm_ingest_protection_test.rs`
- Modify: `tests/session_lcm_payload_test.rs`
- Modify: `tests/agent_test.rs`
- Modify: `tests/mcp_handler_test.rs`
- Modify: `tests/session_lcm_query_test.rs`

- [ ] **Step 1: Write failing doctor/status integrity tests**

```rust
#[tokio::test]
async fn lcm_status_reports_missing_and_unreferenced_payloads_without_previewing_content() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = open_lcm_db(tmp.path()).await;
    let secret = "SUPER_SECRET_PAYLOAD";
    let payload_ref = insert_external_payload(&db, "cursor", "session-1", "message-1", secret).await;
    std::fs::remove_file(payload_path(tmp.path(), &payload_ref)).unwrap();
    std::fs::write(payload_dir(tmp.path()).join("orphan.payload"), "orphan secret").unwrap();

    let status = db.lcm_status("cursor", Some("session-1")).await.unwrap();
    assert_eq!(status.missing_payload_count, 1);
    assert_eq!(status.unreferenced_payload_count, 1);
    let rendered = serde_json::to_string(&status).unwrap();
    assert!(!rendered.contains(secret));
    assert!(!rendered.contains("orphan secret"));
}
```

Run: `cargo test --test session_lcm_payload_test lcm_status_reports_missing_and_unreferenced_payloads_without_previewing_content -- --nocapture`

Expected: FAIL until integrity scan fields exist.

- [ ] **Step 2: Add diagnostics JSON to `tokensave_lcm_status`**

Status output sketch:

```json
{
  "status": "ok",
  "schema_version": 1,
  "storage_scope": "project_local",
  "raw_message_count": 12,
  "summary_node_count": 2,
  "payload": {
    "externalized_count": 1,
    "missing_count": 0,
    "unreferenced_count": 0,
    "root_contained": true
  },
  "lifecycle": {
    "current_session_id": "session-1",
    "current_frontier_store_id": 10,
    "maintenance_debt_count": 0
  },
  "redaction": {
    "enabled": false,
    "lossy_records": 0
  }
}
```

Do not include payload content in diagnostics.

- [ ] **Step 3: Add regression scan against authoritative caps**

Test:

```rust
#[test]
fn no_authoritative_session_write_uses_legacy_text_cap() {
    let source = std::fs::read_to_string("src/global_db.rs").unwrap();
    assert!(!source.contains("MAX_SESSION_MESSAGE_TEXT_BYTES"));
    assert!(!source.contains("SESSION_MESSAGE_TRUNCATION_MARKER"));

    let lcm_raw = std::fs::read_to_string("src/sessions/lcm/raw.rs").unwrap();
    assert!(lcm_raw.contains("MAX_DERIVED_TEXT_CHARS"));
    assert!(lcm_raw.contains("derived_text_for_index"));
}
```

Run: `cargo test --test session_lcm_ingest_protection_test no_authoritative_session_write_uses_legacy_text_cap -- --nocapture`

Expected: PASS after old authoritative cap symbols are removed or renamed to derived-only constants.

- [ ] **Step 4: Add separation regressions for codegraph and fact memory**

In `tests/session_lcm_query_test.rs`:

```rust
#[test]
fn lcm_modules_do_not_depend_on_context_builder_or_memory_fact_store() {
    for path in [
        "src/sessions/lcm/raw.rs",
        "src/sessions/lcm/dag.rs",
        "src/sessions/lcm/query.rs",
        "src/sessions/lcm/compression.rs",
    ] {
        let source = std::fs::read_to_string(path).unwrap();
        assert!(!source.contains("ContextBuilder"));
        assert!(!source.contains("MemoryCategory"));
        assert!(!source.contains("memory_facts"));
    }
}
```

Run: `cargo test --test session_lcm_query_test lcm_modules_do_not_depend_on_context_builder_or_memory_fact_store -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Extend Hermes doctor tests**

In `tests/agent_test.rs`, assert Hermes healthcheck recognizes generated LCM plugin files and local/profile storage hints:

```rust
assert!(init_py.contains("TokenSaveContextEngine"));
assert!(init_py.contains("storage_scope"));
assert!(manifest.contains("tokensave_lcm_status"));
assert!(manifest.contains("tokensave_lcm_compress"));
```

Run: `cargo test --test agent_test hermes -- --nocapture`

Expected: PASS.

- [ ] **Step 6: Run focused regression suite**

Run:

```bash
cargo test --test session_lcm_schema_test -- --nocapture
cargo test --test session_lcm_raw_test -- --nocapture
cargo test --test session_lcm_payload_test -- --nocapture
cargo test --test session_lcm_dag_test -- --nocapture
cargo test --test session_lcm_query_test -- --nocapture
cargo test --test session_lcm_compression_test -- --nocapture
cargo test --test session_lcm_ingest_protection_test -- --nocapture
cargo test --test hermes_lcm_bridge_test -- --nocapture
cargo test --test session_global_db_test -- --nocapture
cargo test --test mcp_handler_test -- --nocapture
cargo test --test agent_test hermes -- --nocapture
```

Expected: all commands PASS.

- [ ] **Step 7: Commit checkpoint**

```bash
git add src/sessions/lcm/security.rs src/sessions/lcm/query.rs src/mcp/tools/definitions.rs src/mcp/tools/handlers/session.rs src/agents/mod.rs src/agents/hermes.rs tests/session_lcm_ingest_protection_test.rs tests/session_lcm_payload_test.rs tests/agent_test.rs tests/mcp_handler_test.rs tests/session_lcm_query_test.rs
git commit -m "Add LCM diagnostics and regressions"
```

## Final Verification

Run these after all implementation tasks are complete:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets
cargo test
```

Expected:

- `cargo fmt --all -- --check` exits 0 with no formatting drift.
- `cargo clippy --all-targets` exits 0 with no new warnings.
- `cargo test` exits 0 across the full Rust test suite.

Manual verification for generated Hermes Python:

```bash
cargo test --test agent_test hermes -- --nocapture
cargo test --test hermes_lcm_bridge_test -- --nocapture
```

Expected:

- Generated `plugin.yaml`, `schemas.py`, `schemas.json`, `tools.py`, `__init__.py`, and `skills/tokensave/SKILL.md` compile and register.
- `TokenSaveContextEngine` registers when Hermes exposes `register_context_engine`.
- The generated bridge calls `tokensave tool ... --json --args` and can parse nested MCP text JSON.
- Auxiliary summaries strip reasoning tags and fall back to deterministic truncation when all routes fail.

## Self-Review

- Spec coverage: The tasks cover in-place `sessions.db` migration, lossless raw storage, bounded derived search/display text, externalized payload containment, summary DAG lineage, lifecycle/frontier/debt state, deterministic Rust query APIs, additive MCP tools, compatibility for `tokensave_message_search`, Hermes generated Python lifecycle registration, subprocess bridge use, auxiliary LLM reasoning stripping/fallbacks, storage locality, ingest protection, diagnostics, and separation from codegraph/memory systems.
- Red-flag scan: The plan contains concrete file paths, test names, commands, expected red/green outcomes, commit checkpoints, and code/API sketches for each code-changing task.
- Type consistency: `LcmRawMessage`, `LcmStorageKind`, `LcmPayloadRef`, `LcmSourceRef`, `LcmSummaryNode`, `LcmLoadSessionRequest`, `LcmGrepRequest`, `LcmSummarizerMode`, and `TokenSaveContextEngine` are introduced before later tasks use them.
- Caveat: This plan chooses `<selected-hermes-profile>/.tokensave/sessions.db` for non-local Hermes profile storage so the implementation has one concrete path rule. A later PyO3/native binding milestone remains outside this plan and should require measured bridge overhead before it starts.
