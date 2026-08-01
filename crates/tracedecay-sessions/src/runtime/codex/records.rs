//! Mapping of Codex rollout lines (`event_msg`, `response_item`, `compacted`)
//! to provider-neutral [`SessionMessageRecord`] rows.

use std::path::Path;

use serde_json::Value;

use super::PROVIDER;
use super::goals::{CodexGoalContext, codex_goal_context_from_text};
use super::meta::CodexMeta;
use crate::accounting::parser::parse_timestamp;
use crate::runtime::SessionMessageRecord;
use crate::runtime::shared::{append_tool_calls_metadata, content_storage_text_and_tools};

/// Threshold above which a tool call's arguments / a tool output is flagged as
/// truncated in metadata. Raw tool-call arguments and tool outputs are never
/// embedded in the FTS-searchable message text (they can carry secrets); only
/// byte counts and this truncation flag are recorded. The lossless body already
/// lives in the Codex rollout itself, recoverable via `source_path`/
/// `source_offset`.
const TOOL_EVENT_PREVIEW_BYTES: usize = 2000;

/// Map one rollout line to a provider-neutral message, or `None` for non-message
/// events (`response_item`, tool calls, token counts, …).
pub fn message_from_line(
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

    let timestamp = timestamp_from_record(record);
    if let Some(goal_context) = codex_goal_context_from_text(&text) {
        return Some(goal_context_message(
            meta,
            model,
            path,
            offset,
            timestamp,
            &goal_context,
            &message_metadata(payload, Some(&goal_context)),
        ));
    }

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
        metadata_json: serde_json::to_string(&message_metadata(payload, None)).ok(),
    })
}

pub fn response_item_goal_context_from_line(
    record: &Value,
    meta: &CodexMeta,
    model: Option<&str>,
    path: &Path,
    offset: i64,
) -> Option<SessionMessageRecord> {
    if record.get("type").and_then(Value::as_str) != Some("response_item") {
        return None;
    }
    let payload = record.get("payload")?;
    if payload.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let text = collect_response_item_text(payload.get("content").unwrap_or(payload));
    let goal_context = codex_goal_context_from_text(&text)?;
    let mut metadata = message_metadata(payload, Some(&goal_context));
    if let Value::Object(map) = &mut metadata {
        map.insert(
            "source_event".to_string(),
            Value::String("response_item".to_string()),
        );
        if let Some(role) = payload.get("role").and_then(Value::as_str) {
            map.insert("source_role".to_string(), Value::String(role.to_string()));
        }
    }

    Some(goal_context_message(
        meta,
        model,
        path,
        offset,
        timestamp_from_record(record),
        &goal_context,
        &metadata,
    ))
}

pub fn response_item_tool_event_from_line(
    record: &Value,
    meta: &CodexMeta,
    model: Option<&str>,
    path: &Path,
    offset: i64,
) -> Option<SessionMessageRecord> {
    if record.get("type").and_then(Value::as_str) != Some("response_item") {
        return None;
    }
    let payload = record.get("payload")?;
    let response_item_type = payload.get("type").and_then(Value::as_str)?;
    // Serialize the output payload once and share it with both helpers below.
    let output = payload.get("output").map(compact_response_item_value);
    let (role, text, metadata) = match response_item_type {
        "function_call" | "custom_tool_call" | "tool_search_call" | "web_search_call" => {
            let tool_name = response_item_tool_name(payload, response_item_type);
            let text =
                response_item_tool_call_text(response_item_type, tool_name.as_deref(), payload);
            (
                "tool",
                text,
                response_item_tool_metadata(
                    response_item_type,
                    payload,
                    tool_name,
                    output.as_deref(),
                ),
            )
        }
        "function_call_output" | "custom_tool_call_output" => {
            let text = response_item_tool_output_text(payload, output.as_deref())?;
            (
                "tool",
                text,
                response_item_tool_metadata(response_item_type, payload, None, output.as_deref()),
            )
        }
        "reasoning" => {
            let text = response_item_reasoning_summary_text(payload)?;
            (
                "assistant",
                text,
                response_item_tool_metadata(response_item_type, payload, None, output.as_deref()),
            )
        }
        _ => return None,
    };
    if text.trim().is_empty() {
        return None;
    }
    Some(SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id: format!("{}:{offset}", meta.session_id),
        session_id: meta.session_id.clone(),
        role: role.to_string(),
        timestamp: timestamp_from_record(record),
        ordinal: offset,
        text,
        kind: Some(if response_item_type == "reasoning" {
            "reasoning".to_string()
        } else {
            "tool_event".to_string()
        }),
        model: model.map(str::to_string),
        tool_names: response_item_tool_name(payload, response_item_type),
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&metadata).ok(),
    })
}

pub fn response_item_tool_name(payload: &Value, response_item_type: &str) -> Option<String> {
    payload
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| match response_item_type {
            "tool_search_call" => Some("tool_search".to_string()),
            "web_search_call" => Some("web_search".to_string()),
            _ => None,
        })
}

fn response_item_tool_call_text(
    response_item_type: &str,
    tool_name: Option<&str>,
    payload: &Value,
) -> String {
    let label = tool_name.unwrap_or(response_item_type);
    let mut parts = vec![format!("Codex tool call: {label}")];
    if let Some(namespace) = payload.get("namespace").and_then(Value::as_str) {
        parts.push(format!("namespace: {namespace}"));
    }
    if let Some(call_id) = payload.get("call_id").and_then(Value::as_str) {
        parts.push(format!("call_id: {call_id}"));
    }
    // Never embed raw arguments in the FTS-searchable text — they can carry
    // secrets (tokens, credentials, private paths). Record only the byte count;
    // the lossless arguments remain in the rollout at `source_offset`.
    if let Some(arguments_bytes) = response_item_arguments_bytes(payload) {
        parts.push(format!("arguments_bytes: {arguments_bytes}"));
    }
    parts.join("\n")
}

/// Byte length of a tool call's arguments payload (`arguments`/`input`/`action`,
/// whichever is present) after compact serialization. Returns `None` when the
/// item carries no argument payload.
fn response_item_arguments_bytes(payload: &Value) -> Option<usize> {
    payload
        .get("arguments")
        .or_else(|| payload.get("input"))
        .or_else(|| payload.get("action"))
        .map(compact_response_item_value)
        .map(|arguments| arguments.len())
}

fn response_item_tool_output_text(payload: &Value, output: Option<&str>) -> Option<String> {
    let call_id = payload
        .get("call_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let output = output?;
    let output_bytes = output.len();
    // Record only the byte count — the raw tool output can carry secrets and
    // must not land in the FTS-searchable text. The full body stays in the
    // rollout, recoverable via `source_path`/`source_offset`.
    Some(format!(
        "Codex tool output: {call_id}\noutput_bytes: {output_bytes}"
    ))
}

fn response_item_reasoning_summary_text(payload: &Value) -> Option<String> {
    let summary = payload.get("summary")?;
    let text = collect_response_item_text(summary);
    (!text.trim().is_empty()).then(|| format!("Codex reasoning summary:\n{text}"))
}

fn compact_response_item_value(value: &Value) -> String {
    value.as_str().map_or_else(
        || serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
        str::to_string,
    )
}

pub fn response_item_tool_metadata(
    response_item_type: &str,
    payload: &Value,
    tool_name: Option<String>,
    output: Option<&str>,
) -> Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("codex_response_item".to_string()),
    );
    metadata.insert(
        "response_item_type".to_string(),
        Value::String(response_item_type.to_string()),
    );
    for key in ["call_id", "id", "status", "namespace"] {
        if let Some(value) = payload.get(key) {
            metadata.insert(key.to_string(), value.clone());
        }
    }
    if let Some(tool_name) = tool_name {
        metadata.insert("tool_name".to_string(), Value::String(tool_name));
    }
    if response_item_type == "reasoning" {
        metadata.insert(
            "reasoning_visibility".to_string(),
            Value::String("provider_exposed".to_string()),
        );
        metadata.insert(
            "reasoning_retention".to_string(),
            Value::String("provider_exposed".to_string()),
        );
    }
    // Byte counts + truncation flags only — never the raw argument/output bytes.
    if let Some(arguments_bytes) = response_item_arguments_bytes(payload) {
        metadata.insert(
            "arguments_bytes".to_string(),
            Value::from(arguments_bytes as i64),
        );
        metadata.insert(
            "arguments_truncated".to_string(),
            Value::Bool(arguments_bytes > TOOL_EVENT_PREVIEW_BYTES),
        );
    }
    if let Some(output) = output {
        metadata.insert("output_bytes".to_string(), Value::from(output.len() as i64));
        metadata.insert(
            "output_truncated".to_string(),
            Value::Bool(output.len() > TOOL_EVENT_PREVIEW_BYTES),
        );
    }
    Value::Object(metadata)
}

fn goal_context_message(
    meta: &CodexMeta,
    model: Option<&str>,
    path: &Path,
    offset: i64,
    timestamp: Option<i64>,
    goal_context: &CodexGoalContext,
    metadata: &Value,
) -> SessionMessageRecord {
    SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id: format!("{}:{offset}", meta.session_id),
        session_id: meta.session_id.clone(),
        role: "system".to_string(),
        timestamp,
        ordinal: offset,
        text: goal_context.storage_text(),
        kind: Some("goal_context".to_string()),
        model: model.map(str::to_string),
        tool_names: None,
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&metadata).ok(),
    }
}

pub fn collect_response_item_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(collect_response_item_text)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(map) => {
            if let Some(text) = map.get("text").and_then(Value::as_str) {
                return text.to_string();
            }
            ["content", "message", "item"]
                .iter()
                .filter_map(|key| map.get(*key))
                .map(collect_response_item_text)
                .find(|text| !text.is_empty())
                .unwrap_or_default()
        }
        _ => String::new(),
    }
}

pub fn timestamp_from_record(record: &Value) -> Option<i64> {
    record
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_timestamp)
        .map(|secs| secs as i64)
}

pub fn compacted_summary_from_line(
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
        timestamp: timestamp_from_record(record),
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

fn message_metadata(payload: &Value, goal_context: Option<&CodexGoalContext>) -> Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("codex_rollout".to_string()),
    );
    if let Some(goal_context) = goal_context {
        metadata.insert(
            "codex_internal_context".to_string(),
            Value::String("goal".to_string()),
        );
        metadata.insert("codex_goal".to_string(), goal_context.metadata());
    }
    append_tool_calls_metadata(&mut metadata, payload);
    Value::Object(metadata)
}
