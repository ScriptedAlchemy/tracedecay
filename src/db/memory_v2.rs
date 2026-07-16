//! Additive V2 fact-lineage storage and resumable legacy projection backfill.

use libsql::{Connection, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    Confidence, EvidenceClass, FactAssertionKindV1, FactAssertionV1, FactEventId,
    FactEvidenceRefV1, FactEvidenceRelationV1, FactId, FactIdentityMaterialV1,
    FactIdentitySourceV1, FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1, FactPayloadV1,
    LegacyFactMappingV1, LegacyHistoryCoverageV1, RetentionClass, RetrievalAnchorId,
    SanitizerDispositionV1, SourceStoreId, UtcMicros,
};

use crate::errors::{Result, TraceDecayError};
use crate::privacy::{
    MemoryFactSanitizationV1, sanitize_memory_fact_payload, sanitize_provider_metadata_text,
};
use crate::tracedecay::current_timestamp;

const OPERATION: &str = "memory_v2_backfill_v1";
const BACKFILL_BATCH_SIZE: i64 = 64;
const RETENTION_CLASS: &str = "legacy-current-snapshot-v1";

pub(crate) async fn create_schema(conn: &Connection, operation: &str) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_v2_facts (
            fact_id TEXT PRIMARY KEY,
            owner_kind TEXT NOT NULL CHECK (owner_kind IN ('profile', 'project')),
            project_id TEXT,
            payload_access TEXT NOT NULL CHECK (
                payload_access IN (
                    'eligible', 'redacted', 'quarantined', 'retention_expired',
                    'deleted', 'unavailable', 'ambiguous'
                )
            ),
            trust_score REAL CHECK (
                trust_score IS NULL OR (trust_score >= 0.0 AND trust_score <= 1.0)
            ),
            current_assertion_id TEXT,
            last_event_id TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            CHECK (
                (owner_kind = 'profile' AND project_id IS NULL) OR
                (owner_kind = 'project' AND project_id IS NOT NULL)
            ),
            FOREIGN KEY (current_assertion_id)
                REFERENCES memory_v2_assertions(assertion_id),
            FOREIGN KEY (last_event_id)
                REFERENCES memory_v2_lineage_events(event_id)
        );

        CREATE TABLE IF NOT EXISTS memory_v2_assertions (
            assertion_id TEXT PRIMARY KEY,
            fact_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL CHECK (owner_kind IN ('profile', 'project')),
            project_id TEXT,
            assertion_kind TEXT NOT NULL CHECK (
                assertion_kind IN ('initial', 'correction', 'merge', 'legacy_import')
            ),
            payload_digest TEXT NOT NULL,
            payload_byte_len INTEGER NOT NULL CHECK (payload_byte_len >= 0),
            receipt_json TEXT NOT NULL CHECK (json_valid(receipt_json)),
            asserted_at INTEGER NOT NULL,
            actor_id TEXT,
            CHECK (
                (owner_kind = 'profile' AND project_id IS NULL) OR
                (owner_kind = 'project' AND project_id IS NOT NULL)
            ),
            FOREIGN KEY (fact_id) REFERENCES memory_v2_facts(fact_id)
        );

        CREATE TABLE IF NOT EXISTS memory_v2_assertion_payloads (
            assertion_id TEXT PRIMARY KEY,
            content TEXT NOT NULL,
            category TEXT NOT NULL CHECK (
                category IN ('general', 'user_pref', 'project', 'tool', 'decision', 'code_area')
            ),
            tags_json TEXT NOT NULL CHECK (json_valid(tags_json)),
            entities_json TEXT NOT NULL CHECK (json_valid(entities_json)),
            metadata_json TEXT NOT NULL CHECK (json_valid(metadata_json)),
            retention_class TEXT NOT NULL,
            FOREIGN KEY (assertion_id)
                REFERENCES memory_v2_assertions(assertion_id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS memory_v2_retrieval_anchors (
            anchor_id TEXT PRIMARY KEY,
            fact_id TEXT NOT NULL,
            record_json TEXT NOT NULL CHECK (json_valid(record_json)),
            observed_at INTEGER NOT NULL,
            FOREIGN KEY (fact_id) REFERENCES memory_v2_facts(fact_id)
        );

        CREATE TABLE IF NOT EXISTS memory_v2_evidence (
            evidence_id TEXT PRIMARY KEY,
            fact_id TEXT NOT NULL,
            assertion_id TEXT NOT NULL,
            anchor_id TEXT NOT NULL,
            relation TEXT NOT NULL CHECK (
                relation IN ('supports', 'contradicts', 'derived_from', 'copied_from', 'corrects')
            ),
            evidence_class TEXT NOT NULL CHECK (
                evidence_class IN (
                    'heuristic', 'inferred', 'derived_exact', 'user_declared',
                    'provider_declared', 'observed'
                )
            ),
            confidence REAL NOT NULL CHECK (confidence >= 0.0 AND confidence <= 1.0),
            observed_at INTEGER NOT NULL,
            FOREIGN KEY (fact_id) REFERENCES memory_v2_facts(fact_id),
            FOREIGN KEY (assertion_id) REFERENCES memory_v2_assertions(assertion_id),
            FOREIGN KEY (anchor_id) REFERENCES memory_v2_retrieval_anchors(anchor_id),
            UNIQUE (assertion_id, evidence_id)
        );

        CREATE TABLE IF NOT EXISTS memory_v2_lineage_events (
            event_seq INTEGER PRIMARY KEY AUTOINCREMENT,
            event_id TEXT NOT NULL UNIQUE,
            fact_id TEXT NOT NULL,
            assertion_id TEXT,
            kind_json TEXT NOT NULL CHECK (json_valid(kind_json)),
            occurred_at INTEGER NOT NULL,
            recorded_at INTEGER NOT NULL,
            origin_source TEXT NOT NULL,
            origin_id INTEGER NOT NULL,
            FOREIGN KEY (fact_id) REFERENCES memory_v2_facts(fact_id),
            FOREIGN KEY (assertion_id) REFERENCES memory_v2_assertions(assertion_id),
            UNIQUE (origin_source, origin_id)
        );

        CREATE TABLE IF NOT EXISTS memory_v2_legacy_map (
            source_store_id TEXT NOT NULL,
            legacy_fact_id INTEGER NOT NULL CHECK (legacy_fact_id > 0),
            fact_id TEXT NOT NULL UNIQUE,
            history_coverage TEXT NOT NULL CHECK (history_coverage IN ('complete', 'unknown')),
            migrated_at INTEGER NOT NULL,
            PRIMARY KEY (source_store_id, legacy_fact_id),
            FOREIGN KEY (fact_id) REFERENCES memory_v2_facts(fact_id)
        );

        CREATE TABLE IF NOT EXISTS memory_v2_backfill_progress (
            singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
            phase TEXT NOT NULL CHECK (phase IN ('feedback', 'oplog', 'facts', 'complete')),
            source_store_id TEXT,
            feedback_cursor INTEGER NOT NULL DEFAULT 0 CHECK (feedback_cursor >= 0),
            oplog_cursor INTEGER NOT NULL DEFAULT 0 CHECK (oplog_cursor >= 0),
            fact_cursor INTEGER NOT NULL DEFAULT 0 CHECK (fact_cursor >= 0),
            batch_size INTEGER NOT NULL DEFAULT 64 CHECK (batch_size BETWEEN 1 AND 500),
            snapshot_at INTEGER NOT NULL,
            completed_at INTEGER,
            updated_at INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_memory_v2_facts_owner
            ON memory_v2_facts(owner_kind, project_id);
        CREATE INDEX IF NOT EXISTS idx_memory_v2_facts_access_trust
            ON memory_v2_facts(payload_access, trust_score);
        CREATE INDEX IF NOT EXISTS idx_memory_v2_assertions_fact
            ON memory_v2_assertions(fact_id, asserted_at);
        CREATE INDEX IF NOT EXISTS idx_memory_v2_evidence_fact
            ON memory_v2_evidence(fact_id, observed_at);
        CREATE INDEX IF NOT EXISTS idx_memory_v2_evidence_anchor
            ON memory_v2_evidence(anchor_id);
        CREATE INDEX IF NOT EXISTS idx_memory_v2_events_fact
            ON memory_v2_lineage_events(fact_id, occurred_at, event_seq);
        CREATE INDEX IF NOT EXISTS idx_memory_v2_events_origin
            ON memory_v2_lineage_events(origin_source, origin_id);
        CREATE INDEX IF NOT EXISTS idx_memory_v2_anchors_fact
            ON memory_v2_retrieval_anchors(fact_id);
        CREATE INDEX IF NOT EXISTS idx_memory_v2_legacy_fact
            ON memory_v2_legacy_map(legacy_fact_id);

        CREATE VIRTUAL TABLE IF NOT EXISTS memory_v2_assertion_payloads_fts USING fts5(
            content, tags_json, entities_json, metadata_json,
            content='memory_v2_assertion_payloads', content_rowid='rowid'
        );
        CREATE TRIGGER IF NOT EXISTS memory_v2_payloads_fts_insert
            AFTER INSERT ON memory_v2_assertion_payloads BEGIN
                INSERT INTO memory_v2_assertion_payloads_fts(
                    rowid, content, tags_json, entities_json, metadata_json
                ) VALUES (
                    NEW.rowid, NEW.content, NEW.tags_json, NEW.entities_json, NEW.metadata_json
                );
            END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_payloads_fts_delete
            AFTER DELETE ON memory_v2_assertion_payloads BEGIN
                INSERT INTO memory_v2_assertion_payloads_fts(
                    memory_v2_assertion_payloads_fts,
                    rowid, content, tags_json, entities_json, metadata_json
                ) VALUES (
                    'delete', OLD.rowid, OLD.content, OLD.tags_json,
                    OLD.entities_json, OLD.metadata_json
                );
            END;

        CREATE TRIGGER IF NOT EXISTS memory_v2_facts_no_delete
            BEFORE DELETE ON memory_v2_facts BEGIN
                SELECT RAISE(ABORT, 'memory_v2_facts identities cannot be deleted');
            END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_assertions_no_update
            BEFORE UPDATE ON memory_v2_assertions BEGIN
                SELECT RAISE(ABORT, 'memory_v2_assertions is append-only');
            END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_assertions_no_delete
            BEFORE DELETE ON memory_v2_assertions BEGIN
                SELECT RAISE(ABORT, 'memory_v2_assertions is append-only');
            END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_payloads_no_update
            BEFORE UPDATE ON memory_v2_assertion_payloads BEGIN
                SELECT RAISE(ABORT, 'memory_v2_assertion_payloads is immutable');
            END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_evidence_no_update
            BEFORE UPDATE ON memory_v2_evidence BEGIN
                SELECT RAISE(ABORT, 'memory_v2_evidence is append-only');
            END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_evidence_no_delete
            BEFORE DELETE ON memory_v2_evidence BEGIN
                SELECT RAISE(ABORT, 'memory_v2_evidence is append-only');
            END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_events_no_update
            BEFORE UPDATE ON memory_v2_lineage_events BEGIN
                SELECT RAISE(ABORT, 'memory_v2_lineage_events is append-only');
            END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_events_no_delete
            BEFORE DELETE ON memory_v2_lineage_events BEGIN
                SELECT RAISE(ABORT, 'memory_v2_lineage_events is append-only');
            END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_anchors_no_update
            BEFORE UPDATE ON memory_v2_retrieval_anchors BEGIN
                SELECT RAISE(ABORT, 'memory_v2_retrieval_anchors is append-only');
            END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_anchors_no_delete
            BEFORE DELETE ON memory_v2_retrieval_anchors BEGIN
                SELECT RAISE(ABORT, 'memory_v2_retrieval_anchors is append-only');
            END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_legacy_map_no_update
            BEFORE UPDATE ON memory_v2_legacy_map BEGIN
                SELECT RAISE(ABORT, 'memory_v2_legacy_map is append-only');
            END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_legacy_map_no_delete
            BEFORE DELETE ON memory_v2_legacy_map BEGIN
                SELECT RAISE(ABORT, 'memory_v2_legacy_map is append-only');
            END;",
    )
    .await
    .map_err(|error| db_error(operation, error))?;

    let now = seconds_to_micros(current_timestamp())?;
    conn.execute(
        "INSERT OR IGNORE INTO memory_v2_backfill_progress (
            singleton, phase, snapshot_at, updated_at
         ) VALUES (1, 'feedback', ?1, ?1)",
        params![now],
    )
    .await
    .map_err(|error| db_error(operation, error))?;
    Ok(())
}

/// Completes any pending bounded backfill. Each batch and its cursor advance
/// commit together, so cancellation or process failure repeats at most one
/// idempotent batch on the next open.
pub(crate) async fn resume_backfill(conn: &Connection) -> Result<()> {
    conn.execute("PRAGMA secure_delete = ON", ())
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    while run_backfill_batch(conn).await? {}
    Ok(())
}

async fn run_backfill_batch(conn: &Connection) -> Result<bool> {
    conn.execute("BEGIN IMMEDIATE", ())
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    let result = run_backfill_batch_inner(conn).await;
    match result {
        Ok(more) => {
            conn.execute("COMMIT", ())
                .await
                .map_err(|error| db_error(OPERATION, error))?;
            Ok(more)
        }
        Err(error) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            Err(error)
        }
    }
}

async fn run_backfill_batch_inner(conn: &Connection) -> Result<bool> {
    let (phase, batch_size) = read_phase(conn).await?;
    match phase.as_str() {
        "feedback" => backfill_feedback_batch(conn, batch_size).await,
        "oplog" => backfill_oplog_batch(conn, batch_size).await,
        "facts" => backfill_fact_batch(conn, batch_size).await,
        "complete" => Ok(false),
        _ => Err(db_message(OPERATION, "invalid persisted backfill phase")),
    }
}

async fn read_phase(conn: &Connection) -> Result<(String, i64)> {
    let mut rows = conn
        .query(
            "SELECT phase, batch_size FROM memory_v2_backfill_progress WHERE singleton = 1",
            (),
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
        .ok_or_else(|| db_message(OPERATION, "missing backfill progress row"))?;
    Ok((
        row.get(0).map_err(|error| db_error(OPERATION, error))?,
        row.get(1).map_err(|error| db_error(OPERATION, error))?,
    ))
}

async fn backfill_feedback_batch(conn: &Connection, limit: i64) -> Result<bool> {
    let cursor = progress_cursor(conn, "feedback_cursor").await?;
    let mut rows = conn
        .query(
            "SELECT event_id, fact_id, action, trust_delta, old_trust, new_trust, created_at
             FROM memory_feedback_events
             WHERE event_id > ?1
             ORDER BY event_id
             LIMIT ?2",
            params![cursor, limit],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    let mut batch = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        batch.push(LegacyFeedback {
            event_id: row.get(0).map_err(|error| db_error(OPERATION, error))?,
            fact_id: row.get(1).map_err(|error| db_error(OPERATION, error))?,
            action: row.get(2).map_err(|error| db_error(OPERATION, error))?,
            trust_delta: row.get(3).map_err(|error| db_error(OPERATION, error))?,
            old_trust: row.get(4).map_err(|error| db_error(OPERATION, error))?,
            new_trust: row.get(5).map_err(|error| db_error(OPERATION, error))?,
            created_at: row.get(6).map_err(|error| db_error(OPERATION, error))?,
        });
    }
    if batch.is_empty() {
        set_phase(conn, "oplog").await?;
        return Ok(true);
    }
    let source_store_id = source_store_id(conn).await?;
    for feedback in &batch {
        let identity = ensure_legacy_identity(conn, &source_store_id, feedback.fact_id).await?;
        let event_id = derived_event_id(
            &source_store_id,
            "feedback",
            feedback.event_id,
            identity.fact_id.as_str(),
        )?;
        let action = match feedback.action.as_str() {
            "helpful" => "helpful",
            "unhelpful" => "unhelpful",
            _ => "other",
        };
        let kind = json!({
            "kind": "legacy_feedback_observed",
            "action": action,
            "trust_delta": feedback.trust_delta,
            "old_trust": feedback.old_trust,
            "new_trust": feedback.new_trust,
            "history_coverage": "unknown"
        });
        insert_event(
            conn,
            &event_id,
            &identity.fact_id,
            None,
            &kind,
            seconds_to_micros(feedback.created_at)?,
            "memory_feedback_events",
            feedback.event_id,
        )
        .await?;
        update_placeholder_last_event(conn, &identity.fact_id, &event_id).await?;
    }
    set_cursor(
        conn,
        "feedback_cursor",
        batch.last().expect("non-empty").event_id,
    )
    .await?;
    Ok(true)
}

async fn backfill_oplog_batch(conn: &Connection, limit: i64) -> Result<bool> {
    let cursor = progress_cursor(conn, "oplog_cursor").await?;
    let mut rows = conn
        .query(
            "SELECT id, ts, op, fact_id
             FROM memory_oplog
             WHERE id > ?1
             ORDER BY id
             LIMIT ?2",
            params![cursor, limit],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    let mut batch = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        batch.push(LegacyOplog {
            id: row.get(0).map_err(|error| db_error(OPERATION, error))?,
            ts: row.get(1).map_err(|error| db_error(OPERATION, error))?,
            op: row.get(2).map_err(|error| db_error(OPERATION, error))?,
            fact_id: row.get(3).map_err(|error| db_error(OPERATION, error))?,
        });
    }
    if batch.is_empty() {
        set_phase(conn, "facts").await?;
        return Ok(true);
    }
    let source_store_id = source_store_id(conn).await?;
    for item in &batch {
        let Some(legacy_fact_id) = item.fact_id else {
            continue;
        };
        let identity = ensure_legacy_identity(conn, &source_store_id, legacy_fact_id).await?;
        let event_id = derived_event_id(
            &source_store_id,
            "oplog",
            item.id,
            identity.fact_id.as_str(),
        )?;
        let op = safe_op(&item.op);
        let kind = json!({
            "kind": "legacy_oplog_observed",
            "operation": op,
            "history_coverage": "unknown"
        });
        insert_event(
            conn,
            &event_id,
            &identity.fact_id,
            None,
            &kind,
            seconds_to_micros(item.ts)?,
            "memory_oplog",
            item.id,
        )
        .await?;
        if op == "remove" {
            conn.execute(
                "UPDATE memory_v2_facts
                 SET payload_access = 'deleted', current_assertion_id = NULL,
                     last_event_id = ?1, updated_at = ?2
                 WHERE fact_id = ?3",
                params![
                    event_id.as_str(),
                    seconds_to_micros(item.ts)?,
                    identity.fact_id.as_str()
                ],
            )
            .await
            .map_err(|error| db_error(OPERATION, error))?;
        } else {
            update_placeholder_last_event(conn, &identity.fact_id, &event_id).await?;
        }
    }
    set_cursor(conn, "oplog_cursor", batch.last().expect("non-empty").id).await?;
    Ok(true)
}

async fn backfill_fact_batch(conn: &Connection, limit: i64) -> Result<bool> {
    let cursor = progress_cursor(conn, "fact_cursor").await?;
    let mut rows = conn
        .query(
            "SELECT fact_id, content, category, tags, trust_score, source, metadata,
                    created_at, updated_at
             FROM memory_facts
             WHERE fact_id > ?1
             ORDER BY fact_id
             LIMIT ?2",
            params![cursor, limit],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    let mut batch = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        batch.push(LegacyFact {
            fact_id: row.get(0).map_err(|error| db_error(OPERATION, error))?,
            content: row.get(1).map_err(|error| db_error(OPERATION, error))?,
            category: row.get(2).map_err(|error| db_error(OPERATION, error))?,
            tags_json: row.get(3).map_err(|error| db_error(OPERATION, error))?,
            trust_score: row.get(4).map_err(|error| db_error(OPERATION, error))?,
            source: row.get(5).map_err(|error| db_error(OPERATION, error))?,
            metadata_json: row.get(6).map_err(|error| db_error(OPERATION, error))?,
            created_at: row.get(7).map_err(|error| db_error(OPERATION, error))?,
            updated_at: row.get(8).map_err(|error| db_error(OPERATION, error))?,
        });
    }
    if batch.is_empty() {
        let now = seconds_to_micros(current_timestamp())?;
        conn.execute(
            "UPDATE memory_v2_backfill_progress
             SET phase = 'complete', completed_at = ?1, updated_at = ?1
             WHERE singleton = 1",
            params![now],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
        return Ok(false);
    }
    let source_store_id = source_store_id(conn).await?;
    for fact in &batch {
        backfill_fact(conn, &source_store_id, fact).await?;
    }
    set_cursor(
        conn,
        "fact_cursor",
        batch.last().expect("non-empty").fact_id,
    )
    .await?;
    Ok(true)
}

async fn backfill_fact(
    conn: &Connection,
    source_store_id: &SourceStoreId,
    legacy: &LegacyFact,
) -> Result<()> {
    let identity = ensure_legacy_identity(conn, source_store_id, legacy.fact_id).await?;
    let tags: Vec<String> = serde_json::from_str(&legacy.tags_json)
        .map_err(|_| db_message(OPERATION, "legacy fact tags are not valid JSON"))?;
    let metadata: Value = serde_json::from_str(&legacy.metadata_json)
        .map_err(|_| db_message(OPERATION, "legacy fact metadata is not valid JSON"))?;
    let entities = load_legacy_entities(conn, legacy.fact_id).await?;
    let original = json!({
        "content": legacy.content,
        "category": legacy.category,
        "tags": tags,
        "entities": entities,
        "metadata": metadata
    });
    match sanitize_memory_fact_payload(original.clone())
        .map_err(|_| db_message(OPERATION, "legacy fact privacy sanitization failed"))?
    {
        MemoryFactSanitizationV1::Quarantined => {
            quarantine_fact(conn, &identity.fact_id, legacy).await
        }
        MemoryFactSanitizationV1::Durable { payload, receipt } => {
            if legacy_content_conflicts(conn, legacy.fact_id, payload_string(&payload, "content")?)
                .await?
            {
                return quarantine_fact(conn, &identity.fact_id, legacy).await;
            }
            let content = payload_string(&payload, "content")?.to_string();
            let category_text = payload_string(&payload, "category")?.to_string();
            let category = parse_fact_category(&category_text)?;
            let tags = payload_strings(&payload, "tags")?;
            let entities = payload_strings(&payload, "entities")?;
            let metadata = payload
                .get("metadata")
                .cloned()
                .ok_or_else(|| db_message(OPERATION, "sanitized fact metadata is missing"))?;
            let retention_class = RetentionClass::new(RETENTION_CLASS)
                .map_err(|_| db_message(OPERATION, "invalid fact retention class"))?;
            let fact_payload = FactPayloadV1::new(
                content.clone(),
                category,
                tags.clone(),
                entities.clone(),
                metadata.clone(),
                receipt.clone(),
                retention_class,
            )
            .map_err(|_| db_message(OPERATION, "sanitized fact payload violates its contract"))?;
            let snapshot_at = snapshot_at(conn).await?;
            let anchor_id = derived_anchor_id(source_store_id, legacy.fact_id)?;
            let evidence = FactEvidenceRefV1::new(
                identity.fact_id.clone(),
                anchor_id.clone(),
                FactEvidenceRelationV1::CopiedFrom,
                EvidenceClass::Observed,
                Confidence::new(1.0)
                    .map_err(|_| db_message(OPERATION, "invalid legacy evidence confidence"))?,
            )
            .map_err(|_| db_message(OPERATION, "invalid legacy fact evidence"))?;
            let assertion = FactAssertionV1::new(
                identity.fact_id.clone(),
                FactOwnerV1::Profile,
                FactAssertionKindV1::LegacyImport,
                fact_payload,
                vec![evidence.clone()],
                UtcMicros(snapshot_at),
                None,
            )
            .map_err(|_| db_message(OPERATION, "invalid legacy fact assertion"))?;
            let assertion_event = FactLineageEventV1::new(
                identity.fact_id.clone(),
                FactOwnerV1::Profile,
                FactLineageEventKindV1::AssertionRecorded {
                    assertion_id: assertion.assertion_id().clone(),
                },
                UtcMicros(snapshot_at),
                None,
            )
            .map_err(|_| db_message(OPERATION, "invalid legacy assertion event"))?;
            insert_durable_snapshot(
                conn,
                source_store_id,
                legacy,
                &payload,
                &receipt,
                &anchor_id,
                &evidence,
                &assertion,
                &assertion_event,
            )
            .await?;
            let safe_source = sanitize_provider_metadata_text(&legacy.source)
                .unwrap_or_else(|| "withheld".to_string());
            if payload != original || safe_source != legacy.source {
                mirror_sanitized_legacy_fact(
                    conn,
                    legacy,
                    &content,
                    &category_text,
                    &tags,
                    &entities,
                    &metadata,
                    &safe_source,
                )
                .await?;
            }
            let access = match receipt.disposition() {
                SanitizerDispositionV1::Accepted => "eligible",
                SanitizerDispositionV1::Redacted => "redacted",
                SanitizerDispositionV1::Rejected | SanitizerDispositionV1::Quarantined => {
                    return Err(db_message(OPERATION, "durable receipt forbids payload"));
                }
            };
            Confidence::new(legacy.trust_score)
                .map_err(|_| db_message(OPERATION, "legacy fact trust is invalid"))?;
            conn.execute(
                "UPDATE memory_v2_facts
                 SET payload_access = ?1, trust_score = ?2,
                     current_assertion_id = ?3, last_event_id = ?4,
                     created_at = ?5, updated_at = ?6
                 WHERE fact_id = ?7",
                params![
                    access,
                    legacy.trust_score,
                    assertion.assertion_id().as_str(),
                    assertion_event.event_id().as_str(),
                    seconds_to_micros(legacy.created_at)?,
                    seconds_to_micros(legacy.updated_at)?,
                    identity.fact_id.as_str(),
                ],
            )
            .await
            .map_err(|error| db_error(OPERATION, error))?;
            Ok(())
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn insert_durable_snapshot(
    conn: &Connection,
    source_store_id: &SourceStoreId,
    legacy: &LegacyFact,
    payload: &Value,
    receipt: &tracedecay_domain::SanitizationReceiptV1,
    anchor_id: &RetrievalAnchorId,
    evidence: &FactEvidenceRefV1,
    assertion: &FactAssertionV1,
    assertion_event: &FactLineageEventV1,
) -> Result<()> {
    let snapshot_at = snapshot_at(conn).await?;
    let anchor_record = json!({
        "kind": "legacy_current_snapshot",
        "source_store_id": source_store_id.as_str(),
        "legacy_fact_id": legacy.fact_id,
        "history_coverage": "unknown"
    });
    conn.execute(
        "INSERT OR IGNORE INTO memory_v2_retrieval_anchors (
            anchor_id, fact_id, record_json, observed_at
         ) VALUES (?1, ?2, ?3, ?4)",
        params![
            anchor_id.as_str(),
            assertion.fact_id().as_str(),
            json_text(&anchor_record)?,
            snapshot_at,
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    let payload_reference = assertion
        .payload()
        .payload_reference()
        .map_err(|_| db_message(OPERATION, "invalid assertion payload reference"))?;
    conn.execute(
        "INSERT OR IGNORE INTO memory_v2_assertions (
            assertion_id, fact_id, owner_kind, project_id, assertion_kind,
            payload_digest, payload_byte_len, receipt_json, asserted_at, actor_id
         ) VALUES (?1, ?2, 'profile', NULL, 'legacy_import', ?3, ?4, ?5, ?6, NULL)",
        params![
            assertion.assertion_id().as_str(),
            assertion.fact_id().as_str(),
            payload_reference.digest().as_str(),
            payload_reference.byte_len() as i64,
            serde_json::to_string(receipt)
                .map_err(|_| db_message(OPERATION, "failed to encode fact receipt"))?,
            snapshot_at,
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    conn.execute(
        "INSERT OR IGNORE INTO memory_v2_assertion_payloads (
            assertion_id, content, category, tags_json, entities_json,
            metadata_json, retention_class
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            assertion.assertion_id().as_str(),
            payload_string(payload, "content")?,
            payload_string(payload, "category")?,
            json_text(
                payload
                    .get("tags")
                    .ok_or_else(|| { db_message(OPERATION, "sanitized fact tags are missing") })?
            )?,
            json_text(payload.get("entities").ok_or_else(|| {
                db_message(OPERATION, "sanitized fact entities are missing")
            })?)?,
            json_text(
                payload.get("metadata").ok_or_else(|| {
                    db_message(OPERATION, "sanitized fact metadata is missing")
                })?
            )?,
            RETENTION_CLASS,
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    conn.execute(
        "INSERT OR IGNORE INTO memory_v2_evidence (
            evidence_id, fact_id, assertion_id, anchor_id, relation,
            evidence_class, confidence, observed_at
         ) VALUES (?1, ?2, ?3, ?4, 'copied_from', 'observed', ?5, ?6)",
        params![
            evidence.evidence_id().as_str(),
            evidence.fact_id().as_str(),
            assertion.assertion_id().as_str(),
            evidence.anchor_id().as_str(),
            evidence.confidence().as_f64(),
            snapshot_at,
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    insert_event(
        conn,
        assertion_event.event_id(),
        assertion_event.fact_id(),
        Some(assertion.assertion_id().as_str()),
        &serde_json::to_value(assertion_event.kind())
            .map_err(|_| db_message(OPERATION, "failed to encode assertion event"))?,
        snapshot_at,
        "memory_facts_snapshot",
        legacy.fact_id,
    )
    .await
}

async fn quarantine_fact(conn: &Connection, fact_id: &FactId, legacy: &LegacyFact) -> Result<()> {
    let source_store_id = source_store_id(conn).await?;
    let event_id = derived_event_id(
        &source_store_id,
        "quarantine",
        legacy.fact_id,
        fact_id.as_str(),
    )?;
    let kind = json!({
        "kind": "legacy_payload_quarantined",
        "history_coverage": "unknown"
    });
    let snapshot_at = snapshot_at(conn).await?;
    insert_event(
        conn,
        &event_id,
        fact_id,
        None,
        &kind,
        snapshot_at,
        "memory_facts_quarantine",
        legacy.fact_id,
    )
    .await?;
    purge_legacy_projection(conn, legacy.fact_id).await?;
    conn.execute(
        "UPDATE memory_v2_facts
         SET payload_access = 'quarantined', trust_score = ?1,
             current_assertion_id = NULL, last_event_id = ?2,
             created_at = ?3, updated_at = ?4
         WHERE fact_id = ?5",
        params![
            legacy.trust_score,
            event_id.as_str(),
            seconds_to_micros(legacy.created_at)?,
            snapshot_at,
            fact_id.as_str(),
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

async fn ensure_legacy_identity(
    conn: &Connection,
    source_store_id: &SourceStoreId,
    legacy_fact_id: i64,
) -> Result<LegacyIdentity> {
    if legacy_fact_id <= 0 {
        return Err(db_message(OPERATION, "legacy fact id must be positive"));
    }
    let snapshot_at = snapshot_at(conn).await?;
    let owner = FactOwnerV1::Profile;
    let fact_id = FactId::derive(
        &FactIdentityMaterialV1::new(
            owner.clone(),
            FactIdentitySourceV1::Legacy {
                source_store_id: source_store_id.clone(),
                legacy_fact_id,
            },
        )
        .map_err(|_| db_message(OPERATION, "invalid legacy fact identity material"))?,
    )
    .map_err(|_| db_message(OPERATION, "failed to derive legacy fact identity"))?;
    let mapping = LegacyFactMappingV1::new(
        owner.clone(),
        source_store_id.clone(),
        legacy_fact_id,
        fact_id.clone(),
        LegacyHistoryCoverageV1::Unknown,
        UtcMicros(snapshot_at),
    )
    .map_err(|_| db_message(OPERATION, "invalid legacy fact mapping"))?;
    let import_event = FactLineageEventV1::new(
        fact_id.clone(),
        owner,
        FactLineageEventKindV1::LegacyImported {
            mapping: mapping.clone(),
        },
        UtcMicros(snapshot_at),
        None,
    )
    .map_err(|_| db_message(OPERATION, "invalid legacy import event"))?;
    conn.execute(
        "INSERT OR IGNORE INTO memory_v2_facts (
            fact_id, owner_kind, project_id, payload_access, trust_score,
            current_assertion_id, last_event_id, created_at, updated_at
         ) VALUES (?1, 'profile', NULL, 'unavailable', NULL, NULL, NULL, ?2, ?2)",
        params![fact_id.as_str(), snapshot_at],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    conn.execute(
        "INSERT OR IGNORE INTO memory_v2_legacy_map (
            source_store_id, legacy_fact_id, fact_id, history_coverage, migrated_at
         ) VALUES (?1, ?2, ?3, 'unknown', ?4)",
        params![
            source_store_id.as_str(),
            legacy_fact_id,
            fact_id.as_str(),
            snapshot_at
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    insert_event(
        conn,
        import_event.event_id(),
        &fact_id,
        None,
        &serde_json::to_value(import_event.kind())
            .map_err(|_| db_message(OPERATION, "failed to encode legacy import event"))?,
        snapshot_at,
        "memory_v2_legacy_map",
        legacy_fact_id,
    )
    .await?;
    conn.execute(
        "UPDATE memory_v2_facts
         SET last_event_id = COALESCE(last_event_id, ?1)
         WHERE fact_id = ?2",
        params![import_event.event_id().as_str(), fact_id.as_str()],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(LegacyIdentity { fact_id })
}

#[allow(clippy::too_many_arguments)]
async fn mirror_sanitized_legacy_fact(
    conn: &Connection,
    legacy: &LegacyFact,
    content: &str,
    category: &str,
    tags: &[String],
    entities: &[String],
    metadata: &Value,
    source: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE memory_facts
         SET content = ?1, category = ?2, tags = ?3, metadata = ?4,
             source = ?5, hrr_vector = NULL
         WHERE fact_id = ?6",
        params![
            content,
            category,
            json_text(tags)?,
            json_text(metadata)?,
            source,
            legacy.fact_id,
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    replace_legacy_entities(conn, legacy.fact_id, entities).await?;
    invalidate_legacy_banks(conn, &legacy.category).await
}

async fn replace_legacy_entities(
    conn: &Connection,
    legacy_fact_id: i64,
    entities: &[String],
) -> Result<()> {
    let mut rows = conn
        .query(
            "SELECT entity_id FROM memory_fact_entities WHERE fact_id = ?1",
            params![legacy_fact_id],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    let mut old_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        old_ids.push(
            row.get::<i64>(0)
                .map_err(|error| db_error(OPERATION, error))?,
        );
    }
    conn.execute(
        "DELETE FROM memory_fact_entities WHERE fact_id = ?1",
        params![legacy_fact_id],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    for entity in entities {
        let normalized = entity.to_ascii_lowercase();
        let now = current_timestamp();
        conn.execute(
            "INSERT OR IGNORE INTO memory_entities (
                name, normalized_name, entity_type, aliases, created_at, updated_at
             ) VALUES (?1, ?2, 'unknown', '[]', ?3, ?3)",
            params![entity.as_str(), normalized.as_str(), now],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
        conn.execute(
            "INSERT OR IGNORE INTO memory_fact_entities (fact_id, entity_id)
             SELECT ?1, entity_id FROM memory_entities WHERE normalized_name = ?2",
            params![legacy_fact_id, normalized.as_str()],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    }
    for entity_id in old_ids {
        conn.execute(
            "DELETE FROM memory_entities
             WHERE entity_id = ?1
               AND NOT EXISTS (
                   SELECT 1 FROM memory_fact_entities WHERE entity_id = ?1
               )",
            params![entity_id],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    }
    Ok(())
}

/// Purges every searchable/derived payload byte while retaining typed identity,
/// legacy mapping, append-only lineage, and an explicit tombstone event.
pub(crate) async fn purge_fact_payload(conn: &Connection, legacy_fact_id: i64) -> Result<bool> {
    conn.execute("PRAGMA secure_delete = ON", ())
        .await
        .map_err(|error| db_error("memory_v2_purge", error))?;
    conn.execute("BEGIN IMMEDIATE", ())
        .await
        .map_err(|error| db_error("memory_v2_purge", error))?;
    let result = purge_fact_payload_inner(conn, legacy_fact_id).await;
    match result {
        Ok(changed) => {
            conn.execute("COMMIT", ())
                .await
                .map_err(|error| db_error("memory_v2_purge", error))?;
            Ok(changed)
        }
        Err(error) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            Err(error)
        }
    }
}

async fn purge_fact_payload_inner(conn: &Connection, legacy_fact_id: i64) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT m.fact_id, m.source_store_id, f.payload_access
             FROM memory_v2_legacy_map m
             JOIN memory_v2_facts f ON f.fact_id = m.fact_id
             WHERE m.legacy_fact_id = ?1",
            params![legacy_fact_id],
        )
        .await
        .map_err(|error| db_error("memory_v2_purge", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error("memory_v2_purge", error))?
    else {
        return Ok(false);
    };
    let fact_id_text: String = row
        .get(0)
        .map_err(|error| db_error("memory_v2_purge", error))?;
    let source_store_text: String = row
        .get(1)
        .map_err(|error| db_error("memory_v2_purge", error))?;
    let access: String = row
        .get(2)
        .map_err(|error| db_error("memory_v2_purge", error))?;
    let fact_id = FactId::new(fact_id_text)
        .map_err(|_| db_message("memory_v2_purge", "stored fact id is invalid"))?;
    let source_store_id = SourceStoreId::new(source_store_text)
        .map_err(|_| db_message("memory_v2_purge", "stored source store id is invalid"))?;
    let event_id = derived_event_id(&source_store_id, "delete", legacy_fact_id, fact_id.as_str())?;
    let now = seconds_to_micros(current_timestamp())?;
    let kind = json!({
        "kind": "curated",
        "action": { "kind": "forgotten" },
        "evidence_ids": []
    });
    insert_event(
        conn,
        &event_id,
        &fact_id,
        None,
        &kind,
        now,
        "memory_v2_delete",
        legacy_fact_id,
    )
    .await?;
    conn.execute(
        "DELETE FROM memory_v2_assertion_payloads
         WHERE assertion_id IN (
             SELECT assertion_id FROM memory_v2_assertions WHERE fact_id = ?1
         )",
        params![fact_id.as_str()],
    )
    .await
    .map_err(|error| db_error("memory_v2_purge", error))?;
    purge_legacy_projection(conn, legacy_fact_id).await?;
    conn.execute(
        "UPDATE memory_v2_facts
         SET payload_access = 'deleted', current_assertion_id = NULL,
             last_event_id = ?1, updated_at = ?2
         WHERE fact_id = ?3",
        params![event_id.as_str(), now, fact_id.as_str()],
    )
    .await
    .map_err(|error| db_error("memory_v2_purge", error))?;
    Ok(access != "deleted")
}

async fn purge_legacy_projection(conn: &Connection, legacy_fact_id: i64) -> Result<()> {
    let mut rows = conn
        .query(
            "SELECT category FROM memory_facts WHERE fact_id = ?1",
            params![legacy_fact_id],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    let category = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
        .map(|row| row.get::<String>(0))
        .transpose()
        .map_err(|error| db_error(OPERATION, error))?;
    conn.execute(
        "DELETE FROM memory_facts WHERE fact_id = ?1",
        params![legacy_fact_id],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    if let Some(category) = category {
        invalidate_legacy_banks(conn, &category).await?;
    }
    Ok(())
}

async fn invalidate_legacy_banks(conn: &Connection, category: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM memory_banks WHERE bank_name IN ('all', ?1)",
        params![category],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    let now = current_timestamp();
    for bank_name in ["all", category] {
        conn.execute(
            "INSERT INTO memory_bank_dirty (bank_name, updated_at)
             VALUES (?1, ?2)
             ON CONFLICT(bank_name) DO UPDATE SET
                 updated_at = max(excluded.updated_at, memory_bank_dirty.updated_at + 1)",
            params![bank_name, now],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    }
    Ok(())
}

async fn insert_event(
    conn: &Connection,
    event_id: &FactEventId,
    fact_id: &FactId,
    assertion_id: Option<&str>,
    kind: &Value,
    occurred_at: i64,
    origin_source: &str,
    origin_id: i64,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO memory_v2_lineage_events (
            event_id, fact_id, assertion_id, kind_json, occurred_at,
            recorded_at, origin_source, origin_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            event_id.as_str(),
            fact_id.as_str(),
            assertion_id,
            json_text(kind)?,
            occurred_at,
            seconds_to_micros(current_timestamp())?,
            origin_source,
            origin_id,
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

async fn update_placeholder_last_event(
    conn: &Connection,
    fact_id: &FactId,
    event_id: &FactEventId,
) -> Result<()> {
    conn.execute(
        "UPDATE memory_v2_facts
         SET last_event_id = CASE
             WHEN current_assertion_id IS NULL THEN ?1 ELSE last_event_id END
         WHERE fact_id = ?2",
        params![event_id.as_str(), fact_id.as_str()],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

async fn source_store_id(conn: &Connection) -> Result<SourceStoreId> {
    conn.execute(
        "UPDATE memory_v2_backfill_progress
         SET source_store_id = 'legacy-memory-v1.' || lower(hex(randomblob(16)))
         WHERE singleton = 1 AND source_store_id IS NULL",
        (),
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    let mut rows = conn
        .query(
            "SELECT source_store_id FROM memory_v2_backfill_progress WHERE singleton = 1",
            (),
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
        .ok_or_else(|| db_message(OPERATION, "missing backfill progress row"))?;
    let value: String = row.get(0).map_err(|error| db_error(OPERATION, error))?;
    SourceStoreId::new(value).map_err(|_| db_message(OPERATION, "invalid source store id"))
}

async fn snapshot_at(conn: &Connection) -> Result<i64> {
    let mut rows = conn
        .query(
            "SELECT snapshot_at FROM memory_v2_backfill_progress WHERE singleton = 1",
            (),
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    rows.next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
        .ok_or_else(|| db_message(OPERATION, "missing backfill progress row"))?
        .get(0)
        .map_err(|error| db_error(OPERATION, error))
}

async fn progress_cursor(conn: &Connection, column: &str) -> Result<i64> {
    let sql = match column {
        "feedback_cursor" => {
            "SELECT feedback_cursor FROM memory_v2_backfill_progress WHERE singleton = 1"
        }
        "oplog_cursor" => {
            "SELECT oplog_cursor FROM memory_v2_backfill_progress WHERE singleton = 1"
        }
        "fact_cursor" => "SELECT fact_cursor FROM memory_v2_backfill_progress WHERE singleton = 1",
        _ => return Err(db_message(OPERATION, "invalid backfill cursor")),
    };
    let mut rows = conn
        .query(sql, ())
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    rows.next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
        .ok_or_else(|| db_message(OPERATION, "missing backfill progress row"))?
        .get(0)
        .map_err(|error| db_error(OPERATION, error))
}

async fn set_cursor(conn: &Connection, column: &str, value: i64) -> Result<()> {
    let sql = match column {
        "feedback_cursor" => {
            "UPDATE memory_v2_backfill_progress SET feedback_cursor = ?1, updated_at = ?2 WHERE singleton = 1"
        }
        "oplog_cursor" => {
            "UPDATE memory_v2_backfill_progress SET oplog_cursor = ?1, updated_at = ?2 WHERE singleton = 1"
        }
        "fact_cursor" => {
            "UPDATE memory_v2_backfill_progress SET fact_cursor = ?1, updated_at = ?2 WHERE singleton = 1"
        }
        _ => return Err(db_message(OPERATION, "invalid backfill cursor")),
    };
    conn.execute(sql, params![value, seconds_to_micros(current_timestamp())?])
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

async fn set_phase(conn: &Connection, phase: &str) -> Result<()> {
    conn.execute(
        "UPDATE memory_v2_backfill_progress SET phase = ?1, updated_at = ?2 WHERE singleton = 1",
        params![phase, seconds_to_micros(current_timestamp())?],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

async fn load_legacy_entities(conn: &Connection, legacy_fact_id: i64) -> Result<Vec<String>> {
    let mut rows = conn
        .query(
            "SELECT e.name
             FROM memory_fact_entities fe
             JOIN memory_entities e ON e.entity_id = fe.entity_id
             WHERE fe.fact_id = ?1
             ORDER BY e.name",
            params![legacy_fact_id],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    let mut entities = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        entities.push(row.get(0).map_err(|error| db_error(OPERATION, error))?);
    }
    Ok(entities)
}

async fn legacy_content_conflicts(
    conn: &Connection,
    legacy_fact_id: i64,
    content: &str,
) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM memory_facts WHERE content = ?1 AND fact_id != ?2 LIMIT 1",
            params![content, legacy_fact_id],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    Ok(rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
        .is_some())
}

fn derived_anchor_id(
    source_store_id: &SourceStoreId,
    legacy_fact_id: i64,
) -> Result<RetrievalAnchorId> {
    RetrievalAnchorId::new(derived_id(
        "retrieval.legacy-memory.v1",
        source_store_id.as_str(),
        legacy_fact_id,
        "snapshot",
    ))
    .map_err(|_| db_message(OPERATION, "failed to derive retrieval anchor id"))
}

fn derived_event_id(
    source_store_id: &SourceStoreId,
    source: &str,
    source_id: i64,
    fact_id: &str,
) -> Result<FactEventId> {
    FactEventId::new(derived_id(
        "fact-event.legacy-memory.v1",
        source_store_id.as_str(),
        source_id,
        &format!("{source}:{fact_id}"),
    ))
    .map_err(|_| db_message(OPERATION, "failed to derive legacy event id"))
}

fn derived_id(prefix: &str, source_store_id: &str, source_id: i64, kind: &str) -> String {
    let mut hash = Sha256::new();
    for value in [
        prefix.as_bytes(),
        source_store_id.as_bytes(),
        &source_id.to_be_bytes(),
        kind.as_bytes(),
    ] {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value);
    }
    format!("{prefix}.{}", hex::encode(hash.finalize()))
}

fn parse_fact_category(value: &str) -> Result<tracedecay_domain::FactCategoryV1> {
    use tracedecay_domain::FactCategoryV1;
    match value {
        "general" => Ok(FactCategoryV1::General),
        "user_pref" => Ok(FactCategoryV1::UserPref),
        "project" => Ok(FactCategoryV1::Project),
        "tool" => Ok(FactCategoryV1::Tool),
        "decision" => Ok(FactCategoryV1::Decision),
        "code_area" => Ok(FactCategoryV1::CodeArea),
        _ => Err(db_message(OPERATION, "legacy fact category is unsupported")),
    }
}

fn payload_string<'a>(payload: &'a Value, field: &str) -> Result<&'a str> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| db_message(OPERATION, "sanitized fact string field is invalid"))
}

fn payload_strings(payload: &Value, field: &str) -> Result<Vec<String>> {
    let values = payload
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| db_message(OPERATION, "sanitized fact list field is invalid"))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| db_message(OPERATION, "sanitized fact list item is invalid"))
        })
        .collect()
}

fn safe_op(value: &str) -> &'static str {
    match value {
        "add" => "add",
        "update" => "update",
        "remove" => "remove",
        "feedback" => "feedback",
        "reject_secret_like" => "reject_secret_like",
        "curation_apply" => "curation_apply",
        _ => "other",
    }
}

fn seconds_to_micros(value: i64) -> Result<i64> {
    value
        .checked_mul(1_000_000)
        .ok_or_else(|| db_message(OPERATION, "legacy timestamp is out of range"))
}

fn json_text(value: &(impl serde::Serialize + ?Sized)) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|_| db_message(OPERATION, "failed to encode safe migration JSON"))
}

fn db_error(operation: &str, error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        message: format!("{operation}: {error}"),
        operation: operation.to_string(),
    }
}

fn db_message(operation: &str, message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Database {
        message: message.into(),
        operation: operation.to_string(),
    }
}

struct LegacyIdentity {
    fact_id: FactId,
}

struct LegacyFeedback {
    event_id: i64,
    fact_id: i64,
    action: String,
    trust_delta: f64,
    old_trust: f64,
    new_trust: f64,
    created_at: i64,
}

struct LegacyOplog {
    id: i64,
    ts: i64,
    op: String,
    fact_id: Option<i64>,
}

struct LegacyFact {
    fact_id: i64,
    content: String,
    category: String,
    tags_json: String,
    trust_score: f64,
    source: String,
    metadata_json: String,
    created_at: i64,
    updated_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn database() -> (Connection, libsql::Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = libsql::Builder::new_local(dir.path().join("memory-v2.db"))
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .await
            .unwrap();
        crate::db::migrations::create_schema(&conn).await.unwrap();
        (conn, db, dir)
    }

    async fn reset_backfill(conn: &Connection, batch_size: i64) {
        conn.execute(
            "UPDATE memory_v2_backfill_progress
             SET phase = 'feedback', source_store_id = NULL,
                 feedback_cursor = 0, oplog_cursor = 0, fact_cursor = 0,
                 batch_size = ?1, completed_at = NULL",
            params![batch_size],
        )
        .await
        .unwrap();
    }

    async fn scalar(conn: &Connection, sql: &str) -> i64 {
        let mut rows = conn.query(sql, ()).await.unwrap();
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }

    #[tokio::test]
    async fn legacy_ids_are_preserved_by_deterministic_mapping() {
        let (conn, _db, _dir) = database().await;
        conn.execute(
            "INSERT INTO memory_facts (
                fact_id, content, category, trust_score, created_at, updated_at
             ) VALUES (41, 'keep compatibility', 'project', 0.75, 10, 11)",
            (),
        )
        .await
        .unwrap();
        reset_backfill(&conn, BACKFILL_BATCH_SIZE).await;
        resume_backfill(&conn).await.unwrap();

        let mut rows = conn
            .query(
                "SELECT source_store_id, fact_id FROM memory_v2_legacy_map
                 WHERE legacy_fact_id = 41",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let source_store_id = SourceStoreId::new(row.get::<String>(0).unwrap()).unwrap();
        let stored = row.get::<String>(1).unwrap();
        let expected = FactId::derive(
            &FactIdentityMaterialV1::new(
                FactOwnerV1::Profile,
                FactIdentitySourceV1::Legacy {
                    source_store_id,
                    legacy_fact_id: 41,
                },
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(stored, expected.as_str());
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM memory_facts
                 WHERE fact_id = 41 AND content = 'keep compatibility'"
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn backfill_resumes_after_a_committed_partial_batch() {
        let (conn, _db, _dir) = database().await;
        for id in [1, 2] {
            conn.execute(
                "INSERT INTO memory_facts (
                    fact_id, content, category, created_at, updated_at
                 ) VALUES (?1, ?2, 'general', 10, 10)",
                params![id, format!("fact {id}")],
            )
            .await
            .unwrap();
        }
        reset_backfill(&conn, 1).await;
        assert!(run_backfill_batch(&conn).await.unwrap());
        assert!(run_backfill_batch(&conn).await.unwrap());
        assert!(run_backfill_batch(&conn).await.unwrap());
        assert_eq!(progress_cursor(&conn, "fact_cursor").await.unwrap(), 1);

        resume_backfill(&conn).await.unwrap();
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM memory_v2_legacy_map").await,
            2
        );
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM memory_v2_assertions").await,
            2
        );
        resume_backfill(&conn).await.unwrap();
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM memory_v2_assertions").await,
            2
        );
    }

    #[tokio::test]
    async fn backfill_imports_only_observed_feedback_and_oplog_rows() {
        let (conn, _db, _dir) = database().await;
        conn.execute(
            "INSERT INTO memory_facts (
                fact_id, content, category, helpful_count, unhelpful_count,
                created_at, updated_at
             ) VALUES (7, 'observed history only', 'general', 9, 8, 10, 10)",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO memory_feedback_events (
                event_id, fact_id, action, trust_delta, old_trust, new_trust, created_at
             ) VALUES (3, 7, 'helpful', 0.1, 0.5, 0.6, 11)",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO memory_oplog (id, ts, op, fact_id, detail_json)
             VALUES (4, 12, 'update', 7, '{\"raw\":\"not imported\"}')",
            (),
        )
        .await
        .unwrap();
        reset_backfill(&conn, BACKFILL_BATCH_SIZE).await;
        resume_backfill(&conn).await.unwrap();
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM memory_v2_lineage_events
                 WHERE origin_source = 'memory_feedback_events'"
            )
            .await,
            1
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM memory_v2_lineage_events
                 WHERE origin_source = 'memory_oplog'"
            )
            .await,
            1
        );
        let mut rows = conn
            .query(
                "SELECT kind_json FROM memory_v2_lineage_events
                 WHERE origin_source = 'memory_oplog'",
                (),
            )
            .await
            .unwrap();
        assert!(
            !rows
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap()
                .contains("not imported")
        );
    }

    #[tokio::test]
    async fn redaction_and_quarantine_leave_no_raw_searchable_payload() {
        let (conn, _db, _dir) = database().await;
        conn.execute(
            "INSERT INTO memory_facts (
                fact_id, content, category, metadata, hrr_vector, created_at, updated_at
             ) VALUES (
                1, 'redact metadata', 'general',
                '{\"api_key\":\"raw-secret-canary\"}', x'010203', 10, 10
             )",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO memory_facts (
                fact_id, content, category, metadata, hrr_vector, created_at, updated_at
             ) VALUES (
                2, 'quarantine metadata', 'general',
                '{\"sk-test-123456\":\"raw-quarantine-canary\"}', x'040506', 10, 10
             )",
            (),
        )
        .await
        .unwrap();
        reset_backfill(&conn, BACKFILL_BATCH_SIZE).await;
        resume_backfill(&conn).await.unwrap();

        let mut rows = conn
            .query("SELECT metadata FROM memory_facts WHERE fact_id = 1", ())
            .await
            .unwrap();
        let metadata = rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap();
        assert!(!metadata.contains("raw-secret-canary"));
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM memory_facts WHERE fact_id = 2").await,
            0
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM memory_v2_facts WHERE payload_access = 'quarantined'"
            )
            .await,
            1
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM memory_v2_assertion_payloads p
                 JOIN memory_v2_assertions a ON a.assertion_id = p.assertion_id
                 JOIN memory_v2_legacy_map m ON m.fact_id = a.fact_id
                 WHERE m.legacy_fact_id = 2"
            )
            .await,
            0
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM memory_v2_assertion_payloads_fts
                 WHERE memory_v2_assertion_payloads_fts MATCH 'raw-secret-canary OR raw-quarantine-canary'"
            )
            .await,
            0
        );
    }

    #[tokio::test]
    async fn deletion_purges_payload_fts_and_vectors_but_keeps_tombstone() {
        let (conn, _db, _dir) = database().await;
        conn.execute(
            "INSERT INTO memory_facts (
                fact_id, content, category, hrr_vector, created_at, updated_at
             ) VALUES (9, 'purgeable canary', 'project', x'010203', 10, 10)",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO memory_banks (
                bank_name, vector, hrr_algebra, hrr_dim, fact_count, updated_at
             ) VALUES ('all', x'010203', 'amari_fhrr', 2048, 1, 10),
                      ('project', x'010203', 'amari_fhrr', 2048, 1, 10)",
            (),
        )
        .await
        .unwrap();
        reset_backfill(&conn, BACKFILL_BATCH_SIZE).await;
        resume_backfill(&conn).await.unwrap();
        assert!(purge_fact_payload(&conn, 9).await.unwrap());
        assert!(!purge_fact_payload(&conn, 9).await.unwrap());
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM memory_facts WHERE fact_id = 9").await,
            0
        );
        assert_eq!(scalar(&conn, "SELECT COUNT(*) FROM memory_banks").await, 0);
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM memory_v2_assertion_payloads p
                 JOIN memory_v2_assertions a ON a.assertion_id = p.assertion_id
                 JOIN memory_v2_legacy_map m ON m.fact_id = a.fact_id
                 WHERE m.legacy_fact_id = 9"
            )
            .await,
            0
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM memory_v2_assertion_payloads_fts
                 WHERE memory_v2_assertion_payloads_fts MATCH 'purgeable'"
            )
            .await,
            0
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM memory_facts_fts
                 WHERE memory_facts_fts MATCH 'purgeable'"
            )
            .await,
            0
        );
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM memory_v2_legacy_map").await,
            1
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM memory_v2_lineage_events
                 WHERE origin_source = 'memory_v2_delete'"
            )
            .await,
            1
        );
    }
}
