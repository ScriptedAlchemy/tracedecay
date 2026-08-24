use std::path::Path;

use libsql::Connection;

use super::{
    SessionMergeOffsets, attach_as, build_consolidation_message_map, db_error, db_message,
    mapped_parent_metadata, mapped_turn_message_id, query_i64,
};
use crate::errors::Result;

struct TableVerification {
    label: &'static str,
    table: &'static str,
    columns: &'static str,
    expected: String,
}

pub(in crate::consolidate) async fn verify_session_union_sql(
    input_snapshots: &crate::sqlite_read_snapshot::SnapshotSet,
    source: &Path,
    target: &Path,
    destination_snapshots: &crate::sqlite_read_snapshot::SnapshotSet,
    destination_root: &Path,
    offsets: &SessionMergeOffsets,
    source_project_id: &str,
) -> Result<()> {
    let destination = destination_root.join(crate::storage::SESSIONS_DB_FILENAME);
    let conn = destination_snapshots.get(&destination).map_err(|error| {
        db_error(
            "verify_consolidation",
            format!("could not read destination snapshot: {error}"),
        )
    })?;
    let conn = conn.connection();
    conn.execute_batch("PRAGMA temp_store=FILE; PRAGMA cache_size=-32768;")
        .await
        .map_err(|error| db_error("verify_consolidation", error))?;
    attach_as(
        conn,
        input_snapshots
            .get(source)
            .map_err(|error| db_error("verify_consolidation", error))?
            .path(),
        "source_input",
    )
    .await?;
    attach_as(
        conn,
        input_snapshots
            .get(target)
            .map_err(|error| db_error("verify_consolidation", error))?
            .path(),
        "target_input",
    )
    .await?;
    build_consolidation_message_map(conn, "source_input", "target_input", source_project_id)
        .await?;
    conn.execute_batch("PRAGMA query_only = ON;")
        .await
        .map_err(|error| db_error("verify_consolidation", error))?;

    let result = match verify_attached_tables(conn, offsets).await {
        Ok(()) => verify_payload_files(conn, destination_root).await,
        Err(error) => Err(error),
    };
    let _ = conn.execute("DETACH DATABASE source_input", ()).await;
    let _ = conn.execute("DETACH DATABASE target_input", ()).await;
    result
}

async fn verify_attached_tables(conn: &Connection, offsets: &SessionMergeOffsets) -> Result<()> {
    for spec in verification_specs(offsets) {
        verify_table(conn, &spec).await?;
    }
    for (label, backing, fts) in [
        (
            "session message FTS",
            "session_messages",
            "session_messages_fts",
        ),
        (
            "LCM raw-message FTS",
            "lcm_raw_messages",
            "lcm_raw_messages_fts",
        ),
        (
            "LCM summary-node FTS",
            "lcm_summary_nodes",
            "lcm_summary_nodes_fts",
        ),
    ] {
        let sql = format!(
            "SELECT
               (SELECT COUNT(*) FROM (SELECT rowid FROM {backing} EXCEPT SELECT rowid FROM {fts}))
             + (SELECT COUNT(*) FROM (SELECT rowid FROM {fts} EXCEPT SELECT rowid FROM {backing}))"
        );
        let differences = query_i64(conn, &sql).await?;
        if differences != 0 {
            return Err(db_message(
                "verify_consolidation",
                format!("destination {label} differs from its durable backing table"),
            ));
        }
    }
    Ok(())
}

async fn verify_table(conn: &Connection, spec: &TableVerification) -> Result<()> {
    let sql = format!(
        "WITH expected AS ({expected})
         SELECT
           (SELECT COUNT(*) FROM (
                SELECT * FROM expected
                EXCEPT SELECT {columns} FROM main.{table}
            ))
         + (SELECT COUNT(*) FROM (
                SELECT {columns} FROM main.{table}
                EXCEPT SELECT * FROM expected
            ))",
        expected = spec.expected,
        columns = spec.columns,
        table = spec.table,
    );
    let differences = query_i64(conn, &sql).await?;
    if differences != 0 {
        return Err(db_message(
            "verify_consolidation",
            format!(
                "destination {} logical union differs from frozen inputs: {differences} difference(s)",
                spec.label
            ),
        ));
    }
    Ok(())
}

fn verification_specs(offsets: &SessionMergeOffsets) -> Vec<TableVerification> {
    let turn_message_id = mapped_turn_message_id("s");
    let session_metadata = mapped_parent_metadata("s", false);
    let raw_metadata = mapped_parent_metadata("s", true);
    let mut specs = vec![
        custom(
            "project accounting",
            "projects",
            "path, tokens_saved",
            "SELECT path, MAX(tokens_saved) AS tokens_saved FROM (
                 SELECT path, tokens_saved FROM target_input.projects
                 UNION ALL SELECT path, tokens_saved FROM source_input.projects
             ) GROUP BY path",
        ),
        custom_owned(
            "turn",
            "turns",
            "message_id, project_hash, session_id, model, timestamp, input_tokens, output_tokens, cache_write_tokens, cache_read_tokens, cost_usd, category, tool_names",
            format!(
                "SELECT message_id, project_hash, session_id, model, timestamp,
                        input_tokens, output_tokens, cache_write_tokens,
                        cache_read_tokens, cost_usd, category, tool_names
                 FROM target_input.turns
                 UNION ALL
                 SELECT {turn_message_id}, s.project_hash, s.session_id, s.model,
                        s.timestamp, s.input_tokens, s.output_tokens,
                        s.cache_write_tokens, s.cache_read_tokens, s.cost_usd,
                        s.category, s.tool_names
                 FROM source_input.turns s
                 WHERE {turn_message_id} != s.message_id OR NOT EXISTS (
                     SELECT 1 FROM target_input.turns t
                     WHERE t.message_id=s.message_id
                 )"
            ),
        ),
        custom(
            "parse offset",
            "parse_offsets",
            "file_path, byte_offset, mtime, file_id",
            "SELECT t.file_path,
                    CASE WHEN s.mtime > t.mtime THEN s.byte_offset ELSE t.byte_offset END,
                    MAX(t.mtime, s.mtime),
                    CASE WHEN s.mtime > t.mtime THEN s.file_id ELSE t.file_id END
             FROM target_input.parse_offsets t
             JOIN source_input.parse_offsets s ON s.file_path=t.file_path
             UNION ALL
             SELECT t.file_path, t.byte_offset, t.mtime, t.file_id
             FROM target_input.parse_offsets t
             WHERE NOT EXISTS (SELECT 1 FROM source_input.parse_offsets s WHERE s.file_path=t.file_path)
             UNION ALL
             SELECT s.file_path, s.byte_offset, s.mtime, s.file_id
             FROM source_input.parse_offsets s
             WHERE NOT EXISTS (SELECT 1 FROM target_input.parse_offsets t WHERE t.file_path=s.file_path)",
        ),
        offset_append(
            "savings ledger",
            "savings_ledger",
            "id, ts, project_path, tool_name, before_tokens, after_tokens",
            &format!(
                "id + {}, ts, project_path, tool_name, before_tokens, after_tokens",
                offsets.savings
            ),
        ),
        offset_append(
            "analytics event",
            "analytics_events",
            "id, provider, project_id, session_id, timestamp, event_kind, hook_name, tool_name, tool_category, skill_name, hint_category, hint_id, outcome, metadata_json",
            &format!(
                "id + {}, provider, project_id, session_id, timestamp, event_kind, hook_name, tool_name, tool_category, skill_name, hint_category, hint_id, outcome, metadata_json",
                offsets.analytics
            ),
        ),
        custom(
            "session",
            "sessions",
            "provider, session_id, project_key, project_path, title, started_at, ended_at, transcript_path, metadata_json, parent_session_id, is_subagent, agent_id, parent_tool_use_id",
            "SELECT t.provider, t.session_id, t.project_key, t.project_path,
                    COALESCE(t.title, s.title),
                    CASE WHEN t.started_at IS NULL THEN s.started_at
                         WHEN s.started_at IS NULL THEN t.started_at
                         ELSE MIN(t.started_at, s.started_at) END,
                    CASE WHEN t.ended_at IS NULL THEN s.ended_at
                         WHEN s.ended_at IS NULL THEN t.ended_at
                         ELSE MAX(t.ended_at, s.ended_at) END,
                    COALESCE(t.transcript_path, s.transcript_path),
                    COALESCE(t.metadata_json, s.metadata_json),
                    COALESCE(t.parent_session_id, s.parent_session_id),
                    MAX(t.is_subagent, s.is_subagent),
                    COALESCE(t.agent_id, s.agent_id),
                    COALESCE(t.parent_tool_use_id, s.parent_tool_use_id)
             FROM target_input.sessions t
             JOIN source_input.sessions s ON s.provider=t.provider AND s.session_id=t.session_id
             UNION ALL
             SELECT t.provider, t.session_id, t.project_key, t.project_path, t.title,
                    t.started_at, t.ended_at, t.transcript_path, t.metadata_json,
                    t.parent_session_id, t.is_subagent, t.agent_id, t.parent_tool_use_id
             FROM target_input.sessions t
             WHERE NOT EXISTS (SELECT 1 FROM source_input.sessions s WHERE s.provider=t.provider AND s.session_id=t.session_id)
             UNION ALL
             SELECT s.provider, s.session_id, s.project_key, s.project_path, s.title,
                    s.started_at, s.ended_at, s.transcript_path, s.metadata_json,
                    s.parent_session_id, s.is_subagent, s.agent_id, s.parent_tool_use_id
             FROM source_input.sessions s
             WHERE NOT EXISTS (SELECT 1 FROM target_input.sessions t WHERE t.provider=s.provider AND t.session_id=s.session_id)",
        ),
        custom_owned(
            "session message",
            "session_messages",
            "provider, message_id, session_id, role, timestamp, ordinal, text, kind, model, tool_names, source_path, source_offset, metadata_json",
            format!("SELECT provider, message_id, session_id, role, timestamp, ordinal, text,
                    kind, model, tool_names, source_path, source_offset, metadata_json
             FROM target_input.session_messages
             UNION ALL
             SELECT s.provider, COALESCE(m.mapped_id, s.message_id), s.session_id,
                    s.role, s.timestamp, s.ordinal, s.text, s.kind, s.model,
                    s.tool_names, s.source_path, s.source_offset, {session_metadata}
             FROM source_input.session_messages s
             LEFT JOIN consolidation_message_map m
               ON m.provider=s.provider AND m.original_id=s.message_id
             WHERE m.mapped_id IS NOT NULL OR NOT EXISTS (
                 SELECT 1 FROM target_input.session_messages t
                 WHERE t.provider=s.provider AND t.message_id=s.message_id
             )"),
        ),
        custom(
            "session schema migration",
            "session_schema_migrations",
            "name, version, applied_at",
            "SELECT name, MAX(version), MAX(applied_at) FROM (
                 SELECT name, version, applied_at FROM target_input.session_schema_migrations
                 UNION ALL SELECT name, version, applied_at FROM source_input.session_schema_migrations
             ) GROUP BY name",
        ),
        custom_owned(
            "LCM raw message",
            "lcm_raw_messages",
            "provider, message_id, session_id, store_id, role, ordinal, timestamp, content, content_hash, storage_kind, payload_ref, snippet_text, index_text, legacy_source, legacy_truncated, metadata_json",
            format!(
                "SELECT provider, message_id, session_id, store_id, role, ordinal,
                        timestamp, content, content_hash, storage_kind, payload_ref,
                        snippet_text, index_text, legacy_source, legacy_truncated, metadata_json
                 FROM target_input.lcm_raw_messages
                 UNION ALL
                 SELECT s.provider,
                        CASE WHEN m.raw_content_divergent=1
                             THEN m.mapped_id ELSE s.message_id END,
                        s.session_id,
                        s.store_id + {}, s.role, s.ordinal, s.timestamp, s.content,
                        s.content_hash, s.storage_kind, s.payload_ref, s.snippet_text,
                        s.index_text, s.legacy_source, s.legacy_truncated, {raw_metadata}
                 FROM source_input.lcm_raw_messages s
                 LEFT JOIN consolidation_message_map m
                   ON m.provider=s.provider AND m.original_id=s.message_id
                 WHERE m.raw_content_divergent=1 OR NOT EXISTS (
                     SELECT 1 FROM target_input.lcm_raw_messages t
                     WHERE t.provider=s.provider AND t.message_id=s.message_id
                 )",
                offsets.raw
            ),
        ),
        custom(
            "LCM external payload",
            "lcm_external_payloads",
            "payload_ref, provider, session_id, message_id, kind, content_hash, byte_count, char_count, created_at, metadata_json",
            "SELECT payload_ref, provider, session_id, message_id, kind, content_hash,
                    byte_count, char_count, created_at, metadata_json
             FROM target_input.lcm_external_payloads
             UNION ALL
             SELECT s.payload_ref, s.provider, s.session_id,
                    CASE WHEN m.raw_content_divergent=1
                         THEN m.mapped_id ELSE s.message_id END,
                    s.kind, s.content_hash,
                    s.byte_count, s.char_count, s.created_at, s.metadata_json
             FROM source_input.lcm_external_payloads s
             LEFT JOIN consolidation_message_map m
               ON m.provider=s.provider AND m.original_id=s.message_id
             WHERE NOT EXISTS (
                 SELECT 1 FROM target_input.lcm_external_payloads t
                 WHERE t.payload_ref=s.payload_ref
             )",
        ),
        target_wins(
            "LCM GC mark",
            "lcm_gc_marks",
            "payload_ref, state, first_seen_at, updated_at",
            "t.payload_ref=s.payload_ref",
        ),
        target_wins("LCM GC metadata", "lcm_gc_meta", "key, value", "t.key=s.key"),
        target_wins(
            "LCM summary node",
            "lcm_summary_nodes",
            "node_id, provider, conversation_id, session_id, depth, summary_text, summary_hash, summary_token_count, source_token_count, source_time_start, source_time_end, expand_hint, metadata_json, created_at",
            "t.node_id=s.node_id",
        ),
        projected_target_wins(
            "LCM summary source",
            "lcm_summary_sources",
            "node_id, source_kind, source_id, ordinal",
            &format!(
                "s.node_id, s.source_kind,
                 CASE WHEN s.source_kind='raw_message' THEN CAST({} AS TEXT)
                      ELSE s.source_id END,
                 s.ordinal",
                remapped_raw_id("CAST(s.source_id AS INTEGER)", offsets.raw)
            ),
            "t.node_id=s.node_id AND t.ordinal=s.ordinal",
        ),
    ];

    let current_frontier = remapped_raw_id("s.current_frontier_store_id", offsets.raw);
    let finalized_frontier = remapped_raw_id("s.last_finalized_frontier_store_id", offsets.raw);
    specs.push(custom_owned(
        "LCM lifecycle state",
        "lcm_lifecycle_state",
        "provider, conversation_id, current_session_id, last_finalized_session_id, current_frontier_store_id, last_finalized_frontier_store_id, rollover_at, reset_at, maintenance_at, boundary_skip_at, updated_at",
        format!(
            "SELECT t.provider, t.conversation_id, t.current_session_id,
                    t.last_finalized_session_id, t.current_frontier_store_id,
                    t.last_finalized_frontier_store_id, t.rollover_at, t.reset_at,
                    t.maintenance_at, t.boundary_skip_at, t.updated_at
             FROM target_input.lcm_lifecycle_state t
             WHERE NOT EXISTS (
                 SELECT 1 FROM source_input.lcm_lifecycle_state s
                 WHERE s.provider=t.provider AND s.conversation_id=t.conversation_id
                   AND s.updated_at > t.updated_at
             )
             UNION ALL
             SELECT s.provider, s.conversation_id, s.current_session_id,
                    s.last_finalized_session_id, {current_frontier}, {finalized_frontier},
                    s.rollover_at, s.reset_at, s.maintenance_at, s.boundary_skip_at,
                    s.updated_at
             FROM source_input.lcm_lifecycle_state s
             WHERE NOT EXISTS (
                 SELECT 1 FROM target_input.lcm_lifecycle_state t
                 WHERE t.provider=s.provider AND t.conversation_id=s.conversation_id
                   AND t.updated_at >= s.updated_at
             )"
        ),
    ));

    let from_store = remapped_raw_id("s.from_store_id", offsets.raw);
    let to_store = remapped_raw_id("s.to_store_id", offsets.raw);
    specs.push(custom_owned(
        "LCM maintenance debt",
        "lcm_maintenance_debt",
        "provider, conversation_id, debt_id, debt_kind, from_store_id, to_store_id, metadata_json, created_at",
        format!(
            "SELECT provider, conversation_id, debt_id, debt_kind, from_store_id,
                    to_store_id, metadata_json, created_at
             FROM target_input.lcm_maintenance_debt
             UNION ALL
             SELECT s.provider, s.conversation_id, s.debt_id, s.debt_kind,
                    {from_store}, {to_store}, s.metadata_json, s.created_at
             FROM source_input.lcm_maintenance_debt s
             WHERE NOT EXISTS (
                 SELECT 1 FROM target_input.lcm_maintenance_debt t
                 WHERE t.provider=s.provider AND t.conversation_id=s.conversation_id
                   AND t.debt_id=s.debt_id
             )"
        ),
    ));

    specs.extend([
        newest_wins(
            "workflow run",
            "workflow_runs",
            "run_id, parent_session_id, name, description, phase_json, status, started_ts, ended_ts, result_summary, agent_count, created_at, updated_at",
            "t.run_id=s.run_id",
        ),
        newest_wins(
            "workflow agent",
            "workflow_agents",
            "run_id, agent_label, agent_id, phase, transcript_path, agent_session_id, status, model, tokens, started_ts, ended_ts, created_at, updated_at",
            "t.run_id=s.run_id AND t.agent_label=s.agent_label AND t.agent_id=s.agent_id",
        ),
        max_meta("workflow index metadata", "workflow_index_meta"),
        offset_append(
            "session git span",
            "session_git_spans",
            "span_id, provider, session_id, thread_id, branch, worktree, first_ts, last_ts, event_count, source, created_at, updated_at",
            &format!(
                "span_id + {}, provider, session_id, thread_id, branch, worktree, first_ts, last_ts, event_count, source, created_at, updated_at",
                offsets.span
            ),
        ),
        commit_sessions(offsets.span),
        max_meta("git correlation metadata", "git_correlation_meta"),
        max_meta("session backfill metadata", "session_backfill_meta"),
        custom(
            "dashboard token count",
            "dashboard_token_counts",
            "store, provider, message_id, text_len, encoder, token_count, computed_at",
            "SELECT store, provider, message_id, text_len, encoder, token_count, computed_at
             FROM target_input.dashboard_token_counts
             UNION ALL
             SELECT s.store, s.provider, COALESCE(m.mapped_id, s.message_id),
                    s.text_len, s.encoder, s.token_count, s.computed_at
             FROM source_input.dashboard_token_counts s
             LEFT JOIN consolidation_message_map m
               ON m.provider=s.provider AND m.original_id=s.message_id
             WHERE m.mapped_id IS NOT NULL OR NOT EXISTS (
                 SELECT 1 FROM target_input.dashboard_token_counts t
                 WHERE t.store=s.store AND t.provider=s.provider
                   AND t.message_id=s.message_id
             )",
        ),
    ]);
    specs
}

fn target_wins(
    label: &'static str,
    table: &'static str,
    columns: &'static str,
    key: &'static str,
) -> TableVerification {
    projected_target_wins(label, table, columns, columns, key)
}

fn projected_target_wins(
    label: &'static str,
    table: &'static str,
    columns: &'static str,
    source_projection: &str,
    key: &'static str,
) -> TableVerification {
    custom_owned(
        label,
        table,
        columns,
        format!(
            "SELECT {columns} FROM target_input.{table}
             UNION ALL
             SELECT {source_projection} FROM source_input.{table} s
             WHERE NOT EXISTS (SELECT 1 FROM target_input.{table} t WHERE {key})"
        ),
    )
}

fn offset_append(
    label: &'static str,
    table: &'static str,
    columns: &'static str,
    source_projection: &str,
) -> TableVerification {
    custom_owned(
        label,
        table,
        columns,
        format!(
            "SELECT {columns} FROM target_input.{table}
             UNION ALL SELECT {source_projection} FROM source_input.{table}"
        ),
    )
}

fn newest_wins(
    label: &'static str,
    table: &'static str,
    columns: &'static str,
    key: &'static str,
) -> TableVerification {
    let target_columns = prefixed_columns(columns, "t");
    let source_columns = prefixed_columns(columns, "s");
    custom_owned(
        label,
        table,
        columns,
        format!(
            "SELECT {target_columns} FROM target_input.{table} t
             WHERE NOT EXISTS (
                 SELECT 1 FROM source_input.{table} s
                 WHERE {key} AND s.updated_at > t.updated_at
             )
             UNION ALL
             SELECT {source_columns} FROM source_input.{table} s
             WHERE NOT EXISTS (
                 SELECT 1 FROM target_input.{table} t
                 WHERE {key} AND t.updated_at >= s.updated_at
             )"
        ),
    )
}

fn max_meta(label: &'static str, table: &'static str) -> TableVerification {
    custom_owned(
        label,
        table,
        "key, value, updated_at",
        format!(
            "SELECT key, MAX(value), MAX(updated_at) FROM (
                 SELECT key, value, updated_at FROM target_input.{table}
                 UNION ALL SELECT key, value, updated_at FROM source_input.{table}
             ) GROUP BY key"
        ),
    )
}

fn commit_sessions(span_offset: i64) -> TableVerification {
    custom_owned(
        "commit-session attribution",
        "commit_sessions",
        "commit_sha, provider, session_id, branch, worktree, committed_at, span_overlap_kind, span_id, relation, evidence, confidence, evidence_message_id, created_at",
        format!(
            "SELECT t.commit_sha, t.provider, t.session_id,
                    CASE WHEN s.confidence > t.confidence THEN s.branch ELSE t.branch END,
                    CASE WHEN s.confidence > t.confidence THEN s.worktree ELSE t.worktree END,
                    CASE WHEN s.confidence > t.confidence THEN s.committed_at ELSE t.committed_at END,
                    CASE WHEN s.confidence > t.confidence THEN s.span_overlap_kind ELSE t.span_overlap_kind END,
                    CASE WHEN s.confidence > t.confidence
                         THEN CASE WHEN s.span_id IS NULL THEN NULL ELSE s.span_id + {span_offset} END
                         ELSE t.span_id END,
                    CASE WHEN s.confidence > t.confidence THEN s.relation ELSE t.relation END,
                    CASE WHEN s.confidence > t.confidence THEN s.evidence ELSE t.evidence END,
                    MAX(t.confidence, s.confidence),
                    CASE WHEN s.confidence > t.confidence THEN COALESCE(
                         (SELECT mapped_id FROM consolidation_message_map m
                          WHERE m.provider=s.provider
                            AND m.original_id=s.evidence_message_id),
                         s.evidence_message_id)
                         ELSE t.evidence_message_id END,
                    t.created_at
             FROM target_input.commit_sessions t
             JOIN source_input.commit_sessions s
               ON s.commit_sha=t.commit_sha AND s.provider=t.provider AND s.session_id=t.session_id
             UNION ALL
             SELECT t.commit_sha, t.provider, t.session_id, t.branch, t.worktree,
                    t.committed_at, t.span_overlap_kind, t.span_id, t.relation,
                    t.evidence, t.confidence, t.evidence_message_id, t.created_at
             FROM target_input.commit_sessions t
             WHERE NOT EXISTS (
                 SELECT 1 FROM source_input.commit_sessions s
                 WHERE s.commit_sha=t.commit_sha AND s.provider=t.provider AND s.session_id=t.session_id
             )
             UNION ALL
             SELECT s.commit_sha, s.provider, s.session_id, s.branch, s.worktree,
                    s.committed_at, s.span_overlap_kind,
                    CASE WHEN s.span_id IS NULL THEN NULL ELSE s.span_id + {span_offset} END,
                    s.relation, s.evidence, s.confidence, COALESCE(
                        (SELECT mapped_id FROM consolidation_message_map m
                         WHERE m.provider=s.provider
                           AND m.original_id=s.evidence_message_id),
                        s.evidence_message_id), s.created_at
             FROM source_input.commit_sessions s
             WHERE NOT EXISTS (
                 SELECT 1 FROM target_input.commit_sessions t
                 WHERE t.commit_sha=s.commit_sha AND t.provider=s.provider AND t.session_id=s.session_id
             )"
        ),
    )
}

fn remapped_raw_id(expression: &str, offset: i64) -> String {
    format!(
        "(SELECT COALESCE(t.store_id, r.store_id + {offset})
          FROM source_input.lcm_raw_messages r
          LEFT JOIN consolidation_message_map m
            ON m.provider=r.provider AND m.original_id=r.message_id
          LEFT JOIN target_input.lcm_raw_messages t
            ON t.provider=r.provider
           AND t.message_id=CASE WHEN m.raw_content_divergent=1
                                 THEN m.mapped_id ELSE r.message_id END
          WHERE r.store_id={expression})"
    )
}

fn prefixed_columns(columns: &str, alias: &str) -> String {
    columns
        .split(',')
        .map(|column| format!("{alias}.{}", column.trim()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn custom(
    label: &'static str,
    table: &'static str,
    columns: &'static str,
    expected: &'static str,
) -> TableVerification {
    custom_owned(label, table, columns, expected.to_string())
}

fn custom_owned(
    label: &'static str,
    table: &'static str,
    columns: &'static str,
    expected: String,
) -> TableVerification {
    TableVerification {
        label,
        table,
        columns,
        expected,
    }
}

async fn verify_payload_files(conn: &Connection, destination_root: &Path) -> Result<()> {
    let mut rows = conn
        .query(
            "SELECT payload_ref, content_hash, byte_count FROM lcm_external_payloads",
            (),
        )
        .await
        .map_err(|error| db_error("verify_consolidation", error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error("verify_consolidation", error))?
    {
        let payload_ref = row
            .get::<String>(0)
            .map_err(|error| db_error("verify_consolidation", error))?;
        let content_hash = row
            .get::<String>(1)
            .map_err(|error| db_error("verify_consolidation", error))?;
        let byte_count = row
            .get::<i64>(2)
            .map_err(|error| db_error("verify_consolidation", error))?;
        tracedecay_sessions::lcm::payload::validate_payload_ref(&payload_ref).map_err(|_| {
            db_message(
                "verify_consolidation",
                format!("destination contains invalid external payload ref '{payload_ref}'"),
            )
        })?;
        let path = destination_root.join("lcm-payloads").join(&payload_ref);
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            db_message(
                "verify_consolidation",
                format!(
                    "destination external payload '{}' is missing: {error}",
                    path.display()
                ),
            )
        })?;
        if !metadata.is_file() || i64::try_from(metadata.len()).ok() != Some(byte_count) {
            return Err(db_message(
                "verify_consolidation",
                format!(
                    "destination external payload '{}' has the wrong file shape or length",
                    path.display()
                ),
            ));
        }
        let digest = super::super::files::file_digest(&path)?;
        if hex::encode(digest) != content_hash {
            return Err(db_message(
                "verify_consolidation",
                format!(
                    "destination external payload '{}' failed content hash verification",
                    path.display()
                ),
            ));
        }
    }
    Ok(())
}
