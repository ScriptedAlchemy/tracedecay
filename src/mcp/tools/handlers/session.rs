use serde_json::{json, Value};

use crate::errors::{Result, TokenSaveError};
use crate::mcp::tools::ToolResult;
use crate::sessions::lcm::{
    LcmContentSlice, LcmExpandRequest, LcmExpandTarget, LcmGrepRequest, LcmLoadSessionRequest,
    LcmScope,
};
use crate::sessions::SessionSearchScope;
use crate::tokensave::TokenSave;

use super::truncate_response;

const DEFAULT_LCM_CONTENT_LIMIT: usize = 4096;
const MAX_LCM_CONTENT_LIMIT: usize = 8192;

fn tool_json(value: &Value) -> ToolResult {
    let formatted = serde_json::to_string_pretty(value).unwrap_or_default();
    ToolResult {
        value: json!({ "content": [{ "type": "text", "text": truncate_response(&formatted) }] }),
        touched_files: Vec::new(),
    }
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

fn usize_arg(args: &Value, name: &str) -> Option<usize> {
    args.get(name)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

fn i64_arg(args: &Value, name: &str) -> Option<i64> {
    args.get(name).and_then(Value::as_i64)
}

fn provider_arg(args: &Value) -> &str {
    string_arg(args, "provider").unwrap_or("cursor")
}

fn lcm_content_slice(args: &Value) -> LcmContentSlice {
    LcmContentSlice {
        offset: usize_arg(args, "content_offset").unwrap_or(0),
        limit: usize_arg(args, "content_limit")
            .unwrap_or(DEFAULT_LCM_CONTENT_LIMIT)
            .clamp(1, MAX_LCM_CONTENT_LIMIT),
    }
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

fn parse_lcm_scope(args: &Value) -> LcmScope {
    match string_arg(args, "scope").unwrap_or("all") {
        "current" => LcmScope::Current,
        "session" => LcmScope::Session,
        _ => LcmScope::All,
    }
}

fn parse_lcm_expand_target(args: &Value) -> Result<LcmExpandTarget> {
    let target = args.get("target").ok_or_else(|| TokenSaveError::Config {
        message: "missing required parameter: target".to_string(),
    })?;
    match string_arg(target, "kind").unwrap_or_default() {
        "raw_message" => {
            let store_id = i64_arg(target, "store_id").ok_or_else(|| TokenSaveError::Config {
                message: "target.store_id is required when target.kind is raw_message".to_string(),
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
    let Some(db) = crate::sessions::cursor::open_project_session_db(cg.project_root()).await else {
        return Ok(lcm_unavailable());
    };
    let status = db
        .lcm_status(provider, session_id)
        .await
        .map_err(lcm_error)?;
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
    let Some(db) = crate::sessions::cursor::open_project_session_db(cg.project_root()).await else {
        return Ok(lcm_unavailable());
    };
    let page = db
        .lcm_load_session(LcmLoadSessionRequest {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            after_store_id: i64_arg(&args, "after_store_id"),
            limit: usize_arg(&args, "limit").unwrap_or(50),
            role: string_arg(&args, "role").map(str::to_string),
            start_time: i64_arg(&args, "start_time"),
            end_time: i64_arg(&args, "end_time"),
            content_slice: Some(lcm_content_slice(&args)),
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
    let Some(db) = crate::sessions::cursor::open_project_session_db(cg.project_root()).await else {
        return Ok(lcm_unavailable());
    };
    let hits = db
        .lcm_grep(LcmGrepRequest {
            provider: provider.to_string(),
            query: query.to_string(),
            scope: parse_lcm_scope(&args),
            session_id: string_arg(&args, "session_id").map(str::to_string),
            include_summaries: args
                .get("include_summaries")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            limit: usize_arg(&args, "limit").unwrap_or(10),
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
    let Some(db) = crate::sessions::cursor::open_project_session_db(cg.project_root()).await else {
        return Ok(lcm_unavailable());
    };
    let description = db
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
    let Some(db) = crate::sessions::cursor::open_project_session_db(cg.project_root()).await else {
        return Ok(lcm_unavailable());
    };
    let expansion = db
        .lcm_expand(LcmExpandRequest {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            target,
            content_slice: Some(lcm_content_slice(&args)),
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

pub(super) async fn handle_lcm_expand_query(_cg: &TokenSave, _args: Value) -> Result<ToolResult> {
    Ok(tool_json(&json!({
        "status": "not_implemented",
        "message": "tokensave_lcm_expand_query is registered, but synthesized expansion answers require the later Hermes/LLM bridge task.",
    })))
}

pub(super) async fn handle_lcm_preflight(_cg: &TokenSave, _args: Value) -> Result<ToolResult> {
    Ok(tool_json(&json!({
        "status": "not_implemented",
        "message": "tokensave_lcm_preflight is registered, but compression lifecycle preflight is implemented in a later task.",
    })))
}

pub(super) async fn handle_lcm_compress(_cg: &TokenSave, _args: Value) -> Result<ToolResult> {
    Ok(tool_json(&json!({
        "status": "not_implemented",
        "message": "tokensave_lcm_compress is registered, but LCM compression is implemented in a later task.",
    })))
}
