use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::time::Instant;

use serde::{Deserialize, Serialize};

#[cfg(test)]
use tracedecay_runtime_core::db::engine::{Connection, TransactionBehavior};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, Value as SqlValue, params};

use super::{
    LCM_SCAN_PAGE_MAX_BYTES, LCM_SCAN_PAGE_ROWS, LcmError, LcmGcConfig, maintenance, payload,
    schema, util,
};

mod orphan_scan;
mod pending_delete;
mod placeholder_scan;
use orphan_scan::{payload_file_present, preview_orphan_files, stage_orphan_files};
pub use pending_delete::{
    PayloadDeleteDrain, drain_pending_payload_delete_in_transaction,
    drain_pending_payload_deletes_in_transaction, stage_payload_delete,
};
pub(crate) use placeholder_scan::{
    PlaceholderScanScope, PlaceholderTextRow, all_placeholder_like_patterns,
    any_placeholder_text_row, count_placeholder_text_rows, gc_prefix_like_patterns,
    gc_prefix_ref_like_patterns, live_prefix_like_patterns, placeholder_text_rows_by_store_id,
    scan_placeholder_text_rows, scan_placeholder_text_rows_between,
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
const GC_MARK_UPSERT_BINDS_PER_ROW: usize = 4;

pub(crate) fn is_known_payload_placeholder_prefix(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    LIVE_PREFIX_REWRITES
        .iter()
        .any(|(prefix, _)| lower.starts_with(prefix))
        || GC_PREFIXES.iter().any(|prefix| lower.starts_with(prefix))
}

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

/// The raw row a payload's metadata names as its owner. Payload refs hash
/// `(provider, session_id, message_id, content)`, so a payload has exactly one
/// owner row, and expansion already refuses every other row
/// (`payload::expand_payload`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PayloadOwner {
    pub(crate) payload_ref: String,
    pub(crate) provider: String,
    pub(crate) session_id: String,
    pub(crate) message_id: String,
}

/// Owner probes per query. Owner rows carry message bodies, so a chunk stays
/// far below the SQL channel's materialization limit.
const OWNER_PROBE_CHUNK: usize = 64;

/// The payloads among `owners` whose owner row still references them: the
/// row stores the payload as its external body, or its text carries a live
/// placeholder naming it. Each probe is one unique-index lookup, so the cost
/// follows the candidates, never the store.
pub(crate) async fn owner_referenced_payloads(
    conn: &(impl QueryExecutor + ?Sized),
    owners: &[PayloadOwner],
) -> Result<BTreeSet<String>, LcmError> {
    let mut referenced = BTreeSet::new();
    for chunk in owners.chunks(OWNER_PROBE_CHUNK) {
        let wanted = serde_json::to_string(
            &chunk
                .iter()
                .map(|owner| {
                    [
                        owner.payload_ref.as_str(),
                        owner.provider.as_str(),
                        owner.message_id.as_str(),
                        owner.session_id.as_str(),
                    ]
                })
                .collect::<Vec<_>>(),
        )
        .map_err(|error| LcmError::Db(format!("encode payload owner probe: {error}")))?;
        let mut rows = conn
            .query(
                "SELECT json_extract(wanted.value, '$[0]'), r.storage_kind, r.payload_ref,
                        r.content, r.placeholder_text, r.metadata_json
                 FROM json_each(?1) AS wanted
                 JOIN lcm_raw_messages AS r
                   ON r.provider = json_extract(wanted.value, '$[1]')
                  AND r.message_id = json_extract(wanted.value, '$[2]')
                  AND r.session_id = json_extract(wanted.value, '$[3]')",
                params![wanted],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            let payload_ref: String = row.get(0)?;
            let storage_kind: String = row.get(1)?;
            let stored_ref: Option<String> = row.get(2)?;
            let stores_body =
                storage_kind == "external" && stored_ref.as_deref() == Some(payload_ref.as_str());
            let mut texts = [row.get::<Option<String>>(3)?, row.get(4)?, row.get(5)?].into_iter();
            if stores_body
                || texts.any(|text| {
                    text.is_some_and(|text| {
                        extract_live_payload_refs_from_text(&text).contains(&payload_ref)
                    })
                })
            {
                referenced.insert(payload_ref);
            }
        }
    }
    Ok(referenced)
}

/// The payloads among `payload_refs` with metadata whose owner row still
/// references them.
pub(crate) async fn owner_referenced_metadata(
    conn: &(impl QueryExecutor + ?Sized),
    payload_refs: &BTreeSet<String>,
) -> Result<BTreeSet<String>, LcmError> {
    let payload_refs = payload_refs.iter().cloned().collect::<Vec<_>>();
    let owners = payload_owners(conn, &payload_refs).await?;
    owner_referenced_payloads(conn, &owners).await
}

/// Owners of the payloads in `payload_refs` that still have metadata.
async fn payload_owners(
    conn: &(impl QueryExecutor + ?Sized),
    payload_refs: &[String],
) -> Result<Vec<PayloadOwner>, LcmError> {
    let mut owners = Vec::with_capacity(payload_refs.len());
    for chunk in payload_refs.chunks(util::SQLITE_IN_BATCH_SIZE) {
        let sql = format!(
            "SELECT payload_ref, provider, session_id, message_id
             FROM lcm_external_payloads WHERE payload_ref IN ({})",
            util::sql_in_placeholders(chunk.len())
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
            owners.push(PayloadOwner {
                payload_ref: row.get(0)?,
                provider: row.get(1)?,
                session_id: row.get(2)?,
                message_id: row.get(3)?,
            });
        }
    }
    Ok(owners)
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

/// Read-only payload GC preview. Mutation runs through
/// [`run_payload_gc_in_transaction`]; this entry point never writes.
#[tracing::instrument(name = "sessions.lcm.gc.preview", level = "trace", skip_all)]
pub async fn run_payload_gc(
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
    let snapshot = read_payload_gc_snapshot(conn, storage_root, provider, session_id).await?;
    let mut remaining = cfg.max_batch_size.max(1);

    if let Some(dir) = dir.as_deref() {
        preview_orphan_files(
            dir,
            &snapshot.all_metadata_refs,
            now,
            &cfg,
            &mut remaining,
            &mut report,
        )?;
    }
    plan_unreferenced(conn, provider, session_id, &cfg, now)
        .await?
        .preview(&mut remaining, &mut report);
    let missing = plan_missing(
        conn,
        dir.as_deref(),
        &snapshot.scoped_metadata_refs,
        &mut report,
    )
    .await?;
    preview_missing_reaps(conn, &missing, &cfg, now, &mut remaining, &mut report).await?;
    snapshot.dangling.preview(&mut report);
    report.ended_at = now;

    Ok(report)
}

/// Unreferenced-payload candidates one GC pass acts on.
struct UnreferencedPlan {
    /// Marks whose payload metadata is gone; only unscoped passes see them.
    stale: Vec<String>,
    /// Due candidates whose owner row still references them.
    referenced: Vec<String>,
    /// Due candidates no owner row references, with their metadata size.
    unreferenced: Vec<(PayloadOwner, u64)>,
    within_grace: u64,
    /// Due candidates beyond this pass's batch.
    beyond_batch: u64,
}

impl UnreferencedPlan {
    fn record_deferred(&self, report: &mut LcmGcReport) {
        if self.within_grace > 0 {
            report.deferred.count += usize::try_from(self.within_grace).unwrap_or(usize::MAX);
            report
                .deferred
                .reason
                .get_or_insert_with(|| "within_grace".to_string());
        }
        report.batch_cap(usize::try_from(self.beyond_batch).unwrap_or(usize::MAX));
    }

    fn preview(&self, remaining: &mut usize, report: &mut LcmGcReport) {
        self.record_deferred(report);
        for (owner, bytes) in &self.unreferenced {
            if *remaining == 0 {
                report.batch_cap(1);
                continue;
            }
            report.unreferenced.add(&owner.payload_ref, *bytes);
            *remaining -= 1;
        }
    }
}

/// Reports the referenced missing payloads and counts the ones an applied
/// pass would reap.
async fn preview_missing_reaps(
    conn: &(impl QueryExecutor + ?Sized),
    missing: &MissingPlan,
    cfg: &LcmGcConfig,
    now: i64,
    remaining: &mut usize,
    report: &mut LcmGcReport,
) -> Result<(), LcmError> {
    for payload_ref in &missing.referenced {
        report.missing.add(payload_ref, 0);
    }
    if !cfg.reap_missing_enabled || cfg.reap_missing_after == 0 {
        return Ok(());
    }
    let marks = gc_marks(conn, &missing.referenced).await?;
    for payload_ref in &missing.referenced {
        let due = marks
            .get(payload_ref.as_str())
            .is_some_and(|(state, first_seen_at)| {
                state == "missing"
                    && now.saturating_sub(*first_seen_at) >= cfg.reap_missing_after as i64
            });
        if !due {
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

/// Scope predicate over a mark's metadata row aliased `e`. A mark without
/// metadata belongs to no provider, so only an unscoped pass selects it.
const MARK_SCOPE_SQL: &str =
    "(?1 = 'all' OR e.provider = ?1) AND (?2 IS NULL OR e.session_id = ?2)";

/// Reads the `unreferenced` marks past the grace window, oldest first, and
/// verifies each against its owner row. Marks still inside the window are only
/// counted. Every read is a range of the marks index or an owner-row lookup,
/// so a pass costs what changed since the candidates were recorded.
async fn plan_unreferenced(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
    cfg: &LcmGcConfig,
    now: i64,
) -> Result<UnreferencedPlan, LcmError> {
    let due_before = now.saturating_sub(i64::try_from(cfg.grace_seconds).unwrap_or(i64::MAX));
    let count = |comparison: &'static str| {
        format!(
            "SELECT COUNT(*) FROM lcm_gc_marks AS m
             LEFT JOIN lcm_external_payloads AS e ON e.payload_ref = m.payload_ref
             WHERE m.state = 'unreferenced' AND m.first_seen_at {comparison} ?3
               AND {MARK_SCOPE_SQL}"
        )
    };
    let marks_count = |comparison| {
        let sql = count(comparison);
        async move {
            util::fetch_i64(
                conn,
                &sql,
                params![provider, session_id, due_before],
                "payload GC mark count returned no row",
            )
            .await
            .map(|count| count.max(0) as u64)
        }
    };
    let within_grace = marks_count(">").await?;
    let due_total = marks_count("<=").await?;
    let batch = i64::try_from(cfg.max_batch_size.max(1)).unwrap_or(i64::MAX);
    let mut rows = conn
        .query(
            &format!(
                "SELECT m.payload_ref, e.provider, e.session_id, e.message_id, e.byte_count
                 FROM lcm_gc_marks AS m
                 LEFT JOIN lcm_external_payloads AS e ON e.payload_ref = m.payload_ref
                 WHERE m.state = 'unreferenced' AND m.first_seen_at <= ?3
                   AND {MARK_SCOPE_SQL}
                 ORDER BY m.first_seen_at, m.payload_ref
                 LIMIT ?4"
            ),
            params![provider, session_id, due_before, batch],
        )
        .await?;
    let mut stale = Vec::new();
    let mut due = Vec::new();
    while let Some(row) = rows.next().await? {
        let payload_ref: String = row.get(0)?;
        match (
            row.get::<Option<String>>(1)?,
            row.get::<Option<String>>(2)?,
            row.get::<Option<String>>(3)?,
        ) {
            (Some(provider), Some(session_id), Some(message_id)) => due.push((
                PayloadOwner {
                    payload_ref,
                    provider,
                    session_id,
                    message_id,
                },
                row.get::<Option<i64>>(4)?.unwrap_or_default().max(0) as u64,
            )),
            _ => stale.push(payload_ref),
        }
    }
    drop(rows);
    let examined = (stale.len() + due.len()) as u64;
    let owners = due
        .iter()
        .map(|(owner, _)| owner.clone())
        .collect::<Vec<_>>();
    let still_referenced = owner_referenced_payloads(conn, &owners).await?;
    let (referenced, unreferenced): (Vec<_>, Vec<_>) = due
        .into_iter()
        .partition(|(owner, _)| still_referenced.contains(&owner.payload_ref));
    Ok(UnreferencedPlan {
        stale,
        referenced: referenced
            .into_iter()
            .map(|(owner, _)| owner.payload_ref)
            .collect(),
        unreferenced,
        within_grace,
        beyond_batch: due_total.saturating_sub(examined),
    })
}

/// Referenced payloads whose file is missing, and `missing` marks whose file
/// is back.
struct MissingPlan {
    referenced: Vec<String>,
    restored: Vec<String>,
}

/// Stats every in-scope payload file and verifies the missing ones against
/// their owner rows.
///
/// ponytail: one `stat` per payload per pass; a file removed behind the
/// store's back has no other signal. Track payload file loss at its source
/// if payload counts ever make this measurable.
async fn plan_missing(
    conn: &(impl QueryExecutor + ?Sized),
    dir: Option<&Path>,
    metadata_refs: &BTreeSet<String>,
    report: &mut LcmGcReport,
) -> Result<MissingPlan, LcmError> {
    let mut missing = Vec::new();
    let mut present = BTreeSet::new();
    for payload_ref in metadata_refs {
        match payload_file_present(dir, payload_ref) {
            Ok(true) => {
                present.insert(payload_ref.clone());
            }
            Ok(false) => missing.push(payload_ref.clone()),
            Err(error) => report.add_error(payload_ref, "payload_stat_failed", error.to_string()),
        }
    }
    let owners = payload_owners(conn, &missing).await?;
    let referenced_set = owner_referenced_payloads(conn, &owners).await?;
    let referenced = missing
        .into_iter()
        .filter(|payload_ref| referenced_set.contains(payload_ref))
        .collect::<Vec<_>>();
    let mut rows = conn
        .query(
            "SELECT payload_ref FROM lcm_gc_marks WHERE state = 'missing'",
            (),
        )
        .await?;
    let mut restored = Vec::new();
    while let Some(row) = rows.next().await? {
        let payload_ref: String = row.get(0)?;
        if present.contains(&payload_ref) {
            restored.push(payload_ref);
        }
    }
    Ok(MissingPlan {
        referenced,
        restored,
    })
}

/// Live placeholders in rows written since the last applied pass that name a
/// payload with neither metadata nor a file.
struct DanglingPlan {
    refs: BTreeSet<String>,
    /// Rows carrying one of `refs`, re-read by the transaction that rewrites
    /// them.
    store_ids: Vec<i64>,
    scanned_through: i64,
    stat_errors: Vec<(String, String)>,
}

impl DanglingPlan {
    fn preview(&self, report: &mut LcmGcReport) {
        for (payload_ref, detail) in &self.stat_errors {
            report.add_error(payload_ref, "dangling_payload_stat_failed", detail.clone());
        }
        for payload_ref in &self.refs {
            report.dangling.add(payload_ref, 0);
        }
    }
}

pub(crate) const DANGLING_SCAN_CURSOR: &str = "dangling_scan_store_id";

/// Scans only the rows written since the last applied pass. An owner row
/// loses its placeholders when its payload is deleted, so a dangling
/// placeholder can only arrive with new text.
async fn plan_dangling(
    conn: &(impl QueryExecutor + ?Sized),
    dir: Option<&Path>,
    provider: &str,
    session_id: Option<&str>,
) -> Result<DanglingPlan, LcmError> {
    let after = schema::get_gc_meta(conn, DANGLING_SCAN_CURSOR)
        .await?
        .ok_or_else(|| LcmError::Db("payload GC dangling scan cursor is missing".to_string()))?
        .parse::<i64>()
        .map_err(|error| LcmError::Db(format!("payload GC dangling scan cursor: {error}")))?;
    let scanned_through = util::fetch_i64(
        conn,
        "SELECT COALESCE(MAX(store_id), 0) FROM lcm_raw_messages",
        (),
        "payload GC raw row watermark returned no row",
    )
    .await?
    .max(after);
    let rows = scan_placeholder_text_rows_between(
        conn,
        PlaceholderScanScope::ProviderOrAll {
            provider,
            session_id,
        },
        &live_prefix_like_patterns(),
        after,
        scanned_through,
    )
    .await?;
    let mut named = BTreeSet::new();
    for row in &rows {
        for text in row.texts() {
            named.extend(extract_live_payload_refs_from_text(text));
        }
    }
    let named = named.into_iter().collect::<Vec<_>>();
    let owners = payload_owners(conn, &named).await?;
    let with_metadata = owners
        .into_iter()
        .map(|owner| owner.payload_ref)
        .collect::<BTreeSet<_>>();
    let mut refs = BTreeSet::new();
    let mut stat_errors = Vec::new();
    for payload_ref in named {
        if with_metadata.contains(&payload_ref) {
            continue;
        }
        match payload_file_present(dir, &payload_ref) {
            Ok(true) => {}
            Ok(false) => {
                refs.insert(payload_ref);
            }
            Err(error) => stat_errors.push((payload_ref, error.to_string())),
        }
    }
    let store_ids = rows
        .iter()
        .filter(|row| {
            row.texts()
                .any(|text| refs.iter().any(|payload_ref| text.contains(payload_ref)))
        })
        .map(|row| row.store_id)
        .collect();
    Ok(DanglingPlan {
        refs,
        store_ids,
        scanned_through,
        stat_errors,
    })
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
        return run_payload_gc(conn, storage_root, provider, session_id, cfg, now).await;
    }

    let snapshot = read_payload_gc_snapshot(conn, storage_root, provider, session_id).await?;
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
        &snapshot,
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

/// What payload GC reads on the reader before its write transaction: every
/// metadata ref, which orphan detection subtracts from the payload directory;
/// the refs in scope, whose files the missing phase checks; and the dangling
/// placeholders in rows written since the last applied pass. The transaction
/// re-reads each payload and row it acts on.
pub struct PayloadGcSnapshot {
    all_metadata_refs: BTreeSet<String>,
    scoped_metadata_refs: BTreeSet<String>,
    dangling: DanglingPlan,
}

#[tracing::instrument(name = "sessions.lcm.gc.snapshot", level = "trace", skip_all)]
pub async fn read_payload_gc_snapshot(
    conn: &(impl QueryExecutor + ?Sized),
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
) -> Result<PayloadGcSnapshot, LcmError> {
    let all_metadata_refs = maintenance::all_payload_metadata_refs(conn).await?;
    let scoped_metadata_refs = if provider == "all" && session_id.is_none() {
        all_metadata_refs.clone()
    } else {
        payload_metadata_refs_for_scope(conn, provider, session_id).await?
    };
    let dir = payload::existing_payload_dir_opt(storage_root)?;
    let dangling = plan_dangling(conn, dir.as_deref(), provider, session_id).await?;
    Ok(PayloadGcSnapshot {
        all_metadata_refs,
        scoped_metadata_refs,
        dangling,
    })
}

#[tracing::instrument(name = "sessions.lcm.gc.apply", level = "trace", skip_all)]
#[allow(clippy::too_many_arguments)]
pub async fn run_payload_gc_in_transaction(
    conn: &(impl Executor + ?Sized),
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
    cfg: &LcmGcConfig,
    apply: bool,
    now: i64,
    snapshot: &PayloadGcSnapshot,
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
    let PayloadGcSnapshot {
        all_metadata_refs,
        scoped_metadata_refs,
        dangling,
    } = snapshot;

    let mut remaining = cfg.max_batch_size.max(1);
    // Orphan files have no metadata row, so they cannot be attributed to a
    // provider/session. Include them in every scoped GC preview/apply just as
    // the payload-health surface includes them for scoped drill-downs.
    if let Some(dir) = dir.as_deref() {
        if apply {
            stage_orphan_files(
                conn,
                dir,
                all_metadata_refs,
                now,
                &cfg,
                &mut remaining,
                &mut report,
            )
            .await?;
        } else {
            preview_orphan_files(
                dir,
                all_metadata_refs,
                now,
                &cfg,
                &mut remaining,
                &mut report,
            )?;
        }
    }
    reap_unreferenced_metadata(ReapRequest {
        conn,
        storage_root,
        provider,
        session_id,
        now,
        cfg: &cfg,
        apply,
        remaining: &mut remaining,
        report: &mut report,
    })
    .await?;
    reap_missing_metadata(
        ReapRequest {
            conn,
            storage_root,
            provider,
            session_id,
            now,
            cfg: &cfg,
            apply,
            remaining: &mut remaining,
            report: &mut report,
        },
        dir.as_deref(),
        scoped_metadata_refs,
    )
    .await?;
    rewrite_dangling_placeholders(conn, dangling, provider, session_id, apply, &mut report).await?;

    report.ended_at = now;
    if apply {
        let duration_ms =
            tracedecay_runtime_core::tracedecay::saturating_duration_millis(started.elapsed());
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

struct ReapRequest<'a, E: Executor + ?Sized> {
    conn: &'a E,
    storage_root: &'a Path,
    provider: &'a str,
    session_id: Option<&'a str>,
    now: i64,
    cfg: &'a LcmGcConfig,
    apply: bool,
    remaining: &'a mut usize,
    report: &'a mut LcmGcReport,
}

async fn reap_unreferenced_metadata<E: Executor + ?Sized>(
    request: ReapRequest<'_, E>,
) -> Result<(), LcmError> {
    let ReapRequest {
        conn,
        storage_root,
        provider,
        session_id,
        now,
        cfg,
        apply,
        remaining,
        report,
    } = request;
    let plan = plan_unreferenced(conn, provider, session_id, cfg, now).await?;
    plan.record_deferred(report);
    let mut marks_to_delete = Vec::new();
    if apply {
        marks_to_delete.extend(plan.stale.iter().cloned());
        marks_to_delete.extend(plan.referenced.iter().cloned());
    }
    for (owner, bytes) in &plan.unreferenced {
        let payload_ref = &owner.payload_ref;
        if *remaining == 0 {
            report.batch_cap(1);
            continue;
        }
        if apply {
            match payload::prepare_external_payload_delete_in_transaction(
                conn,
                storage_root,
                payload_ref,
                &payload::DeleteOpts::default(),
            )
            .await
            {
                Ok(prepared) => {
                    marks_to_delete.push(payload_ref.clone());
                    let outcome = prepared.outcome;
                    if outcome.metadata_row_existed {
                        report.totals.rows_deleted += 1;
                    }
                    report.totals.placeholders_rewritten += outcome.placeholders_rewritten;
                }
                Err(LcmError::StillReferenced) => {
                    marks_to_delete.push(payload_ref.clone());
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
        report.unreferenced.add(payload_ref, *bytes);
        *remaining -= 1;
    }
    if apply {
        delete_gc_marks(conn, &marks_to_delete).await?;
    }
    Ok(())
}

async fn reap_missing_metadata<E: Executor + ?Sized>(
    request: ReapRequest<'_, E>,
    dir: Option<&Path>,
    metadata_refs: &BTreeSet<String>,
) -> Result<(), LcmError> {
    let ReapRequest {
        conn,
        storage_root,
        now,
        cfg,
        apply,
        remaining,
        report,
        ..
    } = request;
    let plan = plan_missing(conn, dir, metadata_refs, report).await?;
    for payload_ref in &plan.referenced {
        report.missing.add(payload_ref, 0);
    }
    if apply {
        delete_gc_marks_in_state(conn, &plan.restored, "missing").await?;
    }
    if !apply || !cfg.reap_missing_enabled || cfg.reap_missing_after == 0 {
        return Ok(());
    }
    let missing_refs = plan.referenced;
    if missing_refs.is_empty() {
        return Ok(());
    }
    let marks = gc_marks(conn, &missing_refs).await?;
    let mut marks_to_upsert = Vec::new();
    let mut marks_to_delete = Vec::new();
    for payload_ref in &missing_refs {
        let first_seen_at = match marks.get(payload_ref) {
            Some((state, first_seen_at)) if state == "missing" => *first_seen_at,
            _ => {
                marks_to_upsert.push(payload_ref.clone());
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
        match payload::prepare_external_payload_delete_in_transaction(
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
                marks_to_delete.push(payload_ref.clone());
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
    if apply {
        upsert_gc_marks(conn, &marks_to_upsert, "missing", now).await?;
        delete_gc_marks(conn, &marks_to_delete).await?;
    }
    Ok(())
}

/// Tombstones the dangling placeholders the reader snapshot found in rows
/// written since the last applied pass. Only an unscoped pass advances the
/// scan cursor, so a scoped pass never hides rows outside its scope from the
/// next full pass.
async fn rewrite_dangling_placeholders(
    conn: &(impl Executor + ?Sized),
    plan: &DanglingPlan,
    provider: &str,
    session_id: Option<&str>,
    apply: bool,
    report: &mut LcmGcReport,
) -> Result<(), LcmError> {
    for (payload_ref, detail) in &plan.stat_errors {
        report.add_error(payload_ref, "dangling_payload_stat_failed", detail.clone());
    }
    let mut refs = plan.refs.clone();
    if apply && !refs.is_empty() {
        // A payload written since the snapshot owns its placeholders again.
        let probe =
            pending_delete::probe_metadata_rows(conn, &refs.iter().cloned().collect::<Vec<_>>())
                .await;
        for (payload_ref, detail) in &probe.failures {
            report.add_error(payload_ref, "metadata_check_failed", detail.clone());
        }
        refs.retain(|payload_ref| {
            !probe.existing.contains(payload_ref) && !probe.failures.contains_key(payload_ref)
        });
    }
    for payload_ref in &refs {
        report.dangling.add(payload_ref, 0);
    }
    if !apply {
        return Ok(());
    }
    let rows = if refs.is_empty() {
        Vec::new()
    } else {
        placeholder_text_rows_by_store_id(conn, &plan.store_ids).await?
    };
    let mut total = 0usize;
    for row in rows {
        let store_id = row.store_id;
        let (content, placeholder_text, metadata_json, changed) =
            tombstone_row_for_refs(row, &refs);
        if changed == 0 {
            continue;
        }
        conn.execute(
            "UPDATE lcm_raw_messages
             SET content = ?2, placeholder_text = ?3, metadata_json = ?4
             WHERE store_id = ?1",
            params![
                store_id,
                content.as_deref(),
                placeholder_text.as_deref(),
                metadata_json.as_deref()
            ],
        )
        .await?;
        total += changed;
    }
    report.totals.placeholders_rewritten += total;
    if provider == "all" && session_id.is_none() {
        schema::set_gc_meta(
            conn,
            DANGLING_SCAN_CURSOR,
            &plan.scanned_through.to_string(),
        )
        .await?;
    }
    Ok(())
}

fn tombstone_row_for_refs(
    row: PlaceholderTextRow,
    payload_refs: &BTreeSet<String>,
) -> (Option<String>, Option<String>, Option<String>, usize) {
    let mut changed = 0usize;
    let content = row.content.map(|text| {
        let (tombstoned, field_changes) = tombstone_text_for_refs(&text, payload_refs);
        changed += field_changes;
        tombstoned
    });
    // The snippet and index columns derive from the tombstoned body; they are
    // counted because they are retrieval text a reader sees change.
    let (_, snippet_changes) = tombstone_text_for_refs(&row.snippet_text, payload_refs);
    changed += snippet_changes;
    let (_, index_changes) = tombstone_text_for_refs(&row.index_text, payload_refs);
    changed += index_changes;
    let placeholder_text = row
        .placeholder_text
        .map(|text| tombstone_text_for_refs(&text, payload_refs).0);
    let metadata_json = row.metadata_json.map(|text| {
        let (tombstoned, field_changes) = tombstone_text_for_refs(&text, payload_refs);
        changed += field_changes;
        tombstoned
    });
    (content, placeholder_text, metadata_json, changed)
}

fn tombstone_text_for_refs(text: &str, payload_refs: &BTreeSet<String>) -> (String, usize) {
    let mut out = text.to_string();
    let mut changed = 0usize;
    for payload_ref in payload_refs {
        let tombstoned = tombstone_placeholder_in_text(&out, payload_ref);
        if tombstoned != out {
            changed += 1;
            out = tombstoned;
        }
    }
    (out, changed)
}

pub(super) async fn delete_gc_marks(
    conn: &(impl Executor + ?Sized),
    payload_refs: &[String],
) -> Result<(), LcmError> {
    for chunk in payload_refs.chunks(util::SQLITE_IN_BATCH_SIZE) {
        if chunk.is_empty() {
            continue;
        }
        let sql = format!(
            "DELETE FROM lcm_gc_marks
             WHERE payload_ref IN ({})",
            util::sql_in_placeholders(chunk.len())
        );
        conn.execute(
            &sql,
            chunk
                .iter()
                .cloned()
                .map(SqlValue::Text)
                .collect::<Vec<_>>(),
        )
        .await?;
    }
    Ok(())
}

async fn delete_gc_marks_in_state(
    conn: &(impl Executor + ?Sized),
    payload_refs: &[String],
    state: &str,
) -> Result<(), LcmError> {
    for chunk in payload_refs.chunks(util::SQLITE_IN_BATCH_SIZE) {
        if chunk.is_empty() {
            continue;
        }
        let sql = format!(
            "DELETE FROM lcm_gc_marks
             WHERE state = ? AND payload_ref IN ({})",
            util::sql_in_placeholders(chunk.len())
        );
        let mut values = vec![SqlValue::Text(state.to_string())];
        values.extend(chunk.iter().cloned().map(SqlValue::Text));
        conn.execute(&sql, values).await?;
    }
    Ok(())
}

#[cfg(test)]
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
pub(crate) async fn gc_marks(
    conn: &(impl QueryExecutor + ?Sized),
    payload_refs: &[String],
) -> Result<HashMap<String, (String, i64)>, LcmError> {
    let mut marks = HashMap::new();
    for chunk in payload_refs.chunks(util::SQLITE_IN_BATCH_SIZE) {
        if chunk.is_empty() {
            continue;
        }
        let sql = format!(
            "SELECT payload_ref, state, first_seen_at
             FROM lcm_gc_marks
             WHERE payload_ref IN ({})",
            util::sql_in_placeholders(chunk.len())
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

async fn upsert_gc_marks(
    conn: &(impl Executor + ?Sized),
    payload_refs: &[String],
    state: &str,
    now: i64,
) -> Result<(), LcmError> {
    let chunk_size = (util::SQLITE_IN_BATCH_SIZE / GC_MARK_UPSERT_BINDS_PER_ROW).max(1);
    for chunk in payload_refs.chunks(chunk_size) {
        if chunk.is_empty() {
            continue;
        }
        let values_sql = std::iter::repeat_n("(?, ?, ?, ?)", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "INSERT INTO lcm_gc_marks(payload_ref, state, first_seen_at, updated_at)
             VALUES {values_sql}
             ON CONFLICT(payload_ref) DO UPDATE SET
                state = excluded.state,
                first_seen_at = excluded.first_seen_at,
                updated_at = excluded.updated_at"
        );
        let mut values = Vec::with_capacity(chunk.len() * GC_MARK_UPSERT_BINDS_PER_ROW);
        for payload_ref in chunk {
            values.push(SqlValue::Text(payload_ref.clone()));
            values.push(SqlValue::Text(state.to_string()));
            values.push(SqlValue::Integer(now));
            values.push(SqlValue::Integer(now));
        }
        conn.execute(&sql, values).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
