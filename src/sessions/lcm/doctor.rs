use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use libsql::{params, Connection, Value as SqlValue};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{payload, schema, LcmError, LCM_SCHEMA_VERSION};

const MAX_SAMPLES: usize = 20;
const RETENTION_OLD_DAYS: f64 = 30.0;
const RETENTION_HEAVY_CHARS: i64 = 128 * 1024;

fn opt_text(value: Option<&str>) -> SqlValue {
    value.map_or(SqlValue::Null, |value| SqlValue::Text(value.to_string()))
}

fn unixepoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

pub(crate) async fn doctor(
    conn: &Connection,
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
    mode: &str,
    apply: bool,
) -> Result<Value, LcmError> {
    let diagnostics = gather_diagnostics(conn, storage_root, provider, session_id).await?;
    let repairs = plan_and_apply_repairs(conn, &diagnostics, mode, apply).await?;
    let issue_count = issue_count(&diagnostics);
    let applied_count = repairs["applied_actions"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_default();
    let status = if applied_count > 0 {
        "repaired"
    } else if issue_count > 0 {
        "issues_found"
    } else {
        "ok"
    };

    Ok(json!({
        "status": status,
        "provider": provider,
        "session_id": session_id,
        "mode": mode,
        "dry_run": mode == "repair" && !apply,
        "apply": apply,
        "diagnostics": diagnostics,
        "repairs": repairs,
    }))
}

async fn gather_diagnostics(
    conn: &Connection,
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
) -> Result<Value, LcmError> {
    let schema_version = schema::schema_version(conn).await;
    let raw_message_count =
        count_provider_session(conn, "lcm_raw_messages", provider, session_id).await?;
    let summary_node_count =
        count_provider_session(conn, "lcm_summary_nodes", provider, session_id).await?;
    let external_payload_count =
        count_provider_session(conn, "lcm_external_payloads", provider, session_id).await?;
    let payloads = payload_diagnostics(conn, storage_root, provider, session_id).await?;
    let fts = fts_diagnostics(conn, provider, session_id).await?;
    let summaries = summary_integrity(conn, provider, session_id).await?;
    let lifecycle = lifecycle_integrity(conn, provider, session_id).await?;
    let retention = retention_candidates(conn, provider, session_id).await?;

    Ok(json!({
        "schema": {
            "migration_present": schema_version.is_some(),
            "schema_version": schema_version,
            "expected_schema_version": LCM_SCHEMA_VERSION,
            "schema_current": schema_version == Some(LCM_SCHEMA_VERSION),
        },
        "raw_message_count": raw_message_count,
        "summary_node_count": summary_node_count,
        "external_payload_count": external_payload_count,
        "payloads": payloads,
        "fts": fts,
        "summaries": summaries,
        "lifecycle": lifecycle,
        "retention": retention,
    }))
}

async fn plan_and_apply_repairs(
    conn: &Connection,
    diagnostics: &Value,
    mode: &str,
    apply: bool,
) -> Result<Value, LcmError> {
    let mut planned = Vec::new();
    let mut applied = Vec::new();
    let raw_rebuild_needed = diagnostics["fts"]["raw"]["rebuild_needed"]
        .as_bool()
        .unwrap_or(false);
    let summary_rebuild_needed = diagnostics["fts"]["summaries"]["rebuild_needed"]
        .as_bool()
        .unwrap_or(false);

    if mode == "repair" && raw_rebuild_needed {
        let action = json!({
            "kind": "rebuild_raw_fts",
            "safe": true,
            "description": "Rebuild lcm_raw_messages_fts from lcm_raw_messages"
        });
        planned.push(action.clone());
        if apply {
            conn.execute(
                "INSERT INTO lcm_raw_messages_fts(lcm_raw_messages_fts) VALUES('rebuild')",
                (),
            )
            .await
            .map_err(|err| LcmError::Db(err.to_string()))?;
            applied.push(action);
        }
    }

    if mode == "repair" && summary_rebuild_needed {
        let action = json!({
            "kind": "rebuild_summary_fts",
            "safe": true,
            "description": "Rebuild lcm_summary_nodes_fts from lcm_summary_nodes"
        });
        planned.push(action.clone());
        if apply {
            conn.execute(
                "INSERT INTO lcm_summary_nodes_fts(lcm_summary_nodes_fts) VALUES('rebuild')",
                (),
            )
            .await
            .map_err(|err| LcmError::Db(err.to_string()))?;
            applied.push(action);
        }
    }

    Ok(json!({
        "planned_actions": planned,
        "applied_actions": applied,
        "unsafe_actions_skipped": [],
    }))
}

fn issue_count(diagnostics: &Value) -> i64 {
    let schema_issues = if diagnostics["schema"]["schema_current"]
        .as_bool()
        .unwrap_or(false)
    {
        0
    } else {
        1
    };
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
}

async fn count_provider_session(
    conn: &Connection,
    table: &str,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let sql = format!(
        "SELECT COUNT(*) FROM {table} WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)"
    );
    let mut rows = conn
        .query(&sql, params![provider, opt_text(session_id)])
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or_else(|| LcmError::Db("count query returned no rows".to_string()))?;
    row.get(0).map_err(|err| LcmError::Db(err.to_string()))
}

async fn table_or_trigger_count(
    conn: &Connection,
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
            .await
            .map_err(|err| LcmError::Db(err.to_string()))?;
        let row = rows
            .next()
            .await
            .map_err(|err| LcmError::Db(err.to_string()))?
            .ok_or_else(|| LcmError::Db("sqlite_master query returned no rows".to_string()))?;
        let count: i64 = row.get(0).map_err(|err| LcmError::Db(err.to_string()))?;
        if count > 0 {
            found += 1;
        }
    }
    Ok(found)
}

async fn payload_diagnostics(
    conn: &Connection,
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
) -> Result<Value, LcmError> {
    let dir = payload::payload_dir(storage_root);
    let mut missing = 0;
    let mut missing_refs = Vec::new();
    let mut metadata_refs = BTreeSet::new();
    let mut rows = conn
        .query(
            "SELECT payload_ref
             FROM lcm_external_payloads
             WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)",
            params![provider, opt_text(session_id)],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let payload_ref: String = row.get(0).map_err(|err| LcmError::Db(err.to_string()))?;
        metadata_refs.insert(payload_ref.clone());
        if payload::validate_payload_ref(&payload_ref).is_err() || !dir.join(&payload_ref).is_file()
        {
            missing += 1;
            if missing_refs.len() < MAX_SAMPLES {
                missing_refs.push(payload_ref);
            }
        }
    }

    let mut orphan_files = 0;
    let mut orphan_refs = Vec::new();
    let file_owner_refs = if session_id.is_some() {
        all_payload_metadata_refs(conn).await?
    } else {
        metadata_refs
    };
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if payload::validate_payload_ref(&name).is_err() || !entry.path().is_file() {
                continue;
            }
            if !file_owner_refs.contains(&name) {
                orphan_files += 1;
                if orphan_refs.len() < MAX_SAMPLES {
                    orphan_refs.push(name);
                }
            }
        }
    }

    let unreferenced_metadata =
        count_unreferenced_payload_metadata(conn, provider, session_id).await?;
    Ok(json!({
        "missing_files": missing,
        "missing_payload_refs": missing_refs,
        "orphan_files": orphan_files,
        "orphan_payload_refs": orphan_refs,
        "unreferenced_metadata": unreferenced_metadata,
    }))
}

async fn all_payload_metadata_refs(conn: &Connection) -> Result<BTreeSet<String>, LcmError> {
    let mut refs = BTreeSet::new();
    let mut rows = conn
        .query("SELECT payload_ref FROM lcm_external_payloads", ())
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let payload_ref: String = row.get(0).map_err(|err| LcmError::Db(err.to_string()))?;
        refs.insert(payload_ref);
    }
    Ok(refs)
}

async fn count_unreferenced_payload_metadata(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let mut rows = conn
        .query(
            "SELECT COUNT(*)
             FROM lcm_external_payloads p
             LEFT JOIN lcm_raw_messages r
               ON r.provider = p.provider
              AND r.session_id = p.session_id
              AND r.message_id = p.message_id
              AND r.payload_ref = p.payload_ref
              AND r.storage_kind = 'external'
             WHERE p.provider = ?1
               AND (?2 IS NULL OR p.session_id = ?2)
               AND r.message_id IS NULL",
            params![provider, opt_text(session_id)],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or_else(|| LcmError::Db("payload metadata count returned no rows".to_string()))?;
    row.get(0).map_err(|err| LcmError::Db(err.to_string()))
}

async fn fts_diagnostics(
    conn: &Connection,
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
    let raw_rebuild_needed = !raw_table_present
        || raw_trigger_count < 3
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
    conn: &Connection,
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
        .query(&sql, params![provider, opt_text(session_id)])
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let text: String = row.get(0).unwrap_or_default();
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
        let mut match_rows = match conn
            .query(&match_sql, params![term, provider, opt_text(session_id)])
            .await
        {
            Ok(rows) => rows,
            Err(_) => return Ok(true),
        };
        let row = match_rows
            .next()
            .await
            .map_err(|err| LcmError::Db(err.to_string()))?
            .ok_or_else(|| LcmError::Db("FTS probe returned no rows".to_string()))?;
        let count: i64 = row.get(0).map_err(|err| LcmError::Db(err.to_string()))?;
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
    conn: &Connection,
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
    conn: &Connection,
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
            params![provider, opt_text(session_id)],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or_else(|| LcmError::Db("summary source count returned no rows".to_string()))?;
    row.get(0).map_err(|err| LcmError::Db(err.to_string()))
}

async fn count_summary_hash_mismatches(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let mut rows = conn
        .query(
            "SELECT summary_text, summary_hash
             FROM lcm_summary_nodes
             WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)",
            params![provider, opt_text(session_id)],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let mut mismatches = 0;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        let text: String = row.get(0).unwrap_or_default();
        let hash: String = row.get(1).unwrap_or_default();
        if sha256_hex(text.as_bytes()) != hash {
            mismatches += 1;
        }
    }
    Ok(mismatches)
}

async fn lifecycle_integrity(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<Value, LcmError> {
    let lifecycle_state_count = count_lifecycle_states(conn, provider, session_id).await?;
    let invalid_frontiers = count_invalid_frontiers(conn, provider, session_id).await?;
    let orphan_debt = count_orphan_debt(conn, provider, session_id).await?;
    Ok(json!({
        "lifecycle_state_count": lifecycle_state_count,
        "invalid_frontiers": invalid_frontiers,
        "orphan_debt": orphan_debt,
    }))
}

async fn count_lifecycle_states(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let mut rows = conn
        .query(
            "SELECT COUNT(*)
             FROM lcm_lifecycle_state
             WHERE provider = ?1
               AND (?2 IS NULL OR current_session_id = ?2 OR last_finalized_session_id = ?2)",
            params![provider, opt_text(session_id)],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or_else(|| LcmError::Db("lifecycle count returned no rows".to_string()))?;
    row.get(0).map_err(|err| LcmError::Db(err.to_string()))
}

async fn count_invalid_frontiers(
    conn: &Connection,
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
            params![provider, opt_text(session_id)],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or_else(|| LcmError::Db("frontier count returned no rows".to_string()))?;
    row.get(0).map_err(|err| LcmError::Db(err.to_string()))
}

async fn count_orphan_debt(
    conn: &Connection,
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
            params![provider, opt_text(session_id)],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or_else(|| LcmError::Db("debt count returned no rows".to_string()))?;
    row.get(0).map_err(|err| LcmError::Db(err.to_string()))
}

async fn retention_candidates(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<Value, LcmError> {
    let now = unixepoch();
    let mut rows = conn
        .query(
            "SELECT session_id,
                    COUNT(*) AS message_count,
                    COALESCE(SUM(LENGTH(index_text)), 0) AS retained_chars,
                    MIN(COALESCE(timestamp, 0)) AS first_message_at,
                    MAX(COALESCE(timestamp, 0)) AS last_message_at
             FROM lcm_raw_messages
             WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)
             GROUP BY session_id
             ORDER BY retained_chars DESC, last_message_at ASC
             LIMIT 100",
            params![provider, opt_text(session_id)],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let mut candidates = Vec::new();
    let mut analyzed = 0;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    {
        analyzed += 1;
        let session_id: String = row.get(0).map_err(|err| LcmError::Db(err.to_string()))?;
        let message_count: i64 = row.get(1).map_err(|err| LcmError::Db(err.to_string()))?;
        let retained_chars: i64 = row.get(2).map_err(|err| LcmError::Db(err.to_string()))?;
        let first_message_at: i64 = row.get(3).unwrap_or_default();
        let last_message_at: i64 = row.get(4).unwrap_or_default();
        let age_days = if last_message_at > 0 {
            (now.saturating_sub(last_message_at) as f64) / 86_400.0
        } else {
            0.0
        };
        let old = age_days >= RETENTION_OLD_DAYS;
        let heavy = retained_chars >= RETENTION_HEAVY_CHARS;
        let session_only = summary_count_for_session(conn, provider, &session_id).await? == 0;
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

async fn summary_count_for_session(
    conn: &Connection,
    provider: &str,
    session_id: &str,
) -> Result<i64, LcmError> {
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM lcm_summary_nodes WHERE provider = ?1 AND session_id = ?2",
            params![provider, session_id],
        )
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or_else(|| LcmError::Db("summary count returned no rows".to_string()))?;
    row.get(0).map_err(|err| LcmError::Db(err.to_string()))
}

fn sha256_hex(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    hex::encode(hasher.finalize())
}
