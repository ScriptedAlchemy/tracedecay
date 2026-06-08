use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::global_db::GlobalDb;
use crate::sessions::{SessionMessageRecord, SessionRecord};

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

/// Ingest the Cursor transcript referenced by a hook payload into the
/// provider-neutral session/message tables for the provided database. Project
/// hooks should pass the project-local DB from [`open_project_session_db`].
///
/// Ingestion is **incremental**: it resumes from the byte offset recorded in the
/// DB's `parse_offsets` table (mirroring the Claude accounting parser), so each
/// call only parses and upserts transcript lines appended since the last run
/// rather than re-reading the whole file. Repeated calls on an unchanged file
/// are a no-op.
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

    ingest_cursor_transcript(&event, &transcript_path, db, max_new_bytes).await
}

async fn ingest_cursor_transcript(
    event: &Value,
    transcript_path: &Path,
    db: &GlobalDb,
    max_new_bytes: Option<u64>,
) -> CursorTranscriptIngestStats {
    let path_str = transcript_path.to_string_lossy().to_string();
    let Ok(meta) = std::fs::metadata(transcript_path) else {
        return CursorTranscriptIngestStats::default();
    };
    let file_size = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());

    let (prev_offset, prev_mtime) = db.get_parse_offset(&path_str).await.unwrap_or((0, 0));

    // Resume from the saved offset on an append; restart from the beginning when
    // the file shrank (truncation) or its mtime regressed (rewrite).
    let resume_from_prev = prev_offset > 0 && file_size >= prev_offset && mtime >= prev_mtime;
    let seek_to = if resume_from_prev { prev_offset } else { 0 };

    // Nothing new to read. Refresh the stored mtime so we don't keep re-stat-ing,
    // then no-op.
    if seek_to >= file_size {
        if mtime != prev_mtime {
            db.set_parse_offset(&path_str, seek_to, mtime).await;
        }
        return CursorTranscriptIngestStats::default();
    }

    // Hot-path guard: defer large backlogs to the lower-frequency hooks rather
    // than risk the prompt-submit budget.
    if let Some(cap) = max_new_bytes {
        if file_size.saturating_sub(seek_to) > cap {
            return CursorTranscriptIngestStats::default();
        }
    }

    let Ok(file) = std::fs::File::open(transcript_path) else {
        return CursorTranscriptIngestStats::default();
    };
    let mut reader = BufReader::new(file);
    if seek_to > 0 && reader.seek(SeekFrom::Start(seek_to)).is_err() {
        return CursorTranscriptIngestStats::default();
    }

    let session_id = event_session_id(event, transcript_path);
    let mut parsed = Vec::new();
    let mut offset = seek_to;
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                // A line without a trailing newline is a partial write at EOF:
                // stop without consuming it so the next call re-reads it whole.
                if !line.ends_with('\n') {
                    break;
                }
                let line_offset = offset;
                offset = offset.saturating_add(n as u64);
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(record) = serde_json::from_str::<Value>(trimmed) else {
                    continue;
                };
                if let Some(message) = event_message(
                    &record,
                    event,
                    &session_id,
                    transcript_path,
                    line_offset as i64,
                    line_offset as i64,
                ) {
                    parsed.push(message);
                }
            }
        }
    }

    // Persist progress (even when no messages were parsed) so subsequent calls
    // only see genuinely new bytes.
    db.set_parse_offset(&path_str, offset, mtime).await;

    if parsed.is_empty() {
        return CursorTranscriptIngestStats::default();
    }

    let (project_key, project_path) = event_project(event);
    let existing = db.get_session("cursor", &session_id).await;
    // Preserve the session's original start time and title across incremental
    // appends; only advance ended_at to the latest message seen.
    let started_at = existing
        .as_ref()
        .and_then(|session| session.started_at)
        .or_else(|| parsed.first().and_then(|message| message.timestamp));
    let title = existing
        .as_ref()
        .and_then(|session| session.title.clone())
        .or_else(|| {
            parsed
                .iter()
                .find(|message| message.role == "user")
                .map(|message| preview_title(&message.text))
        });
    let ended_at = parsed
        .last()
        .and_then(|message| message.timestamp)
        .or_else(|| existing.as_ref().and_then(|session| session.ended_at));
    let session = SessionRecord {
        provider: "cursor".to_string(),
        session_id,
        project_key,
        project_path,
        title,
        started_at,
        ended_at,
        transcript_path: Some(transcript_path.to_string_lossy().to_string()),
        metadata_json: serde_json::to_string(&session_metadata(event)).ok(),
    };

    let sessions_upserted = u64::from(db.upsert_session(&session).await);
    let mut messages_upserted = 0_u64;
    for message in &parsed {
        if db.upsert_session_message(message).await {
            messages_upserted = messages_upserted.saturating_add(1);
        }
    }

    CursorTranscriptIngestStats {
        sessions_upserted,
        messages_upserted,
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

fn preview_title(text: &str) -> String {
    const MAX_TITLE_CHARS: usize = 80;

    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_TITLE_CHARS {
        collapsed
    } else {
        collapsed.chars().take(MAX_TITLE_CHARS).collect()
    }
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
