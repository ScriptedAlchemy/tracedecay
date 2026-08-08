//! Canonical named SDK state for application capabilities.
//!
//! This module does not introduce a router. It projects each executable
//! capability's already-mounted transport into the stable SDK method spelling
//! the generator emits and retains typed unavailability for incomplete wires.

use std::collections::BTreeSet;

use tracedecay_tool_catalog::{
    BindingStatus, BindingSurface, CapabilityId, CatalogContributionV1, CatalogValidationError,
    CodecBindingKey, ExecutableBindingAvailabilityV1, ExecutableBindingRegistryV1,
    ExecutableBindingV1, ExecutableUnavailableDispositionV1, OperationId, RouteExposureV1,
    SdkExecutableBindingAvailabilityV1, SdkExecutableBindingRegistryV1, SdkExecutableBindingV1,
    SdkTransportBindingV1, ServiceId, SurfaceBindingV1, SurfaceOperationName,
};

use crate::{
    ApplicationContractError, application_catalog_contributions,
    configuration_executable_binding_registry, context_scout_executable_binding_registry,
    handoff_executable_binding_registry, multi_root::multi_root_executable_binding_registry,
    retained_surface_executable_binding_registry, work_executable_binding_registry,
    workflow_executable_binding_registry,
};

/// Every mounted HTTP executable registry the SDK projects.
///
/// This is the single place a product family joins the official SDK. Both the
/// projection below and its conformance guard read this list, so a registry
/// cannot be projected without being asserted, and a registry added here is
/// exposed in the generated Rust and TypeScript SDKs by the same edit. Each
/// operation ID names its own family, so the list needs no parallel labels.
fn mounted_executable_binding_registries()
-> Result<Vec<ExecutableBindingRegistryV1>, ApplicationContractError> {
    Ok(vec![
        work_executable_binding_registry()?,
        workflow_executable_binding_registry()?,
        configuration_executable_binding_registry()?,
        context_scout_executable_binding_registry()?,
        retained_surface_executable_binding_registry()?,
        handoff_executable_binding_registry()?,
        multi_root_executable_binding_registry()?,
    ])
}

/// Canonical SDK state for every current application operation.
///
/// Mounted HTTP registries remain authoritative for executable schemas and
/// lifecycle semantics. MCP operations derive from their owning catalog
/// contribution: a canonical executable schema projects to the official MCP
/// transport, while a missing schema remains typed unavailable.
pub fn sdk_executable_binding_registry()
-> Result<SdkExecutableBindingRegistryV1, ApplicationContractError> {
    let mounted = mounted_executable_binding_registries()?;
    let mut bindings = mounted
        .iter()
        .flat_map(|registry| registry.iter())
        .map(project_http_binding)
        .collect::<Result<Vec<_>, _>>()?;
    let http_operations = bindings
        .iter()
        .map(|availability| availability.operation_id().clone())
        .collect::<BTreeSet<_>>();
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
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|availability| !http_operations.contains(availability.operation_id())),
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
    let Some(schema) = contribution.executable_schema(surface.capability_id()) else {
        return Ok(SdkExecutableBindingAvailabilityV1::Unavailable {
            operation_id,
            disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
        });
    };
    // A schema-backed callable MCP operation is executable through the
    // official SDK MCP transport: the generated SDK selects the mounted tool
    // name while the caller's host owns connection lifecycle and framing.
    let executable = ExecutableBindingV1::daemon_owned(
        manifest,
        operation_id,
        mcp_service_id(surface.capability_id())?,
        schema.request_schema().clone(),
        schema.result_schema().clone(),
        CodecBindingKey::new(format!(
            "codec.application.{}.json.v1",
            surface.operation().as_str()
        ))
        .map_err(|_| CatalogValidationError::InvalidValue {
            field: "SDK MCP codec binding",
            reason: "operation spelling cannot form a canonical codec key",
        })?,
        RouteExposureV1::Internal,
    )?;
    let binding = SdkExecutableBindingV1::new(
        executable,
        surface.binding_id().clone(),
        surface.operation().clone(),
        SdkTransportBindingV1::McpTool {
            tool_name: format!("tracedecay_{}", surface.operation().as_str()),
        },
    )?;
    Ok(SdkExecutableBindingAvailabilityV1::available(binding))
}

/// The daemon service family that owns one MCP-bound application capability
/// (`capability.application.git.status` -> `service.application.git`).
fn mcp_service_id(capability_id: &CapabilityId) -> Result<ServiceId, CatalogValidationError> {
    let family = capability_id
        .as_str()
        .strip_prefix("capability.application.")
        .and_then(|rest| rest.split('.').next())
        .filter(|family| !family.is_empty())
        .ok_or(CatalogValidationError::InvalidValue {
            field: "SDK MCP service family",
            reason: "capability is not rooted at capability.application.",
        })?;
    ServiceId::new(format!("service.application.{family}")).map_err(|_| {
        CatalogValidationError::InvalidValue {
            field: "SDK MCP service ID",
            reason: "capability family cannot form a canonical service identifier",
        }
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
        BindingSurface, CancellationContract, DeadlineBehavior, EffectClass,
        ExecutableUnavailableDispositionV1, IdempotencyContract, OperationId, ReceiptContract,
        ReconciliationContract, RouteExposureV1, SdkExecutableBindingAvailabilityV1,
        SdkTransportBindingV1, TerminalState,
    };

    use super::{
        mounted_executable_binding_registries, project_mcp_availability,
        sdk_executable_binding_registry,
    };
    use crate::{
        application_catalog_contributions, context_scout_surface_catalog_contribution,
        git_surface_catalog_contribution,
    };

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct TestGitStatusRequest {
        max_entries: Option<u32>,
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct TestGitStatusResult {
        changed_paths: Vec<String>,
    }

    /// Every mounted product family reaches the official SDK.
    ///
    /// Handoff and multi-root shipped mounted HTTP routes that the SDK
    /// projection silently omitted, so authorized non-enumerating results were
    /// callable over HTTP but absent from both generated SDKs. Asserting the
    /// whole mounted set — not one named family — is what keeps a future
    /// family from repeating that omission.
    #[test]
    fn sdk_registry_projects_every_mounted_family_including_handoff_and_multi_root() {
        let registry = sdk_executable_binding_registry().expect("SDK registry");
        let mounted = mounted_executable_binding_registries().expect("mounted registries");
        let mounted_operations = mounted
            .iter()
            .flat_map(|source| source.iter())
            .map(|availability| availability.operation_id().as_str().to_owned())
            .collect::<BTreeSet<_>>();
        for operation_id in [
            "operation.handoff.open_investigation_handoff",
            "operation.handoff.open_task_handoff",
            "operation.multi_root.scope_set_read",
            "operation.multi_root.scope_set_compare_and_swap",
            "operation.multi_root.execute",
        ] {
            assert!(
                mounted_operations.contains(operation_id),
                "{operation_id} is mounted, so the SDK must project it"
            );
        }

        for availability in mounted.iter().flat_map(|source| source.iter()) {
            let operation_id = availability.operation_id();
            let projected = registry.get(operation_id).unwrap_or_else(|| {
                panic!(
                    "mounted operation {} is missing from the SDK registry",
                    operation_id.as_str()
                )
            });
            let Some(mounted_binding) = availability.binding() else {
                continue;
            };
            let projected_binding = projected.binding().unwrap_or_else(|| {
                panic!(
                    "mounted operation {} must not be projected as SDK-unavailable",
                    operation_id.as_str()
                )
            });
            let RouteExposureV1::Public { route_path, .. } = mounted_binding.exposure() else {
                continue;
            };
            assert!(
                matches!(
                    projected_binding.transport(),
                    SdkTransportBindingV1::Http { route_path: projected }
                        if projected == route_path
                ),
                "{} must keep its mounted route {route_path} in the SDK",
                operation_id.as_str()
            );
            assert_eq!(
                projected_binding.sdk_method().as_str(),
                operation_id
                    .as_str()
                    .strip_prefix("operation.")
                    .expect("canonical operation ID")
                    .replace('.', "_"),
                "{} must keep its canonical SDK method spelling",
                operation_id.as_str()
            );
        }
    }

    #[test]
    fn sdk_registry_projects_mounted_routes_as_named_direct_methods() {
        let registry = sdk_executable_binding_registry().expect("SDK registry");
        assert!(
            registry
                .iter()
                .filter(|availability| availability
                    .operation_id()
                    .as_str()
                    .starts_with("operation.work."))
                .all(|availability| availability.binding().is_some()),
            "mounted Work operations must not be projected as unavailable"
        );

        let work = registry
            .get(&OperationId::new("operation.work.generate_proposal").expect("operation ID"))
            .and_then(|availability| availability.binding())
            .expect("mounted work generate-proposal");
        assert!(matches!(
            work.transport(),
            SdkTransportBindingV1::Http { route_path }
                if route_path == "/application/work/generate-proposal"
        ));
        assert_eq!(work.sdk_method().as_str(), "work_generate_proposal");

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
    fn sdk_registry_mounts_every_configuration_operation_with_canonical_lifecycle() {
        let registry = sdk_executable_binding_registry().expect("SDK registry");
        for operation in crate::configuration::CONFIGURATION_SURFACE_OPERATION_NAMES {
            let operation_id =
                OperationId::new(format!("operation.application.{operation}")).expect("operation");
            let binding = registry
                .get(&operation_id)
                .and_then(|availability| availability.binding())
                .expect("mounted configuration SDK binding");
            assert_eq!(
                binding.sdk_method().as_str(),
                format!("application_{operation}")
            );
            assert!(matches!(
                binding.transport(),
                SdkTransportBindingV1::Http { route_path }
                    if route_path == &format!("/application/configuration/{operation}")
            ));
            assert_eq!(binding.deadline().maximum_millis(), 15_000);
            if binding.effect() == EffectClass::ConfigurationWrite {
                assert_eq!(binding.idempotency(), IdempotencyContract::Required);
                assert_eq!(binding.receipt(), ReceiptContract::DurableEffect);
                assert_eq!(binding.reconciliation(), ReconciliationContract::Required);
                assert_eq!(
                    binding.terminal_states().states(),
                    [
                        TerminalState::Completed,
                        TerminalState::TimedOut,
                        TerminalState::Failed,
                        TerminalState::EffectUnknown,
                        TerminalState::Partial,
                    ]
                );
                assert_eq!(
                    binding.deadline().behavior(),
                    DeadlineBehavior::ReturnEffectReceipt
                );
                assert!(matches!(
                    binding.cancellation(),
                    CancellationContract::NotCancellable
                ));
            } else {
                assert_eq!(binding.idempotency(), IdempotencyContract::NotRequired);
                assert_eq!(binding.receipt(), ReceiptContract::Operation);
                assert_eq!(
                    binding.terminal_states().states(),
                    [
                        TerminalState::Completed,
                        TerminalState::Cancelled,
                        TerminalState::TimedOut,
                        TerminalState::Failed,
                        TerminalState::Partial,
                    ]
                );
                assert_eq!(
                    binding.deadline().behavior(),
                    DeadlineBehavior::ReturnOperationReceipt
                );
                assert!(matches!(
                    binding.cancellation(),
                    CancellationContract::Cooperative { .. }
                ));
            }
        }
    }

    /// Regression: every Context Scout operation was cataloged and MCP-routed
    /// but projected as `schema_unavailable` by both official SDKs.
    #[test]
    fn sdk_registry_mounts_every_context_scout_operation() {
        let registry = sdk_executable_binding_registry().expect("SDK registry");
        let contribution =
            context_scout_surface_catalog_contribution().expect("Context Scout catalog");
        let mcp_bindings = contribution
            .bindings()
            .iter()
            .filter(|surface| {
                surface.surface() == BindingSurface::Mcp
                    && matches!(
                        surface.status(),
                        tracedecay_tool_catalog::BindingStatus::Current
                    )
                    && !surface.is_alias()
            })
            .collect::<Vec<_>>();
        assert_eq!(mcp_bindings.len(), 11, "all shipped Scout operations");

        for surface in mcp_bindings {
            let operation = surface.operation().as_str();
            let operation_id = OperationId::new(format!("operation.application.{operation}"))
                .expect("catalog operation ID");
            let binding = registry
                .get(&operation_id)
                .and_then(|availability| availability.binding())
                .unwrap_or_else(|| panic!("{operation} must be SDK-callable"));
            let schema = contribution
                .executable_schema(surface.capability_id())
                .unwrap_or_else(|| panic!("{operation} must own executable schemas"));
            assert_eq!(binding.request_schema(), schema.request_schema());
            assert_eq!(binding.result_schema(), schema.result_schema());
            assert!(matches!(
                binding.transport(),
                SdkTransportBindingV1::Http { route_path }
                    if route_path == &format!("/application/context-scout/{operation}")
            ));
        }
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
                let schema_backed = contribution
                    .executable_schema(surface.capability_id())
                    .is_some();
                if availability.binding().is_some() {
                    assert!(
                        manifest.availability().is_callable() && schema_backed,
                        "{} may only be available when callable and schema-backed",
                        operation_id.as_str()
                    );
                    continue;
                }
                let expected_disposition = if !manifest.availability().is_callable() {
                    ExecutableUnavailableDispositionV1::CapabilityDisabled
                } else {
                    assert!(
                        !schema_backed,
                        "{} is callable and schema-backed, so the SDK MCP transport must \
                         project it as available",
                        operation_id.as_str()
                    );
                    ExecutableUnavailableDispositionV1::SchemaUnavailable
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
    fn sdk_registry_exposes_every_mounted_mcp_operation_with_its_schema() {
        let registry = sdk_executable_binding_registry().expect("SDK registry");
        let unavailable = registry
            .iter()
            .filter_map(|availability| match availability {
                SdkExecutableBindingAvailabilityV1::Unavailable {
                    operation_id,
                    disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
                } => Some(operation_id.as_str().to_owned()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();

        assert_eq!(
            unavailable,
            BTreeSet::new(),
            "every mounted MCP operation needs a Rust-owned request/result schema before the \\
             SDK advertises it callable"
        );
    }

    #[test]
    fn schema_backed_catalog_binding_projects_its_mcp_tool_transport() {
        let contribution = git_surface_catalog_contribution().expect("Git contribution");
        let manifest = contribution
            .capabilities()
            .iter()
            .find(|manifest| {
                manifest.capability_id().as_str() == "capability.application.git.status"
            })
            .expect("Git status manifest");
        let authority = tracedecay_tool_catalog::ExecutableSchemaAuthority::for_types_at_paths::<
            TestGitStatusRequest,
            TestGitStatusResult,
        >(
            manifest,
            "tracedecay_application::sdk_catalog::tests::TestGitStatusRequest",
            "tracedecay_application::sdk_catalog::tests::TestGitStatusResult",
        )
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

        let binding = availability
            .binding()
            .expect("schema-backed callable Git status must be SDK-available");
        assert_eq!(binding.sdk_method().as_str(), "git_status");
        assert!(matches!(
            binding.transport(),
            SdkTransportBindingV1::McpTool { tool_name } if tool_name == "tracedecay_git_status"
        ));
        assert!(matches!(
            binding.binding().owner(),
            tracedecay_tool_catalog::ExecutionOwnerV1::DaemonOwned { service_id }
                if service_id.as_str() == "service.application.git"
        ));
    }
}
