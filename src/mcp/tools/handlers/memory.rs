//! Cross-session and holographic memory handlers.

use serde_json::{json, Value};

use crate::errors::{Result, TokenSaveError};
use crate::memory::types::{
    AddFactRequest, FeedbackAction, FeedbackRequest, MemoryCategory, SearchFactsRequest,
    UpdateFactRequest,
};
use crate::tokensave::TokenSave;

use super::super::ToolResult;
use super::truncate_response;

fn tool_json(value: &Value) -> ToolResult {
    let formatted = serde_json::to_string_pretty(value).unwrap_or_default();
    ToolResult {
        value: json!({ "content": [{ "type": "text", "text": truncate_response(&formatted) }] }),
        touched_files: vec![],
    }
}

fn config_error(message: impl Into<String>) -> TokenSaveError {
    TokenSaveError::Config {
        message: message.into(),
    }
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| config_error(format!("missing required parameter: {key}")))
}

fn optional_category(args: &Value) -> Result<Option<MemoryCategory>> {
    args.get("category")
        .and_then(Value::as_str)
        .map(str::parse::<MemoryCategory>)
        .transpose()
        .map_err(|e| config_error(format!("invalid category: {e}")))
}

fn limit(args: &Value) -> usize {
    args.get("limit")
        .and_then(Value::as_u64)
        .map_or(20, |n| (n as usize).clamp(1, 200))
}

fn optional_f64(args: &Value, key: &str) -> Option<f64> {
    args.get(key).and_then(Value::as_f64)
}

fn string_array(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn fact_id(args: &Value) -> Result<i64> {
    let value = args
        .get("fact_id")
        .or_else(|| args.get("id"))
        .ok_or_else(|| config_error("missing required parameter: fact_id"))?;
    if let Some(id) = value.as_i64() {
        return Ok(id);
    }
    value
        .as_str()
        .and_then(|s| s.parse::<i64>().ok())
        .ok_or_else(|| config_error("fact_id must be a number or numeric string"))
}

fn metadata_with_tags(args: &Value) -> Value {
    let mut metadata = args
        .get("metadata")
        .cloned()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    let tags = string_array(args, "tags");
    if !tags.is_empty() {
        if let Some(map) = metadata.as_object_mut() {
            map.insert("tags".to_string(), json!(tags));
        }
    }
    metadata
}

fn request_entities(args: &Value) -> Vec<String> {
    let mut entities = string_array(args, "entities");
    if let Some(entity) = args.get("entity").and_then(Value::as_str) {
        entities.push(entity.to_string());
    }
    entities
}

fn feedback_action(args: &Value) -> Result<FeedbackAction> {
    if let Some(action) = args.get("action").and_then(Value::as_str) {
        return match action {
            "helpful" => Ok(FeedbackAction::Helpful),
            "unhelpful" => Ok(FeedbackAction::Unhelpful),
            other => Err(config_error(format!("unknown feedback action: {other}"))),
        };
    }
    match (
        args.get("helpful")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        args.get("unhelpful")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    ) {
        (true, false) => Ok(FeedbackAction::Helpful),
        (false, true) => Ok(FeedbackAction::Unhelpful),
        _ => Err(config_error(
            "missing feedback action: set action, helpful, or unhelpful",
        )),
    }
}

fn results_envelope(action: &str, results: &Value, count: usize) -> Value {
    json!({
        "action": action,
        "results": results,
        "facts": results,
        "count": count,
    })
}

async fn update_trust(args: &Value, cg: &TokenSave, fact_id: i64) -> Result<Option<f64>> {
    if let Some(trust) = optional_f64(args, "trust") {
        return Ok(Some(trust));
    }
    let Some(delta) = optional_f64(args, "trust_delta") else {
        return Ok(None);
    };
    let existing = cg
        .get_fact(fact_id)
        .await?
        .ok_or_else(|| config_error(format!("fact {fact_id} not found")))?;
    Ok(Some((existing.trust_score + delta).clamp(0.0, 1.0)))
}

pub(super) async fn handle_fact_store(cg: &TokenSave, args: Value) -> Result<ToolResult> {
    let action = required_str(&args, "action")?;
    let out = match action {
        "add" => {
            let outcome = cg
                .add_fact(AddFactRequest {
                    content: required_str(&args, "content")?.to_string(),
                    category: optional_category(&args)?.unwrap_or(MemoryCategory::General),
                    source: args
                        .get("source")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    tags: string_array(&args, "tags"),
                    entities: request_entities(&args),
                    trust: optional_f64(&args, "trust"),
                    metadata: metadata_with_tags(&args),
                })
                .await?;
            // Additive write-time diff report fields, so writers SEE
            // near-duplicates, possible conflicts, and secret rejections.
            let count = usize::from(outcome.fact.is_some());
            json!({
                "action": action,
                "fact": outcome.fact,
                "count": count,
                "diff": outcome.diff.diff.as_str(),
                "closest_fact_id": outcome.diff.closest_fact_id,
                "similarity": outcome.diff.similarity,
                "reason": outcome.diff.reason,
            })
        }
        "search" => {
            let facts = cg
                .search_facts(SearchFactsRequest {
                    query: required_str(&args, "query")?.to_string(),
                    category: optional_category(&args)?,
                    limit: Some(limit(&args)),
                    min_trust: optional_f64(&args, "min_trust"),
                    include_why: true,
                })
                .await?;
            let count = facts.len();
            results_envelope(action, &json!(facts), count)
        }
        "probe" => {
            let facts = cg
                .probe_entity(
                    required_str(&args, "entity")?,
                    optional_category(&args)?,
                    optional_f64(&args, "min_trust"),
                    limit(&args),
                )
                .await?;
            let count = facts.len();
            results_envelope(action, &json!(facts), count)
        }
        "related" => {
            let facts = cg
                .related_facts(
                    required_str(&args, "entity")?,
                    optional_category(&args)?,
                    optional_f64(&args, "min_trust"),
                    limit(&args),
                )
                .await?;
            let count = facts.len();
            results_envelope(action, &json!(facts), count)
        }
        "reason" => {
            let entities = request_entities(&args);
            let facts = cg
                .reason_facts(
                    &entities,
                    optional_category(&args)?,
                    optional_f64(&args, "min_trust"),
                    limit(&args),
                )
                .await?;
            let count = facts.len();
            results_envelope(action, &json!(facts), count)
        }
        "contradict" => {
            let facts = cg
                .contradict_facts(
                    optional_category(&args)?,
                    optional_f64(&args, "threshold").unwrap_or(0.3),
                    limit(&args),
                )
                .await?;
            let count = facts.len();
            results_envelope(action, &json!(facts), count)
        }
        "update" => {
            let id = fact_id(&args)?;
            let fact = cg
                .update_fact(UpdateFactRequest {
                    fact_id: id,
                    content: args
                        .get("content")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    category: optional_category(&args)?,
                    tags: args.get("tags").map(|_| string_array(&args, "tags")),
                    entities: args.get("entities").map(|_| request_entities(&args)),
                    trust: update_trust(&args, cg, id).await?,
                    source: args
                        .get("source")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    metadata: args.get("metadata").cloned(),
                })
                .await?;
            json!({ "action": action, "fact": fact, "count": 1 })
        }
        "remove" => {
            let removed = cg.remove_fact(fact_id(&args)?).await?;
            json!({ "action": action, "removed": removed, "count": usize::from(removed) })
        }
        "list" => {
            let facts = cg
                .list_facts(
                    optional_category(&args)?,
                    optional_f64(&args, "min_trust"),
                    limit(&args),
                )
                .await?;
            let count = facts.len();
            results_envelope(action, &json!(facts), count)
        }
        other => return Err(config_error(format!("unknown fact_store action: {other}"))),
    };
    Ok(tool_json(&out))
}

pub(super) async fn handle_fact_feedback(cg: &TokenSave, args: Value) -> Result<ToolResult> {
    let note = args
        .get("note")
        .or_else(|| args.get("reason"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let result = cg
        .record_fact_feedback(FeedbackRequest {
            fact_id: fact_id(&args)?,
            action: feedback_action(&args)?,
            source: args
                .get("source")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            note,
        })
        .await?;
    Ok(tool_json(
        &json!({ "status": "recorded", "feedback": result }),
    ))
}

pub(super) async fn handle_memory_status(cg: &TokenSave) -> Result<ToolResult> {
    let status = cg.memory_status().await?;
    Ok(tool_json(&json!({ "status": "ok", "memory": status })))
}
