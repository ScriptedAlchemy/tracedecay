use std::collections::HashMap;
use std::fs;
use std::path::Path;

use tracedecay_runtime_core::db::engine::{Value, params_from_iter};

use super::scope::LcmScopeSql;
use super::*;

use crate::{LCM_SCAN_PAGE_MAX_BYTES, LCM_SCAN_PAGE_ROWS};

/// Shallow payload census, covered by `idx_lcm_external_payloads_owner_bytes`
/// so the status probe never reads payload metadata records. The unbounded
/// scope omits the non-sargable `(?1 = 'all' OR …)` tautology.
pub(super) fn payload_summary_query(
    provider: &str,
    session_id: Option<&str>,
) -> (String, Vec<Value>) {
    let scope = LcmScopeSql::new("provider", "session_id", provider, session_id);
    let sql = format!(
        "SELECT COUNT(*),
                COALESCE(SUM(CASE WHEN byte_count > 0 THEN byte_count ELSE 0 END), 0)
         FROM lcm_external_payloads
         {scope}",
        scope = scope.where_clause()
    );
    (sql, scope.into_values())
}

#[hotpath::measure(label = "sessions.lcm.status.payload_summary", future = true)]
pub async fn payload_health_summary(
    conn: &(impl QueryExecutor + ?Sized),
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
    gc_config: &LcmGcConfig,
) -> Result<PayloadHealthDetail, LcmError> {
    let gc_config = gc_config.clone().normalized();
    let (sql, values) = payload_summary_query(provider, session_id);
    let mut rows = conn.query(&sql, params_from_iter(values)).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| LcmError::Db("payload summary query returned no rows".to_owned()))?;
    let externalized_count: i64 = row.get(0)?;
    let total_bytes = row.get::<i64>(1)?.max(0) as u64;
    Ok(PayloadHealthDetail {
        payload: LcmPayloadStatus {
            coverage: LcmPayloadCoverage {
                state: LcmPayloadCoverageState::Partial,
                scanned_metadata_refs: 0,
                scanned_files: 0,
                reason: Some("payload_file_census_requires_deep_status".to_owned()),
            },
            externalized_count,
            missing_count: 0,
            unreferenced_count: 0,
            placeholder_ref_count: 0,
            missing_placeholder_metadata_count: 0,
            missing_placeholder_file_count: 0,
            gc_candidate_count: 0,
            root_contained: payload_root_contained(storage_root),
            orphan_file_count: 0,
            tombstoned_count: 0,
            referenced_count: 0,
            total_bytes,
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
            grace_seconds: i64::try_from(gc_config.grace_seconds).unwrap_or(i64::MAX),
            reap_missing_metadata_after_seconds: i64::try_from(gc_config.reap_missing_after)
                .unwrap_or(i64::MAX),
            next_run_eligible_at: None,
        },
        missing_payload_refs: Vec::new(),
        orphan_files: Vec::new(),
        unreferenced_refs: Vec::new(),
        missing_placeholder_refs: Vec::new(),
        integrity_mismatch_refs: Vec::new(),
    })
}

#[hotpath::measure(label = "sessions.lcm.status.payload_detail", future = true)]
pub async fn payload_health_detail(
    conn: &(impl QueryExecutor + ?Sized),
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
    deep: bool,
    sample_limit: usize,
    gc_config: &LcmGcConfig,
) -> Result<PayloadHealthDetail, LcmError> {
    let gc_config = gc_config.clone().normalized();
    let now = current_timestamp();
    let sample_limit = sample_limit.max(1);
    let metadata_refs =
        maintenance::payload_metadata_refs_for_scope(conn, provider, session_id).await?;
    let metadata_bytes = payload_byte_counts_for_scope(conn, provider, session_id).await?;
    let payload_locations = payload_ref_locations_for_scope(conn, provider, session_id).await?;
    let referenced_refs = gc::referenced_payload_refs(conn, provider, session_id).await?;
    let placeholder_status = placeholder_payload_status(
        conn,
        storage_root,
        provider,
        session_id,
        &metadata_refs,
        &payload_locations,
        sample_limit,
    )
    .await?;
    let file_owner_refs = if session_id.is_some() {
        maintenance::all_payload_metadata_refs(conn).await?
    } else {
        metadata_refs.clone()
    };
    let payload_dir = payload::payload_dir(storage_root);
    let grace_seconds_i64 = i64::try_from(gc_config.grace_seconds).unwrap_or(i64::MAX);
    let reap_missing_after_seconds =
        i64::try_from(gc_config.reap_missing_after).unwrap_or(i64::MAX);

    let last_gc_at = gc_meta_i64(conn, "last_gc_at").await?;
    let last_gc_duration_ms = gc_meta_i64(conn, "last_gc_duration_ms")
        .await?
        .map(|value| value.max(0) as u64);
    let last_gc_error = schema::get_gc_meta(conn, "last_error").await?;
    let last_reaped_refs = gc_meta_i64(conn, "last_reaped_refs").await?;
    let last_reaped_bytes = gc_meta_i64(conn, "last_reaped_bytes")
        .await?
        .map(|value| value.max(0) as u64);
    let last_gc_status = schema::get_gc_meta(conn, "last_gc_status")
        .await?
        .or_else(|| match (last_gc_at, last_gc_error.as_deref()) {
            (None, _) => None,
            (Some(_), None | Some("")) => Some("ok".to_string()),
            (Some(_), Some("partial")) => Some("partial".to_string()),
            (Some(_), Some(_)) => Some("failed".to_string()),
        });

    let mut missing_count = 0_i64;
    let mut missing_payload_refs = Vec::new();
    let mut unreferenced_count = 0_i64;
    let mut total_bytes = 0_u64;
    let mut referenced_bytes = 0_u64;
    let mut reclaimable_unreferenced_bytes = 0_u64;
    let mut reclaimable_bytes_after_grace = 0_u64;
    let mut next_run_eligible_at: Option<i64> = None;
    let mut integrity_mismatch_count = 0_i64;
    let mut integrity_mismatch_refs = Vec::new();
    let root_contained = payload_root_contained(storage_root);
    let unreferenced_payload_refs = metadata_refs
        .difference(&referenced_refs)
        .cloned()
        .collect::<Vec<_>>();
    let unreferenced_marks = gc::gc_marks(conn, &unreferenced_payload_refs).await?;

    for payload_ref in &metadata_refs {
        let bytes = metadata_bytes.get(payload_ref).copied().unwrap_or_default();
        total_bytes = total_bytes.saturating_add(bytes);
        let is_referenced = referenced_refs.contains(payload_ref);
        if is_referenced {
            referenced_bytes = referenced_bytes.saturating_add(bytes);
        } else {
            unreferenced_count += 1;
            reclaimable_bytes_after_grace = reclaimable_bytes_after_grace.saturating_add(bytes);
            let eligible_at =
                unreferenced_eligible_at(&unreferenced_marks, payload_ref, grace_seconds_i64);
            if let Some(eligible_at) = eligible_at {
                next_run_eligible_at = Some(
                    next_run_eligible_at.map_or(eligible_at, |current| current.min(eligible_at)),
                );
                if last_gc_at.is_some() && eligible_at <= now {
                    reclaimable_unreferenced_bytes =
                        reclaimable_unreferenced_bytes.saturating_add(bytes);
                }
            }
        }

        let missing_file = payload::validate_payload_ref(payload_ref).is_err()
            || !payload_file_present_strict(&payload_dir, payload_ref)?;
        if missing_file {
            missing_count += 1;
            if missing_payload_refs.len() < sample_limit {
                missing_payload_refs.push(
                    payload_locations
                        .get(payload_ref)
                        .cloned()
                        .unwrap_or_else(|| {
                            payload_ref_location(payload_ref, session_id, "payload_ref")
                        }),
                );
            }
            continue;
        }

        if deep && payload_has_integrity_mismatch(storage_root, payload_ref, conn).await? {
            integrity_mismatch_count += 1;
            if integrity_mismatch_refs.len() < sample_limit {
                integrity_mismatch_refs.push(payload_ref.clone());
            }
        }
    }

    let mut orphan_file_count = 0_i64;
    let mut orphan_file_bytes = 0_u64;
    let mut reclaimable_orphan_bytes = 0_u64;
    let mut orphan_files = Vec::new();
    let mut scanned_files = 0_i64;
    if let Ok(entries) = fs::read_dir(&payload_dir) {
        for entry in entries {
            let entry = entry.map_err(|err| LcmError::Io(err.to_string()))?;
            scanned_files = scanned_files.saturating_add(1);
            let name = entry.file_name().to_string_lossy().to_string();
            if payload::validate_payload_ref(&name).is_err() {
                continue;
            }
            let metadata =
                fs::symlink_metadata(entry.path()).map_err(|err| LcmError::Io(err.to_string()))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                continue;
            }
            if file_owner_refs.contains(&name) {
                continue;
            }
            orphan_file_count += 1;
            orphan_file_bytes = orphan_file_bytes.saturating_add(metadata.len());
            reclaimable_bytes_after_grace =
                reclaimable_bytes_after_grace.saturating_add(metadata.len());
            let mtime = util::file_mtime_seconds(&metadata);
            let age_seconds = now.saturating_sub(mtime);
            let eligible_at = mtime.saturating_add(grace_seconds_i64);
            next_run_eligible_at =
                Some(next_run_eligible_at.map_or(eligible_at, |current| current.min(eligible_at)));
            if age_seconds >= grace_seconds_i64 && last_gc_at.is_some() {
                reclaimable_orphan_bytes = reclaimable_orphan_bytes.saturating_add(metadata.len());
            }
            if orphan_files.len() < sample_limit {
                orphan_files.push(PayloadFileStatusSample {
                    payload_ref: name,
                    bytes: metadata.len(),
                    age_seconds,
                    eligible_at,
                });
            }
        }
    }

    let tombstoned_count = tombstoned_count(conn, provider, session_id).await?;
    let externalized_count = metadata_refs.len() as i64;
    let referenced_count = externalized_count.saturating_sub(unreferenced_count);
    let reclaimable_bytes = if last_gc_at.is_some() {
        reclaimable_unreferenced_bytes.saturating_add(reclaimable_orphan_bytes)
    } else {
        0
    };
    let unreferenced_refs = payload_unreferenced_samples(PayloadUnreferencedSamplesRequest {
        metadata_refs: &metadata_refs,
        referenced_refs: &referenced_refs,
        metadata_bytes: &metadata_bytes,
        unreferenced_marks: &unreferenced_marks,
        last_gc_at,
        grace_seconds: grace_seconds_i64,
        now,
        sample_limit,
    });

    Ok(PayloadHealthDetail {
        payload: LcmPayloadStatus {
            coverage: LcmPayloadCoverage {
                state: LcmPayloadCoverageState::Complete,
                scanned_metadata_refs: metadata_refs.len() as i64,
                scanned_files,
                reason: None,
            },
            externalized_count,
            missing_count,
            unreferenced_count,
            placeholder_ref_count: placeholder_status.placeholder_ref_count,
            missing_placeholder_metadata_count: placeholder_status.missing_metadata_count,
            missing_placeholder_file_count: placeholder_status.missing_file_count,
            gc_candidate_count: unreferenced_count,
            root_contained,
            orphan_file_count,
            tombstoned_count,
            referenced_count,
            total_bytes,
            referenced_bytes,
            orphan_file_bytes,
            reclaimable_bytes,
            reclaimable_bytes_after_grace,
            integrity_mismatch_count: deep.then_some(integrity_mismatch_count),
        },
        payload_gc: LcmPayloadGcStatus {
            last_gc_at,
            last_gc_duration_ms,
            last_gc_status,
            last_gc_error,
            last_reaped_refs,
            last_reaped_bytes,
            grace_seconds: grace_seconds_i64,
            reap_missing_metadata_after_seconds: reap_missing_after_seconds,
            next_run_eligible_at,
        },
        missing_payload_refs,
        orphan_files,
        unreferenced_refs,
        missing_placeholder_refs: placeholder_status.missing_refs,
        integrity_mismatch_refs,
    })
}

#[derive(Debug, Clone)]
pub struct PayloadHealthDetail {
    pub payload: LcmPayloadStatus,
    pub payload_gc: LcmPayloadGcStatus,
    pub missing_payload_refs: Vec<PayloadRefLocation>,
    pub orphan_files: Vec<PayloadFileStatusSample>,
    pub unreferenced_refs: Vec<PayloadRefStatusSample>,
    pub missing_placeholder_refs: Vec<PayloadRefLocation>,
    pub integrity_mismatch_refs: Vec<String>,
}

async fn payload_byte_counts_for_scope(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<BTreeMap<String, u64>, LcmError> {
    let mut bytes = BTreeMap::new();
    let mut after_rowid = 0_i64;
    loop {
        let mut rows = conn
            .query(
                "SELECT payload_ref, byte_count, rowid
                 FROM lcm_external_payloads
                 WHERE (?1 = 'all' OR provider = ?1)
                   AND (?2 IS NULL OR session_id = ?2)
                   AND rowid > ?3
                 ORDER BY rowid
                 LIMIT ?4",
                params![provider, session_id, after_rowid, LCM_SCAN_PAGE_ROWS],
            )
            .await?;
        let mut page_rows = 0_i64;
        while let Some(row) = rows.next().await? {
            let payload_ref: String = row.get(0)?;
            let byte_count: i64 = row.get(1)?;
            after_rowid = row.get(2)?;
            page_rows += 1;
            bytes.insert(payload_ref, byte_count.max(0) as u64);
        }
        drop(rows);
        if page_rows < LCM_SCAN_PAGE_ROWS {
            return Ok(bytes);
        }
    }
}

async fn payload_ref_locations_for_scope(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<BTreeMap<String, PayloadRefLocation>, LcmError> {
    let mut refs = BTreeMap::new();
    let mut after_store_id = 0_i64;
    loop {
        let mut rows = conn
            .query(
                "WITH page AS (
                     SELECT store_id, message_id, session_id, storage_kind, payload_ref,
                            content, snippet_text, index_text, metadata_json
                     FROM lcm_raw_messages
                     WHERE (?1 = 'all' OR provider = ?1)
                       AND (?2 IS NULL OR session_id = ?2)
                       AND store_id > ?3
                     ORDER BY store_id
                     LIMIT ?4
                 ),
                 bounded AS (
                     SELECT store_id, message_id, session_id, storage_kind, payload_ref,
                            content, snippet_text, index_text, metadata_json,
                            ROW_NUMBER() OVER (ORDER BY store_id) AS page_row,
                            SUM(length(CAST(COALESCE(content, '') AS BLOB))
                                + length(CAST(COALESCE(snippet_text, '') AS BLOB))
                                + length(CAST(COALESCE(index_text, '') AS BLOB))
                                + length(CAST(COALESCE(metadata_json, '') AS BLOB)))
                                OVER (ORDER BY store_id) AS cumulative_bytes
                     FROM page
                 )
                 SELECT store_id, message_id, session_id, storage_kind, payload_ref,
                        content, snippet_text, index_text, metadata_json
                 FROM bounded
                 WHERE cumulative_bytes <= ?5 OR page_row = 1
                 ORDER BY store_id",
                params![
                    provider,
                    session_id,
                    after_store_id,
                    LCM_SCAN_PAGE_ROWS,
                    LCM_SCAN_PAGE_MAX_BYTES
                ],
            )
            .await?;
        let mut page_rows = 0_usize;
        while let Some(row) = rows.next().await? {
            let store_id: i64 = row.get(0)?;
            if store_id <= after_store_id {
                return Err(LcmError::Db(
                    "LCM payload reference scan page did not advance".to_string(),
                ));
            }
            after_store_id = store_id;
            page_rows += 1;
            let message_id: String = row.get(1)?;
            let owner_session_id: String = row.get(2)?;
            let storage_kind: String = row.get(3)?;
            let raw_payload_ref: Option<String> = row.get(4).unwrap_or(None);
            if storage_kind == "external"
                && let Some(payload_ref) = raw_payload_ref
            {
                refs.entry(payload_ref.clone())
                    .or_insert_with(|| PayloadRefLocation {
                        payload_ref,
                        session_id: owner_session_id.clone(),
                        message_id: message_id.clone(),
                        store_id,
                        field: "payload_ref".to_string(),
                    });
            }
            for index in 5..9 {
                let value: Option<String> = row.get(index).unwrap_or(None);
                let field = match index {
                    5 => "content",
                    6 => "snippet_text",
                    7 => "index_text",
                    _ => "metadata_json",
                };
                for payload_ref in value
                    .as_deref()
                    .map(payload::extract_payload_refs_from_text)
                    .unwrap_or_default()
                {
                    refs.entry(payload_ref.clone())
                        .or_insert_with(|| PayloadRefLocation {
                            payload_ref,
                            session_id: owner_session_id.clone(),
                            message_id: message_id.clone(),
                            store_id,
                            field: field.to_string(),
                        });
                }
            }
        }
        drop(rows);
        // A byte-bounded page can stop short of the row budget, so only an
        // empty page proves the scan is complete.
        if page_rows == 0 {
            return Ok(refs);
        }
    }
}

fn payload_ref_location(
    payload_ref: &str,
    session_id: Option<&str>,
    field: &str,
) -> PayloadRefLocation {
    PayloadRefLocation {
        payload_ref: payload_ref.to_string(),
        session_id: session_id.unwrap_or_default().to_string(),
        message_id: String::new(),
        store_id: 0,
        field: field.to_string(),
    }
}

struct PayloadUnreferencedSamplesRequest<'a> {
    metadata_refs: &'a BTreeSet<String>,
    referenced_refs: &'a BTreeSet<String>,
    metadata_bytes: &'a BTreeMap<String, u64>,
    unreferenced_marks: &'a HashMap<String, (String, i64)>,
    last_gc_at: Option<i64>,
    grace_seconds: i64,
    now: i64,
    sample_limit: usize,
}

fn payload_unreferenced_samples(
    request: PayloadUnreferencedSamplesRequest<'_>,
) -> Vec<PayloadRefStatusSample> {
    let PayloadUnreferencedSamplesRequest {
        metadata_refs,
        referenced_refs,
        metadata_bytes,
        unreferenced_marks,
        last_gc_at,
        grace_seconds,
        now,
        sample_limit,
    } = request;
    let mut samples = Vec::new();
    for payload_ref in metadata_refs.difference(referenced_refs) {
        if samples.len() >= sample_limit {
            break;
        }
        let eligible_at = unreferenced_eligible_at(unreferenced_marks, payload_ref, grace_seconds);
        let grace_remaining_seconds = eligible_at.map(|ts| ts.saturating_sub(now).max(0));
        samples.push(PayloadRefStatusSample {
            payload_ref: payload_ref.clone(),
            bytes: metadata_bytes.get(payload_ref).copied().unwrap_or_default(),
            eligible_at: if last_gc_at.is_some() {
                eligible_at
            } else {
                None
            },
            grace_remaining_seconds: if last_gc_at.is_some() {
                grace_remaining_seconds
            } else {
                None
            },
        });
    }
    samples
}

fn unreferenced_eligible_at(
    marks: &HashMap<String, (String, i64)>,
    payload_ref: &str,
    grace_seconds: i64,
) -> Option<i64> {
    marks.get(payload_ref).and_then(|(state, first_seen_at)| {
        (state.as_str() == "unreferenced").then_some(first_seen_at.saturating_add(grace_seconds))
    })
}

async fn gc_meta_i64(
    conn: &(impl QueryExecutor + ?Sized),
    key: &str,
) -> Result<Option<i64>, LcmError> {
    Ok(schema::get_gc_meta(conn, key)
        .await?
        .and_then(|value| value.parse::<i64>().ok()))
}

async fn tombstoned_count(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<i64, LcmError> {
    gc::count_placeholder_text_rows(
        conn,
        gc::PlaceholderScanScope::ProviderOrAll {
            provider,
            session_id,
        },
        &gc::gc_prefix_like_patterns(),
    )
    .await
}

async fn placeholder_payload_status(
    conn: &(impl QueryExecutor + ?Sized),
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
    metadata_refs: &BTreeSet<String>,
    payload_locations: &BTreeMap<String, PayloadRefLocation>,
    sample_limit: usize,
) -> Result<PlaceholderPayloadStatus, LcmError> {
    let placeholder_refs = placeholder_refs_for_scope(conn, provider, session_id).await?;
    let dir = payload::payload_dir(storage_root);
    let mut missing_metadata_count = 0_i64;
    let mut missing_file_count = 0_i64;
    let mut missing_refs = Vec::new();
    for payload_ref in &placeholder_refs {
        let missing_metadata = !metadata_refs.contains(payload_ref);
        let missing_file = payload::validate_payload_ref(payload_ref).is_err()
            || !payload_file_present_strict(&dir, payload_ref)?;
        if missing_metadata {
            missing_metadata_count += 1;
        }
        if missing_file {
            missing_file_count += 1;
        }
        if (missing_metadata || missing_file) && missing_refs.len() < sample_limit {
            missing_refs.push(
                payload_locations
                    .get(payload_ref)
                    .cloned()
                    .unwrap_or_else(|| {
                        payload_ref_location(payload_ref, session_id, "placeholder")
                    }),
            );
        }
    }
    Ok(PlaceholderPayloadStatus {
        placeholder_ref_count: placeholder_refs.len() as i64,
        missing_metadata_count,
        missing_file_count,
        missing_refs,
    })
}

#[allow(clippy::struct_field_names)]
struct PlaceholderPayloadStatus {
    placeholder_ref_count: i64,
    missing_metadata_count: i64,
    missing_file_count: i64,
    missing_refs: Vec<PayloadRefLocation>,
}

async fn placeholder_refs_for_scope(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<BTreeSet<String>, LcmError> {
    let rows = gc::scan_placeholder_text_rows(
        conn,
        gc::PlaceholderScanScope::ProviderOrAll {
            provider,
            session_id,
        },
        &gc::all_placeholder_like_patterns(),
    )
    .await?;
    let mut refs = BTreeSet::new();
    for row in rows {
        for text in row.texts() {
            refs.extend(payload::extract_payload_refs_from_text(text));
        }
    }
    Ok(refs)
}

fn payload_file_present_strict(dir: &Path, payload_ref: &str) -> Result<bool, LcmError> {
    let path = dir.join(payload_ref);
    payload::ensure_contained(dir, &path)?;
    let Ok(metadata) = fs::symlink_metadata(&path) else {
        return Ok(false);
    };
    Ok(metadata.is_file() && !metadata.file_type().is_symlink())
}

async fn payload_has_integrity_mismatch(
    storage_root: &Path,
    payload_ref: &str,
    conn: &(impl QueryExecutor + ?Sized),
) -> Result<bool, LcmError> {
    let metadata = match payload::load_payload_metadata(conn, payload_ref).await {
        Ok(metadata) => metadata,
        Err(LcmError::PayloadNotFound) => return Ok(false),
        Err(err) => return Err(err),
    };
    let dir = payload::existing_payload_dir(storage_root)?;
    let path = dir.join(payload_ref);
    payload::ensure_contained(&dir, &path)?;
    let Ok(fs_metadata) = fs::symlink_metadata(&path) else {
        return Ok(false);
    };
    if fs_metadata.file_type().is_symlink() || !fs_metadata.is_file() {
        return Ok(true);
    }
    let bytes = fs::read(&path).map_err(|err| LcmError::Io(err.to_string()))?;
    Ok(util::sha256_hex(&bytes) != metadata.content_hash)
}

fn payload_root_contained(storage_root: &Path) -> bool {
    let dir = payload::payload_dir(storage_root);
    if !dir.exists() {
        return true;
    }
    let Ok(root) = storage_root.canonicalize() else {
        return false;
    };
    let Ok(canonical_dir) = dir.canonicalize() else {
        return false;
    };
    canonical_dir.parent() == Some(root.as_path())
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PayloadRefLocation {
    pub payload_ref: String,
    pub session_id: String,
    pub message_id: String,
    pub store_id: i64,
    pub field: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PayloadFileStatusSample {
    pub payload_ref: String,
    pub bytes: u64,
    pub age_seconds: i64,
    pub eligible_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PayloadRefStatusSample {
    pub payload_ref: String,
    pub bytes: u64,
    pub eligible_at: Option<i64>,
    pub grace_remaining_seconds: Option<i64>,
}
