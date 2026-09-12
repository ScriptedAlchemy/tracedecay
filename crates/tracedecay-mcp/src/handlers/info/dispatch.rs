//! Name table for the graph-backed file-inspection tools of the info family.
//!
//! Registry, status, and remote-status tools read daemon authorities the
//! composition root holds, so the root dispatches those itself.

use std::path::Path;

use serde_json::Value;
use tracedecay_contracts::retrieval::{CallableCodeOperationKind, callable_code_operation};
use tracedecay_domain::errors::{Result, TraceDecayError};

use super::{
    handle_body, handle_config, handle_files, handle_outline, handle_port_order,
    handle_port_status, handle_read, handle_signature_search, handle_todos, handle_type_hierarchy,
};
use crate::ToolResult;
use crate::handlers::support::unknown_tool_error;
use crate::handlers::verified_read::{VerifiedGraphOpen, verified_read_operation as read};

/// Dispatches one graph-backed info tool (`tracedecay_read`,
/// `tracedecay_files`, ...) onto its handler, opening the verified graph
/// through `open` under the operation the catalog registers for it.
pub async fn dispatch_tool(
    project_root: &Path,
    open: &VerifiedGraphOpen<'_>,
    tool_name: &str,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    match tool_name {
        "tracedecay_files" => {
            let operation = callable_code_operation(CallableCodeOperationKind::SourceMetadata)
                .map_err(|error| TraceDecayError::Config {
                    message: format!("invalid source metadata operation: {error}"),
                })?;
            handle_files(&open(operation).await?, args, scope_prefix).await
        }
        "tracedecay_port_status" => {
            handle_port_status(&open(read("port_status")?).await?, args).await
        }
        "tracedecay_port_order" => handle_port_order(&open(read("port_order")?).await?, args).await,
        "tracedecay_type_hierarchy" => {
            handle_type_hierarchy(&open(read("code_type_hierarchy")?).await?, args).await
        }
        "tracedecay_body" => {
            handle_body(&open(read("source_body")?).await?, args, scope_prefix).await
        }
        "tracedecay_todos" => handle_todos(&open(read("todos")?).await?, args, scope_prefix).await,
        "tracedecay_read" => {
            let operation = match args.get("mode").and_then(Value::as_str).unwrap_or("full") {
                "map" => "source_outline",
                "signatures" => "code_signature_search",
                _ => "source_lines",
            };
            handle_read(&open(read(operation)?).await?, args).await
        }
        "tracedecay_outline" => handle_outline(&open(read("source_outline")?).await?, args).await,
        "tracedecay_config" => handle_config(project_root, &args).await,
        "tracedecay_signature_search" => {
            handle_signature_search(
                &open(read("code_signature_search")?).await?,
                args,
                scope_prefix,
            )
            .await
        }
        _ => Err(unknown_tool_error(tool_name)),
    }
}
