use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{Value, json};

use tracedecay_runtime_core::db::engine::{QueryExecutor, params};
use tracedecay_runtime_core::tracedecay::current_timestamp;

use super::{
    LCM_SCHEMA_VERSION, LcmError, LcmGcConfig, LcmNoiseClassificationConfig, query, schema,
    security, util,
};

const MAX_SAMPLES: usize = 20;
const RETENTION_OLD_DAYS: f64 = 30.0;
const RETENTION_HEAVY_CHARS: i64 = 128 * 1024;
pub struct DoctorRequest<'a> {
    pub storage_root: &'a Path,
    pub provider: &'a str,
    pub session_id: Option<&'a str>,
}

pub async fn doctor(
    conn: &(impl QueryExecutor + ?Sized),
    request: DoctorRequest<'_>,
) -> Result<Value, LcmError> {
    let diagnostics = gather_diagnostics(
        conn,
        request.storage_root,
        request.provider,
        request.session_id,
    )
    .await?;
    let observed = diagnostics["raw_message_count"]
        .as_i64()
        .unwrap_or_default()
        + diagnostics["summary_node_count"]
            .as_i64()
            .unwrap_or_default()
        + diagnostics["external_payload_count"]
            .as_i64()
            .unwrap_or_default();
    let (status, coverage) = if observed == 0 {
        ("absent", "absent")
    } else if issue_count(&diagnostics) > 0 {
        ("issues_found", "complete")
    } else {
        ("ok", "complete")
    };

    Ok(json!({
        "status": status,
        "provider": request.provider,
        "session_id": request.session_id,
        "coverage": {
            "state": coverage,
            "observed_records": observed,
        },
        "diagnostics": diagnostics,
    }))
}

async fn gather_diagnostics(
    conn: &(impl QueryExecutor + ?Sized),
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
) -> Result<Value, LcmError> {
    let schema_version = schema::schema_version(conn).await;
    let raw_message_count =
        util::count_by_provider_session(conn, "lcm_raw_messages", provider, session_id).await?;
    let summary_node_count =
        util::count_by_provider_session(conn, "lcm_summary_nodes", provider, session_id).await?;
    let external_payload_count =
        util::count_by_provider_session(conn, "lcm_external_payloads", provider, session_id)
            .await?;
    let payloads = payload_diagnostics(
        conn,
        storage_root,
        provider,
        session_id,
        &LcmGcConfig::default(),
    )
    .await?;
    let fts = fts_diagnostics(conn, provider, session_id).await?;
    let summaries = summary_integrity(conn, provider, session_id).await?;
    let lifecycle = lifecycle_integrity(conn, provider, session_id).await?;
    let retention = retention_candidates(conn, provider, session_id).await?;
    let noise_candidates = noise_candidates(
        conn,
        provider,
        session_id,
        &LcmNoiseClassificationConfig::default(),
    )
    .await?;

    Ok(json!({
        "schema": {
            "present": schema_version.is_some(),
            "observed_version": schema_version,
            "expected_version": LCM_SCHEMA_VERSION,
            "current": schema_version == Some(LCM_SCHEMA_VERSION),
        },
        "raw_message_count": raw_message_count,
        "summary_node_count": summary_node_count,
        "external_payload_count": external_payload_count,
        "payloads": payloads,
        "fts": fts,
        "summaries": summaries,
        "lifecycle": lifecycle,
        "retention": retention,
        "noise_candidates": noise_candidates,
    }))
}

fn issue_count(diagnostics: &Value) -> i64 {
    let schema_issues = i64::from(!diagnostics["schema"]["current"].as_bool().unwrap_or(false));
    schema_issues
        + diagnostics["payloads"]["missing_files"]
            .as_i64()
            .unwrap_or(0)
        + diagnostics["payloads"]["orphan_files"]
            .as_i64()
            .unwrap_or(0)
        + diagnostics["payloads"]["unreferenced_metadata"]
            .as_i64()
            .unwrap_or(0)
        + diagnostics["payloads"]["missing_placeholder_metadata"]
            .as_i64()
            .unwrap_or(0)
        + diagnostics["payloads"]["missing_placeholder_files"]
            .as_i64()
            .unwrap_or(0)
        + i64::from(
            diagnostics["fts"]["rebuild_needed"]
                .as_bool()
                .unwrap_or(false),
        )
        + diagnostics["summaries"]["broken_sources"]
            .as_i64()
            .unwrap_or(0)
        + diagnostics["summaries"]["hash_mismatches"]
            .as_i64()
            .unwrap_or(0)
        + diagnostics["lifecycle"]["invalid_frontiers"]
            .as_i64()
            .unwrap_or(0)
        + diagnostics["lifecycle"]["orphan_debt"]
            .as_i64()
            .unwrap_or(0)
        + diagnostics["noise_candidates"]["candidate_count"]
            .as_i64()
            .unwrap_or(0)
}

async fn table_or_trigger_count(
    conn: &(impl QueryExecutor + ?Sized),
    names: &[&str],
    object_type: &str,
) -> Result<i64, LcmError> {
    let mut found = 0;
    for name in names {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = ?1 AND name = ?2",
                params![object_type, *name],
            )
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or_else(|| LcmError::Db("sqlite_master query returned no rows".to_string()))?;
        let count: i64 = row.get(0)?;
        if count > 0 {
            found += 1;
        }
    }
    Ok(found)
}

async fn payload_diagnostics(
    conn: &(impl QueryExecutor + ?Sized),
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
    gc_config: &LcmGcConfig,
) -> Result<Value, LcmError> {
    let detail = query::payload_health_detail(
        conn,
        storage_root,
        provider,
        session_id,
        false,
        MAX_SAMPLES,
        gc_config,
    )
    .await?;
    let orphan_refs = detail
        .orphan_files
        .iter()
        .map(|sample| sample.payload_ref.clone())
        .collect::<Vec<_>>();

    Ok(json!({
        "missing_files": detail.payload.missing_count,
        "missing_payload_refs": detail.missing_payload_refs,
        "orphan_files": detail.payload.orphan_file_count,
        "orphan_payload_refs": orphan_refs,
        "unreferenced_metadata": detail.payload.unreferenced_count,
        "placeholder_refs_total": detail.payload.placeholder_ref_count,
        "missing_placeholder_metadata": detail.payload.missing_placeholder_metadata_count,
        "missing_placeholder_files": detail.payload.missing_placeholder_file_count,
        "missing_placeholder_refs": detail.missing_placeholder_refs,
        "gc_candidate_files": detail.payload.orphan_file_count,
        "gc_candidate_payload_refs": orphan_refs,
        "total_bytes": detail.payload.total_bytes,
        "referenced_bytes": detail.payload.referenced_bytes,
        "orphan_file_bytes": detail.payload.orphan_file_bytes,
        "reclaimable_bytes": detail.payload.reclaimable_bytes,
        "reclaimable_bytes_after_grace": detail.payload.reclaimable_bytes_after_grace,
        "integrity_mismatch_count": detail.payload.integrity_mismatch_count,
        "integrity_mismatch_refs": detail.integrity_mismatch_refs,
        "last_gc_status": detail.payload_gc.last_gc_status,
        "last_gc_error": detail.payload_gc.last_gc_error,
    }))
}

async fn fts_diagnostics(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<Value, LcmError> {
    let raw_table_present =
        table_or_trigger_count(conn, &["lcm_raw_messages_fts"], "table").await? == 1;
    let summary_table_present =
        table_or_trigger_count(conn, &["lcm_summary_nodes_fts"], "table").await? == 1;
    let raw_trigger_count = table_or_trigger_count(
        conn,
        &[
            "lcm_raw_messages_fts_insert",
            "lcm_raw_messages_fts_delete",
            "lcm_raw_messages_fts_update",
        ],
        "trigger",
    )
    .await?;
    let summary_trigger_count = table_or_trigger_count(
        conn,
        &[
            "lcm_summary_nodes_fts_insert",
            "lcm_summary_nodes_fts_delete",
            "lcm_summary_nodes_fts_update",
        ],
        "trigger",
    )
    .await?;
    // Pre-v3 FTS objects still index role/metadata_json and over-match
    // unqualified grep queries; treat that stale structure as rebuild-needed.
    let raw_structure_current = schema::raw_fts_structure_is_current(conn)
        .await
        .unwrap_or(false);
    let raw_rebuild_needed = !raw_table_present
        || raw_trigger_count < 3
        || !raw_structure_current
        || fts_probe_needs_rebuild(
            conn,
            "lcm_raw_messages",
            "lcm_raw_messages_fts",
            "index_text",
            provider,
            session_id,
        )
        .await?;
    let summary_rebuild_needed = !summary_table_present
        || summary_trigger_count < 3
        || fts_probe_needs_rebuild(
            conn,
            "lcm_summary_nodes",
            "lcm_summary_nodes_fts",
            "summary_text",
            provider,
            session_id,
        )
        .await?;

    Ok(json!({
        "rebuild_needed": raw_rebuild_needed || summary_rebuild_needed,
        "raw": {
            "table_present": raw_table_present,
            "trigger_count": raw_trigger_count,
            "structure_current": raw_structure_current,
            "rebuild_needed": raw_rebuild_needed,
        },
        "summaries": {
            "table_present": summary_table_present,
            "trigger_count": summary_trigger_count,
            "rebuild_needed": summary_rebuild_needed,
        },
    }))
}

async fn fts_probe_needs_rebuild(
    conn: &(impl QueryExecutor + ?Sized),
    content_table: &str,
    fts_table: &str,
    text_column: &str,
    provider: &str,
    session_id: Option<&str>,
) -> Result<bool, LcmError> {
    let sql = format!(
        "SELECT {text_column}
         FROM {content_table}
         WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)
         LIMIT 20"
    );
    let mut rows = conn
        .query(&sql, params![provider, util::opt_text(session_id)])
        .await?;
    while let Some(row) = rows.next().await? {
        let text: String = row.get(0)?;
        let Some(term) = first_fts_term(&text) else {
            continue;
        };
        let match_sql = format!(
            "SELECT COUNT(*)
             FROM {fts_table}
             JOIN {content_table} content ON content.rowid = {fts_table}.rowid
             WHERE {fts_table} MATCH ?1
               AND content.provider = ?2
               AND (?3 IS NULL OR content.session_id = ?3)"
        );
        let Ok(mut match_rows) = conn
            .query(
                &match_sql,
                params![term, provider, util::opt_text(session_id)],
            )
            .await
        else {
            return Ok(true);
        };
        let row = match_rows
            .next()
            .await?
            .ok_or_else(|| LcmError::Db("FTS probe returned no rows".to_string()))?;
        let count: i64 = row.get(0)?;
        return Ok(count == 0);
    }
    Ok(false)
}

fn first_fts_term(text: &str) -> Option<String> {
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            current.push(ch);
        } else if current.len() >= 2 {
            return Some(current);
        } else {
            current.clear();
        }
    }
    (current.len() >= 2).then_some(current)
}

async fn summary_integrity(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<Value, LcmError> {
    let broken_sources = count_broken_summary_sources(conn, provider, session_id).await?;
    let hash_mismatches = count_summary_hash_mismatches(conn, provider, session_id).await?;
    Ok(json!({
        "broken_sources": broken_sources,
        "hash_mismatches": hash_mismatches,
    }))
}

async fn count_broken_summary_sources(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let mut rows = conn
        .query(
            "SELECT COUNT(*)
             FROM lcm_summary_sources src
             LEFT JOIN lcm_summary_nodes owner ON owner.node_id = src.node_id
             LEFT JOIN lcm_raw_messages raw
               ON src.source_kind = 'raw_message'
              AND CAST(raw.store_id AS TEXT) = src.source_id
             LEFT JOIN lcm_summary_nodes child
               ON src.source_kind = 'summary_node'
              AND child.node_id = src.source_id
             WHERE (
                    owner.provider = ?1
                AND (?2 IS NULL OR owner.session_id = ?2)
                AND (
                       (src.source_kind = 'raw_message'
                        AND (
                              raw.store_id IS NULL
                           OR raw.provider != owner.provider
                           OR raw.session_id != owner.session_id
                        ))
                    OR (src.source_kind = 'summary_node'
                        AND (
                              child.node_id IS NULL
                           OR child.provider != owner.provider
                           OR child.session_id != owner.session_id
                        ))
                )
             )
             OR (
                    owner.node_id IS NULL
                AND (
                       (src.source_kind = 'raw_message'
                        AND raw.provider = ?1
                        AND (?2 IS NULL OR raw.session_id = ?2))
                    OR (src.source_kind = 'summary_node'
                        AND child.provider = ?1
                        AND (?2 IS NULL OR child.session_id = ?2))
                )
             )",
            params![provider, util::opt_text(session_id)],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| LcmError::Db("summary source count returned no rows".to_string()))?;
    row.get(0).map_err(|err| LcmError::Db(err.to_string()))
}

async fn count_summary_hash_mismatches(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let mut rows = conn
        .query(
            "SELECT summary_text, summary_hash
             FROM lcm_summary_nodes
             WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)",
            params![provider, util::opt_text(session_id)],
        )
        .await?;
    let mut mismatches = 0;
    while let Some(row) = rows.next().await? {
        let text: String = row.get(0)?;
        let hash: String = row.get(1)?;
        if util::sha256_hex(text.as_bytes()) != hash {
            mismatches += 1;
        }
    }
    Ok(mismatches)
}

async fn lifecycle_integrity(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<Value, LcmError> {
    let lifecycle_state_count =
        count_lifecycle_states_for_session_scope(conn, provider, session_id).await?;
    let invalid_frontiers = count_invalid_frontiers(conn, provider, session_id).await?;
    let orphan_debt = count_orphan_debt(conn, provider, session_id).await?;
    Ok(json!({
        "lifecycle_state_count": lifecycle_state_count,
        "invalid_frontiers": invalid_frontiers,
        "orphan_debt": orphan_debt,
    }))
}

async fn count_lifecycle_states_for_session_scope(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    util::fetch_i64(
        conn,
        "SELECT COUNT(*)
             FROM lcm_lifecycle_state
             WHERE provider = ?1
               AND (?2 IS NULL OR current_session_id = ?2 OR last_finalized_session_id = ?2)",
        params![provider, util::opt_text(session_id)],
        "lifecycle count returned no rows",
    )
    .await
}

async fn count_invalid_frontiers(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let mut rows = conn
        .query(
            "SELECT COUNT(*)
             FROM lcm_lifecycle_state state
             LEFT JOIN lcm_raw_messages raw
               ON raw.provider = state.provider
              AND raw.session_id = state.current_session_id
              AND raw.store_id = state.current_frontier_store_id
             WHERE state.provider = ?1
               AND (?2 IS NULL OR state.current_session_id = ?2)
               AND state.current_frontier_store_id IS NOT NULL
               AND raw.store_id IS NULL",
            params![provider, util::opt_text(session_id)],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| LcmError::Db("frontier count returned no rows".to_string()))?;
    row.get(0).map_err(|err| LcmError::Db(err.to_string()))
}

async fn count_orphan_debt(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let mut rows = conn
        .query(
            "SELECT COUNT(*)
             FROM lcm_maintenance_debt debt
             LEFT JOIN lcm_lifecycle_state state
               ON state.provider = debt.provider
              AND state.conversation_id = debt.conversation_id
             WHERE debt.provider = ?1
               AND (?2 IS NULL OR debt.conversation_id = ?2)
               AND state.conversation_id IS NULL",
            params![provider, util::opt_text(session_id)],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| LcmError::Db("debt count returned no rows".to_string()))?;
    row.get(0).map_err(|err| LcmError::Db(err.to_string()))
}

async fn retention_candidates(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<Value, LcmError> {
    let now = current_timestamp();
    let mut rows = conn
        .query(
            "SELECT raw.session_id,
                    raw.message_count,
                    raw.retained_chars,
                    raw.first_message_at,
                    raw.last_message_at,
                    COALESCE(summary_counts.summary_node_count, 0) AS summary_node_count
             FROM (
                SELECT session_id,
                       COUNT(*) AS message_count,
                       COALESCE(SUM(LENGTH(index_text)), 0) AS retained_chars,
                       MIN(COALESCE(timestamp, 0)) AS first_message_at,
                       MAX(COALESCE(timestamp, 0)) AS last_message_at
                FROM lcm_raw_messages
                WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)
                GROUP BY session_id
             ) raw
             LEFT JOIN (
                SELECT session_id, COUNT(*) AS summary_node_count
                FROM lcm_summary_nodes
                WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)
                GROUP BY session_id
             ) summary_counts ON summary_counts.session_id = raw.session_id
             ORDER BY raw.retained_chars DESC, raw.last_message_at ASC
             LIMIT 100",
            params![provider, util::opt_text(session_id)],
        )
        .await?;
    let mut candidates = Vec::new();
    let mut analyzed = 0;
    while let Some(row) = rows.next().await? {
        analyzed += 1;
        let session_id: String = row.get(0)?;
        let message_count: i64 = row.get(1)?;
        let retained_chars: i64 = row.get(2)?;
        let first_message_at: i64 = row.get(3)?;
        let last_message_at: i64 = row.get(4)?;
        let summary_node_count: i64 = row.get(5)?;
        let age_days = if last_message_at > 0 {
            (now.saturating_sub(last_message_at) as f64) / 86_400.0
        } else {
            0.0
        };
        let old = age_days >= RETENTION_OLD_DAYS;
        let heavy = retained_chars >= RETENTION_HEAVY_CHARS;
        let session_only = summary_node_count == 0;
        if old || heavy || session_only {
            candidates.push(json!({
                "session_id": session_id,
                "message_count": message_count,
                "retained_chars": retained_chars,
                "first_message_at": first_message_at,
                "last_message_at": last_message_at,
                "age_days": age_days,
                "old": old,
                "heavy": heavy,
                "session_only": session_only,
                "protected": false,
            }));
        }
        if candidates.len() >= MAX_SAMPLES {
            break;
        }
    }

    Ok(json!({
        "read_only": true,
        "sessions_analyzed": analyzed,
        "candidate_count": candidates.len(),
        "candidates": candidates,
    }))
}

#[derive(Default)]
struct CleanupSessionCandidate {
    classes: BTreeSet<&'static str>,
    message_count: i64,
    summary_node_count: i64,
}

async fn noise_candidates(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
    classification: &LcmNoiseClassificationConfig,
) -> Result<Value, LcmError> {
    let ignore_session_patterns =
        security::compile_session_patterns(&classification.ignore_session_patterns);
    let stateless_session_patterns =
        security::compile_session_patterns(&classification.stateless_session_patterns);
    let ignore_message_patterns =
        security::compile_message_patterns(&classification.ignore_message_patterns);
    let summary_counts = summary_counts_by_session(conn, provider, session_id).await?;
    let protected_raw_sources =
        raw_store_ids_with_summary_sources(conn, provider, session_id).await?;

    let mut rows = conn
        .query(
            "SELECT store_id, message_id, session_id, role, COALESCE(content, index_text, '')
             FROM lcm_raw_messages
             WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)
             ORDER BY session_id, store_id
             LIMIT 5000",
            params![provider, util::opt_text(session_id)],
        )
        .await?;

    let mut sessions = BTreeMap::<String, CleanupSessionCandidate>::new();
    let mut message_candidates = Vec::new();
    let mut heartbeat_message_candidates = Vec::new();
    let mut ignored_session_count = 0_i64;
    let mut stateless_session_count = 0_i64;
    let mut noise_message_count = 0_i64;
    let mut heartbeat_message_count = 0_i64;
    let mut protected_noise_count = 0_i64;

    while let Some(row) = rows.next().await? {
        let store_id: i64 = row.get(0)?;
        let message_id: String = row.get(1)?;
        let row_session_id: String = row.get(2)?;
        let role: String = row.get(3)?;
        let content: String = row.get(4).unwrap_or_default();

        let ignored =
            security::matches_any_compiled_pattern(&ignore_session_patterns, &row_session_id);
        let stateless =
            security::matches_any_compiled_pattern(&stateless_session_patterns, &row_session_id);
        if ignored || stateless {
            let is_new = !sessions.contains_key(&row_session_id);
            let candidate = sessions.entry(row_session_id.clone()).or_default();
            candidate.message_count += 1;
            if is_new {
                candidate.summary_node_count = summary_counts
                    .get(&row_session_id)
                    .copied()
                    .unwrap_or_default();
            }
            if ignored {
                candidate.classes.insert("ignored_session");
            }
            if stateless {
                candidate.classes.insert("stateless_session");
            }
            continue;
        }

        if let Some(reason) = security::heartbeat_noise_reason(&role, &content) {
            heartbeat_message_count += 1;
            if heartbeat_message_candidates.len() < MAX_SAMPLES {
                heartbeat_message_candidates.push(json!({
                    "store_id": store_id,
                    "message_id": message_id.clone(),
                    "session_id": row_session_id.clone(),
                    "role": role.clone(),
                    "reason": reason,
                }));
            }
        }

        let Some(reason) =
            security::ignore_message_reason_with_compiled(&content, &ignore_message_patterns)
        else {
            continue;
        };
        if protected_raw_sources.contains(&store_id) {
            protected_noise_count += 1;
            continue;
        }
        noise_message_count += 1;
        if message_candidates.len() < MAX_SAMPLES {
            message_candidates.push(json!({
                "store_id": store_id,
                "message_id": message_id,
                "session_id": row_session_id,
                "role": role,
                "reason": reason,
            }));
        }
    }

    let session_candidates = sessions
        .iter()
        .take(MAX_SAMPLES)
        .map(|(session_id, candidate)| {
            let classes = candidate.classes.iter().copied().collect::<Vec<_>>();
            json!({
                "session_id": session_id,
                "classes": classes,
                "message_count": candidate.message_count,
                "summary_node_count": candidate.summary_node_count,
            })
        })
        .collect::<Vec<_>>();
    for candidate in sessions.values() {
        if candidate.classes.contains("ignored_session") {
            ignored_session_count += 1;
        }
        if candidate.classes.contains("stateless_session") {
            stateless_session_count += 1;
        }
    }

    Ok(json!({
        "read_only": true,
        "candidate_count": sessions.len() as i64 + noise_message_count,
        "ignored_session_candidates": ignored_session_count,
        "stateless_session_candidates": stateless_session_count,
        "noise_message_candidates": noise_message_count,
        "heartbeat_noise_message_candidates": heartbeat_message_count,
        "protected_noise_messages_skipped": protected_noise_count,
        "session_candidates": session_candidates,
        "message_candidates": message_candidates,
        "heartbeat_message_candidates": heartbeat_message_candidates,
    }))
}

async fn summary_counts_by_session(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<BTreeMap<String, i64>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT session_id, COUNT(*)
             FROM lcm_summary_nodes
             WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)
             GROUP BY session_id",
            params![provider, util::opt_text(session_id)],
        )
        .await?;
    let mut counts = BTreeMap::new();
    while let Some(row) = rows.next().await? {
        let session_id: String = row.get(0)?;
        let count: i64 = row.get(1)?;
        counts.insert(session_id, count);
    }
    Ok(counts)
}

async fn raw_store_ids_with_summary_sources(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<BTreeSet<i64>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT DISTINCT raw.store_id
             FROM lcm_summary_sources src
             JOIN lcm_raw_messages raw
               ON src.source_kind = 'raw_message'
              AND raw.store_id = CAST(src.source_id AS INTEGER)
             WHERE raw.provider = ?1
               AND (?2 IS NULL OR raw.session_id = ?2)",
            params![provider, util::opt_text(session_id)],
        )
        .await?;
    let mut store_ids = BTreeSet::new();
    while let Some(row) = rows.next().await? {
        let store_id: i64 = row.get(0)?;
        store_ids.insert(store_id);
    }
    Ok(store_ids)
}
