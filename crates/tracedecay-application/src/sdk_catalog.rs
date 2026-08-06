//! Canonical named SDK state for application capabilities.
//!
//! This module does not introduce a router. It projects each executable
//! capability's already-mounted transport into the stable SDK method spelling
//! the generator emits and retains typed unavailability for incomplete wires.

use tracedecay_tool_catalog::{
    BindingStatus, BindingSurface, CatalogContributionV1, CatalogValidationError,
    ExecutableBindingAvailabilityV1, ExecutableUnavailableDispositionV1, OperationId,
    RouteExposureV1, SdkExecutableBindingAvailabilityV1, SdkExecutableBindingRegistryV1,
    SdkExecutableBindingV1, SdkTransportBindingV1, SurfaceBindingV1, SurfaceOperationName,
};

use crate::{
    ApplicationContractError, application_catalog_contributions, work_executable_binding_registry,
    workflow_executable_binding_registry,
};

/// Canonical SDK state for every current application operation.
///
/// Mounted HTTP registries remain authoritative for executable schemas and
/// lifecycle semantics. MCP operations are derived from their owning catalog
/// contributions and remain explicitly unavailable until both canonical Rust
/// request/result schemas and an official SDK MCP transport are shipped.
pub fn sdk_executable_binding_registry()
-> Result<SdkExecutableBindingRegistryV1, ApplicationContractError> {
    let work = work_executable_binding_registry()?;
    let workflow = workflow_executable_binding_registry()?;
    let mut bindings = work
        .iter()
        .chain(workflow.iter())
        .map(project_http_binding)
        .collect::<Result<Vec<_>, _>>()?;
    for contribution in application_catalog_contributions()? {
        bindings.extend(
            contribution
                .bindings()
                .iter()
                .filter(|binding| {
                    binding.surface() == BindingSurface::Mcp
                        && matches!(binding.status(), BindingStatus::Current)
                        && !binding.is_alias()
                })
                .map(|binding| project_mcp_availability(&contribution, binding))
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    Ok(SdkExecutableBindingRegistryV1::new(bindings)?)
}

fn project_http_binding(
    availability: &ExecutableBindingAvailabilityV1,
) -> Result<SdkExecutableBindingAvailabilityV1, CatalogValidationError> {
    let Some(executable) = availability.binding() else {
        return Ok(SdkExecutableBindingAvailabilityV1::Unavailable {
            operation_id: availability.operation_id().clone(),
            disposition: unavailable_disposition(availability),
        });
    };
    let RouteExposureV1::Public {
        binding_id,
        route_path,
    } = executable.exposure()
    else {
        return Ok(SdkExecutableBindingAvailabilityV1::Unavailable {
            operation_id: executable.operation_id().clone(),
            disposition: ExecutableUnavailableDispositionV1::RouteUnavailable,
        });
    };
    let sdk_method = SurfaceOperationName::new(sdk_method_name(executable.operation_id())?)?;
    let binding = SdkExecutableBindingV1::new(
        executable.clone(),
        binding_id.clone(),
        sdk_method,
        SdkTransportBindingV1::Http {
            route_path: route_path.clone(),
        },
    )?;
    Ok(SdkExecutableBindingAvailabilityV1::available(binding))
}

fn unavailable_disposition(
    availability: &ExecutableBindingAvailabilityV1,
) -> ExecutableUnavailableDispositionV1 {
    match availability {
        ExecutableBindingAvailabilityV1::Unavailable { disposition, .. } => *disposition,
        ExecutableBindingAvailabilityV1::Available { .. } => {
            ExecutableUnavailableDispositionV1::RouteUnavailable
        }
    }
}

fn project_mcp_availability(
    contribution: &CatalogContributionV1,
    surface: &SurfaceBindingV1,
) -> Result<SdkExecutableBindingAvailabilityV1, CatalogValidationError> {
    let operation_id = OperationId::new(format!(
        "operation.application.{}",
        surface.operation().as_str()
    ))
    .map_err(|_| CatalogValidationError::InvalidValue {
        field: "SDK MCP operation ID",
        reason: "surface spelling cannot form a canonical operation ID",
    })?;
    let manifest = contribution
        .capabilities()
        .binary_search_by(|manifest| manifest.capability_id().cmp(surface.capability_id()))
        .ok()
        .map(|index| &contribution.capabilities()[index])
        .ok_or_else(|| CatalogValidationError::InvalidCapability {
            capability_id: surface.capability_id().clone(),
            reason: "SDK surface binding has no owning manifest",
        })?;
    if !manifest.availability().is_callable() {
        return Ok(SdkExecutableBindingAvailabilityV1::Unavailable {
            operation_id,
            disposition: ExecutableUnavailableDispositionV1::CapabilityDisabled,
        });
    }
    if contribution
        .executable_schema(surface.capability_id())
        .is_none()
    {
        return Ok(SdkExecutableBindingAvailabilityV1::Unavailable {
            operation_id,
            disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
        });
    }
    Ok(SdkExecutableBindingAvailabilityV1::Unavailable {
        operation_id,
        disposition: ExecutableUnavailableDispositionV1::HostUnsupported,
    })
}

fn sdk_method_name(operation_id: &OperationId) -> Result<String, CatalogValidationError> {
    let operation = operation_id.as_str().strip_prefix("operation.").ok_or(
        CatalogValidationError::InvalidValue {
            field: "SDK operation ID",
            reason: "must be rooted at operation.",
        },
    )?;
    if operation.split('.').count() != 2 {
        return Err(CatalogValidationError::InvalidValue {
            field: "SDK operation ID",
            reason: "must identify one product family and operation",
        });
    }
    Ok(operation.replace('.', "_"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use schemars::JsonSchema;
    use tracedecay_tool_catalog::{
        BindingSurface, ExecutableUnavailableDispositionV1, OperationId,
        SdkExecutableBindingAvailabilityV1, SdkTransportBindingV1,
    };

    use super::{project_mcp_availability, sdk_executable_binding_registry};
    use crate::{application_catalog_contributions, git_surface_catalog_contribution};

    #[derive(JsonSchema)]
    struct TestGitStatusRequest {
        max_entries: Option<u32>,
    }

    #[derive(JsonSchema)]
    struct TestGitStatusResult {
        changed_paths: Vec<String>,
    }

    #[test]
    fn sdk_registry_projects_mounted_routes_as_named_direct_methods() {
        let registry = sdk_executable_binding_registry().expect("SDK registry");
        assert!(
            registry.iter().all(|availability| !availability
                .operation_id()
                .as_str()
                .starts_with("operation.work.")),
            "the unavailable Work family must not leak stale SDK bindings"
        );

        let workflow = registry
            .get(&OperationId::new("operation.workflow.register_definition").expect("operation ID"))
            .and_then(|availability| availability.binding())
            .expect("mounted workflow register-definition");
        assert!(matches!(
            workflow.transport(),
            SdkTransportBindingV1::Http { route_path }
                if route_path == "/application/workflow/register-definition"
        ));
        assert_eq!(
            workflow.sdk_method().as_str(),
            "workflow_register_definition"
        );
    }

    #[test]
    fn sdk_registry_derives_every_canonical_mcp_operation_without_claiming_missing_schemas() {
        let registry = sdk_executable_binding_registry().expect("SDK registry");
        let contributions = application_catalog_contributions().expect("application catalog");
        let expected = contributions
            .iter()
            .flat_map(|contribution| contribution.bindings())
            .filter(|binding| {
                binding.surface() == BindingSurface::Mcp
                    && matches!(
                        binding.status(),
                        tracedecay_tool_catalog::BindingStatus::Current
                    )
                    && !binding.is_alias()
            })
            .map(|binding| format!("operation.application.{}", binding.operation().as_str()))
            .collect::<BTreeSet<_>>();
        let actual = registry
            .iter()
            .filter(|availability| {
                availability
                    .operation_id()
                    .as_str()
                    .starts_with("operation.application.")
            })
            .map(|availability| availability.operation_id().as_str().to_owned())
            .collect::<BTreeSet<_>>();

        assert_eq!(actual, expected);
        for contribution in &contributions {
            for surface in contribution.bindings().iter().filter(|binding| {
                binding.surface() == BindingSurface::Mcp
                    && matches!(
                        binding.status(),
                        tracedecay_tool_catalog::BindingStatus::Current
                    )
                    && !binding.is_alias()
            }) {
                let operation_id = OperationId::new(format!(
                    "operation.application.{}",
                    surface.operation().as_str()
                ))
                .expect("operation ID");
                let manifest = contribution
                    .capabilities()
                    .iter()
                    .find(|manifest| manifest.capability_id() == surface.capability_id())
                    .expect("binding manifest");
                let availability = registry.get(&operation_id).expect("SDK availability");
                let expected_disposition = if !manifest.availability().is_callable() {
                    ExecutableUnavailableDispositionV1::CapabilityDisabled
                } else if contribution
                    .executable_schema(surface.capability_id())
                    .is_none()
                {
                    ExecutableUnavailableDispositionV1::SchemaUnavailable
                } else {
                    ExecutableUnavailableDispositionV1::HostUnsupported
                };
                assert!(matches!(
                    availability,
                    SdkExecutableBindingAvailabilityV1::Unavailable {
                        disposition,
                        ..
                    } if *disposition == expected_disposition
                ));
            }
        }
    }

    #[test]
    fn schema_backed_catalog_binding_remains_unavailable_without_a_sdk_mcp_transport() {
        let contribution = git_surface_catalog_contribution().expect("Git contribution");
        let manifest = contribution
            .capabilities()
            .iter()
            .find(|manifest| {
                manifest.capability_id().as_str() == "capability.application.git.status"
            })
            .expect("Git status manifest");
        let authority = tracedecay_tool_catalog::ExecutableSchemaAuthority::for_types::<
            TestGitStatusRequest,
            TestGitStatusResult,
        >(manifest)
        .expect("test schema authority");
        let contribution = contribution
            .with_executable_schemas(vec![authority])
            .expect("schema-backed contribution");
        let surface = contribution
            .bindings()
            .iter()
            .find(|binding| {
                binding.surface() == BindingSurface::Mcp
                    && binding.operation().as_str() == "git_status"
            })
            .expect("Git status MCP binding");
        let availability =
            project_mcp_availability(&contribution, surface).expect("SDK projection");

        assert!(matches!(
            availability,
            SdkExecutableBindingAvailabilityV1::Unavailable {
                disposition: ExecutableUnavailableDispositionV1::HostUnsupported,
                ..
            }
        ));
    }
}
