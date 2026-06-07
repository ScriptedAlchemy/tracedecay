use std::{
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::global_db::GlobalDb;

/// Provider-neutral metadata for an indexed agent session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub provider: String,
    pub session_id: String,
    pub project_key: String,
    pub project_path: String,
    pub title: Option<String>,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub transcript_path: Option<String>,
    pub metadata_json: Option<String>,
}

/// Provider-neutral message payload extracted from an agent transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMessageRecord {
    pub provider: String,
    pub message_id: String,
    pub session_id: String,
    pub role: String,
    pub timestamp: Option<i64>,
    pub ordinal: i64,
    pub text: String,
    pub kind: Option<String>,
    pub model: Option<String>,
    pub tool_names: Option<String>,
    pub source_path: Option<String>,
    pub source_offset: Option<i64>,
    pub metadata_json: Option<String>,
}

/// Search hit for session-message full-text lookup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMessageSearchResult {
    pub session: SessionRecord,
    pub message: SessionMessageRecord,
    pub score: f64,
}

/// Transcript providers supported by the sample ingestion path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionIngestProvider {
    Cursor,
    Codex,
    All,
}

/// Explicit roots used by tests and local callers instead of reading real homes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionIngestRoots {
    pub cursor_home: PathBuf,
    pub codex_home: PathBuf,
}

/// Minimal token usage summary reported by transcript ingestion.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionTokenUsage {
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub output_tokens: u64,
}

/// Best-effort counters for a transcript ingestion run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionIngestStats {
    pub files_seen: usize,
    pub messages_inserted: usize,
    pub malformed_lines: usize,
    pub token_usages: Vec<SessionTokenUsage>,
}

struct CursorFileContext {
    session_id: String,
    project_key: String,
    project_path: String,
    transcript_path: String,
    ordinal: i64,
}

struct CodexFileContext {
    session_id: String,
    project_key: String,
    project_path: String,
    transcript_path: String,
    ordinal: i64,
    model: Option<String>,
    started_at: Option<i64>,
}

/// Ingests the small Cursor/Codex fixture shapes currently covered by tests.
pub async fn ingest_sessions_from_roots(
    db: &GlobalDb,
    provider: SessionIngestProvider,
    roots: &SessionIngestRoots,
) -> SessionIngestStats {
    let mut stats = SessionIngestStats::default();

    if matches!(
        provider,
        SessionIngestProvider::Cursor | SessionIngestProvider::All
    ) {
        ingest_cursor_sessions(db, &roots.cursor_home, &mut stats).await;
    }

    if matches!(
        provider,
        SessionIngestProvider::Codex | SessionIngestProvider::All
    ) {
        ingest_codex_sessions(db, &roots.codex_home, &mut stats).await;
    }

    stats
}

async fn ingest_cursor_sessions(db: &GlobalDb, cursor_home: &Path, stats: &mut SessionIngestStats) {
    let mut files = Vec::new();
    collect_jsonl_files(&cursor_home.join(".cursor").join("projects"), &mut files);

    files.retain(|path| {
        path.components()
            .any(|component| component.as_os_str() == "agent-transcripts")
    });

    for path in files {
        stats.files_seen += 1;
        let mut context = cursor_context_for_path(&path);
        for (value, line_offset) in read_jsonl_records(db, "cursor", &path, stats).await {
            parse_cursor_record(db, value, line_offset, stats, &mut context).await;
        }
    }
}

async fn ingest_codex_sessions(db: &GlobalDb, codex_home: &Path, stats: &mut SessionIngestStats) {
    let mut files = Vec::new();
    collect_jsonl_files(&codex_home.join(".codex").join("sessions"), &mut files);

    for path in files {
        stats.files_seen += 1;
        let mut context = codex_context_for_path(&path);
        for (value, line_offset) in read_jsonl_records(db, "codex", &path, stats).await {
            parse_codex_record(db, value, line_offset, stats, &mut context).await;
        }
    }
}

async fn read_jsonl_records(
    db: &GlobalDb,
    provider: &str,
    path: &Path,
    stats: &mut SessionIngestStats,
) -> Vec<(Value, u64)> {
    let offset_key = format!("{provider}:{}", stable_path_string(path));
    let saved_offset = db
        .get_parse_offset(&offset_key)
        .await
        .map_or(0, |(offset, _)| offset);

    let Ok(bytes) = fs::read(path) else {
        return Vec::new();
    };
    let start = if saved_offset <= bytes.len() as u64 {
        saved_offset as usize
    } else {
        0
    };
    let mut current_offset = start as u64;
    let mut records = Vec::new();

    for chunk in bytes[start..].split_inclusive(|byte| *byte == b'\n') {
        let line_offset = current_offset;
        current_offset = current_offset.saturating_add(chunk.len() as u64);
        let line = trim_jsonl_line(chunk);
        if line.is_empty() {
            continue;
        }

        let Ok(line) = std::str::from_utf8(line) else {
            stats.malformed_lines += 1;
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            stats.malformed_lines += 1;
            continue;
        };

        records.push((value, line_offset));
    }

    db.set_parse_offset(&offset_key, bytes.len() as u64, file_mtime(path))
        .await;
    records
}

async fn parse_cursor_record(
    db: &GlobalDb,
    value: Value,
    line_offset: u64,
    stats: &mut SessionIngestStats,
    context: &mut CursorFileContext,
) {
    let Some(role) = value.get("role").and_then(Value::as_str) else {
        return;
    };
    let Some(content) = value
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
    else {
        return;
    };

    let (text, tool_names) = text_and_tools_from_content(content, &["text"]);
    if text.is_empty() {
        return;
    }

    let session = SessionRecord {
        provider: "cursor".to_string(),
        session_id: context.session_id.clone(),
        project_key: context.project_key.clone(),
        project_path: context.project_path.clone(),
        title: None,
        started_at: None,
        ended_at: None,
        transcript_path: Some(context.transcript_path.clone()),
        metadata_json: None,
    };
    if !db.upsert_session(&session).await {
        return;
    }

    let model = value
        .get("model")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let message = SessionMessageRecord {
        provider: "cursor".to_string(),
        message_id: format!(
            "{}:{}:{line_offset}",
            context.session_id, context.transcript_path
        ),
        session_id: context.session_id.clone(),
        role: role.to_string(),
        timestamp: None,
        ordinal: context.ordinal,
        text,
        kind: Some("message".to_string()),
        model,
        tool_names,
        source_path: Some(context.transcript_path.clone()),
        source_offset: Some(line_offset as i64),
        metadata_json: None,
    };
    if db.upsert_session_message(&message).await {
        context.ordinal += 1;
        stats.messages_inserted += 1;
    }
}

async fn parse_codex_record(
    db: &GlobalDb,
    value: Value,
    line_offset: u64,
    stats: &mut SessionIngestStats,
    context: &mut CodexFileContext,
) {
    match value.get("type").and_then(Value::as_str) {
        Some("session_meta") => {
            if let Some(id) = value
                .get("payload")
                .and_then(|payload| payload.get("id"))
                .and_then(Value::as_str)
            {
                context.session_id = id.to_string();
            }
            upsert_codex_session(db, context).await;
        }
        Some("turn_context") => {
            let payload = value.get("payload");
            if let Some(cwd) = payload
                .and_then(|payload| payload.get("cwd"))
                .and_then(Value::as_str)
            {
                context.project_key = cwd.to_string();
                context.project_path = cwd.to_string();
            }
            context.model = payload
                .and_then(|payload| payload.get("model"))
                .and_then(Value::as_str)
                .map(ToString::to_string);
            upsert_codex_session(db, context).await;
        }
        Some("response_item") => {
            parse_codex_response_item(db, value, line_offset, stats, context).await;
        }
        Some("event_msg") => {
            if let Some(token_usage) = codex_token_usage(&value) {
                stats.token_usages.push(token_usage);
            }
        }
        _ => {}
    }
}

async fn parse_codex_response_item(
    db: &GlobalDb,
    value: Value,
    line_offset: u64,
    stats: &mut SessionIngestStats,
    context: &mut CodexFileContext,
) {
    let Some(item) = value.get("payload").and_then(|payload| payload.get("item")) else {
        return;
    };
    if item.get("type").and_then(Value::as_str) != Some("message") {
        return;
    }
    let Some(role) = item.get("role").and_then(Value::as_str) else {
        return;
    };
    let Some(content) = item.get("content").and_then(Value::as_array) else {
        return;
    };

    let (text, tool_names) = text_and_tools_from_content(content, &["output_text", "text"]);
    if text.is_empty() {
        return;
    }
    if !upsert_codex_session(db, context).await {
        return;
    }

    let local_id = item
        .get("id")
        .and_then(Value::as_str)
        .map_or_else(|| line_offset.to_string(), ToString::to_string);
    let message = SessionMessageRecord {
        provider: "codex".to_string(),
        message_id: format!(
            "{}:{local_id}:{}",
            context.session_id, context.transcript_path
        ),
        session_id: context.session_id.clone(),
        role: role.to_string(),
        timestamp: None,
        ordinal: context.ordinal,
        text,
        kind: Some("message".to_string()),
        model: context.model.clone(),
        tool_names,
        source_path: Some(context.transcript_path.clone()),
        source_offset: Some(line_offset as i64),
        metadata_json: None,
    };
    if db.upsert_session_message(&message).await {
        context.ordinal += 1;
        stats.messages_inserted += 1;
    }
}

async fn upsert_codex_session(db: &GlobalDb, context: &CodexFileContext) -> bool {
    let session = SessionRecord {
        provider: "codex".to_string(),
        session_id: context.session_id.clone(),
        project_key: context.project_key.clone(),
        project_path: context.project_path.clone(),
        title: None,
        started_at: context.started_at,
        ended_at: None,
        transcript_path: Some(context.transcript_path.clone()),
        metadata_json: None,
    };
    db.upsert_session(&session).await
}

fn cursor_context_for_path(path: &Path) -> CursorFileContext {
    let transcript_path = stable_path_string(path);
    let session_id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("cursor-session")
        .to_string();
    let project_key = component_after(path, "projects").unwrap_or_else(|| "unknown".to_string());

    CursorFileContext {
        session_id,
        project_path: project_key.clone(),
        project_key,
        transcript_path,
        ordinal: 0,
    }
}

fn codex_context_for_path(path: &Path) -> CodexFileContext {
    let transcript_path = stable_path_string(path);
    let session_id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("codex-session")
        .to_string();

    CodexFileContext {
        session_id,
        project_key: String::new(),
        project_path: String::new(),
        transcript_path,
        ordinal: 0,
        model: None,
        started_at: None,
    }
}

fn text_and_tools_from_content(
    content: &[Value],
    text_block_types: &[&str],
) -> (String, Option<String>) {
    let mut text_parts = Vec::new();
    let mut tool_names = Vec::new();

    for block in content {
        let block_type = block.get("type").and_then(Value::as_str);
        if block_type.is_some_and(|kind| text_block_types.contains(&kind)) {
            if let Some(text) = block.get("text").and_then(Value::as_str) {
                text_parts.push(text.to_string());
            }
        } else if block_type == Some("tool_use") {
            if let Some(name) = block.get("name").and_then(Value::as_str) {
                tool_names.push(name.to_string());
            }
        }
    }

    let text = text_parts.join("\n");
    let tool_names = if tool_names.is_empty() {
        None
    } else {
        Some(tool_names.join(","))
    };
    (text, tool_names)
}

fn codex_token_usage(value: &Value) -> Option<SessionTokenUsage> {
    let msg = value.get("msg")?;
    if msg.get("type").and_then(Value::as_str) != Some("token_count") {
        return None;
    }
    let usage = msg.get("info")?.get("last_token_usage")?;
    Some(SessionTokenUsage {
        input_tokens: usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_read_tokens: usage
            .get("cached_input_tokens")
            .or_else(|| usage.get("cache_read_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
}

fn collect_jsonl_files(root: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_files(&path, files);
        } else if path
            .extension()
            .is_some_and(|extension| extension.to_string_lossy() == "jsonl")
        {
            files.push(path);
        }
    }
    files.sort();
}

fn component_after(path: &Path, marker: &str) -> Option<String> {
    let mut components = path.components();
    while let Some(component) = components.next() {
        if component.as_os_str() == marker {
            return components
                .next()
                .and_then(|next| next.as_os_str().to_str())
                .map(ToString::to_string);
        }
    }
    None
}

fn trim_jsonl_line(mut line: &[u8]) -> &[u8] {
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line = &line[..line.len().saturating_sub(1)];
    }
    line
}

fn file_mtime(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs())
}

fn stable_path_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
