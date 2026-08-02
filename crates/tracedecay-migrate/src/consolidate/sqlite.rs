#[cfg(test)]
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::runtime::{ConsolidationArtifactAuthorityV1, ConsolidationAttachTokenV1};
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::db::engine::{
    DatabaseAttachmentExecutor, Executor, QueryExecutor, params,
};
use tracedecay_runtime_core::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::memory::store::MemoryStore;

mod external_source;
mod inspect;
mod memory_v2;
mod observation;
pub(super) mod projection;
mod temporal;
mod verify;

pub use memory_v2::MemoryV2ArchiveMergeProof;
use memory_v2::{LegacyMappingPolicy, merge_memory_v2_authority, merge_memory_v2_owner_archives};
use observation::merge_observation_authority;
pub(super) use observation::{preflight_observation_merge, verify_observation_merge};

#[cfg(test)]
pub(super) use inspect::count_rows;
pub(super) use inspect::{
    GraphLogicalIdentities, count_rows_in, extend_graph_identities, inspect_collisions,
    quick_check_connection, quick_check_in,
};
pub(super) use verify::verify_session_union_sql;

#[cfg(test)]
pub(super) async fn verify_projection_plan_for_test(
    database: &crate::root_seam::global_db::RegisteredGlobalDb,
    source: &Path,
    target_input: &Path,
    source_project_id: &str,
) -> Result<()> {
    let conn = database.begin_write_transaction().await?;
    attach_as(&conn, source, "source_input").await?;
    attach_as(&conn, target_input, "target_input").await?;
    let result = match build_consolidation_message_map(
        &conn,
        "source_input",
        "target_input",
        source_project_id,
    )
    .await
    {
        Ok(()) => match projection::materialize(&conn, "target_input", "source_input").await {
            Ok(()) => projection::verify(&conn).await,
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    };
    let rollback = conn
        .rollback()
        .await
        .map_err(|error| db_error("verify_projection_plan_for_test", error));
    result.and(rollback)
}

#[cfg(test)]
pub(super) async fn merge_memory_v2_for_test(target_path: &Path, source: &Path) -> Result<()> {
    let target = open_migration_database(target_path, "merge memory_v2 test").await?;
    let transaction = target
        .begin_memory_write_transaction("merge memory_v2 test")
        .await?;
    attach_as(&transaction, source, "source").await?;
    transaction
        .execute("PRAGMA defer_foreign_keys = ON", ())
        .await
        .map_err(|error| db_error("merge_memory_v2_for_test", error))?;
    merge_memory_v2_owner_archives(&transaction).await?;
    transaction
        .commit()
        .await
        .map_err(|error| db_error("merge_memory_v2_for_test", error))?;
    target.checkpoint().await?;
    target.close();
    Ok(())
}

#[cfg(test)]
pub(super) fn set_forward_migrate_fault_after_import(enabled: bool) {
    temporal::set_forward_migrate_fault_after_import(enabled);
}

#[cfg(test)]
pub(super) fn set_temporal_merge_fault_phase(phase: &str) {
    temporal::set_temporal_merge_fault_phase(phase);
}

#[cfg(test)]
pub(super) async fn merge_temporal_for_test(target_path: &Path, source: &Path) -> Result<()> {
    normalize_sessions(target_path).await?;
    normalize_sessions(source).await?;
    let target = open_migration_database(target_path, "merge temporal test store").await?;
    let transaction = target
        .begin_write_transaction("merge temporal test store")
        .await
        .map_err(|error| db_error("merge_temporal_for_test", error))?;
    attach_as(&transaction, source, "source").await?;
    transaction
        .execute("PRAGMA defer_foreign_keys = ON", ())
        .await
        .map_err(|error| db_error("merge_temporal_for_test", error))?;
    temporal::preflight(&transaction).await?;
    temporal::merge(&transaction).await?;
    transaction
        .commit()
        .await
        .map_err(|error| db_error("merge_temporal_for_test", error))?;
    target.checkpoint().await?;
    target.close();
    Ok(())
}

pub(super) const LCM_RAW_MESSAGE_DIVERGENCE_PREDICATE: &str =
    "t.session_id IS NOT s.session_id OR t.content_hash IS NOT s.content_hash
     OR t.storage_kind IS NOT s.storage_kind OR t.payload_ref IS NOT s.payload_ref";
pub(super) const LCM_CONTENT_HASH_DIVERGENCE_PREDICATE: &str =
    "t.content_hash IS NOT s.content_hash";
const SESSION_MESSAGE_DIVERGENCE_PREDICATE: &str =
    "t.session_id IS NOT s.session_id OR t.role IS NOT s.role
     OR t.timestamp IS NOT s.timestamp OR t.ordinal IS NOT s.ordinal
     OR t.text IS NOT s.text OR t.kind IS NOT s.kind OR t.model IS NOT s.model
     OR t.tool_names IS NOT s.tool_names OR t.source_path IS NOT s.source_path
     OR t.source_offset IS NOT s.source_offset OR t.metadata_json IS NOT s.metadata_json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct GraphMergeOffsets {
    pub source_authority: ConsolidationArtifactAuthorityV1,
    pub fact_id: i64,
    pub entity_id: i64,
    pub feedback_id: i64,
    pub oplog_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SessionMergeOffsets {
    pub raw: i64,
    pub span: i64,
    pub savings: i64,
    pub analytics: i64,
}

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

pub(super) fn graph_offsets(
    source_authority: ConsolidationArtifactAuthorityV1,
    target_maxima: (i64, i64, i64, i64),
) -> GraphMergeOffsets {
    GraphMergeOffsets {
        source_authority,
        fact_id: target_maxima.0,
        entity_id: target_maxima.1,
        feedback_id: target_maxima.2,
        oplog_id: target_maxima.3,
    }
}

pub(super) fn advance_graph_maxima(
    maxima: &mut (i64, i64, i64, i64),
    source: (i64, i64, i64, i64),
) -> Result<()> {
    maxima.0 = checked_advance(maxima.0, source.0, "fact_id")?;
    maxima.1 = checked_advance(maxima.1, source.1, "entity_id")?;
    maxima.2 = checked_advance(maxima.2, source.2, "feedback_id")?;
    maxima.3 = checked_advance(maxima.3, source.3, "oplog_id")?;
    Ok(())
}

pub(super) async fn merge_registered_graph_facts(
    target: &Database,
    sources: Vec<(&GraphMergeOffsets, ConsolidationAttachTokenV1)>,
) -> Result<()> {
    for (offset, token) in sources {
        let transaction = target
            .begin_memory_write_transaction("merge graph facts")
            .await?;
        attach_token_as(&transaction, token, "source").await?;
        transaction
            .execute("PRAGMA defer_foreign_keys = ON", ())
            .await
            .map_err(|error| db_error("merge_graph_facts", error))?;
        if let Err(error) = merge_one_graph_tx(&transaction, offset).await {
            let _ = transaction.rollback().await;
            return Err(error);
        }
        transaction
            .commit()
            .await
            .map_err(|error| db_error("merge_graph_facts", error))?;
    }
    let transaction = target
        .begin_memory_write_transaction("rebuild merged memory banks")
        .await?;
    MemoryStore::new_database_transaction(&transaction)
        .rebuild_all_banks()
        .await?;
    transaction.commit().await
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

async fn merge_one_graph_tx(conn: &impl Executor, offset: &GraphMergeOffsets) -> Result<()> {
    external_source::merge(conn, "main", "source").await?;
    merge_legacy_memory_tx(
        conn,
        (
            offset.fact_id,
            offset.entity_id,
            offset.feedback_id,
            offset.oplog_id,
        ),
    )
    .await?;
    merge_memory_v2_authority(conn, LegacyMappingPolicy::PreserveSourceRows).await
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

#[cfg(test)]
pub(super) async fn plan_session_offsets(
    target: &Path,
    source: &Path,
) -> Result<SessionMergeOffsets> {
    reject_future_lcm_schema_path(target, "target sessions").await?;
    reject_future_lcm_schema_path(source, "source sessions").await?;
    normalize_sessions(target).await?;
    normalize_sessions(source).await?;
    reject_session_registry_rows(source).await?;
    Ok(SessionMergeOffsets {
        raw: db_table_max(target, "lcm_raw_messages", "store_id").await?,
        span: db_table_max(target, "session_git_spans", "span_id").await?,
        savings: db_table_max(target, "savings_ledger", "id").await?,
        analytics: db_table_max(target, "analytics_events", "id").await?,
    })
}

pub(super) async fn registered_session_offsets(target: &Database) -> Result<SessionMergeOffsets> {
    let target = target
        .begin_engine_read_snapshot("plan consolidation session offsets")
        .await?;
    Ok(SessionMergeOffsets {
        raw: table_max(&target, "lcm_raw_messages", "store_id").await?,
        span: table_max(&target, "session_git_spans", "span_id").await?,
        savings: table_max(&target, "savings_ledger", "id").await?,
        analytics: table_max(&target, "analytics_events", "id").await?,
    })
}

pub(super) async fn validate_registered_session_source(source: &Database) -> Result<()> {
    reject_session_registry_rows_database(source).await
}

#[cfg(test)]
pub(super) async fn merge_sessions(
    target_path: &Path,
    source_path: &Path,
    target_input_path: &Path,
    source_project_id: &str,
    offsets: &SessionMergeOffsets,
) -> Result<()> {
    reject_future_lcm_schema_path(target_path, "target sessions").await?;
    reject_future_lcm_schema_path(source_path, "source sessions").await?;
    reject_future_lcm_schema_path(target_input_path, "target input sessions").await?;
    normalize_sessions(target_path).await?;
    normalize_sessions(source_path).await?;
    let target = open_migration_database(target_path, "merge consolidated session store").await?;
    let transaction = target
        .begin_write_transaction("merge consolidated session store")
        .await
        .map_err(|error| db_error("merge_sessions", error))?;
    attach_as(&transaction, source_path, "source").await?;
    attach_as(&transaction, target_input_path, "target_input").await?;
    reject_future_lcm_schema(&transaction, "main", "target sessions").await?;
    reject_future_lcm_schema(&transaction, "source", "source sessions").await?;
    reject_future_lcm_schema(&transaction, "target_input", "target input sessions").await?;
    transaction
        .execute("PRAGMA defer_foreign_keys = ON", ())
        .await
        .map_err(|error| db_error("merge_sessions", error))?;
    external_source::merge(&transaction, "main", "source").await?;
    reject_session_content_collisions(&transaction, "source", "target_input").await?;
    build_consolidation_message_map(&transaction, "source", "target_input", source_project_id)
        .await?;
    projection::materialize(&transaction, "target_input", "source").await?;
    temporal::preflight(&transaction).await?;
    preflight_observation_merge(&transaction).await?;
    merge_sessions_tx(&transaction, offsets).await?;
    verify_observation_merge(&transaction).await?;
    crate::root_seam::sessions::lcm::schema::rebuild_raw_fts(&transaction)
        .await
        .ok_or_else(|| db_message("merge_sessions", "could not rebuild raw-message FTS"))?;
    transaction
        .commit()
        .await
        .map_err(|error| db_error("merge_sessions", error))?;
    target.checkpoint().await?;
    target.close();
    Ok(())
}

pub(super) async fn merge_registered_sessions(
    target: &Database,
    source: ConsolidationAttachTokenV1,
    target_input: ConsolidationAttachTokenV1,
    source_project_id: &str,
    offsets: &SessionMergeOffsets,
) -> Result<()> {
    let transaction = target
        .begin_write_transaction("merge consolidated session store")
        .await
        .map_err(|error| db_error("merge_sessions", error))?;
    attach_token_as(&transaction, source, "source").await?;
    attach_token_as(&transaction, target_input, "target_input").await?;
    reject_future_lcm_schema(&transaction, "main", "target sessions").await?;
    reject_future_lcm_schema(&transaction, "source", "source sessions").await?;
    reject_future_lcm_schema(&transaction, "target_input", "target input sessions").await?;
    transaction
        .execute("PRAGMA defer_foreign_keys = ON", ())
        .await
        .map_err(|error| db_error("merge_sessions", error))?;
    external_source::merge(&transaction, "main", "source").await?;
    reject_session_content_collisions(&transaction, "source", "target_input").await?;
    build_consolidation_message_map(&transaction, "source", "target_input", source_project_id)
        .await?;
    projection::materialize(&transaction, "target_input", "source").await?;
    temporal::preflight(&transaction).await?;
    preflight_observation_merge(&transaction).await?;
    merge_sessions_tx(&transaction, offsets).await?;
    verify_observation_merge(&transaction).await?;
    crate::root_seam::sessions::lcm::schema::rebuild_raw_fts(&transaction)
        .await
        .ok_or_else(|| db_message("merge_sessions", "could not rebuild raw-message FTS"))?;
    transaction
        .commit()
        .await
        .map_err(|error| db_error("merge_sessions", error))
}

#[cfg(test)]
async fn normalize_sessions(path: &Path) -> Result<()> {
    let db = open_migration_database(path, "normalize consolidated session store").await?;
    normalize_registered_sessions(&db).await?;
    db.checkpoint().await?;
    db.close();
    Ok(())
}

pub(super) async fn normalize_registered_sessions(db: &Database) -> Result<()> {
    let transaction = db
        .begin_write_transaction("normalize consolidated session store")
        .await
        .map_err(|error| db_error("normalize_sessions", error))?;
    reject_future_lcm_schema(&transaction, "main", "sessions database").await?;
    crate::root_seam::sessions::lcm::schema::ensure_lcm_schema_in_transaction(&transaction)
        .await
        .map_err(|error| db_error("normalize_sessions", error))?;
    crate::root_seam::sessions::git_correlation::ensure_git_correlation_schema_in_transaction(
        &transaction,
    )
    .await
    .map_err(|error| db_error("normalize_sessions", error))?;
    crate::root_seam::sessions::workflow_index::ensure_workflow_index_schema(&transaction)
        .await
        .map_err(|error| db_error("normalize_sessions", error))?;
    transaction
        .execute(
            "CREATE TABLE IF NOT EXISTS session_backfill_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at INTEGER NOT NULL DEFAULT (unixepoch())
            )",
            (),
        )
        .await
        .map_err(|error| db_error("normalize_sessions", error))?;
    transaction
        .commit()
        .await
        .map_err(|error| db_error("normalize_sessions", error))?;
    Ok(())
}

#[cfg(test)]
async fn reject_future_lcm_schema_path(path: &Path, role: &str) -> Result<()> {
    let scratch_root = path.parent().unwrap_or_else(|| Path::new("."));
    let database = tracedecay_runtime_core::sqlite_read_snapshot::open_in(path, scratch_root)
        .await
        .map_err(|error| db_error("validate_lcm_schema", error))?;
    reject_future_lcm_schema(database.connection(), "main", role).await
}

async fn reject_future_lcm_schema(
    conn: &impl QueryExecutor,
    schema: &str,
    role: &str,
) -> Result<()> {
    if !table_exists(conn, schema, "session_schema_migrations").await? {
        return Ok(());
    }
    let sql = format!(
        "SELECT version FROM {}.session_schema_migrations WHERE name='lcm'",
        quote_identifier(schema)
    );
    let mut rows = conn
        .query(&sql, ())
        .await
        .map_err(|error| db_error("validate_lcm_schema", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error("validate_lcm_schema", error))?
    else {
        return Ok(());
    };
    let version = row
        .get::<i64>(0)
        .map_err(|error| db_error("validate_lcm_schema", error))?;
    if version > crate::root_seam::sessions::lcm::LCM_SCHEMA_VERSION {
        return Err(db_message(
            "validate_lcm_schema",
            format!(
                "{role} uses newer LCM schema version {version}; supported maximum is {}",
                crate::root_seam::sessions::lcm::LCM_SCHEMA_VERSION
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
async fn reject_session_registry_rows(path: &Path) -> Result<()> {
    let scratch_root = path.parent().unwrap_or_else(|| Path::new("."));
    let db = tracedecay_runtime_core::sqlite_read_snapshot::open_in(path, scratch_root)
        .await
        .map_err(|error| db_error("merge_sessions", error))?;
    let snapshot = db.connection();
    for table in [
        "code_projects",
        "project_aliases",
        "store_instances",
        "graph_scopes",
        "store_artifacts",
    ] {
        if table_exists(snapshot, "main", table).await?
            && table_max_count(snapshot, table).await? > 0
        {
            return Err(db_message(
                "merge_sessions",
                format!("source sessions DB unexpectedly contains registry rows in {table}"),
            ));
        }
    }
    Ok(())
}

async fn reject_session_registry_rows_database(database: &Database) -> Result<()> {
    let snapshot = database
        .begin_engine_read_snapshot("inspect consolidation source sessions")
        .await?;
    for table in [
        "code_projects",
        "project_aliases",
        "store_instances",
        "graph_scopes",
        "store_artifacts",
    ] {
        if table_exists(&snapshot, "main", table).await?
            && table_max_count(&snapshot, table).await? > 0
        {
            return Err(db_message(
                "merge_sessions",
                format!("source sessions DB unexpectedly contains registry rows in {table}"),
            ));
        }
    }
    Ok(())
}

async fn reject_session_content_collisions(
    conn: &impl Executor,
    source_schema: &str,
    target_schema: &str,
) -> Result<()> {
    let source = quote_identifier(source_schema);
    let target = quote_identifier(target_schema);
    let external_payloads = format!(
        "SELECT COUNT(*) FROM {source}.lcm_external_payloads s
         JOIN {target}.lcm_external_payloads t ON t.payload_ref=s.payload_ref
         WHERE t.content_hash IS NOT s.content_hash OR t.byte_count IS NOT s.byte_count"
    );
    let summary_nodes = format!(
        "SELECT COUNT(*) FROM {source}.lcm_summary_nodes s
         JOIN {target}.lcm_summary_nodes t ON t.node_id=s.node_id
         WHERE t.summary_hash IS NOT s.summary_hash OR t.summary_text IS NOT s.summary_text"
    );
    for (label, sql) in [
        ("LCM external payload", external_payloads.as_str()),
        ("LCM summary node", summary_nodes.as_str()),
    ] {
        let count = query_i64(conn, sql).await?;
        if count > 0 {
            return Err(db_message(
                "merge_sessions",
                format!(
                    "{count} divergent {label} collision(s); inputs and backups were preserved"
                ),
            ));
        }
    }
    Ok(())
}

pub(super) fn session_variant_family_cte() -> &'static str {
    "WITH RECURSIVE variant_family(provider, message_id) AS (
         SELECT provider, original_id FROM consolidation_message_map
         UNION
         SELECT edge.provider, edge.child_id
         FROM variant_family parent
         JOIN consolidation_parent_edges edge
           ON edge.provider=parent.provider
          AND edge.parent_id=parent.message_id
     )"
}

pub(super) fn reserved_message_collision_sql() -> &'static str {
    "SELECT COUNT(*) FROM consolidation_message_map m
     WHERE EXISTS (
         SELECT 1 FROM consolidation_reserved_message_ids r
         WHERE r.message_id=m.mapped_id
           AND r.provider IN ('', m.provider)
     )"
}

fn scalar_parent_rows_sql(schema: &str, table: &str, include_child: bool) -> String {
    let child_projection = if include_child {
        ", message_id AS child_id"
    } else {
        ""
    };
    format!(
        "SELECT provider, CAST(parent_id AS TEXT) AS parent_id{child_projection}
         FROM (
             SELECT provider, message_id,
                    CASE WHEN json_valid(metadata_json)
                         THEN json_extract(metadata_json, '$.parent_message_id') END
                        AS parent_id,
                    CASE WHEN json_valid(metadata_json)
                         THEN json_type(metadata_json, '$.parent_message_id') END
                        AS parent_type
             FROM {schema}.{table}
         )
         WHERE parent_type IN ('text', 'integer', 'real', 'true', 'false')"
    )
}

pub(super) async fn build_consolidation_message_map(
    conn: &impl Executor,
    source_schema: &str,
    target_schema: &str,
    source_project_id: &str,
) -> Result<()> {
    conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS consolidation_message_map(
             provider TEXT NOT NULL,
             original_id TEXT NOT NULL,
             mapped_id TEXT NOT NULL,
             session_divergent INTEGER NOT NULL,
             raw_content_divergent INTEGER NOT NULL,
             PRIMARY KEY(provider, original_id),
             UNIQUE(provider, mapped_id)
         );
         CREATE TEMP TABLE IF NOT EXISTS consolidation_parent_edges(
             provider TEXT NOT NULL,
             parent_id TEXT NOT NULL,
             child_id TEXT NOT NULL,
             PRIMARY KEY(provider, parent_id, child_id)
         ) WITHOUT ROWID;
         CREATE TEMP TABLE IF NOT EXISTS consolidation_reserved_message_ids(
             provider TEXT NOT NULL,
             message_id TEXT NOT NULL,
             PRIMARY KEY(message_id, provider)
         ) WITHOUT ROWID;
         CREATE TEMP TABLE IF NOT EXISTS consolidation_turn_message_map(
             session_id TEXT NOT NULL,
             original_id TEXT NOT NULL,
             mapped_id TEXT NOT NULL,
             owner_count INTEGER NOT NULL,
             PRIMARY KEY(session_id, original_id)
         ) WITHOUT ROWID;",
    )
    .await
    .map_err(|error| db_error("message_variant_map_init", error))?;
    conn.execute("DELETE FROM temp.consolidation_message_map", ())
        .await
        .map_err(|error| db_error("message_variant_map_reset", error))?;
    conn.execute_batch(
        "DELETE FROM temp.consolidation_parent_edges;
         DELETE FROM temp.consolidation_reserved_message_ids;
         DELETE FROM temp.consolidation_turn_message_map;",
    )
    .await
    .map_err(|error| db_error("message_lookup_tables_reset", error))?;
    let source = quote_identifier(source_schema);
    let target = quote_identifier(target_schema);
    let sql = format!(
        "INSERT INTO consolidation_message_map(
             provider, original_id, mapped_id, session_divergent, raw_content_divergent
         )
         SELECT provider, message_id, 'consolidated/' || ?1 || '/' || message_id,
                MAX(session_divergent), MAX(raw_content_divergent)
         FROM (
             SELECT s.provider, s.message_id, 1 AS session_divergent,
                    0 AS raw_content_divergent
             FROM {source}.session_messages s
             JOIN {target}.session_messages t
               ON t.provider=s.provider AND t.message_id=s.message_id
             WHERE {SESSION_MESSAGE_DIVERGENCE_PREDICATE}
             UNION
             SELECT s.provider, s.message_id, 0 AS session_divergent,
                    1 AS raw_content_divergent
             FROM {source}.lcm_raw_messages s
             JOIN {target}.lcm_raw_messages t
               ON t.provider=s.provider AND t.message_id=s.message_id
             WHERE {LCM_CONTENT_HASH_DIVERGENCE_PREDICATE}
         ) GROUP BY provider, message_id"
    );
    conn.execute(&sql, params![source_project_id])
        .await
        .map_err(|error| db_error("message_variant_map_fill", error))?;
    let source_session_parents = scalar_parent_rows_sql(&source, "session_messages", true);
    let parent_edges_sql = format!(
        "INSERT OR IGNORE INTO consolidation_parent_edges(provider, parent_id, child_id)
         SELECT child.provider, child.parent_id, child.child_id
         FROM ({source_session_parents}) child
         JOIN {target}.session_messages target_child
           ON target_child.provider=child.provider
          AND target_child.message_id=child.child_id"
    );
    conn.execute(&parent_edges_sql, ())
        .await
        .map_err(|error| db_error("message_parent_edges_fill", error))?;
    let session_family_sql = format!(
        "{}
         INSERT OR IGNORE INTO consolidation_message_map(
             provider, original_id, mapped_id, session_divergent, raw_content_divergent
         )
         SELECT provider, message_id, 'consolidated/' || ?1 || '/' || message_id, 1, 0
         FROM variant_family WHERE 1
         ON CONFLICT(provider, original_id) DO UPDATE SET
             session_divergent=MAX(session_divergent, excluded.session_divergent),
             raw_content_divergent=MAX(raw_content_divergent, excluded.raw_content_divergent)",
        session_variant_family_cte()
    );
    conn.execute(&session_family_sql, params![source_project_id])
        .await
        .map_err(|error| db_error("message_session_variant_family", error))?;
    let target_session_parents = scalar_parent_rows_sql(&target, "session_messages", false);
    let source_raw_parents = scalar_parent_rows_sql(&source, "lcm_raw_messages", false);
    let target_raw_parents = scalar_parent_rows_sql(&target, "lcm_raw_messages", false);
    let reserved_references_sql = format!(
        "INSERT OR IGNORE INTO consolidation_reserved_message_ids(provider, message_id)
             SELECT provider, message_id FROM {source}.session_messages;
         INSERT OR IGNORE INTO consolidation_reserved_message_ids(provider, message_id)
             SELECT provider, message_id FROM {target}.session_messages;
         INSERT OR IGNORE INTO consolidation_reserved_message_ids(provider, message_id)
             SELECT provider, message_id FROM {source}.lcm_raw_messages;
         INSERT OR IGNORE INTO consolidation_reserved_message_ids(provider, message_id)
             SELECT provider, message_id FROM {target}.lcm_raw_messages;
         INSERT OR IGNORE INTO consolidation_reserved_message_ids(provider, message_id)
             SELECT provider, message_id FROM {source}.lcm_external_payloads;
         INSERT OR IGNORE INTO consolidation_reserved_message_ids(provider, message_id)
             SELECT provider, message_id FROM {target}.lcm_external_payloads;
         INSERT OR IGNORE INTO consolidation_reserved_message_ids(provider, message_id)
             SELECT provider, evidence_message_id FROM {source}.commit_sessions
             WHERE evidence_message_id IS NOT NULL;
         INSERT OR IGNORE INTO consolidation_reserved_message_ids(provider, message_id)
             SELECT provider, evidence_message_id FROM {target}.commit_sessions
             WHERE evidence_message_id IS NOT NULL;
         INSERT OR IGNORE INTO consolidation_reserved_message_ids(provider, message_id)
             SELECT provider, parent_id FROM ({source_session_parents});
         INSERT OR IGNORE INTO consolidation_reserved_message_ids(provider, message_id)
             SELECT provider, parent_id FROM ({target_session_parents});
         INSERT OR IGNORE INTO consolidation_reserved_message_ids(provider, message_id)
             SELECT provider, parent_id FROM ({source_raw_parents});
         INSERT OR IGNORE INTO consolidation_reserved_message_ids(provider, message_id)
             SELECT provider, parent_id FROM ({target_raw_parents});
         INSERT OR IGNORE INTO consolidation_reserved_message_ids(provider, message_id)
             SELECT '', message_id FROM {source}.turns;
         INSERT OR IGNORE INTO consolidation_reserved_message_ids(provider, message_id)
             SELECT '', message_id FROM {target}.turns;"
    );
    conn.execute_batch(&reserved_references_sql)
        .await
        .map_err(|error| db_error("message_reserved_references_fill", error))?;
    let collisions = query_i64(conn, reserved_message_collision_sql()).await?;
    if collisions != 0 {
        return Err(db_message(
            "message_variant_map",
            format!(
                "{collisions} synthetic consolidation message key collision(s); inputs and backups were preserved"
            ),
        ));
    }
    let turn_message_map_sql = format!(
        "INSERT INTO consolidation_turn_message_map(
             session_id, original_id, mapped_id, owner_count
         )
         SELECT sm.session_id, sm.message_id, MIN(m.mapped_id), COUNT(*)
         FROM {source}.session_messages sm
         LEFT JOIN consolidation_message_map m
           ON m.provider=sm.provider AND m.original_id=sm.message_id
         GROUP BY sm.session_id, sm.message_id
         HAVING COUNT(m.mapped_id) > 0"
    );
    conn.execute(&turn_message_map_sql, ())
        .await
        .map_err(|error| db_error("turn_message_map_fill", error))?;
    let ambiguous_turns = query_i64(
        conn,
        &format!(
            "SELECT COUNT(*) FROM {source}.turns tr
             JOIN consolidation_turn_message_map m
               ON m.session_id=tr.session_id AND m.original_id=tr.message_id
             WHERE m.owner_count != 1"
        ),
    )
    .await?;
    if ambiguous_turns != 0 {
        return Err(db_message(
            "message_variant_map",
            format!(
                "{ambiguous_turns} source turn message mapping ambiguity collision(s); inputs and backups were preserved"
            ),
        ));
    }
    Ok(())
}

pub(super) fn mapped_parent_metadata(alias: &str, raw_family_only: bool) -> String {
    let family_filter = if raw_family_only {
        " AND parent_map.raw_content_divergent=1"
    } else {
        ""
    };
    format!(
        "CASE
             WHEN NOT json_valid({alias}.metadata_json)
             THEN {alias}.metadata_json
             ELSE COALESCE((
                 SELECT json_set(
                     {alias}.metadata_json,
                     '$.consolidation_original_parent_message_id',
                     json_extract({alias}.metadata_json, '$.parent_message_id'),
                     '$.parent_message_id', parent_map.mapped_id
                 )
                 FROM consolidation_message_map parent_map
                 WHERE parent_map.provider={alias}.provider
                   AND parent_map.original_id=json_extract(
                       {alias}.metadata_json, '$.parent_message_id'
                   ){family_filter}
             ), {alias}.metadata_json)
         END"
    )
}

pub(super) fn mapped_turn_message_id(alias: &str) -> String {
    format!(
        "COALESCE((
             SELECT m.mapped_id
             FROM consolidation_turn_message_map m
             WHERE m.original_id={alias}.message_id AND m.session_id={alias}.session_id
         ), {alias}.message_id)"
    )
}

async fn merge_sessions_tx(conn: &impl Executor, offsets: &SessionMergeOffsets) -> Result<()> {
    let session_metadata = mapped_parent_metadata("s", false);
    let raw_metadata = mapped_parent_metadata("s", true);
    let turn_message_id = mapped_turn_message_id("s");
    conn.execute_batch(&format!(
        "CREATE TEMP TABLE IF NOT EXISTS consolidation_raw_map(
             source_id INTEGER PRIMARY KEY, target_id INTEGER NOT NULL
         );
         DELETE FROM consolidation_raw_map;

         INSERT OR IGNORE INTO projects(path, tokens_saved)
         SELECT path, tokens_saved FROM source.projects;
         UPDATE projects AS t SET tokens_saved = MAX(t.tokens_saved, COALESCE((
             SELECT s.tokens_saved FROM source.projects s WHERE s.path=t.path
         ), t.tokens_saved));

         INSERT OR IGNORE INTO turns(
             message_id, project_hash, session_id, model, timestamp, input_tokens,
             output_tokens, cache_write_tokens, cache_read_tokens, cost_usd,
             category, tool_names
         ) SELECT {turn_message_id}, s.project_hash, s.session_id, s.model, s.timestamp,
             s.input_tokens, s.output_tokens, s.cache_write_tokens, s.cache_read_tokens,
             s.cost_usd, s.category, s.tool_names FROM source.turns s;

         INSERT OR IGNORE INTO parse_offsets(file_path, byte_offset, mtime, file_id)
         SELECT file_path, byte_offset, mtime, file_id FROM source.parse_offsets;
         UPDATE parse_offsets AS t SET
             byte_offset = CASE WHEN COALESCE((SELECT s.mtime FROM source.parse_offsets s WHERE s.file_path=t.file_path), -1) > t.mtime
                 THEN (SELECT s.byte_offset FROM source.parse_offsets s WHERE s.file_path=t.file_path) ELSE t.byte_offset END,
             file_id = CASE WHEN COALESCE((SELECT s.mtime FROM source.parse_offsets s WHERE s.file_path=t.file_path), -1) > t.mtime
                 THEN (SELECT s.file_id FROM source.parse_offsets s WHERE s.file_path=t.file_path) ELSE t.file_id END,
             mtime = MAX(t.mtime, COALESCE((SELECT s.mtime FROM source.parse_offsets s WHERE s.file_path=t.file_path), t.mtime));

         INSERT OR IGNORE INTO savings_ledger(id, ts, project_path, tool_name, before_tokens, after_tokens)
         SELECT id + {savings}, ts, project_path, tool_name, before_tokens, after_tokens
         FROM source.savings_ledger;
         INSERT OR IGNORE INTO analytics_events(
             id, provider, project_id, session_id, timestamp, event_kind, hook_name,
             tool_name, tool_category, skill_name, hint_category, hint_id, outcome, metadata_json
         ) SELECT id + {analytics}, provider, project_id, session_id, timestamp,
             event_kind, hook_name, tool_name, tool_category, skill_name, hint_category,
             hint_id, outcome, metadata_json FROM source.analytics_events;

         INSERT OR IGNORE INTO sessions(
             provider, session_id, project_key, project_path, title, started_at,
             ended_at, transcript_path, metadata_json, parent_session_id,
             is_subagent, agent_id, parent_tool_use_id
         ) SELECT provider, session_id, project_key, project_path, title, started_at,
             ended_at, transcript_path, metadata_json, parent_session_id,
             is_subagent, agent_id, parent_tool_use_id FROM source.sessions;
         UPDATE sessions AS t SET
             started_at = CASE
                 WHEN t.started_at IS NULL THEN (SELECT s.started_at FROM source.sessions s WHERE s.provider=t.provider AND s.session_id=t.session_id)
                 WHEN (SELECT s.started_at FROM source.sessions s WHERE s.provider=t.provider AND s.session_id=t.session_id) IS NULL THEN t.started_at
                 ELSE MIN(t.started_at, (SELECT s.started_at FROM source.sessions s WHERE s.provider=t.provider AND s.session_id=t.session_id)) END,
             ended_at = CASE
                 WHEN t.ended_at IS NULL THEN (SELECT s.ended_at FROM source.sessions s WHERE s.provider=t.provider AND s.session_id=t.session_id)
                 WHEN (SELECT s.ended_at FROM source.sessions s WHERE s.provider=t.provider AND s.session_id=t.session_id) IS NULL THEN t.ended_at
                 ELSE MAX(t.ended_at, (SELECT s.ended_at FROM source.sessions s WHERE s.provider=t.provider AND s.session_id=t.session_id)) END,
             title = COALESCE(t.title, (SELECT s.title FROM source.sessions s WHERE s.provider=t.provider AND s.session_id=t.session_id)),
             transcript_path = COALESCE(t.transcript_path, (SELECT s.transcript_path FROM source.sessions s WHERE s.provider=t.provider AND s.session_id=t.session_id)),
             metadata_json = COALESCE(t.metadata_json, (SELECT s.metadata_json FROM source.sessions s WHERE s.provider=t.provider AND s.session_id=t.session_id)),
             parent_session_id = COALESCE(t.parent_session_id, (SELECT s.parent_session_id FROM source.sessions s WHERE s.provider=t.provider AND s.session_id=t.session_id)),
             is_subagent = MAX(t.is_subagent, COALESCE((SELECT s.is_subagent FROM source.sessions s WHERE s.provider=t.provider AND s.session_id=t.session_id), 0)),
             agent_id = COALESCE(t.agent_id, (SELECT s.agent_id FROM source.sessions s WHERE s.provider=t.provider AND s.session_id=t.session_id)),
             parent_tool_use_id = COALESCE(t.parent_tool_use_id, (SELECT s.parent_tool_use_id FROM source.sessions s WHERE s.provider=t.provider AND s.session_id=t.session_id))
         WHERE EXISTS (SELECT 1 FROM source.sessions s WHERE s.provider=t.provider AND s.session_id=t.session_id);

         INSERT OR IGNORE INTO session_messages(
             provider, message_id, session_id, role, timestamp, ordinal, text, kind,
             model, tool_names, source_path, source_offset, metadata_json
         ) SELECT s.provider, COALESCE(m.mapped_id, s.message_id), s.session_id, s.role,
             s.timestamp, s.ordinal, s.text, s.kind, s.model, s.tool_names,
             s.source_path, s.source_offset, {session_metadata}
         FROM source.session_messages s
         LEFT JOIN consolidation_message_map m
           ON m.provider=s.provider AND m.original_id=s.message_id;

         INSERT OR IGNORE INTO session_schema_migrations(name, version, applied_at)
         SELECT name, version, applied_at FROM source.session_schema_migrations;
         UPDATE session_schema_migrations AS t SET
             version = MAX(t.version, COALESCE((SELECT s.version FROM source.session_schema_migrations s WHERE s.name=t.name), t.version)),
             applied_at = MAX(t.applied_at, COALESCE((SELECT s.applied_at FROM source.session_schema_migrations s WHERE s.name=t.name), t.applied_at));

         INSERT OR IGNORE INTO lcm_raw_messages(
             provider, message_id, session_id, store_id, role, ordinal, timestamp,
             content, content_hash, storage_kind, payload_ref, snippet_text, index_text,
             legacy_source, legacy_truncated, metadata_json
         ) SELECT s.provider,
             CASE WHEN m.raw_content_divergent=1 THEN m.mapped_id ELSE s.message_id END,
             s.session_id,
             s.store_id + {raw}, s.role, s.ordinal, s.timestamp, s.content,
             s.content_hash, s.storage_kind, s.payload_ref, s.snippet_text,
             s.index_text, s.legacy_source, s.legacy_truncated, {raw_metadata}
         FROM source.lcm_raw_messages s
         LEFT JOIN consolidation_message_map m
           ON m.provider=s.provider AND m.original_id=s.message_id;
         INSERT INTO consolidation_raw_map(source_id, target_id)
         SELECT s.store_id, t.store_id FROM source.lcm_raw_messages s
         LEFT JOIN consolidation_message_map m
           ON m.provider=s.provider AND m.original_id=s.message_id
         JOIN lcm_raw_messages t
           ON t.provider=s.provider
          AND t.message_id=CASE WHEN m.raw_content_divergent=1
                                THEN m.mapped_id ELSE s.message_id END;

         INSERT OR IGNORE INTO lcm_external_payloads(
             payload_ref, provider, session_id, message_id, kind, content_hash,
             byte_count, char_count, created_at, metadata_json
         ) SELECT s.payload_ref, s.provider, s.session_id,
             CASE WHEN m.raw_content_divergent=1 THEN m.mapped_id ELSE s.message_id END,
             s.kind, s.content_hash,
             s.byte_count, s.char_count, s.created_at, s.metadata_json
         FROM source.lcm_external_payloads s
         LEFT JOIN consolidation_message_map m
           ON m.provider=s.provider AND m.original_id=s.message_id
         WHERE NOT EXISTS (
             SELECT 1 FROM target_input.lcm_external_payloads t
             WHERE t.payload_ref=s.payload_ref
         );
         INSERT OR IGNORE INTO lcm_gc_marks(payload_ref, state, first_seen_at, updated_at)
         SELECT payload_ref, state, first_seen_at, updated_at FROM source.lcm_gc_marks;
         INSERT OR IGNORE INTO lcm_gc_meta(key, value) SELECT key, value FROM source.lcm_gc_meta;
         INSERT OR IGNORE INTO lcm_summary_nodes(
             node_id, provider, conversation_id, session_id, depth, summary_text,
             summary_hash, summary_token_count, source_token_count, source_time_start,
             source_time_end, expand_hint, metadata_json, created_at
         ) SELECT node_id, provider, conversation_id, session_id, depth, summary_text,
             summary_hash, summary_token_count, source_token_count, source_time_start,
             source_time_end, expand_hint, metadata_json, created_at FROM source.lcm_summary_nodes;
         INSERT OR IGNORE INTO lcm_summary_sources(node_id, source_kind, source_id, ordinal)
         SELECT s.node_id, s.source_kind,
             CASE WHEN s.source_kind='raw_message' THEN CAST((
                 SELECT target_id FROM consolidation_raw_map
                 WHERE source_id=CAST(s.source_id AS INTEGER)
             ) AS TEXT) ELSE s.source_id END,
             s.ordinal
         FROM source.lcm_summary_sources s;

         INSERT OR REPLACE INTO lcm_lifecycle_state(
             provider, conversation_id, current_session_id, last_finalized_session_id,
             current_frontier_store_id, last_finalized_frontier_store_id, rollover_at,
             reset_at, maintenance_at, boundary_skip_at, updated_at
         ) SELECT s.provider, s.conversation_id, s.current_session_id,
             s.last_finalized_session_id,
             (SELECT target_id FROM consolidation_raw_map WHERE source_id=s.current_frontier_store_id),
             (SELECT target_id FROM consolidation_raw_map WHERE source_id=s.last_finalized_frontier_store_id),
             s.rollover_at, s.reset_at, s.maintenance_at, s.boundary_skip_at, s.updated_at
         FROM source.lcm_lifecycle_state s
         WHERE NOT EXISTS (
             SELECT 1 FROM lcm_lifecycle_state t
             WHERE t.provider=s.provider AND t.conversation_id=s.conversation_id
               AND t.updated_at >= s.updated_at
         );
         INSERT OR IGNORE INTO lcm_maintenance_debt(
             provider, conversation_id, debt_id, debt_kind, from_store_id,
             to_store_id, metadata_json, created_at
         ) SELECT s.provider, s.conversation_id, s.debt_id, s.debt_kind,
             (SELECT target_id FROM consolidation_raw_map WHERE source_id=s.from_store_id),
             (SELECT target_id FROM consolidation_raw_map WHERE source_id=s.to_store_id),
             s.metadata_json, s.created_at FROM source.lcm_maintenance_debt s;

         INSERT OR IGNORE INTO workflow_runs(
             run_id, parent_session_id, name, description, phase_json, status,
             started_ts, ended_ts, result_summary, agent_count, created_at, updated_at
         ) SELECT run_id, parent_session_id, name, description, phase_json, status,
             started_ts, ended_ts, result_summary, agent_count, created_at, updated_at
         FROM source.workflow_runs;
         INSERT OR REPLACE INTO workflow_runs(
             run_id, parent_session_id, name, description, phase_json, status,
             started_ts, ended_ts, result_summary, agent_count, created_at, updated_at
         ) SELECT s.run_id, s.parent_session_id, s.name, s.description, s.phase_json,
             s.status, s.started_ts, s.ended_ts, s.result_summary, s.agent_count,
             s.created_at, s.updated_at FROM source.workflow_runs s
         WHERE EXISTS (SELECT 1 FROM workflow_runs t WHERE t.run_id=s.run_id AND t.updated_at < s.updated_at);
         INSERT OR IGNORE INTO workflow_agents(
             run_id, agent_label, agent_id, phase, transcript_path, agent_session_id,
             status, model, tokens, started_ts, ended_ts, created_at, updated_at
         ) SELECT run_id, agent_label, agent_id, phase, transcript_path, agent_session_id,
             status, model, tokens, started_ts, ended_ts, created_at, updated_at
         FROM source.workflow_agents;
         INSERT OR REPLACE INTO workflow_agents(
             run_id, agent_label, agent_id, phase, transcript_path, agent_session_id,
             status, model, tokens, started_ts, ended_ts, created_at, updated_at
         ) SELECT s.run_id, s.agent_label, s.agent_id, s.phase, s.transcript_path,
             s.agent_session_id, s.status, s.model, s.tokens, s.started_ts, s.ended_ts,
             s.created_at, s.updated_at FROM source.workflow_agents s
         WHERE EXISTS (
             SELECT 1 FROM workflow_agents t
             WHERE t.run_id=s.run_id AND t.agent_label=s.agent_label AND t.agent_id=s.agent_id
               AND t.updated_at < s.updated_at
         );
         INSERT OR IGNORE INTO workflow_index_meta(key, value, updated_at)
         SELECT key, value, updated_at FROM source.workflow_index_meta;
         UPDATE workflow_index_meta AS t SET
             value = MAX(t.value, COALESCE((SELECT s.value FROM source.workflow_index_meta s WHERE s.key=t.key), t.value)),
             updated_at = MAX(t.updated_at, COALESCE((SELECT s.updated_at FROM source.workflow_index_meta s WHERE s.key=t.key), t.updated_at));

         INSERT OR IGNORE INTO session_git_spans(
             span_id, provider, session_id, thread_id, branch, worktree, first_ts,
             last_ts, event_count, source, created_at, updated_at
         ) SELECT span_id + {span}, provider, session_id, thread_id, branch, worktree,
             first_ts, last_ts, event_count, source, created_at, updated_at
         FROM source.session_git_spans;
         INSERT OR IGNORE INTO commit_sessions(
             commit_sha, provider, session_id, branch, worktree, committed_at,
             span_overlap_kind, span_id, relation, evidence, confidence,
             evidence_message_id, created_at
         ) SELECT cs.commit_sha, cs.provider, cs.session_id, cs.branch, cs.worktree,
             cs.committed_at, cs.span_overlap_kind,
             CASE WHEN cs.span_id IS NULL THEN NULL ELSE cs.span_id + {span} END,
             cs.relation, cs.evidence, cs.confidence,
             COALESCE((SELECT mapped_id FROM consolidation_message_map m
                       WHERE m.provider=cs.provider
                         AND m.original_id=cs.evidence_message_id),
                      cs.evidence_message_id),
             cs.created_at
         FROM source.commit_sessions cs;
         UPDATE commit_sessions AS t SET
             branch = (SELECT s.branch FROM source.commit_sessions s WHERE s.commit_sha=t.commit_sha AND s.provider=t.provider AND s.session_id=t.session_id),
             worktree = (SELECT s.worktree FROM source.commit_sessions s WHERE s.commit_sha=t.commit_sha AND s.provider=t.provider AND s.session_id=t.session_id),
             committed_at = (SELECT s.committed_at FROM source.commit_sessions s WHERE s.commit_sha=t.commit_sha AND s.provider=t.provider AND s.session_id=t.session_id),
             span_overlap_kind = (SELECT s.span_overlap_kind FROM source.commit_sessions s WHERE s.commit_sha=t.commit_sha AND s.provider=t.provider AND s.session_id=t.session_id),
             span_id = (SELECT CASE WHEN s.span_id IS NULL THEN NULL ELSE s.span_id + {span} END FROM source.commit_sessions s WHERE s.commit_sha=t.commit_sha AND s.provider=t.provider AND s.session_id=t.session_id),
             relation = (SELECT s.relation FROM source.commit_sessions s WHERE s.commit_sha=t.commit_sha AND s.provider=t.provider AND s.session_id=t.session_id),
             evidence = (SELECT s.evidence FROM source.commit_sessions s WHERE s.commit_sha=t.commit_sha AND s.provider=t.provider AND s.session_id=t.session_id),
             confidence = (SELECT s.confidence FROM source.commit_sessions s WHERE s.commit_sha=t.commit_sha AND s.provider=t.provider AND s.session_id=t.session_id),
             evidence_message_id = (SELECT COALESCE(
                 (SELECT mapped_id FROM consolidation_message_map m
                  WHERE m.provider=s.provider AND m.original_id=s.evidence_message_id),
                 s.evidence_message_id)
                 FROM source.commit_sessions s
                 WHERE s.commit_sha=t.commit_sha AND s.provider=t.provider AND s.session_id=t.session_id)
         WHERE EXISTS (
             SELECT 1 FROM source.commit_sessions s
             WHERE s.commit_sha=t.commit_sha AND s.provider=t.provider AND s.session_id=t.session_id
               AND s.confidence > t.confidence
         );
         INSERT OR IGNORE INTO git_correlation_meta(key, value, updated_at)
         SELECT key, value, updated_at FROM source.git_correlation_meta;
         UPDATE git_correlation_meta AS t SET
             value = MAX(t.value, COALESCE((SELECT s.value FROM source.git_correlation_meta s WHERE s.key=t.key), t.value)),
             updated_at = MAX(t.updated_at, COALESCE((SELECT s.updated_at FROM source.git_correlation_meta s WHERE s.key=t.key), t.updated_at));

         INSERT OR IGNORE INTO session_backfill_meta(key, value, updated_at)
         SELECT key, value, updated_at FROM source.session_backfill_meta;
         UPDATE session_backfill_meta AS t SET
             value = MAX(t.value, COALESCE((SELECT s.value FROM source.session_backfill_meta s WHERE s.key=t.key), t.value)),
             updated_at = MAX(t.updated_at, COALESCE((SELECT s.updated_at FROM source.session_backfill_meta s WHERE s.key=t.key), t.updated_at));",
        raw = offsets.raw,
        span = offsets.span,
        savings = offsets.savings,
        analytics = offsets.analytics,
    ))
    .await
    .map_err(|error| db_error("merge_sessions", error))?;
    merge_observation_authority(conn).await?;
    temporal::merge(conn).await?;
    Ok(())
}

#[cfg(test)]
async fn attach_as(conn: &impl DatabaseAttachmentExecutor, path: &Path, alias: &str) -> Result<()> {
    conn.attach_database(path, alias)
        .await
        .map_err(|error| db_error("attach_database", error))?;
    Ok(())
}

async fn attach_token_as(
    conn: &impl DatabaseAttachmentExecutor,
    token: ConsolidationAttachTokenV1,
    alias: &str,
) -> Result<()> {
    let path = token.into_verified_path()?;
    conn.attach_database(&path, alias)
        .await
        .map_err(|error| db_error("attach_database", error))?;
    Ok(())
}

pub(super) async fn attach_snapshot_as(
    conn: &impl Executor,
    token: &tracedecay_runtime_core::sqlite_read_snapshot::SnapshotAttachToken<'_>,
    alias: &str,
) -> Result<()> {
    let path = token
        .verified_path()
        .map_err(|error| db_error("attach_snapshot", error))?;
    let filename = path.to_str().ok_or_else(|| {
        db_message(
            "attach_snapshot",
            "verified SQLite snapshot path is not valid UTF-8",
        )
    })?;
    let sql = format!("ATTACH DATABASE ?1 AS {}", quote_identifier(alias));
    conn.execute(&sql, params![filename])
        .await
        .map_err(|error| db_error("attach_snapshot", error))?;
    Ok(())
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

#[cfg(test)]
async fn db_table_max(path: &Path, table: &str, column: &str) -> Result<i64> {
    let scratch_root = path.parent().unwrap_or_else(|| Path::new("."));
    let db = tracedecay_runtime_core::sqlite_read_snapshot::open_in(path, scratch_root)
        .await
        .map_err(|error| db_error("table_max", error))?;
    table_max(db.connection(), table, column).await
}

#[cfg(test)]
async fn open_migration_database(path: &Path, operation: &'static str) -> Result<Database> {
    // The kernel initialises the profile sidecar shard through a fail-closed
    // port whose real installer lives in `tracedecay-global-db`. Idempotent —
    // the port keeps the first registration.
    tracedecay_global_db::register_test_schema_installer();
    let authority = tracedecay_runtime_core::db::DatabaseAuthority::for_runtime(path, operation)?;
    match Database::open(path, &authority).await {
        Ok((database, _)) => Ok(database),
        #[cfg(test)]
        Err(_) if authority.role() == tracedecay_runtime_core::db::DatabaseAuthorityRole::Test => {
            Database::publish_test_runtime(
                path,
                &authority,
                tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Existing,
            )
            .await
            .map(|(database, _)| database)
        }
        Err(error) => Err(error),
    }
}

async fn table_max_count(conn: &impl QueryExecutor, table: &str) -> Result<i64> {
    query_i64(
        conn,
        &format!("SELECT COUNT(*) FROM {}", quote_identifier(table)),
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

fn checked_advance(base: i64, source_max: i64, label: &str) -> Result<i64> {
    base.checked_add(source_max)
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| db_message("plan_offsets", format!("{label} offset overflow")))
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
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
