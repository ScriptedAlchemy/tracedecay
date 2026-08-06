use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use serde::{Deserialize, Serialize};

use tracedecay_runtime_core::db::engine::{QueryExecutor, Value as SqlValue, params};

use super::{
    LCM_SCAN_PAGE_MAX_BYTES, LCM_SCAN_PAGE_ROWS, LcmError, LcmGcConfig, maintenance, payload,
    schema,
};

mod orphan_scan;
mod pending_delete;
use orphan_scan::{payload_file_present, preview_orphan_files};
pub use pending_delete::{
    drain_pending_payload_delete_in_transaction, drain_pending_payload_deletes_in_transaction,
    stage_payload_delete,
};

const GC_PAYLOAD_PREFIX: &str = "[gc'd externalized payload:";
const GC_TOOL_OUTPUT_PREFIX: &str = "[gc'd externalized tool output:";
const LIVE_PREFIX_REWRITES: [(&str, &str); 3] = [
    ("[externalized payload:", GC_PAYLOAD_PREFIX),
    ("[externalized lcm ingest payload:", GC_PAYLOAD_PREFIX),
    ("[externalized tool output:", GC_TOOL_OUTPUT_PREFIX),
];
const GC_PREFIXES: [&str; 2] = [GC_PAYLOAD_PREFIX, GC_TOOL_OUTPUT_PREFIX];
const MAX_SAMPLES: usize = 20;
const SQLITE_IN_BATCH_SIZE: usize = 500;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LcmGcPhaseReport {
    pub count: usize,
    pub bytes: u64,
    pub refs: Vec<String>,
}

impl LcmGcPhaseReport {
    fn add(&mut self, payload_ref: &str, bytes: u64) {
        self.count += 1;
        self.bytes = self.bytes.saturating_add(bytes);
        if self.refs.len() < MAX_SAMPLES {
            self.refs.push(payload_ref.to_string());
        }
    }

    fn is_empty(&self) -> bool {
        self.count == 0
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LcmGcDeferredReport {
    pub count: usize,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LcmGcError {
    #[serde(rename = "ref")]
    pub payload_ref: String,
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LcmGcReportConfig {
    pub grace_seconds: u64,
    pub reap_missing_after: u64,
    pub max_batch_size: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LcmGcReport {
    pub provider: String,
    pub session_id: Option<String>,
    pub started_at: i64,
    pub ended_at: i64,
    pub config: LcmGcReportConfig,
    pub orphans: LcmGcPhaseReport,
    pub unreferenced: LcmGcPhaseReport,
    pub missing: LcmGcPhaseReport,
    pub dangling: LcmGcPhaseReport,
    pub deferred: LcmGcDeferredReport,
    pub errors: Vec<LcmGcError>,
    pub last_gc_at: Option<i64>,
    pub last_error: Option<String>,
}

impl LcmGcReport {
    fn new(provider: &str, session_id: Option<&str>, cfg: &LcmGcConfig, now: i64) -> Self {
        Self {
            provider: provider.to_string(),
            session_id: session_id.map(str::to_string),
            started_at: now,
            ended_at: now,
            config: LcmGcReportConfig {
                grace_seconds: cfg.grace_seconds,
                reap_missing_after: cfg.reap_missing_after,
                max_batch_size: cfg.max_batch_size,
            },
            orphans: LcmGcPhaseReport::default(),
            unreferenced: LcmGcPhaseReport::default(),
            missing: LcmGcPhaseReport::default(),
            dangling: LcmGcPhaseReport::default(),
            deferred: LcmGcDeferredReport::default(),
            errors: Vec::new(),
            last_gc_at: None,
            last_error: None,
        }
    }

    fn add_error(&mut self, payload_ref: &str, kind: &str, detail: String) {
        if self.errors.len() < MAX_SAMPLES {
            self.errors.push(LcmGcError {
                payload_ref: payload_ref.to_string(),
                kind: kind.to_string(),
                detail,
            });
        }
    }

    fn batch_cap(&mut self, count: usize) {
        if count > 0 {
            self.deferred.count += count;
            self.deferred.reason = Some("batch_cap".to_string());
        }
    }
}

pub async fn referenced_payload_refs(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<BTreeSet<String>, LcmError> {
    // Read through byte-bounded `store_id` keyset pages: the raw-message text
    // for a whole profile exceeds what the SQLite runtime will materialize for
    // one query. Every page folds into the same set, so the answer stays the
    // complete reference closure.
    let mut refs = BTreeSet::new();
    let mut after_store_id = 0_i64;
    loop {
        let mut rows = conn
            .query(
                "WITH page AS (
                     SELECT store_id, storage_kind, payload_ref,
                            content, snippet_text, index_text, metadata_json
                     FROM lcm_raw_messages
                     WHERE (?1 = 'all' OR provider = ?1)
                       AND (?2 IS NULL OR session_id = ?2)
                       AND store_id > ?3
                     ORDER BY store_id
                     LIMIT ?4
                 ),
                 bounded AS (
                     SELECT store_id, storage_kind, payload_ref,
                            content, snippet_text, index_text, metadata_json,
                            ROW_NUMBER() OVER (ORDER BY store_id) AS page_row,
                            SUM(length(CAST(COALESCE(content, '') AS BLOB))
                                + length(CAST(COALESCE(snippet_text, '') AS BLOB))
                                + length(CAST(COALESCE(index_text, '') AS BLOB))
                                + length(CAST(COALESCE(metadata_json, '') AS BLOB)))
                                OVER (ORDER BY store_id) AS cumulative_bytes
                     FROM page
                 )
                 SELECT store_id, storage_kind, payload_ref,
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
                    "LCM referenced payload scan page did not advance".to_string(),
                ));
            }
            after_store_id = store_id;
            page_rows += 1;
            let storage_kind: String = row.get(1)?;
            let payload_ref: Option<String> = row.get(2).unwrap_or(None);
            if storage_kind == "external"
                && let Some(payload_ref) = payload_ref
            {
                refs.insert(payload_ref);
            }
            for index in 3..7 {
                let value: Option<String> = row.get(index).unwrap_or(None);
                if let Some(value) = value.as_deref() {
                    refs.extend(extract_live_payload_refs_from_text(value));
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

fn extract_live_payload_refs_from_text(text: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let mut offset = 0usize;
    while let Some(relative) = text[offset..].find('[') {
        let start = offset + relative;
        let tail = &text[start..];
        let Some(end_relative) = tail.find(']') else {
            break;
        };
        let placeholder = &tail[..=end_relative];
        offset = start + end_relative + 1;
        let lower = placeholder.to_ascii_lowercase();
        if GC_PREFIXES.iter().any(|prefix| lower.starts_with(prefix)) {
            continue;
        }
        refs.extend(payload::extract_payload_refs_from_text(placeholder));
    }
    refs
}

pub fn text_has_tombstoned_payload_ref(text: &str, payload_ref: &str) -> bool {
    if text.is_empty() || !text.contains(payload_ref) {
        return false;
    }
    let mut offset = 0usize;
    while let Some(relative) = text[offset..].find('[') {
        let start = offset + relative;
        let tail = &text[start..];
        let Some(end_relative) = tail.find(']') else {
            return false;
        };
        let placeholder = &tail[..=end_relative];
        let lower = placeholder.to_ascii_lowercase();
        if GC_PREFIXES.iter().any(|prefix| lower.starts_with(prefix))
            && payload::extract_payload_refs_from_text(placeholder)
                .iter()
                .any(|candidate| candidate == payload_ref)
        {
            return true;
        }
        offset = start + end_relative + 1;
    }
    false
}

pub fn tombstone_placeholder_in_text(text: &str, payload_ref: &str) -> String {
    if text.is_empty() || !text.contains(payload_ref) {
        return text.to_string();
    }

    let mut result = String::with_capacity(text.len());
    let mut cursor = 0usize;
    while let Some(relative_start) = text[cursor..].find('[') {
        let start = cursor + relative_start;
        result.push_str(&text[cursor..start]);
        let tail = &text[start..];
        let Some(relative_end) = tail.find(']') else {
            result.push_str(tail);
            return result;
        };
        let end = start + relative_end + 1;
        let placeholder = &text[start..end];
        if placeholder_mentions_ref(placeholder, payload_ref) {
            result.push_str(&tombstone_placeholder(placeholder));
        } else {
            result.push_str(placeholder);
        }
        cursor = end;
    }
    result.push_str(&text[cursor..]);
    result
}

fn placeholder_mentions_ref(placeholder: &str, payload_ref: &str) -> bool {
    payload::extract_payload_refs_from_text(placeholder)
        .iter()
        .any(|candidate| candidate == payload_ref)
}

fn tombstone_placeholder(placeholder: &str) -> String {
    let lower = placeholder.to_ascii_lowercase();
    if GC_PREFIXES.iter().any(|prefix| lower.starts_with(prefix)) {
        return placeholder.to_string();
    }
    for (live_prefix, gc_prefix) in LIVE_PREFIX_REWRITES {
        if lower.starts_with(live_prefix) {
            return format!("{gc_prefix}{}", &placeholder[live_prefix.len()..]);
        }
    }
    placeholder.to_string()
}

pub async fn payload_metadata_refs_for_scope(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<BTreeSet<String>, LcmError> {
    maintenance::payload_metadata_refs_for_scope(conn, provider, session_id).await
}

async fn payload_metadata_bytes(
    conn: &(impl QueryExecutor + ?Sized),
) -> Result<BTreeMap<String, u64>, LcmError> {
    let mut bytes = BTreeMap::new();
    let mut rows = conn
        .query(
            "SELECT payload_ref, byte_count FROM lcm_external_payloads",
            (),
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let payload_ref: String = row.get(0)?;
        let byte_count: i64 = row.get(1)?;
        bytes.insert(payload_ref, byte_count.max(0) as u64);
    }
    Ok(bytes)
}

pub async fn run_payload_gc(
    conn: &(impl QueryExecutor + ?Sized),
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
    cfg: &LcmGcConfig,
    now: i64,
) -> Result<LcmGcReport, LcmError> {
    run_payload_gc_preview(conn, storage_root, provider, session_id, cfg, now).await
}

async fn run_payload_gc_preview(
    conn: &(impl QueryExecutor + ?Sized),
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
    cfg: &LcmGcConfig,
    now: i64,
) -> Result<LcmGcReport, LcmError> {
    let cfg = cfg.clone().normalized();
    let mut report = LcmGcReport::new(provider, session_id, &cfg, now);
    report.last_gc_at = schema::get_gc_meta(conn, "last_gc_at")
        .await?
        .and_then(|value| value.parse::<i64>().ok());
    report.last_error = schema::get_gc_meta(conn, "last_error").await?;

    let dir = payload::existing_payload_dir_opt(storage_root)?;
    let all_metadata_refs = maintenance::all_payload_metadata_refs(conn).await?;
    let scoped_metadata_refs = payload_metadata_refs_for_scope(conn, provider, session_id).await?;
    let referenced = referenced_payload_refs(conn, provider, session_id).await?;
    let metadata_bytes = payload_metadata_bytes(conn).await?;
    let mut remaining = cfg.max_batch_size.max(1);

    if let Some(dir) = dir.as_deref() {
        preview_orphan_files(
            dir,
            &all_metadata_refs,
            now,
            &cfg,
            &mut remaining,
            &mut report,
        )?;
    }
    preview_unreferenced_metadata(
        conn,
        &scoped_metadata_refs,
        &referenced,
        &metadata_bytes,
        now,
        &cfg,
        &mut remaining,
        &mut report,
    )
    .await?;
    preview_missing_metadata(
        conn,
        storage_root,
        &all_metadata_refs,
        &referenced,
        now,
        &cfg,
        &mut remaining,
        &mut report,
    )
    .await?;
    preview_dangling_placeholders(
        conn,
        dir.as_deref(),
        &all_metadata_refs,
        provider,
        session_id,
        &mut report,
    )
    .await?;
    report.ended_at = now;
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
async fn preview_unreferenced_metadata(
    conn: &(impl QueryExecutor + ?Sized),
    metadata_refs: &BTreeSet<String>,
    referenced: &BTreeSet<String>,
    metadata_bytes: &BTreeMap<String, u64>,
    now: i64,
    cfg: &LcmGcConfig,
    remaining: &mut usize,
    report: &mut LcmGcReport,
) -> Result<(), LcmError> {
    let candidates = metadata_refs
        .difference(referenced)
        .cloned()
        .collect::<Vec<_>>();
    let marks = gc_marks(conn, &candidates).await?;
    for payload_ref in &candidates {
        let Some((state, first_seen_at)) = marks.get(payload_ref.as_str()) else {
            report.deferred.count += 1;
            report
                .deferred
                .reason
                .get_or_insert_with(|| "within_grace".to_string());
            continue;
        };
        if state.as_str() != "unreferenced"
            || now.saturating_sub(*first_seen_at) < cfg.grace_seconds as i64
        {
            report.deferred.count += 1;
            report
                .deferred
                .reason
                .get_or_insert_with(|| "within_grace".to_string());
            continue;
        }
        if *remaining == 0 {
            report.batch_cap(1);
            continue;
        }
        report.unreferenced.add(
            payload_ref,
            metadata_bytes.get(payload_ref).copied().unwrap_or_default(),
        );
        *remaining -= 1;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn preview_missing_metadata(
    conn: &(impl QueryExecutor + ?Sized),
    storage_root: &Path,
    metadata_refs: &BTreeSet<String>,
    referenced: &BTreeSet<String>,
    now: i64,
    cfg: &LcmGcConfig,
    remaining: &mut usize,
    report: &mut LcmGcReport,
) -> Result<(), LcmError> {
    let dir = payload::existing_payload_dir_opt(storage_root)?;
    let mut candidates = Vec::new();
    for payload_ref in metadata_refs.intersection(referenced) {
        match payload_file_present(dir.as_deref(), payload_ref) {
            Ok(true) => continue,
            Ok(false) => {}
            Err(error) => {
                report.add_error(payload_ref, "payload_stat_failed", error.to_string());
                continue;
            }
        }
        report.missing.add(payload_ref, 0);
        if !cfg.reap_missing_enabled || cfg.reap_missing_after == 0 {
            continue;
        }
        candidates.push(payload_ref.clone());
    }
    let marks = gc_marks(conn, &candidates).await?;
    for payload_ref in &candidates {
        let Some((state, first_seen_at)) = marks.get(payload_ref.as_str()) else {
            continue;
        };
        if state.as_str() != "missing"
            || now.saturating_sub(*first_seen_at) < cfg.reap_missing_after as i64
        {
            continue;
        }
        if *remaining == 0 {
            report.batch_cap(1);
            continue;
        }
        *remaining -= 1;
    }
    Ok(())
}

async fn preview_dangling_placeholders(
    conn: &(impl QueryExecutor + ?Sized),
    dir: Option<&Path>,
    metadata_refs: &BTreeSet<String>,
    provider: &str,
    session_id: Option<&str>,
    report: &mut LcmGcReport,
) -> Result<(), LcmError> {
    let referenced = referenced_payload_refs(conn, provider, session_id).await?;
    for payload_ref in referenced.difference(metadata_refs) {
        match payload_file_present(dir, payload_ref) {
            Ok(true) => {}
            Ok(false) => report.dangling.add(payload_ref, 0),
            Err(error) => {
                report.add_error(
                    payload_ref,
                    "dangling_payload_stat_failed",
                    error.to_string(),
                );
            }
        }
    }
    Ok(())
}

/// Loads GC marks in batches so read-only previews do not issue one query per
/// candidate payload reference.
async fn gc_marks(
    conn: &(impl QueryExecutor + ?Sized),
    payload_refs: &[String],
) -> Result<HashMap<String, (String, i64)>, LcmError> {
    let mut marks = HashMap::new();
    for chunk in payload_refs.chunks(SQLITE_IN_BATCH_SIZE) {
        if chunk.is_empty() {
            continue;
        }
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT payload_ref, state, first_seen_at
             FROM lcm_gc_marks
             WHERE payload_ref IN ({placeholders})"
        );
        let mut rows = conn
            .query(
                &sql,
                chunk
                    .iter()
                    .cloned()
                    .map(SqlValue::Text)
                    .collect::<Vec<_>>(),
            )
            .await?;
        while let Some(row) = rows.next().await? {
            marks.insert(row.get(0)?, (row.get(1)?, row.get(2)?));
        }
    }
    Ok(marks)
}
