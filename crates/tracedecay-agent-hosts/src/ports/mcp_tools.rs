//! The advertised MCP tool catalog, as host installers see it.
//!
//! **Not a registered port.** `tracedecay-mcp` owns the catalog
//! (`~160` JSON-Schema descriptors plus host-capability filtering) and sits
//! *below* this crate, so installers read it directly instead of through a
//! process-global callback the composition root had to remember to install.
//!
//! That direction matters for correctness, not tidiness. While this was a
//! `OnceLock<fn>` pair, an unwired process answered with an empty catalog —
//! a semantically valid tool set — so any installer reached before the root
//! registered wrote an empty permission allowlist and an empty schema file
//! with no error. `crates/tracedecay/src/runtime_ports.rs` even had a
//! documented "without the MCP catalog" registration form that produced
//! exactly that state. Reading the owning crate makes an unavailable catalog
//! a typed error that no installer can mistake for "this host advertises no
//! tools".
//!
//! Host installers need only the four fields of [`AdvertisedToolV1`]; the
//! full `ToolDefinition` stays in the owning crate.

use serde_json::Value;

use crate::errors::{Result, TraceDecayError};

/// One advertised MCP tool, reduced to what host installers consume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdvertisedToolV1 {
    /// Bare tool name, without any host's `mcp__tracedecay__` prefix.
    pub name: String,
    /// Model-facing description, copied into generated host manifests.
    pub description: String,
    /// JSON Schema for the tool's input parameters.
    pub input_schema: Value,
    /// The tool's `readOnlyHint` annotation, defaulted to `false`.
    ///
    /// Hosts that gate MCP calls behind per-call review use this to let the
    /// read-only subset run unattended, so a missing annotation must read as
    /// "not read-only" rather than as permission.
    pub read_only: bool,
}

/// The tools advertised on this host.
///
/// Errors when the catalog cannot be assembled — the application catalog
/// snapshot is the one remaining runtime input — so a caller writing host
/// permissions or schema files fails loudly instead of writing an empty set.
pub fn advertised_tools() -> Result<Vec<AdvertisedToolV1>> {
    let definitions = tracedecay_mcp::get_tool_definitions().map_err(|error| {
        TraceDecayError::project_route(
            "mcp.catalog_discovery_unavailable",
            false,
            format!("MCP tool discovery is unavailable: {error}"),
        )
    })?;
    Ok(definitions
        .into_iter()
        .map(|tool| {
            let read_only = tool
                .annotations
                .as_ref()
                .and_then(|annotations| annotations.get("readOnlyHint"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            AdvertisedToolV1 {
                name: tool.name,
                description: tool.description,
                input_schema: tool.input_schema,
                read_only,
            }
        })
        .collect())
}

/// The tool names whose output honours a `format` argument.
#[must_use]
pub fn format_capable_tool_names() -> &'static [&'static str] {
    tracedecay_mcp::format_capable_tool_names()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalog is read from its owning crate, so a process that never ran
    /// any composition-root registration still sees the real tool set —
    /// never the empty-but-valid one installers used to write.
    #[test]
    fn the_catalog_is_readable_without_any_registration() {
        let tools = advertised_tools().expect("the owning crate assembles the catalog");
        let search = tools
            .iter()
            .find(|tool| tool.name == "tracedecay_search")
            .expect("search must be advertised to agent hosts");
        assert!(search.read_only);
        assert!(
            tools
                .iter()
                .any(|tool| tool.name == "tracedecay_str_replace" && !tool.read_only),
            "editing must be advertised as not read-only"
        );
    }
}
