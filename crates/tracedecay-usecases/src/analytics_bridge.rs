//! Bridges hook telemetry into the durable `analytics_events` table.
//!
//! Hooks append JSONL rows to `hook_analytics.jsonl` (project store when the
//! hook can resolve a project root, user-level profile root otherwise), while
//! the MCP server writes `mcp_tool_call` / `hook_route` rows straight into the
//! user-level global DB. This module imports the JSONL side into
//! `analytics_events` so one durable table answers adoption questions, using
//! per-file byte cursors in `parse_offsets` to stay idempotent across runs.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

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
pub async fn import_hook_analytics(
    gdb: &RegisteredGlobalDb,
    sources: &[HookImportSource],
) -> HookImportOutcome {
    let mut outcome = HookImportOutcome::default();
    for source in sources {
        outcome.sources.push(import_source(gdb, source).await);
    }
    outcome
}

async fn import_source(
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
    let start = match gdb.get_parse_offset(&cursor_key).await {
        // Truncated/rotated files restart from the top.
        Some(cursor) if cursor.byte_offset <= file_len => cursor.byte_offset,
        _ => 0,
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
                ParseOffset {
                    byte_offset: start + frontier,
                    mtime,
                    file_id: 0,
                },
            )
            .await
        {
            result.error = Some(err);
            return result;
        }
        acknowledged = frontier;
        result.imported = result.imported.saturating_add(events.len() as u64);
    }
    if acknowledged < consumed as u64
        && let Err(err) = gdb
            .append_analytics_events_with_cursor(
                &[],
                &cursor_key,
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
    result
}

/// Namespaced `parse_offsets` key so hook cursors never collide with the
/// accounting transcript cursors that share the table.
fn import_cursor_key(path: &Path) -> String {
    format!("hook_analytics:{}", path.display())
}

fn read_from_offset(path: &Path, offset: u64) -> Result<String, String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file =
        std::fs::File::open(path).map_err(|err| format!("open {}: {err}", path.display()))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|err| format!("seek {}: {err}", path.display()))?;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|err| format!("read {}: {err}", path.display()))?;
    Ok(text)
}

fn hook_row_to_analytics_event(
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

// The CLI entry points that used to close this file (`run_analytics_sync`,
// `run_analytics_diagnostics`, `call_admin_cli`, `analytics_sync_with_db`,
// `analytics_diagnostics_with_db`) stayed in the root binary: they drive
// `daemon::DaemonHandshake`, `dashboard::analytics_api` and the observability
// read models, none of which sit below this crate. Only the durable
// hook-JSONL importer moved down. See SEAMS.md.
