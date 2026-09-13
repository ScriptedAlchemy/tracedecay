//! Canonical executable projection for current MCP application bindings.
//!
//! The application catalog owns the capability, schema, and binding facts.
//! This module only materializes those facts into daemon-owned executable
//! metadata for MCP discovery and dispatch. It deliberately does not define a
//! second operation list or transport DTO.

use std::sync::LazyLock;

use tracedecay_tool_catalog::{ExecutableBindingRegistryV1, RouteExposureV1};

use crate::ApplicationContractError;
use crate::application_catalog_projection::{
    ApplicationCatalogProjection, project_application_executable_bindings,
};

/// Executable metadata for every current, non-alias application MCP binding.
///
/// A binding remains present when its capability is disabled or schema is
/// unavailable so discovery and dispatch can distinguish unavailable execution
/// from an operation that was never declared.
pub fn mcp_executable_binding_registry()
-> Result<&'static ExecutableBindingRegistryV1, ApplicationContractError> {
    static REGISTRY: LazyLock<Result<ExecutableBindingRegistryV1, ApplicationContractError>> =
        LazyLock::new(build_mcp_executable_binding_registry);
    REGISTRY.as_ref().map_err(Clone::clone)
}

fn build_mcp_executable_binding_registry()
-> Result<ExecutableBindingRegistryV1, ApplicationContractError> {
    project_application_executable_bindings(ApplicationCatalogProjection::Mcp, |_, _| {
        Ok(RouteExposureV1::Internal)
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use tracedecay_tool_catalog::{ApplicationSurfaceOperation, BindingStatus, BindingSurface};

    use super::mcp_executable_binding_registry;
    use crate::application_catalog_contributions;

    #[test]
    fn registry_projects_each_current_application_mcp_binding_once() {
        let expected = application_catalog_contributions()
            .expect("application catalog")
            .iter()
            .flat_map(|contribution| contribution.bindings())
            .filter(|binding| {
                binding.surface() == BindingSurface::Mcp
                    && matches!(binding.status(), BindingStatus::Current)
                    && !binding.is_alias()
            })
            .map(|binding| {
                let operation =
                    ApplicationSurfaceOperation::from_tool_name(binding.operation().as_str())
                        .map_or_else(
                            || binding.operation().as_str(),
                            |operation| operation.as_str(),
                        );
                format!("operation.application.{operation}")
            })
            .collect::<BTreeSet<_>>();
        let registry = mcp_executable_binding_registry().expect("MCP executable registry");
        let actual = registry
            .iter()
            .map(|availability| availability.operation_id().as_str().to_owned())
            .collect::<BTreeSet<_>>();

        assert_eq!(actual, expected);
    }
}
