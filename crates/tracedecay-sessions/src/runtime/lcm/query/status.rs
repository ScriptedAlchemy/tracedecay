use std::path::Path;

use super::*;

const STORE_STATUS_PAGE_SIZE: i64 = 512;
const STORE_STATUS_TOKEN_SCAN_MAX_BYTES: i64 = 1024 * 1024;

/// Message bodies summed for the replay token estimate in one status call.
///
/// The estimate needs each message's text, so an unbounded scan reads the whole
/// raw store — gigabytes on a long-lived profile — and the request is
/// interrupted before it can answer. Past this many rows the status reports a
/// typed partial estimate with a resume cursor instead.
const STORE_STATUS_TOKEN_SCAN_BUDGET: i64 = 20_000;

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
    let payload_health = if deep {
        payload_health_detail(
            conn,
            storage_root,
            provider,
            session_id,
            true,
            20,
            gc_config,
        )
        .await?
    } else {
        payload_health_summary(conn, storage_root, provider, session_id, gc_config).await?
    };
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
    let payload_health = if deep {
        payload_health_detail(conn, storage_root, "all", session_id, true, 20, gc_config).await?
    } else {
        payload_health_summary(conn, storage_root, "all", session_id, gc_config).await?
    };
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
            "SELECT
                 CASE WHEN
                     EXISTS (
                         SELECT 1 FROM lcm_raw_messages
                          WHERE (?1 = 'all' OR provider = ?1)
                            AND (?2 IS NULL OR session_id = ?2)
                     )
                     OR EXISTS (
                         SELECT 1 FROM lcm_summary_nodes
                          WHERE (?1 = 'all' OR provider = ?1)
                            AND (?2 IS NULL OR session_id = ?2)
                     )
                     OR EXISTS (
                         SELECT 1 FROM lcm_external_payloads
                          WHERE (?1 = 'all' OR provider = ?1)
                            AND (?2 IS NULL OR session_id = ?2)
                     )
                     OR EXISTS (
                         SELECT 1 FROM lcm_lifecycle_state
                          WHERE (?1 = 'all' OR provider = ?1)
                            AND (?2 IS NULL OR current_session_id = ?2)
                     )
                 THEN 1 ELSE 0 END,
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
    // A merged estimate is only complete when every merged scope was.
    target.store.token_estimate.complete &= source.store.token_estimate.complete;
    target.store.token_estimate.scanned_messages += source.store.token_estimate.scanned_messages;
    target.store.token_estimate.next_after_store_id = min_option_i64(
        target.store.token_estimate.next_after_store_id,
        source.store.token_estimate.next_after_store_id,
    );
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
            token_estimate: LcmStoreTokenCoverage::complete(0),
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
            coverage: LcmPayloadCoverage {
                state: LcmPayloadCoverageState::Complete,
                scanned_metadata_refs: 0,
                scanned_files: 0,
                reason: None,
            },
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
    store_status_within(conn, provider, session_id, STORE_STATUS_TOKEN_SCAN_BUDGET).await
}

/// Exact message count plus a token estimate over at most `token_scan_budget`
/// message bodies.
///
/// The count is a cheap indexed aggregate, so the reported store size is always
/// the true one. The token estimate has to read text, so it stops at the budget
/// and reports the resume cursor instead of streaming a multi-gigabyte store
/// past the caller's deadline.
async fn store_status_within(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
    token_scan_budget: i64,
) -> Result<LcmStoreStatus, LcmError> {
    let messages = store_message_count(conn, provider, session_id).await?;
    let mut estimated_tokens = 0_i64;
    let mut scanned_messages = 0_i64;
    let mut scanned_bytes = 0_i64;
    let mut after_store_id = 0_i64;
    let mut complete = true;
    while scanned_messages < token_scan_budget && scanned_bytes < STORE_STATUS_TOKEN_SCAN_MAX_BYTES
    {
        let page_limit = STORE_STATUS_PAGE_SIZE.min(token_scan_budget - scanned_messages);
        let remaining_bytes = STORE_STATUS_TOKEN_SCAN_MAX_BYTES.saturating_sub(scanned_bytes);
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
                    page_limit,
                    remaining_bytes
                ],
            )
            .await?;
        let mut page_count = 0i64;
        while let Some(row) = rows.next().await? {
            let store_id: i64 = row.get(0)?;
            if store_id <= after_store_id {
                return Err(LcmError::Db(
                    "LCM store status page did not advance".to_string(),
                ));
            }
            // Externalized rows count their inline placeholder, matching what the
            // engine replays into active context.
            let text: String = row.get(1)?;
            estimated_tokens += estimate_tokens(&text);
            scanned_bytes =
                scanned_bytes.saturating_add(i64::try_from(text.len()).unwrap_or(i64::MAX));
            after_store_id = store_id;
            page_count += 1;
        }
        drop(rows);
        if page_count == 0 {
            break;
        }
        scanned_messages += page_count;
        if (scanned_messages >= token_scan_budget
            || scanned_bytes >= STORE_STATUS_TOKEN_SCAN_MAX_BYTES)
            && scanned_messages < messages
        {
            complete = false;
            break;
        }
    }
    let token_estimate = if complete {
        LcmStoreTokenCoverage::complete(scanned_messages)
    } else {
        LcmStoreTokenCoverage {
            complete: false,
            scanned_messages,
            next_after_store_id: Some(after_store_id),
        }
    };
    Ok(LcmStoreStatus {
        messages,
        estimated_tokens,
        token_estimate,
    })
}

/// Exact raw-message count for the scope. The `(provider, session_id,
/// store_id)` index serves this without touching message text.
async fn store_message_count(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let mut rows = conn
        .query(
            "SELECT COUNT(*)
             FROM lcm_raw_messages
             WHERE (?1 = 'all' OR provider = ?1)
               AND (?2 IS NULL OR session_id = ?2)",
            params![provider, session_id],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| LcmError::Db("LCM store count query returned no rows".to_string()))?;
    Ok(row.get(0)?)
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
    use tempfile::TempDir;
    use tracedecay_runtime_core::db::engine::{Connection, TestConnection};

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
        schema::ensure_lcm_schema(&conn)
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
        let payload_health = if deep {
            payload_health_detail(conn, storage_root, "all", None, true, 20, &gc_config)
                .await
                .expect("load aggregate payload health")
        } else {
            payload_health_summary(conn, storage_root, "all", None, &gc_config)
                .await
                .expect("load aggregate payload summary")
        };
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
    async fn shallow_status_marks_the_skipped_payload_census_partial() {
        let (_database_dir, conn) = test_lcm_connection().await;
        let storage = TempDir::new().expect("storage tempdir");
        seed_provider(&conn, 0).await;

        let status =
            aggregate_provider_status(&*conn, storage.path(), None, false, &LcmGcConfig::default())
                .await
                .expect("load shallow aggregate status");
        let payload = serde_json::to_value(status).expect("status json");

        assert_eq!(payload["payload"]["coverage"]["state"], "partial");
        assert_eq!(
            payload["payload"]["coverage"]["reason"],
            "payload_file_census_requires_deep_status"
        );
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
        assert_eq!(
            status.token_estimate,
            LcmStoreTokenCoverage::complete(10001)
        );
    }

    #[tokio::test]
    async fn store_status_bounds_the_total_message_bytes_scanned() {
        const ROWS: i64 = 40;
        let (_database_dir, conn) = test_lcm_connection().await;
        conn.execute(
            "INSERT INTO sessions(provider, session_id, project_key, project_path)
             VALUES ('cursor', 'session-byte-budget', '/project', '/project')",
            (),
        )
        .await
        .unwrap();
        let content = "x".repeat(1024 * 1024);
        for ordinal in 1..=ROWS {
            conn.execute(
                "INSERT INTO lcm_raw_messages (
                    provider, message_id, session_id, role, ordinal, timestamp,
                    content, content_hash, storage_kind, payload_ref, snippet_text,
                    index_text, legacy_source, legacy_truncated, metadata_json
                 ) VALUES (
                    'cursor', ?1, 'session-byte-budget', 'assistant', ?2, ?2,
                    ?3, ?4, 'inline', NULL, ?3, ?3, 0, 0, NULL
                 )",
                params![
                    format!("byte-budget-message-{ordinal}"),
                    ordinal,
                    &content,
                    format!("byte-budget-hash-{ordinal}")
                ],
            )
            .await
            .unwrap();
        }

        let status = store_status(&*conn, "cursor", None).await.unwrap();

        assert_eq!(status.messages, ROWS);
        assert!(
            !status.token_estimate.complete,
            "a status call must not read every body after crossing its byte budget: {:?}",
            status.token_estimate
        );
        assert!(
            status.token_estimate.scanned_messages < ROWS,
            "the byte bound must stop before the complete store is materialized"
        );
        assert!(status.token_estimate.next_after_store_id.is_some());
    }

    async fn seed_raw_messages(conn: &Connection, session_id: &str, rows: i64) {
        conn.execute(
            "INSERT INTO sessions(provider, session_id, project_key, project_path)
             VALUES ('cursor', ?1, '/project', '/project')",
            params![session_id],
        )
        .await
        .unwrap();
        conn.execute(
            &format!(
                "WITH RECURSIVE fixture(value) AS (
                     SELECT 1
                     UNION ALL
                     SELECT value + 1 FROM fixture WHERE value < {rows}
                 )
                 INSERT INTO lcm_raw_messages (
                     provider, message_id, session_id, role, ordinal, timestamp,
                     content, content_hash, storage_kind, payload_ref, snippet_text,
                     index_text, legacy_source, legacy_truncated, metadata_json
                 )
                 SELECT 'cursor', printf('message-%05d', value), ?1,
                        'assistant', value, value, 'one token',
                        printf('hash-%05d', value), 'inline', NULL, 'one token',
                        'one token', 0, 0, NULL
                 FROM fixture"
            ),
            params![session_id],
        )
        .await
        .unwrap();
    }

    /// The token estimate must not stream the whole raw store. Past the scan
    /// budget the status reports the exact message count with a typed partial
    /// estimate and a resume cursor — never a truncated total presented as the
    /// whole store.
    #[tokio::test]
    async fn store_status_reports_a_partial_token_estimate_beyond_the_scan_budget() {
        const ROWS: i64 = 900;
        const BUDGET: i64 = 512;
        let (_database_dir, conn) = test_lcm_connection().await;
        seed_raw_messages(&conn, "session-budgeted-status", ROWS).await;

        let status = store_status_within(&*conn, "cursor", None, BUDGET)
            .await
            .unwrap();

        assert_eq!(
            status.messages, ROWS,
            "the reported store size must stay exact"
        );
        assert!(
            !status.token_estimate.complete,
            "an estimate that skipped rows must not be reported as complete: {:?}",
            status.token_estimate
        );
        assert_eq!(status.token_estimate.scanned_messages, BUDGET);
        assert_eq!(
            status.estimated_tokens,
            BUDGET * 2,
            "the estimate must cover exactly the scanned prefix"
        );
        let cursor = status
            .token_estimate
            .next_after_store_id
            .expect("a partial estimate must carry a resume cursor");
        assert!(cursor > 0, "resume cursor must address a real store id");
    }

    /// A store inside the budget is fully covered, and the coverage says so.
    #[tokio::test]
    async fn store_status_reports_a_complete_token_estimate_within_the_scan_budget() {
        const ROWS: i64 = 300;
        let (_database_dir, conn) = test_lcm_connection().await;
        seed_raw_messages(&conn, "session-complete-status", ROWS).await;

        let status = store_status_within(&*conn, "cursor", None, 512)
            .await
            .unwrap();

        assert_eq!(status.messages, ROWS);
        assert_eq!(status.estimated_tokens, ROWS * 2);
        assert_eq!(status.token_estimate, LcmStoreTokenCoverage::complete(ROWS));
    }

    /// The bound has to hold on the production entry point too, not only on the
    /// test-visible inner function.
    #[tokio::test]
    async fn store_status_applies_the_production_scan_budget() {
        let (_database_dir, conn) = test_lcm_connection().await;
        seed_raw_messages(&conn, "session-production-budget", 600).await;

        let status = store_status(&*conn, "cursor", None).await.unwrap();

        assert!(
            status.token_estimate.scanned_messages <= STORE_STATUS_TOKEN_SCAN_BUDGET,
            "the production entry point must respect its own budget: {:?}",
            status.token_estimate
        );
        assert_eq!(status.messages, 600);
    }

    #[tokio::test]
    async fn payload_health_pages_more_rows_than_runtime_materialization_limit() {
        const ROWS: i64 = 10_001;
        let (_database_dir, conn) = test_lcm_connection().await;
        let storage = TempDir::new().expect("storage tempdir");
        conn.execute(
            "INSERT INTO sessions(provider, session_id, project_key, project_path)
             VALUES ('cursor', 'session-paged-payloads', '/project', '/project')",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            &format!(
                "WITH RECURSIVE fixture(value) AS (
                     SELECT 1
                     UNION ALL
                     SELECT value + 1 FROM fixture WHERE value < {ROWS}
                 )
                 INSERT INTO lcm_raw_messages (
                     provider, message_id, session_id, role, ordinal, timestamp,
                     content, content_hash, storage_kind, payload_ref, snippet_text,
                     index_text, legacy_source, legacy_truncated, metadata_json
                 )
                 SELECT 'cursor', printf('message-%05d', value), 'session-paged-payloads',
                        'assistant', value, value, 'one token',
                        printf('hash-%05d', value), 'external',
                        printf('payload-%05d', value), 'one token',
                        'one token', 0, 0, NULL
                 FROM fixture"
            ),
            (),
        )
        .await
        .unwrap();
        conn.execute(
            &format!(
                "WITH RECURSIVE fixture(value) AS (
                     SELECT 1
                     UNION ALL
                     SELECT value + 1 FROM fixture WHERE value < {ROWS}
                 )
                 INSERT INTO lcm_external_payloads (
                     payload_ref, provider, session_id, message_id, kind,
                     content_hash, byte_count, char_count
                 )
                 SELECT printf('payload-%05d', value), 'cursor', 'session-paged-payloads',
                        printf('message-%05d', value), 'tool_output',
                        printf('hash-%05d', value), 16, 16
                 FROM fixture"
            ),
            (),
        )
        .await
        .unwrap();

        let (status, _work) = aggregate_provider_status_with_work(
            &*conn,
            storage.path(),
            None,
            false,
            &LcmGcConfig::default(),
        )
        .await
        .expect("aggregate status must page instead of exceeding the materialization limit");

        assert_eq!(status.raw_message_count, ROWS);
        assert_eq!(status.external_payload_count, ROWS);
        // Every reference is live, so nothing is reported as unreferenced.
        assert_eq!(status.unreferenced_payload_count, 0);
    }
}
