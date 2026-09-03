//! Canonical physical DDL for the tables whose shape more than one engine
//! depends on.
//!
//! A table's constraints are part of its contract, not an implementation
//! detail of whichever engine happens to install it. `retrieval_anchors` and
//! `generation_diagnostics` are each written by the root SQLite engines in
//! `src/db/` and by the concrete executors in the rusqlite runtime crate, and
//! are read by test and parity harnesses besides. When those copies drift, the
//! weaker copy silently accepts rows the real table would reject, and the
//! divergence surfaces only in production.
//!
//! Every consumer that creates one of these tables — production installer,
//! adapter test fixture, or parity harness — should install it from here
//! rather than restating the columns.

/// Immutable, owner-bound retrieval anchors.
///
/// The composite unique index is required, not incidental: SQLite needs an
/// exact unique parent key for the owner-bound alias, disposition, and
/// evidence foreign keys that reference `(anchor_id, owner_json)`, even though
/// `anchor_id` is already unique on its own.
pub const RETRIEVAL_ANCHORS_SCHEMA_DDL: &str = "
    CREATE TABLE IF NOT EXISTS retrieval_anchors (
        anchor_id TEXT PRIMARY KEY CHECK(length(anchor_id) > 0),
        anchor_json TEXT NOT NULL CHECK(json_valid(anchor_json)),
        owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
        projection_generation TEXT NOT NULL CHECK(length(projection_generation) > 0)
    );
    -- SQLite requires an exact unique parent key for the composite owner-bound
    -- alias and evidence foreign keys, even though anchor_id is itself unique.
    CREATE UNIQUE INDEX IF NOT EXISTS idx_retrieval_anchors_owner
        ON retrieval_anchors(anchor_id, owner_json);
";

/// Durable generation-bound diagnostic records and their publication ledger.
///
/// `record_state` and `state_generation` are decoded by
/// [`crate::diagnostics::codec`]; the default of `'current'` and the partial
/// unique index on the publication table together enforce that at most one
/// generation is current at a time.
pub const GENERATION_DIAGNOSTICS_SCHEMA_DDL: &str =
    "CREATE TABLE IF NOT EXISTS generation_diagnostics (
        diagnostic_anchor TEXT PRIMARY KEY,
        generation_id TEXT NOT NULL,
        repository TEXT NOT NULL,
        worktree TEXT,
        reference TEXT,
        source_revision TEXT,
        file_occurrence_id TEXT NOT NULL,
        content_digest TEXT NOT NULL,
        symbol_occurrence_id TEXT,
        span_start INTEGER NOT NULL,
        span_end INTEGER NOT NULL,
        code TEXT NOT NULL,
        severity TEXT NOT NULL,
        message TEXT NOT NULL,
        message_digest TEXT NOT NULL,
        producer_kind TEXT NOT NULL,
        producer TEXT NOT NULL,
        analyzer_revision TEXT NOT NULL,
        configuration_revision TEXT NOT NULL,
        sanitization_receipt TEXT,
        evidence_class TEXT NOT NULL,
        collected_at INTEGER NOT NULL,
        record_state TEXT NOT NULL DEFAULT 'current',
        state_generation TEXT,
        persisted_at INTEGER NOT NULL DEFAULT 0
    );

    CREATE INDEX IF NOT EXISTS idx_generation_diagnostics_generation_state
        ON generation_diagnostics (generation_id, record_state);

    CREATE INDEX IF NOT EXISTS idx_generation_diagnostics_generation_state_anchor
        ON generation_diagnostics (generation_id, record_state, diagnostic_anchor);

    CREATE INDEX IF NOT EXISTS idx_generation_diagnostics_file
        ON generation_diagnostics (file_occurrence_id, generation_id);

    CREATE INDEX IF NOT EXISTS idx_generation_diagnostics_file_generation_state_anchor
        ON generation_diagnostics (
            file_occurrence_id, generation_id, record_state, diagnostic_anchor
        );

    CREATE TABLE IF NOT EXISTS diagnostic_generation_publications (
        generation_id TEXT PRIMARY KEY,
        record_state TEXT NOT NULL,
        state_generation TEXT,
        published_at INTEGER NOT NULL
    );

    CREATE UNIQUE INDEX IF NOT EXISTS idx_diagnostic_generation_current
        ON diagnostic_generation_publications (record_state)
        WHERE record_state = 'current';";
