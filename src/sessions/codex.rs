//! Codex CLI transcript source.
//!
//! Codex appends one JSON object per line to
//! `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` (sessions archived from the
//! picker move to a flat `~/.codex/archived_sessions/rollout-*.jsonl`). Each
//! line is `{"timestamp": "<iso8601>", "type": "<kind>", "payload": {…}}`. The
//! relevant kinds for conversation text are:
//!
//! * `session_meta` — first line; `payload.cwd`, session `id`. Real rollouts
//!   carry no `model` here (only `model_provider`); the active model is on
//!   `turn_context` lines and can change mid-session.
//! * `event_msg` with `payload.type == "user_message"` — a real user prompt
//!   (`payload.message`).
//! * `event_msg` with `payload.type == "agent_message"` — a real assistant reply
//!   (`payload.message`).
//! * `event_msg` with `payload.type == "token_count"` — per-API-call usage; a
//!   turn's tool loop emits one per call, so a turn's true cost is the *sum*
//!   (see [`CodexTurnUsage`]).
//! * `compacted` — Codex context-compression boundary. The rollout stores the
//!   replacement history and an encrypted compaction body, so `TraceDecay` records
//!   the boundary/provenance as a summary record without claiming plaintext
//!   access to Codex's private summary.
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
use crate::sessions::shared::{
    append_tool_calls_metadata, content_storage_text_and_tools, paths_equal, title_from_messages,
    StoredCursor,
};
use crate::sessions::source::{
    collect_files_with_ext, stream_new_jsonl, ParsedTranscript, SessionDraft, TranscriptSource,
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
    archived_sessions_dir: PathBuf,
}

impl CodexSource {
    /// Source rooted at the real `~/.codex`. Returns `None` when the
    /// home directory cannot be resolved.
    pub fn new() -> Option<Self> {
        let home = dirs::home_dir()?;
        Some(Self::with_home(&home))
    }

    /// Source rooted at `<home>/.codex` (used by tests).
    pub fn with_home(home: &Path) -> Self {
        let codex_home = home.join(".codex");
        Self {
            sessions_dir: codex_home.join("sessions"),
            archived_sessions_dir: codex_home.join("archived_sessions"),
        }
    }
}

impl TranscriptSource for CodexSource {
    fn provider(&self) -> &'static str {
        PROVIDER
    }

    fn transcript_paths(&self, _project_root: &Path) -> Vec<PathBuf> {
        // Archiving a session moves its rollout out of the dated tree; both
        // locations are real transcripts and must be ingested.
        let mut paths = collect_files_with_ext(&self.sessions_dir, "jsonl", MAX_SCAN_DEPTH);
        paths.extend(collect_files_with_ext(
            &self.archived_sessions_dir,
            "jsonl",
            MAX_SCAN_DEPTH,
        ));
        paths
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
        let mut turn_usage = CodexTurnUsage::default();
        // Real session_meta lines carry no model; track the active model from
        // `turn_context` lines instead (it can change mid-session).
        let mut current_model = meta.model.clone();
        let replayed_from_start =
            prev.position > 0 && new.lines.first().is_some_and(|line| line.offset == 0);
        let mut compaction_depth = if replayed_from_start {
            0
        } else {
            prior_compaction_depth(path, prev.position)
        };
        for line in &new.lines {
            if turn_usage.observe(&line.value) {
                continue;
            }
            if let Some(model) = turn_context_model(&line.value) {
                current_model = Some(model);
                continue;
            }
            if let Some(message) = compacted_summary_from_line(
                &line.value,
                &meta,
                current_model.as_deref(),
                path,
                line.offset,
                compaction_depth + 1,
            ) {
                flush_turn_usage(&mut messages, &mut turn_usage);
                compaction_depth += 1;
                messages.push(message);
                continue;
            }
            if let Some(message) = message_from_line(
                &line.value,
                &meta,
                current_model.as_deref(),
                path,
                line.offset,
            ) {
                // A new user prompt closes the previous turn: attach that
                // turn's summed API-call usage to its assistant reply.
                if message.role == "user" {
                    flush_turn_usage(&mut messages, &mut turn_usage);
                }
                messages.push(message);
            }
        }
        // The final turn's trailing token_count(s) arrive after its
        // agent_message; flush them onto it.
        flush_turn_usage(&mut messages, &mut turn_usage);

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
        // Note: real rollouts have no `model` in session_meta — only
        // `model_provider` (e.g. "openai"), which is *not* a model and must
        // not be stored as one; `turn_context` lines carry the actual model.
        let model = payload
            .get("model")
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

fn prior_compaction_depth(path: &Path, before_offset: u64) -> i64 {
    if before_offset == 0 {
        return 0;
    }
    use std::io::BufRead;
    let Ok(file) = std::fs::File::open(path) else {
        return 0;
    };
    let mut reader = std::io::BufReader::new(file);
    let mut line = String::new();
    let mut offset = 0_u64;
    let mut depth = 0_i64;
    loop {
        line.clear();
        let Ok(n) = reader.read_line(&mut line) else {
            break;
        };
        if n == 0 || offset >= before_offset {
            break;
        }
        let line_offset = offset;
        offset = offset.saturating_add(n as u64);
        if line_offset >= before_offset {
            break;
        }
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) == Some("compacted") {
            depth += 1;
        }
    }
    depth
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

/// Model recorded on a `turn_context` line, the only place rollouts store the
/// active model (`session_meta` only has `model_provider`).
fn turn_context_model(record: &Value) -> Option<String> {
    if record.get("type").and_then(Value::as_str) != Some("turn_context") {
        return None;
    }
    record
        .pointer("/payload/model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
}

/// Map one rollout line to a provider-neutral message, or `None` for non-message
/// events (`response_item`, tool calls, token counts, …).
fn message_from_line(
    record: &Value,
    meta: &CodexMeta,
    model: Option<&str>,
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
        model: model.map(str::to_string),
        tool_names: (!tool_names.is_empty()).then(|| tool_names.join(",")),
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&message_metadata(payload)).ok(),
    })
}

fn compacted_summary_from_line(
    record: &Value,
    meta: &CodexMeta,
    model: Option<&str>,
    path: &Path,
    offset: i64,
    depth: i64,
) -> Option<SessionMessageRecord> {
    if record.get("type").and_then(Value::as_str) != Some("compacted") {
        return None;
    }
    let payload = record.get("payload")?;
    let replacement_history_count = payload
        .get("replacement_history")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let compaction = payload
        .get("replacement_history")
        .and_then(Value::as_array)
        .and_then(|history| {
            history
                .iter()
                .rev()
                .find(|entry| entry.get("type").and_then(Value::as_str) == Some("compaction"))
        });
    let plaintext = payload
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty());
    let encrypted = compaction
        .and_then(|entry| entry.get("encrypted_content"))
        .and_then(Value::as_str)
        .is_some_and(|content| !content.is_empty());
    let summary_body = if plaintext.is_some() {
        "plaintext"
    } else if encrypted {
        "encrypted"
    } else {
        "unavailable"
    };
    let timestamp_text = record
        .get("timestamp")
        .and_then(Value::as_str)
        .unwrap_or("unknown time");
    let text = plaintext.map_or_else(
        || {
            format!(
                "Codex context compaction at {timestamp_text}. Summary body is {summary_body} in the rollout; replacement history entries: {replacement_history_count}."
            )
        },
        str::to_string,
    );

    let timestamp = record
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_timestamp)
        .map(|secs| secs as i64);

    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("codex_context_compacted".to_string()),
    );
    metadata.insert(
        "source_event".to_string(),
        Value::String("compacted".to_string()),
    );
    metadata.insert(
        "summary_body".to_string(),
        Value::String(summary_body.to_string()),
    );
    metadata.insert(
        "replacement_history_count".to_string(),
        Value::from(replacement_history_count as i64),
    );
    metadata.insert(
        "codex_compaction_depth".to_string(),
        Value::from(depth.max(1)),
    );
    metadata.insert("source_offset".to_string(), Value::from(offset));
    metadata.insert("encrypted".to_string(), Value::from(encrypted));

    Some(SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id: format!("{}:{offset}", meta.session_id),
        session_id: meta.session_id.clone(),
        role: "assistant".to_string(),
        timestamp,
        ordinal: offset,
        text,
        kind: Some("summary".to_string()),
        model: model.map(str::to_string),
        tool_names: None,
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&Value::Object(metadata)).ok(),
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

/// Accumulates per-API-call `token_count` usage across one turn's tool loop.
///
/// Codex emits one `token_count` event per API call: the tool-loop calls
/// report *during* the turn (before the final `agent_message`) and the final
/// call reports right after it. Real rollouts on this machine showed ~64% of
/// input spend in those mid-turn reports, so honest cost accounting must sum
/// every call rather than keep only the one following the assistant reply.
/// Consecutive events whose cumulative `total_token_usage.total_tokens` did
/// not advance are duplicate reports of the same call and are skipped.
///
/// Counters are normalized for the savings dashboard's additive pricing
/// (Anthropic semantics): `OpenAI` `input_tokens` *includes*
/// `cached_input_tokens`, so the cached portion is split out into
/// `cache_read_input_tokens` and `input_tokens` keeps only the uncached
/// remainder.
#[derive(Default)]
pub(crate) struct CodexTurnUsage {
    input: i64,
    output: i64,
    cache_read: i64,
    reasoning: i64,
    total: i64,
    seen: bool,
    last_cumulative: Option<i64>,
}

impl CodexTurnUsage {
    /// Consume a rollout line when it is a `token_count` event, adding its
    /// per-call counters to the running turn sums. Returns `true` for every
    /// `token_count` line (even malformed or duplicate ones, which add
    /// nothing) and `false` for any other line kind.
    pub(crate) fn observe(&mut self, record: &Value) -> bool {
        if record.get("type").and_then(Value::as_str) != Some("event_msg") {
            return false;
        }
        let Some(payload) = record.get("payload") else {
            return false;
        };
        if payload.get("type").and_then(Value::as_str) != Some("token_count") {
            return false;
        }
        let Some(info) = payload.get("info") else {
            return true;
        };
        let cumulative = info
            .pointer("/total_token_usage/total_tokens")
            .and_then(Value::as_i64);
        if cumulative.is_some() && cumulative == self.last_cumulative {
            return true;
        }
        if cumulative.is_some() {
            self.last_cumulative = cumulative;
        }
        let Some(last) = info
            .get("last_token_usage")
            .or_else(|| info.get("total_token_usage"))
        else {
            return true;
        };
        let input = last
            .get("input_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let output = last
            .get("output_tokens")
            .or_else(|| last.get("completion_tokens"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let cached = last
            .get("cached_input_tokens")
            .or_else(|| last.get("cache_read_input_tokens"))
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .max(0);
        let reasoning = last
            .get("reasoning_output_tokens")
            .or_else(|| last.get("reasoning_tokens"))
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .max(0);
        let total = last
            .get("total_tokens")
            .and_then(Value::as_i64)
            .or(cumulative)
            .unwrap_or_else(|| input.saturating_add(output).saturating_add(reasoning));
        if input == 0 && output == 0 && cached == 0 && reasoning == 0 && total == 0 {
            return true;
        }
        self.input = self
            .input
            .saturating_add((input.saturating_sub(cached)).max(0));
        self.cache_read = self.cache_read.saturating_add(cached);
        self.reasoning = self.reasoning.saturating_add(reasoning);
        self.output = self
            .output
            .saturating_add(output.max(0).saturating_add(reasoning));
        self.total = self.total.saturating_add(total.max(0));
        self.seen = true;
        true
    }

    /// The summed counters as a dashboard-shaped usage object, resetting the
    /// turn sums (the cumulative-total dedup guard survives across turns).
    pub(crate) fn take(&mut self) -> Option<Value> {
        if !self.seen {
            return None;
        }
        let mut usage = serde_json::Map::new();
        usage.insert("input_tokens".to_string(), Value::from(self.input));
        usage.insert("output_tokens".to_string(), Value::from(self.output));
        if self.cache_read > 0 {
            usage.insert(
                "cache_read_input_tokens".to_string(),
                Value::from(self.cache_read),
            );
        }
        if self.reasoning > 0 {
            usage.insert("reasoning_tokens".to_string(), Value::from(self.reasoning));
        }
        if self.total > 0 {
            usage.insert("total_tokens".to_string(), Value::from(self.total));
        }
        self.input = 0;
        self.output = 0;
        self.cache_read = 0;
        self.reasoning = 0;
        self.total = 0;
        self.seen = false;
        Some(Value::Object(usage))
    }
}

/// Add `add`'s numeric counters field-wise into `existing` (both are usage
/// objects). Used when several flushes land on the same assistant message
/// (e.g. an aborted turn with no reply of its own).
pub(crate) fn merge_usage_counters(existing: &mut Value, add: &Value) {
    let (Some(map), Some(add_map)) = (existing.as_object_mut(), add.as_object()) else {
        return;
    };
    for (key, value) in add_map {
        if let Some(count) = value.as_i64() {
            let current = map.get(key).and_then(Value::as_i64).unwrap_or(0);
            map.insert(key.clone(), Value::from(current.saturating_add(count)));
        }
    }
}

/// Attach the finished turn's summed usage to the most recent assistant
/// message of the batch (the reply the turn's `token_count` events report
/// on), merging additively when that message already carries usage.
fn flush_turn_usage(messages: &mut [SessionMessageRecord], turn_usage: &mut CodexTurnUsage) {
    let Some(usage) = turn_usage.take() else {
        return;
    };
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
    match metadata.get_mut("usage") {
        Some(existing) => merge_usage_counters(existing, &usage),
        None => {
            metadata.insert("usage".to_string(), usage);
        }
    }
    if let Ok(serialized) = serde_json::to_string(&Value::Object(metadata)) {
        message.metadata_json = Some(serialized);
    }
}
