use std::path::{Component, PathBuf};

use serde_json::{json, Map, Value};

use crate::errors::{Result, TokenSaveError};
use crate::global_db::GlobalDb;
use crate::mcp::tools::{ToolResult, MAX_RESPONSE_CHARS};
use crate::sessions::lcm::{
    LcmCompressionRequest, LcmContentSlice, LcmExpandQueryRequest, LcmExpandRequest,
    LcmExpandTarget, LcmGrepRequest, LcmLoadSessionRequest, LcmPreflightRequest, LcmScope,
    LcmSummarizerMode,
};
use crate::sessions::SessionSearchScope;
use crate::tokensave::TokenSave;

const DEFAULT_LCM_CONTENT_LIMIT: usize = 4096;
const DEFAULT_LCM_EXPAND_QUERY_CONTEXT_LIMIT: usize = 32_000;
const MAX_LCM_EXPAND_QUERY_CONTEXT_LIMIT: usize = 65_536;
const MAX_LCM_CONTENT_LIMIT: usize = 8192;
const MAX_LCM_RESULT_LIMIT: usize = 100;
const MAX_LCM_EXPAND_QUERY_PROMPT_CHARS: usize = 2_048;
const MAX_LCM_EXPAND_QUERY_QUERY_CHARS: usize = 1_024;
const MAX_LCM_EXPAND_QUERY_SYNTHESIS_SYSTEM_CHARS: usize = 1_024;
const MAX_LCM_EXPAND_QUERY_SYNTHESIS_PROMPT_CHARS: usize = 2_048;

fn tool_json(value: &Value) -> ToolResult {
    let formatted = serde_json::to_string_pretty(value).unwrap_or_default();
    let text = if formatted.len() <= MAX_RESPONSE_CHARS {
        formatted
    } else {
        truncated_json_envelope(&formatted)
    };
    ToolResult {
        value: json!({ "content": [{ "type": "text", "text": text }] }),
        touched_files: Vec::new(),
    }
}

fn truncated_json_envelope(formatted: &str) -> String {
    let mut end = formatted.len().min(MAX_RESPONSE_CHARS.saturating_sub(1024));
    loop {
        while end > 0 && !formatted.is_char_boundary(end) {
            end -= 1;
        }
        let preview = &formatted[..end];
        let envelope = json!({
            "truncated": true,
            "original_chars": formatted.len(),
            "preview_chars": preview.len(),
            "preview": preview,
        });
        let text = serde_json::to_string_pretty(&envelope).unwrap_or_default();
        if text.len() <= MAX_RESPONSE_CHARS || end == 0 {
            return text;
        }
        end = end.saturating_sub(1024);
    }
}

fn lcm_expand_query_tool_json(value: &Value) -> ToolResult {
    let formatted = serde_json::to_string_pretty(value).unwrap_or_default();
    let needs_synthesis = value
        .get("needs_synthesis")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let text = if formatted.len() <= MAX_RESPONSE_CHARS {
        formatted
    } else if needs_synthesis {
        let compact = compact_lcm_expand_query_payload(value, formatted.len());
        let text = serde_json::to_string_pretty(&compact).unwrap_or_default();
        if text.len() <= MAX_RESPONSE_CHARS {
            text
        } else {
            let fallback = minimal_lcm_expand_query_contract(value, formatted.len(), text.len());
            serde_json::to_string_pretty(&fallback).unwrap_or_default()
        }
    } else {
        truncated_json_envelope(&formatted)
    };
    let text = if text.len() <= MAX_RESPONSE_CHARS || needs_synthesis {
        text
    } else {
        truncated_json_envelope(&text)
    };
    ToolResult {
        value: json!({ "content": [{ "type": "text", "text": text }] }),
        touched_files: Vec::new(),
    }
}

fn compact_lcm_expand_query_payload(value: &Value, original_chars: usize) -> Value {
    const MAX_CONTEXT_BLOCKS: usize = 3;
    const MAX_CONTEXT_BLOCK_CHARS: usize = 600;
    const MAX_MATCHES: usize = 10;
    const MAX_MATCH_SNIPPET_CHARS: usize = 160;
    const MAX_NODE_IDS: usize = 50;
    const MAX_NODE_ID_CHARS: usize = 160;
    const MAX_PAGINATION_ITEMS: usize = 50;

    let mut object = Map::new();
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
    insert_bounded_text_field(
        &mut object,
        value,
        "prompt",
        MAX_LCM_EXPAND_QUERY_PROMPT_CHARS,
    );
    insert_bounded_text_field(
        &mut object,
        value,
        "query",
        MAX_LCM_EXPAND_QUERY_QUERY_CHARS,
    );
    object.insert("mcp_response_truncated".to_string(), json!(true));
    object.insert("contract_truncated".to_string(), json!(true));
    object.insert(
        "mcp_original_response_chars".to_string(),
        json!(original_chars),
    );
    object.insert(
        "mcp_truncation_reason".to_string(),
        json!("expand-query response compacted to preserve synthesis contract fields"),
    );

    let (context_blocks, context_blocks_truncated) = compact_context_blocks(
        value.get("context_blocks"),
        MAX_CONTEXT_BLOCKS,
        MAX_CONTEXT_BLOCK_CHARS,
    );
    let (matches, matches_truncated) =
        compact_matches(value.get("matches"), MAX_MATCHES, MAX_MATCH_SNIPPET_CHARS);
    let (node_ids, node_ids_truncated) =
        compact_string_array(value.get("node_ids"), MAX_NODE_IDS, MAX_NODE_ID_CHARS);
    let (context_pagination, pagination_truncated) =
        compact_array(value.get("context_pagination"), MAX_PAGINATION_ITEMS);

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
        compact_synthesis_prompt(value, &context_blocks),
    );

    Value::Object(object)
}

fn minimal_lcm_expand_query_contract(
    value: &Value,
    original_chars: usize,
    compact_chars: usize,
) -> Value {
    const MAX_CONTEXT_BLOCKS: usize = 1;
    const MAX_CONTEXT_BLOCK_CHARS: usize = 240;
    const MAX_MATCHES: usize = 5;
    const MAX_MATCH_SNIPPET_CHARS: usize = 80;
    const MAX_NODE_IDS: usize = 25;
    const MAX_NODE_ID_CHARS: usize = 120;
    const MAX_PAGINATION_ITEMS: usize = 10;
    const MAX_SCALAR_CHARS: usize = 512;
    const MAX_PROMPT_CHARS: usize = 512;
    const MAX_QUERY_CHARS: usize = 512;
    const MAX_SYNTHESIS_SYSTEM_CHARS: usize = 512;
    const MAX_SYNTHESIS_PROMPT_CHARS: usize = 512;

    let mut object = Map::new();
    for key in [
        "status",
        "provider",
        "session_id",
        "storage_scope",
        "answer",
    ] {
        insert_bounded_scalar_field(&mut object, value, key, MAX_SCALAR_CHARS);
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
    insert_bounded_text_field(&mut object, value, "prompt", MAX_PROMPT_CHARS);
    insert_bounded_text_field(&mut object, value, "query", MAX_QUERY_CHARS);

    let (context_blocks, context_blocks_truncated) = compact_context_blocks(
        value.get("context_blocks"),
        MAX_CONTEXT_BLOCKS,
        MAX_CONTEXT_BLOCK_CHARS,
    );
    let (matches, matches_truncated) =
        compact_matches(value.get("matches"), MAX_MATCHES, MAX_MATCH_SNIPPET_CHARS);
    let (node_ids, node_ids_truncated) =
        compact_string_array(value.get("node_ids"), MAX_NODE_IDS, MAX_NODE_ID_CHARS);
    let (context_pagination, pagination_truncated) =
        compact_array(value.get("context_pagination"), MAX_PAGINATION_ITEMS);

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
            MAX_SYNTHESIS_SYSTEM_CHARS,
            MAX_SYNTHESIS_PROMPT_CHARS,
        ),
    );
    object.insert("mcp_response_truncated".to_string(), json!(true));
    object.insert("contract_truncated".to_string(), json!(true));
    object.insert(
        "mcp_original_response_chars".to_string(),
        json!(original_chars),
    );
    object.insert(
        "mcp_compact_response_chars".to_string(),
        json!(compact_chars),
    );
    object.insert(
        "mcp_truncation_reason".to_string(),
        json!("expand-query response reduced to minimal synthesis contract after compact payload overflow"),
    );

    Value::Object(object)
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

fn compact_synthesis_prompt(value: &Value, context_blocks: &Value) -> Value {
    compact_synthesis_prompt_with_limits(
        value,
        context_blocks,
        MAX_LCM_EXPAND_QUERY_SYNTHESIS_SYSTEM_CHARS,
        MAX_LCM_EXPAND_QUERY_SYNTHESIS_PROMPT_CHARS,
    )
}

fn compact_synthesis_prompt_with_limits(
    value: &Value,
    context_blocks: &Value,
    system_chars: usize,
    prompt_chars: usize,
) -> Value {
    let default_system = "You answer questions using expanded LCM retrieval context. Be concise, factual, and grounded in the provided context. If the context is insufficient, say so plainly.";
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
    let mut chars = value.chars();
    let truncated = value.chars().count() > max_chars;
    let text = chars.by_ref().take(max_chars).collect::<String>();
    (text, truncated)
}

fn string_arg<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn required_string_arg<'a>(args: &'a Value, name: &str) -> Result<&'a str> {
    string_arg(args, name).ok_or_else(|| TokenSaveError::Config {
        message: format!("missing required parameter: {name}"),
    })
}

fn argument_error(message: impl Into<String>) -> TokenSaveError {
    TokenSaveError::Config {
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
    serde_json::from_value(summarizer.clone()).map_err(|err| TokenSaveError::Config {
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

fn lcm_error(err: crate::sessions::lcm::LcmError) -> TokenSaveError {
    TokenSaveError::Config {
        message: err.to_string(),
    }
}

fn lcm_unavailable() -> ToolResult {
    tool_json(&json!({
        "status": "unavailable",
        "message": "could not open project-local tokensave session database",
    }))
}

fn lcm_scoped_unavailable(storage_scope: &str, message: impl Into<String>) -> ToolResult {
    tool_json(&json!({
        "status": "unavailable",
        "storage_scope": storage_scope,
        "message": message.into(),
    }))
}

fn lcm_storage_scope_unavailable(storage_scope: &str) -> ToolResult {
    lcm_scoped_unavailable(
        storage_scope,
        format!(
            "{storage_scope} LCM status storage is not available from the project-local handler"
        ),
    )
}

struct LcmStorage {
    db: GlobalDb,
    scope: &'static str,
}

enum LcmStorageResolution {
    Available(LcmStorage),
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

async fn open_lcm_storage(cg: &TokenSave, args: &Value) -> LcmStorageResolution {
    let storage_scope = string_arg(args, "storage_scope").unwrap_or("project_local");
    match storage_scope {
        "project_local" => {
            let Some(db) =
                crate::sessions::cursor::open_project_session_db(cg.project_root()).await
            else {
                return LcmStorageResolution::Unavailable(lcm_unavailable());
            };
            LcmStorageResolution::Available(LcmStorage {
                db,
                scope: "project_local",
            })
        }
        "hermes_profile" => {
            let hermes_home = match hermes_profile_home(args) {
                Ok(hermes_home) => hermes_home,
                Err(result) => return LcmStorageResolution::Unavailable(result),
            };
            let db_path =
                match crate::sessions::cursor::resolve_hermes_profile_session_db_path(&hermes_home)
                {
                    Ok(db_path) => db_path,
                    Err(message) => {
                        return LcmStorageResolution::Unavailable(invalid_hermes_profile_home(
                            message,
                        ))
                    }
                };
            let Some(db) = GlobalDb::open_at(&db_path).await else {
                return LcmStorageResolution::Unavailable(invalid_hermes_profile_home(
                    "could not open hermes_profile tokensave session database",
                ));
            };
            LcmStorageResolution::Available(LcmStorage {
                db,
                scope: "hermes_profile",
            })
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

fn parse_lcm_expand_target(args: &Value) -> Result<LcmExpandTarget> {
    let target = args.get("target").ok_or_else(|| TokenSaveError::Config {
        message: "missing required parameter: target".to_string(),
    })?;
    match string_arg(target, "kind").unwrap_or_default() {
        "raw_message" => {
            let store_id = non_negative_i64_arg(target, "store_id")?.ok_or_else(|| {
                TokenSaveError::Config {
                    message: "target.store_id is required when target.kind is raw_message"
                        .to_string(),
                }
            })?;
            Ok(LcmExpandTarget::RawMessage { store_id })
        }
        "summary_node" => {
            let node_id = required_string_arg(target, "node_id")
                .map(str::to_string)
                .map_err(|_| TokenSaveError::Config {
                    message: "target.node_id is required when target.kind is summary_node"
                        .to_string(),
                })?;
            Ok(LcmExpandTarget::SummaryNode { node_id })
        }
        "external_payload" => {
            let payload_ref = required_string_arg(target, "payload_ref")
                .map(str::to_string)
                .map_err(|_| TokenSaveError::Config {
                    message: "target.payload_ref is required when target.kind is external_payload"
                        .to_string(),
                })?;
            Ok(LcmExpandTarget::ExternalPayload { payload_ref })
        }
        _ => Err(TokenSaveError::Config {
            message: "target.kind must be one of raw_message, summary_node, external_payload"
                .to_string(),
        }),
    }
}

pub(super) async fn handle_message_search(cg: &TokenSave, args: Value) -> Result<ToolResult> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .ok_or_else(|| TokenSaveError::Config {
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
    let mut scope = match args
        .get("scope")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("all")
    {
        "parents_only" => SessionSearchScope::ParentsOnly,
        "subagents_only" => SessionSearchScope::SubagentsOnly,
        _ => SessionSearchScope::All,
    };
    if !include_subagents && matches!(scope, SessionSearchScope::All) {
        scope = SessionSearchScope::ParentsOnly;
    }
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(10)
        .clamp(1, 50) as usize;

    let Some(db) = crate::sessions::cursor::open_project_session_db(cg.project_root()).await else {
        return Ok(tool_json(&json!({
            "status": "unavailable",
            "message": "could not open project-local tokensave session database",
            "results": [],
            "count": 0
        })));
    };
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

    Ok(tool_json(&json!({
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
    })))
}

pub(super) async fn handle_lcm_status(cg: &TokenSave, args: Value) -> Result<ToolResult> {
    let provider = provider_arg(&args);
    let session_id = string_arg(&args, "session_id");
    let storage = match open_lcm_storage(cg, &args).await {
        LcmStorageResolution::Available(storage) => storage,
        LcmStorageResolution::Unavailable(result) => return Ok(result),
    };
    let mut status = storage
        .db
        .lcm_status(provider, session_id)
        .await
        .map_err(lcm_error)?;
    status.storage_scope = Some(storage.scope.to_string());
    Ok(tool_json(&json!({
        "status": "ok",
        "provider": provider,
        "session_id": session_id,
        "lcm": status,
    })))
}

pub(super) async fn handle_lcm_load_session(cg: &TokenSave, args: Value) -> Result<ToolResult> {
    let provider = provider_arg(&args);
    let session_id = required_string_arg(&args, "session_id")?;
    let storage = match open_lcm_storage(cg, &args).await {
        LcmStorageResolution::Available(storage) => storage,
        LcmStorageResolution::Unavailable(result) => return Ok(result),
    };
    let page = storage
        .db
        .lcm_load_session(LcmLoadSessionRequest {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            after_store_id: non_negative_i64_arg(&args, "after_store_id")?,
            limit: bounded_usize_arg(&args, "limit", 1, MAX_LCM_RESULT_LIMIT)?.unwrap_or(50),
            role: string_arg(&args, "role").map(str::to_string),
            start_time: non_negative_i64_arg(&args, "start_time")?,
            end_time: non_negative_i64_arg(&args, "end_time")?,
            content_slice: Some(lcm_content_slice(&args)?),
        })
        .await
        .map_err(lcm_error)?;
    Ok(tool_json(&json!({
        "status": "ok",
        "provider": provider,
        "session_id": session_id,
        "messages": page.messages,
        "next_cursor": page.next_cursor,
    })))
}

pub(super) async fn handle_lcm_grep(cg: &TokenSave, args: Value) -> Result<ToolResult> {
    let provider = provider_arg(&args);
    let query = required_string_arg(&args, "query")?;
    let storage = match open_lcm_storage(cg, &args).await {
        LcmStorageResolution::Available(storage) => storage,
        LcmStorageResolution::Unavailable(result) => return Ok(result),
    };
    let hits = storage
        .db
        .lcm_grep(LcmGrepRequest {
            provider: provider.to_string(),
            query: query.to_string(),
            scope: parse_lcm_scope(&args)?,
            session_id: string_arg(&args, "session_id").map(str::to_string),
            include_summaries: args
                .get("include_summaries")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            limit: bounded_usize_arg(&args, "limit", 1, MAX_LCM_RESULT_LIMIT)?.unwrap_or(10),
        })
        .await
        .map_err(lcm_error)?;
    Ok(tool_json(&json!({
        "status": "ok",
        "provider": provider,
        "query": query,
        "count": hits.len(),
        "hits": hits,
    })))
}

pub(super) async fn handle_lcm_describe(cg: &TokenSave, args: Value) -> Result<ToolResult> {
    let provider = provider_arg(&args);
    let session_id = required_string_arg(&args, "session_id")?;
    let storage = match open_lcm_storage(cg, &args).await {
        LcmStorageResolution::Available(storage) => storage,
        LcmStorageResolution::Unavailable(result) => return Ok(result),
    };
    let description = storage
        .db
        .lcm_describe(provider, session_id)
        .await
        .map_err(lcm_error)?;
    Ok(tool_json(&json!({
        "status": "ok",
        "provider": provider,
        "session_id": session_id,
        "description": description,
    })))
}

pub(super) async fn handle_lcm_expand(cg: &TokenSave, args: Value) -> Result<ToolResult> {
    let provider = provider_arg(&args);
    let session_id = required_string_arg(&args, "session_id")?;
    let target = parse_lcm_expand_target(&args)?;
    let storage = match open_lcm_storage(cg, &args).await {
        LcmStorageResolution::Available(storage) => storage,
        LcmStorageResolution::Unavailable(result) => return Ok(result),
    };
    let expansion = storage
        .db
        .lcm_expand(LcmExpandRequest {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            target,
            content_slice: Some(lcm_content_slice(&args)?),
        })
        .await
        .map_err(lcm_error)?;
    Ok(tool_json(&json!({
        "status": "ok",
        "provider": provider,
        "session_id": session_id,
        "expansion": expansion,
    })))
}

pub(super) async fn handle_lcm_expand_query(cg: &TokenSave, args: Value) -> Result<ToolResult> {
    let provider = provider_arg(&args);
    let session_id = required_string_arg(&args, "session_id")?;
    let prompt = required_string_arg(&args, "prompt")?;
    let max_results =
        bounded_usize_arg(&args, "max_results", 1, MAX_LCM_RESULT_LIMIT)?.unwrap_or(5);
    let max_tokens =
        bounded_usize_arg(&args, "max_tokens", 1, MAX_LCM_CONTENT_LIMIT)?.unwrap_or(2000);
    let default_context_limit = max_tokens
        .max(DEFAULT_LCM_EXPAND_QUERY_CONTEXT_LIMIT)
        .min(MAX_LCM_EXPAND_QUERY_CONTEXT_LIMIT);
    let context_max_tokens = bounded_usize_arg(
        &args,
        "context_max_tokens",
        1,
        MAX_LCM_EXPAND_QUERY_CONTEXT_LIMIT,
    )?
    .unwrap_or(default_context_limit);
    let storage = match open_lcm_storage(cg, &args).await {
        LcmStorageResolution::Available(storage) => storage,
        LcmStorageResolution::Unavailable(result) => return Ok(result),
    };
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
    let mut payload = serde_json::to_value(response).map_err(|err| TokenSaveError::Config {
        message: format!("failed to serialize expand-query response: {err}"),
    })?;
    if let Some(object) = payload.as_object_mut() {
        object.insert("status".to_string(), json!("ok"));
        object.insert("provider".to_string(), json!(provider));
        object.insert("session_id".to_string(), json!(session_id));
        object.insert("storage_scope".to_string(), json!(storage.scope));
    }
    Ok(lcm_expand_query_tool_json(&payload))
}

pub(super) async fn handle_lcm_preflight(cg: &TokenSave, args: Value) -> Result<ToolResult> {
    let provider = provider_arg(&args);
    let session_id = required_string_arg(&args, "session_id")?;
    let storage = match open_lcm_storage(cg, &args).await {
        LcmStorageResolution::Available(storage) => storage,
        LcmStorageResolution::Unavailable(result) => return Ok(result),
    };
    let response = storage
        .db
        .lcm_preflight(LcmPreflightRequest {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            messages: messages_arg(&args)?,
            current_tokens: non_negative_i64_arg(&args, "current_tokens")?,
        })
        .await
        .map_err(lcm_error)?;
    Ok(tool_json(&json!({
        "status": response.status,
        "provider": provider,
        "session_id": session_id,
        "should_compress": response.should_compress,
        "reason": response.reason,
        "replay_messages": response.replay_messages,
    })))
}

pub(super) async fn handle_lcm_compress(cg: &TokenSave, args: Value) -> Result<ToolResult> {
    let provider = provider_arg(&args);
    let session_id = required_string_arg(&args, "session_id")?;
    let storage = match open_lcm_storage(cg, &args).await {
        LcmStorageResolution::Available(storage) => storage,
        LcmStorageResolution::Unavailable(result) => return Ok(result),
    };
    let response = storage
        .db
        .lcm_compress(LcmCompressionRequest {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            messages: messages_arg(&args)?,
            current_tokens: non_negative_i64_arg(&args, "current_tokens")?,
            focus_topic: string_arg(&args, "focus_topic").map(str::to_string),
            expected_current_frontier_store_id: non_negative_i64_arg(
                &args,
                "expected_current_frontier_store_id",
            )?,
            summarizer: summarizer_arg(&args)?,
        })
        .await
        .map_err(lcm_error)?;
    Ok(tool_json(&json!({
        "status": response.status,
        "provider": provider,
        "session_id": session_id,
        "reason": response.reason,
        "summary_nodes_created": response.summary_nodes_created,
        "summary_nodes": response.summary_nodes,
        "replay_messages": response.replay_messages,
        "frontier": response.frontier,
        "summary_request": response.summary_request,
    })))
}
