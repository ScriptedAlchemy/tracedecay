//! Compatibility shim for the extracted `agents` subsystem.
//!
//! The implementation moved to `crates/tracedecay-agent-hosts` (together with
//! `automation`, which it is mutually recursive with). This glob re-export
//! keeps every previously public path resolving unchanged — both leaf items
//! (`crate::agents::AgentIntegration`) and the host submodules
//! (`crate::agents::claude::…`, `crate::agents::host_bundle_v2::…`), since a
//! `pub mod` is itself a re-exportable item.
//!
//! Items that were `pub(crate)` in the old tree are deliberately NOT covered:
//! they are now private to `tracedecay-agent-hosts`. Root call sites that
//! reached them are cataloged in
//! `crates/tracedecay-agent-hosts/SEAMS.md`.
pub use tracedecay_agent_hosts::agents::*;

fn advertised_mcp_tools() -> Vec<tracedecay_agent_hosts::ports::mcp_tools::AdvertisedToolV1> {
    crate::mcp::tools::get_tool_definitions()
        .into_iter()
        .map(|tool| {
            let read_only = tool
                .annotations
                .as_ref()
                .and_then(|annotations| annotations.get("readOnlyHint"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            tracedecay_agent_hosts::ports::mcp_tools::AdvertisedToolV1 {
                name: tool.name,
                description: tool.description,
                input_schema: tool.input_schema,
                read_only,
            }
        })
        .collect()
}

/// Wires the root MCP catalog into extracted agent-host installers.
pub fn register_mcp_tool_catalog_ports() {
    tracedecay_agent_hosts::ports::mcp_tools::register(advertised_mcp_tools);
    tracedecay_agent_hosts::ports::mcp_tools::register_format_capable_names(
        crate::mcp::tools::format_capable_tool_names,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_mcp_catalog_adapter_preserves_tools_and_annotations() {
        let tools = advertised_mcp_tools();
        let search = tools
            .iter()
            .find(|tool| tool.name == "tracedecay_search")
            .expect("search must be advertised to agent hosts");
        let edit = tools
            .iter()
            .find(|tool| tool.name == "tracedecay_str_replace")
            .expect("editing must be advertised to agent hosts");

        assert!(search.read_only);
        assert!(!edit.read_only);
    }
}
