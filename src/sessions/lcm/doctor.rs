use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use libsql::{params, Connection, Value as SqlValue};
use serde_json::{json, Value};

use crate::tokensave::current_timestamp;

use super::{payload, schema, security, util, LcmCleanConfig, LcmError, LCM_SCHEMA_VERSION};

const MAX_SAMPLES: usize = 20;
const RETENTION_OLD_DAYS: f64 = 30.0;
const RETENTION_HEAVY_CHARS: i64 = 128 * 1024;
const SQLITE_IN_BATCH_SIZE: usize = 500;

fn sql_placeholders(len: usize) -> String {
    std::iter::repeat_n("?", len).collect::<Vec<_>>().join(", ")
}

pub(crate) struct DoctorRequest<'a> {
    pub(crate) storage_root: &'a Path,
    pub(crate) db_path: &'a Path,
    pub(crate) provider: &'a str,
    pub(crate) session_id: Option<&'a str>,
    pub(crate) mode: &'a str,
    pub(crate) apply: bool,
    pub(crate) clean_config: LcmCleanConfig,
}

struct RepairRequest<'a> {
    db_path: &'a Path,
    storage_root: &'a Path,
    provider: &'a str,
    session_id: Option<&'a str>,
    diagnostics: &'a Value,
    mode: &'a str,
    apply: bool,
    clean_config: &'a LcmCleanConfig,
}

pub(crate) async fn doctor(
    conn: &Connection,
    request: DoctorRequest<'_>,
) -> Result<Value, LcmError> {
    let diagnostics = gather_diagnostics(
        conn,
        request.storage_root,
        request.provider,
        request.session_id,
        &request.clean_config,
    )
    .await?;
    let repairs = plan_and_apply_repairs(
        conn,
        RepairRequest {
            db_path: request.db_path,
            storage_root: request.storage_root,
            provider: request.provider,
            session_id: request.session_id,
            diagnostics: &diagnostics,
            mode: request.mode,
            apply: request.apply,
            clean_config: &request.clean_config,
        },
    )
    .await?;
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
        "provider": request.provider,
        "session_id": request.session_id,
        "mode": request.mode,
        "dry_run": matches!(request.mode, "repair" | "clean") && !request.apply,
        "apply": request.apply,
        "diagnostics": diagnostics,
        "repairs": repairs,
    }))
}

async fn gather_diagnostics(
    conn: &Connection,
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
    clean_config: &LcmCleanConfig,
) -> Result<Value, LcmError> {
    let schema_version = schema::schema_version(conn).await;
    let raw_message_count =
        util::count_by_provider_session(conn, "lcm_raw_messages", provider, session_id).await?;
    let summary_node_count =
        util::count_by_provider_session(conn, "lcm_summary_nodes", provider, session_id).await?;
    let external_payload_count =
        util::count_by_provider_session(conn, "lcm_external_payloads", provider, session_id)
            .await?;
    let payloads = payload_diagnostics(conn, storage_root, provider, session_id).await?;
    let fts = fts_diagnostics(conn, provider, session_id).await?;
    let summaries = summary_integrity(conn, provider, session_id).await?;
    let lifecycle = lifecycle_integrity(conn, provider, session_id).await?;
    let retention = retention_candidates(conn, provider, session_id).await?;
    let cleanup = cleanup_candidates(conn, provider, session_id, clean_config).await?;

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
        "cleanup": cleanup,
    }))
}

async fn plan_and_apply_repairs(
    conn: &Connection,
    request: RepairRequest<'_>,
) -> Result<Value, LcmError> {
    let RepairRequest {
        db_path,
        storage_root,
        provider,
        session_id,
        diagnostics,
        mode,
        apply,
        clean_config,
    } = request;
    let mut planned = Vec::new();
    let mut applied = Vec::new();
    let mut backup = Value::Null;
    let raw_rebuild_needed = diagnostics["fts"]["raw"]["rebuild_needed"]
        .as_bool()
        .unwrap_or(false);
    let summary_rebuild_needed = diagnostics["fts"]["summaries"]["rebuild_needed"]
        .as_bool()
        .unwrap_or(false);
    if mode == "repair" && apply && (raw_rebuild_needed || summary_rebuild_needed) {
        checkpoint_wal_for_backup(conn).await?;
        backup = backup_database(db_path, storage_root)?;
    }

    if mode == "repair" && raw_rebuild_needed {
        let action = json!({
            "kind": "rebuild_raw_fts",
            "safe": true,
            "description": "Recreate the content-only lcm_raw_messages_fts structure and rebuild it from lcm_raw_messages"
        });
        planned.push(action.clone());
        if apply {
            // Recreates the table/triggers in the current (v3, content-only)
            // structure before rebuilding, so this also repairs databases
            // whose FTS objects predate the role/metadata_json removal.
            schema::rebuild_raw_fts(conn)
                .await
                .ok_or_else(|| LcmError::Db("raw FTS rebuild failed".to_string()))?;
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
            .await?;
            applied.push(action);
        }
    }

    if mode == "clean" {
        let candidate_count = diagnostics["cleanup"]["candidate_count"]
            .as_i64()
            .unwrap_or_default();
        if candidate_count > 0 {
            let action = json!({
                "kind": "clean_lcm_noise",
                "safe": true,
                "description": "Delete LCM ignored/stateless session candidates and standalone configured-noise raw messages",
                "candidate_count": candidate_count,
                "session_candidate_count": diagnostics["cleanup"]["session_candidates"].as_array().map(Vec::len).unwrap_or_default(),
                "message_candidate_count": diagnostics["cleanup"]["message_candidates"].as_array().map(Vec::len).unwrap_or_default(),
            });
            planned.push(action.clone());
            if apply {
                let (backup_result, deleted) = backup_and_delete_clean_candidates(
                    conn,
                    db_path,
                    storage_root,
                    provider,
                    session_id,
                    clean_config,
                )
                .await?;
                backup = backup_result;
                let mut applied_action = action;
                if let Some(object) = applied_action.as_object_mut() {
                    object.insert("deleted".to_string(), deleted);
                }
                applied.push(applied_action);
            }
        }
    }

    Ok(json!({
        "planned_actions": planned,
        "applied_actions": applied,
        "backup": backup,
        "unsafe_actions_skipped": [],
    }))
}

fn issue_count(diagnostics: &Value) -> i64 {
    let schema_issues = i64::from(
        !diagnostics["schema"]["schema_current"]
            .as_bool()
            .unwrap_or(false),
    );
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
        + diagnostics["cleanup"]["candidate_count"]
            .as_i64()
            .unwrap_or(0)
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
            params![provider, util::opt_text(session_id)],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let payload_ref: String = row.get(0)?;
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
        metadata_refs.clone()
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
    let placeholder_refs =
        placeholder_ref_diagnostics(conn, &dir, provider, session_id, &metadata_refs).await?;
    Ok(json!({
        "missing_files": missing,
        "missing_payload_refs": missing_refs,
        "orphan_files": orphan_files,
        "orphan_payload_refs": orphan_refs,
        "unreferenced_metadata": unreferenced_metadata,
        "placeholder_refs_total": placeholder_refs["placeholder_refs_total"],
        "missing_placeholder_metadata": placeholder_refs["missing_placeholder_metadata"],
        "missing_placeholder_files": placeholder_refs["missing_placeholder_files"],
        "missing_placeholder_refs": placeholder_refs["missing_placeholder_refs"],
        "gc_candidate_files": orphan_files,
        "gc_candidate_payload_refs": orphan_refs,
    }))
}

async fn placeholder_ref_diagnostics(
    conn: &Connection,
    payload_dir: &Path,
    provider: &str,
    session_id: Option<&str>,
    metadata_refs: &BTreeSet<String>,
) -> Result<Value, LcmError> {
    let mut refs = BTreeSet::new();
    let mut first_locations = serde_json::Map::new();
    let mut rows = conn
        .query(
            "SELECT store_id, message_id, content, snippet_text, index_text, metadata_json
             FROM lcm_raw_messages
             WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)",
            params![provider, util::opt_text(session_id)],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let store_id: i64 = row.get(0)?;
        let message_id: String = row.get(1)?;
        for index in 2..6 {
            let value: Option<String> = row.get(index).unwrap_or(None);
            let field = match index {
                2 => "content",
                3 => "snippet_text",
                4 => "index_text",
                _ => "metadata_json",
            };
            for payload_ref in value
                .as_deref()
                .map(payload::extract_payload_refs_from_text)
                .unwrap_or_default()
            {
                if refs.insert(payload_ref.clone()) {
                    first_locations.insert(
                        payload_ref.clone(),
                        json!({
                            "payload_ref": payload_ref,
                            "store_id": store_id,
                            "message_id": message_id,
                            "field": field,
                        }),
                    );
                }
            }
        }
    }

    let missing_metadata = refs
        .iter()
        .filter(|payload_ref| !metadata_refs.contains(*payload_ref))
        .count();
    let missing_files = refs
        .iter()
        .filter(|payload_ref| {
            payload::validate_payload_ref(payload_ref).is_err()
                || !payload_dir.join(payload_ref).is_file()
        })
        .count();
    let missing_refs = refs
        .iter()
        .filter(|payload_ref| {
            !metadata_refs.contains(*payload_ref)
                || payload::validate_payload_ref(payload_ref).is_err()
                || !payload_dir.join(payload_ref).is_file()
        })
        .filter_map(|payload_ref| first_locations.get(payload_ref).cloned())
        .take(MAX_SAMPLES)
        .collect::<Vec<_>>();

    Ok(json!({
        "placeholder_refs_total": refs.len(),
        "missing_placeholder_metadata": missing_metadata,
        "missing_placeholder_files": missing_files,
        "missing_placeholder_refs": missing_refs,
    }))
}

async fn all_payload_metadata_refs(conn: &Connection) -> Result<BTreeSet<String>, LcmError> {
    let mut refs = BTreeSet::new();
    let mut rows = conn
        .query("SELECT payload_ref FROM lcm_external_payloads", ())
        .await?;
    while let Some(row) = rows.next().await? {
        let payload_ref: String = row.get(0)?;
        refs.insert(payload_ref);
    }
    Ok(refs)
}

async fn count_unreferenced_payload_metadata(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    let referenced_refs = referenced_payload_refs(conn, provider, session_id).await?;
    let mut rows = conn
        .query(
            "SELECT payload_ref
             FROM lcm_external_payloads
             WHERE provider = ?1
               AND (?2 IS NULL OR session_id = ?2)",
            params![provider, util::opt_text(session_id)],
        )
        .await?;
    let mut unreferenced = 0_i64;
    while let Some(row) = rows.next().await? {
        let payload_ref: String = row.get(0)?;
        if !referenced_refs.contains(&payload_ref) {
            unreferenced += 1;
        }
    }
    Ok(unreferenced)
}

async fn referenced_payload_refs(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<BTreeSet<String>, LcmError> {
    let mut refs = BTreeSet::new();
    let mut rows = conn
        .query(
            "SELECT storage_kind, payload_ref, content, snippet_text, index_text, metadata_json
             FROM lcm_raw_messages
             WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)",
            params![provider, util::opt_text(session_id)],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let storage_kind: String = row.get(0)?;
        let payload_ref: Option<String> = row.get(1).unwrap_or(None);
        if storage_kind == "external" {
            if let Some(payload_ref) = payload_ref {
                refs.insert(payload_ref);
            }
        }
        for index in 2..6 {
            let value: Option<String> = row.get(index).unwrap_or(None);
            refs.extend(
                value
                    .as_deref()
                    .map(payload::extract_payload_refs_from_text)
                    .unwrap_or_default(),
            );
        }
    }
    Ok(refs)
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
    conn: &Connection,
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
    conn: &Connection,
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
    conn: &Connection,
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
    conn: &Connection,
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

async fn cleanup_candidates(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
    clean_config: &LcmCleanConfig,
) -> Result<Value, LcmError> {
    let ignore_session_patterns =
        security::compile_session_patterns(&clean_config.ignore_session_patterns);
    let stateless_session_patterns =
        security::compile_session_patterns(&clean_config.stateless_session_patterns);
    let ignore_message_patterns =
        security::compile_message_patterns(&clean_config.ignore_message_patterns);
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

fn backup_database(db_path: &Path, storage_root: &Path) -> Result<Value, LcmError> {
    let backup_dir = storage_root.join("lcm-clean-backups");
    fs::create_dir_all(&backup_dir).map_err(|err| LcmError::Io(err.to_string()))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let backup_path = backup_dir.join(format!("sessions-clean-{stamp}-{}.db", std::process::id()));
    let byte_count = copy_sqlite_file_set(db_path, &backup_path)?;
    Ok(json!({
        "ok": true,
        "path": backup_path,
        "byte_count": byte_count,
    }))
}

fn copy_sqlite_file_set(db_path: &Path, backup_path: &Path) -> Result<u64, LcmError> {
    let mut byte_count =
        fs::copy(db_path, backup_path).map_err(|err| LcmError::Io(err.to_string()))?;
    // Copy only the WAL sidecar. The -shm file is rebuildable shared memory
    // that SQLite never reads from a backup, and its live byte-range locks
    // make plain file reads fail with ERROR_LOCK_VIOLATION (os error 33) on
    // Windows while any connection is open.
    let source = sqlite_sidecar_path(db_path, "-wal");
    if source.is_file() {
        let target = sqlite_sidecar_path(backup_path, "-wal");
        byte_count += fs::copy(&source, target).map_err(|err| LcmError::Io(err.to_string()))?;
    }
    Ok(byte_count)
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

async fn checkpoint_wal_for_backup(conn: &Connection) -> Result<(), LcmError> {
    let mut rows = conn.query("PRAGMA wal_checkpoint(TRUNCATE);", ()).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| LcmError::Db("WAL checkpoint returned no status row".to_string()))?;
    let busy: i64 = row.get(0)?;
    let log_frames: i64 = row.get(1)?;
    let checkpointed_frames: i64 = row.get(2)?;
    if busy != 0 || checkpointed_frames < log_frames {
        return Err(LcmError::Db(format!(
            "WAL checkpoint incomplete before clean backup: busy={busy}, log_frames={log_frames}, checkpointed_frames={checkpointed_frames}"
        )));
    }
    Ok(())
}

async fn backup_and_delete_clean_candidates(
    conn: &Connection,
    db_path: &Path,
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
    clean_config: &LcmCleanConfig,
) -> Result<(Value, Value), LcmError> {
    backup_and_delete_clean_candidates_with_backup(
        conn,
        provider,
        session_id,
        clean_config,
        || async { backup_database(db_path, storage_root) },
    )
    .await
}

async fn backup_and_delete_clean_candidates_with_backup<F, Fut>(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
    clean_config: &LcmCleanConfig,
    backup: F,
) -> Result<(Value, Value), LcmError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Value, LcmError>>,
{
    checkpoint_wal_for_backup(conn).await?;
    conn.execute("BEGIN IMMEDIATE", ()).await?;
    let result = async {
        let (session_ids, message_store_ids) =
            collect_clean_delete_targets(conn, provider, session_id, clean_config).await?;
        let backup_result = backup().await?;
        let deleted = delete_clean_candidates_in_transaction(
            conn,
            provider,
            &session_ids,
            &message_store_ids,
        )
        .await?;
        Ok((backup_result, deleted))
    }
    .await;

    match result {
        Ok(values) => match conn.execute("COMMIT", ()).await {
            Ok(_) => Ok(values),
            Err(err) => {
                let _ = conn.execute("ROLLBACK", ()).await;
                Err(LcmError::Db(err.to_string()))
            }
        },
        Err(err) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            Err(err)
        }
    }
}

async fn collect_clean_delete_targets(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
    clean_config: &LcmCleanConfig,
) -> Result<(Vec<String>, Vec<i64>), LcmError> {
    let ignore_session_patterns =
        security::compile_session_patterns(&clean_config.ignore_session_patterns);
    let stateless_session_patterns =
        security::compile_session_patterns(&clean_config.stateless_session_patterns);
    let ignore_message_patterns =
        security::compile_message_patterns(&clean_config.ignore_message_patterns);
    let protected_raw_sources =
        raw_store_ids_with_summary_sources(conn, provider, session_id).await?;

    let mut rows = conn
        .query(
            "SELECT store_id, session_id, COALESCE(content, index_text, '')
             FROM lcm_raw_messages
             WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)
             ORDER BY session_id, store_id",
            params![provider, util::opt_text(session_id)],
        )
        .await?;

    let mut session_ids = BTreeSet::<String>::new();
    let mut message_store_ids = Vec::<i64>::new();
    while let Some(row) = rows.next().await? {
        let store_id: i64 = row.get(0)?;
        let row_session_id: String = row.get(1)?;
        let content: String = row.get(2).unwrap_or_default();

        let filtered_session =
            security::matches_any_compiled_pattern(&ignore_session_patterns, &row_session_id)
                || security::matches_any_compiled_pattern(
                    &stateless_session_patterns,
                    &row_session_id,
                );
        if filtered_session {
            session_ids.insert(row_session_id);
            continue;
        }

        if security::ignore_message_reason_with_compiled(&content, &ignore_message_patterns)
            .is_some()
            && !protected_raw_sources.contains(&store_id)
        {
            message_store_ids.push(store_id);
        }
    }

    Ok((session_ids.into_iter().collect(), message_store_ids))
}

async fn delete_clean_candidates_in_transaction(
    conn: &Connection,
    provider: &str,
    session_ids: &[String],
    message_store_ids: &[i64],
) -> Result<Value, LcmError> {
    let deleted_sessions = session_ids.len() as i64;
    let mut deleted_messages = 0_i64;

    for session_chunk in session_ids.chunks(SQLITE_IN_BATCH_SIZE) {
        if session_chunk.is_empty() {
            continue;
        }
        let placeholders = sql_placeholders(session_chunk.len());

        let mut summary_values = vec![SqlValue::Text(provider.to_string())];
        summary_values.extend(session_chunk.iter().cloned().map(SqlValue::Text));
        conn.execute(
            &format!(
                "DELETE FROM lcm_summary_nodes
                 WHERE provider = ? AND session_id IN ({placeholders})"
            ),
            summary_values,
        )
        .await?;

        let mut payload_values = vec![SqlValue::Text(provider.to_string())];
        payload_values.extend(session_chunk.iter().cloned().map(SqlValue::Text));
        conn.execute(
            &format!(
                "DELETE FROM lcm_external_payloads
                 WHERE provider = ? AND session_id IN ({placeholders})"
            ),
            payload_values,
        )
        .await?;

        let mut raw_values = vec![SqlValue::Text(provider.to_string())];
        raw_values.extend(session_chunk.iter().cloned().map(SqlValue::Text));
        let changed = conn
            .execute(
                &format!(
                    "DELETE FROM lcm_raw_messages
                     WHERE provider = ? AND session_id IN ({placeholders})"
                ),
                raw_values,
            )
            .await?;
        deleted_messages += changed as i64;

        let mut lifecycle_values = vec![SqlValue::Text(provider.to_string())];
        lifecycle_values.extend(session_chunk.iter().cloned().map(SqlValue::Text));
        lifecycle_values.extend(session_chunk.iter().cloned().map(SqlValue::Text));
        lifecycle_values.extend(session_chunk.iter().cloned().map(SqlValue::Text));
        conn.execute(
            &format!(
                "DELETE FROM lcm_lifecycle_state
                 WHERE provider = ?
                   AND (
                        conversation_id IN ({placeholders})
                        OR current_session_id IN ({placeholders})
                        OR last_finalized_session_id IN ({placeholders})
                   )"
            ),
            lifecycle_values,
        )
        .await?;
    }

    let message_ids = message_ids_for_store_ids(conn, message_store_ids).await?;
    let message_ids = message_ids.into_iter().collect::<Vec<_>>();
    for message_id_chunk in message_ids.chunks(SQLITE_IN_BATCH_SIZE) {
        if message_id_chunk.is_empty() {
            continue;
        }
        let placeholders = sql_placeholders(message_id_chunk.len());
        let mut values = vec![SqlValue::Text(provider.to_string())];
        values.extend(message_id_chunk.iter().cloned().map(SqlValue::Text));
        conn.execute(
            &format!(
                "DELETE FROM lcm_external_payloads
                 WHERE provider = ? AND message_id IN ({placeholders})"
            ),
            values,
        )
        .await?;
    }
    for store_id_chunk in message_store_ids.chunks(SQLITE_IN_BATCH_SIZE) {
        if store_id_chunk.is_empty() {
            continue;
        }
        let placeholders = sql_placeholders(store_id_chunk.len());
        let changed = conn
            .execute(
                &format!("DELETE FROM lcm_raw_messages WHERE store_id IN ({placeholders})"),
                store_id_chunk
                    .iter()
                    .map(|store_id| SqlValue::Integer(*store_id))
                    .collect::<Vec<_>>(),
            )
            .await?;
        deleted_messages += changed as i64;
    }

    Ok(json!({
        "sessions": deleted_sessions,
        "raw_messages": deleted_messages,
    }))
}

async fn summary_counts_by_session(
    conn: &Connection,
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
    conn: &Connection,
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

async fn message_ids_for_store_ids(
    conn: &Connection,
    store_ids: &[i64],
) -> Result<BTreeSet<String>, LcmError> {
    let mut message_ids = BTreeSet::new();
    for store_id_chunk in store_ids.chunks(SQLITE_IN_BATCH_SIZE) {
        if store_id_chunk.is_empty() {
            continue;
        }
        let placeholders = sql_placeholders(store_id_chunk.len());
        let sql =
            format!("SELECT message_id FROM lcm_raw_messages WHERE store_id IN ({placeholders})");
        let mut rows = conn
            .query(
                &sql,
                store_id_chunk
                    .iter()
                    .map(|store_id| SqlValue::Integer(*store_id))
                    .collect::<Vec<_>>(),
            )
            .await?;
        while let Some(row) = rows.next().await? {
            let message_id: String = row.get(0)?;
            message_ids.insert(message_id);
        }
    }
    Ok(message_ids)
}
#[cfg(test)]
mod tests {
    #![allow(dead_code)]

    use std::path::Path;
    use std::time::Duration;

    use super::*;

    async fn insert_test_clean_candidate(
        conn: &Connection,
        project_root: &Path,
        session_id: &str,
        message_id: &str,
    ) -> Result<(), String> {
        let project_key = project_root.to_string_lossy().to_string();
        conn.execute(
            "INSERT INTO sessions (
                provider, session_id, project_key, project_path, title, started_at
             ) VALUES ('cursor', ?1, ?2, ?2, ?3, 1)
             ON CONFLICT(provider, session_id) DO NOTHING",
            params![session_id, project_key.as_str(), session_id],
        )
        .await
        .map_err(|err| err.to_string())?;
        conn.execute(
            "INSERT INTO lcm_raw_messages (
                provider, message_id, session_id, role, ordinal, timestamp,
                content, content_hash, storage_kind, payload_ref, snippet_text,
                index_text, legacy_source, legacy_truncated, metadata_json
             )
             VALUES (
                'cursor', ?1, ?2, 'assistant', 1, 2,
                'test cron body', ?3, 'inline', NULL, 'test cron body',
                'test cron body', 0, 0, NULL
             )",
            params![message_id, session_id, format!("{message_id}-hash")],
        )
        .await
        .map_err(|err| err.to_string())?;
        Ok(())
    }

    async fn raw_count(conn: &Connection, session_id: &str) -> Result<i64, String> {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM lcm_raw_messages WHERE session_id = ?1",
                params![session_id],
            )
            .await
            .map_err(|err| format!("count raw messages for {session_id}: {err}"))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|err| format!("read raw message count row for {session_id}: {err}"))?
        else {
            return Err(format!("missing raw message count row for {session_id}"));
        };
        row.get(0)
            .map_err(|err| format!("read raw message count for {session_id}: {err}"))
    }

    #[tokio::test]
    async fn clean_apply_backup_callback_runs_under_immediate_transaction() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|err| format!("create tempdir: {err}"))?;
        let project_root = temp.path().to_path_buf();
        let db_path = project_root.join("sessions.db");
        let _global = crate::global_db::GlobalDb::open_at(&db_path)
            .await
            .ok_or_else(|| "test session database should open".to_string())?;
        let db = libsql::Builder::new_local(&db_path)
            .build()
            .await
            .map_err(|err| format!("build test database: {err}"))?;
        let conn = db
            .connect()
            .map_err(|err| format!("connect to test database: {err}"))?;
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(|err| format!("set test database busy timeout: {err}"))?;
        insert_test_clean_candidate(
            &conn,
            &project_root,
            "cron-before-lock",
            "cron-before-lock-message",
        )
        .await?;

        let writer_db = libsql::Builder::new_local(&db_path)
            .build()
            .await
            .map_err(|err| format!("build writer database: {err}"))?;
        let writer_conn = writer_db
            .connect()
            .map_err(|err| format!("connect to writer database: {err}"))?;
        writer_conn
            .busy_timeout(Duration::from_millis(25))
            .map_err(|err| format!("set writer database busy timeout: {err}"))?;
        let writer_project_root = project_root.clone();
        let backup_path = project_root.join("backup.db");
        let backup_path_for_callback = backup_path.clone();
        let clean_config = LcmCleanConfig {
            ignore_session_patterns: vec!["cron-*".to_string()],
            ..Default::default()
        };

        let (backup, deleted) = backup_and_delete_clean_candidates_with_backup(
            &conn,
            "cursor",
            None,
            &clean_config,
            move || async move {
                let write_result = insert_test_clean_candidate(
                    &writer_conn,
                    &writer_project_root,
                    "cron-raced-lock",
                    "cron-raced-lock-message",
                )
                .await;
                assert!(
                    write_result.is_err(),
                    "backup callback must run while BEGIN IMMEDIATE blocks concurrent clean candidates"
                );
                Ok(json!({
                    "ok": true,
                    "path": backup_path_for_callback,
                }))
            },
        )
        .await
        .map_err(|err| format!("backup and delete clean candidates: {err}"))?;

        assert_eq!(backup["ok"], true);
        assert_eq!(deleted["sessions"], 1);
        assert_eq!(raw_count(&conn, "cron-before-lock").await?, 0);
        assert_eq!(raw_count(&conn, "cron-raced-lock").await?, 0);
        Ok(())
    }
}
