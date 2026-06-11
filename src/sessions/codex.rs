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
//! * subagent rollouts — separate `rollout-*.jsonl` files whose leading
//!   `session_meta` has `thread_source == "subagent"` and parent ids in
//!   `forked_from_id` / `source.subagent.thread_spawn.parent_thread_id`.
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
    append_tool_calls_metadata, collect_files_with_ext, content_storage_text_and_tools,
    paths_equal, stream_new_jsonl, title_from_messages, ParsedTranscript, SessionDraft,
    StoredCursor, TranscriptSource,
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
    parent_session_id: Option<String>,
    is_subagent: bool,
    agent_id: Option<String>,
    agent_nickname: Option<String>,
    agent_role: Option<String>,
    thread_source: Option<String>,
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
            } else if let Some(usage) = token_count_usage(&line.value) {
                // A `token_count` event reports the turn that just finished;
                // attach its per-turn counters to the assistant reply it
                // follows so the savings dashboard can cost it as `actual`.
                attach_usage_to_last_assistant(&mut messages, &usage);
            }
        }

        let project = project_root.to_string_lossy().to_string();
        let draft = SessionDraft {
            session_id: meta.session_id.clone(),
            project_key: project.clone(),
            project_path: project,
            title: title_from_messages(&messages),
            metadata_json: codex_metadata_json(&meta),
            parent_session_id: meta.parent_session_id.clone(),
            is_subagent: meta.is_subagent,
            agent_id: meta.agent_id.clone(),
            parent_tool_use_id: None,
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
        let parent_session_id = string_field(payload, "forked_from_id").or_else(|| {
            nested_string_field(payload, "/source/subagent/thread_spawn/parent_thread_id")
        });
        let thread_source = string_field(payload, "thread_source");
        let agent_nickname = string_field(payload, "agent_nickname").or_else(|| {
            nested_string_field(payload, "/source/subagent/thread_spawn/agent_nickname")
        });
        let agent_role = string_field(payload, "agent_role")
            .or_else(|| nested_string_field(payload, "/source/subagent/thread_spawn/agent_role"));
        let is_subagent = thread_source.as_deref() == Some("subagent")
            || parent_session_id.is_some()
            || payload.pointer("/source/subagent").is_some();
        let agent_id = is_subagent.then(|| session_id.clone());
        return Some(CodexMeta {
            cwd,
            session_id,
            model,
            parent_session_id,
            is_subagent,
            agent_id,
            agent_nickname,
            agent_role,
            thread_source,
        });
    }
    None
}

fn string_field(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn nested_string_field(payload: &Value, pointer: &str) -> Option<String> {
    payload
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn codex_metadata_json(meta: &CodexMeta) -> Option<String> {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("codex_rollout".to_string()),
    );
    if let Some(thread_source) = &meta.thread_source {
        metadata.insert(
            "thread_source".to_string(),
            Value::String(thread_source.clone()),
        );
    }
    if let Some(agent_role) = &meta.agent_role {
        metadata.insert("agent_role".to_string(), Value::String(agent_role.clone()));
    }
    if let Some(agent_nickname) = &meta.agent_nickname {
        metadata.insert(
            "agent_nickname".to_string(),
            Value::String(agent_nickname.clone()),
        );
    }
    serde_json::to_string(&Value::Object(metadata)).ok()
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
    let content = payload.get("message")?;
    let (text, tool_names) = content_storage_text_and_tools(content, payload.get("tool_calls"));
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
        text,
        kind: Some("message".to_string()),
        model: meta.model.clone(),
        tool_names: (!tool_names.is_empty()).then(|| tool_names.join(",")),
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&message_metadata(payload)).ok(),
    })
}

fn message_metadata(payload: &Value) -> Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("codex_rollout".to_string()),
    );
    append_tool_calls_metadata(&mut metadata, payload);
    Value::Object(metadata)
}

/// Per-turn token counters from an `event_msg`/`token_count` rollout line,
/// normalized from `payload.info.last_token_usage`.
///
/// Codex reports `OpenAI` semantics where `input_tokens` *includes*
/// `cached_input_tokens`; the savings dashboard prices counters additively
/// (Anthropic semantics: input + `cache_read` + `cache_write`), so the cached
/// portion is split out into `cache_read_input_tokens` and `input_tokens`
/// keeps only the uncached remainder.
pub(crate) fn token_count_usage(record: &Value) -> Option<Value> {
    if record.get("type").and_then(Value::as_str) != Some("event_msg") {
        return None;
    }
    let payload = record.get("payload")?;
    if payload.get("type").and_then(Value::as_str) != Some("token_count") {
        return None;
    }
    let last = payload.pointer("/info/last_token_usage")?;
    let input = last.get("input_tokens").and_then(Value::as_i64)?;
    let cached = last
        .get("cached_input_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let output = last.get("output_tokens").and_then(Value::as_i64)?;

    let mut usage = serde_json::Map::new();
    usage.insert(
        "input_tokens".to_string(),
        Value::from((input - cached).max(0)),
    );
    usage.insert("output_tokens".to_string(), Value::from(output));
    if cached > 0 {
        usage.insert("cache_read_input_tokens".to_string(), Value::from(cached));
    }
    if let Some(total) = last.get("total_tokens").and_then(Value::as_i64) {
        usage.insert("total_tokens".to_string(), Value::from(total));
    }
    Some(Value::Object(usage))
}

/// Merges turn usage into the most recent assistant message of the batch
/// (the reply the `token_count` event reports on). First usage wins; later
/// duplicate `token_count` events for the same turn are ignored.
fn attach_usage_to_last_assistant(messages: &mut [SessionMessageRecord], usage: &Value) {
    let Some(message) = messages
        .iter_mut()
        .rev()
        .find(|message| message.role == "assistant")
    else {
        return;
    };
    let mut metadata = message
        .metadata_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    if metadata.contains_key("usage") {
        return;
    }
    metadata.insert("usage".to_string(), usage.clone());
    if let Ok(serialized) = serde_json::to_string(&Value::Object(metadata)) {
        message.metadata_json = Some(serialized);
    }
}
