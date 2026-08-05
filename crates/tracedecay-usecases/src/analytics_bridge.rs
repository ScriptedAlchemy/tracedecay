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
use tokio::io::{AsyncReadExt, AsyncSeekExt};

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
    let file = match tokio::fs::File::open(&source.path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return result,
        Err(error) => {
            result.error = Some(format!("open {}: {error}", source.path.display()));
            return result;
        }
    };
    let file = file.into_std().await;
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            result.error = Some(format!("inspect {}: {error}", source.path.display()));
            return result;
        }
    };
    let file_id = match tracedecay_runtime_core::db::file_generation_identity(&file, &source.path) {
        Ok(file_id) => file_id,
        Err(error) => {
            result.error = Some(format!(
                "identify {} for durable import: {error:?}",
                source.path.display()
            ));
            return result;
        }
    };
    let mut file = tokio::fs::File::from_std(file);
    let file_len = metadata.len();
    let leading = match read_exact_window(&mut file, &source.path, 0, file_len.min(4096)).await {
        Ok(leading) => leading,
        Err(error) => {
            result.error = Some(error);
            return result;
        }
    };

    let cursor_key = import_cursor_key(&source.path);
    let expected_cursor = match gdb.get_parse_offset_result(&cursor_key).await {
        Ok(Some(cursor)) => cursor,
        Ok(None) => ParseOffset::default(),
        Err(err) => {
            result.error = Some(format!("read analytics import cursor: {err}"));
            return result;
        }
    };
    // Replacement and truncation restart from the top while retaining the
    // exact durable cursor as the compare-and-swap precondition.
    let start = if expected_cursor.file_id == file_id && expected_cursor.byte_offset <= file_len {
        let trailing = if expected_cursor.byte_offset > 4096 {
            match read_exact_window(
                &mut file,
                &source.path,
                expected_cursor.byte_offset - 4096,
                4096,
            )
            .await
            {
                Ok(trailing) => trailing,
                Err(error) => {
                    result.error = Some(error);
                    return result;
                }
            }
        } else {
            Vec::new()
        };
        let expected_leading_len = match usize::try_from(expected_cursor.byte_offset.min(4096)) {
            Ok(length) => length,
            Err(error) => {
                result.error = Some(format!(
                    "verify {} durable cursor window: {error}",
                    source.path.display()
                ));
                return result;
            }
        };
        let Some(expected_leading) = leading.get(..expected_leading_len) else {
            result.error = Some(format!(
                "verify {} durable cursor leading window",
                source.path.display()
            ));
            return result;
        };
        match tracedecay_runtime_core::db::resume_fingerprint_from_windows(
            expected_cursor.byte_offset,
            expected_leading,
            &trailing,
        ) {
            Ok(fingerprint) if fingerprint == expected_cursor.mtime => expected_cursor.byte_offset,
            Ok(_) => 0,
            Err(error) => {
                result.error = Some(format!(
                    "verify {} durable cursor: {error:?}",
                    source.path.display()
                ));
                return result;
            }
        }
    } else {
        0
    };
    if start == file_len {
        return result;
    }

    let read_start = start.saturating_sub(4096);
    let captured = match read_from_offset(&mut file, &source.path, read_start).await {
        Ok(captured) => captured,
        Err(err) => {
            result.error = Some(err);
            return result;
        }
    };
    let parse_start = match usize::try_from(start.saturating_sub(read_start)) {
        Ok(parse_start) if parse_start <= captured.len() => parse_start,
        _ => {
            result.error = Some(format!(
                "captured {} durable import bytes do not contain the admitted frontier",
                source.path.display()
            ));
            return result;
        }
    };
    let text = match std::str::from_utf8(&captured[parse_start..]) {
        Ok(text) => text,
        Err(error) => {
            result.error = Some(format!(
                "decode {} analytics import bytes: {error}",
                source.path.display()
            ));
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
    for (index, line) in text[..consumed].split_inclusive('\n').enumerate() {
        relative_offset = relative_offset.saturating_add(line.len() as u64);
        match hook_row_to_analytics_event(line.trim_end(), source.default_project_root.as_deref()) {
            Some(event) => batch.push((event, relative_offset)),
            None => result.skipped += 1,
        }
        if index % 256 == 255 {
            tokio::task::yield_now().await;
        }
    }
    let mut acknowledged = 0u64;
    let mut claimed_cursor = expected_cursor;
    for chunk in batch.chunks(IMPORT_BATCH_SIZE) {
        let events = chunk
            .iter()
            .map(|(event, _)| event.clone())
            .collect::<Vec<_>>();
        let frontier = chunk.last().map_or(acknowledged, |(_, offset)| *offset);
        let absolute_frontier = start + frontier;
        let fingerprint = match tracedecay_runtime_core::db::resume_fingerprint_from_capture(
            absolute_frontier,
            &leading,
            read_start,
            &captured,
        ) {
            Ok(fingerprint) => fingerprint,
            Err(error) => {
                result.error = Some(format!(
                    "anchor {} durable cursor: {error:?}",
                    source.path.display()
                ));
                return result;
            }
        };
        if let Err(err) = gdb
            .append_analytics_events_with_cursor(
                &events,
                &cursor_key,
                claimed_cursor,
                ParseOffset {
                    byte_offset: absolute_frontier,
                    mtime: fingerprint,
                    file_id,
                },
            )
            .await
        {
            result.error = Some(err);
            return result;
        }
        acknowledged = frontier;
        claimed_cursor = ParseOffset {
            byte_offset: absolute_frontier,
            mtime: fingerprint,
            file_id,
        };
        result.imported = result.imported.saturating_add(events.len() as u64);
    }
    if acknowledged < consumed as u64 {
        let absolute_frontier = start + consumed as u64;
        let fingerprint = match tracedecay_runtime_core::db::resume_fingerprint_from_capture(
            absolute_frontier,
            &leading,
            read_start,
            &captured,
        ) {
            Ok(fingerprint) => fingerprint,
            Err(error) => {
                result.error = Some(format!(
                    "anchor {} durable cursor: {error:?}",
                    source.path.display()
                ));
                return result;
            }
        };
        if let Err(err) = gdb
            .append_analytics_events_with_cursor(
                &[],
                &cursor_key,
                claimed_cursor,
                ParseOffset {
                    byte_offset: absolute_frontier,
                    mtime: fingerprint,
                    file_id,
                },
            )
            .await
        {
            result.error = Some(err);
        }
    }
    result
}

/// Namespaced `parse_offsets` key so hook cursors never collide with the
/// accounting transcript cursors that share the table.
fn import_cursor_key(path: &Path) -> String {
    format!("hook_analytics:{}", path.display())
}

async fn read_from_offset(
    file: &mut tokio::fs::File,
    path: &Path,
    offset: u64,
) -> Result<Vec<u8>, String> {
    file.seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(|err| format!("seek {}: {err}", path.display()))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .await
        .map_err(|err| format!("read {}: {err}", path.display()))?;
    Ok(bytes)
}

async fn read_exact_window(
    file: &mut tokio::fs::File,
    path: &Path,
    offset: u64,
    length: u64,
) -> Result<Vec<u8>, String> {
    file.seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(|err| format!("seek {}: {err}", path.display()))?;
    let length = usize::try_from(length)
        .map_err(|_| format!("read window for {} is too large", path.display()))?;
    let mut bytes = vec![0_u8; length];
    file.read_exact(&mut bytes)
        .await
        .map_err(|err| format!("read {} cursor window: {err}", path.display()))?;
    Ok(bytes)
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
    use std::io::{Seek, Write};

    use super::*;
    use tracedecay_global_db::AnalyticsEventQuery;

    #[tokio::test]
    async fn hook_import_restarts_after_same_file_rewrite_past_old_frontier() {
        let harness = tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open(
            "hook-import-same-file-rewrite",
        )
        .await;
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("hook_analytics.jsonl");
        let initial = json!({
            "event": "initial_event",
            "agent": "claude",
            "session_id": "initial",
            "padding": "x".repeat(128),
        })
        .to_string()
            + "\n";
        std::fs::write(&path, &initial).expect("initial hook source");
        let source = HookImportSource {
            path: path.clone(),
            default_project_root: None,
        };

        let first = import_source(&harness.registered, &source).await;
        assert_eq!(first.imported, 1);
        assert!(first.error.is_none());

        let replacement = json!({
            "event": "replacement_event",
            "agent": "claude",
            "session_id": "replacement",
            "padding": "y".repeat(initial.len() + 128),
        })
        .to_string()
            + "\n";
        assert!(replacement.len() >= initial.len());
        let mut retained = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("retain hook source");
        retained.set_len(0).expect("truncate in place");
        retained
            .seek(std::io::SeekFrom::Start(0))
            .expect("rewind replacement");
        retained
            .write_all(replacement.as_bytes())
            .expect("write replacement");
        retained.flush().expect("flush replacement");

        let second = import_source(&harness.registered, &source).await;
        assert_eq!(second.imported, 1);
        assert!(second.error.is_none());
        let replacement_rows = harness
            .registered
            .query_analytics_events(&AnalyticsEventQuery {
                event_kind: Some("replacement_event".to_owned()),
                limit: 10,
                ..AnalyticsEventQuery::default()
            })
            .await
            .expect("query replacement event");
        assert_eq!(replacement_rows.len(), 1);
        assert_eq!(
            replacement_rows[0].session_id.as_deref(),
            Some("replacement")
        );
    }
}

// The CLI entry points that used to close this file (`run_analytics_sync`,
// `run_analytics_diagnostics`, `call_admin_cli`, `analytics_sync_with_db`,
// `analytics_diagnostics_with_db`) stayed in the root binary: they drive
// `daemon::DaemonHandshake`, `dashboard::analytics_api` and the observability
// read models, none of which sit below this crate. Only the durable
// hook-JSONL importer moved down. See SEAMS.md.
