//! Bridges hook telemetry into the durable `analytics_events` table.
//!
//! Hooks append JSONL rows to `hook_analytics.jsonl` (project store when the
//! hook can resolve a project root, user-level profile root otherwise), while
//! the MCP server writes `mcp_tool_call` / `hook_route` rows straight into the
//! user-level global DB. This module imports the JSONL side into
//! `analytics_events` so one durable table answers adoption questions, using
//! per-file byte cursors in `parse_offsets` to stay idempotent across runs.

mod diagnostics;
pub mod summary;

pub use diagnostics::analytics_diagnostics_with_db;
pub use summary::{
    AnalyticsDiagnosticsPayloadV1, AnalyticsDiagnosticsRatiosV1, AnalyticsEventKindCountV1,
    AnalyticsHintEfficacyCategoryV1, AnalyticsHintEfficacyTotalsV1, AnalyticsHintEfficacyV1,
    AnalyticsHookNameCountV1, AnalyticsHookWindowV1, AnalyticsOutcomeCountV1,
    AnalyticsPromptCategoryCountV1, AnalyticsRecentEventV1, AnalyticsRecentHookV1,
    AnalyticsToolCategoryCountV1, AnalyticsToolCountV1, HOOK_ANALYTICS_WINDOW_ROWS,
    HookAnalyticsRows, HookAnalyticsWindow, HookReadinessProjectionPort,
    aggregate_hook_completed_readiness, diagnostics_payload_from_parts,
    diagnostics_summary_from_parts, durable_analytics_event_row, hint_efficacy_from_events,
    install_hook_readiness_projection, read_hook_analytics_file, read_hook_analytics_rows_at,
    recent_hook_rows, sort_hook_analytics_rows,
};

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use tracedecay_domain::{CoverageStateV1, StorageObservationKindV1, StorageObservedV1};
use tracedecay_global_db::{AnalyticsEventInsert, ParseOffset, RegisteredGlobalDb};

/// Maximum events committed with one matching durable cursor frontier.
const IMPORT_BATCH_SIZE: usize = 500;

/// Size above which a fully imported hook analytics file is truncated. Every
/// row of a consumed file is already durable in `analytics_events`, so the file
/// only needs to hold the not-yet-imported tail.
const HOOK_ANALYTICS_ROTATE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct HookImportSource {
    /// JSONL file to import.
    pub path: PathBuf,
    /// Project attributed to rows that carry no `project_root` field. Rows in
    /// a project-store file all belong to that project even before writers
    /// stamped attribution; user-level rows without it stay unattributed.
    pub default_project_root: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct HookImportSourceOutcome {
    pub path: PathBuf,
    pub imported: u64,
    pub skipped: u64,
    pub error: Option<String>,
}

impl HookImportSourceOutcome {
    fn as_json(&self) -> Value {
        json!({
            "path": self.path.display().to_string(),
            "imported": self.imported,
            "skipped": self.skipped,
            "error": self.error,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct HookImportOutcome {
    pub sources: Vec<HookImportSourceOutcome>,
}

impl HookImportOutcome {
    pub fn imported(&self) -> u64 {
        self.sources.iter().map(|source| source.imported).sum()
    }

    pub fn as_json(&self) -> Value {
        json!({
            "imported": self.imported(),
            "sources": self.sources.iter().map(HookImportSourceOutcome::as_json).collect::<Vec<_>>(),
        })
    }
}

/// The hook JSONL files relevant to a project in the profile whose data
/// directory is `profile_root`: its store file plus the user-level fallback
/// file shared by every project. A project whose store layout cannot be
/// resolved is a typed failure, never a silently shorter source list.
pub fn hook_import_sources(
    profile_root: &Path,
    project_root: Option<&Path>,
) -> tracedecay_domain::errors::Result<Vec<HookImportSource>> {
    let mut sources = Vec::new();
    if let Some(root) = project_root {
        let layout = tracedecay_runtime_core::storage::resolve_layout(root, profile_root)?;
        sources.push(HookImportSource {
            path: layout.data_root.join("hook_analytics.jsonl"),
            default_project_root: Some(root.to_path_buf()),
        });
    }
    let path = profile_root.join("hook_analytics.jsonl");
    if !sources.iter().any(|source| source.path == path) {
        sources.push(HookImportSource {
            path,
            default_project_root: None,
        });
    }
    Ok(sources)
}

/// Imports new hook JSONL rows into `analytics_events`, advancing a byte
/// cursor per source file so re-runs only ingest the appended tail.
// Takes the source list by value: a borrowed slice iterator held across the
// per-source awaits trips rustc's higher-ranked Send leak check when this
// future runs inside a spawned startup catch-up task.
#[tracing::instrument(name = "usecases.analytics.import", level = "trace", skip_all)]
pub async fn import_hook_analytics(
    gdb: &RegisteredGlobalDb,
    sources: Vec<HookImportSource>,
) -> HookImportOutcome {
    let mut outcome = HookImportOutcome::default();
    for source in sources {
        outcome.sources.push(import_source(gdb, &source).await);
    }
    outcome
}

/// Resolves the hook JSONL sources for `project_root` and imports them,
/// returning the JSON outcome shape the admin CLI and dashboard report.
#[tracing::instrument(name = "usecases.analytics.sync", level = "trace", skip_all)]
pub async fn analytics_sync_with_db(
    gdb: &RegisteredGlobalDb,
    profile_root: &Path,
    project_root: Option<&Path>,
) -> tracedecay_domain::errors::Result<Value> {
    let sources = hook_import_sources(profile_root, project_root)?;
    Ok(import_hook_analytics(gdb, sources).await.as_json())
}

pub async fn import_source(
    gdb: &RegisteredGlobalDb,
    source: &HookImportSource,
) -> HookImportSourceOutcome {
    import_source_rotating_above(gdb, source, HOOK_ANALYTICS_ROTATE_BYTES).await
}

async fn import_source_rotating_above(
    gdb: &RegisteredGlobalDb,
    source: &HookImportSource,
    rotate_bytes: u64,
) -> HookImportSourceOutcome {
    let mut result = HookImportSourceOutcome {
        path: source.path.clone(),
        imported: 0,
        skipped: 0,
        error: None,
    };
    let Ok(metadata) = std::fs::metadata(&source.path) else {
        return result;
    };
    let file_len = metadata.len();
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs());

    let cursor_key = import_cursor_key(&source.path);
    let expected_cursor = match gdb.get_parse_offset(&cursor_key).await {
        Ok(Some(cursor)) => cursor,
        Ok(None) => ParseOffset::default(),
        Err(err) => {
            result.error = Some(format!("read analytics import cursor: {err}"));
            return result;
        }
    };
    // Truncated/rotated files restart from the top while retaining the exact
    // durable cursor as the compare-and-swap precondition.
    let start = if expected_cursor.byte_offset <= file_len {
        expected_cursor.byte_offset
    } else {
        0
    };
    if start == file_len {
        return result;
    }

    let text = match read_from_offset(&source.path, start) {
        Ok(text) => text,
        Err(err) => {
            result.error = Some(err);
            return result;
        }
    };
    // Only consume up to the last complete line; a concurrent writer may have
    // an unfinished row at EOF.
    let consumed = text.rfind('\n').map_or(0, |index| index + 1);
    if consumed == 0 {
        return result;
    }

    let mut batch = Vec::new();
    let mut relative_offset = 0u64;
    for line in text[..consumed].split_inclusive('\n') {
        relative_offset = relative_offset.saturating_add(line.len() as u64);
        match hook_row_to_analytics_event(line.trim_end(), source.default_project_root.as_deref()) {
            Some(event) => batch.push((event, relative_offset)),
            None => result.skipped += 1,
        }
    }
    let mut acknowledged = 0u64;
    let mut claimed_cursor = expected_cursor;
    // Durable-append span only. Parsing and file I/O above are deliberately
    // outside it so the storage measurement denominates store write latency
    // rather than total import cost.
    let durable_started = std::time::Instant::now();
    for chunk in batch.chunks(IMPORT_BATCH_SIZE) {
        let events = chunk
            .iter()
            .map(|(event, _)| event.clone())
            .collect::<Vec<_>>();
        let frontier = chunk.last().map_or(acknowledged, |(_, offset)| *offset);
        if let Err(err) = gdb
            .append_analytics_events_with_cursor(
                &events,
                &cursor_key,
                claimed_cursor,
                ParseOffset {
                    byte_offset: start + frontier,
                    mtime,
                    file_id: 0,
                },
            )
            .await
        {
            result.error = Some(err);
            observe_import_write_latency(gdb, durable_started, false).await;
            return result;
        }
        acknowledged = frontier;
        claimed_cursor = ParseOffset {
            byte_offset: start + frontier,
            mtime,
            file_id: 0,
        };
        result.imported = result.imported.saturating_add(events.len() as u64);
    }
    if acknowledged < consumed as u64
        && let Err(err) = gdb
            .append_analytics_events_with_cursor(
                &[],
                &cursor_key,
                claimed_cursor,
                ParseOffset {
                    byte_offset: start + consumed as u64,
                    mtime,
                    file_id: 0,
                },
            )
            .await
    {
        result.error = Some(err);
    }
    observe_import_write_latency(gdb, durable_started, result.error.is_none()).await;
    let read_end = start + text.len() as u64;
    if result.error.is_none() && consumed == text.len() && read_end > rotate_bytes {
        rotate_consumed_file(
            gdb,
            &source.path,
            &cursor_key,
            ParseOffset {
                byte_offset: read_end,
                mtime,
                file_id: 0,
            },
        )
        .await;
    }
    result
}

/// Truncates `path` to zero when its length still equals `consumed_end`,
/// holding the sidecar lock every analytics append takes so no row can land
/// between the length check and the truncate. A held lock or a longer file
/// means rows arrived after the import read; rotation waits for the next
/// import. Returns whether the file was truncated.
fn truncate_if_unchanged(path: &Path, consumed_end: u64) -> std::io::Result<bool> {
    let Some(_append_lock) = tracedecay_runtime_core::storage::try_acquire_sidecar_lock(
        &tracedecay_runtime_core::storage::append_lock_path(path),
    )?
    else {
        return Ok(false);
    };
    let file = std::fs::OpenOptions::new().write(true).open(path)?;
    if file.metadata()?.len() != consumed_end {
        return Ok(false);
    }
    file.set_len(0)?;
    Ok(true)
}

/// Rotates a fully imported file and rewinds its durable cursor to the start,
/// so a file that regrows past the old offset before the next import is not
/// read from the middle. Rotation failures lose no rows and are only logged.
async fn rotate_consumed_file(
    gdb: &RegisteredGlobalDb,
    path: &Path,
    cursor_key: &str,
    consumed: ParseOffset,
) {
    match truncate_if_unchanged(path, consumed.byte_offset) {
        Ok(false) => return,
        Ok(true) => {}
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "hook analytics rotation failed");
            return;
        }
    }
    tracing::info!(
        path = %path.display(),
        rotated_bytes = consumed.byte_offset,
        "hook analytics file rotated after full import"
    );
    if let Err(error) = gdb
        .append_analytics_events_with_cursor(
            &[],
            cursor_key,
            consumed,
            ParseOffset {
                byte_offset: 0,
                mtime: consumed.mtime,
                file_id: 0,
            },
        )
        .await
    {
        tracing::warn!(
            path = %path.display(),
            %error,
            "hook analytics cursor rewind failed after rotation"
        );
    }
}

/// Retains one `storage.measurement.observed.v1` write-latency observation for
/// the durable append span of this import.
///
/// Telemetry is strictly downstream of the import: the observation is recorded
/// after the product outcome is already determined and its result is discarded,
/// so a failed or unavailable observation store can never change what the
/// import returned. An import that ends in error retains `Partial` coverage
/// because the measured span covers only the appends that were reached.
///
/// A user-level analytics database is not bound to a project scope; the
/// observation authority refuses it and this becomes a no-op rather than an
/// event attributed to the wrong scope.
async fn observe_import_write_latency(
    gdb: &RegisteredGlobalDb,
    started: std::time::Instant,
    complete: bool,
) {
    let Ok(duration_micros) = u64::try_from(started.elapsed().as_micros()) else {
        return;
    };
    let _ = crate::observability::record_storage(
        gdb,
        StorageObservedV1 {
            kind: StorageObservationKindV1::WriteLatency,
            duration_micros: Some(duration_micros),
            quantity: None,
            coverage: if complete {
                CoverageStateV1::Known
            } else {
                CoverageStateV1::Partial
            },
        },
    )
    .await;
}

/// Namespaced `parse_offsets` key so hook cursors never collide with the
/// accounting transcript cursors that share the table.
fn import_cursor_key(path: &Path) -> String {
    format!("hook_analytics:{}", path.display())
}

fn read_from_offset(path: &Path, offset: u64) -> Result<String, String> {
    let mut file = File::open(path).map_err(|err| format!("open {}: {err}", path.display()))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|err| format!("seek {}: {err}", path.display()))?;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|err| format!("read {}: {err}", path.display()))?;
    Ok(text)
}

pub fn hook_row_to_analytics_event(
    line: &str,
    default_project_root: Option<&Path>,
) -> Option<AnalyticsEventInsert> {
    let row: Value = serde_json::from_str(line).ok()?;
    let event_kind = row.get("event").and_then(Value::as_str)?.to_string();
    let agent = row
        .get("agent")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let project_id = row
        .get("project_root")
        .and_then(Value::as_str)
        .map(|root| RegisteredGlobalDb::canonical_project_key(Path::new(root)))
        .or_else(|| default_project_root.map(RegisteredGlobalDb::canonical_project_key))
        .unwrap_or_default();
    let timestamp = row
        .get("ts_unix_ms")
        .and_then(Value::as_i64)
        .map_or(0, |millis| millis / 1000);
    Some(AnalyticsEventInsert {
        provider: format!("hook_{agent}"),
        project_id,
        session_id: text_field(&row, "session_id"),
        timestamp,
        event_kind,
        hook_name: text_field(&row, "hook_name"),
        tool_name: text_field(&row, "tool_name"),
        tool_category: None,
        skill_name: None,
        hint_category: text_field(&row, "category"),
        hint_id: text_field(&row, "hint_id"),
        outcome: Some("observed".to_string()),
        metadata_json: Some(row.to_string()),
    })
}

fn text_field(row: &Value, key: &str) -> Option<String> {
    row.get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        HookImportSource, RegisteredGlobalDb, hook_row_to_analytics_event,
        import_source_rotating_above, truncate_if_unchanged,
    };
    use tracedecay_global_db::AnalyticsEventQuery;

    fn rows(first: usize, count: usize) -> String {
        (first..first + count)
            .map(|index| {
                format!(
                    r#"{{"agent":"claude","event":"hook_invoked","hook_name":"hook{index}","ts_unix_ms":1783000000000}}{}"#,
                    "\n"
                )
            })
            .collect()
    }

    async fn stored_hook_names(db: &RegisteredGlobalDb) -> Vec<String> {
        let mut names = db
            .query_analytics_events(&AnalyticsEventQuery {
                provider: None,
                project_id: None,
                session_id: None,
                event_kind: None,
                since: None,
                until: None,
                before_id: None,
                limit: 1000,
            })
            .await
            .expect("query analytics events")
            .into_iter()
            .filter_map(|event| event.hook_name)
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    fn append(path: &Path, text: &str) {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
            .expect("open analytics file")
            .write_all(text.as_bytes())
            .expect("append analytics rows");
    }

    fn source(path: &Path) -> HookImportSource {
        HookImportSource {
            path: path.to_path_buf(),
            default_project_root: None,
        }
    }

    #[tokio::test]
    async fn a_consumed_file_over_the_threshold_rotates_and_new_rows_import_once() {
        let harness = tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open(
            "hook-analytics-rotation",
        )
        .await;
        let directory = tempfile::tempdir().expect("analytics directory");
        let path = directory.path().join("hook_analytics.jsonl");
        append(&path, &rows(0, 3));

        let outcome = import_source_rotating_above(&harness.registered, &source(&path), 1).await;
        assert_eq!((outcome.imported, outcome.error), (3, None));
        assert_eq!(std::fs::metadata(&path).expect("rotated file").len(), 0);

        // Longer than the rotated file was, so a cursor left at the old end
        // would resume mid-file and drop rows.
        append(&path, &rows(3, 5));
        let outcome =
            import_source_rotating_above(&harness.registered, &source(&path), u64::MAX).await;
        assert_eq!((outcome.imported, outcome.error), (5, None));
        let expected = (0..8)
            .map(|index| format!("hook{index}"))
            .collect::<Vec<_>>();
        assert_eq!(stored_hook_names(&harness.registered).await, expected);
    }

    #[tokio::test]
    async fn a_file_under_the_threshold_or_with_an_unconsumed_tail_is_not_rotated() {
        let harness = tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open(
            "hook-analytics-no-rotation",
        )
        .await;
        let directory = tempfile::tempdir().expect("analytics directory");
        let small = directory.path().join("small.jsonl");
        append(&small, &rows(0, 2));
        let small_len = std::fs::metadata(&small).expect("small file").len();
        let outcome =
            import_source_rotating_above(&harness.registered, &source(&small), small_len).await;
        assert_eq!(outcome.imported, 2);
        assert_eq!(
            std::fs::metadata(&small).expect("small file").len(),
            small_len
        );

        let partial = directory.path().join("partial.jsonl");
        append(&partial, &rows(10, 2));
        append(&partial, r#"{"agent":"claude","event":"hook_inv"#);
        let partial_len = std::fs::metadata(&partial).expect("partial file").len();
        let outcome = import_source_rotating_above(&harness.registered, &source(&partial), 1).await;
        assert_eq!(outcome.imported, 2);
        assert_eq!(
            std::fs::metadata(&partial).expect("partial file").len(),
            partial_len,
            "an unfinished trailing row keeps the file"
        );
    }

    #[test]
    fn truncation_requires_the_exact_consumed_length_and_the_append_lock() {
        let directory = tempfile::tempdir().expect("analytics directory");
        let path = directory.path().join("hook_analytics.jsonl");
        append(&path, &rows(0, 2));
        let consumed = std::fs::metadata(&path).expect("file").len();

        append(&path, &rows(2, 1));
        assert!(!truncate_if_unchanged(&path, consumed).expect("stale length"));
        assert!(std::fs::metadata(&path).expect("file").len() > consumed);

        let length = std::fs::metadata(&path).expect("file").len();
        let held = tracedecay_runtime_core::storage::try_acquire_sidecar_lock(
            &tracedecay_runtime_core::storage::append_lock_path(&path),
        )
        .expect("append lock")
        .expect("lock is free");
        assert!(!truncate_if_unchanged(&path, length).expect("held lock"));
        assert_eq!(std::fs::metadata(&path).expect("file").len(), length);
        drop(held);

        assert!(truncate_if_unchanged(&path, length).expect("exact length"));
        assert_eq!(std::fs::metadata(&path).expect("file").len(), 0);
    }

    fn aliased_root() -> (tempfile::TempDir, String, std::path::PathBuf) {
        let root = tempfile::tempdir().expect("project root");
        let real = root.path().join("real");
        std::fs::create_dir(&real).expect("real root");
        let alias = root.path().join("link");
        // Directory alias: canonical project identity must follow it on every host.
        tracedecay_runtime_core::test_fs::create_directory_alias(&real, &alias)
            .expect("alias root");
        let canonical = RegisteredGlobalDb::canonical_project_key(&real);
        (root, canonical, alias)
    }

    #[test]
    fn maps_hook_invoked_row_with_attribution() {
        let (_root, canonical, alias) = aliased_root();
        let line = serde_json::json!({
            "agent": "claude",
            "event": "hook_invoked",
            "hook_name": "preToolUse",
            "project_root": alias,
            "session_id": "s1",
            "tool_name": "Agent",
            "ts_unix_ms": 1_783_000_000_000_i64,
        })
        .to_string();
        let event = hook_row_to_analytics_event(&line, None).expect("row should map");
        assert_eq!(event.provider, "hook_claude");
        assert_eq!(event.event_kind, "hook_invoked");
        assert_eq!(event.hook_name.as_deref(), Some("preToolUse"));
        assert_eq!(event.session_id.as_deref(), Some("s1"));
        assert_eq!(event.tool_name.as_deref(), Some("Agent"));
        assert_eq!(event.outcome.as_deref(), Some("observed"));
        assert_eq!(event.timestamp, 1_783_000_000);
        assert_eq!(event.project_id, canonical);
        assert_ne!(event.project_id, alias.to_string_lossy());
    }

    #[test]
    fn unattributed_row_falls_back_to_default_project() {
        let (_root, canonical, alias) = aliased_root();
        let line = r#"{"agent":"cursor","event":"hook_invoked","hook_name":"postToolUse","ts_unix_ms":1783000000000}"#;
        let event = hook_row_to_analytics_event(line, Some(&alias)).expect("row should map");
        assert_eq!(event.provider, "hook_cursor");
        assert_eq!(event.hook_name.as_deref(), Some("postToolUse"));
        assert_eq!(event.project_id, canonical);
        let event = hook_row_to_analytics_event(line, None).expect("row should map");
        assert_eq!(event.project_id, "");
        assert_eq!(event.session_id, None);
    }

    #[test]
    fn rows_without_event_field_are_skipped() {
        assert_eq!(hook_row_to_analytics_event("{}", None), None);
        assert_eq!(hook_row_to_analytics_event("not json", None), None);
        let event =
            hook_row_to_analytics_event(r#"{"agent":"claude","event":"hook_invoked"}"#, None)
                .expect("a row with an event field maps");
        assert_eq!(event.provider, "hook_claude");
        assert_eq!(event.event_kind, "hook_invoked");
        assert_eq!(event.project_id, "");
        assert_eq!(event.timestamp, 0);
        assert_eq!(event.outcome.as_deref(), Some("observed"));
    }
}
