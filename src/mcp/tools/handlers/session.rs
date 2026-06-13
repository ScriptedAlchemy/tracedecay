use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use serde_json::{json, Map, Value};

use super::truncated_json_envelope_with_handle;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::GlobalDb;
use crate::mcp::tools::{ToolResult, MAX_RESPONSE_CHARS};
use crate::sessions::cursor::HermesProfileDbReadOnly;
use crate::sessions::lcm::{
    LcmCleanConfig, LcmCompressionRequest, LcmContentSlice, LcmDescribeRequest, LcmDescribeTarget,
    LcmExpandQueryRequest, LcmExpandRequest, LcmExpandTarget, LcmGrepRequest, LcmGrepSort,
    LcmLoadSessionRequest, LcmPreflightRequest, LcmScope, LcmSessionBoundaryRequest,
    LcmSummarizerMode, LCM_EXPAND_QUERY_SYNTHESIS_SYSTEM_PROMPT,
};
use crate::sessions::SessionSearchScope;
use crate::tracedecay::TraceDecay;

const DEFAULT_LCM_CONTENT_LIMIT: usize = 4096;
const DEFAULT_LCM_EXPAND_QUERY_CONTEXT_LIMIT: usize = 32_000;
const MAX_LCM_EXPAND_QUERY_CONTEXT_LIMIT: usize = 65_536;
const MAX_LCM_CONTENT_LIMIT: usize = 8192;
const MAX_LCM_LOAD_CONTENT_LIMIT: usize = 20_000;
const MAX_LCM_RESULT_LIMIT: usize = 100;
const MAX_LCM_EXPAND_QUERY_PROMPT_CHARS: usize = 2_048;
const MAX_LCM_EXPAND_QUERY_QUERY_CHARS: usize = 1_024;
const MAX_LCM_EXPAND_QUERY_SYNTHESIS_SYSTEM_CHARS: usize = 1_024;
const MAX_LCM_EXPAND_QUERY_SYNTHESIS_PROMPT_CHARS: usize = 2_048;

fn tool_json(project_root: Option<&Path>, value: &Value) -> ToolResult {
    let formatted = serde_json::to_string_pretty(value).unwrap_or_default();
    let text = if formatted.len() <= MAX_RESPONSE_CHARS {
        formatted
    } else {
        truncated_json_envelope_with_handle(project_root, &formatted)
    };
    ToolResult {
        value: json!({ "content": [{ "type": "text", "text": text }] }),
        touched_files: Vec::new(),
    }
}

fn lcm_preflight_tool_json(value: &Value) -> ToolResult {
    let formatted = serde_json::to_string_pretty(value).unwrap_or_default();
    let text = if formatted.len() <= MAX_RESPONSE_CHARS {
        formatted
    } else {
        let compact = compact_lcm_preflight_payload(value, formatted.len(), 8, 512);
        let compact_text = serde_json::to_string_pretty(&compact).unwrap_or_default();
        if compact_text.len() <= MAX_RESPONSE_CHARS {
            compact_text
        } else {
            let minimal = compact_lcm_preflight_payload(value, formatted.len(), 4, 256);
            serde_json::to_string_pretty(&minimal).unwrap_or_default()
        }
    };
    ToolResult {
        value: json!({ "content": [{ "type": "text", "text": text }] }),
        touched_files: Vec::new(),
    }
}

fn compact_lcm_preflight_payload(
    value: &Value,
    original_chars: usize,
    replay_limit: usize,
    replay_content_chars: usize,
) -> Value {
    let mut object = Map::new();
    for key in [
        "status",
        "provider",
        "session_id",
        "should_compress",
        "reason",
    ] {
        if let Some(field) = value.get(key) {
            object.insert(key.to_string(), field.clone());
        }
    }
    let (replay_messages, replay_truncated) = compact_replay_messages(
        value.get("replay_messages"),
        replay_limit,
        replay_content_chars,
    );
    object.insert("replay_messages".to_string(), replay_messages);
    object.insert(
        "replay_messages_truncated_for_mcp".to_string(),
        json!(replay_truncated),
    );
    object.insert("mcp_response_truncated".to_string(), json!(true));
    object.insert("contract_truncated".to_string(), json!(true));
    object.insert(
        "mcp_original_response_chars".to_string(),
        json!(original_chars),
    );
    object.insert(
        "mcp_truncation_reason".to_string(),
        json!("lcm-preflight response compacted to preserve Hermes bridge contract"),
    );
    Value::Object(object)
}

fn lcm_compress_tool_json(project_root: Option<&Path>, value: &Value) -> ToolResult {
    let formatted = serde_json::to_string_pretty(value).unwrap_or_default();
    let text = if formatted.len() <= MAX_RESPONSE_CHARS {
        formatted
    } else if value.get("status").and_then(Value::as_str) == Some("needs_summary") {
        let compact = compact_lcm_compress_payload(value, formatted.len(), false);
        let compact_text = serde_json::to_string_pretty(&compact).unwrap_or_default();
        if compact_text.len() <= MAX_RESPONSE_CHARS {
            compact_text
        } else {
            let minimal = compact_lcm_compress_payload(value, formatted.len(), true);
            serde_json::to_string_pretty(&minimal).unwrap_or_default()
        }
    } else {
        truncated_json_envelope_with_handle(project_root, &formatted)
    };
    ToolResult {
        value: json!({ "content": [{ "type": "text", "text": text }] }),
        touched_files: Vec::new(),
    }
}

fn compact_lcm_compress_payload(value: &Value, original_chars: usize, minimal: bool) -> Value {
    let mut object = Map::new();
    let keys: &[&str] = if minimal {
        &[
            "status",
            "provider",
            "session_id",
            "reason",
            "summary_nodes_created",
            "replay_token_estimate",
            "replay_over_budget",
            "compression_attempts",
            "fallback_used",
            "retry_status",
        ]
    } else {
        &[
            "status",
            "provider",
            "session_id",
            "reason",
            "summary_nodes_created",
            "summary_nodes",
            "replay_messages",
            "replay_token_estimate",
            "replay_over_budget",
            "compression_attempts",
            "fallback_used",
            "retry_status",
            "frontier",
        ]
    };
    for key in keys {
        if let Some(field) = value.get(key) {
            object.insert((*key).to_string(), field.clone());
        }
    }
    if minimal {
        let (replay_messages, replay_truncated) =
            compact_replay_messages(value.get("replay_messages"), 8, 512);
        object.insert("replay_messages".to_string(), replay_messages);
        object.insert(
            "replay_messages_truncated_for_mcp".to_string(),
            json!(replay_truncated),
        );
    }

    if let Some(summary_request) = value.get("summary_request").and_then(Value::as_object) {
        let mut compact_request = Map::new();
        for key in [
            "provider",
            "session_id",
            "focus_topic",
            "source_range",
            "token_budget",
            "routes",
        ] {
            if let Some(field) = summary_request.get(key) {
                compact_request.insert(key.to_string(), field.clone());
            }
        }
        compact_request.insert("source_messages_omitted_for_mcp".to_string(), json!(true));
        compact_request.insert("prompt_omitted_for_mcp".to_string(), json!(true));
        compact_request.insert(
            "extraction_request_omitted_for_mcp".to_string(),
            json!(true),
        );
        object.insert(
            "summary_request".to_string(),
            Value::Object(compact_request),
        );
    } else if let Some(field) = value.get("summary_request") {
        object.insert("summary_request".to_string(), field.clone());
    }

    object.insert("mcp_response_truncated".to_string(), json!(true));
    object.insert("contract_truncated".to_string(), json!(true));
    object.insert(
        "mcp_original_response_chars".to_string(),
        json!(original_chars),
    );
    object.insert(
        "mcp_truncation_reason".to_string(),
        json!("lcm-compress needs-summary response compacted to preserve Hermes bridge contract"),
    );
    Value::Object(object)
}

fn compact_replay_messages(
    value: Option<&Value>,
    limit: usize,
    content_chars: usize,
) -> (Value, bool) {
    let Some(array) = value.and_then(Value::as_array) else {
        return (json!([]), false);
    };
    let mut truncated = array.len() > limit;
    let messages = array
        .iter()
        .take(limit)
        .map(|item| {
            let mut object = Map::new();
            if let Some(map) = item.as_object() {
                for (key, field) in map {
                    if key == "content" {
                        if let Some(content) = field.as_str() {
                            let (content, content_truncated) =
                                truncate_chars(content, content_chars);
                            object.insert(key.clone(), json!(content));
                            object.insert(
                                "content_truncated_for_mcp".to_string(),
                                json!(content_truncated),
                            );
                            truncated |= content_truncated;
                        } else {
                            object.insert(key.clone(), field.clone());
                        }
                    } else {
                        object.insert(key.clone(), field.clone());
                    }
                }
            }
            Value::Object(object)
        })
        .collect::<Vec<_>>();
    (Value::Array(messages), truncated)
}

fn lcm_expand_query_tool_json(project_root: Option<&Path>, value: &Value) -> ToolResult {
    let formatted = serde_json::to_string_pretty(value).unwrap_or_default();
    let needs_synthesis = value
        .get("needs_synthesis")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let text = if formatted.len() <= MAX_RESPONSE_CHARS {
        formatted
    } else if needs_synthesis {
        let compact =
            compact_lcm_expand_query_payload(value, formatted.len(), CompactTier::Standard);
        let text = serde_json::to_string_pretty(&compact).unwrap_or_default();
        if text.len() <= MAX_RESPONSE_CHARS {
            text
        } else {
            let fallback = compact_lcm_expand_query_payload(
                value,
                formatted.len(),
                CompactTier::Minimal {
                    compact_chars: text.len(),
                },
            );
            serde_json::to_string_pretty(&fallback).unwrap_or_default()
        }
    } else {
        truncated_json_envelope_with_handle(project_root, &formatted)
    };
    let text = if text.len() <= MAX_RESPONSE_CHARS || needs_synthesis {
        text
    } else {
        truncated_json_envelope_with_handle(project_root, &text)
    };
    ToolResult {
        value: json!({ "content": [{ "type": "text", "text": text }] }),
        touched_files: Vec::new(),
    }
}

#[derive(Copy, Clone)]
enum CompactTier {
    Standard,
    Minimal { compact_chars: usize },
}

fn compact_lcm_expand_query_payload(
    value: &Value,
    original_chars: usize,
    tier: CompactTier,
) -> Value {
    let limits = match tier {
        CompactTier::Standard => LcmExpandQueryCompactLimits {
            max_context_blocks: 3,
            max_context_block_chars: 600,
            max_matches: 10,
            max_match_snippet_chars: 160,
            max_node_ids: 50,
            max_node_id_chars: 160,
            max_pagination_items: 50,
            max_scalar_chars: None,
            max_prompt_chars: MAX_LCM_EXPAND_QUERY_PROMPT_CHARS,
            max_query_chars: MAX_LCM_EXPAND_QUERY_QUERY_CHARS,
            max_synthesis_system_chars: MAX_LCM_EXPAND_QUERY_SYNTHESIS_SYSTEM_CHARS,
            max_synthesis_prompt_chars: MAX_LCM_EXPAND_QUERY_SYNTHESIS_PROMPT_CHARS,
            compact_chars: None,
            truncation_reason:
                "expand-query response compacted to preserve synthesis contract fields",
        },
        CompactTier::Minimal { compact_chars } => LcmExpandQueryCompactLimits {
            max_context_blocks: 1,
            max_context_block_chars: 240,
            max_matches: 5,
            max_match_snippet_chars: 80,
            max_node_ids: 25,
            max_node_id_chars: 120,
            max_pagination_items: 10,
            max_scalar_chars: Some(512),
            max_prompt_chars: 512,
            max_query_chars: 512,
            max_synthesis_system_chars: 512,
            max_synthesis_prompt_chars: 512,
            compact_chars: Some(compact_chars),
            truncation_reason: "expand-query response reduced to minimal synthesis contract after compact payload overflow",
        },
    };

    let mut object = Map::new();
    if let Some(max_scalar_chars) = limits.max_scalar_chars {
        for key in [
            "status",
            "provider",
            "session_id",
            "storage_scope",
            "answer",
        ] {
            insert_bounded_scalar_field(&mut object, value, key, max_scalar_chars);
        }
        for key in [
            "needs_synthesis",
            "max_tokens",
            "context_max_tokens",
            "context_budget",
            "context_truncated",
        ] {
            if let Some(field) = value.get(key) {
                object.insert(key.to_string(), field.clone());
            }
        }
        insert_bounded_text_field(&mut object, value, "prompt", limits.max_prompt_chars);
        insert_bounded_text_field(&mut object, value, "query", limits.max_query_chars);
    } else {
        for key in [
            "status",
            "provider",
            "session_id",
            "storage_scope",
            "answer",
            "needs_synthesis",
            "max_tokens",
            "context_max_tokens",
            "context_budget",
            "context_truncated",
        ] {
            if let Some(field) = value.get(key) {
                object.insert(key.to_string(), field.clone());
            }
        }
        insert_bounded_text_field(&mut object, value, "prompt", limits.max_prompt_chars);
        insert_bounded_text_field(&mut object, value, "query", limits.max_query_chars);
        object.insert("mcp_response_truncated".to_string(), json!(true));
        object.insert("contract_truncated".to_string(), json!(true));
        object.insert(
            "mcp_original_response_chars".to_string(),
            json!(original_chars),
        );
        object.insert(
            "mcp_truncation_reason".to_string(),
            json!(limits.truncation_reason),
        );
    }

    let (context_blocks, context_blocks_truncated) = compact_context_blocks(
        value.get("context_blocks"),
        limits.max_context_blocks,
        limits.max_context_block_chars,
    );
    let (matches, matches_truncated) = compact_matches(
        value.get("matches"),
        limits.max_matches,
        limits.max_match_snippet_chars,
    );
    let (node_ids, node_ids_truncated) = compact_string_array(
        value.get("node_ids"),
        limits.max_node_ids,
        limits.max_node_id_chars,
    );
    let (context_pagination, pagination_truncated) =
        compact_array(value.get("context_pagination"), limits.max_pagination_items);

    object.insert("context_blocks".to_string(), context_blocks.clone());
    object.insert(
        "context_blocks_truncated_for_mcp".to_string(),
        json!(context_blocks_truncated),
    );
    object.insert("matches".to_string(), matches);
    object.insert(
        "matches_truncated_for_mcp".to_string(),
        json!(matches_truncated),
    );
    object.insert("node_ids".to_string(), node_ids);
    object.insert(
        "node_ids_truncated_for_mcp".to_string(),
        json!(node_ids_truncated),
    );
    object.insert("context_pagination".to_string(), context_pagination);
    object.insert(
        "context_pagination_truncated_for_mcp".to_string(),
        json!(pagination_truncated),
    );
    object.insert(
        "synthesis_prompt".to_string(),
        compact_synthesis_prompt_with_limits(
            value,
            &context_blocks,
            limits.max_synthesis_system_chars,
            limits.max_synthesis_prompt_chars,
        ),
    );

    if limits.max_scalar_chars.is_some() {
        object.insert("mcp_response_truncated".to_string(), json!(true));
        object.insert("contract_truncated".to_string(), json!(true));
        object.insert(
            "mcp_original_response_chars".to_string(),
            json!(original_chars),
        );
        if let Some(compact_chars) = limits.compact_chars {
            object.insert(
                "mcp_compact_response_chars".to_string(),
                json!(compact_chars),
            );
        }
        object.insert(
            "mcp_truncation_reason".to_string(),
            json!(limits.truncation_reason),
        );
    }

    Value::Object(object)
}

struct LcmExpandQueryCompactLimits {
    max_context_blocks: usize,
    max_context_block_chars: usize,
    max_matches: usize,
    max_match_snippet_chars: usize,
    max_node_ids: usize,
    max_node_id_chars: usize,
    max_pagination_items: usize,
    max_scalar_chars: Option<usize>,
    max_prompt_chars: usize,
    max_query_chars: usize,
    max_synthesis_system_chars: usize,
    max_synthesis_prompt_chars: usize,
    compact_chars: Option<usize>,
    truncation_reason: &'static str,
}

fn compact_array(value: Option<&Value>, limit: usize) -> (Value, bool) {
    let Some(array) = value.and_then(Value::as_array) else {
        return (json!([]), false);
    };
    (
        Value::Array(array.iter().take(limit).cloned().collect()),
        array.len() > limit,
    )
}

fn compact_matches(value: Option<&Value>, limit: usize, snippet_chars: usize) -> (Value, bool) {
    let Some(array) = value.and_then(Value::as_array) else {
        return (json!([]), false);
    };
    let matches = array
        .iter()
        .take(limit)
        .map(|item| {
            let mut object = Map::new();
            for key in ["kind", "node_id", "store_id"] {
                if let Some(field) = item.get(key) {
                    object.insert(key.to_string(), field.clone());
                }
            }
            if let Some(snippet) = item.get("snippet").and_then(Value::as_str) {
                let (snippet, truncated) = truncate_chars(snippet, snippet_chars);
                object.insert("snippet".to_string(), json!(snippet));
                object.insert("snippet_truncated_for_mcp".to_string(), json!(truncated));
            }
            Value::Object(object)
        })
        .collect::<Vec<_>>();
    (Value::Array(matches), array.len() > limit)
}

fn compact_string_array(value: Option<&Value>, limit: usize, item_chars: usize) -> (Value, bool) {
    let Some(array) = value.and_then(Value::as_array) else {
        return (json!([]), false);
    };
    let mut truncated = array.len() > limit;
    let values = array
        .iter()
        .take(limit)
        .filter_map(|item| item.as_str())
        .map(|item| {
            let (item, item_truncated) = truncate_chars(item, item_chars);
            truncated |= item_truncated;
            json!(item)
        })
        .collect::<Vec<_>>();
    (Value::Array(values), truncated)
}

fn compact_context_blocks(
    value: Option<&Value>,
    limit: usize,
    content_chars: usize,
) -> (Value, bool) {
    let Some(array) = value.and_then(Value::as_array) else {
        return (json!([]), false);
    };
    let mut truncated = array.len() > limit;
    let blocks = array
        .iter()
        .take(limit)
        .map(|item| {
            let mut object = Map::new();
            for key in ["kind", "node_id", "source_ref", "content_range"] {
                if let Some(field) = item.get(key) {
                    object.insert(key.to_string(), field.clone());
                }
            }
            let content = item
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let (content, content_truncated) = truncate_chars(content, content_chars);
            truncated |= content_truncated;
            object.insert("content".to_string(), json!(content));
            object.insert(
                "content_truncated_for_mcp".to_string(),
                json!(content_truncated),
            );
            object.insert("raw_message".to_string(), Value::Null);
            object.insert("summary_node".to_string(), Value::Null);
            Value::Object(object)
        })
        .collect::<Vec<_>>();
    (Value::Array(blocks), truncated)
}

fn compact_synthesis_prompt_with_limits(
    value: &Value,
    context_blocks: &Value,
    system_chars: usize,
    prompt_chars: usize,
) -> Value {
    let default_system = LCM_EXPAND_QUERY_SYNTHESIS_SYSTEM_PROMPT;
    let system = value
        .get("synthesis_prompt")
        .and_then(|prompt| prompt.get("system"))
        .and_then(Value::as_str)
        .unwrap_or(default_system);
    let (system, system_truncated) = truncate_chars(system, system_chars);
    let prompt = value
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let (prompt, prompt_truncated) = truncate_chars(prompt, prompt_chars);
    let context_json = serde_json::to_string_pretty(context_blocks).unwrap_or_else(|_| "[]".into());
    let truncation_note = if value
        .get("context_truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        "\n\nNOTE: Some LCM context was truncated before MCP response compaction; pagination metadata is included in the tool response."
    } else {
        ""
    };
    let prompt_truncation_note = if prompt_truncated {
        "\n\nNOTE: The original question was truncated in this MCP response; synthesize from the bounded question preview and returned context, or state that the response degraded because the prompt exceeded the MCP response budget."
    } else {
        ""
    };
    json!({
        "system": system,
        "system_truncated_for_mcp": system_truncated,
        "user_prompt_truncated_for_mcp": prompt_truncated,
        "user": format!(
            "QUESTION:\n{prompt}\n\nCOMPACT EXPANDED CONTEXT:\n{context_json}{truncation_note}{prompt_truncation_note}\n\nNOTE: The MCP response was compacted to preserve the synthesis contract. Use node_ids and context_pagination for follow-up expansion if more context is needed."
        ),
    })
}

fn insert_bounded_text_field(
    object: &mut Map<String, Value>,
    value: &Value,
    key: &str,
    max_chars: usize,
) {
    let truncated_key = format!("{key}_truncated_for_mcp");
    match value.get(key) {
        Some(Value::String(text)) => {
            let (text, truncated) = truncate_chars(text, max_chars);
            object.insert(key.to_string(), json!(text));
            object.insert(truncated_key, json!(truncated));
        }
        Some(Value::Null) => {
            object.insert(key.to_string(), Value::Null);
            object.insert(truncated_key, json!(false));
        }
        Some(field) => {
            object.insert(key.to_string(), field.clone());
            object.insert(truncated_key, json!(false));
        }
        None => {}
    }
}

fn insert_bounded_scalar_field(
    object: &mut Map<String, Value>,
    value: &Value,
    key: &str,
    max_chars: usize,
) {
    match value.get(key) {
        Some(Value::String(text)) => {
            let (text, truncated) = truncate_chars(text, max_chars);
            object.insert(key.to_string(), json!(text));
            object.insert(format!("{key}_truncated_for_mcp"), json!(truncated));
        }
        Some(Value::Bool(_) | Value::Number(_) | Value::Null) => {
            object.insert(key.to_string(), value[key].clone());
        }
        _ => {}
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> (String, bool) {
    let truncated = value.chars().nth(max_chars).is_some();
    let text = value.chars().take(max_chars).collect::<String>();
    (text, truncated)
}

fn string_arg<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn required_string_arg<'a>(args: &'a Value, name: &str) -> Result<&'a str> {
    string_arg(args, name).ok_or_else(|| TraceDecayError::Config {
        message: format!("missing required parameter: {name}"),
    })
}

fn argument_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

fn bounded_usize_arg(args: &Value, name: &str, min: usize, max: usize) -> Result<Option<usize>> {
    let Some(value) = args.get(name) else {
        return Ok(None);
    };
    let Some(integer) = value.as_i64() else {
        return Err(argument_error(format!("{name} must be an integer")));
    };
    if integer < 0 {
        return Err(argument_error(format!("{name} must be >= {min}")));
    }
    let integer =
        usize::try_from(integer).map_err(|_| argument_error(format!("{name} must be <= {max}")))?;
    if integer < min {
        return Err(argument_error(format!("{name} must be >= {min}")));
    }
    if integer > max {
        return Err(argument_error(format!("{name} must be <= {max}")));
    }
    Ok(Some(integer))
}

fn non_negative_i64_arg(args: &Value, name: &str) -> Result<Option<i64>> {
    let Some(value) = args.get(name) else {
        return Ok(None);
    };
    let Some(integer) = value.as_i64() else {
        return Err(argument_error(format!("{name} must be an integer")));
    };
    if integer < 0 {
        return Err(argument_error(format!("{name} must be >= 0")));
    }
    Ok(Some(integer))
}

fn signed_i64_arg(args: &Value, name: &str) -> Result<Option<i64>> {
    let Some(value) = args.get(name) else {
        return Ok(None);
    };
    value
        .as_i64()
        .map(Some)
        .ok_or_else(|| argument_error(format!("{name} must be an integer")))
}

fn bool_arg(args: &Value, name: &str) -> Result<Option<bool>> {
    let Some(value) = args.get(name) else {
        return Ok(None);
    };
    value
        .as_bool()
        .map(Some)
        .ok_or_else(|| argument_error(format!("{name} must be a boolean")))
}

fn non_negative_i64_arg_alias(args: &Value, primary: &str, alias: &str) -> Result<Option<i64>> {
    match non_negative_i64_arg(args, primary)? {
        Some(value) => Ok(Some(value)),
        None => non_negative_i64_arg(args, alias),
    }
}

fn non_negative_timestamp_arg_alias(
    args: &Value,
    primary: &str,
    alias: &str,
) -> Result<Option<i64>> {
    match non_negative_timestamp_arg(args, primary)? {
        Some(value) => Ok(Some(value)),
        None => non_negative_timestamp_arg(args, alias),
    }
}

fn non_negative_timestamp_arg(args: &Value, name: &str) -> Result<Option<i64>> {
    let Some(value) = args.get(name) else {
        return Ok(None);
    };
    let timestamp = match value {
        Value::Number(number) => number
            .as_i64()
            .ok_or_else(|| timestamp_argument_error(name))?,
        Value::String(text) => parse_timestamp_string(text, name)?,
        _ => return Err(timestamp_argument_error(name)),
    };
    if timestamp < 0 {
        return Err(argument_error(format!("{name} must be >= 0")));
    }
    Ok(Some(timestamp))
}

fn parse_timestamp_string(value: &str, name: &str) -> Result<i64> {
    let text = value.trim();
    if text.is_empty() {
        return Err(argument_error(format!("{name} must not be empty")));
    }
    if let Ok(timestamp) = text.parse::<i64>() {
        if timestamp >= 0 {
            return Ok(timestamp);
        }
        return Err(argument_error(format!("{name} must be >= 0")));
    }
    crate::timeutil::parse_rfc3339_timestamp(text).ok_or_else(|| timestamp_argument_error(name))
}

fn timestamp_argument_error(name: &str) -> TraceDecayError {
    argument_error(format!(
        "{name} must be a non-negative Unix timestamp or timezone-aware ISO/RFC3339 string"
    ))
}

fn provider_arg(args: &Value) -> &str {
    string_arg(args, "provider").unwrap_or("cursor")
}

fn messages_arg(args: &Value) -> Result<Vec<Value>> {
    let Some(messages) = args.get("messages") else {
        return Ok(Vec::new());
    };
    let Some(messages) = messages.as_array() else {
        return Err(argument_error("messages must be an array"));
    };
    Ok(messages.clone())
}

fn string_array_arg(args: &Value, name: &str) -> Result<Vec<String>> {
    let Some(value) = args.get(name) else {
        return Ok(Vec::new());
    };
    let Some(values) = value.as_array() else {
        return Err(argument_error(format!("{name} must be an array")));
    };
    values
        .iter()
        .map(|value| {
            if let Some(text) = value
                .as_str()
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                return Ok(text.to_string());
            }
            if let Some(integer) = value.as_i64() {
                if integer >= 0 {
                    return Ok(integer.to_string());
                }
            }
            Err(argument_error(format!(
                "{name} must contain only non-empty strings or non-negative integers"
            )))
        })
        .collect()
}

fn summarizer_arg(args: &Value) -> Result<LcmSummarizerMode> {
    let Some(summarizer) = args.get("summarizer") else {
        return Ok(LcmSummarizerMode::Noop);
    };
    serde_json::from_value(summarizer.clone()).map_err(|err| TraceDecayError::Config {
        message: format!("invalid summarizer: {err}"),
    })
}

fn lcm_content_slice(args: &Value) -> Result<LcmContentSlice> {
    Ok(LcmContentSlice {
        offset: bounded_usize_arg(args, "content_offset", 0, usize::MAX)?.unwrap_or(0),
        limit: bounded_usize_arg(args, "content_limit", 1, MAX_LCM_CONTENT_LIMIT)?
            .unwrap_or(DEFAULT_LCM_CONTENT_LIMIT),
    })
}

fn lcm_load_content_slice(args: &Value) -> Result<(LcmContentSlice, Option<usize>)> {
    let offset = bounded_usize_arg(args, "content_offset", 0, usize::MAX)?.unwrap_or(0);
    let requested_limit = match args.get("content_limit") {
        Some(value) => {
            let Some(integer) = value.as_i64() else {
                return Err(argument_error("content_limit must be an integer"));
            };
            if integer <= 0 {
                return Err(argument_error("content_limit must be >= 1"));
            }
            usize::try_from(integer).map_err(|_| {
                argument_error(format!(
                    "content_limit must be <= {MAX_LCM_LOAD_CONTENT_LIMIT}"
                ))
            })?
        }
        None => DEFAULT_LCM_CONTENT_LIMIT,
    };
    let limit = requested_limit.min(MAX_LCM_LOAD_CONTENT_LIMIT);
    let clamped_from = (requested_limit > limit).then_some(requested_limit);
    Ok((LcmContentSlice { offset, limit }, clamped_from))
}

fn lcm_doctor_mode(args: &Value) -> Result<&str> {
    let mode = string_arg(args, "mode").unwrap_or("diagnose");
    match mode {
        "diagnose" | "repair" | "retention" | "clean" => Ok(mode),
        _ => Err(argument_error(
            "mode must be one of diagnose, repair, retention, clean",
        )),
    }
}

fn lcm_doctor_clean_apply_enabled(args: &Value) -> Result<bool> {
    match args.get("doctor_clean_apply_enabled") {
        Some(value) => value
            .as_bool()
            .ok_or_else(|| argument_error("doctor_clean_apply_enabled must be a boolean")),
        None => Ok(crate::global_db::env_flag("LCM_DOCTOR_CLEAN_APPLY_ENABLED")),
    }
}

fn lcm_clean_config(args: &Value) -> Result<LcmCleanConfig> {
    Ok(LcmCleanConfig {
        ignore_session_patterns: string_array_arg(args, "ignore_session_patterns")?,
        stateless_session_patterns: string_array_arg(args, "stateless_session_patterns")?,
        ignore_message_patterns: string_array_arg(args, "ignore_message_patterns")?,
    })
}

// By-value so it can be used point-free as a `map_err` adapter.
#[allow(clippy::needless_pass_by_value)]
fn lcm_error(err: crate::sessions::lcm::LcmError) -> TraceDecayError {
    TraceDecayError::Config {
        message: err.to_string(),
    }
}

fn lcm_unavailable() -> ToolResult {
    tool_json(
        None,
        &json!({
            "status": "unavailable",
            "message": "could not open project-local tracedecay session database",
        }),
    )
}

/// Returned by pure-read tools when the sessions.db file has not been
/// created yet (nothing has been ingested). Distinct from "unavailable"
/// so callers can tell "no data yet" apart from "open failed".
/// The `store_exists: false` field is the machine-readable discriminator;
/// other fields are backward-compatible additions.
fn lcm_not_yet_ingested(storage_scope: &str) -> ToolResult {
    tool_json(
        None,
        &json!({
            "status": "not_ingested",
            "store_exists": false,
            "storage_scope": storage_scope,
            "message": "session store does not exist yet — nothing has been ingested",
        }),
    )
}

fn lcm_scoped_unavailable(storage_scope: &str, message: impl Into<String>) -> ToolResult {
    tool_json(
        None,
        &json!({
            "status": "unavailable",
            "storage_scope": storage_scope,
            "message": message.into(),
        }),
    )
}

fn lcm_storage_scope_unavailable(storage_scope: &str) -> ToolResult {
    lcm_scoped_unavailable(
        storage_scope,
        format!(
            "{storage_scope} LCM status storage is not available from the project-local handler"
        ),
    )
}

fn project_local_storage_without_project() -> ToolResult {
    lcm_scoped_unavailable(
        "project_local",
        "project_local LCM storage requires an initialized TraceDecay project root",
    )
}

struct LcmStorage {
    db: GlobalDb,
    scope: &'static str,
}

/// Database paths whose schema (sessions DDL + LCM migrations) has already
/// been ensured by this process. In `tracedecay serve`, every LCM tool call
/// re-opens the project session DB; once `GlobalDb::open_at` has ensured the
/// schema for a path, later opens skip the DDL batch and the LCM version
/// gate entirely via `open_at_assuming_schema`. The schema only ever grows
/// and lives in the file itself, so a concurrent process cannot invalidate
/// the flag; the `is_file` check below covers the file being deleted
/// underneath a long-lived server. One-shot CLI invocations open each path
/// once, so their behavior is unchanged.
///
/// Connections are deliberately NOT cached: each call still opens a fresh
/// libsql local connection. Sharing a long-lived handle across tool calls
/// would have to prove cross-process WAL safety and stale-handle recovery
/// (other processes checkpoint and write the same file), which is not worth
/// the risk for a per-call open that is cheap once the DDL is skipped.
static ENSURED_SCHEMA_DB_PATHS: LazyLock<Mutex<HashSet<PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn schema_already_ensured(db_path: &Path) -> bool {
    db_path.is_file()
        && ENSURED_SCHEMA_DB_PATHS
            .lock()
            .is_ok_and(|paths| paths.contains(db_path))
}

fn mark_schema_ensured(db_path: &Path) {
    if let Ok(mut paths) = ENSURED_SCHEMA_DB_PATHS.lock() {
        paths.insert(db_path.to_path_buf());
    }
}

/// Opens a writable session DB, ensuring the schema at most once per
/// process per path (see [`ENSURED_SCHEMA_DB_PATHS`]).
async fn open_session_db_with_cached_ensure(db_path: &Path) -> Option<GlobalDb> {
    if schema_already_ensured(db_path) {
        if let Some(db) = GlobalDb::open_at_assuming_schema(db_path).await {
            return Some(db);
        }
        // Fast path failed (e.g. file replaced mid-session): fall through to
        // a full ensure rather than failing the tool call.
    }
    let db = GlobalDb::open_at(db_path).await?;
    mark_schema_ensured(db_path);
    Some(db)
}

enum LcmStorageResolution {
    Available(Box<LcmStorage>),
    Unavailable(ToolResult),
}

fn invalid_hermes_profile_home(message: impl Into<String>) -> ToolResult {
    lcm_scoped_unavailable("hermes_profile", message)
}

fn hermes_profile_home(args: &Value) -> std::result::Result<PathBuf, ToolResult> {
    let Some(hermes_home) = string_arg(args, "hermes_home") else {
        return Err(invalid_hermes_profile_home(
            "hermes_profile LCM storage requires an explicit absolute hermes_home",
        ));
    };
    let path = PathBuf::from(hermes_home);
    if !path.is_absolute() {
        return Err(invalid_hermes_profile_home(
            "hermes_profile LCM storage requires an absolute hermes_home",
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(invalid_hermes_profile_home(
            "hermes_profile LCM storage requires a normalized absolute hermes_home",
        ));
    }
    let Ok(canonical) = std::fs::canonicalize(&path) else {
        return Err(invalid_hermes_profile_home(format!(
            "hermes_home does not exist or is not a directory: {}",
            path.display()
        )));
    };
    if !canonical.is_dir() {
        return Err(invalid_hermes_profile_home(format!(
            "hermes_home does not exist or is not a directory: {}",
            path.display()
        )));
    }
    Ok(canonical)
}

/// How an LCM storage open treats the backing sessions.db.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LcmOpenMode {
    /// Writable open: creates the store and ensures schema as needed.
    Writable,
    /// Read-only: a missing store is a hard error.
    ReadOnlyExisting,
    /// Read-only: a missing store is a distinguishable `not_ingested`
    /// result, without creating the file. Use this for every `readOnlyHint`
    /// LCM handler so "nothing ingested yet" never looks like "ok, 0 rows"
    /// (and the tool never ghost-creates an empty sessions.db).
    ReadOnlyOrMissing,
}

macro_rules! lcm_open_storage {
    ($project_root:expr, $args:expr) => {
        match open_lcm_storage($project_root, $args, LcmOpenMode::Writable).await {
            LcmStorageResolution::Available(storage) => storage,
            LcmStorageResolution::Unavailable(result) => return Ok(result),
        }
    };
}

/// Like `lcm_open_storage!` but with [`LcmOpenMode::ReadOnlyOrMissing`]
/// semantics for `readOnlyHint` handlers.
macro_rules! lcm_open_storage_ro {
    ($project_root:expr, $args:expr) => {
        match open_lcm_storage($project_root, $args, LcmOpenMode::ReadOnlyOrMissing).await {
            LcmStorageResolution::Available(storage) => storage,
            LcmStorageResolution::Unavailable(result) => return Ok(result),
        }
    };
}

async fn open_lcm_storage(
    project_root: Option<&Path>,
    args: &Value,
    mode: LcmOpenMode,
) -> LcmStorageResolution {
    let storage_scope = string_arg(args, "storage_scope").unwrap_or("project_local");
    match storage_scope {
        "project_local" => {
            let Some(project_root) = project_root else {
                return LcmStorageResolution::Unavailable(project_local_storage_without_project());
            };
            let db_path = crate::sessions::cursor::project_session_db_path(project_root);
            let db = match mode {
                LcmOpenMode::Writable => open_session_db_with_cached_ensure(&db_path).await,
                LcmOpenMode::ReadOnlyExisting => GlobalDb::open_read_only_at(&db_path).await,
                LcmOpenMode::ReadOnlyOrMissing => {
                    if !db_path.is_file() {
                        return LcmStorageResolution::Unavailable(lcm_not_yet_ingested(
                            "project_local",
                        ));
                    }
                    GlobalDb::open_read_only_at(&db_path).await
                }
            };
            let Some(db) = db else {
                return LcmStorageResolution::Unavailable(lcm_unavailable());
            };
            LcmStorageResolution::Available(Box::new(LcmStorage {
                db,
                scope: "project_local",
            }))
        }
        "hermes_profile" => {
            let hermes_home = match hermes_profile_home(args) {
                Ok(hermes_home) => hermes_home,
                Err(result) => return LcmStorageResolution::Unavailable(result),
            };
            let db_path = match mode {
                LcmOpenMode::Writable => {
                    match crate::sessions::cursor::resolve_hermes_profile_session_db_path(
                        &hermes_home,
                    ) {
                        Ok(db_path) => db_path,
                        Err(message) => {
                            return LcmStorageResolution::Unavailable(invalid_hermes_profile_home(
                                message,
                            ))
                        }
                    }
                }
                LcmOpenMode::ReadOnlyExisting | LcmOpenMode::ReadOnlyOrMissing => {
                    match crate::sessions::cursor::resolve_hermes_profile_session_db_readonly(
                        &hermes_home,
                    ) {
                        HermesProfileDbReadOnly::Exists(db_path) => db_path,
                        HermesProfileDbReadOnly::NotIngested(db_path) => {
                            return LcmStorageResolution::Unavailable(match mode {
                                LcmOpenMode::ReadOnlyOrMissing => {
                                    lcm_not_yet_ingested("hermes_profile")
                                }
                                _ => invalid_hermes_profile_home(format!(
                                    "hermes_profile LCM storage requires an existing session database: {}",
                                    db_path.display()
                                )),
                            })
                        }
                        HermesProfileDbReadOnly::ConfigError(msg) => {
                            return LcmStorageResolution::Unavailable(invalid_hermes_profile_home(
                                msg,
                            ))
                        }
                    }
                }
            };
            let db = match mode {
                LcmOpenMode::Writable => open_session_db_with_cached_ensure(&db_path).await,
                LcmOpenMode::ReadOnlyExisting | LcmOpenMode::ReadOnlyOrMissing => {
                    GlobalDb::open_read_only_at(&db_path).await
                }
            };
            let Some(db) = db else {
                return LcmStorageResolution::Unavailable(invalid_hermes_profile_home(
                    "could not open hermes_profile tracedecay session database",
                ));
            };
            LcmStorageResolution::Available(Box::new(LcmStorage {
                db,
                scope: "hermes_profile",
            }))
        }
        other => LcmStorageResolution::Unavailable(lcm_storage_scope_unavailable(other)),
    }
}

fn parse_lcm_scope(args: &Value) -> Result<LcmScope> {
    let Some(value) = args.get("scope") else {
        return Ok(LcmScope::All);
    };
    let Some(scope) = value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err(argument_error("scope must be one of current, session, all"));
    };
    match scope {
        "current" => Ok(LcmScope::Current),
        "session" => Ok(LcmScope::Session),
        "all" => Ok(LcmScope::All),
        _ => Err(argument_error("scope must be one of current, session, all")),
    }
}

fn parse_lcm_grep_sort(args: &Value) -> Result<LcmGrepSort> {
    let Some(sort) = string_arg(args, "sort") else {
        return Ok(LcmGrepSort::Recency);
    };
    sort.parse::<LcmGrepSort>()
        .map_err(|()| argument_error("sort must be one of recency, relevance, hybrid"))
}

fn parse_lcm_summary_node_id(target: &Value) -> Result<String> {
    required_string_arg(target, "node_id")
        .map(str::to_string)
        .map_err(|_| TraceDecayError::Config {
            message: "target.node_id is required when target.kind is summary_node".to_string(),
        })
}

fn parse_lcm_external_payload_ref(target: &Value) -> Result<String> {
    required_string_arg(target, "payload_ref")
        .map(str::to_string)
        .map_err(|_| TraceDecayError::Config {
            message: "target.payload_ref is required when target.kind is external_payload"
                .to_string(),
        })
}

fn parse_lcm_describe_target(args: &Value) -> Result<LcmDescribeTarget> {
    let Some(target) = args.get("target") else {
        return Ok(LcmDescribeTarget::Session);
    };
    match string_arg(target, "kind").unwrap_or_default() {
        "summary_node" => Ok(LcmDescribeTarget::SummaryNode {
            node_id: parse_lcm_summary_node_id(target)?,
        }),
        "external_payload" => Ok(LcmDescribeTarget::ExternalPayload {
            payload_ref: parse_lcm_external_payload_ref(target)?,
        }),
        "session" => Ok(LcmDescribeTarget::Session),
        _ => Err(TraceDecayError::Config {
            message: "target.kind must be one of session, summary_node, external_payload"
                .to_string(),
        }),
    }
}

fn parse_lcm_expand_target(args: &Value) -> Result<LcmExpandTarget> {
    let target = args.get("target").ok_or_else(|| TraceDecayError::Config {
        message: "missing required parameter: target".to_string(),
    })?;
    match string_arg(target, "kind").unwrap_or_default() {
        "raw_message" => {
            let store_id = non_negative_i64_arg(target, "store_id")?.ok_or_else(|| {
                TraceDecayError::Config {
                    message: "target.store_id is required when target.kind is raw_message"
                        .to_string(),
                }
            })?;
            Ok(LcmExpandTarget::RawMessage { store_id })
        }
        "summary_node" => Ok(LcmExpandTarget::SummaryNode {
            node_id: parse_lcm_summary_node_id(target)?,
        }),
        "external_payload" => Ok(LcmExpandTarget::ExternalPayload {
            payload_ref: parse_lcm_external_payload_ref(target)?,
        }),
        _ => Err(TraceDecayError::Config {
            message: "target.kind must be one of raw_message, summary_node, external_payload"
                .to_string(),
        }),
    }
}

/// Parses the `scope` argument for `tracedecay_message_search`. Like
/// [`parse_lcm_scope`], invalid values are a hard error naming the valid set —
/// never silently broadened to `all`.
fn parse_message_search_scope(args: &Value) -> Result<SessionSearchScope> {
    let Some(value) = args.get("scope") else {
        return Ok(SessionSearchScope::All);
    };
    match value.as_str().map(str::trim) {
        Some("all") => Ok(SessionSearchScope::All),
        Some("parents_only") => Ok(SessionSearchScope::ParentsOnly),
        Some("subagents_only") => Ok(SessionSearchScope::SubagentsOnly),
        _ => Err(argument_error(
            "scope must be one of all, parents_only, subagents_only",
        )),
    }
}

pub(super) async fn handle_message_search(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: query".to_string(),
        })?;
    let provider = args
        .get("provider")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .unwrap_or("cursor");
    let project_key = args
        .get("project_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|project_key| !project_key.is_empty());
    let parent_session_id = args
        .get("parent_session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|parent_session_id| !parent_session_id.is_empty());
    let include_subagents = args
        .get("include_subagents")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let mut scope = parse_message_search_scope(&args)?;
    if !include_subagents && matches!(scope, SessionSearchScope::All) {
        scope = SessionSearchScope::ParentsOnly;
    }
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(10)
        .clamp(1, 50) as usize;

    let db_path = crate::sessions::cursor::project_session_db_path(cg.project_root());
    let Some(db) = open_session_db_with_cached_ensure(&db_path).await else {
        return Ok(tool_json(
            Some(cg.project_root()),
            &json!({
                "status": "unavailable",
                "message": "could not open project-local tracedecay session database",
                "results": [],
                "count": 0
            }),
        ));
    };
    if provider == "hermes" {
        // Hermes history lives in per-profile state.db stores normally swept
        // by the serve/dashboard startup catch-ups; an incremental
        // search-time catch-up makes the `tracedecay tool` / generated-plugin
        // path self-sufficient (cursor-based, so it is cheap when fresh).
        let _ = crate::sessions::hermes::ingest_for_project(&db, cg.project_root()).await;
    }
    let results = db
        .search_session_messages_filtered(
            provider,
            project_key,
            query,
            limit,
            scope,
            parent_session_id,
        )
        .await;

    Ok(tool_json(
        Some(cg.project_root()),
        &json!({
            "status": "ok",
            "provider": provider,
            "project_key": project_key,
            "parent_session_id": parent_session_id,
            "include_subagents": include_subagents,
            "scope": match scope {
                SessionSearchScope::All => "all",
                SessionSearchScope::ParentsOnly => "parents_only",
                SessionSearchScope::SubagentsOnly => "subagents_only",
            },
            "query": query,
            "count": results.len(),
            "results": results,
        }),
    ))
}

pub(super) async fn handle_lcm_status(
    project_root: Option<&Path>,
    args: Value,
) -> Result<ToolResult> {
    let provider = provider_arg(&args);
    let session_id = string_arg(&args, "session_id");
    let storage = lcm_open_storage_ro!(project_root, &args);
    let mut status = storage
        .db
        .lcm_status(provider, session_id)
        .await
        .map_err(lcm_error)?;
    status.storage_scope = Some(storage.scope.to_string());
    Ok(tool_json(
        project_root,
        &json!({
            "status": "ok",
            "provider": provider,
            "session_id": session_id,
            "lcm": status,
        }),
    ))
}

pub(super) async fn handle_lcm_doctor(
    project_root: Option<&Path>,
    args: Value,
) -> Result<ToolResult> {
    let provider = provider_arg(&args);
    let session_id = string_arg(&args, "session_id");
    let mode = lcm_doctor_mode(&args)?;
    let apply = args.get("apply").and_then(Value::as_bool).unwrap_or(false);
    let clean_apply_enabled = lcm_doctor_clean_apply_enabled(&args)?;
    if mode == "clean" && apply && !clean_apply_enabled {
        return Ok(tool_json(
            project_root,
            &json!({
                "status": "denied",
                "provider": provider,
                "session_id": session_id,
                "mode": mode,
                "dry_run": false,
                "apply": true,
                "error": "destructive cleanup is disabled by default",
                "note": "set LCM_DOCTOR_CLEAN_APPLY_ENABLED=true only in trusted operator environments",
                "repairs": {
                    "planned_actions": [],
                    "applied_actions": [],
                    "backup": Value::Null,
                    "unsafe_actions_skipped": [
                        {
                            "kind": "clean_lcm_noise",
                            "safe": false,
                            "reason": "doctor_clean_apply_disabled"
                        }
                    ]
                }
            }),
        ));
    }
    let clean_config = lcm_clean_config(&args)?;
    let open_mode = if matches!(mode, "repair" | "clean") && apply {
        LcmOpenMode::Writable
    } else {
        LcmOpenMode::ReadOnlyExisting
    };
    let storage = match open_lcm_storage(project_root, &args, open_mode).await {
        LcmStorageResolution::Available(storage) => storage,
        LcmStorageResolution::Unavailable(result) => return Ok(result),
    };
    let mut payload = storage
        .db
        .lcm_doctor(provider, session_id, mode, apply, clean_config)
        .await
        .map_err(lcm_error)?;
    if let Some(object) = payload.as_object_mut() {
        object.insert("storage_scope".to_string(), json!(storage.scope));
    }
    Ok(tool_json(project_root, &payload))
}

pub(super) async fn handle_lcm_load_session(
    project_root: Option<&Path>,
    args: Value,
) -> Result<ToolResult> {
    let provider = provider_arg(&args);
    let session_id = required_string_arg(&args, "session_id")?;
    let (content_slice, content_limit_clamped_from) = lcm_load_content_slice(&args)?;
    let storage = lcm_open_storage_ro!(project_root, &args);
    let page = storage
        .db
        .lcm_load_session(LcmLoadSessionRequest {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            after_store_id: non_negative_i64_arg(&args, "after_store_id")?,
            limit: bounded_usize_arg(&args, "limit", 1, MAX_LCM_RESULT_LIMIT)?.unwrap_or(50),
            roles: {
                let mut roles = string_array_arg(&args, "roles")?;
                if roles.is_empty() {
                    if let Some(role) = string_arg(&args, "role") {
                        roles.push(role.to_string());
                    }
                }
                roles
            },
            start_time: non_negative_i64_arg_alias(&args, "start_time", "time_from")?,
            end_time: non_negative_i64_arg_alias(&args, "end_time", "time_to")?,
            content_slice: Some(content_slice),
        })
        .await
        .map_err(lcm_error)?;
    let mut payload = json!({
        "status": "ok",
        "provider": provider,
        "session_id": session_id,
        "messages": page.messages,
        "next_cursor": page.next_cursor,
        "content_limit": content_slice.limit,
    });
    if let Some(clamped_from) = content_limit_clamped_from {
        if let Some(object) = payload.as_object_mut() {
            object.insert(
                "content_limit_clamped_from".to_string(),
                json!(clamped_from),
            );
        }
    }
    Ok(tool_json(project_root, &payload))
}

pub(super) async fn handle_lcm_grep(
    project_root: Option<&Path>,
    args: Value,
) -> Result<ToolResult> {
    let provider = provider_arg(&args);
    let query = required_string_arg(&args, "query")?;
    // Validate scope before opening storage so argument errors are reported
    // even when the sessions DB does not exist yet.
    let scope = parse_lcm_scope(&args)?;
    let storage = lcm_open_storage_ro!(project_root, &args);
    let hits = storage
        .db
        .lcm_grep(LcmGrepRequest {
            provider: provider.to_string(),
            query: query.to_string(),
            scope,
            session_id: string_arg(&args, "session_id").map(str::to_string),
            include_summaries: args
                .get("include_summaries")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            limit: bounded_usize_arg(&args, "limit", 1, MAX_LCM_RESULT_LIMIT)?.unwrap_or(10),
            sort: parse_lcm_grep_sort(&args)?,
            source: string_arg(&args, "source").map(str::to_string),
            role: string_arg(&args, "role").map(str::to_string),
            start_time: non_negative_timestamp_arg_alias(&args, "start_time", "time_from")?,
            end_time: non_negative_timestamp_arg_alias(&args, "end_time", "time_to")?,
        })
        .await
        .map_err(lcm_error)?;
    Ok(tool_json(
        project_root,
        &json!({
            "status": "ok",
            "provider": provider,
            "query": query,
            "count": hits.len(),
            "hits": hits,
            "sort": string_arg(&args, "sort").unwrap_or("recency"),
        }),
    ))
}

pub(super) async fn handle_lcm_describe(
    project_root: Option<&Path>,
    args: Value,
) -> Result<ToolResult> {
    let provider = provider_arg(&args);
    let session_id = required_string_arg(&args, "session_id")?;
    // Validate target before opening storage so argument errors are reported
    // even when the sessions DB does not exist yet.
    let target = parse_lcm_describe_target(&args)?;
    let storage = lcm_open_storage_ro!(project_root, &args);
    let description = storage
        .db
        .lcm_describe(LcmDescribeRequest {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            target,
        })
        .await
        .map_err(lcm_error)?;
    Ok(tool_json(
        project_root,
        &json!({
            "status": "ok",
            "provider": provider,
            "session_id": session_id,
            "description": description,
        }),
    ))
}

pub(super) async fn handle_lcm_expand(
    project_root: Option<&Path>,
    args: Value,
) -> Result<ToolResult> {
    let provider = provider_arg(&args);
    let session_id = required_string_arg(&args, "session_id")?;
    let target = parse_lcm_expand_target(&args)?;
    let storage = lcm_open_storage_ro!(project_root, &args);
    let expansion = storage
        .db
        .lcm_expand(LcmExpandRequest {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            target,
            content_slice: Some(lcm_content_slice(&args)?),
            source_offset: bounded_usize_arg(&args, "source_offset", 0, usize::MAX)?.unwrap_or(0),
            source_limit: bounded_usize_arg(&args, "source_limit", 1, usize::MAX)?,
        })
        .await
        .map_err(lcm_error)?;
    Ok(tool_json(
        project_root,
        &json!({
            "status": "ok",
            "provider": provider,
            "session_id": session_id,
            "expansion": expansion,
        }),
    ))
}

pub(super) async fn handle_lcm_expand_query(
    project_root: Option<&Path>,
    args: Value,
) -> Result<ToolResult> {
    let provider = provider_arg(&args);
    let session_id = required_string_arg(&args, "session_id")?;
    let prompt = required_string_arg(&args, "prompt")?;
    let max_results =
        bounded_usize_arg(&args, "max_results", 1, MAX_LCM_RESULT_LIMIT)?.unwrap_or(5);
    let max_tokens =
        bounded_usize_arg(&args, "max_tokens", 1, MAX_LCM_CONTENT_LIMIT)?.unwrap_or(2000);
    // `context_max_tokens` is the retrieval context budget (how much LCM
    // material is assembled before host synthesis). It is orthogonal to
    // `max_tokens` (the synthesis *output* budget): max_tokens ≤ 8 192
    // while context_max_tokens lives in [32 000, 65 536], so a clamp of
    // the form `max_tokens.clamp(32_000, 65_536)` always evaluates to
    // 32_000 — making max_tokens dead. The default is therefore a fixed
    // constant; pass `context_max_tokens` explicitly when a larger budget
    // is wanted.
    let context_max_tokens = bounded_usize_arg(
        &args,
        "context_max_tokens",
        1,
        MAX_LCM_EXPAND_QUERY_CONTEXT_LIMIT,
    )?
    .unwrap_or(DEFAULT_LCM_EXPAND_QUERY_CONTEXT_LIMIT);
    let storage = lcm_open_storage_ro!(project_root, &args);
    let response = storage
        .db
        .lcm_expand_query(LcmExpandQueryRequest {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            prompt: prompt.to_string(),
            query: string_arg(&args, "query").map(str::to_string),
            node_ids: string_array_arg(&args, "node_ids")?,
            max_results,
            max_tokens,
            context_max_tokens,
        })
        .await
        .map_err(lcm_error)?;
    let mut payload = serde_json::to_value(response).map_err(|err| TraceDecayError::Config {
        message: format!("failed to serialize expand-query response: {err}"),
    })?;
    if let Some(object) = payload.as_object_mut() {
        object.insert("status".to_string(), json!("ok"));
        object.insert("provider".to_string(), json!(provider));
        object.insert("session_id".to_string(), json!(session_id));
        object.insert("storage_scope".to_string(), json!(storage.scope));
    }
    Ok(lcm_expand_query_tool_json(project_root, &payload))
}

pub(super) async fn handle_lcm_session_boundary(
    project_root: Option<&Path>,
    args: Value,
) -> Result<ToolResult> {
    let provider = provider_arg(&args);
    let session_id = required_string_arg(&args, "session_id")?;
    let storage = lcm_open_storage!(project_root, &args);
    let response = storage
        .db
        .lcm_session_boundary(LcmSessionBoundaryRequest {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            old_session_id: string_arg(&args, "old_session_id").map(str::to_string),
            boundary_reason: string_arg(&args, "boundary_reason").map(str::to_string),
            bound_session_id: string_arg(&args, "bound_session_id").map(str::to_string),
            boundary_skip_at: None,
        })
        .await
        .map_err(lcm_error)?;
    Ok(tool_json(
        project_root,
        &json!({
            "status": response.status,
            "provider": provider,
            "session_id": session_id,
            "recorded": response.recorded,
            "reason": response.reason,
        }),
    ))
}

pub(super) async fn handle_lcm_preflight(
    project_root: Option<&Path>,
    args: Value,
) -> Result<ToolResult> {
    let provider = provider_arg(&args);
    let session_id = required_string_arg(&args, "session_id")?;
    let storage = lcm_open_storage!(project_root, &args);
    let response = storage
        .db
        .lcm_preflight(LcmPreflightRequest {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            messages: messages_arg(&args)?,
            current_tokens: non_negative_i64_arg(&args, "current_tokens")?,
            threshold_tokens: non_negative_i64_arg(&args, "threshold_tokens")?,
            max_assembly_tokens: non_negative_i64_arg(&args, "max_assembly_tokens")?,
            leaf_chunk_tokens: non_negative_i64_arg(&args, "leaf_chunk_tokens")?,
            max_source_messages: bounded_usize_arg(&args, "max_source_messages", 1, usize::MAX)?,
            summary_fan_in: bounded_usize_arg(&args, "summary_fan_in", 2, usize::MAX)?,
            incremental_max_depth: signed_i64_arg(&args, "incremental_max_depth")?,
            fresh_tail_count: bounded_usize_arg(&args, "fresh_tail_count", 0, usize::MAX)?,
            dynamic_leaf_chunk_enabled: bool_arg(&args, "dynamic_leaf_chunk_enabled")?,
            dynamic_leaf_chunk_max: non_negative_i64_arg(&args, "dynamic_leaf_chunk_max")?,
            context_length: non_negative_i64_arg(&args, "context_length")?,
            reserve_tokens_floor: non_negative_i64_arg(&args, "reserve_tokens_floor")?,
            ignore_session_patterns: string_array_arg(&args, "ignore_session_patterns")?,
            stateless_session_patterns: string_array_arg(&args, "stateless_session_patterns")?,
            ignore_message_patterns: string_array_arg(&args, "ignore_message_patterns")?,
        })
        .await
        .map_err(lcm_error)?;
    Ok(lcm_preflight_tool_json(&json!({
        "status": response.status,
        "provider": provider,
        "session_id": session_id,
        "should_compress": response.should_compress,
        "reason": response.reason,
        "replay_messages": response.replay_messages,
    })))
}

pub(super) async fn handle_lcm_compress(
    project_root: Option<&Path>,
    args: Value,
) -> Result<ToolResult> {
    let provider = provider_arg(&args);
    let session_id = required_string_arg(&args, "session_id")?;
    let storage = lcm_open_storage!(project_root, &args);
    let response = storage
        .db
        .lcm_compress(LcmCompressionRequest {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            messages: messages_arg(&args)?,
            current_tokens: non_negative_i64_arg(&args, "current_tokens")?,
            focus_topic: string_arg(&args, "focus_topic").map(str::to_string),
            ignore_session_patterns: string_array_arg(&args, "ignore_session_patterns")?,
            stateless_session_patterns: string_array_arg(&args, "stateless_session_patterns")?,
            ignore_message_patterns: string_array_arg(&args, "ignore_message_patterns")?,
            expected_current_frontier_store_id: non_negative_i64_arg(
                &args,
                "expected_current_frontier_store_id",
            )?,
            threshold_tokens: non_negative_i64_arg(&args, "threshold_tokens")?,
            max_assembly_tokens: non_negative_i64_arg(&args, "max_assembly_tokens")?,
            leaf_chunk_tokens: non_negative_i64_arg(&args, "leaf_chunk_tokens")?,
            max_source_messages: bounded_usize_arg(&args, "max_source_messages", 1, usize::MAX)?,
            summary_fan_in: bounded_usize_arg(&args, "summary_fan_in", 2, usize::MAX)?,
            incremental_max_depth: signed_i64_arg(&args, "incremental_max_depth")?,
            fresh_tail_count: bounded_usize_arg(&args, "fresh_tail_count", 0, usize::MAX)?,
            dynamic_leaf_chunk_enabled: bool_arg(&args, "dynamic_leaf_chunk_enabled")?,
            dynamic_leaf_chunk_max: non_negative_i64_arg(&args, "dynamic_leaf_chunk_max")?,
            context_length: non_negative_i64_arg(&args, "context_length")?,
            reserve_tokens_floor: non_negative_i64_arg(&args, "reserve_tokens_floor")?,
            summarizer: summarizer_arg(&args)?,
        })
        .await
        .map_err(lcm_error)?;
    Ok(lcm_compress_tool_json(
        project_root,
        &json!({
            "status": response.status,
            "provider": provider,
            "session_id": session_id,
            "reason": response.reason,
            "summary_nodes_created": response.summary_nodes_created,
            "summary_nodes": response.summary_nodes,
            "replay_messages": response.replay_messages,
            "replay_token_estimate": response.replay_token_estimate,
            "replay_over_budget": response.replay_over_budget,
            "compression_attempts": response.compression_attempts,
            "fallback_used": response.fallback_used,
            "retry_status": response.retry_status,
            "frontier": response.frontier,
            "summary_request": response.summary_request,
        }),
    ))
}
