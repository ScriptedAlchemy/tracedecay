use serde_json::{json, Value};

use crate::global_db::{AnalyticsEventInsert, GlobalDb};

pub(super) struct McpToolAnalyticsEvent<'a> {
    pub(super) project_root: &'a std::path::Path,
    pub(super) session_id: Option<String>,
    pub(super) tool_name: &'a str,
    pub(super) outcome: &'a str,
    pub(super) raw_file_tokens: u64,
    pub(super) response_tokens: u64,
    pub(super) net_saved_tokens: u64,
    pub(super) timestamp: i64,
    pub(super) request_id: &'a Value,
    pub(super) arguments: &'a Value,
    pub(super) response: Option<&'a Value>,
}

pub(super) fn mcp_tool_analytics_event(input: McpToolAnalyticsEvent<'_>) -> AnalyticsEventInsert {
    let category = crate::accounting::classifier::classify(&[input.tool_name], &[]);
    let mut metadata = json!({
        "request_id": input.request_id,
        "transport": "mcp",
        "tool_kind": "mcp_tool",
        "before_tokens": input.raw_file_tokens,
        "after_tokens": input.response_tokens,
        "tokens_saved": input.net_saved_tokens,
    });
    if input.outcome == "error" {
        metadata["failure_reason"] = json!("tool_dispatch_error");
    }
    if crate::analytics::is_skill_view_tool(input.tool_name) {
        metadata["arguments"] = input.arguments.clone();
        metadata["function"] = json!({
            "name": input.tool_name,
            "arguments": input.arguments,
        });
    }
    append_tool_response_analytics(
        input.tool_name,
        input.arguments,
        input.response,
        &mut metadata,
    );
    AnalyticsEventInsert {
        provider: "mcp".to_string(),
        project_id: GlobalDb::canonical_project_key(input.project_root),
        session_id: input.session_id,
        timestamp: input.timestamp,
        event_kind: "mcp_tool_call".to_string(),
        hook_name: None,
        tool_name: Some(input.tool_name.to_string()),
        tool_category: Some(category.as_str().to_string()),
        skill_name: None,
        hint_category: None,
        hint_id: None,
        outcome: Some(input.outcome.to_string()),
        metadata_json: Some(metadata.to_string()),
    }
}

fn append_tool_response_analytics(
    tool_name: &str,
    arguments: &Value,
    response: Option<&Value>,
    metadata: &mut Value,
) {
    if tool_name != "tracedecay_context" {
        return;
    }
    let include_memory = arguments
        .get("include_memory")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let limit = arguments
        .get("memory_limit")
        .and_then(Value::as_u64)
        .unwrap_or(3)
        .clamp(1, 10);
    let min_trust = arguments
        .get("memory_min_trust")
        .and_then(Value::as_f64)
        .unwrap_or(0.5)
        .clamp(0.0, 1.0);
    let payload = response.and_then(tool_result_json_payload);
    let memory_matches = payload
        .as_ref()
        .and_then(|payload| payload.get("memory_matches"))
        .and_then(Value::as_array);
    let fact_ids: Vec<Value> = memory_matches
        .into_iter()
        .flatten()
        .filter_map(|hit| {
            hit.get("fact")
                .and_then(|fact| fact.get("fact_id"))
                .and_then(Value::as_i64)
        })
        .map(Value::from)
        .collect();
    let match_count = fact_ids.len();
    let memory_error = payload
        .as_ref()
        .and_then(|payload| payload.get("memory_matches_error"))
        .and_then(Value::as_str);
    metadata["context_memory"] = json!({
        "include_memory": include_memory,
        "limit": limit,
        "min_trust": min_trust,
        "match_count": match_count,
        "fact_ids": fact_ids,
        "error": memory_error,
    });
}

fn tool_result_json_payload(response: &Value) -> Option<Value> {
    response
        .get("content")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .find_map(|text| serde_json::from_str::<Value>(text).ok())
}
