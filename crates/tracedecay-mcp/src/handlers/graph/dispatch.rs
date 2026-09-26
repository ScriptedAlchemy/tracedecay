//! Name table for the code-graph navigation and lookup family.

use serde_json::Value;
use tracedecay_application::code_index::CodeIndexIgnoredDependencyAdmissionPortV1;
use tracedecay_domain::errors::Result;

use super::handle_search;
use crate::ToolResult;
use crate::handlers::support::unknown_tool_error;
use crate::handlers::verified_read::{VerifiedGraphOpen, verified_read_operation as read};
use crate::tool_context::McpToolContext;

/// Dispatches one graph-family tool (`tracedecay_search`) onto its handler,
/// opening the verified graph through `open` under the operation the catalog
/// registers for it.
pub async fn dispatch_tool(
    ctx: &McpToolContext<'_>,
    open: &VerifiedGraphOpen<'_>,
    tool_name: &str,
    args: Value,
    scope_prefix: Option<&str>,
    ignored_dependency_admission: Option<&dyn CodeIndexIgnoredDependencyAdmissionPortV1>,
) -> Result<ToolResult> {
    match tool_name {
        "tracedecay_search" => {
            handle_search(
                ctx,
                open(read("code_symbol_search")?),
                args,
                scope_prefix,
                ignored_dependency_admission,
            )
            .await
        }
        _ => Err(unknown_tool_error(tool_name)),
    }
}
