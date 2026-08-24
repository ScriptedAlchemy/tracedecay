use std::path::{Path, PathBuf};

use libsql::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::errors::{Result, TraceDecayError};
use crate::memory::store::MemoryStore;
use crate::registry_adapter::{RegistryDatabase, RegistryRuntime};

mod inspect;
mod verify;

pub use inspect::count_rows;
pub(super) use inspect::{
    GraphLogicalIdentities, acquire_offline_guards, count_rows_in, extend_graph_identities,
    inspect_collisions, quick_check_connection, quick_check_in,
};
pub(super) use verify::verify_session_union_sql;

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
    pub source_path: PathBuf,
    pub fact_id: i64,
    pub entity_id: i64,
    pub feedback_id: i64,
    pub oplog_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[doc(hidden)]
pub struct SessionMergeOffsets {
    pub raw: i64,
    pub span: i64,
    pub savings: i64,
    pub analytics: i64,
}

pub(super) async fn plan_graph_offsets(paths: &[PathBuf]) -> Result<Vec<GraphMergeOffsets>> {
    let (target_path, source_paths) = paths
        .split_first()
        .ok_or_else(|| db_message("plan_graph_offsets", "no graph databases were supplied"))?;
    normalize_graph(target_path).await?;
    for path in source_paths {
        normalize_graph(path).await?;
    }
    let mut maxima = graph_maxima(target_path).await?;
    let mut offsets = Vec::new();
    for path in source_paths {
        let source = graph_maxima(path).await?;
        let offset = GraphMergeOffsets {
            source_path: path.clone(),
            fact_id: maxima.0,
            entity_id: maxima.1,
            feedback_id: maxima.2,
            oplog_id: maxima.3,
        };
        maxima.0 = checked_advance(maxima.0, source.0, "fact_id")?;
        maxima.1 = checked_advance(maxima.1, source.1, "entity_id")?;
        maxima.2 = checked_advance(maxima.2, source.2, "feedback_id")?;
        maxima.3 = checked_advance(maxima.3, source.3, "oplog_id")?;
        offsets.push(offset);
    }
    Ok(offsets)
}

pub(super) async fn merge_graph_facts(
    paths: &[PathBuf],
    offsets: &[GraphMergeOffsets],
) -> Result<()> {
    let target_path = paths
        .first()
        .ok_or_else(|| db_message("merge_graph_facts", "no target graph database"))?;
    let authority = crate::db::DatabaseAuthority::for_runtime(target_path, "merge graph facts")?;
    let (target, _) = Database::open(target_path, &authority).await?;
    for offset in offsets {
        merge_one_graph(target.conn(), offset).await?;
    }
    MemoryStore::new(target.conn()).rebuild_all_banks().await?;
    target.checkpoint().await?;
    target.close();
    Ok(())
}

async fn normalize_graph(path: &Path) -> Result<()> {
    let authority = crate::db::DatabaseAuthority::for_runtime(path, "normalize graph")?;
    let (db, _) = Database::open(path, &authority).await?;
    db.checkpoint().await?;
    db.close();
    Ok(())
}

async fn graph_maxima(path: &Path) -> Result<(i64, i64, i64, i64)> {
    let authority = crate::db::DatabaseAuthority::for_runtime(path, "read graph maxima")?;
    let (db, _) = Database::open_read_only(path, &authority).await?;
    let result = (
        table_max(db.conn(), "memory_facts", "fact_id").await?,
        table_max(db.conn(), "memory_entities", "entity_id").await?,
        table_max(db.conn(), "memory_feedback_events", "event_id").await?,
        table_max(db.conn(), "memory_oplog", "id").await?,
    );
    db.close();
    Ok(result)
}

async fn merge_one_graph(conn: &Connection, offset: &GraphMergeOffsets) -> Result<()> {
    attach_as(conn, &offset.source_path, "source").await?;
    conn.execute("PRAGMA foreign_keys = OFF", ())
        .await
        .map_err(|error| db_error("merge_graph_facts", error))?;
    conn.execute("BEGIN IMMEDIATE", ())
        .await
        .map_err(|error| db_error("merge_graph_facts", error))?;
    let result = merge_one_graph_tx(conn, offset).await;
    match result {
        Ok(()) => conn
            .execute("COMMIT", ())
            .await
            .map_err(|error| db_error("merge_graph_facts", error))?,
        Err(error) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            let _ = conn.execute("DETACH DATABASE source", ()).await;
            return Err(error);
        }
    };
    conn.execute("DETACH DATABASE source", ())
        .await
        .map_err(|error| db_error("merge_graph_facts", error))?;
    Ok(())
}

async fn merge_one_graph_tx(conn: &Connection, offset: &GraphMergeOffsets) -> Result<()> {
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
             metadata = json_patch(COALESCE((
                 SELECT s.metadata FROM source.memory_facts s WHERE s.content = t.content
             ), '{{}}'), t.metadata)
         WHERE EXISTS (SELECT 1 FROM source.memory_facts s WHERE s.content = t.content);

         INSERT INTO consolidation_fact_map(source_id, target_id)
         SELECT s.fact_id, t.fact_id
         FROM source.memory_facts s JOIN memory_facts t ON t.content = s.content;

         INSERT INTO memory_fact_relations (
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
             updated_at = MAX(memory_fact_relations.updated_at, excluded.updated_at);

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
         LEFT JOIN consolidation_fact_map fm ON fm.source_id = o.fact_id;",
        fact = offset.fact_id,
        entity = offset.entity_id,
        feedback = offset.feedback_id,
        oplog = offset.oplog_id,
    ))
    .await
    .map_err(|error| db_error("merge_graph_facts", error))?;
    Ok(())
}

pub async fn plan_session_offsets<R: RegistryRuntime>(
    target: &Path,
    source: &Path,
    registry: &R,
) -> Result<SessionMergeOffsets> {
    normalize_sessions(target, registry).await?;
    normalize_sessions(source, registry).await?;
    reject_session_registry_rows(source, registry).await?;
    Ok(SessionMergeOffsets {
        raw: db_table_max(target, "lcm_raw_messages", "store_id", registry).await?,
        span: db_table_max(target, "session_git_spans", "span_id", registry).await?,
        savings: db_table_max(target, "savings_ledger", "id", registry).await?,
        analytics: db_table_max(target, "analytics_events", "id", registry).await?,
    })
}

pub(super) async fn merge_sessions<R: RegistryRuntime>(
    target_path: &Path,
    source_path: &Path,
    target_input_path: &Path,
    source_project_id: &str,
    offsets: &SessionMergeOffsets,
    registry: &R,
) -> Result<()> {
    normalize_sessions(target_path, registry).await?;
    normalize_sessions(source_path, registry).await?;
    let target = registry
        .open_at(target_path)
        .await
        .ok_or_else(|| db_message("merge_sessions", "could not open target sessions DB"))?;
    attach_as(target.conn(), source_path, "source").await?;
    attach_as(target.conn(), target_input_path, "target_input").await?;
    reject_session_content_collisions(target.conn(), "source", "target_input").await?;
    target
        .conn()
        .execute("PRAGMA foreign_keys = OFF", ())
        .await
        .map_err(|error| db_error("merge_sessions", error))?;
    target
        .conn()
        .execute("BEGIN IMMEDIATE", ())
        .await
        .map_err(|error| db_error("merge_sessions", error))?;
    let result = match build_consolidation_message_map(
        target.conn(),
        "source",
        "target_input",
        source_project_id,
    )
    .await
    {
        Ok(()) => merge_sessions_tx(target.conn(), offsets).await,
        Err(error) => Err(error),
    };
    match result {
        Ok(()) => target
            .conn()
            .execute("COMMIT", ())
            .await
            .map_err(|error| db_error("merge_sessions", error))?,
        Err(error) => {
            let _ = target.conn().execute("ROLLBACK", ()).await;
            let _ = target.conn().execute("DETACH DATABASE source", ()).await;
            let _ = target
                .conn()
                .execute("DETACH DATABASE target_input", ())
                .await;
            return Err(error);
        }
    };
    target
        .conn()
        .execute("DETACH DATABASE source", ())
        .await
        .map_err(|error| db_error("merge_sessions", error))?;
    target
        .conn()
        .execute("DETACH DATABASE target_input", ())
        .await
        .map_err(|error| db_error("merge_sessions", error))?;
    tracedecay_sessions::lcm::schema::rebuild_raw_fts(target.conn())
        .await
        .ok_or_else(|| db_message("merge_sessions", "could not rebuild raw-message FTS"))?;
    target.checkpoint().await;
    Ok(())
}

async fn normalize_sessions<R: RegistryRuntime>(path: &Path, registry: &R) -> Result<()> {
    let db = registry.open_at(path).await.ok_or_else(|| {
        db_message(
            "normalize_sessions",
            format!("could not open '{}'", path.display()),
        )
    })?;
    tracedecay_sessions::lcm::schema::ensure_lcm_schema(db.conn())
        .await
        .map_err(|error| db_error("normalize_sessions", error))?;
    tracedecay_sessions::git_correlation::ensure_git_correlation_schema(db.conn())
        .await
        .map_err(|error| db_error("normalize_sessions", error))?;
    tracedecay_sessions::workflow_index::ensure_workflow_index_schema(db.conn())
        .await
        .map_err(|error| db_error("normalize_sessions", error))?;
    db.conn()
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
    if !db.ensure_token_count_cache().await {
        return Err(db_message(
            "normalize_sessions",
            "could not ensure dashboard token-count schema",
        ));
    }
    db.checkpoint().await;
    Ok(())
}

async fn reject_session_registry_rows<R: RegistryRuntime>(path: &Path, registry: &R) -> Result<()> {
    let db = registry
        .open_read_only_at(path)
        .await
        .ok_or_else(|| db_message("merge_sessions", "could not inspect source sessions DB"))?;
    for table in [
        "code_projects",
        "project_aliases",
        "store_instances",
        "graph_scopes",
        "store_artifacts",
    ] {
        if table_exists(db.conn(), "main", table).await?
            && table_max_count(db.conn(), table).await? > 0
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
    conn: &Connection,
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

pub fn session_variant_family_cte() -> &'static str {
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

pub fn reserved_message_collision_sql() -> &'static str {
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

pub async fn build_consolidation_message_map(
    conn: &Connection,
    source_schema: &str,
    target_schema: &str,
    source_project_id: &str,
) -> Result<()> {
    conn.execute_batch(
        "PRAGMA query_only = OFF;
         CREATE TEMP TABLE IF NOT EXISTS consolidation_message_map(
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
             SELECT provider, message_id FROM {source}.dashboard_token_counts;
         INSERT OR IGNORE INTO consolidation_reserved_message_ids(provider, message_id)
             SELECT provider, message_id FROM {target}.dashboard_token_counts;
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

pub fn mapped_turn_message_id(alias: &str) -> String {
    format!(
        "COALESCE((
             SELECT m.mapped_id
             FROM consolidation_turn_message_map m
             WHERE m.original_id={alias}.message_id AND m.session_id={alias}.session_id
         ), {alias}.message_id)"
    )
}

async fn merge_sessions_tx(conn: &Connection, offsets: &SessionMergeOffsets) -> Result<()> {
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
             updated_at = MAX(t.updated_at, COALESCE((SELECT s.updated_at FROM source.session_backfill_meta s WHERE s.key=t.key), t.updated_at));

         INSERT OR IGNORE INTO dashboard_token_counts(
             store, provider, message_id, text_len, encoder, token_count, computed_at
         ) SELECT s.store, s.provider, COALESCE(m.mapped_id, s.message_id),
             s.text_len, s.encoder, s.token_count, s.computed_at
         FROM source.dashboard_token_counts s
         LEFT JOIN consolidation_message_map m
           ON m.provider=s.provider AND m.original_id=s.message_id;",
        raw = offsets.raw,
        span = offsets.span,
        savings = offsets.savings,
        analytics = offsets.analytics,
    ))
    .await
    .map_err(|error| db_error("merge_sessions", error))?;
    Ok(())
}

async fn attach(conn: &Connection, path: &Path) -> Result<()> {
    attach_as(conn, path, "other").await
}

async fn attach_as(conn: &Connection, path: &Path, alias: &str) -> Result<()> {
    let sql = format!("ATTACH DATABASE ?1 AS {}", quote_identifier(alias));
    conn.execute(&sql, params![path.to_string_lossy().to_string()])
        .await
        .map_err(|error| db_error("attach_database", error))?;
    Ok(())
}

async fn table_exists(conn: &Connection, schema: &str, table: &str) -> Result<bool> {
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

async fn table_max(conn: &Connection, table: &str, column: &str) -> Result<i64> {
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

async fn db_table_max<R: RegistryRuntime>(
    path: &Path,
    table: &str,
    column: &str,
    registry: &R,
) -> Result<i64> {
    let db = registry
        .open_read_only_at(path)
        .await
        .ok_or_else(|| db_message("table_max", format!("could not open '{}'", path.display())))?;
    let value = table_max(db.conn(), table, column).await?;
    Ok(value)
}

async fn table_max_count(conn: &Connection, table: &str) -> Result<i64> {
    query_i64(
        conn,
        &format!("SELECT COUNT(*) FROM {}", quote_identifier(table)),
    )
    .await
}

async fn query_i64(conn: &Connection, sql: &str) -> Result<i64> {
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
