use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(test)]
use tracedecay_runtime_core::db::engine::{Connection, TransactionBehavior};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, Value as SqlValue, params};

use super::{
    LCM_SCAN_PAGE_MAX_BYTES, LCM_SCAN_PAGE_ROWS, LcmError, LcmGcConfig, maintenance, payload,
    schema,
};

mod orphan_scan;
mod pending_delete;
use orphan_scan::{payload_file_present, preview_orphan_files, stage_orphan_files};
pub use pending_delete::{
    PayloadDeleteDrain, drain_pending_payload_delete_in_transaction,
    drain_pending_payload_deletes_in_transaction, stage_payload_delete,
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

    fn merge(&mut self, other: Self) {
        self.count += other.count;
        self.bytes = self.bytes.saturating_add(other.bytes);
        for payload_ref in other.refs {
            if self.refs.len() >= MAX_SAMPLES {
                break;
            }
            if !self.refs.contains(&payload_ref) {
                self.refs.push(payload_ref);
            }
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LcmGcTotals {
    pub files: usize,
    pub bytes: u64,
    pub rows_deleted: usize,
    pub placeholders_rewritten: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LcmGcReportConfig {
    pub grace_seconds: u64,
    pub reap_missing_after: u64,
    pub max_batch_size: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LcmGcReport {
    pub status: String,
    pub provider: String,
    pub session_id: Option<String>,
    pub apply: bool,
    pub started_at: i64,
    pub ended_at: i64,
    pub config: LcmGcReportConfig,
    pub orphans: LcmGcPhaseReport,
    pub unreferenced: LcmGcPhaseReport,
    pub missing: LcmGcPhaseReport,
    pub dangling: LcmGcPhaseReport,
    pub deferred: LcmGcDeferredReport,
    pub errors: Vec<LcmGcError>,
    pub totals: LcmGcTotals,
    pub last_gc_at: Option<i64>,
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<Value>,
}

impl LcmGcReport {
    fn new(
        provider: &str,
        session_id: Option<&str>,
        cfg: &LcmGcConfig,
        apply: bool,
        now: i64,
    ) -> Self {
        Self {
            status: if apply { "applied" } else { "dry_run" }.to_string(),
            provider: provider.to_string(),
            session_id: session_id.map(str::to_string),
            apply,
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
            totals: LcmGcTotals::default(),
            last_gc_at: None,
            last_error: None,
            backup: None,
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
        self.status = if self.apply { "partial" } else { "dry_run" }.to_string();
    }

    fn batch_cap(&mut self, count: usize) {
        if count > 0 {
            self.deferred.count += count;
            self.deferred.reason = Some("batch_cap".to_string());
        }
    }

    fn reconcile_file_drain(&mut self, drain: PayloadDeleteDrain) {
        self.totals.files = self
            .totals
            .files
            .saturating_add(drain.outcomes.removed.count);
        self.totals.bytes = self
            .totals
            .bytes
            .saturating_add(drain.outcomes.removed.bytes);
        for error in drain.errors {
            self.add_error(&error.payload_ref, &error.kind, error.detail);
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
    let mut report = LcmGcReport::new(provider, session_id, &cfg, false, now);
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

#[cfg(test)]
pub async fn run_payload_gc_with_apply(
    conn: &Connection,
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
    cfg: &LcmGcConfig,
    apply: bool,
    now: i64,
) -> Result<LcmGcReport, LcmError> {
    if !apply {
        return run_payload_gc_preview(conn, storage_root, provider, session_id, cfg, now).await;
    }

    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let mut report = run_payload_gc_in_transaction(
        &transaction,
        storage_root,
        provider,
        session_id,
        cfg,
        true,
        now,
    )
    .await?;
    transaction.commit().await?;
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let drain = drain_pending_payload_deletes_in_transaction(&transaction, storage_root).await?;
    finalize_gc_report(&transaction, &mut report, drain).await?;
    transaction.commit().await?;
    Ok(report)
}

pub async fn finalize_gc_report(
    conn: &(impl Executor + ?Sized),
    report: &mut LcmGcReport,
    drain: PayloadDeleteDrain,
) -> Result<(), LcmError> {
    let had_delete_failures = drain.has_failures();
    report.reconcile_file_drain(drain);
    schema::set_gc_meta(conn, "last_reaped_refs", &report.totals.files.to_string()).await?;
    schema::set_gc_meta(conn, "last_reaped_bytes", &report.totals.bytes.to_string()).await?;
    if had_delete_failures {
        schema::set_gc_meta(conn, "last_gc_status", "partial").await?;
    } else if report.errors.is_empty() {
        schema::set_gc_meta(conn, "last_gc_status", "ok").await?;
        schema::clear_gc_meta(conn, "last_error").await?;
    } else {
        schema::set_gc_meta(conn, "last_gc_status", "partial").await?;
        schema::set_gc_meta(conn, "last_error", "partial").await?;
    }
    report.last_error = schema::get_gc_meta(conn, "last_error").await?;
    Ok(())
}

pub async fn finalize_gc_report_value(
    conn: &(impl Executor + ?Sized),
    report_value: &mut Value,
    drain: PayloadDeleteDrain,
) -> Result<(), LcmError> {
    let mut report: LcmGcReport = serde_json::from_value(report_value.clone())
        .map_err(|err| LcmError::Db(format!("invalid GC report: {err}")))?;
    finalize_gc_report(conn, &mut report, drain).await?;
    *report_value = serde_json::to_value(report)
        .map_err(|err| LcmError::Db(format!("serialize GC report: {err}")))?;
    Ok(())
}

pub async fn run_payload_gc_in_transaction(
    conn: &(impl Executor + ?Sized),
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
    cfg: &LcmGcConfig,
    apply: bool,
    now: i64,
) -> Result<LcmGcReport, LcmError> {
    let started = Instant::now();
    let cfg = cfg.clone().normalized();
    let mut report = LcmGcReport::new(provider, session_id, &cfg, apply, now);
    report.last_gc_at = schema::get_gc_meta(conn, "last_gc_at")
        .await?
        .and_then(|value| value.parse::<i64>().ok());
    report.last_error = schema::get_gc_meta(conn, "last_error").await?;

    // The payload directory is created lazily on first externalization, so a
    // missing directory is a normal state: filesystem scans see it as empty
    // while the DB-side phases below still run (missing payloads, stale
    // marks, dangling placeholders).
    let dir = payload::existing_payload_dir_opt(storage_root)?;
    let all_metadata_refs = maintenance::all_payload_metadata_refs(conn).await?;

    if apply && cfg.backup_before_reap && (dir.is_some() || !all_metadata_refs.is_empty()) {
        report.backup = Some(
            maintenance::backup_database(
                &gc_database_path(storage_root),
                storage_root,
                maintenance::BackupKind::Gc,
            )
            .await?,
        );
    }

    let scoped_metadata_refs = payload_metadata_refs_for_scope(conn, provider, session_id).await?;
    let referenced = referenced_payload_refs(conn, provider, session_id).await?;
    let metadata_bytes = payload_metadata_bytes(conn).await?;

    let mut remaining = cfg.max_batch_size.max(1);
    // Orphan files have no metadata row, so they cannot be attributed to a
    // provider/session. Include them in every scoped GC preview/apply just as
    // the payload-health surface includes them for scoped drill-downs.
    if let Some(dir) = dir.as_deref() {
        if apply {
            stage_orphan_files(
                conn,
                dir,
                &all_metadata_refs,
                now,
                &cfg,
                &mut remaining,
                &mut report,
            )
            .await?;
        } else {
            preview_orphan_files(
                dir,
                &all_metadata_refs,
                now,
                &cfg,
                &mut remaining,
                &mut report,
            )?;
        }
    }
    reap_unreferenced_metadata(ReapUnreferencedMetadataRequest {
        conn,
        storage_root,
        metadata_refs: &scoped_metadata_refs,
        referenced: &referenced,
        metadata_bytes: &metadata_bytes,
        now,
        cfg: &cfg,
        apply,
        remaining: &mut remaining,
        report: &mut report,
    })
    .await?;
    reap_missing_metadata(ReapMissingMetadataRequest {
        conn,
        storage_root,
        metadata_refs: &all_metadata_refs,
        referenced: &referenced,
        now,
        cfg: &cfg,
        apply,
        remaining: &mut remaining,
        report: &mut report,
    })
    .await?;
    rewrite_dangling_placeholders(
        conn,
        dir.as_deref(),
        &all_metadata_refs,
        provider,
        session_id,
        apply,
        &mut report,
    )
    .await?;

    report.ended_at = now;
    if apply {
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let status = if report.errors.is_empty() {
            "ok"
        } else {
            "partial"
        };
        schema::set_gc_meta(conn, "last_gc_at", &now.to_string()).await?;
        schema::set_gc_meta(conn, "last_gc_duration_ms", &duration_ms.to_string()).await?;
        schema::set_gc_meta(conn, "last_gc_status", status).await?;
        schema::set_gc_meta(conn, "last_reaped_refs", &report.totals.files.to_string()).await?;
        schema::set_gc_meta(conn, "last_reaped_bytes", &report.totals.bytes.to_string()).await?;
        if report.errors.is_empty() {
            schema::clear_gc_meta(conn, "last_error").await?;
        } else {
            schema::set_gc_meta(conn, "last_error", "partial").await?;
        }
    }
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

struct ReapUnreferencedMetadataRequest<'a, E: Executor + ?Sized> {
    conn: &'a E,
    storage_root: &'a Path,
    metadata_refs: &'a BTreeSet<String>,
    referenced: &'a BTreeSet<String>,
    metadata_bytes: &'a BTreeMap<String, u64>,
    now: i64,
    cfg: &'a LcmGcConfig,
    apply: bool,
    remaining: &'a mut usize,
    report: &'a mut LcmGcReport,
}

async fn reap_unreferenced_metadata<E: Executor + ?Sized>(
    request: ReapUnreferencedMetadataRequest<'_, E>,
) -> Result<(), LcmError> {
    let ReapUnreferencedMetadataRequest {
        conn,
        storage_root,
        metadata_refs,
        referenced,
        metadata_bytes,
        now,
        cfg,
        apply,
        remaining,
        report,
    } = request;
    for payload_ref in metadata_refs.intersection(referenced) {
        if apply {
            conn.execute(
                "DELETE FROM lcm_gc_marks WHERE payload_ref = ?1 AND state = 'unreferenced'",
                params![payload_ref.as_str()],
            )
            .await?;
        }
    }

    for payload_ref in metadata_refs.difference(referenced) {
        let mark = gc_mark(conn, payload_ref).await?;
        let Some((state, first_seen_at)) = mark else {
            if apply {
                upsert_gc_mark(conn, payload_ref, "unreferenced", now).await?;
            }
            report.deferred.count += 1;
            report
                .deferred
                .reason
                .get_or_insert_with(|| "within_grace".to_string());
            continue;
        };
        if state != "unreferenced" {
            if apply {
                upsert_gc_mark(conn, payload_ref, "unreferenced", now).await?;
            }
            continue;
        }
        if now.saturating_sub(first_seen_at) < cfg.grace_seconds as i64 {
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
        let bytes = metadata_bytes.get(payload_ref).copied().unwrap_or_default();
        if apply {
            match payload::delete_external_payload_in_transaction(
                conn,
                storage_root,
                payload_ref,
                &payload::DeleteOpts::default(),
            )
            .await
            {
                Ok(prepared) => {
                    let outcome = prepared.outcome;
                    if outcome.metadata_row_existed {
                        report.totals.rows_deleted += 1;
                    }
                    report.totals.placeholders_rewritten += outcome.placeholders_rewritten;
                }
                Err(LcmError::StillReferenced) => {
                    conn.execute(
                        "DELETE FROM lcm_gc_marks WHERE payload_ref = ?1",
                        params![payload_ref.as_str()],
                    )
                    .await?;
                    continue;
                }
                Err(LcmError::PayloadIntegrityMismatch) => {
                    report.add_error(
                        payload_ref,
                        "integrity_mismatch",
                        "sha256 mismatch".to_string(),
                    );
                    continue;
                }
                Err(err) => {
                    report.add_error(payload_ref, "delete_failed", err.to_string());
                    continue;
                }
            }
        }
        report.unreferenced.add(payload_ref, bytes);
        *remaining -= 1;
    }
    Ok(())
}

struct ReapMissingMetadataRequest<'a, E: Executor + ?Sized> {
    conn: &'a E,
    storage_root: &'a Path,
    metadata_refs: &'a BTreeSet<String>,
    referenced: &'a BTreeSet<String>,
    now: i64,
    cfg: &'a LcmGcConfig,
    apply: bool,
    remaining: &'a mut usize,
    report: &'a mut LcmGcReport,
}

async fn reap_missing_metadata<E: Executor + ?Sized>(
    request: ReapMissingMetadataRequest<'_, E>,
) -> Result<(), LcmError> {
    let ReapMissingMetadataRequest {
        conn,
        storage_root,
        metadata_refs,
        referenced,
        now,
        cfg,
        apply,
        remaining,
        report,
    } = request;
    let dir = payload::existing_payload_dir_opt(storage_root)?;
    for payload_ref in metadata_refs.intersection(referenced) {
        let file_present = match payload_file_present(dir.as_deref(), payload_ref) {
            Ok(present) => present,
            Err(err) => {
                report.add_error(payload_ref, "payload_stat_failed", err.to_string());
                continue;
            }
        };
        if file_present {
            if apply {
                conn.execute(
                    "DELETE FROM lcm_gc_marks WHERE payload_ref = ?1 AND state = 'missing'",
                    params![payload_ref.as_str()],
                )
                .await?;
            }
            continue;
        }
        report.missing.add(payload_ref, 0);
        if !apply || !cfg.reap_missing_enabled || cfg.reap_missing_after == 0 {
            continue;
        }
        let mark = gc_mark(conn, payload_ref).await?;
        let first_seen_at = match mark {
            Some((state, first_seen_at)) if state == "missing" => first_seen_at,
            _ => {
                upsert_gc_mark(conn, payload_ref, "missing", now).await?;
                continue;
            }
        };
        if now.saturating_sub(first_seen_at) < cfg.reap_missing_after as i64 {
            continue;
        }
        if *remaining == 0 {
            report.batch_cap(1);
            continue;
        }
        match payload::delete_external_payload_in_transaction(
            conn,
            storage_root,
            payload_ref,
            &payload::DeleteOpts {
                rewrite_placeholders: true,
                remove_file: false,
                verify_hash: false,
            },
        )
        .await
        {
            Ok(prepared) => {
                let outcome = prepared.outcome;
                if outcome.metadata_row_existed {
                    report.totals.rows_deleted += 1;
                }
                report.totals.placeholders_rewritten += outcome.placeholders_rewritten;
            }
            Err(err) => {
                report.add_error(payload_ref, "missing_reap_failed", err.to_string());
                continue;
            }
        }
        *remaining -= 1;
    }
    Ok(())
}

pub async fn rewrite_dangling_placeholders(
    conn: &(impl Executor + ?Sized),
    dir: Option<&Path>,
    metadata_refs: &BTreeSet<String>,
    provider: &str,
    session_id: Option<&str>,
    apply: bool,
    report: &mut LcmGcReport,
) -> Result<(), LcmError> {
    let referenced = referenced_payload_refs(conn, provider, session_id).await?;
    for payload_ref in referenced.difference(metadata_refs) {
        match payload_file_present(dir, payload_ref) {
            Ok(true) => continue,
            Ok(false) => {}
            Err(err) => {
                report.add_error(payload_ref, "dangling_payload_stat_failed", err.to_string());
                continue;
            }
        }
        let changed = if apply {
            tombstone_dangling_ref_in_transaction(conn, payload_ref, provider, session_id).await?
        } else {
            0
        };
        if apply {
            report.totals.placeholders_rewritten += changed;
        }
        report.dangling.add(payload_ref, 0);
    }
    Ok(())
}

async fn tombstone_dangling_ref_in_transaction(
    conn: &(impl Executor + ?Sized),
    payload_ref: &str,
    provider: &str,
    session_id: Option<&str>,
) -> Result<usize, LcmError> {
    let mut rows = conn
    .query(
        "SELECT store_id, content, snippet_text, index_text, metadata_json
             FROM lcm_raw_messages
             WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)
               AND (content LIKE ?3 OR snippet_text LIKE ?3 OR index_text LIKE ?3 OR metadata_json LIKE ?3)",
        params![provider, session_id, format!("%{payload_ref}%")],
    )
    .await?;
    let mut updates = Vec::new();
    while let Some(row) = rows.next().await? {
        let store_id: i64 = row.get(0)?;
        let content: Option<String> = row.get(1).unwrap_or(None);
        let snippet_text: String = row.get(2)?;
        let index_text: String = row.get(3)?;
        let metadata_json: Option<String> = row.get(4).unwrap_or(None);
        let mut changed = 0usize;
        let new_content = content.map(|text| {
            let tombstoned = tombstone_placeholder_in_text(&text, payload_ref);
            if tombstoned != text {
                changed += 1;
            }
            tombstoned
        });
        let new_snippet = tombstone_placeholder_in_text(&snippet_text, payload_ref);
        if new_snippet != snippet_text {
            changed += 1;
        }
        let new_index = tombstone_placeholder_in_text(&index_text, payload_ref);
        if new_index != index_text {
            changed += 1;
        }
        let new_metadata = metadata_json.map(|text| {
            let tombstoned = tombstone_placeholder_in_text(&text, payload_ref);
            if tombstoned != text {
                changed += 1;
            }
            tombstoned
        });
        if changed > 0 {
            updates.push((
                store_id,
                new_content,
                new_snippet,
                new_index,
                new_metadata,
                changed,
            ));
        }
    }
    let mut total = 0usize;
    for (store_id, content, snippet_text, index_text, metadata_json, changed) in updates {
        conn.execute(
            "UPDATE lcm_raw_messages
             SET content = ?2, snippet_text = ?3, index_text = ?4, metadata_json = ?5
             WHERE store_id = ?1",
            params![
                store_id,
                content.as_deref(),
                snippet_text,
                index_text,
                metadata_json.as_deref()
            ],
        )
        .await?;
        total += changed;
    }
    Ok(total)
}

async fn gc_mark(
    conn: &(impl QueryExecutor + ?Sized),
    payload_ref: &str,
) -> Result<Option<(String, i64)>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT state, first_seen_at FROM lcm_gc_marks WHERE payload_ref = ?1",
            params![payload_ref],
        )
        .await?;
    if let Some(row) = rows.next().await? {
        Ok(Some((row.get(0)?, row.get(1)?)))
    } else {
        Ok(None)
    }
}

/// Batch form of [`gc_mark`] for read-only preview passes that would otherwise
/// issue one query per candidate payload ref.
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

async fn upsert_gc_mark(
    conn: &(impl Executor + ?Sized),
    payload_ref: &str,
    state: &str,
    now: i64,
) -> Result<(), LcmError> {
    conn.execute(
    "INSERT INTO lcm_gc_marks(payload_ref, state, first_seen_at, updated_at)
     VALUES (?1, ?2, ?3, ?3)
     ON CONFLICT(payload_ref) DO UPDATE SET state = excluded.state, first_seen_at = excluded.first_seen_at, updated_at = excluded.updated_at",
    params![payload_ref, state, now],
)
.await?;
    Ok(())
}

fn gc_database_path(storage_root: &Path) -> PathBuf {
    let sessions = storage_root.join("sessions.db");
    if sessions.is_file() {
        return sessions;
    }
    let global = storage_root.join("global.db");
    if global.is_file() {
        return global;
    }
    sessions
}

#[cfg(test)]
mod tests;
