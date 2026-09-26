//! Name table for the graph-backed file-inspection tools of the info family.
//!
//! Registry, status, and remote-status tools read daemon authorities the
//! composition root holds, so the root dispatches those itself.

use std::path::Path;

use serde_json::Value;
use tracedecay_contracts::retrieval::{CallableCodeOperationKind, callable_code_operation};
use tracedecay_domain::errors::{Result, TraceDecayError};

use super::{handle_config, handle_files};
use crate::ToolResult;
use crate::handlers::support::unknown_tool_error;
use crate::handlers::verified_read::VerifiedGraphOpen;

/// Dispatches one graph-backed info tool (`tracedecay_files`,
/// `tracedecay_todos`, ...) onto its handler, opening the verified graph
/// through `open` under the operation the catalog registers for it.
pub async fn dispatch_tool(
    project_root: &Path,
    response_handle_root: &Path,
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
            handle_files(
                response_handle_root,
                &open(operation).await?,
                args,
                scope_prefix,
            )
            .await
        }
        "tracedecay_config" => handle_config(project_root, response_handle_root, &args).await,
        _ => Err(unknown_tool_error(tool_name)),
    }
}
