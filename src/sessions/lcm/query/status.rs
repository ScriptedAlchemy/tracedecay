use std::path::Path;

use super::*;

const STORE_STATUS_PAGE_SIZE: i64 = 512;
const STORE_STATUS_PAGE_MAX_BYTES: i64 = 32 * 1024 * 1024;

#[derive(Debug, Default, PartialEq, Eq)]
struct StatusQueryWork {
    status_query_calls: usize,
    payload_health_scans: usize,
}

impl StatusQueryWork {
    fn record_query(&mut self) {
        self.status_query_calls += 1;
    }

    fn record_payload_health(&mut self) {
        self.payload_health_scans += 1;
    }
}

struct StatusCounts {
    provider_count: i64,
    raw_message_count: i64,
    summary_node_count: i64,
    maintenance_debt_count: i64,
    lifecycle_state_count: i64,
    frontier_count: i64,
    legacy_truncated_count: i64,
    lossy_ingest_records: i64,
}

pub(super) async fn status_for_provider(
    conn: &(impl QueryExecutor + ?Sized),
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
    deep: bool,
    gc_config: &LcmGcConfig,
) -> Result<LcmStatus, LcmError> {
    let mut work = StatusQueryWork::default();
    status_for_provider_with_work(
        conn,
        storage_root,
        provider,
        session_id,
        deep,
        gc_config,
        &mut work,
    )
    .await
}

async fn status_for_provider_with_work(
    conn: &(impl QueryExecutor + ?Sized),
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
    deep: bool,
    gc_config: &LcmGcConfig,
    work: &mut StatusQueryWork,
) -> Result<LcmStatus, LcmError> {
    work.record_query();
    let schema_version = schema::schema_version(conn)
        .await
        .unwrap_or(LCM_SCHEMA_VERSION);
    work.record_query();
    let counts = status_counts(conn, provider, session_id).await?;
    work.record_payload_health();
    let payload_health = payload_health_detail(
        conn,
        storage_root,
        provider,
        session_id,
        deep,
        20,
        gc_config,
    )
    .await?;
    work.record_query();
    let lifecycle_metadata = load_lifecycle_metadata(conn, provider, session_id).await?;
    work.record_query();
    let store = store_status(conn, provider, session_id).await?;
    work.record_query();
    let dag = dag_status(conn, provider, session_id).await?;

    Ok(status_from_parts(
        schema_version,
        counts,
        store,
        dag,
        payload_health,
        lifecycle_metadata,
    ))
}

pub(super) async fn aggregate_provider_status(
    conn: &(impl QueryExecutor + ?Sized),
    storage_root: &Path,
    session_id: Option<&str>,
    deep: bool,
    gc_config: &LcmGcConfig,
) -> Result<LcmStatus, LcmError> {
    Ok(
        aggregate_provider_status_with_work(conn, storage_root, session_id, deep, gc_config)
            .await?
            .0,
    )
}

async fn aggregate_provider_status_with_work(
    conn: &(impl QueryExecutor + ?Sized),
    storage_root: &Path,
    session_id: Option<&str>,
    deep: bool,
    gc_config: &LcmGcConfig,
) -> Result<(LcmStatus, StatusQueryWork), LcmError> {
    let mut work = StatusQueryWork::default();
    work.record_query();
    let schema_version = schema::schema_version(conn)
        .await
        .unwrap_or(LCM_SCHEMA_VERSION);
    work.record_query();
    let counts = status_counts(conn, "all", session_id).await?;
    if counts.provider_count == 0 {
        return Ok((empty_status(schema_version, gc_config), work));
    }

    work.record_payload_health();
    let payload_health =
        payload_health_detail(conn, storage_root, "all", session_id, deep, 20, gc_config).await?;
    work.record_query();
    let store = store_status(conn, "all", session_id).await?;
    work.record_query();
    let dag = dag_status(conn, "all", session_id).await?;
    let status = status_from_parts(
        schema_version,
        counts,
        store,
        dag,
        payload_health,
        LcmLifecycleMetadata::default(),
    );
    Ok((status, work))
}

async fn status_counts(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<StatusCounts, LcmError> {
    let mut rows = conn
        .query(
            "WITH status_providers(provider, session_id) AS (
                 SELECT provider, session_id FROM lcm_raw_messages
                 UNION
                 SELECT provider, session_id FROM lcm_summary_nodes
                 UNION
                 SELECT provider, session_id FROM lcm_external_payloads
                 UNION
                 SELECT provider, current_session_id AS session_id FROM lcm_lifecycle_state
             )
             SELECT
                 (SELECT COUNT(DISTINCT provider)
                    FROM status_providers
                   WHERE (?1 = 'all' OR provider = ?1)
                     AND (?2 IS NULL OR session_id = ?2)),
                 (SELECT COUNT(*)
                    FROM lcm_raw_messages
                   WHERE (?1 = 'all' OR provider = ?1)
                     AND (?2 IS NULL OR session_id = ?2)),
                 (SELECT COUNT(*)
                    FROM lcm_summary_nodes
                   WHERE (?1 = 'all' OR provider = ?1)
                     AND (?2 IS NULL OR session_id = ?2)),
                 (SELECT COUNT(*)
                    FROM lcm_maintenance_debt d
                    JOIN lcm_lifecycle_state s
                      ON s.provider = d.provider
                     AND s.conversation_id = d.conversation_id
                   WHERE (?1 = 'all' OR d.provider = ?1)
                     AND (?2 IS NULL OR s.current_session_id = ?2)),
                 (SELECT COUNT(*)
                    FROM lcm_lifecycle_state
                   WHERE (?1 = 'all' OR provider = ?1)
                     AND (?2 IS NULL OR current_session_id = ?2)),
                 (SELECT COUNT(*)
                    FROM lcm_lifecycle_state
                   WHERE (?1 = 'all' OR provider = ?1)
                     AND (?2 IS NULL OR current_session_id = ?2)
                     AND current_frontier_store_id IS NOT NULL),
                 (SELECT COUNT(*)
                    FROM lcm_raw_messages
                   WHERE (?1 = 'all' OR provider = ?1)
                     AND (?2 IS NULL OR session_id = ?2)
                     AND legacy_truncated != 0),
                 (SELECT COUNT(*)
                    FROM lcm_raw_messages
                   WHERE (?1 = 'all' OR provider = ?1)
                     AND (?2 IS NULL OR session_id = ?2)
                     AND metadata_json IS NOT NULL
                     AND json_valid(metadata_json)
                     AND json_type(
                         metadata_json,
                         '$.ingest_protection.lossy'
                     ) = 'true')",
            params![provider, session_id],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| LcmError::Db("status count query returned no rows".to_string()))?;
    Ok(StatusCounts {
        provider_count: row.get(0)?,
        raw_message_count: row.get(1)?,
        summary_node_count: row.get(2)?,
        maintenance_debt_count: row.get(3)?,
        lifecycle_state_count: row.get(4)?,
        frontier_count: row.get(5)?,
        legacy_truncated_count: row.get(6)?,
        lossy_ingest_records: row.get(7)?,
    })
}

fn status_from_parts(
    schema_version: i64,
    counts: StatusCounts,
    store: LcmStoreStatus,
    dag: LcmDagStatus,
    payload_health: PayloadHealthDetail,
    lifecycle_metadata: LcmLifecycleMetadata,
) -> LcmStatus {
    let lossy_records = counts.legacy_truncated_count + counts.lossy_ingest_records;
    LcmStatus {
        schema_version,
        raw_message_count: counts.raw_message_count,
        summary_node_count: counts.summary_node_count,
        external_payload_count: payload_health.payload.externalized_count,
        missing_payload_count: payload_health.payload.missing_count,
        unreferenced_payload_count: payload_health.payload.unreferenced_count,
        maintenance_debt_count: counts.maintenance_debt_count,
        store,
        dag,
        config: LcmConfigStatus {
            fresh_tail_count: LCM_DEFAULT_FRESH_TAIL_COUNT,
            summary_fan_in: LCM_DEFAULT_SUMMARY_FAN_IN,
            compression_boundary_cooldown_seconds: LCM_COMPRESSION_BOUNDARY_COOLDOWN_SECONDS,
        },
        payload: payload_health.payload,
        payload_gc: payload_health.payload_gc,
        lifecycle: LcmLifecycleStatus {
            lifecycle_state_count: counts.lifecycle_state_count,
            frontier_count: counts.frontier_count,
            maintenance_debt_count: counts.maintenance_debt_count,
            current_session_id: lifecycle_metadata.current_session_id,
            current_frontier_store_id: lifecycle_metadata.current_frontier_store_id,
            last_finalized_session_id: lifecycle_metadata.last_finalized_session_id,
            last_finalized_frontier_store_id: lifecycle_metadata.last_finalized_frontier_store_id,
        },
        redaction: LcmRedactionStatus {
            enabled: lossy_records > 0,
            lossy_records,
            legacy_truncated_count: counts.legacy_truncated_count,
        },
    }
}

#[cfg(test)]
fn merge_lcm_status(target: &mut LcmStatus, source: LcmStatus) {
    target.raw_message_count += source.raw_message_count;
    target.summary_node_count += source.summary_node_count;
    target.external_payload_count += source.external_payload_count;
    target.missing_payload_count += source.missing_payload_count;
    target.unreferenced_payload_count += source.unreferenced_payload_count;
    target.maintenance_debt_count += source.maintenance_debt_count;
    target.store.messages += source.store.messages;
    target.store.estimated_tokens += source.store.estimated_tokens;
    target.dag.total_nodes += source.dag.total_nodes;
    target.dag.total_tokens += source.dag.total_tokens;
    target.dag.total_source_tokens += source.dag.total_source_tokens;
    for (depth, source_depth) in source.dag.depths {
        let target_depth = target
            .dag
            .depths
            .entry(depth)
            .or_insert_with(|| LcmDagDepthStatus {
                count: 0,
                tokens: 0,
                source_tokens: 0,
            });
        target_depth.count += source_depth.count;
        target_depth.tokens += source_depth.tokens;
        target_depth.source_tokens += source_depth.source_tokens;
    }
    merge_payload_status(&mut target.payload, &source.payload);
    merge_payload_gc_status(&mut target.payload_gc, source.payload_gc);
    target.lifecycle.lifecycle_state_count += source.lifecycle.lifecycle_state_count;
    target.lifecycle.frontier_count += source.lifecycle.frontier_count;
    target.lifecycle.maintenance_debt_count += source.lifecycle.maintenance_debt_count;
    target.redaction.lossy_records += source.redaction.lossy_records;
    target.redaction.legacy_truncated_count += source.redaction.legacy_truncated_count;
}

#[cfg(test)]
fn merge_payload_status(target: &mut LcmPayloadStatus, source: &LcmPayloadStatus) {
    target.externalized_count += source.externalized_count;
    target.missing_count += source.missing_count;
    target.unreferenced_count += source.unreferenced_count;
    target.placeholder_ref_count += source.placeholder_ref_count;
    target.missing_placeholder_metadata_count += source.missing_placeholder_metadata_count;
    target.missing_placeholder_file_count += source.missing_placeholder_file_count;
    target.gc_candidate_count += source.gc_candidate_count;
    target.root_contained &= source.root_contained;
    target.orphan_file_count += source.orphan_file_count;
    target.tombstoned_count += source.tombstoned_count;
    target.referenced_count += source.referenced_count;
    target.total_bytes += source.total_bytes;
    target.referenced_bytes += source.referenced_bytes;
    target.orphan_file_bytes += source.orphan_file_bytes;
    target.reclaimable_bytes += source.reclaimable_bytes;
    target.reclaimable_bytes_after_grace += source.reclaimable_bytes_after_grace;
    target.integrity_mismatch_count = match (
        target.integrity_mismatch_count,
        source.integrity_mismatch_count,
    ) {
        (Some(left), Some(right)) => Some(left + right),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    };
}

#[cfg(test)]
fn merge_payload_gc_status(target: &mut LcmPayloadGcStatus, source: LcmPayloadGcStatus) {
    target.last_gc_at = max_option_i64(target.last_gc_at, source.last_gc_at);
    target.last_gc_duration_ms =
        max_option_u64(target.last_gc_duration_ms, source.last_gc_duration_ms);
    if target.last_gc_status.as_deref() != Some("failed") {
        target.last_gc_status = source.last_gc_status.or(target.last_gc_status.take());
    }
    target.last_gc_error = source.last_gc_error.or(target.last_gc_error.take());
    target.last_reaped_refs = sum_option_i64(target.last_reaped_refs, source.last_reaped_refs);
    target.last_reaped_bytes = sum_option_u64(target.last_reaped_bytes, source.last_reaped_bytes);
    target.next_run_eligible_at =
        min_option_i64(target.next_run_eligible_at, source.next_run_eligible_at);
}

#[cfg(test)]
fn max_option_i64(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

#[cfg(test)]
fn min_option_i64(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

#[cfg(test)]
fn sum_option_i64(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left + right),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

#[cfg(test)]
fn max_option_u64(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

#[cfg(test)]
fn sum_option_u64(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left + right),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

pub(super) fn empty_status(schema_version: i64, gc_config: &LcmGcConfig) -> LcmStatus {
    let gc_config = gc_config.clone().normalized();
    let grace_seconds = i64::try_from(gc_config.grace_seconds).unwrap_or(i64::MAX);
    let reap_missing_after_seconds =
        i64::try_from(gc_config.reap_missing_after).unwrap_or(i64::MAX);
    LcmStatus {
        schema_version,
        raw_message_count: 0,
        summary_node_count: 0,
        external_payload_count: 0,
        missing_payload_count: 0,
        unreferenced_payload_count: 0,
        maintenance_debt_count: 0,
        store: LcmStoreStatus {
            messages: 0,
            estimated_tokens: 0,
        },
        dag: LcmDagStatus {
            total_nodes: 0,
            total_tokens: 0,
            total_source_tokens: 0,
            compression_ratio: "0:1".to_string(),
            depths: BTreeMap::new(),
        },
        config: LcmConfigStatus {
            fresh_tail_count: LCM_DEFAULT_FRESH_TAIL_COUNT,
            summary_fan_in: LCM_DEFAULT_SUMMARY_FAN_IN,
            compression_boundary_cooldown_seconds: LCM_COMPRESSION_BOUNDARY_COOLDOWN_SECONDS,
        },
        payload: LcmPayloadStatus {
            externalized_count: 0,
            missing_count: 0,
            unreferenced_count: 0,
            placeholder_ref_count: 0,
            missing_placeholder_metadata_count: 0,
            missing_placeholder_file_count: 0,
            gc_candidate_count: 0,
            root_contained: true,
            orphan_file_count: 0,
            tombstoned_count: 0,
            referenced_count: 0,
            total_bytes: 0,
            referenced_bytes: 0,
            orphan_file_bytes: 0,
            reclaimable_bytes: 0,
            reclaimable_bytes_after_grace: 0,
            integrity_mismatch_count: None,
        },
        payload_gc: LcmPayloadGcStatus {
            last_gc_at: None,
            last_gc_duration_ms: None,
            last_gc_status: None,
            last_gc_error: None,
            last_reaped_refs: None,
            last_reaped_bytes: None,
            grace_seconds,
            reap_missing_metadata_after_seconds: reap_missing_after_seconds,
            next_run_eligible_at: None,
        },
        lifecycle: LcmLifecycleStatus {
            lifecycle_state_count: 0,
            frontier_count: 0,
            maintenance_debt_count: 0,
            current_session_id: None,
            current_frontier_store_id: None,
            last_finalized_session_id: None,
            last_finalized_frontier_store_id: None,
        },
        redaction: LcmRedactionStatus {
            enabled: false,
            lossy_records: 0,
            legacy_truncated_count: 0,
        },
    }
}

async fn store_status(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<LcmStoreStatus, LcmError> {
    let mut messages = 0_i64;
    let mut estimated_tokens = 0_i64;
    let mut after_store_id = 0_i64;
    loop {
        let mut rows = conn
            .query(
                "WITH page AS (
                     SELECT store_id, COALESCE(content, snippet_text) AS replay_text
                     FROM lcm_raw_messages
                     WHERE (?1 = 'all' OR provider = ?1)
                       AND (?2 IS NULL OR session_id = ?2)
                       AND store_id > ?3
                     ORDER BY store_id
                     LIMIT ?4
                 ),
                 bounded AS (
                     SELECT store_id, replay_text,
                            ROW_NUMBER() OVER (ORDER BY store_id) AS page_row,
                            SUM(length(CAST(replay_text AS BLOB)))
                                OVER (ORDER BY store_id) AS cumulative_bytes
                     FROM page
                 )
                 SELECT store_id, replay_text
                 FROM bounded
                 WHERE cumulative_bytes <= ?5 OR page_row = 1
                 ORDER BY store_id",
                params![
                    provider,
                    session_id,
                    after_store_id,
                    STORE_STATUS_PAGE_SIZE,
                    STORE_STATUS_PAGE_MAX_BYTES
                ],
            )
            .await?;
        let mut page_count = 0usize;
        while let Some(row) = rows.next().await? {
            let store_id: i64 = row.get(0)?;
            if store_id <= after_store_id {
                return Err(LcmError::Db(
                    "LCM store status page did not advance".to_string(),
                ));
            }
            messages += 1;
            // Externalized rows count their inline placeholder, matching what the
            // engine replays into active context.
            let text: String = row.get(1)?;
            estimated_tokens += estimate_tokens(&text);
            after_store_id = store_id;
            page_count += 1;
        }
        drop(rows);
        if page_count == 0 {
            break;
        }
    }
    Ok(LcmStoreStatus {
        messages,
        estimated_tokens,
    })
}

async fn dag_status(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<LcmDagStatus, LcmError> {
    let mut rows = conn
        .query(
            "SELECT depth, COUNT(*), SUM(summary_token_count), SUM(source_token_count)
             FROM lcm_summary_nodes
             WHERE (?1 = 'all' OR provider = ?1)
               AND (?2 IS NULL OR session_id = ?2)
             GROUP BY depth
             ORDER BY depth",
            params![provider, session_id],
        )
        .await?;
    let mut depths = std::collections::BTreeMap::new();
    let mut total_nodes = 0_i64;
    let mut total_tokens = 0_i64;
    let mut total_source_tokens = 0_i64;
    while let Some(row) = rows.next().await? {
        let depth: i64 = row.get(0)?;
        let count: i64 = row.get(1)?;
        let tokens: i64 = row.get(2)?;
        let source_tokens: i64 = row.get(3)?;
        total_nodes += count;
        total_tokens += tokens;
        total_source_tokens += source_tokens;
        depths.insert(
            format!("d{depth}"),
            LcmDagDepthStatus {
                count,
                tokens,
                source_tokens,
            },
        );
    }
    // Hermes renders `round(source/summary, 1)` as "N.N:1" and "0:1" for an
    // empty DAG (`hermes-lcm/tools.py` lcm_status). Python `round` uses
    // bankers rounding (ties-to-even), so mirror it with integer math.
    let compression_ratio = python_round_ratio_to_tenths(total_source_tokens, total_tokens);
    Ok(LcmDagStatus {
        total_nodes,
        total_tokens,
        total_source_tokens,
        compression_ratio,
        depths,
    })
}

fn python_round_ratio_to_tenths(total_source_tokens: i64, total_tokens: i64) -> String {
    if total_tokens <= 0 {
        return "0:1".to_string();
    }
    let numerator = i128::from(total_source_tokens.max(0)) * 10;
    let denominator = i128::from(total_tokens);
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    let rounded = match (remainder * 2).cmp(&denominator) {
        std::cmp::Ordering::Less => quotient,
        std::cmp::Ordering::Greater => quotient + 1,
        std::cmp::Ordering::Equal => {
            if quotient % 2 == 0 {
                quotient
            } else {
                quotient + 1
            }
        }
    };
    let whole = rounded / 10;
    let fractional = (rounded % 10).abs();
    format!("{whole}.{fractional}:1")
}

async fn load_lifecycle_metadata(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<LcmLifecycleMetadata, LcmError> {
    let mut rows = conn
        .query(
            "SELECT current_session_id, current_frontier_store_id,
                    last_finalized_session_id, last_finalized_frontier_store_id
             FROM lcm_lifecycle_state
             WHERE provider = ?1 AND (?2 IS NULL OR current_session_id = ?2)
             ORDER BY updated_at DESC, conversation_id DESC
             LIMIT 1",
            params![provider, session_id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(LcmLifecycleMetadata {
            current_session_id: None,
            current_frontier_store_id: None,
            last_finalized_session_id: None,
            last_finalized_frontier_store_id: None,
        });
    };
    Ok(LcmLifecycleMetadata {
        current_session_id: row.get(0)?,
        current_frontier_store_id: row.get(1)?,
        last_finalized_session_id: row.get(2)?,
        last_finalized_frontier_store_id: row.get(3)?,
    })
}

#[allow(clippy::struct_field_names)]
#[derive(Default)]
struct LcmLifecycleMetadata {
    current_session_id: Option<String>,
    current_frontier_store_id: Option<i64>,
    last_finalized_session_id: Option<String>,
    last_finalized_frontier_store_id: Option<i64>,
}

#[cfg(test)]
mod tests {
    use crate::db::engine::{Connection, TestConnection};
    use tempfile::TempDir;

    use super::*;

    async fn test_lcm_connection() -> (TempDir, TestConnection) {
        let directory = TempDir::new().expect("session database tempdir");
        let conn = TestConnection::open(&directory.path().join("sessions.db"));
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE sessions (
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
             );",
        )
        .await
        .expect("create session tables");
        schema::ensure_lcm_schema(&*conn)
            .await
            .expect("create LCM schema");
        (directory, conn)
    }

    async fn seed_provider(conn: &Connection, index: usize) {
        let provider = format!("provider-{index:02}");
        let session_id = format!("session-{index:02}");
        let message_id = format!("message-{index:02}");
        let node_id = format!("node-{index:02}");
        let conversation_id = format!("conversation-{index:02}");
        let debt_id = format!("debt-{index:02}");
        conn.execute(
            "INSERT INTO sessions(provider, session_id, project_key, project_path)
             VALUES (?1, ?2, '/project', '/project')",
            params![provider.clone(), session_id.clone()],
        )
        .await
        .expect("insert session");
        conn.execute(
            "INSERT INTO lcm_raw_messages (
                 provider, message_id, session_id, role, ordinal, timestamp,
                 content, content_hash, storage_kind, payload_ref, snippet_text,
                 index_text, legacy_source, legacy_truncated, metadata_json
             )
             VALUES (?1, ?2, ?3, 'assistant', 1, 1, ?4, ?5, 'inline', NULL, ?4, ?4, 0, ?6, ?7)",
            params![
                provider.clone(),
                message_id,
                session_id.clone(),
                format!("provider {index} message"),
                format!("hash-{index:02}"),
                i64::from(index.is_multiple_of(2)),
                if index.is_multiple_of(3) {
                    Some(r#"{"ingest_protection":{"lossy":true}}"#.to_string())
                } else {
                    None
                },
            ],
        )
        .await
        .expect("insert raw message");
        conn.execute(
            "INSERT INTO lcm_summary_nodes (
                 node_id, provider, conversation_id, session_id, depth,
                 summary_text, summary_hash, summary_token_count, source_token_count
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                node_id,
                provider.clone(),
                conversation_id.clone(),
                session_id.clone(),
                (index % 2) as i64,
                format!("provider {index} summary"),
                format!("summary-hash-{index:02}"),
                (index + 1) as i64,
                (index + 4) as i64,
            ],
        )
        .await
        .expect("insert summary node");
        conn.execute(
            "INSERT INTO lcm_lifecycle_state (
                 provider, conversation_id, current_session_id,
                 current_frontier_store_id, last_finalized_session_id
             )
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                provider.clone(),
                conversation_id.clone(),
                session_id,
                (index + 1) as i64,
                format!("previous-{index:02}"),
            ],
        )
        .await
        .expect("insert lifecycle state");
        conn.execute(
            "INSERT INTO lcm_maintenance_debt (
                 provider, conversation_id, debt_id, debt_kind
             )
             VALUES (?1, ?2, ?3, 'raw_backlog')",
            params![provider, conversation_id, debt_id],
        )
        .await
        .expect("insert maintenance debt");
    }

    async fn legacy_aggregate(
        conn: &Connection,
        storage_root: &Path,
        providers: &[String],
        deep: bool,
    ) -> LcmStatus {
        let gc_config = LcmGcConfig::default();
        let schema_version = schema::schema_version(conn)
            .await
            .unwrap_or(LCM_SCHEMA_VERSION);
        let mut aggregate = empty_status(schema_version, &gc_config);
        for provider in providers {
            let status = status_for_provider(conn, storage_root, provider, None, deep, &gc_config)
                .await
                .expect("load provider status");
            merge_lcm_status(&mut aggregate, status);
        }
        let payload_health =
            payload_health_detail(conn, storage_root, "all", None, deep, 20, &gc_config)
                .await
                .expect("load aggregate payload health");
        aggregate.external_payload_count = payload_health.payload.externalized_count;
        aggregate.missing_payload_count = payload_health.payload.missing_count;
        aggregate.unreferenced_payload_count = payload_health.payload.unreferenced_count;
        aggregate.payload = payload_health.payload;
        aggregate.payload_gc = payload_health.payload_gc;
        aggregate.dag.compression_ratio = python_round_ratio_to_tenths(
            aggregate.dag.total_source_tokens,
            aggregate.dag.total_tokens,
        );
        aggregate.redaction.enabled = aggregate.redaction.lossy_records > 0;
        aggregate
    }

    #[tokio::test]
    async fn aggregate_status_batches_queries_and_preserves_legacy_output() {
        let (_database_dir, conn) = test_lcm_connection().await;
        let storage = TempDir::new().expect("storage tempdir");
        for index in 0..3 {
            seed_provider(&conn, index).await;
        }
        let providers = (0..3)
            .map(|index| format!("provider-{index:02}"))
            .collect::<Vec<_>>();
        for deep in [false, true] {
            let expected = legacy_aggregate(&conn, storage.path(), &providers, deep).await;
            let (actual, work) = aggregate_provider_status_with_work(
                &*conn,
                storage.path(),
                None,
                deep,
                &LcmGcConfig::default(),
            )
            .await
            .expect("load batched aggregate status");

            assert_eq!(actual, expected);
            assert_eq!(work.status_query_calls, 4);
            assert_eq!(work.payload_health_scans, 1);
        }
    }

    #[tokio::test]
    async fn aggregate_status_work_is_provider_independent_across_thirty_runs() {
        let (_database_dir, conn) = test_lcm_connection().await;
        let storage = TempDir::new().expect("storage tempdir");
        seed_provider(&conn, 0).await;
        let (_, baseline) = aggregate_provider_status_with_work(
            &*conn,
            storage.path(),
            None,
            false,
            &LcmGcConfig::default(),
        )
        .await
        .expect("load one-provider aggregate status");
        assert_eq!(baseline.status_query_calls, 4);
        assert_eq!(baseline.payload_health_scans, 1);

        for index in 1..12 {
            seed_provider(&conn, index).await;
        }
        for _ in 0..30 {
            let (status, work) = aggregate_provider_status_with_work(
                &*conn,
                storage.path(),
                None,
                false,
                &LcmGcConfig::default(),
            )
            .await
            .expect("load multi-provider aggregate status");
            assert_eq!(status.raw_message_count, 12);
            assert_eq!(work, baseline);
        }
    }

    #[tokio::test]
    async fn store_status_pages_more_rows_than_runtime_materialization_limit() {
        let (_database_dir, conn) = test_lcm_connection().await;
        conn.execute(
            "INSERT INTO sessions(provider, session_id, project_key, project_path)
             VALUES ('cursor', 'session-paged-status', '/project', '/project')",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "WITH RECURSIVE fixture(value) AS (
                 SELECT 1
                 UNION ALL
                 SELECT value + 1 FROM fixture WHERE value < 10001
             )
             INSERT INTO lcm_raw_messages (
                 provider, message_id, session_id, role, ordinal, timestamp,
                 content, content_hash, storage_kind, payload_ref, snippet_text,
                 index_text, legacy_source, legacy_truncated, metadata_json
             )
             SELECT 'cursor', printf('message-%05d', value), 'session-paged-status',
                    'assistant', value, value, 'one token',
                    printf('hash-%05d', value), 'inline', NULL, 'one token',
                    'one token', 0, 0, NULL
             FROM fixture",
            (),
        )
        .await
        .unwrap();

        let status = store_status(&*conn, "cursor", None).await.unwrap();

        assert_eq!(status.messages, 10001);
        assert_eq!(status.estimated_tokens, 20002);
    }
}
