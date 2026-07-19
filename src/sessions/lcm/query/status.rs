use std::path::Path;

use super::*;

pub(super) async fn status_for_provider(
    conn: &Connection,
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
    deep: bool,
    gc_config: &LcmGcConfig,
) -> Result<LcmStatus, LcmError> {
    let schema_version = schema::schema_version(conn)
        .await
        .unwrap_or(LCM_SCHEMA_VERSION);
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
    let external_payload_count = payload_health.payload.externalized_count;
    let missing_payload_count = payload_health.payload.missing_count;
    let unreferenced_payload_count = payload_health.payload.unreferenced_count;
    let maintenance_debt_count =
        compression::maintenance_debt_count(conn, provider, session_id).await?;
    let lifecycle_state_count =
        count_lifecycle_states_for_current_session(conn, provider, session_id).await?;
    let frontier_count = count_frontier_rows(conn, provider, session_id).await?;
    let lifecycle_metadata = load_lifecycle_metadata(conn, provider, session_id).await?;
    let legacy_truncated_count = count_legacy_truncated(conn, provider, session_id).await?;
    let lossy_ingest_records = count_lossy_ingest_records(conn, provider, session_id).await?;
    let lossy_records = legacy_truncated_count + lossy_ingest_records;
    let store = store_status(conn, provider, session_id).await?;
    let dag = dag_status(conn, provider, session_id).await?;

    Ok(LcmStatus {
        schema_version,
        raw_message_count: count_raw_messages(conn, provider, session_id).await?,
        summary_node_count: count_summary_nodes(conn, provider, session_id).await?,
        external_payload_count,
        missing_payload_count,
        unreferenced_payload_count,
        maintenance_debt_count,
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
            lifecycle_state_count,
            frontier_count,
            maintenance_debt_count,
            current_session_id: lifecycle_metadata.current_session_id,
            current_frontier_store_id: lifecycle_metadata.current_frontier_store_id,
            last_finalized_session_id: lifecycle_metadata.last_finalized_session_id,
            last_finalized_frontier_store_id: lifecycle_metadata.last_finalized_frontier_store_id,
        },
        redaction: LcmRedactionStatus {
            enabled: lossy_records > 0,
            lossy_records,
            legacy_truncated_count,
        },
    })
}

pub(super) async fn aggregate_provider_status(
    conn: &Connection,
    storage_root: &Path,
    session_id: Option<&str>,
    deep: bool,
    gc_config: &LcmGcConfig,
) -> Result<LcmStatus, LcmError> {
    let schema_version = schema::schema_version(conn)
        .await
        .unwrap_or(LCM_SCHEMA_VERSION);
    let providers = lcm_status_providers(conn, session_id).await?;
    if providers.is_empty() {
        return Ok(empty_status(schema_version, gc_config));
    }

    let mut aggregate = empty_status(schema_version, gc_config);
    for provider in providers {
        let status =
            status_for_provider(conn, storage_root, &provider, session_id, deep, gc_config).await?;
        merge_lcm_status(&mut aggregate, status);
    }
    let payload_health =
        payload_health_detail(conn, storage_root, "all", session_id, deep, 20, gc_config).await?;
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
    Ok(aggregate)
}

async fn lcm_status_providers(
    conn: &Connection,
    session_id: Option<&str>,
) -> Result<Vec<String>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT DISTINCT provider
             FROM (
                 SELECT provider, session_id FROM lcm_raw_messages
                 UNION
                 SELECT provider, session_id FROM lcm_summary_nodes
                 UNION
                 SELECT provider, session_id FROM lcm_external_payloads
                 UNION
                 SELECT provider, current_session_id AS session_id FROM lcm_lifecycle_state
             )
             WHERE (?1 IS NULL OR session_id = ?1)
             ORDER BY provider",
            params![util::opt_text(session_id)],
        )
        .await?;
    let mut providers = Vec::new();
    while let Some(row) = rows.next().await? {
        providers.push(row.get(0)?);
    }
    Ok(providers)
}

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

fn max_option_i64(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn min_option_i64(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn sum_option_i64(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left + right),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn max_option_u64(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

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
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<LcmStoreStatus, LcmError> {
    let mut rows = conn
        .query(
            "SELECT content, snippet_text
             FROM lcm_raw_messages
             WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)",
            params![provider, util::opt_text(session_id)],
        )
        .await?;
    let mut messages = 0_i64;
    let mut estimated_tokens = 0_i64;
    while let Some(row) = rows.next().await? {
        messages += 1;
        let content: Option<String> = row.get(0)?;
        let snippet: String = row.get(1)?;
        // Externalized rows count their inline placeholder, matching what the
        // engine replays into active context.
        let text = content.unwrap_or(snippet);
        estimated_tokens += estimate_tokens(&text);
    }
    Ok(LcmStoreStatus {
        messages,
        estimated_tokens,
    })
}

async fn dag_status(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<LcmDagStatus, LcmError> {
    let mut rows = conn
        .query(
            "SELECT depth, COUNT(*), SUM(summary_token_count), SUM(source_token_count)
             FROM lcm_summary_nodes
             WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)
             GROUP BY depth
             ORDER BY depth",
            params![provider, util::opt_text(session_id)],
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
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<LcmLifecycleMetadata, LcmError> {
    let session_value = util::opt_text(session_id);
    let mut rows = conn
        .query(
            "SELECT current_session_id, current_frontier_store_id,
                    last_finalized_session_id, last_finalized_frontier_store_id
             FROM lcm_lifecycle_state
             WHERE provider = ?1 AND (?2 IS NULL OR current_session_id = ?2)
             ORDER BY updated_at DESC, conversation_id DESC
             LIMIT 1",
            params![provider, session_value],
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
struct LcmLifecycleMetadata {
    current_session_id: Option<String>,
    current_frontier_store_id: Option<i64>,
    last_finalized_session_id: Option<String>,
    last_finalized_frontier_store_id: Option<i64>,
}

async fn count_frontier_rows(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    util::fetch_i64(
        conn,
        "SELECT COUNT(*)
             FROM lcm_lifecycle_state
             WHERE provider = ?1
               AND (?2 IS NULL OR current_session_id = ?2)
               AND current_frontier_store_id IS NOT NULL",
        params![provider, util::opt_text(session_id)],
        "frontier count query returned no rows",
    )
    .await
}

async fn count_legacy_truncated(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    util::fetch_i64(
        conn,
        "SELECT COUNT(*)
             FROM lcm_raw_messages
             WHERE provider = ?1
               AND (?2 IS NULL OR session_id = ?2)
               AND legacy_truncated != 0",
        params![provider, util::opt_text(session_id)],
        "legacy truncated count query returned no rows",
    )
    .await
}

/// SQL pushdown of the former Rust-side metadata scan. Semantics are pinned
/// to the old `serde_json` reader, which counted a row only when
/// `metadata_json.ingest_protection.lossy` was the JSON *boolean* `true`
/// (`Value::as_bool`): `json_type(...) = 'true'` matches exactly — a numeric
/// `1` reports `'integer'` and stays not-lossy (the Rust writer in
/// `raw::add_ingest_protection_metadata` only ever stores `json!(true)`),
/// invalid JSON is screened out by `json_valid` (`SQLite` `AND` short-circuits
/// left-to-right, so `json_type` never raises on malformed text), and a
/// missing key or non-object metadata yields `NULL`, which is not `'true'`.
async fn count_lossy_ingest_records(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    util::fetch_i64(
        conn,
        "SELECT COUNT(*)
             FROM lcm_raw_messages
             WHERE provider = ?1
               AND (?2 IS NULL OR session_id = ?2)
               AND metadata_json IS NOT NULL
               AND json_valid(metadata_json)
               AND json_type(metadata_json, '$.ingest_protection.lossy') = 'true'",
        params![provider, util::opt_text(session_id)],
        "lossy ingest count query returned no rows",
    )
    .await
}
