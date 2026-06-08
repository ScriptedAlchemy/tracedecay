use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::global_db::GlobalDb;
use crate::sessions::source::{
    ingest_source, stream_new_jsonl, title_from_messages, ParsedTranscript, SessionDraft,
    StoredCursor, TranscriptSource,
};
use crate::sessions::SessionMessageRecord;

const PROJECT_SESSION_DB_FILENAME: &str = "sessions.db";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CursorTranscriptIngestStats {
    pub sessions_upserted: u64,
    pub messages_upserted: u64,
}

pub fn project_session_db_path(project_root: &Path) -> PathBuf {
    crate::config::get_tokensave_dir(project_root).join(PROJECT_SESSION_DB_FILENAME)
}

pub async fn open_project_session_db(project_root: &Path) -> Option<GlobalDb> {
    GlobalDb::open_at(&project_session_db_path(project_root)).await
}

/// A Cursor hook event scoped to one transcript file. Cursor is hook-driven —
/// the transcript path, session id, and project all come from the event payload
/// rather than from a directory scan — so the source wraps the parsed event and
/// yields exactly that one path.
struct CursorEventSource {
    event: Value,
    transcript_path: PathBuf,
}

impl TranscriptSource for CursorEventSource {
    fn provider(&self) -> &'static str {
        "cursor"
    }

    fn transcript_paths(&self, _project_root: &Path) -> Vec<PathBuf> {
        vec![self.transcript_path.clone()]
    }

    fn parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        _project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        let new = stream_new_jsonl(path, prev, max_new_bytes)?;
        let session_id = event_session_id(&self.event, path);
        let mut messages = Vec::new();
        for line in &new.lines {
            // The byte offset doubles as the message ordinal and source_offset,
            // matching the original Cursor ingestion.
            if let Some(message) = event_message(
                &line.value,
                &self.event,
                &session_id,
                path,
                line.offset,
                line.offset,
            ) {
                messages.push(message);
            }
        }

        // Defer the (filesystem-walking) project/title/metadata derivation until
        // we actually have new messages; the driver ignores the draft otherwise.
        let draft = if messages.is_empty() {
            SessionDraft {
                session_id,
                project_key: String::new(),
                project_path: String::new(),
                title: None,
                metadata_json: None,
            }
        } else {
            let (project_key, project_path) = event_project(&self.event);
            SessionDraft {
                session_id,
                project_key,
                project_path,
                title: title_from_messages(&messages),
                metadata_json: serde_json::to_string(&session_metadata(&self.event)).ok(),
            }
        };

        Some(ParsedTranscript {
            draft,
            messages,
            new_cursor: new.new_cursor,
        })
    }
}

/// Ingest the Cursor transcript referenced by a hook payload into the
/// provider-neutral session/message tables for the provided database. Project
/// hooks should pass the project-local DB from [`open_project_session_db`].
///
/// Ingestion is **incremental**: it resumes from the byte offset recorded in the
/// DB's `parse_offsets` table (via the shared [`crate::sessions::source`]
/// driver), so each call only parses and upserts transcript lines appended since
/// the last run rather than re-reading the whole file. Repeated calls on an
/// unchanged file are a no-op.
pub async fn ingest_cursor_transcript_event(
    event_json: &str,
    db: &GlobalDb,
) -> CursorTranscriptIngestStats {
    ingest_cursor_transcript_event_capped(event_json, db, None).await
}

/// Like [`ingest_cursor_transcript_event`], but bounds how many newly-appended
/// bytes a single call will read. The Cursor `beforeSubmitPrompt` hot path passes
/// a small cap so it can never threaten the 5s hook budget; backlogs larger than
/// the cap are left for the lower-frequency `sessionStart` / `stop` hooks (which
/// pass `None` for an unbounded catch-up read).
pub async fn ingest_cursor_transcript_event_capped(
    event_json: &str,
    db: &GlobalDb,
    max_new_bytes: Option<u64>,
) -> CursorTranscriptIngestStats {
    let Ok(event) = serde_json::from_str::<Value>(event_json) else {
        return CursorTranscriptIngestStats::default();
    };
    let Some(transcript_path) = event
        .get("transcript_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
    else {
        return CursorTranscriptIngestStats::default();
    };

    // Cursor derives its project from the event, so the driver's project_root
    // argument is unused by `CursorEventSource`; the transcript path's parent is
    // a cheap, side-effect-free placeholder.
    let project_root = transcript_path
        .parent()
        .map_or_else(|| transcript_path.clone(), Path::to_path_buf);
    let source = CursorEventSource {
        event,
        transcript_path,
    };
    let stats = ingest_source(db, &source, &project_root, max_new_bytes).await;
    CursorTranscriptIngestStats {
        sessions_upserted: stats.sessions_upserted,
        messages_upserted: stats.messages_upserted,
    }
}

fn event_message(
    record: &Value,
    event: &Value,
    session_id: &str,
    transcript_path: &Path,
    ordinal: i64,
    source_offset: i64,
) -> Option<SessionMessageRecord> {
    let role = record
        .get("role")
        .and_then(Value::as_str)
        .filter(|role| !role.is_empty())?;
    let message = record.get("message").unwrap_or(record);
    let content = message.get("content").unwrap_or(message);
    let (text, tool_names) = content_text_and_tools(content);
    if text.trim().is_empty() {
        return None;
    }

    let message_id = record
        .get("id")
        .or_else(|| message.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map_or_else(
            || format!("{session_id}:{ordinal}"),
            std::string::ToString::to_string,
        );
    let model = record
        .get("model")
        .or_else(|| message.get("model"))
        .or_else(|| event.get("model"))
        .and_then(Value::as_str)
        .map(str::to_string);

    Some(SessionMessageRecord {
        provider: "cursor".to_string(),
        message_id,
        session_id: session_id.to_string(),
        role: role.to_string(),
        timestamp: record_timestamp(record).or_else(|| record_timestamp(event)),
        ordinal,
        text,
        kind: content_kind(content).map(str::to_string),
        model,
        tool_names: (!tool_names.is_empty()).then(|| tool_names.join(",")),
        source_path: Some(transcript_path.to_string_lossy().to_string()),
        source_offset: Some(source_offset),
        metadata_json: serde_json::to_string(&message_metadata(record)).ok(),
    })
}

fn content_text_and_tools(content: &Value) -> (String, Vec<String>) {
    if let Some(text) = content.as_str() {
        return (text.to_string(), Vec::new());
    }
    let Some(items) = content.as_array() else {
        return (
            content
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            Vec::new(),
        );
    };

    let mut texts = Vec::new();
    let mut tools = Vec::new();
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    texts.push(text.to_string());
                }
            }
            Some("tool_use") => {
                if let Some(name) = item.get("name").and_then(Value::as_str) {
                    tools.push(name.to_string());
                }
            }
            _ => {}
        }
    }
    (texts.join("\n\n"), tools)
}

fn content_kind(content: &Value) -> Option<&'static str> {
    if content.is_array() {
        Some("message")
    } else if content.is_string() {
        Some("text")
    } else {
        None
    }
}

fn event_session_id(event: &Value, transcript_path: &Path) -> String {
    event
        .get("session_id")
        .or_else(|| event.get("conversation_id"))
        .or_else(|| event.get("chat_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map_or_else(
            || {
                transcript_path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("unknown")
                    .to_string()
            },
            str::to_string,
        )
}

fn event_project(event: &Value) -> (String, String) {
    let candidates = event_project_candidates(event);
    let project = candidates
        .iter()
        .find_map(|candidate| crate::config::discover_project_root(candidate))
        .or_else(|| candidates.into_iter().next())
        .map_or_else(
            || "unknown".to_string(),
            |path| path.to_string_lossy().to_string(),
        );
    (project.clone(), project)
}

fn event_project_candidates(event: &Value) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(roots) = event.get("workspace_roots").and_then(Value::as_array) {
        for root in roots {
            if let Some(path) = root.as_str().filter(|path| !path.is_empty()) {
                candidates.push(PathBuf::from(path));
            }
        }
    }
    if let Some(cwd) = event
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
    {
        candidates.push(PathBuf::from(cwd));
    }
    if let Some(file_path) = event
        .get("file_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
    {
        candidates.push(PathBuf::from(file_path));
    }
    candidates
}

fn record_timestamp(value: &Value) -> Option<i64> {
    value
        .get("timestamp")
        .or_else(|| value.get("created_at"))
        .and_then(|timestamp| {
            timestamp
                .as_i64()
                .or_else(|| timestamp.as_str().and_then(|s| s.parse::<i64>().ok()))
        })
}

fn session_metadata(event: &Value) -> Value {
    serde_json::json!({
        "source": "cursor_transcript",
        "conversation_id": event.get("conversation_id").cloned(),
        "hook_event_name": event.get("hook_event_name").cloned(),
        "cursor_version": event.get("cursor_version").cloned(),
    })
}

fn message_metadata(record: &Value) -> Value {
    serde_json::json!({
        "source": "cursor_transcript",
        "raw_type": record.get("type").cloned(),
    })
}
