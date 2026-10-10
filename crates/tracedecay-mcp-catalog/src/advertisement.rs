//! Default MCP `tools/list` advertisement versus the full dispatch catalog.
//!
//! Hosts pay for every advertised definition on every turn. The default
//! handshake therefore publishes only the always-loaded core plus
//! [`TOOL_SEARCH_TOOL_NAME`]. Every other cataloged tool stays reachable
//! through that search tool and through `tools/call` by exact name.
//! `TRACEDECAY_MCP_TOOL_LIST=full` restores the unfiltered listing for
//! clients that still need it.

use serde_json::Value;

use crate::McpCatalogError;
use crate::ToolDefinition;

/// MCP tool that discovers deferred tools by relevance and loads schemas.
pub const TOOL_SEARCH_TOOL_NAME: &str = "tracedecay_tool_search";

/// Process environment that selects the `tools/list` advertisement.
pub const TOOL_LIST_ADVERTISEMENT_ENV: &str = "TRACEDECAY_MCP_TOOL_LIST";

/// How `tools/list` projects the dispatch catalog onto the wire.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolListAdvertisement {
    /// Always-loaded core plus [`TOOL_SEARCH_TOOL_NAME`].
    Default,
    /// Every catalog-filtered definition, the pre-prune handshake.
    Full,
}

/// Approximate prompt tokens for a `tools/list` payload (JSON bytes / 4).
///
/// This is the same chars/4 estimator the MCP response trailers use. Tool
/// definitions are ASCII JSON, so byte length equals character length.
pub fn tool_list_approx_tokens(payload: &Value) -> Result<u64, McpCatalogError> {
    let encoded = serde_json::to_vec(payload).map_err(|error| {
        McpCatalogError::Initialization(format!("tools/list payload must serialize: {error}"))
    })?;
    Ok((encoded.len() / 4) as u64)
}

/// Whether this definition is marked for immediate host loading.
pub fn tool_definition_is_always_loaded(definition: &ToolDefinition) -> bool {
    definition
        .meta
        .as_ref()
        .and_then(|meta| meta.get("anthropic/alwaysLoad"))
        .and_then(Value::as_bool)
        == Some(true)
}

/// Whether the default handshake advertises this definition.
pub fn tool_definition_is_default_advertised(definition: &ToolDefinition) -> bool {
    tool_definition_is_always_loaded(definition) || definition.name == TOOL_SEARCH_TOOL_NAME
}

/// Resolve the advertisement from the process environment.
///
/// Absent or `default` selects [`ToolListAdvertisement::Default`]. `full`
/// selects the unfiltered listing. Any other value is a typed failure so a
/// misspelled opt-in cannot silently dump the catalog.
pub fn tool_list_advertisement_from_env() -> Result<ToolListAdvertisement, McpCatalogError> {
    match std::env::var(TOOL_LIST_ADVERTISEMENT_ENV) {
        Err(std::env::VarError::NotPresent) => Ok(ToolListAdvertisement::Default),
        Ok(value) if value.is_empty() || value == "default" => Ok(ToolListAdvertisement::Default),
        Ok(value) if value == "full" => Ok(ToolListAdvertisement::Full),
        Ok(value) => Err(McpCatalogError::Initialization(format!(
            "{TOOL_LIST_ADVERTISEMENT_ENV}={value} is not a tools/list advertisement; use default or full"
        ))),
        Err(std::env::VarError::NotUnicode(_)) => Err(McpCatalogError::Initialization(format!(
            "{TOOL_LIST_ADVERTISEMENT_ENV} is not valid Unicode"
        ))),
    }
}

/// Project a composed `{"tools": [...]}` payload onto the selected advertisement.
pub fn advertise_tool_list_payload(
    mut payload: Value,
    advertisement: ToolListAdvertisement,
) -> Result<Value, McpCatalogError> {
    if advertisement == ToolListAdvertisement::Full {
        return Ok(payload);
    }
    let tools = payload
        .get_mut("tools")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| {
            McpCatalogError::Initialization(
                "tools/list payload is missing the tools array".to_owned(),
            )
        })?;
    tools.retain(tool_value_is_default_advertised);
    if tools.is_empty() {
        return Err(McpCatalogError::Initialization(
            "default MCP tools/list advertisement produced no tools".to_owned(),
        ));
    }
    Ok(payload)
}

fn tool_value_is_default_advertised(tool: &Value) -> bool {
    tool.get("name").and_then(Value::as_str) == Some(TOOL_SEARCH_TOOL_NAME)
        || tool
            .get("_meta")
            .and_then(|meta| meta.get("anthropic/alwaysLoad"))
            .and_then(Value::as_bool)
            == Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_advertisement_keeps_always_loaded_and_search_only() {
        let payload = json!({
            "tools": [
                {
                    "name": "tracedecay_search",
                    "_meta": { "anthropic/alwaysLoad": true }
                },
                { "name": "tracedecay_tool_search" },
                { "name": "tracedecay_impact" },
                {
                    "name": "tracedecay_runtime",
                    "_meta": { "anthropic/alwaysLoad": false }
                }
            ]
        });
        let advertised = advertise_tool_list_payload(payload, ToolListAdvertisement::Default)
            .expect("default advertisement");
        let names: Vec<&str> = advertised["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert_eq!(
            names,
            ["tracedecay_search", "tracedecay_tool_search"],
            "default handshake must drop deferred tools"
        );
    }

    #[test]
    fn full_advertisement_is_the_unfiltered_payload() {
        let payload = json!({
            "tools": [
                { "name": "tracedecay_search" },
                { "name": "tracedecay_impact" }
            ]
        });
        let advertised = advertise_tool_list_payload(payload.clone(), ToolListAdvertisement::Full)
            .expect("full advertisement");
        assert_eq!(advertised, payload);
    }

    #[test]
    fn full_payload_costs_more_tokens_than_the_default_set() {
        let full = json!({
            "tools": [
                {
                    "name": "tracedecay_search",
                    "description": "search",
                    "inputSchema": { "type": "object", "properties": { "query": { "type": "string" } } },
                    "_meta": { "anthropic/alwaysLoad": true }
                },
                {
                    "name": "tracedecay_tool_search",
                    "description": "discover",
                    "inputSchema": { "type": "object" }
                },
                {
                    "name": "tracedecay_impact",
                    "description": "impact analysis with a long schema",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "node_id": { "type": "string" },
                            "depth": { "type": "integer" },
                            "include_tests": { "type": "boolean" }
                        }
                    }
                }
            ]
        });
        let default = advertise_tool_list_payload(full.clone(), ToolListAdvertisement::Default)
            .expect("default advertisement");
        let full_tokens = tool_list_approx_tokens(&full).expect("full tokens");
        let default_tokens = tool_list_approx_tokens(&default).expect("default tokens");
        assert!(
            default_tokens < full_tokens,
            "default={default_tokens} must be cheaper than full={full_tokens}"
        );
    }
}
