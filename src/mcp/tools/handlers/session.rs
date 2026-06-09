use std::path::{Component, PathBuf};

use serde_json::{json, Value};

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
    Ok(tool_json(&payload))
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
