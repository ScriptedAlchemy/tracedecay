use serde_json::{json, Value};

use crate::errors::{Result, TokenSaveError};
use crate::mcp::tools::ToolResult;
use crate::sessions::SessionSearchScope;
use crate::tokensave::TokenSave;

use super::truncate_response;

fn tool_json(value: &Value) -> ToolResult {
    let formatted = serde_json::to_string_pretty(value).unwrap_or_default();
    ToolResult {
        value: json!({ "content": [{ "type": "text", "text": truncate_response(&formatted) }] }),
        touched_files: Vec::new(),
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
