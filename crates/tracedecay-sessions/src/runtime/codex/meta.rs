//! Codex rollout `session_meta` / `turn_context` record models and readers.

use std::io::BufReader;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::{Mutex, OnceLock};

use serde_json::Value;

use crate::runtime::source::{MAX_JSONL_RECORD_BYTES, RawJsonlFrame, RawJsonlFrameReader};

/// Session metadata read from a rollout's leading `session_meta` line.
#[derive(Clone)]
pub struct CodexMeta {
    pub cwd: PathBuf,
    pub session_id: String,
    pub model: Option<String>,
    pub git: Option<Value>,
    pub parent_session_id: Option<String>,
    pub is_subagent: bool,
    pub agent_id: Option<String>,
    pub agent_nickname: Option<String>,
    pub agent_role: Option<String>,
    pub thread_source: Option<String>,
}

#[derive(Clone)]
pub(super) struct CodexMetaWithProvenance {
    pub meta: CodexMeta,
    pub native_thread_id: Option<String>,
    source_frame_bytes: usize,
}

impl CodexMetaWithProvenance {
    pub(super) fn retained_bytes(&self) -> u64 {
        // The parsed native JSON tree can retain substantially more allocator
        // memory than its encoding. Keep the same conservative 32x structural
        // factor as prepared JSONL pages, plus the owned model/path/id fields.
        let structural = self.source_frame_bytes.saturating_mul(32);
        let owned = crate::runtime::source::path_byte_len(&self.meta.cwd)
            .saturating_add(self.meta.session_id.capacity())
            .saturating_add(self.meta.model.as_ref().map_or(0, String::capacity))
            .saturating_add(
                self.meta
                    .parent_session_id
                    .as_ref()
                    .map_or(0, String::capacity),
            )
            .saturating_add(self.meta.agent_id.as_ref().map_or(0, String::capacity))
            .saturating_add(
                self.meta
                    .agent_nickname
                    .as_ref()
                    .map_or(0, String::capacity),
            )
            .saturating_add(self.meta.agent_role.as_ref().map_or(0, String::capacity))
            .saturating_add(self.meta.thread_source.as_ref().map_or(0, String::capacity))
            .saturating_add(self.native_thread_id.as_ref().map_or(0, String::capacity));
        u64::try_from(
            std::mem::size_of::<Self>()
                .saturating_add(structural)
                .saturating_add(owned),
        )
        .unwrap_or(u64::MAX)
    }
}

pub struct CodexTurnContext {
    pub turn_id: Option<String>,
    pub model: Option<String>,
    pub cwd: Option<PathBuf>,
}

/// Read the leading `session_meta` line of a rollout for cwd/session-id/model.
pub(super) fn session_meta(path: &Path) -> Option<CodexMeta> {
    session_meta_with_provenance(path).map(|parsed| parsed.meta)
}

pub(super) fn session_meta_with_provenance(path: &Path) -> Option<CodexMetaWithProvenance> {
    #[cfg(test)]
    {
        let reads = SESSION_META_READS.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
        let mut reads = reads.lock().unwrap_or_else(|error| error.into_inner());
        let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        *reads.entry(key).or_default() += 1;
    }
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
                    return Some(CodexMetaWithProvenance {
                        source_frame_bytes: frames.record().len(),
                        ..meta
                    });
                }
            }
            RawJsonlFrame::Oversized { .. } | RawJsonlFrame::BudgetExhausted { .. } => {}
        }
    }
    None
}

#[cfg(test)]
static SESSION_META_READS: OnceLock<Mutex<std::collections::HashMap<PathBuf, usize>>> =
    OnceLock::new();

#[cfg(test)]
pub(crate) fn session_meta_read_count_for_test(path: &Path) -> usize {
    let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    SESSION_META_READS
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&key)
        .copied()
        .unwrap_or_default()
}

pub fn session_meta_from_record(record: &Value, path: &Path) -> Option<CodexMeta> {
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
        source_frame_bytes: 0,
    })
}

pub(super) fn string_field(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub(super) fn nested_string_field(payload: &Value, pointer: &str) -> Option<String> {
    payload
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Context recorded on a `turn_context` line. Real rollouts use this for the
/// active model and current cwd; both can change mid-session.
pub fn turn_context_from_record(record: &Value) -> Option<CodexTurnContext> {
    if record.get("type").and_then(Value::as_str) != Some("turn_context") {
        return None;
    }
    let payload = record.get("payload").unwrap_or(record);
    let turn_id = payload
        .get("turn_id")
        .and_then(Value::as_str)
        .filter(|turn_id| !turn_id.is_empty())
        .map(str::to_string);
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
    Some(CodexTurnContext {
        turn_id,
        model,
        cwd,
    })
}
