//! Codex rollout `session_meta` / `turn_context` record models and readers.

use std::io::BufReader;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::runtime::source::{MAX_JSONL_RECORD_BYTES, RawJsonlFrame, RawJsonlFrameReader};

/// Session metadata read from a rollout's leading `session_meta` line.
pub(crate) struct CodexMeta {
    pub(crate) cwd: PathBuf,
    pub(crate) session_id: String,
    pub(crate) model: Option<String>,
    pub(crate) git: Option<Value>,
    pub(crate) parent_session_id: Option<String>,
    pub(crate) is_subagent: bool,
    pub(crate) agent_id: Option<String>,
    pub(crate) agent_nickname: Option<String>,
    pub(crate) agent_role: Option<String>,
    pub(crate) thread_source: Option<String>,
}

pub(crate) struct CodexMetaWithProvenance {
    pub(crate) meta: CodexMeta,
    pub(crate) native_thread_id: Option<String>,
}

pub(crate) struct CodexTurnContext {
    pub(crate) model: Option<String>,
    pub(crate) cwd: Option<PathBuf>,
}

/// Read the leading `session_meta` line of a rollout for cwd/session-id/model.
pub(crate) fn session_meta(path: &Path) -> Option<CodexMeta> {
    session_meta_with_provenance(path).map(|parsed| parsed.meta)
}

pub(crate) fn session_meta_with_provenance(path: &Path) -> Option<CodexMetaWithProvenance> {
    let file = std::fs::File::open(path).ok()?;
    let mut frames = RawJsonlFrameReader::new(BufReader::new(file), MAX_JSONL_RECORD_BYTES);
    for _ in 0..4 {
        match frames.next_frame().ok()? {
            RawJsonlFrame::Eof => break,
            RawJsonlFrame::Complete { .. } | RawJsonlFrame::Partial { .. } => {
                let Ok(value) = serde_json::from_slice::<Value>(frames.record()) else {
                    continue;
                };
                if let Some(meta) = session_meta_with_provenance_from_record(&value, path) {
                    return Some(meta);
                }
            }
            RawJsonlFrame::Oversized { .. } | RawJsonlFrame::BudgetExhausted { .. } => {}
        }
    }
    None
}

pub(crate) fn session_meta_from_record(record: &Value, path: &Path) -> Option<CodexMeta> {
    session_meta_with_provenance_from_record(record, path).map(|parsed| parsed.meta)
}

fn session_meta_with_provenance_from_record(
    record: &Value,
    path: &Path,
) -> Option<CodexMetaWithProvenance> {
    if record.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    let payload = record.get("payload").unwrap_or(record);
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from)?;
    let native_thread_id = payload
        .get("id")
        .or_else(|| payload.get("session_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    let session_id = native_thread_id.clone().unwrap_or_else(|| {
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("unknown")
            .to_string()
    });
    // Note: real rollouts have no `model` in session_meta — only
    // `model_provider` (e.g. "openai"), which is *not* a model and must
    // not be stored as one; `turn_context` lines carry the actual model.
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string);
    let git = payload.get("git").filter(|git| git.is_object()).cloned();
    let parent_session_id = string_field(payload, "forked_from_id")
        .or_else(|| nested_string_field(payload, "/source/subagent/thread_spawn/parent_thread_id"));
    let thread_source = string_field(payload, "thread_source");
    let agent_nickname = string_field(payload, "agent_nickname")
        .or_else(|| nested_string_field(payload, "/source/subagent/thread_spawn/agent_nickname"));
    let agent_role = string_field(payload, "agent_role")
        .or_else(|| nested_string_field(payload, "/source/subagent/thread_spawn/agent_role"));
    let is_subagent = thread_source.as_deref() == Some("subagent")
        || parent_session_id.is_some()
        || payload.pointer("/source/subagent").is_some();
    let agent_id = is_subagent.then(|| {
        agent_nickname
            .clone()
            .or_else(|| agent_role.clone())
            .unwrap_or_else(|| session_id.clone())
    });
    Some(CodexMetaWithProvenance {
        meta: CodexMeta {
            cwd,
            session_id,
            model,
            git,
            parent_session_id,
            is_subagent,
            agent_id,
            agent_nickname,
            agent_role,
            thread_source,
        },
        native_thread_id,
    })
}

pub(crate) fn string_field(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub(crate) fn nested_string_field(payload: &Value, pointer: &str) -> Option<String> {
    payload
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Context recorded on a `turn_context` line. Real rollouts use this for the
/// active model and current cwd; both can change mid-session.
pub(crate) fn turn_context_from_record(record: &Value) -> Option<CodexTurnContext> {
    if record.get("type").and_then(Value::as_str) != Some("turn_context") {
        return None;
    }
    let payload = record.get("payload").unwrap_or(record);
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
        .map(str::to_string);
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from);
    Some(CodexTurnContext { model, cwd })
}
