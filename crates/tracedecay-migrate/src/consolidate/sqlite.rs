//! Retained memory-merge SQL for branch cutover.
//!
//! Trimmed from the deleted profile-shard consolidation pipeline down to the
//! primitives [`crate::memory_cutover`] drives when a tracked branch store is
//! folded back into its project database.

use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::db::engine::{
    DatabaseAttachmentExecutor, Executor, QueryExecutor, params,
};
use tracedecay_runtime_core::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::memory::store::MemoryStore;

mod memory_v2;

pub use memory_v2::MemoryV2ArchiveMergeProof;
use memory_v2::merge_memory_v2_owner_archives;

pub(super) async fn registered_graph_maxima(database: &Database) -> Result<(i64, i64, i64, i64)> {
    let snapshot = database
        .begin_engine_read_snapshot("read consolidation graph maxima")
        .await?;
    Ok((
        table_max(&snapshot, "memory_facts", "fact_id").await?,
        table_max(&snapshot, "memory_entities", "entity_id").await?,
        table_max(&snapshot, "memory_feedback_events", "event_id").await?,
        table_max(&snapshot, "memory_oplog", "id").await?,
    ))
}
/// Merges one frozen branch store's complete memory authority into the
/// canonical project database. Source schemas v17 and v18 are accepted without
/// mutation. Legacy rows are unioned with remapped numeric identities; any
/// Memory V2 authority is then merged by its stable owner-bound identities,
/// including assertions, lineage, evidence, tombstones and feedback history.
pub async fn merge_branch_legacy_memory_snapshot(
    target: &Database,
    source: &tracedecay_runtime_core::sqlite_read_snapshot::SnapshotDatabase,
) -> Result<Vec<MemoryV2ArchiveMergeProof>> {
    let offsets = registered_graph_maxima(target).await?;
    let transaction = target
        .begin_memory_write_transaction("merge branch legacy memory")
        .await?;
    let token = source
        .attach_token()
        .map_err(|error| db_error("merge_branch_legacy_memory", error))?;
    let source_path = token
        .verified_path()
        .map_err(|error| db_error("attach_snapshot", error))?;
    transaction
        .attach_database(source_path, "source")
        .await
        .map_err(|error| db_error("attach_snapshot", error))?;
    transaction
        .execute("PRAGMA defer_foreign_keys = ON", ())
        .await
        .map_err(|error| db_error("merge_branch_legacy_memory", error))?;
    let result = async {
        if source_has_memory_v2_authority(&transaction).await? {
            verify_complete_branch_memory_v2_authority(&transaction).await?;
            merge_memory_v2_owner_archives(&transaction).await
        } else {
            merge_legacy_memory_tx(&transaction, offsets).await?;
            verify_legacy_fact_coverage(&transaction).await?;
            Ok(Vec::new())
        }
    }
    .await;
    let proofs = match result {
        Ok(proofs) => {
            transaction
                .commit()
                .await
                .map_err(|error| db_error("merge_branch_legacy_memory", error))?;
            proofs
        }
        Err(error) => {
            let _ = transaction.rollback().await;
            return Err(error);
        }
    };
    source
        .validate_source()
        .map_err(|error| db_error("merge_branch_legacy_memory", error))?;
    Ok(proofs)
}

pub async fn rebuild_branch_cutover_memory_banks(target: &Database) -> Result<()> {
    let transaction = target
        .begin_memory_write_transaction("rebuild branch-cutover memory banks")
        .await?;
    let result = MemoryStore::new_database_transaction(&transaction)
        .rebuild_all_banks()
        .await;
    match result {
        Ok(_) => transaction
            .commit()
            .await
            .map_err(|error| db_error("rebuild_branch_cutover_memory_banks", error)),
        Err(error) => {
            let _ = transaction.rollback().await;
            Err(error)
        }
    }
}

async fn verify_legacy_fact_coverage(conn: &impl Executor) -> Result<()> {
    let table_missing = query_i64(
        conn,
        "SELECT COUNT(*) FROM source.memory_facts AS source_fact NOT INDEXED
         WHERE NOT EXISTS(
             SELECT 1 FROM memory_facts AS target_fact
             WHERE target_fact.content = source_fact.content
         )",
    )
    .await?;
    let index_missing = query_i64(
        conn,
        "SELECT COUNT(*) FROM source.memory_facts AS source_fact
             INDEXED BY sqlite_autoindex_memory_facts_1
         WHERE NOT EXISTS(
             SELECT 1 FROM memory_facts AS target_fact
             WHERE target_fact.content = source_fact.content
         )",
    )
    .await?;
    if table_missing == 0 && index_missing == 0 {
        Ok(())
    } else {
        Err(db_message(
            "merge_branch_legacy_memory",
            format!(
                "branch memory coverage failed: {table_missing} table row(s) and \
                 {index_missing} content-index row(s) are absent from project memory"
            ),
        ))
    }
}
async fn source_has_memory_v2_authority(conn: &impl Executor) -> Result<bool> {
    if !table_exists(conn, "source", "memory_v2_facts").await? {
        return Ok(false);
    }
    Ok(query_i64(
        conn,
        "SELECT
             (SELECT COUNT(*) FROM source.memory_v2_facts)
           + (SELECT COUNT(*) FROM source.memory_v2_proposals)
           + (SELECT COUNT(*) FROM source.memory_v2_compatibility_operation_receipts)
           + (SELECT COUNT(*) FROM source.memory_v2_legacy_quarantine)",
    )
    .await?
        > 0)
}

async fn verify_complete_branch_memory_v2_authority(conn: &impl Executor) -> Result<()> {
    let unmapped_legacy_facts = query_i64(
        conn,
        "SELECT COUNT(*) FROM source.memory_facts AS legacy
         WHERE NOT EXISTS(
             SELECT 1
             FROM source.memory_v2_legacy_map AS mapping
             JOIN source.memory_v2_facts AS fact
               ON fact.fact_id=mapping.fact_id
              AND fact.owner_kind=mapping.owner_kind
              AND fact.project_id=mapping.project_id
             WHERE mapping.legacy_fact_id=legacy.fact_id
         )",
    )
    .await?;
    if unmapped_legacy_facts != 0 {
        return Err(db_message(
            "merge_branch_memory",
            format!(
                "branch has {unmapped_legacy_facts} legacy fact(s) outside its Memory V2 authority"
            ),
        ));
    }
    if table_exists(conn, "source", "memory_feedback_events").await?
        && table_exists(conn, "source", "memory_v2_legacy_feedback_event_map").await?
    {
        let unmapped_feedback = query_i64(
            conn,
            "SELECT COUNT(*) FROM source.memory_feedback_events AS legacy
             WHERE NOT EXISTS(
                 SELECT 1 FROM source.memory_v2_legacy_feedback_event_map AS mapping
                 WHERE mapping.legacy_feedback_event_id=legacy.event_id
             )",
        )
        .await?;
        if unmapped_feedback != 0 {
            return Err(db_message(
                "merge_branch_memory",
                format!(
                    "branch has {unmapped_feedback} legacy feedback event(s) outside its Memory V2 authority"
                ),
            ));
        }
    }
    Ok(())
}

async fn merge_legacy_memory_tx(conn: &impl Executor, offset: (i64, i64, i64, i64)) -> Result<()> {
    conn.execute_batch(&format!(
        "CREATE TEMP TABLE IF NOT EXISTS consolidation_fact_map(
             source_id INTEGER PRIMARY KEY, target_id INTEGER NOT NULL
         );
         DELETE FROM consolidation_fact_map;
         CREATE TEMP TABLE IF NOT EXISTS consolidation_entity_map(
             source_id INTEGER PRIMARY KEY, target_id INTEGER NOT NULL
         );
         DELETE FROM consolidation_entity_map;

         INSERT OR IGNORE INTO memory_facts (
             fact_id, content, category, tags, trust_score, retrieval_count,
             access_count, helpful_count, unhelpful_count, created_at, updated_at,
             last_retrieved_at, last_recalled_at, last_feedback_at, source,
             metadata, hrr_vector, hrr_algebra, hrr_dim, hrr_precision
         )
         SELECT fact_id + {fact}, content, category, tags, trust_score,
             retrieval_count, access_count, helpful_count, unhelpful_count,
             created_at, updated_at, last_retrieved_at, last_recalled_at,
             last_feedback_at, source, metadata, hrr_vector, hrr_algebra,
             hrr_dim, hrr_precision
         FROM source.memory_facts s
         WHERE NOT EXISTS (
             SELECT 1 FROM memory_facts t WHERE t.content = s.content
         );

         UPDATE memory_facts AS t SET
             tags = (SELECT json_group_array(value) FROM (
                 SELECT value FROM json_each(t.tags)
                 UNION
                 SELECT value FROM json_each((
                     SELECT s.tags FROM source.memory_facts s WHERE s.content = t.content
                 )) ORDER BY value
             )),
             category = CASE WHEN COALESCE((
                 SELECT s.updated_at FROM source.memory_facts s WHERE s.content = t.content
             ), -1) > t.updated_at THEN (
                 SELECT s.category FROM source.memory_facts s WHERE s.content = t.content
             ) ELSE t.category END,
             trust_score = CASE WHEN COALESCE((
                 SELECT s.last_feedback_at FROM source.memory_facts s WHERE s.content = t.content
             ), -1) > COALESCE(t.last_feedback_at, -1) THEN (
                 SELECT s.trust_score FROM source.memory_facts s WHERE s.content = t.content
             ) ELSE t.trust_score END,
             retrieval_count = MAX(t.retrieval_count, COALESCE((
                 SELECT s.retrieval_count FROM source.memory_facts s WHERE s.content = t.content
             ), 0)),
             access_count = MAX(t.access_count, COALESCE((
                 SELECT s.access_count FROM source.memory_facts s WHERE s.content = t.content
             ), 0)),
             helpful_count = MAX(t.helpful_count, COALESCE((
                 SELECT s.helpful_count FROM source.memory_facts s WHERE s.content = t.content
             ), 0)),
             unhelpful_count = MAX(t.unhelpful_count, COALESCE((
                 SELECT s.unhelpful_count FROM source.memory_facts s WHERE s.content = t.content
             ), 0)),
             created_at = MIN(t.created_at, COALESCE((
                 SELECT s.created_at FROM source.memory_facts s WHERE s.content = t.content
             ), t.created_at)),
             updated_at = MAX(t.updated_at, COALESCE((
                 SELECT s.updated_at FROM source.memory_facts s WHERE s.content = t.content
             ), t.updated_at)),
             last_retrieved_at = CASE
                 WHEN t.last_retrieved_at IS NULL THEN (SELECT s.last_retrieved_at FROM source.memory_facts s WHERE s.content = t.content)
                 WHEN (SELECT s.last_retrieved_at FROM source.memory_facts s WHERE s.content = t.content) IS NULL THEN t.last_retrieved_at
                 ELSE MAX(t.last_retrieved_at, (SELECT s.last_retrieved_at FROM source.memory_facts s WHERE s.content = t.content)) END,
             last_recalled_at = CASE
                 WHEN t.last_recalled_at IS NULL THEN (SELECT s.last_recalled_at FROM source.memory_facts s WHERE s.content = t.content)
                 WHEN (SELECT s.last_recalled_at FROM source.memory_facts s WHERE s.content = t.content) IS NULL THEN t.last_recalled_at
                 ELSE MAX(t.last_recalled_at, (SELECT s.last_recalled_at FROM source.memory_facts s WHERE s.content = t.content)) END,
             last_feedback_at = CASE
                 WHEN t.last_feedback_at IS NULL THEN (SELECT s.last_feedback_at FROM source.memory_facts s WHERE s.content = t.content)
                 WHEN (SELECT s.last_feedback_at FROM source.memory_facts s WHERE s.content = t.content) IS NULL THEN t.last_feedback_at
                 ELSE MAX(t.last_feedback_at, (SELECT s.last_feedback_at FROM source.memory_facts s WHERE s.content = t.content)) END,
             source = CASE WHEN COALESCE((
                 SELECT s.updated_at FROM source.memory_facts s WHERE s.content = t.content
             ), -1) > t.updated_at THEN (
                 SELECT s.source FROM source.memory_facts s WHERE s.content = t.content
             ) ELSE t.source END,
             metadata = CASE WHEN COALESCE((
                 SELECT s.updated_at FROM source.memory_facts s WHERE s.content = t.content
             ), -1) > t.updated_at THEN json_patch(
                 t.metadata,
                 COALESCE((SELECT s.metadata FROM source.memory_facts s
                           WHERE s.content = t.content), '{{}}')
             ) ELSE json_patch(
                 COALESCE((SELECT s.metadata FROM source.memory_facts s
                           WHERE s.content = t.content), '{{}}'),
                 t.metadata
             ) END,
             hrr_vector = CASE
                 WHEN (SELECT s.hrr_vector FROM source.memory_facts s
                       WHERE s.content = t.content) IS NOT NULL
                  AND (t.hrr_vector IS NULL OR COALESCE((
                      SELECT s.updated_at FROM source.memory_facts s
                      WHERE s.content = t.content
                  ), -1) > t.updated_at)
                 THEN (SELECT s.hrr_vector FROM source.memory_facts s
                       WHERE s.content = t.content)
                 ELSE t.hrr_vector END,
             hrr_algebra = CASE
                 WHEN (SELECT s.hrr_vector FROM source.memory_facts s
                       WHERE s.content = t.content) IS NOT NULL
                  AND (t.hrr_vector IS NULL OR COALESCE((
                      SELECT s.updated_at FROM source.memory_facts s
                      WHERE s.content = t.content
                  ), -1) > t.updated_at)
                 THEN (SELECT s.hrr_algebra FROM source.memory_facts s
                       WHERE s.content = t.content)
                 ELSE t.hrr_algebra END,
             hrr_dim = CASE
                 WHEN (SELECT s.hrr_vector FROM source.memory_facts s
                       WHERE s.content = t.content) IS NOT NULL
                  AND (t.hrr_vector IS NULL OR COALESCE((
                      SELECT s.updated_at FROM source.memory_facts s
                      WHERE s.content = t.content
                  ), -1) > t.updated_at)
                 THEN (SELECT s.hrr_dim FROM source.memory_facts s
                       WHERE s.content = t.content)
                 ELSE t.hrr_dim END,
             hrr_precision = CASE
                 WHEN (SELECT s.hrr_vector FROM source.memory_facts s
                       WHERE s.content = t.content) IS NOT NULL
                  AND (t.hrr_vector IS NULL OR COALESCE((
                      SELECT s.updated_at FROM source.memory_facts s
                      WHERE s.content = t.content
                  ), -1) > t.updated_at)
                 THEN (SELECT s.hrr_precision FROM source.memory_facts s
                       WHERE s.content = t.content)
                 ELSE t.hrr_precision END
         WHERE EXISTS (SELECT 1 FROM source.memory_facts s WHERE s.content = t.content);

         INSERT INTO consolidation_fact_map(source_id, target_id)
         SELECT s.fact_id, t.fact_id
         FROM source.memory_facts s JOIN memory_facts t ON t.content = s.content;

         INSERT OR IGNORE INTO memory_entities (
             entity_id, name, normalized_name, entity_type, aliases, created_at
         )
         SELECT entity_id + {entity}, name, normalized_name, entity_type, aliases, created_at
         FROM source.memory_entities s
         WHERE NOT EXISTS (
             SELECT 1 FROM memory_entities t WHERE t.normalized_name = s.normalized_name
         );

         UPDATE memory_entities AS t SET
             aliases = (SELECT json_group_array(value) FROM (
                 SELECT value FROM json_each(t.aliases)
                 UNION
                 SELECT value FROM json_each((
                     SELECT s.aliases FROM source.memory_entities s
                     WHERE s.normalized_name = t.normalized_name
                 )) ORDER BY value
             )),
             created_at = MIN(t.created_at, COALESCE((
                 SELECT s.created_at FROM source.memory_entities s
                 WHERE s.normalized_name = t.normalized_name
             ), t.created_at))
         WHERE EXISTS (
             SELECT 1 FROM source.memory_entities s
             WHERE s.normalized_name = t.normalized_name
         );

         INSERT INTO consolidation_entity_map(source_id, target_id)
         SELECT s.entity_id, t.entity_id
         FROM source.memory_entities s
         JOIN memory_entities t ON t.normalized_name = s.normalized_name;

         INSERT OR IGNORE INTO memory_fact_entities(fact_id, entity_id)
         SELECT fm.target_id, em.target_id
         FROM source.memory_fact_entities sfe
         JOIN consolidation_fact_map fm ON fm.source_id = sfe.fact_id
         JOIN consolidation_entity_map em ON em.source_id = sfe.entity_id;

         INSERT OR IGNORE INTO memory_feedback_events (
             event_id, fact_id, action, trust_delta, old_trust, new_trust,
             created_at, source, note
         )
         SELECT e.event_id + {feedback}, fm.target_id, e.action, e.trust_delta,
             e.old_trust, e.new_trust, e.created_at, e.source, e.note
         FROM source.memory_feedback_events e
         JOIN consolidation_fact_map fm ON fm.source_id = e.fact_id
         WHERE NOT EXISTS (
             SELECT 1 FROM memory_feedback_events t
             WHERE t.fact_id = fm.target_id
               AND t.action = e.action
               AND t.trust_delta = e.trust_delta
               AND t.old_trust = e.old_trust
               AND t.new_trust = e.new_trust
               AND t.created_at = e.created_at
               AND t.source = e.source
               AND t.note IS e.note
         );

         UPDATE memory_facts AS f SET
             helpful_count = MAX(f.helpful_count, (
                 SELECT COUNT(*) FROM memory_feedback_events e
                 WHERE e.fact_id=f.fact_id AND e.action='helpful'
             )),
             unhelpful_count = MAX(f.unhelpful_count, (
                 SELECT COUNT(*) FROM memory_feedback_events e
                 WHERE e.fact_id=f.fact_id AND e.action='unhelpful'
             ))
         WHERE f.fact_id IN (SELECT target_id FROM consolidation_fact_map);

         INSERT OR IGNORE INTO memory_oplog(id, ts, op, fact_id, detail_json)
         SELECT o.id + {oplog}, o.ts, o.op, fm.target_id, o.detail_json
         FROM source.memory_oplog o
         LEFT JOIN consolidation_fact_map fm ON fm.source_id = o.fact_id
         WHERE NOT EXISTS (
             SELECT 1 FROM memory_oplog AS target_op
             WHERE target_op.ts = o.ts
               AND target_op.op = o.op
               AND target_op.fact_id IS fm.target_id
               AND target_op.detail_json = o.detail_json
         );",
        fact = offset.0,
        entity = offset.1,
        feedback = offset.2,
        oplog = offset.3,
    ))
    .await
    .map_err(|error| db_error("merge_graph_facts", error))?;
    merge_legacy_fact_relations(conn).await?;
    Ok(())
}

async fn merge_legacy_fact_relations(conn: &impl Executor) -> Result<()> {
    if !table_exists(conn, "source", "memory_fact_relations").await? {
        return Ok(());
    }
    conn.execute_batch(
        "INSERT INTO memory_fact_relations (
             source_fact_id, target_fact_id, relation, confidence,
             source, metadata, created_at, updated_at
         )
         SELECT source_map.target_id, target_map.target_id, relation.relation,
                relation.confidence, relation.source, relation.metadata,
                relation.created_at, relation.updated_at
         FROM source.memory_fact_relations AS relation
         JOIN consolidation_fact_map AS source_map
           ON source_map.source_id = relation.source_fact_id
         JOIN consolidation_fact_map AS target_map
           ON target_map.source_id = relation.target_fact_id
         WHERE source_map.target_id != target_map.target_id
         ON CONFLICT(source_fact_id, target_fact_id, relation) DO UPDATE SET
             confidence = CASE
                 WHEN excluded.updated_at >= memory_fact_relations.updated_at
                 THEN excluded.confidence ELSE memory_fact_relations.confidence END,
             source = CASE
                 WHEN excluded.updated_at >= memory_fact_relations.updated_at
                 THEN excluded.source ELSE memory_fact_relations.source END,
             metadata = CASE
                 WHEN excluded.updated_at >= memory_fact_relations.updated_at
                 THEN excluded.metadata ELSE memory_fact_relations.metadata END,
             created_at = MIN(memory_fact_relations.created_at, excluded.created_at),
             updated_at = MAX(memory_fact_relations.updated_at, excluded.updated_at);",
    )
    .await
    .map_err(|error| db_error("merge_graph_fact_relations", error))
}
async fn table_max(conn: &impl QueryExecutor, table: &str, column: &str) -> Result<i64> {
    if !table_exists(conn, "main", table).await? {
        return Ok(0);
    }
    query_i64(
        conn,
        &format!(
            "SELECT COALESCE(MAX({}), 0) FROM {}",
            quote_identifier(column),
            quote_identifier(table)
        ),
    )
    .await
}
async fn query_i64(conn: &impl QueryExecutor, sql: &str) -> Result<i64> {
    let mut rows = conn
        .query(sql, ())
        .await
        .map_err(|error| db_error("query_i64", error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| db_error("query_i64", error))?
        .ok_or_else(|| db_message("query_i64", "query returned no row"))?;
    row.get::<i64>(0)
        .map_err(|error| db_error("query_i64", error))
}
fn db_error(operation: &str, error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        message: error.to_string(),
        operation: operation.to_string(),
    }
}

fn db_message(operation: &str, message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Database {
        message: message.into(),
        operation: operation.to_string(),
    }
}
async fn table_exists(conn: &impl QueryExecutor, schema: &str, table: &str) -> Result<bool> {
    let sql = format!(
        "SELECT COUNT(*) FROM {}.sqlite_schema WHERE type='table' AND name=?1",
        quote_identifier(schema)
    );
    let mut rows = conn
        .query(&sql, params![table])
        .await
        .map_err(|error| db_error("table_exists", error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| db_error("table_exists", error))?
        .ok_or_else(|| db_message("table_exists", "table probe returned no row"))?;
    Ok(row
        .get::<i64>(0)
        .map_err(|error| db_error("table_exists", error))?
        > 0)
}
fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}
