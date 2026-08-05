use super::*;

pub(super) const MAX_LCM_EXPAND_QUERY_PROMPT_CHARS: usize = 2_048;
pub(super) const MAX_LCM_EXPAND_QUERY_QUERY_CHARS: usize = 1_024;
const MAX_LCM_EXPAND_QUERY_STATUS_CHARS: usize = 512;
const MAX_LCM_EXPAND_QUERY_SYNTHESIS_SYSTEM_CHARS: usize = 1_024;
const MAX_LCM_EXPAND_QUERY_SYNTHESIS_PROMPT_CHARS: usize = 2_048;

pub(super) fn lcm_response_handle_root(
    project_root: Option<&Path>,
    _args: &Value,
) -> Option<PathBuf> {
    project_root.map(Path::to_path_buf)
}

pub(super) fn lcm_expand_query_tool_json(
    project_root: Option<&Path>,
    args: &Value,
    value: &Value,
) -> ToolResult {
    if !render::wants_json(args) {
        return tool_json(project_root, args, value);
    }
    let formatted = serde_json::to_string(value).unwrap_or_default();
    let needs_synthesis = value
        .get("needs_synthesis")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let text = if formatted.len() <= MAX_RESPONSE_CHARS {
        formatted
    } else if needs_synthesis {
        let started = std::time::Instant::now();
        let compact =
            compact_lcm_expand_query_payload(value, formatted.len(), CompactTier::Standard);
        let compact_text = serde_json::to_string(&compact).unwrap_or_default();
        let (text, handle_status) = if compact_text.len() <= MAX_RESPONSE_CHARS {
            (compact_text, "compacted_no_handle")
        } else {
            let fallback = compact_lcm_expand_query_payload(
                value,
                formatted.len(),
                CompactTier::Minimal {
                    compact_chars: compact_text.len(),
                },
            );
            let fallback_text = serde_json::to_string(&fallback).unwrap_or_default();
            if fallback_text.len() <= MAX_RESPONSE_CHARS {
                (fallback_text, "compacted_no_handle")
            } else {
                // Even the Minimal tier overflowed (e.g. oversized cloned
                // pagination or match metadata). Enforce a hard floor that
                // stays valid JSON and keeps the Hermes synthesis contract
                // keys, storing the full payload behind a handle when we can.
                bounded_lcm_expand_query_floor_text(project_root, value, &formatted)
            }
        };
        // The synthesis contract path shrinks the payload in place instead of
        // going through the render-layer envelope, so record the truncation
        // explicitly. It is reversible only when the floor stored a handle.
        observe_response_truncation(
            formatted.len(),
            text.len(),
            handle_status == "stored",
            current_timestamp(),
            handle_status,
            started.elapsed(),
        );
        text
    } else {
        truncated_json_envelope_with_handle(project_root, &formatted)
    };
    // Safety net: every branch above is already bounded (the floor guarantees
    // it for needs_synthesis), but never emit an unbounded body regardless.
    let text = if text.len() <= MAX_RESPONSE_CHARS {
        text
    } else {
        truncated_json_envelope_with_handle(project_root, &text)
    };
    ToolResult::new(
        json!({ "content": [{ "type": "text", "text": text }] }),
        Vec::new(),
    )
}

/// Hard floor for a `needs_synthesis` expand-query payload that is still over
/// [`MAX_RESPONSE_CHARS`] after [`CompactTier::Minimal`] compaction. Emits a
/// bounded JSON object that preserves the Hermes bridge synthesis contract
/// (`status`, `needs_synthesis`, `synthesis_prompt`, bounded scalars) while
/// dropping the unbounded arrays (`context_blocks`, `matches`, `node_ids`,
/// `context_pagination`). When a project root is available the full original
/// payload is stored behind a retrieval handle so nothing is lost; the handle
/// is surfaced as `response_handle` (a key the Hermes plugin recognizes).
///
/// Returns the serialized text plus the telemetry handle status
/// (`"stored"` when the full payload was cached, `"compacted_no_handle"`
/// otherwise).
fn bounded_lcm_expand_query_floor_text(
    project_root: Option<&Path>,
    value: &Value,
    formatted: &str,
) -> (String, &'static str) {
    const FLOOR_SCALAR_CHARS: usize = 512;
    const FLOOR_AUX_JSON_CHARS: usize = 2_048;

    let handle = project_root
        .and_then(|root| store_response_handle(root, formatted, current_timestamp()).ok());
    let handle_status: &'static str = if handle.is_some() {
        "stored"
    } else {
        "compacted_no_handle"
    };

    let mut object = Map::new();
    insert_lcm_expand_query_status(&mut object, value, FLOOR_SCALAR_CHARS);
    for key in ["provider", "session_id", "answer"] {
        insert_bounded_scalar_field(&mut object, value, key, FLOOR_SCALAR_CHARS);
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
    insert_bounded_text_field(&mut object, value, "prompt", FLOOR_SCALAR_CHARS);
    insert_bounded_text_field(&mut object, value, "query", FLOOR_SCALAR_CHARS);
    insert_lcm_expand_query_temporal_fields(&mut object, value, 0);
    // Contract-adjacent recovery metadata survives only when it is itself
    // small; anything larger is recoverable via the response handle.
    for key in ["context_recovery_hint", "summary_request"] {
        if let Some(field) = value.get(key) {
            let serialized_len = serde_json::to_string(field).map_or(usize::MAX, |s| s.len());
            if serialized_len <= FLOOR_AUX_JSON_CHARS {
                object.insert(key.to_string(), field.clone());
            }
        }
    }

    // Drop the unbounded arrays entirely; the synthesis prompt below tells the
    // bridge the context was elided and pagination/node ids are recoverable.
    for key in [
        "context_blocks",
        "matches",
        "node_ids",
        "context_pagination",
    ] {
        object.insert(key.to_string(), json!([]));
        object.insert(format!("{key}_truncated_for_mcp"), json!(true));
    }
    object.insert(
        "synthesis_prompt".to_string(),
        compact_synthesis_prompt_with_limits(
            value,
            &json!([]),
            FLOOR_SCALAR_CHARS,
            FLOOR_SCALAR_CHARS,
        ),
    );

    object.insert("mcp_response_truncated".to_string(), json!(true));
    object.insert("contract_truncated".to_string(), json!(true));
    object.insert(
        "mcp_original_response_chars".to_string(),
        json!(formatted.len()),
    );
    object.insert(
        "mcp_truncation_reason".to_string(),
        json!(
            "expand-query response exceeded the minimal synthesis contract budget; unbounded context arrays were dropped"
        ),
    );
    if let Some(record) = &handle {
        object.insert("response_handle".to_string(), json!(record.handle));
        object.insert("retrieve_tool".to_string(), json!(RESPONSE_RETRIEVE_TOOL));
        object.insert("retrieve_expires_at".to_string(), json!(record.expires_at));
        object.insert(
            "retrieve_instruction".to_string(),
            json!(format!(
                "The full expand-query response ({} chars) was stored locally and expires at {}. Call `{RESPONSE_RETRIEVE_TOOL}` with handle `{}` to recover the dropped context_blocks, matches, node_ids, and context_pagination.",
                formatted.len(),
                record.expires_at,
                record.handle
            )),
        );
    }

    let text = serde_json::to_string(&Value::Object(object)).unwrap_or_default();
    if text.len() <= MAX_RESPONSE_CHARS {
        return (text, handle_status);
    }
    // Absolute floor: every retained field above is bounded, so this branch is
    // effectively unreachable, but never emit an unbounded body.
    (
        serde_json::to_string(&json!({
            "status": value
                .get("status")
                .and_then(Value::as_str)
                .filter(|status| !status.is_empty())
                .map_or_else(
                    || json!("partial"),
                    |status| Value::String(truncate_chars(status, FLOOR_SCALAR_CHARS).0),
                ),
            "needs_synthesis": value
                .get("needs_synthesis")
                .cloned()
                .unwrap_or(json!(true)),
            "context_blocks": [],
            "matches": [],
            "mcp_response_truncated": true,
            "contract_truncated": true,
            "mcp_truncation_reason":
                "expand-query response exceeded the minimum synthesis contract budget",
        }))
        .unwrap_or_default(),
        handle_status,
    )
}

#[derive(Copy, Clone)]
pub(super) enum CompactTier {
    Standard,
    Minimal { compact_chars: usize },
}

pub(super) fn compact_lcm_expand_query_payload(
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
            max_temporal_items: 50,
            max_scalar_chars: None,
            max_prompt_chars: MAX_LCM_EXPAND_QUERY_PROMPT_CHARS,
            max_query_chars: MAX_LCM_EXPAND_QUERY_QUERY_CHARS,
            max_synthesis_system_chars: MAX_LCM_EXPAND_QUERY_SYNTHESIS_SYSTEM_CHARS,
            max_synthesis_prompt_chars: MAX_LCM_EXPAND_QUERY_SYNTHESIS_PROMPT_CHARS,
            compact_chars: None,
            truncation_reason: "expand-query response compacted to preserve synthesis contract fields",
        },
        CompactTier::Minimal { compact_chars } => LcmExpandQueryCompactLimits {
            max_context_blocks: 1,
            max_context_block_chars: 240,
            max_matches: 5,
            max_match_snippet_chars: 80,
            max_node_ids: 25,
            max_node_id_chars: 120,
            max_pagination_items: 10,
            max_temporal_items: 10,
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
        for key in ["provider", "session_id", "answer"] {
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
            "provider",
            "session_id",
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
    insert_lcm_expand_query_status(
        &mut object,
        value,
        limits
            .max_scalar_chars
            .unwrap_or(MAX_LCM_EXPAND_QUERY_STATUS_CHARS),
    );
    insert_lcm_expand_query_temporal_fields(&mut object, value, limits.max_temporal_items);

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
    max_temporal_items: usize,
    max_scalar_chars: Option<usize>,
    max_prompt_chars: usize,
    max_query_chars: usize,
    max_synthesis_system_chars: usize,
    max_synthesis_prompt_chars: usize,
    compact_chars: Option<usize>,
    truncation_reason: &'static str,
}

fn insert_lcm_expand_query_temporal_fields(
    object: &mut Map<String, Value>,
    value: &Value,
    max_items: usize,
) {
    for key in [
        "omitted",
        "watermarks",
        "authorized_root",
        "coverage",
        "next_cursor",
        "capped_sessions",
    ] {
        if let Some(field) = value.get(key) {
            object.insert(key.to_string(), field.clone());
        }
    }
    for key in ["anchors", "source_coverage", "explanations", "omissions"] {
        if value.get(key).is_some() {
            let (items, truncated) = compact_array(value.get(key), max_items);
            object.insert(key.to_string(), items);
            object.insert(format!("{key}_truncated_for_mcp"), json!(truncated));
        }
    }
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
    let context_json = serde_json::to_string(context_blocks).unwrap_or_else(|_| "[]".into());
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

fn insert_lcm_expand_query_status(
    object: &mut Map<String, Value>,
    value: &Value,
    max_chars: usize,
) {
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .filter(|status| !status.is_empty())
        .map_or_else(
            || "partial".to_string(),
            |status| truncate_chars(status, max_chars).0,
        );
    object.insert("status".to_string(), json!(status));
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

pub(super) fn truncate_chars(value: &str, max_chars: usize) -> (String, bool) {
    let truncated = value.chars().nth(max_chars).is_some();
    let text = value.chars().take(max_chars).collect::<String>();
    (text, truncated)
}

#[cfg(test)]
mod authority_tests {
    use super::*;

    #[test]
    fn response_handle_root_ignores_caller_controlled_paths() {
        let args = json!({
            "response_handle_project_root": "/attacker/cache",
            "project_root": "/attacker/project"
        });

        assert_eq!(
            lcm_response_handle_root(Some(Path::new("/authorized/project")), &args),
            Some(PathBuf::from("/authorized/project"))
        );
        assert_eq!(lcm_response_handle_root(None, &args), None);
    }
}
