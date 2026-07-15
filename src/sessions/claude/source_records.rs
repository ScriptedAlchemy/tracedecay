use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::accounting::parser::parse_timestamp;
use crate::privacy::{PR5_MAX_CLAUDE_RECORD_BYTES, parse_claude_record_v1};
use crate::sessions::SessionMessageRecord;
use crate::sessions::shared::{content_storage_text_and_tools, preview_truncated};
use crate::sessions::source::{RawJsonlFrame, RawJsonlFrameReader, SessionDraft};

use super::record_metadata::{
    SessionAccumulator, compact_boundary_row, message_metadata, model_fallback_row, pr_link_row,
    record_timestamp,
};
use super::{CWD_PROBE_LINES, KIND_REASONING, MARKER_PREVIEW_BYTES, PROVIDER};

/// Durable context shared by V1 folding and the V2 observation projector.
pub(crate) struct ClaudeRecordContext<'a> {
    pub session_id: &'a str,
    pub project_key: &'a str,
    pub project_path: &'a str,
    pub file_generation: u64,
    pub offset: u64,
    pub session_cwd: Option<&'a Path>,
}

/// Minimal PR5 projection result. Rich reasoning/marker families remain V1
/// enrichments until their explicit PR6 projection contract.
pub(crate) enum ClaudeRecordDisposition {
    Message {
        draft: Box<SessionDraft>,
        message: Box<SessionMessageRecord>,
    },
    NonConversational {
        record_type: Option<String>,
    },
}

/// Map one sanitized Claude record to the canonical conversational V1 row.
/// Pure: no I/O, cursor access, global state, or persistence.
pub(crate) fn map_sanitized_claude_record(
    record: &Value,
    context: &ClaudeRecordContext<'_>,
) -> ClaudeRecordDisposition {
    let Ok(offset) = i64::try_from(context.offset) else {
        return ClaudeRecordDisposition::NonConversational {
            record_type: record
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_owned),
        };
    };
    let mut accumulator = SessionAccumulator::default();
    let source_id = format!("claude:{}", context.session_id);
    let Some(mut message) = message_from_line(
        record,
        context.session_id,
        Path::new(&source_id),
        offset,
        context.session_cwd,
        &mut accumulator,
    ) else {
        return ClaudeRecordDisposition::NonConversational {
            record_type: record
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_owned),
        };
    };
    let mut metadata = message
        .metadata_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<Map<String, Value>>(json).ok())
        .unwrap_or_default();
    metadata.insert(
        "source_generation".to_string(),
        Value::from(context.file_generation),
    );
    message.metadata_json = serde_json::to_string(&metadata).ok();
    let draft = SessionDraft {
        session_id: context.session_id.to_owned(),
        project_key: context.project_key.to_owned(),
        project_path: context.project_path.to_owned(),
        title: None,
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    };
    ClaudeRecordDisposition::Message {
        draft: Box::new(draft),
        message: Box::new(message),
    }
}

pub(crate) fn transcript_cwd(path: &Path) -> Option<PathBuf> {
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    let mut frames = RawJsonlFrameReader::new(reader, PR5_MAX_CLAUDE_RECORD_BYTES);
    let mut offset = 0_u64;
    for _ in 0..CWD_PROBE_LINES {
        let byte_len = match frames.next_frame().ok()? {
            RawJsonlFrame::Eof
            | RawJsonlFrame::Partial { .. }
            | RawJsonlFrame::Oversized { .. } => return None,
            RawJsonlFrame::Complete { byte_len } => byte_len,
        };
        let end_offset = offset.checked_add(byte_len)?;
        let record = frames.record();
        if record.iter().all(u8::is_ascii_whitespace) {
            offset = end_offset;
            continue;
        }
        let range = tracedecay_domain::ClaudeByteRangeV1::new(offset, end_offset).ok()?;
        if let Ok(parsed) = parse_claude_record_v1(record, range)
            && let Some(cwd) = parsed.value().get("cwd").and_then(Value::as_str)
            && !cwd.is_empty()
        {
            return Some(PathBuf::from(cwd));
        }
        offset = end_offset;
    }
    None
}

/// Map one Claude transcript line to a provider-neutral message, or `None` for
/// lines that carry no conversational text (tool-result-only, meta lines, …).
///
/// Gate: only `user`/`assistant` records become conversational rows here. Other
/// record types fall through to [`system_hook_message_from_line`] and
/// [`structured_marker_from_line`]. Two record families are deliberately dropped
/// with no row at all, because they are pure bloat/redundancy:
///
/// * **hook attachments** — records that inject a hook's `hookAdditionalContext`
///   / attachment payload into the transcript. The signal we care about (hook
///   errors / prevented continuation) is already captured as a compact
///   `hook_event` row; the attachment body just duplicates content that lives on
///   the owning turn.
/// * **queue-operation records** — queued/removed user-turn bookkeeping. These
///   are ephemeral UI state; the actual user turn is ingested when it is sent.
pub(super) fn message_from_line(
    record: &Value,
    session_id: &str,
    path: &Path,
    offset: i64,
    session_cwd: Option<&Path>,
    accumulator: &mut SessionAccumulator,
) -> Option<SessionMessageRecord> {
    let kind = record.get("type").and_then(Value::as_str)?;
    if kind != "user" && kind != "assistant" {
        return None;
    }
    let message = record.get("message").unwrap_or(record);
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or(kind)
        .to_string();

    let content = message.get("content").unwrap_or(message);
    let indexed_content = if role == "assistant" {
        content.as_array().map(|blocks| {
            Value::Array(
                blocks
                    .iter()
                    .filter(|block| {
                        !matches!(
                            block.get("type").and_then(Value::as_str),
                            Some("thinking" | "redacted_thinking")
                        )
                    })
                    .cloned()
                    .collect(),
            )
        })
    } else {
        None
    };
    let content_for_index = indexed_content.as_ref().unwrap_or(content);
    let (text, tool_names) = content_storage_text_and_tools(
        content_for_index,
        message
            .get("tool_calls")
            .or_else(|| record.get("tool_calls")),
    );
    if text.trim().is_empty() {
        return None;
    }

    let message_id = conversational_message_id(message, record, session_id, offset);
    let model = message
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string);
    let timestamp = record
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_timestamp)
        .map(|secs| secs as i64);

    Some(SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id,
        session_id: session_id.to_string(),
        role,
        timestamp,
        ordinal: offset,
        text,
        kind: Some("message".to_string()),
        model,
        tool_names: (!tool_names.is_empty()).then(|| tool_names.join(",")),
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&message_metadata(
            kind,
            record,
            message,
            content,
            session_cwd,
            accumulator,
        ))
        .ok(),
    })
}

/// Stable id for a conversational (`user`/`assistant`) row: the message `id`,
/// else the record `uuid`, else a synthesized `{session}:{offset}`. Shared by
/// the message row and the reasoning row so a reasoning row's
/// `{base}:thinking` id always links back to its owning assistant message.
fn conversational_message_id(
    message: &Value,
    record: &Value,
    session_id: &str,
    offset: i64,
) -> String {
    message
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| record.get("uuid").and_then(Value::as_str))
        .filter(|id| !id.is_empty())
        .map_or_else(|| format!("{session_id}:{offset}"), ToString::to_string)
}

/// Emit a separate `kind="reasoning"` row for an assistant message that carries
/// one or more `thinking` blocks, so the model's reasoning is kind-filterable
/// and searchable on its own row — matching how Codex
/// ([`crate::sessions::codex`]) and Cursor ([`crate::sessions::cursor_composer`])
/// store reasoning as a dedicated row (role "assistant", `kind="reasoning"`)
/// rather than leaving the thinking text embedded in the serialized
/// assistant-message content blob.
///
/// Multiple `thinking` blocks are concatenated in transcript order. A
/// `redacted_thinking` block carries no plaintext, so — mirroring Codex's
/// encrypted-reasoning convention, where
/// `response_item_reasoning_summary_text` declines to emit a row when there is
/// no plaintext summary — it never fabricates a body: a message whose only
/// reasoning is redacted yields no row (the block count is recorded as metadata
/// only when a plaintext row already exists).
///
/// Purely additive: the assistant message row itself is untouched (its content
/// blob still carries the thinking blocks verbatim in lossless storage).
pub(super) fn reasoning_from_line(
    record: &Value,
    session_id: &str,
    path: &Path,
    offset: i64,
) -> Option<SessionMessageRecord> {
    if record.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let message = record.get("message").unwrap_or(record);
    let blocks = message.get("content").and_then(Value::as_array)?;

    let mut thinking_parts = Vec::new();
    let mut redacted_blocks = 0usize;
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("thinking") => {
                if let Some(text) = block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .filter(|text| !text.trim().is_empty())
                {
                    thinking_parts.push(text.to_string());
                }
            }
            Some("redacted_thinking") => redacted_blocks += 1,
            _ => {}
        }
    }
    // No plaintext thinking: mirror Codex, which records nothing for encrypted
    // reasoning rather than fabricating a body from redacted content.
    if thinking_parts.is_empty() {
        return None;
    }
    let text = thinking_parts.join("\n\n");

    let base_id = conversational_message_id(message, record, session_id, offset);
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .filter(|role| !role.is_empty())
        .unwrap_or("assistant")
        .to_string();
    let model = message
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string);

    let mut metadata = Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("claude_thinking".to_string()),
    );
    // Parent linkage back to the assistant message row that owns this reasoning.
    metadata.insert(
        "parent_message_id".to_string(),
        Value::String(base_id.clone()),
    );
    metadata.insert(
        "thinking_blocks".to_string(),
        Value::from(thinking_parts.len() as i64),
    );
    if redacted_blocks > 0 {
        metadata.insert(
            "redacted_thinking_blocks".to_string(),
            Value::from(redacted_blocks as i64),
        );
    }

    Some(SessionMessageRecord {
        provider: PROVIDER.to_string(),
        // `{base}:thinking` keeps re-ingest idempotent and can never collide
        // with the owning message row's `{base}` id under the
        // `(provider, message_id)` primary key.
        message_id: format!("{base_id}:thinking"),
        session_id: session_id.to_string(),
        role,
        timestamp: record_timestamp(record),
        ordinal: offset,
        text,
        kind: Some(KIND_REASONING.to_string()),
        model,
        tool_names: None,
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&Value::Object(metadata)).ok(),
    })
}

/// Map a `type=="system"` hook-summary record to a compact, signal-only
/// `hook_event` row, or `None` for non-system records and routine hook
/// summaries that carry no error/interruption signal.
pub(super) fn system_hook_message_from_line(
    record: &Value,
    session_id: &str,
    path: &Path,
    offset: i64,
    _session_cwd: Option<&Path>,
) -> Option<SessionMessageRecord> {
    if record.get("type").and_then(Value::as_str) != Some("system") {
        return None;
    }

    let hook_errors: Vec<&Value> = record
        .get("hookErrors")
        .and_then(Value::as_array)
        .map(|errors| errors.iter().collect())
        .unwrap_or_default();
    let stop_reason = record
        .get("stopReason")
        .and_then(Value::as_str)
        .filter(|reason| !reason.is_empty());
    let prevented_continuation = record
        .get("preventedContinuation")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if hook_errors.is_empty() && stop_reason.is_none() && !prevented_continuation {
        return None;
    }

    let subtype = record.get("subtype").and_then(Value::as_str).unwrap_or("");
    let tool_use_id = record.get("toolUseID").and_then(Value::as_str);

    let mut lines = vec![format!("Claude hook event: {subtype}")];
    if let Some(tool_use_id) = tool_use_id {
        lines.push(format!("tool_use_id: {tool_use_id}"));
    }
    if let Some(stop_reason) = stop_reason {
        lines.push(format!("stop_reason: {stop_reason}"));
    }
    if prevented_continuation {
        lines.push("prevented_continuation: true".to_string());
    }
    if !hook_errors.is_empty() {
        let joined = hook_errors
            .iter()
            .map(|error| {
                error
                    .as_str()
                    .map_or_else(|| error.to_string(), str::to_string)
            })
            .collect::<Vec<_>>()
            .join("; ");
        lines.push(format!("hook_errors: {joined}"));
    }
    let joined = lines.join("\n");
    let text = preview_truncated(&joined, MARKER_PREVIEW_BYTES);

    let message_id = record
        .get("uuid")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map_or_else(|| format!("{session_id}:{offset}"), ToString::to_string);
    let timestamp = record
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_timestamp)
        .map(|secs| secs as i64);

    let mut metadata = Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("claude_system_record".to_string()),
    );
    metadata.insert("subtype".to_string(), Value::String(subtype.to_string()));
    if let Some(tool_use_id) = tool_use_id {
        metadata.insert(
            "tool_use_id".to_string(),
            Value::String(tool_use_id.to_string()),
        );
    }
    if let Some(hook_count) = record.get("hookCount") {
        metadata.insert("hook_count".to_string(), hook_count.clone());
    }
    if let Some(level) = record.get("level").and_then(Value::as_str) {
        metadata.insert("level".to_string(), Value::String(level.to_string()));
    }
    if prevented_continuation {
        metadata.insert("prevented_continuation".to_string(), Value::Bool(true));
    }

    Some(SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id,
        session_id: session_id.to_string(),
        // role "tool" keeps transient hook telemetry out of LCM policy anchors, which pin role system/developer.
        role: "tool".to_string(),
        timestamp,
        ordinal: offset,
        text,
        kind: Some("hook_event".to_string()),
        model: None,
        tool_names: None,
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&Value::Object(metadata)).ok(),
    })
}

/// Map a structured, non-conversational Claude record to a marker row:
/// `pr-link` records, `system` compaction boundaries, and model-fallback
/// records. Returns `None` for every other record type (leaving the cursor to
/// advance without emitting a row).
pub(super) fn structured_marker_from_line(
    record: &Value,
    session_id: &str,
    path: &Path,
    offset: i64,
    accumulator: &mut SessionAccumulator,
) -> Option<SessionMessageRecord> {
    match record.get("type").and_then(Value::as_str)? {
        "pr-link" => pr_link_row(record, session_id, path, offset, accumulator),
        "system" => compact_boundary_row(record, session_id, path, offset)
            .or_else(|| model_fallback_row(record, session_id, path, offset)),
        _ => None,
    }
}

/// Common ISO-8601 timestamp read for a top-level record.
pub(super) fn record_cwd(record: &Value) -> Option<PathBuf> {
    record
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from)
}
