//! Closed public input for the automatic Memory Curator MCP journey.

use serde::Deserialize;
use serde_json::{Value, json};
use tracedecay_agent_hosts::automation::runner::{
    CURATION_DEFAULT_FACT_REVIEW_LIMIT, CURATION_DEFAULT_MIN_CONFIDENCE,
};

use crate::errors::{Result, TraceDecayError};

const MAX_FACT_REVIEW_LIMIT: usize = 1_000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicMemoryAutomationRun {
    #[serde(default = "default_fact_review_limit")]
    fact_review_limit: usize,
    #[serde(default = "default_min_confidence")]
    min_confidence: f64,
}

pub(super) fn admin_project_args(mut args: Value) -> Result<Value> {
    if let Some(arguments) = args.as_object_mut() {
        arguments.remove("__mcp_request_id");
    }
    let request = serde_json::from_value::<PublicMemoryAutomationRun>(args).map_err(|error| {
        TraceDecayError::Config {
            message: format!("invalid tracedecay_memory_automation_run arguments: {error}"),
        }
    })?;
    if !(1..=MAX_FACT_REVIEW_LIMIT).contains(&request.fact_review_limit) {
        return Err(TraceDecayError::Config {
            message: format!("fact_review_limit must be between 1 and {MAX_FACT_REVIEW_LIMIT}"),
        });
    }
    if !request.min_confidence.is_finite() || !(0.0..=1.0).contains(&request.min_confidence) {
        return Err(TraceDecayError::Config {
            message: "min_confidence must be a finite value between 0 and 1".to_owned(),
        });
    }
    Ok(json!({
        "action": "automation_run",
        "task": "memory_curation",
        "trigger": "manual_mcp",
        "options": {
            "fact_review_limit": request.fact_review_limit,
            "min_confidence": request.min_confidence,
        }
    }))
}

fn default_fact_review_limit() -> usize {
    CURATION_DEFAULT_FACT_REVIEW_LIMIT
}

fn default_min_confidence() -> f64 {
    CURATION_DEFAULT_MIN_CONFIDENCE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_request_selects_only_automatic_memory_curation() {
        assert_eq!(
            admin_project_args(json!({})).unwrap(),
            json!({
                "action": "automation_run",
                "task": "memory_curation",
                "trigger": "manual_mcp",
                "options": {
                    "fact_review_limit": CURATION_DEFAULT_FACT_REVIEW_LIMIT,
                    "min_confidence": CURATION_DEFAULT_MIN_CONFIDENCE,
                }
            })
        );
        assert_eq!(
            admin_project_args(json!({
                "__mcp_request_id": "request.mcp.memory-curator",
                "fact_review_limit": 40,
                "min_confidence": 0.81,
            }))
            .unwrap()["options"],
            json!({ "fact_review_limit": 40, "min_confidence": 0.81 })
        );
    }

    #[test]
    fn public_request_rejects_manual_operations_and_authority_fields() {
        for forbidden in [
            "run_id",
            "task",
            "operations",
            "proposal_id",
            "approve",
            "reject",
            "apply",
        ] {
            let mut input = serde_json::Map::new();
            input.insert(forbidden.to_owned(), Value::Bool(true));
            assert!(
                admin_project_args(Value::Object(input)).is_err(),
                "{forbidden} must remain daemon-owned"
            );
        }
        for invalid in [
            json!({ "fact_review_limit": 0 }),
            json!({ "fact_review_limit": 1_001 }),
            json!({ "min_confidence": -0.01 }),
            json!({ "min_confidence": 1.01 }),
        ] {
            assert!(admin_project_args(invalid).is_err());
        }
    }
}
