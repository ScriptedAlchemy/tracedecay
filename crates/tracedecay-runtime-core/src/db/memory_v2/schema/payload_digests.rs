//! Persisted content digests for assertion payloads.
//!
//! `add_project_memory_fact` asks "does an eligible fact with exactly this
//! content already exist?" on every write. Before this companion table that
//! question decoded every eligible payload for the owner and hashed it in
//! Rust, which the exact-SQL materialization cap turned into a hard ceiling
//! at 10,000 facts (#834). The digest is written by the same insertion
//! authority that writes the payload, is byte-equivalent to
//! `content_digest(payload.content())`, and is dropped with its payload so a
//! privacy purge never leaves a fingerprint behind.

use tracedecay_domain::errors::Result;

use super::super::{MemoryV2Executor, db_error};

/// Companion table plus the lookup index and the immutability / cascade
/// triggers. Everything is `IF NOT EXISTS` so the v34 → v35 step can resume
/// after an interrupted run.
pub(in crate::db) const PAYLOAD_DIGESTS_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS memory_v2_assertion_payload_digests (
            payload_rowid INTEGER PRIMARY KEY,
            assertion_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            content_digest TEXT NOT NULL CHECK(
                length(content_digest) = 71 AND content_digest LIKE 'sha256:%'
            ),
            UNIQUE(assertion_id, fact_id, owner_kind, project_id),
            FOREIGN KEY(payload_rowid)
                REFERENCES memory_v2_assertion_payloads(rowid)
        );

        CREATE INDEX IF NOT EXISTS memory_v2_assertion_payload_digests_lookup
            ON memory_v2_assertion_payload_digests(
                owner_kind, project_id, content_digest, fact_id
            );

        CREATE TRIGGER IF NOT EXISTS memory_v2_assertion_payload_digests_no_update
        BEFORE UPDATE ON memory_v2_assertion_payload_digests BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertion payload digests are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_payloads_digest_delete
        AFTER DELETE ON memory_v2_assertion_payloads BEGIN
            DELETE FROM memory_v2_assertion_payload_digests
            WHERE payload_rowid = OLD.rowid;
        END;";

/// Names of the objects `PAYLOAD_DIGESTS_SCHEMA` creates, in the order the
/// final-shape inventory reports them. The v34 → v35 step admits a store
/// whose inventory is exactly the final shape minus these.
pub(in crate::db) const PAYLOAD_DIGEST_OBJECTS: &[&str] = &[
    "memory_v2_assertion_payload_digests",
    "memory_v2_assertion_payload_digests_lookup",
    "memory_v2_assertion_payload_digests_no_update",
    "memory_v2_payloads_digest_delete",
];

pub(super) async fn install_payload_digests(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
    conn.execute_batch(PAYLOAD_DIGESTS_SCHEMA)
        .await
        .map_err(|error| db_error(operation, error))
}
