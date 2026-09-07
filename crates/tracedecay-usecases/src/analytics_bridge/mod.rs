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

/// The hook JSONL files relevant to a project: its store file plus the
/// user-level fallback file shared by every project.
pub fn hook_import_sources(project_root: Option<&Path>) -> Vec<HookImportSource> {
    let mut sources = Vec::new();
    if let Some(root) = project_root
        && let Ok(layout) =
            tracedecay_runtime_core::storage::resolve_layout_for_current_profile(root)
    {
        sources.push(HookImportSource {
            path: layout.data_root.join("hook_analytics.jsonl"),
            default_project_root: Some(root.to_path_buf()),
        });
    }
    if let Ok(profile_root) = tracedecay_runtime_core::storage::default_profile_root() {
        let path = profile_root.join("hook_analytics.jsonl");
        if !sources.iter().any(|source| source.path == path) {
            sources.push(HookImportSource {
                path,
                default_project_root: None,
            });
        }
    }
    sources
}

/// Imports new hook JSONL rows into `analytics_events`, advancing a byte
/// cursor per source file so re-runs only ingest the appended tail.
// Takes the source list by value: a borrowed slice iterator held across the
// per-source awaits trips rustc's higher-ranked Send leak check when this
// future runs inside a spawned startup catch-up task.
#[hotpath::measure(label = "usecases.analytics.import", future = true)]
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
#[hotpath::measure(label = "usecases.analytics.sync", future = true)]
pub async fn analytics_sync_with_db(
    gdb: &RegisteredGlobalDb,
    project_root: Option<&Path>,
) -> Value {
    let sources = hook_import_sources(project_root);
    import_hook_analytics(gdb, sources).await.as_json()
}

pub async fn import_source(
    gdb: &RegisteredGlobalDb,
    source: &HookImportSource,
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
    let expected_cursor = match gdb.get_parse_offset_result(&cursor_key).await {
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
    result
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

    use super::hook_row_to_analytics_event;

    #[test]
    fn maps_hook_invoked_row_with_attribution() {
        let line = r#"{"agent":"claude","event":"hook_invoked","hook_name":"preToolUse","project_root":"/repo","session_id":"s1","tool_name":"Agent","ts_unix_ms":1783000000000}"#;
        let Some(event) = hook_row_to_analytics_event(line, None) else {
            panic!("row should map");
        };
        assert_eq!(event.provider, "hook_claude");
        assert_eq!(event.event_kind, "hook_invoked");
        assert_eq!(event.hook_name.as_deref(), Some("preToolUse"));
        assert_eq!(event.session_id.as_deref(), Some("s1"));
        assert_eq!(event.timestamp, 1_783_000_000);
        assert!(event.project_id.ends_with("repo"));
    }

    #[test]
    fn maps_hint_row_with_hint_id() {
        let line = r#"{"agent":"cursor","event":"hint_emitted","category":"search","hint_id":"h-abc","project_root":"/repo","session_id":"s1","ts_unix_ms":1783000000000}"#;
        let Some(event) = hook_row_to_analytics_event(line, None) else {
            panic!("row should map");
        };
        assert_eq!(event.hint_category.as_deref(), Some("search"));
        assert_eq!(event.hint_id.as_deref(), Some("h-abc"));

        let line = r#"{"agent":"cursor","event":"hint_emitted","category":"search","project_root":"/repo","session_id":"s1","ts_unix_ms":1783000000000}"#;
        let Some(event) = hook_row_to_analytics_event(line, None) else {
            panic!("row should map");
        };
        assert!(event.hint_id.is_none());
    }

    #[test]
    fn unattributed_row_falls_back_to_default_project() {
        let line = r#"{"agent":"cursor","event":"hook_invoked","hook_name":"postToolUse","ts_unix_ms":1783000000000}"#;
        let Some(event) = hook_row_to_analytics_event(line, Some(Path::new("/repo"))) else {
            panic!("row should map");
        };
        assert!(event.project_id.ends_with("repo"));
        let Some(event) = hook_row_to_analytics_event(line, None) else {
            panic!("row should map");
        };
        assert_eq!(event.project_id, "");
    }

    #[test]
    fn rows_without_event_field_are_skipped() {
        assert!(hook_row_to_analytics_event("{}", None).is_none());
        assert!(hook_row_to_analytics_event("not json", None).is_none());
    }
}
