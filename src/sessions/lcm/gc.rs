use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use libsql::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{payload, schema, util, LcmError, LcmGcConfig};

const GC_PAYLOAD_PREFIX: &str = "[gc'd externalized payload:";
const GC_TOOL_OUTPUT_PREFIX: &str = "[gc'd externalized tool output:";
const LIVE_PREFIX_REWRITES: [(&str, &str); 3] = [
    ("[externalized payload:", GC_PAYLOAD_PREFIX),
    ("[externalized lcm ingest payload:", GC_PAYLOAD_PREFIX),
    ("[externalized tool output:", GC_TOOL_OUTPUT_PREFIX),
];
const GC_PREFIXES: [&str; 2] = [GC_PAYLOAD_PREFIX, GC_TOOL_OUTPUT_PREFIX];
const MAX_SAMPLES: usize = 20;

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
        self.errors.push(LcmGcError {
            payload_ref: payload_ref.to_string(),
            kind: kind.to_string(),
            detail,
        });
        self.status = if self.apply { "applied" } else { "dry_run" }.to_string();
    }

    fn batch_cap(&mut self, count: usize) {
        if count > 0 {
            self.deferred.count += count;
            self.deferred.reason = Some("batch_cap".to_string());
        }
    }
}

pub async fn referenced_payload_refs(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<BTreeSet<String>, LcmError> {
    let mut refs = BTreeSet::new();
    let mut rows = conn
        .query(
            "SELECT storage_kind, payload_ref, content, snippet_text, index_text, metadata_json
             FROM lcm_raw_messages
             WHERE (?1 = 'all' OR provider = ?1)
               AND (?2 IS NULL OR session_id = ?2)",
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
            if let Some(value) = value.as_deref() {
                refs.extend(extract_live_payload_refs_from_text(value));
            }
        }
    }
    Ok(refs)
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

pub async fn all_payload_metadata_refs(conn: &Connection) -> Result<BTreeSet<String>, LcmError> {
    let mut refs = BTreeSet::new();
    let mut rows = conn
        .query("SELECT payload_ref FROM lcm_external_payloads", ())
        .await?;
    while let Some(row) = rows.next().await? {
        refs.insert(row.get(0)?);
    }
    Ok(refs)
}

pub async fn payload_metadata_refs_for_scope(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
) -> Result<BTreeSet<String>, LcmError> {
    let mut refs = BTreeSet::new();
    let mut rows = conn
        .query(
            "SELECT payload_ref
             FROM lcm_external_payloads
             WHERE (?1 = 'all' OR provider = ?1)
               AND (?2 IS NULL OR session_id = ?2)",
            params![provider, util::opt_text(session_id)],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        refs.insert(row.get(0)?);
    }
    Ok(refs)
}

async fn payload_metadata_bytes(conn: &Connection) -> Result<BTreeMap<String, u64>, LcmError> {
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
    conn: &Connection,
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
    cfg: &LcmGcConfig,
    now: i64,
) -> Result<LcmGcReport, LcmError> {
    run_payload_gc_with_apply(conn, storage_root, provider, session_id, cfg, false, now).await
}

pub async fn run_payload_gc_with_apply(
    conn: &Connection,
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

    if apply && cfg.backup_before_reap {
        checkpoint_wal_for_backup(conn).await?;
        report.backup = Some(backup_database(
            &gc_database_path(storage_root),
            storage_root,
        )?);
    }

    let dir = payload::existing_payload_dir(storage_root)?;
    let all_metadata_refs = all_payload_metadata_refs(conn).await?;
    let scoped_metadata_refs = payload_metadata_refs_for_scope(conn, provider, session_id).await?;
    let referenced = referenced_payload_refs(conn, provider, session_id).await?;
    let metadata_bytes = payload_metadata_bytes(conn).await?;

    let mut remaining = cfg.max_batch_size.max(1);
    // Orphan files have no metadata row, so they cannot be attributed to a
    // provider/session. Include them in every scoped GC preview/apply just as
    // the payload-health surface includes them for scoped drill-downs.
    reap_orphan_files(
        &dir,
        &all_metadata_refs,
        now,
        &cfg,
        apply,
        &mut remaining,
        &mut report,
    )?;
    reap_unreferenced_metadata(
        conn,
        storage_root,
        &scoped_metadata_refs,
        &referenced,
        &metadata_bytes,
        now,
        &cfg,
        apply,
        &mut remaining,
        &mut report,
    )
    .await?;
    reap_missing_metadata(
        conn,
        storage_root,
        &all_metadata_refs,
        &referenced,
        now,
        &cfg,
        apply,
        &mut remaining,
        &mut report,
    )
    .await?;
    rewrite_dangling_placeholders(
        conn,
        &dir,
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

pub fn reap_orphan_files(
    dir: &Path,
    metadata_refs: &BTreeSet<String>,
    now: i64,
    cfg: &LcmGcConfig,
    apply: bool,
    remaining: &mut usize,
    report: &mut LcmGcReport,
) -> Result<(), LcmError> {
    let mut candidates = Vec::new();
    for entry in fs::read_dir(dir).map_err(|err| LcmError::Io(err.to_string()))? {
        let entry = entry.map_err(|err| LcmError::Io(err.to_string()))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !is_payload_filename(&name) || payload::validate_payload_ref(&name).is_err() {
            continue;
        }
        if metadata_refs.contains(&name) {
            continue;
        }
        let path = dir.join(&name);
        payload::ensure_contained(dir, &path)?;
        let metadata = fs::symlink_metadata(&path).map_err(|err| LcmError::Io(err.to_string()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let age = now.saturating_sub(file_mtime_seconds(&metadata));
        if age < cfg.grace_seconds as i64 {
            report.deferred.count += 1;
            report
                .deferred
                .reason
                .get_or_insert_with(|| "within_grace".to_string());
            continue;
        }
        candidates.push((name, metadata.len()));
    }
    for (payload_ref, bytes) in candidates {
        if *remaining == 0 {
            report.batch_cap(1);
            continue;
        }
        if apply {
            match payload::safe_remove_payload_file(dir, &payload_ref) {
                Ok(true) => {
                    report.totals.files += 1;
                    report.totals.bytes = report.totals.bytes.saturating_add(bytes);
                }
                Ok(false) => {}
                Err(err) => {
                    report.add_error(&payload_ref, "orphan_remove_failed", err.to_string());
                    continue;
                }
            }
        }
        report.orphans.add(&payload_ref, bytes);
        *remaining -= 1;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn reap_unreferenced_metadata(
    conn: &Connection,
    storage_root: &Path,
    metadata_refs: &BTreeSet<String>,
    referenced: &BTreeSet<String>,
    metadata_bytes: &BTreeMap<String, u64>,
    now: i64,
    cfg: &LcmGcConfig,
    apply: bool,
    remaining: &mut usize,
    report: &mut LcmGcReport,
) -> Result<(), LcmError> {
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
            match payload::delete_external_payload(
                conn,
                storage_root,
                payload_ref,
                &payload::DeleteOpts::default(),
            )
            .await
            {
                Ok(outcome) => {
                    if outcome.file_removed {
                        report.totals.files += 1;
                        report.totals.bytes =
                            report.totals.bytes.saturating_add(outcome.bytes_freed);
                    }
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

#[allow(clippy::too_many_arguments)]
async fn reap_missing_metadata(
    conn: &Connection,
    storage_root: &Path,
    metadata_refs: &BTreeSet<String>,
    referenced: &BTreeSet<String>,
    now: i64,
    cfg: &LcmGcConfig,
    apply: bool,
    remaining: &mut usize,
    report: &mut LcmGcReport,
) -> Result<(), LcmError> {
    let dir = payload::existing_payload_dir(storage_root)?;
    for payload_ref in metadata_refs.intersection(referenced) {
        let path = dir.join(payload_ref);
        payload::ensure_contained(&dir, &path)?;
        if fs::symlink_metadata(&path).is_ok_and(|m| m.is_file() && !m.file_type().is_symlink()) {
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
        match payload::delete_external_payload(
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
            Ok(outcome) => {
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
    conn: &Connection,
    dir: &Path,
    metadata_refs: &BTreeSet<String>,
    provider: &str,
    session_id: Option<&str>,
    apply: bool,
    report: &mut LcmGcReport,
) -> Result<(), LcmError> {
    let referenced = referenced_payload_refs(conn, provider, session_id).await?;
    for payload_ref in referenced.difference(metadata_refs) {
        let path = dir.join(payload_ref);
        payload::ensure_contained(dir, &path)?;
        if fs::symlink_metadata(&path).is_ok_and(|m| m.is_file() && !m.file_type().is_symlink()) {
            continue;
        }
        let changed = if apply {
            tombstone_dangling_ref(conn, payload_ref, provider, session_id).await?
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

async fn tombstone_dangling_ref(
    conn: &Connection,
    payload_ref: &str,
    provider: &str,
    session_id: Option<&str>,
) -> Result<usize, LcmError> {
    conn.execute("BEGIN IMMEDIATE", ()).await?;
    let result: Result<usize, LcmError> = async {
        let mut rows = conn
            .query(
                "SELECT store_id, content, snippet_text, index_text, metadata_json
                 FROM lcm_raw_messages
                 WHERE provider = ?1 AND (?2 IS NULL OR session_id = ?2)
                   AND (content LIKE ?3 OR snippet_text LIKE ?3 OR index_text LIKE ?3 OR metadata_json LIKE ?3)",
                params![provider, util::opt_text(session_id), format!("%{payload_ref}%")],
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
                if tombstoned != text { changed += 1; }
                tombstoned
            });
            let new_snippet = tombstone_placeholder_in_text(&snippet_text, payload_ref);
            if new_snippet != snippet_text { changed += 1; }
            let new_index = tombstone_placeholder_in_text(&index_text, payload_ref);
            if new_index != index_text { changed += 1; }
            let new_metadata = metadata_json.map(|text| {
                let tombstoned = tombstone_placeholder_in_text(&text, payload_ref);
                if tombstoned != text { changed += 1; }
                tombstoned
            });
            if changed > 0 {
                updates.push((store_id, new_content, new_snippet, new_index, new_metadata, changed));
            }
        }
        let mut total = 0usize;
        for (store_id, content, snippet_text, index_text, metadata_json, changed) in updates {
            conn.execute(
                "UPDATE lcm_raw_messages
                 SET content = ?2, snippet_text = ?3, index_text = ?4, metadata_json = ?5
                 WHERE store_id = ?1",
                params![store_id, util::opt_text(content.as_deref()), snippet_text, index_text, util::opt_text(metadata_json.as_deref())],
            )
            .await?;
            total += changed;
        }
        Ok(total)
    }
    .await;
    match result {
        Ok(total) => {
            conn.execute("COMMIT", ()).await?;
            Ok(total)
        }
        Err(err) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            Err(err)
        }
    }
}

async fn gc_mark(conn: &Connection, payload_ref: &str) -> Result<Option<(String, i64)>, LcmError> {
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

async fn upsert_gc_mark(
    conn: &Connection,
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

fn is_payload_filename(name: &str) -> bool {
    name.len() == "payload_".len() + 64 + ".payload".len()
        && name.starts_with("payload_")
        && name.ends_with(".payload")
        && name["payload_".len().."payload_".len() + 64]
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
}

#[cfg(unix)]
fn file_mtime_seconds(metadata: &fs::Metadata) -> i64 {
    use std::os::unix::fs::MetadataExt;
    metadata.mtime()
}

#[cfg(not(unix))]
fn file_mtime_seconds(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
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

fn backup_database(db_path: &Path, storage_root: &Path) -> Result<Value, LcmError> {
    let backup_dir = storage_root.join("lcm-clean-backups");
    fs::create_dir_all(&backup_dir).map_err(|err| LcmError::Io(err.to_string()))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    let backup_path = backup_dir.join(format!("sessions-gc-{stamp}-{}.db", std::process::id()));
    let byte_count = copy_sqlite_file_set(db_path, &backup_path)?;
    Ok(json!({ "ok": true, "path": backup_path, "byte_count": byte_count }))
}

fn copy_sqlite_file_set(db_path: &Path, backup_path: &Path) -> Result<u64, LcmError> {
    let mut byte_count =
        fs::copy(db_path, backup_path).map_err(|err| LcmError::Io(err.to_string()))?;
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
            "WAL checkpoint incomplete before GC backup: busy={busy}, log_frames={log_frames}, checkpointed_frames={checkpointed_frames}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use libsql::Connection;

    use crate::global_db::GlobalDb;
    use crate::sessions::lcm::schema;

    use super::*;

    const PROVIDER: &str = "cursor";
    const PRIMARY_REF: &str =
        "payload_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.payload";
    const SECONDARY_REF: &str =
        "payload_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.payload";

    struct TestStore {
        _temp: tempfile::TempDir,
        storage_root: PathBuf,
        conn: Connection,
    }

    async fn test_store() -> Result<TestStore, String> {
        let temp = tempfile::tempdir().map_err(|err| format!("create tempdir: {err}"))?;
        let storage_root = temp.path().to_path_buf();
        let db_path = storage_root.join("sessions.db");
        let _global = GlobalDb::open_at(&db_path)
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
            .map_err(|err| format!("set test busy timeout: {err}"))?;
        schema::ensure_lcm_schema(&conn)
            .await
            .map_err(|err| format!("ensure lcm schema: {err}"))?;
        Ok(TestStore {
            _temp: temp,
            storage_root,
            conn,
        })
    }

    async fn insert_session(
        conn: &Connection,
        storage_root: &Path,
        session_id: &str,
    ) -> Result<(), String> {
        let project_key = storage_root.to_string_lossy().to_string();
        conn.execute(
            "INSERT INTO sessions (provider, session_id, project_key, project_path, title, started_at)
             VALUES (?1, ?2, ?3, ?3, ?4, 1)
             ON CONFLICT(provider, session_id) DO NOTHING",
            params![PROVIDER, session_id, project_key, session_id],
        )
        .await
        .map_err(|err| format!("insert session {session_id}: {err}"))?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_raw_message(
        conn: &Connection,
        session_id: &str,
        message_id: &str,
        storage_kind: &str,
        payload_ref: Option<&str>,
        content: Option<&str>,
        snippet_text: &str,
        index_text: &str,
        metadata_json: Option<&str>,
    ) -> Result<(), String> {
        conn.execute(
            "INSERT INTO lcm_raw_messages (
                provider, message_id, session_id, role, ordinal, timestamp,
                content, content_hash, storage_kind, payload_ref, snippet_text,
                index_text, legacy_source, legacy_truncated, metadata_json
             ) VALUES (?1, ?2, ?3, 'assistant', 1, 2, ?4, ?5, ?6, ?7, ?8, ?9, 0, 0, ?10)",
            params![
                PROVIDER,
                message_id,
                session_id,
                util::opt_text(content),
                format!("{message_id}-hash"),
                storage_kind,
                payload_ref,
                snippet_text,
                index_text,
                metadata_json
            ],
        )
        .await
        .map_err(|err| format!("insert raw message {message_id}: {err}"))?;
        Ok(())
    }

    async fn seed_payload(
        store: &TestStore,
        message_id: &str,
        content: &str,
    ) -> Result<String, String> {
        insert_session(&store.conn, &store.storage_root, "session-a").await?;
        let payload_ref = payload::write_external_payload(
            &store.storage_root,
            PROVIDER,
            "session-a",
            message_id,
            "message",
            content,
            None,
        )
        .map_err(|err| err.to_string())?;
        payload::upsert_payload_metadata(&store.conn, &payload_ref)
            .await
            .map_err(|err| err.to_string())?;
        let placeholder = format!(
            "[externalized payload: bytes={} ref={}; content]",
            content.len(),
            payload_ref.payload_ref
        );
        insert_raw_message(
            &store.conn,
            "session-a",
            message_id,
            "external",
            Some(&payload_ref.payload_ref),
            None,
            &placeholder,
            &placeholder,
            Some(&placeholder),
        )
        .await?;
        Ok(payload_ref.payload_ref)
    }

    fn payload_path(store: &TestStore, payload_ref: &str) -> PathBuf {
        payload::payload_dir(&store.storage_root).join(payload_ref)
    }

    async fn drop_raw_reference(store: &TestStore, payload_ref: &str) -> Result<(), String> {
        store
            .conn
            .execute(
                "DELETE FROM lcm_raw_messages WHERE payload_ref = ?1",
                params![payload_ref],
            )
            .await
            .map_err(|err| format!("drop raw reference: {err}"))?;
        Ok(())
    }

    async fn insert_gc_mark(
        store: &TestStore,
        payload_ref: &str,
        state: &str,
        first_seen_at: i64,
    ) -> Result<(), String> {
        store
            .conn
            .execute(
                "INSERT INTO lcm_gc_marks(payload_ref, state, first_seen_at, updated_at)
                 VALUES (?1, ?2, ?3, ?3)",
                params![payload_ref, state, first_seen_at],
            )
            .await
            .map_err(|err| format!("insert gc mark: {err}"))?;
        Ok(())
    }

    #[test]
    fn tombstone_helper_rewrites_all_live_prefixes() {
        let cases = [
            (
                format!("[externalized payload: bytes=12 ref={PRIMARY_REF}; note=body]"),
                format!("[gc'd externalized payload: bytes=12 ref={PRIMARY_REF}; note=body]"),
            ),
            (
                format!("[externalized lcm ingest payload: bytes=12 ref={PRIMARY_REF}; note=body]"),
                format!("[gc'd externalized payload: bytes=12 ref={PRIMARY_REF}; note=body]"),
            ),
            (
                format!("[externalized tool output: bytes=12 ref={PRIMARY_REF}; note=body]"),
                format!("[gc'd externalized tool output: bytes=12 ref={PRIMARY_REF}; note=body]"),
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(tombstone_placeholder_in_text(&input, PRIMARY_REF), expected);
        }
    }

    #[test]
    fn tombstone_helper_rewrites_repeated_refs_and_is_idempotent() {
        let input = format!(
            "one [externalized payload: bytes=12 ref={PRIMARY_REF}; a] two [externalized tool output: bytes=8 ref={PRIMARY_REF}; b]"
        );
        let expected = format!(
            "one [gc'd externalized payload: bytes=12 ref={PRIMARY_REF}; a] two [gc'd externalized tool output: bytes=8 ref={PRIMARY_REF}; b]"
        );
        assert_eq!(tombstone_placeholder_in_text(&input, PRIMARY_REF), expected);
        assert_eq!(
            tombstone_placeholder_in_text(&expected, PRIMARY_REF),
            expected
        );
    }

    #[tokio::test]
    async fn referenced_payload_refs_ignores_tombstoned_placeholders() -> Result<(), String> {
        let store = test_store().await?;
        insert_session(&store.conn, &store.storage_root, "session-a").await?;
        let live =
            format!("prefix [externalized payload: bytes=12 ref={PRIMARY_REF}; marker] suffix");
        let tombstoned = format!(
            "prefix [gc'd externalized payload: bytes=12 ref={SECONDARY_REF}; marker] suffix"
        );
        insert_raw_message(
            &store.conn,
            "session-a",
            "message-1",
            "inline",
            None,
            Some(&live),
            &live,
            &live,
            None,
        )
        .await?;
        insert_raw_message(
            &store.conn,
            "session-a",
            "message-2",
            "inline",
            None,
            Some(&tombstoned),
            &tombstoned,
            &tombstoned,
            None,
        )
        .await?;

        let refs = referenced_payload_refs(&store.conn, PROVIDER, Some("session-a"))
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(refs, BTreeSet::from([PRIMARY_REF.to_string()]));
        assert!(text_has_tombstoned_payload_ref(&tombstoned, SECONDARY_REF));
        Ok(())
    }

    #[tokio::test]
    async fn delete_external_payload_aborts_when_still_referenced() -> Result<(), String> {
        let store = test_store().await?;
        let payload_ref = seed_payload(&store, "message-1", "body to delete").await?;
        let Err(err) = payload::delete_external_payload(
            &store.conn,
            &store.storage_root,
            &payload_ref,
            &payload::DeleteOpts::default(),
        )
        .await
        else {
            return Err("live payload must not be deleted".to_string());
        };
        assert_eq!(err, LcmError::StillReferenced);
        assert!(payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_ok());
        assert!(payload::payload_dir(&store.storage_root)
            .join(&payload_ref)
            .is_file());
        Ok(())
    }

    #[tokio::test]
    async fn delete_external_payload_applies_db_then_file_and_is_idempotent() -> Result<(), String>
    {
        let store = test_store().await?;
        let payload_ref = seed_payload(&store, "message-1", "body to delete").await?;
        drop_raw_reference(&store, &payload_ref).await?;

        let outcome = payload::delete_external_payload(
            &store.conn,
            &store.storage_root,
            &payload_ref,
            &payload::DeleteOpts::default(),
        )
        .await
        .map_err(|err| err.to_string())?;
        assert!(outcome.metadata_row_existed);
        assert!(outcome.file_existed);
        assert!(outcome.file_removed);
        assert!(outcome.bytes_freed > 0);
        assert!(payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_err());
        assert!(!payload_path(&store, &payload_ref).exists());

        let second = payload::delete_external_payload(
            &store.conn,
            &store.storage_root,
            &payload_ref,
            &payload::DeleteOpts::default(),
        )
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(second, payload::DeleteOutcome::default());
        Ok(())
    }

    #[tokio::test]
    async fn delete_external_payload_db_only_leaves_orphan_for_crash_convergence(
    ) -> Result<(), String> {
        let store = test_store().await?;
        let payload_ref = seed_payload(&store, "message-1", "body to delete").await?;
        drop_raw_reference(&store, &payload_ref).await?;

        let outcome = payload::delete_external_payload(
            &store.conn,
            &store.storage_root,
            &payload_ref,
            &payload::DeleteOpts {
                rewrite_placeholders: true,
                remove_file: false,
                verify_hash: false,
            },
        )
        .await
        .map_err(|err| err.to_string())?;
        assert!(outcome.metadata_row_existed);
        assert!(outcome.file_existed);
        assert!(!outcome.file_removed);
        assert_eq!(outcome.bytes_freed, 0);
        assert!(payload_path(&store, &payload_ref).is_file());

        let metadata_refs = all_payload_metadata_refs(&store.conn)
            .await
            .map_err(|err| err.to_string())?;
        let file_mtime = file_mtime_seconds(
            &fs::symlink_metadata(payload_path(&store, &payload_ref))
                .map_err(|err| err.to_string())?,
        );
        let cfg = LcmGcConfig {
            grace_seconds: LcmGcConfig::MIN_GRACE_SECONDS,
            backup_before_reap: false,
            ..Default::default()
        }
        .normalized();
        let mut report = LcmGcReport::new(PROVIDER, None, &cfg, true, file_mtime);
        let mut remaining = 10;
        reap_orphan_files(
            &payload::payload_dir(&store.storage_root),
            &metadata_refs,
            file_mtime + LcmGcConfig::MIN_GRACE_SECONDS as i64,
            &cfg,
            true,
            &mut remaining,
            &mut report,
        )
        .map_err(|err| err.to_string())?;
        assert_eq!(report.orphans.count, 1);
        assert!(!payload_path(&store, &payload_ref).exists());
        Ok(())
    }

    #[tokio::test]
    async fn delete_external_payload_hash_gate_preserves_corrupted_payload() -> Result<(), String> {
        let store = test_store().await?;
        let payload_ref = seed_payload(&store, "message-1", "trusted body").await?;
        drop_raw_reference(&store, &payload_ref).await?;
        fs::write(payload_path(&store, &payload_ref), b"tampered body")
            .map_err(|err| err.to_string())?;

        let Err(err) = payload::delete_external_payload(
            &store.conn,
            &store.storage_root,
            &payload_ref,
            &payload::DeleteOpts::default(),
        )
        .await
        else {
            return Err("corrupted payload must not be reaped".to_string());
        };
        assert_eq!(err, LcmError::PayloadIntegrityMismatch);
        assert!(payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_ok());
        assert!(payload_path(&store, &payload_ref).is_file());
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn delete_external_payload_rejects_symlink_payload_at_hash_gate() -> Result<(), String> {
        let store = test_store().await?;
        let payload_ref = seed_payload(&store, "message-1", "trusted body").await?;
        drop_raw_reference(&store, &payload_ref).await?;
        let path = payload_path(&store, &payload_ref);
        fs::remove_file(&path).map_err(|err| err.to_string())?;
        let outside = store.storage_root.join("outside-payload-body.txt");
        fs::write(&outside, b"trusted body").map_err(|err| err.to_string())?;
        std::os::unix::fs::symlink(&outside, &path).map_err(|err| err.to_string())?;

        let Err(err) = payload::delete_external_payload(
            &store.conn,
            &store.storage_root,
            &payload_ref,
            &payload::DeleteOpts::default(),
        )
        .await
        else {
            return Err("symlink payload must be rejected before DB mutation".to_string());
        };
        assert_eq!(err, LcmError::InvalidPayloadRef);
        assert!(payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_ok());
        assert!(fs::symlink_metadata(&path)
            .map_err(|err| err.to_string())?
            .file_type()
            .is_symlink());
        Ok(())
    }

    #[tokio::test]
    async fn delete_external_payload_rejects_invalid_refs() -> Result<(), String> {
        let store = test_store().await?;
        for invalid in [
            "",
            ".",
            "..",
            "../evil",
            "/etc/passwd",
            "payload_../x.payload",
        ] {
            let Err(err) = payload::delete_external_payload(
                &store.conn,
                &store.storage_root,
                invalid,
                &payload::DeleteOpts::default(),
            )
            .await
            else {
                return Err("invalid ref should fail before path access".to_string());
            };
            assert_eq!(err, LcmError::InvalidPayloadRef, "invalid ref {invalid}");
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn safe_remove_payload_file_rejects_symlink_and_directory_payloads() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|err| err.to_string())?;
        let dir = temp.path();
        let outside = temp.path().join("outside.txt");
        fs::write(&outside, b"outside").map_err(|err| err.to_string())?;
        std::os::unix::fs::symlink(&outside, dir.join(PRIMARY_REF))
            .map_err(|err| err.to_string())?;
        let Err(err) = payload::safe_remove_payload_file(dir, PRIMARY_REF) else {
            return Err("symlink payload should not be removed".to_string());
        };
        assert_eq!(err, LcmError::InvalidPayloadRef);
        assert!(outside.is_file());

        fs::create_dir(dir.join(SECONDARY_REF)).map_err(|err| err.to_string())?;
        let Err(err) = payload::safe_remove_payload_file(dir, SECONDARY_REF) else {
            return Err("directory payload should not be removed".to_string());
        };
        assert_eq!(err, LcmError::InvalidPayloadRef);
        assert!(dir.join(SECONDARY_REF).is_dir());
        Ok(())
    }

    #[tokio::test]
    async fn unreferenced_payload_two_scan_reaps_after_grace() -> Result<(), String> {
        let store = test_store().await?;
        let payload_ref = seed_payload(&store, "message-1", "body to delete").await?;
        drop_raw_reference(&store, &payload_ref).await?;
        let cfg = LcmGcConfig {
            grace_seconds: LcmGcConfig::MIN_GRACE_SECONDS,
            backup_before_reap: false,
            ..Default::default()
        }
        .normalized();
        let first = run_payload_gc_with_apply(
            &store.conn,
            &store.storage_root,
            PROVIDER,
            None,
            &cfg,
            true,
            1_000,
        )
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(first.unreferenced.count, 0);
        assert!(payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_ok());

        let second = run_payload_gc_with_apply(
            &store.conn,
            &store.storage_root,
            PROVIDER,
            None,
            &cfg,
            true,
            1_000 + LcmGcConfig::MIN_GRACE_SECONDS as i64,
        )
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(second.unreferenced.count, 1);
        assert!(payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_err());
        assert!(!payload::payload_dir(&store.storage_root)
            .join(&payload_ref)
            .exists());
        Ok(())
    }

    #[tokio::test]
    async fn session_scoped_unreferenced_payload_reaps_after_grace() -> Result<(), String> {
        let store = test_store().await?;
        let payload_ref = seed_payload(&store, "message-1", "body to delete").await?;
        drop_raw_reference(&store, &payload_ref).await?;
        let cfg = LcmGcConfig {
            grace_seconds: LcmGcConfig::MIN_GRACE_SECONDS,
            backup_before_reap: false,
            ..Default::default()
        }
        .normalized();
        let first = run_payload_gc_with_apply(
            &store.conn,
            &store.storage_root,
            PROVIDER,
            Some("session-a"),
            &cfg,
            true,
            1_000,
        )
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(first.unreferenced.count, 0);
        assert!(payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_ok());

        let second = run_payload_gc_with_apply(
            &store.conn,
            &store.storage_root,
            PROVIDER,
            Some("session-a"),
            &cfg,
            true,
            1_000 + LcmGcConfig::MIN_GRACE_SECONDS as i64,
        )
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(second.unreferenced.count, 1);
        assert!(payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_err());
        assert!(!payload::payload_dir(&store.storage_root)
            .join(&payload_ref)
            .exists());
        Ok(())
    }

    #[tokio::test]
    async fn run_payload_gc_dry_run_does_not_mutate() -> Result<(), String> {
        let store = test_store().await?;
        let payload_ref = seed_payload(&store, "message-1", "body to delete").await?;
        drop_raw_reference(&store, &payload_ref).await?;
        insert_gc_mark(&store, &payload_ref, "unreferenced", 1).await?;
        let cfg = LcmGcConfig {
            grace_seconds: LcmGcConfig::MIN_GRACE_SECONDS,
            backup_before_reap: false,
            ..Default::default()
        }
        .normalized();
        let report = run_payload_gc(
            &store.conn,
            &store.storage_root,
            PROVIDER,
            None,
            &cfg,
            1_000,
        )
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(report.status, "dry_run");
        assert_eq!(report.unreferenced.count, 1);
        assert!(payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_ok());
        assert!(payload::payload_dir(&store.storage_root)
            .join(&payload_ref)
            .is_file());
        Ok(())
    }

    #[tokio::test]
    async fn orphan_phase_honors_mtime_grace_then_reaps() -> Result<(), String> {
        let store = test_store().await?;
        let payload_ref = seed_payload(&store, "message-1", "orphan body").await?;
        drop_raw_reference(&store, &payload_ref).await?;
        store
            .conn
            .execute(
                "DELETE FROM lcm_external_payloads WHERE payload_ref = ?1",
                params![payload_ref.as_str()],
            )
            .await
            .map_err(|err| err.to_string())?;
        let metadata_refs = all_payload_metadata_refs(&store.conn)
            .await
            .map_err(|err| err.to_string())?;
        let file_mtime = file_mtime_seconds(
            &fs::symlink_metadata(payload_path(&store, &payload_ref))
                .map_err(|err| err.to_string())?,
        );
        let cfg = LcmGcConfig {
            grace_seconds: LcmGcConfig::MIN_GRACE_SECONDS,
            backup_before_reap: false,
            ..Default::default()
        }
        .normalized();
        let mut report = LcmGcReport::new(PROVIDER, None, &cfg, true, file_mtime);
        let mut remaining = 10;
        reap_orphan_files(
            &payload::payload_dir(&store.storage_root),
            &metadata_refs,
            file_mtime + LcmGcConfig::MIN_GRACE_SECONDS as i64 - 1,
            &cfg,
            true,
            &mut remaining,
            &mut report,
        )
        .map_err(|err| err.to_string())?;
        assert_eq!(report.orphans.count, 0);
        assert!(payload_path(&store, &payload_ref).is_file());

        reap_orphan_files(
            &payload::payload_dir(&store.storage_root),
            &metadata_refs,
            file_mtime + LcmGcConfig::MIN_GRACE_SECONDS as i64,
            &cfg,
            true,
            &mut remaining,
            &mut report,
        )
        .map_err(|err| err.to_string())?;
        assert_eq!(report.orphans.count, 1);
        assert!(!payload_path(&store, &payload_ref).exists());
        Ok(())
    }

    #[tokio::test]
    async fn missing_metadata_defaults_to_report_only_and_opt_in_tombstones_after_window(
    ) -> Result<(), String> {
        let store = test_store().await?;
        let payload_ref = seed_payload(&store, "message-1", "missing body").await?;
        fs::remove_file(payload_path(&store, &payload_ref)).map_err(|err| err.to_string())?;
        let cfg = LcmGcConfig {
            reap_missing_enabled: false,
            reap_missing_after: 10,
            backup_before_reap: false,
            ..Default::default()
        }
        .normalized();
        let first = run_payload_gc_with_apply(
            &store.conn,
            &store.storage_root,
            PROVIDER,
            None,
            &cfg,
            true,
            100,
        )
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(first.missing.count, 1);
        let later = run_payload_gc_with_apply(
            &store.conn,
            &store.storage_root,
            PROVIDER,
            None,
            &cfg,
            true,
            1_000,
        )
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(later.missing.count, 1);
        assert!(payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_ok());

        let cfg = LcmGcConfig {
            reap_missing_enabled: true,
            reap_missing_after: 10,
            backup_before_reap: false,
            ..Default::default()
        }
        .normalized();
        let marked = run_payload_gc_with_apply(
            &store.conn,
            &store.storage_root,
            PROVIDER,
            None,
            &cfg,
            true,
            2_000,
        )
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(marked.missing.count, 1);
        assert!(payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_ok());

        let reaped = run_payload_gc_with_apply(
            &store.conn,
            &store.storage_root,
            PROVIDER,
            None,
            &cfg,
            true,
            2_010,
        )
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(reaped.missing.count, 1);
        assert!(payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_err());
        let refs = referenced_payload_refs(&store.conn, PROVIDER, None)
            .await
            .map_err(|err| err.to_string())?;
        assert!(!refs.contains(&payload_ref));
        Ok(())
    }

    #[tokio::test]
    async fn missing_metadata_clears_mark_when_file_reappears() -> Result<(), String> {
        let store = test_store().await?;
        let payload_ref = seed_payload(&store, "message-1", "restored body").await?;
        fs::remove_file(payload_path(&store, &payload_ref)).map_err(|err| err.to_string())?;
        let cfg = LcmGcConfig {
            reap_missing_enabled: true,
            reap_missing_after: 10,
            backup_before_reap: false,
            ..Default::default()
        }
        .normalized();
        run_payload_gc_with_apply(
            &store.conn,
            &store.storage_root,
            PROVIDER,
            None,
            &cfg,
            true,
            100,
        )
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(
            gc_mark(&store.conn, &payload_ref)
                .await
                .map_err(|err| err.to_string())?
                .map(|mark| mark.0),
            Some("missing".to_string())
        );

        fs::write(payload_path(&store, &payload_ref), b"restored body")
            .map_err(|err| err.to_string())?;
        let report = run_payload_gc_with_apply(
            &store.conn,
            &store.storage_root,
            PROVIDER,
            None,
            &cfg,
            true,
            1_000,
        )
        .await
        .map_err(|err| err.to_string())?;

        assert_eq!(report.missing.count, 0);
        assert!(gc_mark(&store.conn, &payload_ref)
            .await
            .map_err(|err| err.to_string())?
            .is_none());
        assert!(payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn run_payload_gc_isolates_corrupted_ref_errors_while_reaping_orphans(
    ) -> Result<(), String> {
        let store = test_store().await?;
        let corrupted_ref = seed_payload(&store, "message-1", "trusted body").await?;
        drop_raw_reference(&store, &corrupted_ref).await?;
        fs::write(payload_path(&store, &corrupted_ref), b"tampered body")
            .map_err(|err| err.to_string())?;
        insert_gc_mark(&store, &corrupted_ref, "unreferenced", 1).await?;

        let orphan_a =
            "payload_1111111111111111111111111111111111111111111111111111111111111111.payload";
        let orphan_b =
            "payload_2222222222222222222222222222222222222222222222222222222222222222.payload";
        fs::write(payload_path(&store, orphan_a), b"orphan-a").map_err(|err| err.to_string())?;
        fs::write(payload_path(&store, orphan_b), b"orphan-b").map_err(|err| err.to_string())?;
        let orphan_a_mtime = file_mtime_seconds(
            &fs::symlink_metadata(payload_path(&store, orphan_a)).map_err(|err| err.to_string())?,
        );
        let orphan_b_mtime = file_mtime_seconds(
            &fs::symlink_metadata(payload_path(&store, orphan_b)).map_err(|err| err.to_string())?,
        );
        let newest_orphan_mtime = orphan_a_mtime.max(orphan_b_mtime);
        let cfg = LcmGcConfig {
            grace_seconds: LcmGcConfig::MIN_GRACE_SECONDS,
            backup_before_reap: false,
            ..Default::default()
        }
        .normalized();
        let report = run_payload_gc_with_apply(
            &store.conn,
            &store.storage_root,
            PROVIDER,
            None,
            &cfg,
            true,
            newest_orphan_mtime + LcmGcConfig::MIN_GRACE_SECONDS as i64,
        )
        .await
        .map_err(|err| err.to_string())?;

        assert_eq!(report.orphans.count, 2);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].payload_ref, corrupted_ref);
        assert_eq!(report.errors[0].kind, "integrity_mismatch");
        assert_eq!(
            schema::get_gc_meta(&store.conn, "last_gc_status")
                .await
                .map_err(|err| err.to_string())?
                .as_deref(),
            Some("partial")
        );
        assert!(payload::load_payload_metadata(&store.conn, &corrupted_ref)
            .await
            .is_ok());
        assert!(!payload_path(&store, orphan_a).exists());
        assert!(!payload_path(&store, orphan_b).exists());
        Ok(())
    }
}
