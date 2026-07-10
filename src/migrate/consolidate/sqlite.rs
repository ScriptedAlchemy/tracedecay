use std::path::{Path, PathBuf};

use libsql::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::GlobalDb;
use crate::memory::store::MemoryStore;

mod inspect;
mod verify;

#[cfg(test)]
pub(super) use inspect::count_rows;
pub(super) use inspect::{
    GraphLogicalIdentities, acquire_offline_guards, count_rows_in, extend_graph_identities,
    inspect_collisions, quick_check_connection, quick_check_in,
};
pub(super) use verify::verify_session_union_sql;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct GraphMergeOffsets {
    pub source_path: PathBuf,
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
    let (target, _) = Database::open(target_path).await?;
    for offset in offsets {
        merge_one_graph(target.conn(), offset).await?;
    }
    MemoryStore::new(target.conn()).rebuild_all_banks().await?;
    target.checkpoint().await?;
    target.close();
    Ok(())
}

async fn normalize_graph(path: &Path) -> Result<()> {
    let (db, _) = Database::open(path).await?;
    db.checkpoint().await?;
    db.close();
    Ok(())
}

async fn graph_maxima(path: &Path) -> Result<(i64, i64, i64, i64)> {
    let (db, _) = Database::open_read_only(path).await?;
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

pub(super) async fn plan_session_offsets(
    target: &Path,
    source: &Path,
) -> Result<SessionMergeOffsets> {
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

pub(super) async fn merge_sessions(
    target_path: &Path,
    source_path: &Path,
    offsets: &SessionMergeOffsets,
) -> Result<()> {
    normalize_sessions(target_path).await?;
    normalize_sessions(source_path).await?;
    let target = GlobalDb::open_at(target_path)
        .await
        .ok_or_else(|| db_message("merge_sessions", "could not open target sessions DB"))?;
    attach_as(target.conn(), source_path, "source").await?;
    reject_session_content_collisions(target.conn()).await?;
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
    let result = merge_sessions_tx(target.conn(), offsets).await;
    match result {
        Ok(()) => target
            .conn()
            .execute("COMMIT", ())
            .await
            .map_err(|error| db_error("merge_sessions", error))?,
        Err(error) => {
            let _ = target.conn().execute("ROLLBACK", ()).await;
            let _ = target.conn().execute("DETACH DATABASE source", ()).await;
            return Err(error);
        }
    };
    target
        .conn()
        .execute("DETACH DATABASE source", ())
        .await
        .map_err(|error| db_error("merge_sessions", error))?;
    crate::sessions::lcm::schema::rebuild_raw_fts(target.conn())
        .await
        .ok_or_else(|| db_message("merge_sessions", "could not rebuild raw-message FTS"))?;
    target.checkpoint().await;
    target.close();
    Ok(())
}

async fn normalize_sessions(path: &Path) -> Result<()> {
    let db = GlobalDb::open_at(path).await.ok_or_else(|| {
        db_message(
            "normalize_sessions",
            format!("could not open '{}'", path.display()),
        )
    })?;
    crate::sessions::lcm::schema::ensure_lcm_schema(db.conn())
        .await
        .map_err(|error| db_error("normalize_sessions", error))?;
    crate::sessions::git_correlation::ensure_git_correlation_schema(db.conn())
        .await
        .map_err(|error| db_error("normalize_sessions", error))?;
    crate::sessions::workflow_index::ensure_workflow_index_schema(db.conn())
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
    db.close();
    Ok(())
}

async fn reject_session_registry_rows(path: &Path) -> Result<()> {
    let db = GlobalDb::open_read_only_at(path)
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
    db.close();
    Ok(())
}

async fn reject_session_content_collisions(conn: &Connection) -> Result<()> {
    for (label, sql) in [
        (
            "session message",
            "SELECT COUNT(*) FROM source.session_messages s
             JOIN session_messages t ON t.provider=s.provider AND t.message_id=s.message_id
             WHERE t.session_id IS NOT s.session_id OR t.role IS NOT s.role
                OR t.ordinal IS NOT s.ordinal OR t.text IS NOT s.text
                OR t.kind IS NOT s.kind OR t.model IS NOT s.model",
        ),
        (
            "LCM raw message",
            "SELECT COUNT(*) FROM source.lcm_raw_messages s
             JOIN lcm_raw_messages t ON t.provider=s.provider AND t.message_id=s.message_id
             WHERE t.session_id IS NOT s.session_id OR t.content_hash IS NOT s.content_hash
                OR t.storage_kind IS NOT s.storage_kind OR t.payload_ref IS NOT s.payload_ref",
        ),
        (
            "LCM external payload",
            "SELECT COUNT(*) FROM source.lcm_external_payloads s
             JOIN lcm_external_payloads t ON t.payload_ref=s.payload_ref
             WHERE t.content_hash IS NOT s.content_hash OR t.byte_count IS NOT s.byte_count",
        ),
        (
            "LCM summary node",
            "SELECT COUNT(*) FROM source.lcm_summary_nodes s
             JOIN lcm_summary_nodes t ON t.node_id=s.node_id
             WHERE t.summary_hash IS NOT s.summary_hash OR t.summary_text IS NOT s.summary_text",
        ),
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

async fn merge_sessions_tx(conn: &Connection, offsets: &SessionMergeOffsets) -> Result<()> {
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
         ) SELECT message_id, project_hash, session_id, model, timestamp, input_tokens,
             output_tokens, cache_write_tokens, cache_read_tokens, cost_usd,
             category, tool_names FROM source.turns;

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
         ) SELECT provider, message_id, session_id, role, timestamp, ordinal, text, kind,
             model, tool_names, source_path, source_offset, metadata_json
         FROM source.session_messages;

         INSERT OR IGNORE INTO session_schema_migrations(name, version, applied_at)
         SELECT name, version, applied_at FROM source.session_schema_migrations;
         UPDATE session_schema_migrations AS t SET
             version = MAX(t.version, COALESCE((SELECT s.version FROM source.session_schema_migrations s WHERE s.name=t.name), t.version)),
             applied_at = MAX(t.applied_at, COALESCE((SELECT s.applied_at FROM source.session_schema_migrations s WHERE s.name=t.name), t.applied_at));

         INSERT OR IGNORE INTO lcm_raw_messages(
             provider, message_id, session_id, store_id, role, ordinal, timestamp,
             content, content_hash, storage_kind, payload_ref, snippet_text, index_text,
             legacy_source, legacy_truncated, metadata_json
         ) SELECT provider, message_id, session_id, store_id + {raw}, role, ordinal,
             timestamp, content, content_hash, storage_kind, payload_ref, snippet_text,
             index_text, legacy_source, legacy_truncated, metadata_json
         FROM source.lcm_raw_messages;
         INSERT INTO consolidation_raw_map(source_id, target_id)
         SELECT s.store_id, t.store_id FROM source.lcm_raw_messages s
         JOIN lcm_raw_messages t ON t.provider=s.provider AND t.message_id=s.message_id;

         INSERT OR IGNORE INTO lcm_external_payloads(
             payload_ref, provider, session_id, message_id, kind, content_hash,
             byte_count, char_count, created_at, metadata_json
         ) SELECT payload_ref, provider, session_id, message_id, kind, content_hash,
             byte_count, char_count, created_at, metadata_json FROM source.lcm_external_payloads;
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
         ) SELECT commit_sha, provider, session_id, branch, worktree, committed_at,
             span_overlap_kind, CASE WHEN span_id IS NULL THEN NULL ELSE span_id + {span} END,
             relation, evidence, confidence, evidence_message_id, created_at
         FROM source.commit_sessions;
         UPDATE commit_sessions AS t SET
             branch = (SELECT s.branch FROM source.commit_sessions s WHERE s.commit_sha=t.commit_sha AND s.provider=t.provider AND s.session_id=t.session_id),
             worktree = (SELECT s.worktree FROM source.commit_sessions s WHERE s.commit_sha=t.commit_sha AND s.provider=t.provider AND s.session_id=t.session_id),
             committed_at = (SELECT s.committed_at FROM source.commit_sessions s WHERE s.commit_sha=t.commit_sha AND s.provider=t.provider AND s.session_id=t.session_id),
             span_overlap_kind = (SELECT s.span_overlap_kind FROM source.commit_sessions s WHERE s.commit_sha=t.commit_sha AND s.provider=t.provider AND s.session_id=t.session_id),
             span_id = (SELECT CASE WHEN s.span_id IS NULL THEN NULL ELSE s.span_id + {span} END FROM source.commit_sessions s WHERE s.commit_sha=t.commit_sha AND s.provider=t.provider AND s.session_id=t.session_id),
             relation = (SELECT s.relation FROM source.commit_sessions s WHERE s.commit_sha=t.commit_sha AND s.provider=t.provider AND s.session_id=t.session_id),
             evidence = (SELECT s.evidence FROM source.commit_sessions s WHERE s.commit_sha=t.commit_sha AND s.provider=t.provider AND s.session_id=t.session_id),
             confidence = (SELECT s.confidence FROM source.commit_sessions s WHERE s.commit_sha=t.commit_sha AND s.provider=t.provider AND s.session_id=t.session_id),
             evidence_message_id = (SELECT s.evidence_message_id FROM source.commit_sessions s WHERE s.commit_sha=t.commit_sha AND s.provider=t.provider AND s.session_id=t.session_id)
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
         ) SELECT store, provider, message_id, text_len, encoder, token_count, computed_at
         FROM source.dashboard_token_counts;",
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

async fn db_table_max(path: &Path, table: &str, column: &str) -> Result<i64> {
    let db = GlobalDb::open_read_only_at(path)
        .await
        .ok_or_else(|| db_message("table_max", format!("could not open '{}'", path.display())))?;
    let value = table_max(db.conn(), table, column).await?;
    db.close();
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
