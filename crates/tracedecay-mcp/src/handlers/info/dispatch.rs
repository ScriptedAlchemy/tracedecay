//! Name table for the graph-backed file-inspection tools of the info family.
//!
//! Registry, status, and remote-status tools read daemon authorities the
//! composition root holds, so the root dispatches those itself.

use std::path::Path;

use serde_json::Value;
use tracedecay_contracts::retrieval::{CallableCodeOperationKind, callable_code_operation};
use tracedecay_domain::errors::{Result, TraceDecayError};

use super::{handle_config, handle_files, handle_port_order, handle_port_status, handle_todos};
use crate::ToolResult;
use crate::handlers::support::unknown_tool_error;
use crate::handlers::verified_read::{VerifiedGraphOpen, verified_read_operation as read};

/// Dispatches one graph-backed info tool (`tracedecay_files`,
/// `tracedecay_todos`, ...) onto its handler, opening the verified graph
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
        "tracedecay_todos" => handle_todos(&open(read("todos")?).await?, args, scope_prefix).await,
        "tracedecay_config" => handle_config(project_root, &args).await,
        _ => Err(unknown_tool_error(tool_name)),
    }
}
