//! Codex CLI transcript source.
//!
//! Codex appends one JSON object per line to
//! `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`. Each line is
//! `{"timestamp": "<iso8601>", "type": "<kind>", "payload": {…}}`. The relevant
//! kinds for conversation text are:
//!
//! * `session_meta` — first line; `payload.cwd`, session `id`, model info.
//! * `event_msg` with `payload.type == "user_message"` — a real user prompt
//!   (`payload.message`).
//! * `event_msg` with `payload.type == "agent_message"` — a real assistant reply
//!   (`payload.message`).
//!
//! `response_item` entries are intentionally skipped: they carry auto-injected
//! synthetic context and duplicate the `agent_message`/`user_message` turns, so
//! ingesting them would double-count the conversation. This append-only JSONL is
//! read with the shared byte-offset machinery and scoped to the current project
//! by `session_meta.cwd`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::accounting::parser::parse_timestamp;
use crate::sessions::source::{
    collect_files_with_ext, paths_equal, stream_new_jsonl, title_from_messages, ParsedTranscript,
    SessionDraft, StoredCursor, TranscriptSource,
};
use crate::sessions::SessionMessageRecord;

const PROVIDER: &str = "codex";
/// `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` → date dirs add depth.
const MAX_SCAN_DEPTH: u8 = 6;

/// Session metadata read from a rollout's leading `session_meta` line.
struct CodexMeta {
    cwd: PathBuf,
    session_id: String,
    model: Option<String>,
}

/// Codex CLI transcript locator + parser.
pub struct CodexSource {
    sessions_dir: PathBuf,
}

impl CodexSource {
    /// Source rooted at the real `~/.codex/sessions`. Returns `None` when the
    /// home directory cannot be resolved.
    pub fn new() -> Option<Self> {
        let home = dirs::home_dir()?;
        Some(Self::with_home(&home))
    }

    /// Source rooted at `<home>/.codex/sessions` (used by tests).
    pub fn with_home(home: &Path) -> Self {
        Self {
            sessions_dir: home.join(".codex").join("sessions"),
        }
    }
}

impl TranscriptSource for CodexSource {
    fn provider(&self) -> &'static str {
        PROVIDER
    }

    fn transcript_paths(&self, _project_root: &Path) -> Vec<PathBuf> {
        collect_files_with_ext(&self.sessions_dir, "jsonl", MAX_SCAN_DEPTH)
    }

    fn parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        // `session_meta` (line 1) is authoritative for cwd + session id; without
        // it we cannot safely attribute the rollout to a project, so skip.
        let meta = session_meta(path)?;
        if !paths_equal(&meta.cwd, project_root) {
            return None;
        }

        let new = stream_new_jsonl(path, prev, max_new_bytes)?;
        let mut messages = Vec::new();
        for line in &new.lines {
            if let Some(message) = message_from_line(&line.value, &meta, path, line.offset) {
                messages.push(message);
            }
        }

        let project = project_root.to_string_lossy().to_string();
        let draft = SessionDraft {
            session_id: meta.session_id.clone(),
            project_key: project.clone(),
            project_path: project,
            title: title_from_messages(&messages),
            metadata_json: serde_json::to_string(&serde_json::json!({
                "source": "codex_rollout",
            }))
            .ok(),
        };

        Some(ParsedTranscript {
            draft,
            messages,
            new_cursor: new.new_cursor,
        })
    }
}

/// Read the leading `session_meta` line of a rollout for cwd/session-id/model.
fn session_meta(path: &Path) -> Option<CodexMeta> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    for line in reader.lines().take(4).map_while(Result::ok) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        let payload = value.get("payload").unwrap_or(&value);
        let cwd = payload
            .get("cwd")
            .and_then(Value::as_str)
            .filter(|cwd| !cwd.is_empty())
            .map(PathBuf::from)?;
        let session_id = payload
            .get("id")
            .or_else(|| payload.get("session_id"))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map_or_else(
                || {
                    path.file_stem()
                        .and_then(|stem| stem.to_str())
                        .unwrap_or("unknown")
                        .to_string()
                },
                ToString::to_string,
            );
        let model = payload
            .get("model")
            .or_else(|| payload.get("model_provider"))
            .and_then(Value::as_str)
            .map(str::to_string);
        return Some(CodexMeta {
            cwd,
            session_id,
            model,
        });
    }
    None
}

/// Map one rollout line to a provider-neutral message, or `None` for non-message
/// events (`response_item`, tool calls, token counts, …).
fn message_from_line(
    record: &Value,
    meta: &CodexMeta,
    path: &Path,
    offset: i64,
) -> Option<SessionMessageRecord> {
    if record.get("type").and_then(Value::as_str) != Some("event_msg") {
        return None;
    }
    let payload = record.get("payload")?;
    let role = match payload.get("type").and_then(Value::as_str)? {
        "user_message" => "user",
        "agent_message" => "assistant",
        _ => return None,
    };
    let text = payload.get("message").and_then(Value::as_str).unwrap_or("");
    if text.trim().is_empty() {
        return None;
    }

    let timestamp = record
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_timestamp)
        .map(|secs| secs as i64);

    Some(SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id: format!("{}:{offset}", meta.session_id),
        session_id: meta.session_id.clone(),
        role: role.to_string(),
        timestamp,
        ordinal: offset,
        text: text.to_string(),
        kind: Some("message".to_string()),
        model: meta.model.clone(),
        tool_names: None,
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&serde_json::json!({
            "source": "codex_rollout",
        }))
        .ok(),
    })
}
