//! Canonical executable projection for current MCP application bindings.
//!
//! The application catalog owns the capability, schema, and binding facts.
//! This module only materializes those facts into daemon-owned executable
//! metadata for MCP discovery and dispatch. It deliberately does not define a
//! second operation list or transport DTO.

use tracedecay_tool_catalog::{
    BindingStatus, BindingSurface, CatalogContributionV1, CodecBindingKey,
    ExecutableBindingAvailabilityV1, ExecutableBindingRegistryV1, ExecutableBindingV1,
    ExecutableUnavailableDispositionV1, OperationId, RouteExposureV1, ServiceId, SurfaceBindingV1,
};

use crate::{ApplicationContractError, application_catalog_contributions};

/// Executable metadata for every current, non-alias application MCP binding.
///
/// A binding remains present when its capability is disabled or schema is
/// unavailable so discovery and dispatch can distinguish unavailable execution
/// from an operation that was never declared.
pub fn mcp_executable_binding_registry()
-> Result<ExecutableBindingRegistryV1, ApplicationContractError> {
    let mut bindings = Vec::new();
    for contribution in application_catalog_contributions()? {
        for surface in contribution.bindings().iter().filter(|surface| {
            surface.surface() == BindingSurface::Mcp
                && matches!(surface.status(), BindingStatus::Current)
                && !surface.is_alias()
        }) {
            bindings.push(project_mcp_availability(&contribution, surface)?);
        }
    }
    Ok(ExecutableBindingRegistryV1::new(bindings)?)
}

fn project_mcp_availability(
    contribution: &CatalogContributionV1,
    surface: &SurfaceBindingV1,
) -> Result<ExecutableBindingAvailabilityV1, ApplicationContractError> {
    let operation = surface.operation().as_str();
    let operation_id = OperationId::new(format!("operation.application.{operation}"))?;
    let manifest = contribution
        .capabilities()
        .iter()
        .find(|manifest| manifest.capability_id() == surface.capability_id())
        .ok_or(ApplicationContractError::Inconsistent {
            field: "MCP surface binding manifest",
        })?;
    if !manifest.availability().is_callable() {
        return Ok(ExecutableBindingAvailabilityV1::Unavailable {
            operation_id,
            disposition: ExecutableUnavailableDispositionV1::CapabilityDisabled,
        });
    }
    let Some(schema) = contribution.executable_schema(surface.capability_id()) else {
        return Ok(ExecutableBindingAvailabilityV1::Unavailable {
            operation_id,
            disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
        });
    };
    let executable = ExecutableBindingV1::daemon_owned(
        manifest,
        operation_id,
        service_id(surface)?,
        schema.request_schema().clone(),
        schema.result_schema().clone(),
        CodecBindingKey::new(format!("codec.application.{operation}.json.v1"))?,
        RouteExposureV1::Internal,
    )?;
    Ok(ExecutableBindingAvailabilityV1::available(executable))
}

fn service_id(surface: &SurfaceBindingV1) -> Result<ServiceId, ApplicationContractError> {
    let family = surface
        .capability_id()
        .as_str()
        .strip_prefix("capability.application.")
        .and_then(|value| value.split('.').next())
        .filter(|value| !value.is_empty())
        .ok_or(ApplicationContractError::Inconsistent {
            field: "MCP service family",
        })?;
    Ok(ServiceId::new(format!("service.application.{family}"))?)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use tracedecay_tool_catalog::{BindingStatus, BindingSurface};

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
            .map(|binding| format!("operation.application.{}", binding.operation().as_str()))
            .collect::<BTreeSet<_>>();
        let registry = mcp_executable_binding_registry().expect("MCP executable registry");
        let actual = registry
            .iter()
            .map(|availability| availability.operation_id().as_str().to_owned())
            .collect::<BTreeSet<_>>();

        assert_eq!(actual, expected);
    }
}
