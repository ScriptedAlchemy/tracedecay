//! Bridges hook telemetry into the durable `analytics_events` table.
//!
//! Hooks append JSONL rows to `hook_analytics.jsonl` (project store when the
//! hook can resolve a project root, user-level profile root otherwise), while
//! the MCP server writes `mcp_tool_call` / `hook_route` rows straight into the
//! user-level global DB. This module imports the JSONL side into
//! `analytics_events` so one durable table answers adoption questions, using
//! per-file byte cursors in `parse_offsets` to stay idempotent across runs.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::global_db::{AnalyticsEventInsert, GlobalDb, ParseOffset};

/// Largest batch handed to a single `append_analytics_events` transaction.
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
    if let Some(root) = project_root {
        if let Ok(layout) = crate::storage::resolve_layout_for_current_profile(root) {
            sources.push(HookImportSource {
                path: layout.data_root.join("hook_analytics.jsonl"),
                default_project_root: Some(root.to_path_buf()),
            });
        }
    }
    if let Ok(profile_root) = crate::storage::default_profile_root() {
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
    gdb: &GlobalDb,
    sources: &[HookImportSource],
) -> HookImportOutcome {
    let mut outcome = HookImportOutcome::default();
    for source in sources {
        outcome.sources.push(import_source(gdb, source).await);
    }
    outcome
}

async fn import_source(gdb: &GlobalDb, source: &HookImportSource) -> HookImportSourceOutcome {
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
    for line in text[..consumed].lines() {
        match hook_row_to_analytics_event(line, source.default_project_root.as_deref()) {
            Some(event) => batch.push(event),
            None => result.skipped += 1,
        }
    }
    for chunk in batch.chunks(IMPORT_BATCH_SIZE) {
        if let Err(err) = gdb.append_analytics_events(chunk).await {
            result.error = Some(err);
            return result;
        }
        result.imported += chunk.len() as u64;
    }

    gdb.set_parse_offset(
        &cursor_key,
        ParseOffset {
            byte_offset: start + consumed as u64,
            mtime,
            file_id: 0,
        },
    )
    .await;
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
        .map(|root| GlobalDb::canonical_project_key(Path::new(root)))
        .or_else(|| default_project_root.map(GlobalDb::canonical_project_key))
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

// ── CLI entry points (`tracedecay analytics …`) ────────────────────────

fn cli_error(message: String) -> crate::errors::TraceDecayError {
    crate::errors::TraceDecayError::Config { message }
}

fn cli_project_root() -> Option<PathBuf> {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| crate::config::discover_project_root(&cwd))
}

async fn diagnostics_message_count(
    global: &GlobalDb,
    project_root: Option<&Path>,
    all_projects: bool,
) -> i64 {
    if all_projects {
        let mut session_db_paths = BTreeSet::new();
        if let Some(profile_root) = global.db_path().parent() {
            session_db_paths.insert(crate::sessions::user_sessions_db_path(profile_root));
        }
        for project_root in crate::sessions::registered_project_roots_from(global)
            .await
            .unwrap_or_default()
        {
            if let Some(db_path) =
                crate::sessions::cursor::resolved_project_session_db_path(&project_root).await
            {
                session_db_paths.insert(db_path);
            }
        }
        let mut total = 0;
        for db_path in session_db_paths {
            if let Some(sessions) = GlobalDb::open_read_only_at(&db_path).await {
                total += sessions.session_message_count().await.unwrap_or(0);
            }
        }
        return total;
    }
    let Some(project_root) = project_root else {
        return 0;
    };
    let Some(db_path) =
        crate::sessions::cursor::resolved_project_session_db_path(project_root).await
    else {
        return 0;
    };
    let Some(sessions) = GlobalDb::open_read_only_at(&db_path).await else {
        return 0;
    };
    sessions.session_message_count().await.unwrap_or(0)
}

/// `tracedecay analytics sync`: import hook JSONL rows into the durable
/// `analytics_events` table and print what happened.
pub async fn run_analytics_sync() -> crate::errors::Result<()> {
    let project_root = cli_project_root();
    let outcome = call_admin_cli(project_root, json!({ "action": "analytics_sync" })).await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&outcome).unwrap_or_default()
    );
    Ok(())
}

/// `tracedecay analytics diagnostics`: the CLI wrapper around the dashboard
/// diagnostics summary — durable `analytics_events` plus merged hook JSONL.
pub async fn run_analytics_diagnostics(
    all_projects: bool,
    no_sync: bool,
) -> crate::errors::Result<()> {
    let project_root = cli_project_root();
    let summary = call_admin_cli(
        project_root,
        json!({
            "action": "analytics_diagnostics",
            "all": all_projects,
            "no_sync": no_sync,
        }),
    )
    .await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).unwrap_or_default()
    );
    Ok(())
}

async fn call_admin_cli(
    project_root: Option<PathBuf>,
    arguments: Value,
) -> crate::errors::Result<Value> {
    let handshake =
        crate::daemon::DaemonHandshake::for_current_client(project_root, None, false, false)?;
    let result =
        crate::daemon::call_default_tool(&handshake, "tracedecay_admin_cli", arguments).await?;
    crate::daemon::tool_json_payload(&result, "tracedecay_admin_cli")
}

pub(crate) async fn analytics_sync_with_db(gdb: &GlobalDb, project_root: Option<&Path>) -> Value {
    let sources = hook_import_sources(project_root);
    import_hook_analytics(gdb, &sources).await.as_json()
}

pub(crate) async fn analytics_diagnostics_with_db(
    gdb: &GlobalDb,
    project_root: Option<&Path>,
    all_projects: bool,
    no_sync: bool,
) -> crate::errors::Result<Value> {
    const EVENT_SAMPLE_LIMIT: usize = 10_000;

    let import = if no_sync {
        Value::Null
    } else {
        analytics_sync_with_db(gdb, project_root).await
    };

    let project_filter = if all_projects {
        None
    } else {
        project_root.map(GlobalDb::canonical_project_key)
    };
    let events = gdb
        .query_analytics_events(&crate::global_db::AnalyticsEventQuery {
            provider: None,
            project_id: project_filter.clone(),
            session_id: None,
            event_kind: None,
            since: None,
            limit: EVENT_SAMPLE_LIMIT,
        })
        .await
        .map_err(cli_error)?;
    let event_rows: Vec<Value> = events.iter().map(durable_analytics_event_row).collect();

    let store_root = project_root.and_then(|root| {
        crate::storage::resolve_layout_for_current_profile(root)
            .ok()
            .map(|layout| layout.data_root)
    });
    let hook_filter_root = if all_projects { None } else { project_root };
    let hook_analytics = crate::dashboard::analytics_api::read_hook_analytics_rows_at(
        store_root.as_deref(),
        hook_filter_root,
    );

    let message_count = diagnostics_message_count(gdb, project_root, all_projects).await;

    let durable = if event_rows.is_empty() {
        None
    } else {
        Some(event_rows.as_slice())
    };
    let mut summary = crate::dashboard::analytics_api::diagnostics_summary_from_parts(
        message_count,
        &hook_analytics,
        durable,
    );
    if let Some(summary) = summary.as_object_mut() {
        summary.insert(
            "project_id".to_string(),
            project_filter.map_or(Value::Null, Value::String),
        );
        summary.insert("import".to_string(), import);
        summary.insert(
            "global_db".to_string(),
            json!(gdb.db_path().display().to_string()),
        );
        summary.insert("event_sample_limit".to_string(), json!(EVENT_SAMPLE_LIMIT));
        summary.insert(
            "event_count_may_be_truncated".to_string(),
            json!(event_rows.len() >= EVENT_SAMPLE_LIMIT),
        );
    }
    Ok(summary)
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use std::path::Path;

    use super::{diagnostics_message_count, hook_row_to_analytics_event};
    use crate::global_db::GlobalDb;
    use crate::sessions::{SessionMessageRecord, SessionRecord};

    async fn seed_session_message(db: &GlobalDb, project: &Path, id: &str) {
        db.upsert_session(&SessionRecord {
            provider: "codex".to_string(),
            session_id: id.to_string(),
            project_key: project.display().to_string(),
            project_path: project.display().to_string(),
            title: None,
            started_at: Some(1),
            ended_at: None,
            transcript_path: None,
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        })
        .await;
        assert!(
            db.upsert_session_message(&SessionMessageRecord {
                provider: "codex".to_string(),
                message_id: format!("{id}-message"),
                session_id: id.to_string(),
                role: "user".to_string(),
                timestamp: Some(1),
                ordinal: 1,
                text: "diagnostics evidence".to_string(),
                kind: None,
                model: None,
                tool_names: None,
                source_path: None,
                source_offset: None,
                metadata_json: None,
            })
            .await
        );
    }

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

    #[tokio::test]
    async fn diagnostics_counts_messages_from_the_project_session_shard() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project tempdir");
        let layout = crate::storage::resolve_layout_for_current_profile(project.path())
            .expect("project layout");
        std::fs::create_dir_all(&layout.data_root).expect("project data root");

        let global = GlobalDb::open().await.expect("global db");
        let sessions = GlobalDb::open_at(&layout.sessions_db_path)
            .await
            .expect("project session db");
        seed_session_message(&sessions, project.path(), "project-session").await;

        assert_eq!(global.session_message_count().await.unwrap(), 0);
        assert_eq!(
            diagnostics_message_count(&global, Some(project.path()), false).await,
            1
        );
    }

    #[tokio::test]
    async fn all_project_diagnostics_count_registered_session_shards() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project tempdir");
        let layout = crate::storage::resolve_layout_for_current_profile(project.path())
            .expect("project layout");
        std::fs::create_dir_all(&layout.data_root).expect("project data root");

        let global = GlobalDb::open().await.expect("global db");
        global
            .upsert_code_project("project-shard", project.path(), None, None, Some("main"))
            .await;
        let sessions = GlobalDb::open_at(&layout.sessions_db_path)
            .await
            .expect("project session db");
        seed_session_message(&sessions, project.path(), "registered-session").await;

        assert_eq!(global.session_message_count().await.unwrap(), 0);
        assert_eq!(diagnostics_message_count(&global, None, true).await, 1);
    }
}
fn durable_analytics_event_row(event: &crate::global_db::AnalyticsEventRecord) -> Value {
    json!({
        "provider": &event.provider,
        "timestamp": event.timestamp,
        "event_kind": &event.event_kind,
        "hook_name": &event.hook_name,
        "tool_name": &event.tool_name,
        "tool_category": &event.tool_category,
        "skill_name": &event.skill_name,
        "hint_category": &event.hint_category,
        "outcome": &event.outcome,
        "metadata_json": &event.metadata_json,
    })
}
