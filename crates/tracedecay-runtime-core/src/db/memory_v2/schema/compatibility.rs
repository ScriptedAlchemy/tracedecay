//! V22 compatibility projection and V23 fact-relation / compatibility-bank
//! storage installers.

use crate::errors::Result;

use super::super::{MemoryV2Executor, OPERATION, db_error, optional_string};
use super::introspection::{table_exists, table_has_column};

pub(super) async fn install_v22_compatibility_schema(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_v2_compatibility_operation_receipts (
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            operation_id TEXT NOT NULL CHECK(length(operation_id) > 0),
            operation_kind TEXT NOT NULL CHECK(operation_kind IN (
                'add', 'update', 'remove', 'feedback', 'retrieval',
                'curation', 'merge', 'repair', 'proposal_submit',
                'proposal_reject', 'proposal_promote', 'proposal_import'
            )),
            request_digest TEXT NOT NULL CHECK(length(request_digest) > 0),
            fact_id TEXT,
            event_id TEXT,
            receipt_json TEXT NOT NULL CHECK(json_valid(receipt_json)),
            recorded_at INTEGER NOT NULL,
            PRIMARY KEY(owner_kind, project_id, operation_id),
            FOREIGN KEY(fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            FOREIGN KEY(event_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_lineage_events(event_id, fact_id, owner_kind, project_id),
            CHECK(event_id IS NULL OR fact_id IS NOT NULL),
            CHECK(
                (owner_kind = 'profile' AND project_id = '') OR
                (owner_kind = 'project' AND project_id <> '')
            )
        );

        CREATE TABLE IF NOT EXISTS memory_v2_legacy_feedback_event_map (
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            source_store_id TEXT NOT NULL
                CHECK(source_store_id = 'legacy-memory-v1'),
            legacy_feedback_event_id INTEGER NOT NULL
                CHECK(legacy_feedback_event_id > 0),
            fact_id TEXT NOT NULL,
            event_id TEXT NOT NULL,
            PRIMARY KEY(
                owner_kind, project_id, source_store_id, legacy_feedback_event_id
            ),
            UNIQUE(owner_kind, project_id, source_store_id, event_id),
            FOREIGN KEY(fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            FOREIGN KEY(event_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_lineage_events(event_id, fact_id, owner_kind, project_id),
            CHECK(
                (owner_kind = 'profile' AND project_id = '') OR
                (owner_kind = 'project' AND project_id <> '')
            )
        );

        CREATE TABLE IF NOT EXISTS memory_v2_feedback_history (
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            event_id TEXT NOT NULL,
            action TEXT NOT NULL CHECK(action IN ('helpful', 'unhelpful')),
            old_trust REAL NOT NULL CHECK(old_trust >= 0.0 AND old_trust <= 1.0),
            new_trust REAL NOT NULL CHECK(new_trust >= 0.0 AND new_trust <= 1.0),
            occurred_at INTEGER NOT NULL,
            source TEXT,
            note TEXT,
            details_availability TEXT NOT NULL CHECK(
                details_availability IN ('available', 'legacy_redacted', 'unknown')
            ),
            PRIMARY KEY(owner_kind, project_id, fact_id, event_id),
            FOREIGN KEY(fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            FOREIGN KEY(event_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_lineage_events(event_id, fact_id, owner_kind, project_id),
            CHECK(
                (owner_kind = 'profile' AND project_id = '') OR
                (owner_kind = 'project' AND project_id <> '')
            ),
            CHECK(
                details_availability = 'available' OR (source IS NULL AND note IS NULL)
            )
        );

        CREATE TABLE IF NOT EXISTS memory_v2_feedback_history_repair_progress (
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            source_store_id TEXT NOT NULL
                CHECK(source_store_id = 'legacy-memory-v1'),
            owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
            feedback_frontier INTEGER NOT NULL CHECK(feedback_frontier >= 0),
            feedback_cursor INTEGER NOT NULL DEFAULT 0 CHECK(
                feedback_cursor >= 0 AND feedback_cursor <= feedback_frontier
            ),
            phase TEXT NOT NULL CHECK(phase IN ('pending', 'complete')),
            started_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            completed_at INTEGER,
            PRIMARY KEY(owner_kind, project_id, source_store_id),
            CHECK(
                (owner_kind = 'profile' AND project_id = '') OR
                (owner_kind = 'project' AND project_id <> '')
            ),
            CHECK(
                (phase = 'complete'
                    AND feedback_cursor = feedback_frontier
                    AND completed_at IS NOT NULL) OR
                (phase = 'pending' AND completed_at IS NULL)
            )
        );

        CREATE TABLE IF NOT EXISTS memory_v2_fact_relations (
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            source_fact_id TEXT NOT NULL,
            target_fact_id TEXT NOT NULL,
            relation TEXT NOT NULL CHECK(relation IN (
                'supports', 'derived_from'
            )),
            confidence REAL NOT NULL CHECK(confidence >= 0.0 AND confidence <= 1.0),
            source_label TEXT NOT NULL CHECK(
                length(source_label) > 0
                AND length(source_label) <= 4096
                AND trim(source_label) = source_label
            ),
            evidence_fact_ids_json TEXT NOT NULL CHECK(
                json_valid(evidence_fact_ids_json)
                AND json_type(evidence_fact_ids_json) = 'array'
                AND json_array_length(evidence_fact_ids_json) BETWEEN 1 AND 256
            ),
            occurred_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL CHECK(updated_at >= occurred_at),
            PRIMARY KEY(
                owner_kind, project_id, source_fact_id, target_fact_id, relation
            ),
            FOREIGN KEY(source_fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            FOREIGN KEY(target_fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            CHECK(source_fact_id <> target_fact_id),
            CHECK(
                (owner_kind = 'profile' AND project_id = '') OR
                (owner_kind = 'project' AND project_id <> '')
            )
        );

        CREATE INDEX IF NOT EXISTS idx_memory_v2_compatibility_receipts_fact
            ON memory_v2_compatibility_operation_receipts(
                fact_id, owner_kind, project_id, recorded_at
            );
        CREATE INDEX IF NOT EXISTS idx_memory_v2_legacy_feedback_event_map_canonical
            ON memory_v2_legacy_feedback_event_map(
                owner_kind, project_id, fact_id, event_id
            );
        CREATE INDEX IF NOT EXISTS idx_memory_v2_feedback_history_fact
            ON memory_v2_feedback_history(
                owner_kind, project_id, fact_id, occurred_at, event_id
            );
        CREATE INDEX IF NOT EXISTS idx_memory_v2_feedback_history_repair_pending
            ON memory_v2_feedback_history_repair_progress(
                phase, owner_kind, project_id, updated_at
            );
        CREATE INDEX IF NOT EXISTS idx_memory_v2_fact_relations_source
            ON memory_v2_fact_relations(
                owner_kind, project_id, source_fact_id, relation, updated_at DESC
            );
        CREATE INDEX IF NOT EXISTS idx_memory_v2_fact_relations_target
            ON memory_v2_fact_relations(
                owner_kind, project_id, target_fact_id, relation, updated_at DESC
            );
        CREATE TRIGGER IF NOT EXISTS memory_v2_compatibility_receipts_no_update
        BEFORE UPDATE ON memory_v2_compatibility_operation_receipts BEGIN
            SELECT RAISE(ABORT, 'memory_v2 compatibility operation receipts are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_compatibility_receipts_no_delete
        BEFORE DELETE ON memory_v2_compatibility_operation_receipts BEGIN
            SELECT RAISE(ABORT, 'memory_v2 compatibility operation receipts are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_compatibility_receipts_no_payload
        BEFORE INSERT ON memory_v2_compatibility_operation_receipts
        WHEN EXISTS (
            SELECT 1 FROM json_tree(NEW.receipt_json)
            WHERE lower(CAST(key AS TEXT)) IN (
                'content', 'payload', 'payload_json', 'metadata',
                'vector', 'vectors', 'embedding', 'embeddings',
                'vector_watermark', 'vector_watermark_json'
            )
        ) BEGIN
            SELECT RAISE(ABORT, 'memory_v2 compatibility receipts cannot retain payload data');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_legacy_feedback_event_map_no_update
        BEFORE UPDATE ON memory_v2_legacy_feedback_event_map BEGIN
            SELECT RAISE(ABORT, 'memory_v2 legacy feedback event mappings are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_legacy_feedback_event_map_no_delete
        BEFORE DELETE ON memory_v2_legacy_feedback_event_map BEGIN
            SELECT RAISE(ABORT, 'memory_v2 legacy feedback event mappings are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_feedback_history_only_redaction
        BEFORE UPDATE ON memory_v2_feedback_history
        WHEN NOT (
            NEW.owner_kind IS OLD.owner_kind
            AND NEW.project_id IS OLD.project_id
            AND NEW.fact_id IS OLD.fact_id
            AND NEW.event_id IS OLD.event_id
            AND NEW.action IS OLD.action
            AND NEW.old_trust IS OLD.old_trust
            AND NEW.new_trust IS OLD.new_trust
            AND NEW.occurred_at IS OLD.occurred_at
            AND NEW.source IS NULL
            AND NEW.note IS NULL
            AND (
                (OLD.details_availability = 'available'
                    AND NEW.details_availability = 'legacy_redacted')
                OR (
                    OLD.source IS NULL AND OLD.note IS NULL
                    AND NEW.details_availability IS OLD.details_availability
                )
            )
        ) BEGIN
            SELECT RAISE(ABORT, 'memory_v2 feedback history permits only detail redaction');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_feedback_history_no_delete
        BEFORE DELETE ON memory_v2_feedback_history BEGIN
            SELECT RAISE(ABORT, 'memory_v2 feedback history records are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_feedback_history_repair_progress_guard
        BEFORE UPDATE ON memory_v2_feedback_history_repair_progress
        WHEN NEW.owner_kind IS NOT OLD.owner_kind
            OR NEW.project_id IS NOT OLD.project_id
            OR NEW.source_store_id IS NOT OLD.source_store_id
            OR NEW.owner_json IS NOT OLD.owner_json
            OR NEW.feedback_frontier IS NOT OLD.feedback_frontier
            OR NEW.feedback_cursor < OLD.feedback_cursor
            OR NEW.started_at IS NOT OLD.started_at
            OR NEW.updated_at < OLD.updated_at
            OR OLD.phase = 'complete'
        BEGIN
            SELECT RAISE(ABORT, 'memory_v2 feedback history repair progress is append-only');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_feedback_history_repair_progress_no_delete
        BEFORE DELETE ON memory_v2_feedback_history_repair_progress BEGIN
            SELECT RAISE(ABORT, 'memory_v2 feedback history repair progress is immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_fact_relations_validate_evidence_insert
        BEFORE INSERT ON memory_v2_fact_relations
        WHEN EXISTS (
            SELECT 1 FROM json_each(NEW.evidence_fact_ids_json)
            WHERE type <> 'text' OR length(value) = 0
        ) OR EXISTS (
            SELECT 1 FROM json_each(NEW.evidence_fact_ids_json) AS evidence
            LEFT JOIN memory_v2_facts AS fact
              ON fact.fact_id = evidence.value
             AND fact.owner_kind = NEW.owner_kind
             AND fact.project_id = NEW.project_id
            WHERE fact.fact_id IS NULL
        ) OR EXISTS (
            SELECT value FROM json_each(NEW.evidence_fact_ids_json)
            GROUP BY value HAVING COUNT(*) > 1
        ) BEGIN
            SELECT RAISE(ABORT, 'memory_v2 fact relation evidence must be unique owner facts');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_fact_relations_validate_evidence_update
        BEFORE UPDATE ON memory_v2_fact_relations
        WHEN EXISTS (
            SELECT 1 FROM json_each(NEW.evidence_fact_ids_json)
            WHERE type <> 'text' OR length(value) = 0
        ) OR EXISTS (
            SELECT 1 FROM json_each(NEW.evidence_fact_ids_json) AS evidence
            LEFT JOIN memory_v2_facts AS fact
              ON fact.fact_id = evidence.value
             AND fact.owner_kind = NEW.owner_kind
             AND fact.project_id = NEW.project_id
            WHERE fact.fact_id IS NULL
        ) OR EXISTS (
            SELECT value FROM json_each(NEW.evidence_fact_ids_json)
            GROUP BY value HAVING COUNT(*) > 1
        ) BEGIN
            SELECT RAISE(ABORT, 'memory_v2 fact relation evidence must be unique owner facts');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_fact_relations_identity_guard
        BEFORE UPDATE ON memory_v2_fact_relations
        WHEN NEW.owner_kind IS NOT OLD.owner_kind
            OR NEW.project_id IS NOT OLD.project_id
            OR NEW.source_fact_id IS NOT OLD.source_fact_id
            OR NEW.target_fact_id IS NOT OLD.target_fact_id
            OR NEW.relation IS NOT OLD.relation
            OR NEW.occurred_at IS NOT OLD.occurred_at
            OR NEW.updated_at < OLD.updated_at
        BEGIN
            SELECT RAISE(ABORT, 'memory_v2 fact relation identity is immutable');
        END;",
    )
    .await
    .map_err(|error| db_error(operation, error))
}

pub(super) async fn upgrade_v23_fact_relation_schema(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
    if fact_relation_schema_is_v23(conn).await? {
        return install_v23_fact_relation_support(conn, operation).await;
    }
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS memory_v2_fact_relations_validate_evidence_insert;
         DROP TRIGGER IF EXISTS memory_v2_fact_relations_validate_evidence_update;
         DROP TRIGGER IF EXISTS memory_v2_fact_relations_identity_guard;
         DROP INDEX IF EXISTS idx_memory_v2_fact_relations_source;
         DROP INDEX IF EXISTS idx_memory_v2_fact_relations_target;
         ALTER TABLE memory_v2_fact_relations
         RENAME TO memory_v2_fact_relations_v22;",
    )
    .await
    .map_err(|error| db_error(operation, error))?;
    create_v23_fact_relation_table(conn, operation).await?;
    conn.execute_batch(
        "INSERT INTO memory_v2_fact_relations(
            owner_kind, project_id, source_fact_id, target_fact_id, relation,
            confidence, source_label, provenance_json, evidence_fact_ids_json,
            occurred_at, updated_at
         )
         SELECT owner_kind, project_id, source_fact_id, target_fact_id, relation,
                confidence, source_label, '{}', evidence_fact_ids_json,
                occurred_at, updated_at
         FROM memory_v2_fact_relations_v22;
         DROP TABLE memory_v2_fact_relations_v22;",
    )
    .await
    .map_err(|error| db_error(operation, error))?;
    install_v23_fact_relation_support(conn, operation).await
}

async fn fact_relation_schema_is_v23(conn: &impl MemoryV2Executor) -> Result<bool> {
    if !table_exists(conn, "memory_v2_fact_relations").await?
        || !table_has_column(
            conn,
            "memory_v2_fact_relations",
            "provenance_json",
            OPERATION,
        )
        .await?
    {
        return Ok(false);
    }
    let Some(sql) = optional_string(
        conn,
        "SELECT sql FROM sqlite_master
         WHERE type = 'table' AND name = 'memory_v2_fact_relations'",
        (),
    )
    .await?
    else {
        return Ok(false);
    };
    let sql = sql.to_ascii_lowercase();
    Ok(["supports", "contradicts", "supersedes", "derived_from"]
        .into_iter()
        .all(|relation| sql.contains(&format!("'{relation}'"))))
}

async fn create_v23_fact_relation_table(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE memory_v2_fact_relations (
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            source_fact_id TEXT NOT NULL,
            target_fact_id TEXT NOT NULL,
            relation TEXT NOT NULL CHECK(relation IN (
                'supports', 'contradicts', 'supersedes', 'derived_from'
            )),
            confidence REAL NOT NULL CHECK(confidence >= 0.0 AND confidence <= 1.0),
            source_label TEXT NOT NULL CHECK(
                length(source_label) > 0
                AND length(source_label) <= 4096
                AND trim(source_label) = source_label
            ),
            provenance_json TEXT NOT NULL CHECK(
                json_valid(provenance_json)
                AND length(CAST(provenance_json AS BLOB)) <= 4096
            ),
            evidence_fact_ids_json TEXT NOT NULL CHECK(
                json_valid(evidence_fact_ids_json)
                AND json_type(evidence_fact_ids_json) = 'array'
                AND json_array_length(evidence_fact_ids_json) BETWEEN 1 AND 256
            ),
            occurred_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL CHECK(updated_at >= occurred_at),
            PRIMARY KEY(
                owner_kind, project_id, source_fact_id, target_fact_id, relation
            ),
            FOREIGN KEY(source_fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            FOREIGN KEY(target_fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            CHECK(source_fact_id <> target_fact_id),
            CHECK(
                (owner_kind = 'profile' AND project_id = '') OR
                (owner_kind = 'project' AND project_id <> '')
            )
        );",
    )
    .await
    .map_err(|error| db_error(operation, error))
}

async fn install_v23_fact_relation_support(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_memory_v2_fact_relations_source
            ON memory_v2_fact_relations(
                owner_kind, project_id, source_fact_id, relation, updated_at DESC
            );
         CREATE INDEX IF NOT EXISTS idx_memory_v2_fact_relations_target
            ON memory_v2_fact_relations(
                owner_kind, project_id, target_fact_id, relation, updated_at DESC
            );
         CREATE TRIGGER IF NOT EXISTS memory_v2_fact_relations_validate_evidence_insert
         BEFORE INSERT ON memory_v2_fact_relations
         WHEN EXISTS (
            SELECT 1 FROM json_each(NEW.evidence_fact_ids_json)
            WHERE type <> 'text' OR length(value) = 0
         ) OR EXISTS (
            SELECT 1 FROM json_each(NEW.evidence_fact_ids_json) AS evidence
            LEFT JOIN memory_v2_facts AS fact
              ON fact.fact_id = evidence.value
             AND fact.owner_kind = NEW.owner_kind
             AND fact.project_id = NEW.project_id
            WHERE fact.fact_id IS NULL
         ) OR EXISTS (
            SELECT value FROM json_each(NEW.evidence_fact_ids_json)
            GROUP BY value HAVING COUNT(*) > 1
         ) BEGIN
            SELECT RAISE(ABORT, 'memory_v2 fact relation evidence must be unique owner facts');
         END;
         CREATE TRIGGER IF NOT EXISTS memory_v2_fact_relations_validate_evidence_update
         BEFORE UPDATE ON memory_v2_fact_relations
         WHEN EXISTS (
            SELECT 1 FROM json_each(NEW.evidence_fact_ids_json)
            WHERE type <> 'text' OR length(value) = 0
         ) OR EXISTS (
            SELECT 1 FROM json_each(NEW.evidence_fact_ids_json) AS evidence
            LEFT JOIN memory_v2_facts AS fact
              ON fact.fact_id = evidence.value
             AND fact.owner_kind = NEW.owner_kind
             AND fact.project_id = NEW.project_id
            WHERE fact.fact_id IS NULL
         ) OR EXISTS (
            SELECT value FROM json_each(NEW.evidence_fact_ids_json)
            GROUP BY value HAVING COUNT(*) > 1
         ) BEGIN
            SELECT RAISE(ABORT, 'memory_v2 fact relation evidence must be unique owner facts');
         END;
         CREATE TRIGGER IF NOT EXISTS memory_v2_fact_relations_identity_guard
         BEFORE UPDATE ON memory_v2_fact_relations
         WHEN NEW.owner_kind IS NOT OLD.owner_kind
            OR NEW.project_id IS NOT OLD.project_id
            OR NEW.source_fact_id IS NOT OLD.source_fact_id
            OR NEW.target_fact_id IS NOT OLD.target_fact_id
            OR NEW.relation IS NOT OLD.relation
            OR NEW.occurred_at IS NOT OLD.occurred_at
            OR NEW.updated_at < OLD.updated_at
         BEGIN
            SELECT RAISE(ABORT, 'memory_v2 fact relation identity is immutable');
         END;",
    )
    .await
    .map_err(|error| db_error(operation, error))
}

pub(super) async fn install_v23_compatibility_bank_schema(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_v2_compatibility_banks (
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            source_store_id TEXT NOT NULL
                CHECK(source_store_id = 'legacy-memory-v1'),
            owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
            bank_name TEXT NOT NULL CHECK(bank_name IN (
                'all', 'general', 'user_pref', 'project', 'tool', 'decision', 'code_area'
            )),
            vector BLOB NOT NULL CHECK(
                typeof(vector) = 'blob'
                AND length(vector) = 8200
                AND substr(vector, 1, 8) = X'0008000000000000'
            ),
            hrr_algebra TEXT NOT NULL CHECK(hrr_algebra = 'amari_fhrr'),
            hrr_dim INTEGER NOT NULL CHECK(hrr_dim = 2048),
            fact_count INTEGER NOT NULL CHECK(fact_count > 0),
            updated_at INTEGER NOT NULL,
            PRIMARY KEY(owner_kind, project_id, source_store_id, bank_name),
            CHECK(
                (owner_kind = 'profile'
                    AND project_id = ''
                    AND json_extract(owner_json, '$.kind') IS 'profile') OR
                (owner_kind = 'project'
                    AND project_id <> ''
                    AND json_extract(owner_json, '$.kind') IS 'project'
                    AND json_extract(owner_json, '$.project_id') IS project_id)
            )
        );
        CREATE TABLE IF NOT EXISTS memory_v2_compatibility_bank_dirty (
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            source_store_id TEXT NOT NULL
                CHECK(source_store_id = 'legacy-memory-v1'),
            owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
            bank_name TEXT NOT NULL CHECK(bank_name IN (
                'all', 'general', 'user_pref', 'project', 'tool', 'decision', 'code_area'
            )),
            updated_at INTEGER NOT NULL,
            PRIMARY KEY(owner_kind, project_id, source_store_id, bank_name),
            CHECK(
                (owner_kind = 'profile'
                    AND project_id = ''
                    AND json_extract(owner_json, '$.kind') IS 'profile') OR
                (owner_kind = 'project'
                    AND project_id <> ''
                    AND json_extract(owner_json, '$.kind') IS 'project'
                    AND json_extract(owner_json, '$.project_id') IS project_id)
            )
        );
        CREATE INDEX IF NOT EXISTS idx_memory_v2_compatibility_banks_owner
            ON memory_v2_compatibility_banks(
                owner_kind, project_id, source_store_id, owner_json, updated_at DESC
            );
        CREATE INDEX IF NOT EXISTS idx_memory_v2_compatibility_bank_dirty_owner
            ON memory_v2_compatibility_bank_dirty(
                owner_kind, project_id, source_store_id, owner_json, updated_at ASC
            );
        CREATE TRIGGER IF NOT EXISTS memory_v2_compatibility_banks_identity_guard
        BEFORE UPDATE ON memory_v2_compatibility_banks
        WHEN NEW.owner_kind IS NOT OLD.owner_kind
            OR NEW.project_id IS NOT OLD.project_id
            OR NEW.source_store_id IS NOT OLD.source_store_id
            OR NEW.owner_json IS NOT OLD.owner_json
            OR NEW.bank_name IS NOT OLD.bank_name
            OR NEW.updated_at < OLD.updated_at
        BEGIN
            SELECT RAISE(ABORT, 'memory_v2 compatibility bank identity is immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_compatibility_bank_dirty_identity_guard
        BEFORE UPDATE ON memory_v2_compatibility_bank_dirty
        WHEN NEW.owner_kind IS NOT OLD.owner_kind
            OR NEW.project_id IS NOT OLD.project_id
            OR NEW.source_store_id IS NOT OLD.source_store_id
            OR NEW.owner_json IS NOT OLD.owner_json
            OR NEW.bank_name IS NOT OLD.bank_name
            OR NEW.updated_at < OLD.updated_at
        BEGIN
            SELECT RAISE(ABORT, 'memory_v2 compatibility dirty bank identity is immutable');
        END;",
    )
    .await
    .map_err(|error| db_error(operation, error))
}
