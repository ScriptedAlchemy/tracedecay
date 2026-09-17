//! Registered-project selector helpers for the root's MCP tool handlers.
//!
//! Result shaping (`tool_json`, `generic_tool_result`, ...) lives in
//! `tracedecay_mcp::handlers::support`; this module keeps only the selector
//! validation that needs the daemon's project registry.

use serde_json::Value;

use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::{ProjectRegistryContext, RegisteredGlobalDb};

fn invalid_registered_project_selector(detail: impl Into<String>) -> TraceDecayError {
    TraceDecayError::project_route("project_route_invalid_selector", false, detail.into())
}

pub(super) fn validate_registered_project_selector_aliases(
    args: &Value,
    semantic_top_level_fields: &[&str],
) -> Result<()> {
    if let Some(alias) = ["project_id", "project_path", "project_root", "root"]
        .into_iter()
        .find(|key| !semantic_top_level_fields.contains(key) && args.get(*key).is_some())
    {
        return Err(invalid_registered_project_selector(format!(
            "top-level `{alias}` is not a registered-project selector; use project_selector.project_id"
        )));
    }
    Ok(())
}

pub(super) async fn registered_project_context(
    args: &Value,
    semantic_top_level_fields: &[&str],
    global_db: Option<&RegisteredGlobalDb>,
) -> Result<Option<ProjectRegistryContext>> {
    validate_registered_project_selector_aliases(args, semantic_top_level_fields)?;
    let Some(selector_value) = args.get("project_selector") else {
        return Ok(None);
    };
    let selector = selector_value
        .as_object()
        .ok_or_else(|| invalid_registered_project_selector("project_selector must be an object"))?;
    if selector.len() != 1 || !selector.contains_key("project_id") {
        return Err(invalid_registered_project_selector(
            "project_selector accepts only project_id",
        ));
    }
    let project_id = selector
        .get("project_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            invalid_registered_project_selector(
                "project_selector.project_id must be a non-empty string",
            )
        })?;
    let db = global_db.ok_or_else(|| {
        TraceDecayError::project_route(
            "project_route_not_authorized",
            false,
            "client project registry is unavailable for selector resolution",
        )
    })?;
    db.project_registry_context_by_id(project_id)
        .await?
        .map(Some)
        .ok_or_else(|| {
            TraceDecayError::project_route(
                "project_route_not_found",
                false,
                format!(
                    "registered project not found for project_selector.project_id={project_id}; run tracedecay_project_search"
                ),
            )
        })
}
