use serde_json::{Value, json};

use crate::application::memory::MemoryApplication;
use crate::errors::Result;
use crate::memory::types::{FeedbackAction, MemoryCategory};
use crate::store::DatabaseFactStore;

use super::super::support::string_array_values;
use super::{config_error, memory_application_error};

pub(super) const DEFAULT_FACT_LIMIT: usize = 20;
pub(super) const MAX_FACT_LIMIT: usize = 200;

pub(super) fn requests_user_memory(args: &Value) -> bool {
    args.get("memory_scope").and_then(Value::as_str) == Some("user")
}

pub(super) fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| config_error(format!("missing required parameter: {key}")))
}

pub(super) fn optional_category(args: &Value) -> Result<Option<MemoryCategory>> {
    args.get("category")
        .and_then(Value::as_str)
        .map(str::parse::<MemoryCategory>)
        .transpose()
        .map_err(|e| config_error(format!("invalid category: {e}")))
}

pub(super) fn limit(args: &Value) -> usize {
    args.get("limit")
        .and_then(Value::as_u64)
        .map_or(DEFAULT_FACT_LIMIT, |n| {
            (n as usize).clamp(1, MAX_FACT_LIMIT)
        })
}

pub(super) fn optional_f64(args: &Value, key: &str) -> Option<f64> {
    args.get(key).and_then(Value::as_f64)
}

pub(super) fn fact_id(args: &Value) -> Result<i64> {
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

pub(super) fn metadata_with_tags(args: &Value) -> Value {
    let mut metadata = args
        .get("metadata")
        .cloned()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    let tags = string_array_values(args, "tags");
    if !tags.is_empty()
        && let Some(map) = metadata.as_object_mut()
    {
        map.insert("tags".to_string(), json!(tags));
    }
    metadata
}

pub(super) fn request_entities(args: &Value) -> Vec<String> {
    let mut entities = string_array_values(args, "entities");
    if let Some(entity) = args.get("entity").and_then(Value::as_str) {
        entities.push(entity.to_string());
    }
    entities
}

pub(super) fn feedback_action(args: &Value) -> Result<FeedbackAction> {
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

pub(super) async fn update_trust(
    args: &Value,
    memory: &MemoryApplication<DatabaseFactStore<'_>>,
    fact_id: i64,
) -> Result<Option<f64>> {
    if let Some(trust) = optional_f64(args, "trust") {
        return Ok(Some(trust));
    }
    let Some(delta) = optional_f64(args, "trust_delta") else {
        return Ok(None);
    };
    let existing = memory
        .get_fact(fact_id)
        .await
        .map_err(memory_application_error)?
        .ok_or_else(|| config_error(format!("fact {fact_id} not found")))?;
    Ok(Some((existing.trust_score + delta).clamp(0.0, 1.0)))
}
