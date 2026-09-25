//! MCP discovery projected from the canonical Work executable registry.
//!
//! The executable owns Work's operation set, request schemas, effects, and
//! availability. MCP contributes only its transport prefix and presentation.

use tracedecay_api::WorkOperation;

use super::{FamilyOperation, project_executable_family};
use crate::ToolDefinition;

type DiscoveryResult<T> = Result<T, crate::McpCatalogError>;

/// Build every discoverable Work tool from the mounted executable bindings.
///
/// Every mounted Work operation must have an executable binding; discovery
/// fails loudly if the registry is incomplete rather than silently omitting a
/// callable operation. Schema bodies come from the same registry the HTTP
/// owner validates, so MCP cannot omit a required request field or admit one
/// that typed Work decoding rejects.
pub(super) fn work_definitions() -> DiscoveryResult<Vec<ToolDefinition>> {
    let registry = tracedecay_contracts::work_executable_binding_registry()
        .map_err(crate::McpCatalogError::CatalogValidation)?;
    let operations = WorkOperation::ALL
        .into_iter()
        .map(|operation| FamilyOperation {
            operation_id: operation.operation_id(),
            name: format!("tracedecay_work_{}", operation.operation_key()),
            title: format!("Work {}", operation.operation_key()),
            description: operation.description().to_owned(),
        })
        .collect::<Vec<_>>();
    project_executable_family(
        registry,
        &operations,
        (
            "MCP Work executable registry",
            "must expose exactly every canonical Work operation",
        ),
        (
            "MCP Work operation identity",
            "must name one canonical Work operation",
        ),
        (
            "MCP Work executable binding",
            "canonical Work operation is not executable",
        ),
    )
}
