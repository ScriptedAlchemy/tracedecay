use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::global_db::GlobalDb;
use crate::sessions::source::{
    append_tool_calls_metadata, append_usage_metadata, content_storage_text_and_tools,
    ingest_source, paths_equal, stream_new_jsonl, title_from_messages, ParsedTranscript,
    SessionDraft, StoredCursor, TranscriptSource,
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

pub fn hermes_profile_session_db_path(hermes_home: &Path) -> PathBuf {
    hermes_home
        .join(".tokensave")
        .join(PROJECT_SESSION_DB_FILENAME)
}

pub fn resolve_hermes_profile_session_db_path(
    hermes_home: &Path,
) -> std::result::Result<PathBuf, String> {
    Ok(resolve_hermes_profile_tokensave_dir(hermes_home, true)?.join(PROJECT_SESSION_DB_FILENAME))
}

/// Typed outcome of [`resolve_hermes_profile_session_db_readonly`].
pub enum HermesProfileDbReadOnly {
    /// sessions.db exists and is ready to open read-only.
    Exists(PathBuf),
    /// The `.tokensave` dir and path are valid but sessions.db is absent —
    /// nothing has been ingested yet. Carries the path the store would live
    /// at so callers can report it.
    NotIngested(PathBuf),
    /// A security or configuration error (symlink escape, bad path, etc.)
    /// that should be surfaced as a hard error.
    ConfigError(String),
}

/// Resolves the path to the hermes profile session DB for read-only access,
/// distinguishing "valid path but file not yet created" from security /
/// configuration errors such as symlink escapes or non-directory `.tokensave`.
pub fn resolve_hermes_profile_session_db_readonly(hermes_home: &Path) -> HermesProfileDbReadOnly {
    let dir = match resolve_hermes_profile_tokensave_dir(hermes_home, false) {
        Ok(dir) => dir,
        Err(msg) => return HermesProfileDbReadOnly::ConfigError(msg),
    };
    let db_path = dir.join(PROJECT_SESSION_DB_FILENAME);
    if db_path.is_file() {
        HermesProfileDbReadOnly::Exists(db_path)
    } else {
        HermesProfileDbReadOnly::NotIngested(db_path)
    }
}

fn resolve_hermes_profile_tokensave_dir(
    hermes_home: &Path,
    create_missing: bool,
) -> std::result::Result<PathBuf, String> {
    let tokensave_dir = hermes_home.join(".tokensave");
    match std::fs::symlink_metadata(&tokensave_dir) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(format!(
                    "hermes_profile LCM storage rejects symlinked .tokensave directory: {}",
                    tokensave_dir.display()
                ));
            }
            if !metadata.is_dir() {
                return Err(format!(
                    "hermes_profile LCM storage requires .tokensave to be a directory: {}",
                    tokensave_dir.display()
                ));
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound && create_missing => {
            std::fs::create_dir_all(&tokensave_dir).map_err(|err| {
                format!(
                    "could not create hermes_profile .tokensave directory {}: {err}",
                    tokensave_dir.display()
                )
            })?;
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "hermes_profile LCM storage requires an existing .tokensave directory: {}",
                tokensave_dir.display()
            ));
        }
        Err(err) => {
            return Err(format!(
                "could not inspect hermes_profile .tokensave directory {}: {err}",
                tokensave_dir.display()
            ));
        }
    }

    let canonical_parent = tokensave_dir.canonicalize().map_err(|err| {
        format!(
            "could not resolve hermes_profile .tokensave directory {}: {err}",
            tokensave_dir.display()
        )
    })?;
    if !canonical_parent.starts_with(hermes_home) {
        return Err(format!(
            "hermes_profile LCM storage path must stay inside hermes_home: {}",
            canonical_parent.display()
        ));
    }
    Ok(canonical_parent)
}

/// A Cursor hook event scoped to one transcript file.
struct CursorEventSource {
    event: Value,
    transcript_path: PathBuf,
    include_subagents: bool,
}

impl TranscriptSource for CursorEventSource {
    fn provider(&self) -> &'static str {
        "cursor"
    }

    fn transcript_paths(&self, _project_root: &Path) -> Vec<PathBuf> {
        let mut paths = vec![self.transcript_path.clone()];
        if self.include_subagents {
            let parent_session_id = event_session_id(&self.event, &self.transcript_path);
            paths.extend(cursor_subagent_paths(
                &self.transcript_path,
                &parent_session_id,
            ));
        }
        paths
    }

    fn parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        _project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        let new = stream_new_jsonl(path, prev, max_new_bytes)?;
        let parent_session_id = event_session_id(&self.event, &self.transcript_path);
        let subagent = cursor_subagent_identity(path, &parent_session_id);
        let session_id = subagent.as_ref().map_or_else(
            || parent_session_id.clone(),
            |(session_id, _agent_id)| session_id.clone(),
        );
        let mut carry = TimestampCarry::new(i64::try_from(new.new_cursor.mtime).ok());
        let mut messages = Vec::new();
        for line in &new.lines {
            let derived_timestamp = carry.observe(&line.value);
            // The byte offset doubles as the message ordinal and source_offset,
            // matching the original Cursor ingestion.
            if let Some(message) = event_message(
                &line.value,
                &self.event,
                &session_id,
                path,
                line.offset,
                line.offset,
                derived_timestamp,
            ) {
                messages.push(message);
            }
            messages.extend(event_dispatch_messages(
                &line.value,
                &self.event,
                &session_id,
                path,
                line.offset,
                derived_timestamp,
            ));
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
                parent_session_id: None,
                is_subagent: false,
                agent_id: None,
                parent_tool_use_id: None,
            }
        } else {
            let (project_key, project_path) = event_project(&self.event);
            let (draft_parent_session_id, agent_id) = subagent
                .map_or((None, None), |(_session_id, agent_id)| {
                    (Some(parent_session_id), Some(agent_id))
                });
            let is_subagent = draft_parent_session_id.is_some();
            SessionDraft {
                session_id,
                project_key,
                project_path,
                title: title_from_messages(&messages),
                metadata_json: serde_json::to_string(&session_metadata(&self.event)).ok(),
                parent_session_id: draft_parent_session_id,
                is_subagent,
                agent_id,
                parent_tool_use_id: None,
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
/// bytes a single call will read. Cursor hooks pass byte caps to stay within hook
/// budgets; capped reads still discover subagent transcript files, with each file
/// independently subject to the same cap.
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
        include_subagents: true,
    };
    let stats = ingest_source(db, &source, &project_root, max_new_bytes).await;
    CursorTranscriptIngestStats {
        sessions_upserted: stats.sessions_upserted,
        messages_upserted: stats.messages_upserted,
    }
}

fn cursor_subagent_paths(transcript_path: &Path, parent_session_id: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(parent_dir) = transcript_path.parent() {
        if transcript_path.file_stem().and_then(|stem| stem.to_str()) == Some(parent_session_id) {
            candidates.push(parent_dir.join(parent_session_id).join("subagents"));
        }
        if parent_dir.file_name().and_then(|name| name.to_str()) == Some(parent_session_id) {
            candidates.push(parent_dir.join("subagents"));
        }
    }

    let mut paths = Vec::new();
    for dir in candidates {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn cursor_subagent_identity(path: &Path, parent_session_id: &str) -> Option<(String, String)> {
    let is_subagent_path = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        == Some("subagents");
    if !is_subagent_path {
        return None;
    }
    let parent_dir = path.parent()?.parent()?;
    if parent_dir.file_name().and_then(|name| name.to_str()) != Some(parent_session_id) {
        return None;
    }
    let session_id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|id| !id.is_empty())?
        .to_string();
    Some((session_id.clone(), session_id))
}

/// Per-line timestamp derivation for Cursor transcripts, which carry no
/// structured per-message timestamps. The injected `<timestamp>…</timestamp>`
/// tag in user prompts is parsed and carried forward across subsequent lines
/// (assistant turns happen after the prompt that started them); lines seen
/// before any tag fall back to the transcript file's mtime, which on the
/// incremental hook path approximates "now" for freshly appended lines.
pub(crate) struct TimestampCarry {
    carried: Option<i64>,
    fallback: Option<i64>,
}

impl TimestampCarry {
    pub(crate) fn new(fallback_mtime: Option<i64>) -> Self {
        Self {
            carried: None,
            fallback: fallback_mtime.filter(|mtime| *mtime > 0),
        }
    }

    /// Folds one transcript line into the carry and returns the timestamp to
    /// use for messages derived from that line.
    pub(crate) fn observe(&mut self, record: &Value) -> Option<i64> {
        if let Some(tag) = timestamp_tag_from_record(record) {
            self.carried = Some(tag);
        }
        self.carried.or(self.fallback)
    }
}

/// Extracts and parses the first `<timestamp>…</timestamp>` tag found in a
/// transcript line's text content.
fn timestamp_tag_from_record(record: &Value) -> Option<i64> {
    let message = record.get("message").unwrap_or(record);
    let content = message.get("content").unwrap_or(message);
    match content {
        Value::String(text) => timestamp_tag_from_text(text),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .find_map(timestamp_tag_from_text),
        _ => None,
    }
}

fn timestamp_tag_from_text(text: &str) -> Option<i64> {
    let start = text.find("<timestamp>")? + "<timestamp>".len();
    let end = start + text[start..].find("</timestamp>")?;
    crate::timeutil::parse_cursor_human_timestamp(text[start..end].trim())
}

fn event_message(
    record: &Value,
    event: &Value,
    session_id: &str,
    transcript_path: &Path,
    ordinal: i64,
    source_offset: i64,
    derived_timestamp: Option<i64>,
) -> Option<SessionMessageRecord> {
    let role = record
        .get("role")
        .and_then(Value::as_str)
        .filter(|role| !role.is_empty())?;
    let message = record.get("message").unwrap_or(record);
    let content = message.get("content").unwrap_or(message);
    if content_is_only_subagent_dispatch(content) {
        return None;
    }
    let (text, tool_names) = content_storage_text_and_tools(
        content,
        message
            .get("tool_calls")
            .or_else(|| record.get("tool_calls")),
    );
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
        timestamp: record_timestamp(record)
            .or_else(|| record_timestamp(event))
            .or(derived_timestamp),
        ordinal,
        text,
        kind: content_kind(content).map(str::to_string),
        model,
        tool_names: (!tool_names.is_empty()).then(|| tool_names.join(",")),
        source_path: Some(transcript_path.to_string_lossy().to_string()),
        source_offset: Some(source_offset),
        metadata_json: serde_json::to_string(&message_metadata(record, message)).ok(),
    })
}

fn event_dispatch_messages(
    record: &Value,
    event: &Value,
    session_id: &str,
    transcript_path: &Path,
    source_offset: i64,
    derived_timestamp: Option<i64>,
) -> Vec<SessionMessageRecord> {
    let Some(role) = record
        .get("role")
        .and_then(Value::as_str)
        .filter(|role| !role.is_empty())
    else {
        return Vec::new();
    };
    let message = record.get("message").unwrap_or(record);
    let content = message.get("content").unwrap_or(message);
    let Some(items) = content.as_array() else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let Some(name) = item.get("name").and_then(Value::as_str) else {
            continue;
        };
        if !is_subagent_dispatch_tool(name) {
            continue;
        }
        let Some(text) = dispatch_text(item) else {
            continue;
        };
        let tool_use_id = item
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty());
        let message_id = tool_use_id.map_or_else(
            || format!("{session_id}:tool_dispatch:{source_offset}:{index}"),
            |id| format!("{session_id}:tool_dispatch:{id}"),
        );
        out.push(SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id,
            session_id: session_id.to_string(),
            role: role.to_string(),
            timestamp: record_timestamp(record)
                .or_else(|| record_timestamp(event))
                .or(derived_timestamp),
            ordinal: source_offset.saturating_add(index as i64),
            text,
            kind: Some("tool_dispatch".to_string()),
            model: record
                .get("model")
                .or_else(|| message.get("model"))
                .or_else(|| event.get("model"))
                .and_then(Value::as_str)
                .map(str::to_string),
            tool_names: Some(name.to_string()),
            source_path: Some(transcript_path.to_string_lossy().to_string()),
            source_offset: Some(source_offset),
            metadata_json: serde_json::to_string(&serde_json::json!({
                "source": "cursor_transcript",
                "raw_type": record.get("type").cloned(),
                "tool_use_id": tool_use_id,
            }))
            .ok(),
        });
    }
    out
}

fn is_subagent_dispatch_tool(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), "task" | "subagent")
}

fn content_is_only_subagent_dispatch(content: &Value) -> bool {
    let Some(items) = content.as_array() else {
        return false;
    };
    !items.is_empty()
        && items.iter().all(|item| {
            item.get("type").and_then(Value::as_str) == Some("tool_use")
                && item
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(is_subagent_dispatch_tool)
        })
}

fn dispatch_text(item: &Value) -> Option<String> {
    let input = item.get("input").unwrap_or(item);
    let mut parts = Vec::new();
    for key in ["description", "prompt", "subagent_type"] {
        if let Some(value) = input
            .get(key)
            .or_else(|| item.get(key))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            parts.push(value.to_string());
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
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
    let cwd_root = event
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .and_then(|cwd| crate::config::discover_project_root(&cwd));
    let candidates = event_project_candidates(event);
    let resolved = candidates
        .iter()
        .find_map(|candidate| crate::config::discover_project_root(candidate))
        .or_else(|| candidates.into_iter().next());
    let project_path = match (cwd_root, resolved) {
        (Some(cwd_root), Some(resolved)) if !paths_equal(&cwd_root, &resolved) => cwd_root,
        (Some(cwd_root), None) => cwd_root,
        (_, Some(resolved)) => resolved,
        _ => return ("unknown".to_string(), "unknown".to_string()),
    };
    let project = project_path.to_string_lossy().to_string();
    (project.clone(), project)
}

fn event_project_candidates(event: &Value) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let mut push_unique = |candidate: PathBuf| {
        if !candidates.iter().any(|seen| seen == &candidate) {
            candidates.push(candidate);
        }
    };
    if let Some(cwd) = event
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
    {
        push_unique(PathBuf::from(cwd));
    }
    if let Some(file_path) = event
        .get("file_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
    {
        let path = Path::new(file_path);
        push_unique(path.parent().unwrap_or(path).to_path_buf());
    }
    if let Some(transcript_path) = event
        .get("transcript_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
    {
        let path = Path::new(transcript_path);
        push_unique(path.parent().unwrap_or(path).to_path_buf());
    }
    if let Some(roots) = event.get("workspace_roots").and_then(Value::as_array) {
        for root in roots {
            if let Some(path) = root.as_str().filter(|path| !path.is_empty()) {
                push_unique(PathBuf::from(path));
            }
        }
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

fn message_metadata(record: &Value, message: &Value) -> Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("cursor_transcript".to_string()),
    );
    metadata.insert(
        "raw_type".to_string(),
        record.get("type").cloned().unwrap_or(Value::Null),
    );
    append_tool_calls_metadata(&mut metadata, message);
    // Cursor transcripts carry no token counters today (verified across
    // 100k+ real lines); this probe is future-proofing for when they do.
    append_usage_metadata(&mut metadata, &[record, message]);
    Value::Object(metadata)
}
