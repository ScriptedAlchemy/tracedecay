//! Canonical named SDK state for application capabilities.
//!
//! This module does not introduce a router. It projects each executable
//! capability's already-mounted transport into the stable SDK method spelling
//! the generator emits and retains typed unavailability for incomplete wires.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::sync::LazyLock;

use tracedecay_tool_catalog::{
    ApplicationSurfaceOperation, BindingStatus, BindingSurface, CatalogValidationError,
    CodecBindingKey, ExecutableBindingAvailabilityV1, ExecutableBindingRegistryV1,
    ExecutableBindingV1, ExecutableUnavailableDispositionV1, OperationId, RouteExposureV1,
    SdkExecutableBindingAvailabilityV1, SdkExecutableBindingRegistryV1, SdkExecutableBindingV1,
    SdkTransportBindingV1, SurfaceBindingV1, SurfaceOperationName,
};

use crate::{
    ApplicationContractError, application_catalog_contributions, application_handler_descriptors,
    handoff_executable_binding_registry, multi_root::multi_root_executable_binding_registry,
    retained_surface_executable_binding_registry, work_executable_binding_registry,
    workflow_executable_binding_registry,
};

/// Canonical executable HTTP projection for every application-surface handler.
pub fn application_http_executable_binding_registry()
-> Result<&'static ExecutableBindingRegistryV1, ApplicationContractError> {
    static REGISTRY: LazyLock<Result<ExecutableBindingRegistryV1, ApplicationContractError>> =
        LazyLock::new(build_application_http_executable_binding_registry);
    REGISTRY.as_ref().map_err(Clone::clone)
}

fn build_application_http_executable_binding_registry()
-> Result<ExecutableBindingRegistryV1, ApplicationContractError> {
    let handlers = application_handler_descriptors()?;
    let contributions = application_catalog_contributions()?;
    let mut bindings = Vec::new();
    for (operation, descriptor) in handlers.surface_operations() {
        let capability_id = descriptor.operation().capability_id();
        let Some(contribution) = contributions.iter().find(|contribution| {
            contribution
                .capabilities()
                .iter()
                .any(|manifest| manifest.capability_id() == capability_id)
        }) else {
            return Err(ApplicationContractError::Inconsistent {
                field: "application HTTP contribution",
            });
        };
        let Some(http_binding) = contribution.bindings().iter().find(|binding| {
            binding.capability_id() == capability_id
                && binding.surface() == BindingSurface::Http
                && binding.operation().as_str() == operation.name_for_surface(BindingSurface::Http)
                && matches!(binding.status(), BindingStatus::Current)
                && !binding.is_alias()
        }) else {
            continue;
        };
        let manifest = contribution
            .capabilities()
            .iter()
            .find(|manifest| manifest.capability_id() == capability_id)
            .ok_or(ApplicationContractError::Inconsistent {
                field: "application HTTP capability",
            })?;
        let schema = contribution.executable_schema(capability_id).ok_or(
            ApplicationContractError::Inconsistent {
                field: "application HTTP schema",
            },
        )?;
        let service_id = descriptor
            .service_id()
            .ok_or(ApplicationContractError::Inconsistent {
                field: "application HTTP service",
            })?;
        bindings.push(ExecutableBindingAvailabilityV1::available(
            ExecutableBindingV1::daemon_owned(
                manifest,
                OperationId::new(format!("operation.application.{}", operation.as_str()))?,
                service_id.clone(),
                schema.request_schema().clone(),
                schema.result_schema().clone(),
                CodecBindingKey::new(format!("codec.application.{}.json.v1", operation.as_str()))?,
                RouteExposureV1::Public {
                    binding_id: http_binding.binding_id().clone(),
                    route_path: format!("/application{}", application_http_route_path(operation)),
                },
            )?,
        ));
    }
    Ok(ExecutableBindingRegistryV1::new(bindings)?)
}

pub fn application_http_route_path(operation: ApplicationSurfaceOperation) -> String {
    match operation {
        ApplicationSurfaceOperation::GitStatus => "/git/status".to_owned(),
        ApplicationSurfaceOperation::GitDiff => "/git/diff".to_owned(),
        ApplicationSurfaceOperation::GitHistory => "/git/history".to_owned(),
        ApplicationSurfaceOperation::GitBlame => "/git/blame".to_owned(),
        ApplicationSurfaceOperation::GitHunks => "/git/hunks".to_owned(),
        ApplicationSurfaceOperation::GitPreview => "/git/preview".to_owned(),
        ApplicationSurfaceOperation::GitApply => "/git/apply".to_owned(),
        ApplicationSurfaceOperation::GitHubStackSignalExpand => {
            "/github-stack/signal-expand".to_owned()
        }
        operation @ (ApplicationSurfaceOperation::NativeIntegrationStackSnapshot
        | ApplicationSurfaceOperation::NativeIntegrationPreflight
        | ApplicationSurfaceOperation::NativeIntegrationApprove
        | ApplicationSurfaceOperation::NativeIntegrationApply
        | ApplicationSurfaceOperation::NativeIntegrationStatus
        | ApplicationSurfaceOperation::NativeIntegrationCancel
        | ApplicationSurfaceOperation::NativeIntegrationWorktreeInventory
        | ApplicationSurfaceOperation::NativeIntegrationWorktreeInspect
        | ApplicationSurfaceOperation::NativeIntegrationWorktreeConfirm
        | ApplicationSurfaceOperation::NativeIntegrationWorktreeRemove
        | ApplicationSurfaceOperation::NativeIntegrationWorktreeReconcile) => {
            format!("/native-integration/{}", operation.as_str())
        }
        ApplicationSurfaceOperation::AffectedTests => "/tests/affected".to_owned(),
        ApplicationSurfaceOperation::TestResults => "/tests/results".to_owned(),
        ApplicationSurfaceOperation::FeedbackDiagnostics => "/feedback/diagnostics".to_owned(),
        ApplicationSurfaceOperation::FeedbackGet => "/feedback/get".to_owned(),
        ApplicationSurfaceOperation::FeedbackExpand => "/feedback/expand".to_owned(),
        ApplicationSurfaceOperation::FeedbackList => "/feedback/list".to_owned(),
        ApplicationSurfaceOperation::FeedbackImpact => "/feedback/impact".to_owned(),
        ApplicationSurfaceOperation::FeedbackAdvisoryCycle => "/feedback/advisory_cycle".to_owned(),
        operation @ (ApplicationSurfaceOperation::CodeExactOccurrence
        | ApplicationSurfaceOperation::CodePhraseSearch
        | ApplicationSurfaceOperation::CodeSymbolSearch
        | ApplicationSurfaceOperation::CodeSignatureSearch
        | ApplicationSurfaceOperation::CodeImplementations
        | ApplicationSurfaceOperation::CodeTypeHierarchy
        | ApplicationSurfaceOperation::CodeCallers
        | ApplicationSurfaceOperation::CodeCallees
        | ApplicationSurfaceOperation::CodeFacets
        | ApplicationSurfaceOperation::CodeTimeline
        | ApplicationSurfaceOperation::CodeDeclaration
        | ApplicationSurfaceOperation::CodeTypeDefinition
        | ApplicationSurfaceOperation::CodeReferences) => {
            format!("/code/{}", operation.as_str())
        }
        operation @ (ApplicationSurfaceOperation::SessionLookup
        | ApplicationSurfaceOperation::QualifiedName
        | ApplicationSurfaceOperation::CallChain
        | ApplicationSurfaceOperation::FileDependents
        | ApplicationSurfaceOperation::SourceLines
        | ApplicationSurfaceOperation::SourceBody
        | ApplicationSurfaceOperation::SourceOutline
        | ApplicationSurfaceOperation::ModuleApi
        | ApplicationSurfaceOperation::HealthRead
        | ApplicationSurfaceOperation::HealthDelta
        | ApplicationSurfaceOperation::StorageStatus
        | ApplicationSurfaceOperation::DiagnosticsRead) => {
            format!("/primitives/{}", operation.as_str())
        }
        operation @ (ApplicationSurfaceOperation::ConfigurationList
        | ApplicationSurfaceOperation::ConfigurationGet
        | ApplicationSurfaceOperation::ConfigurationSet
        | ApplicationSurfaceOperation::ConfigurationUnset
        | ApplicationSurfaceOperation::ConfigurationBatch
        | ApplicationSurfaceOperation::ConfigurationObservedState
        | ApplicationSurfaceOperation::ConfigurationProtectedPreview
        | ApplicationSurfaceOperation::ConfigurationProtectedApply
        | ApplicationSurfaceOperation::ConfigurationRollbackPreview
        | ApplicationSurfaceOperation::ConfigurationRollbackApply
        | ApplicationSurfaceOperation::ConfigurationAudit) => {
            format!("/configuration/{}", operation.as_str())
        }
        ApplicationSurfaceOperation::ObservatoryRead => "/observatory/read".to_owned(),
        operation @ (ApplicationSurfaceOperation::ContextScoutStatus
        | ApplicationSurfaceOperation::ContextScoutRecent
        | ApplicationSurfaceOperation::ContextScoutExplain
        | ApplicationSurfaceOperation::ContextScoutCapability
        | ApplicationSurfaceOperation::ContextScoutBudget
        | ApplicationSurfaceOperation::ContextScoutPause
        | ApplicationSurfaceOperation::ContextScoutResume
        | ApplicationSurfaceOperation::ContextScoutCancel
        | ApplicationSurfaceOperation::ContextScoutClaim
        | ApplicationSurfaceOperation::ContextScoutDelivery
        | ApplicationSurfaceOperation::ContextScoutFeedback) => {
            format!("/context-scout/{}", operation.as_str())
        }
    }
}

/// Mounted executable authorities outside the canonical application surface.
///
/// Application operations project as one registry above. Work, Workflow,
/// retained, handoff, and multi-root keep separate entries because they have
/// distinct operation identities and runtime owners.
fn mounted_executable_binding_registries()
-> Result<Vec<Cow<'static, ExecutableBindingRegistryV1>>, ApplicationContractError> {
    Ok(vec![
        Cow::Borrowed(application_http_executable_binding_registry()?),
        Cow::Borrowed(work_executable_binding_registry()?),
        Cow::Borrowed(workflow_executable_binding_registry()?),
        Cow::Owned(retained_surface_executable_binding_registry()?),
        Cow::Owned(handoff_executable_binding_registry()?),
        Cow::Owned(multi_root_executable_binding_registry()?),
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
    let mcp_registry = crate::mcp_executable_binding_registry()?;
    let mut bindings = mounted
        .iter()
        .flat_map(|registry| registry.as_ref().iter())
        .filter(|availability| {
            !preserves_shipped_mcp_sdk_transport(availability.operation_id(), mcp_registry)
        })
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
                .map(|binding| project_mcp_availability(mcp_registry, binding))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|availability| !http_operations.contains(availability.operation_id())),
        );
    }
    Ok(SdkExecutableBindingRegistryV1::new(bindings)?)
}

/// Session lookup shipped through `tracedecay_session_lookup` and the
/// `session_lookup` SDK method. Prefer that mounted MCP binding when present;
/// every other application operation continues to select its mounted HTTP
/// binding first.
fn preserves_shipped_mcp_sdk_transport(
    operation_id: &OperationId,
    mcp_registry: &ExecutableBindingRegistryV1,
) -> bool {
    let operation = operation_id
        .as_str()
        .strip_prefix("operation.application.")
        .and_then(ApplicationSurfaceOperation::from_catalog_name);
    operation == Some(ApplicationSurfaceOperation::SessionLookup)
        && mcp_registry
            .get(operation_id)
            .and_then(ExecutableBindingAvailabilityV1::binding)
            .is_some()
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
    registry: &ExecutableBindingRegistryV1,
    surface: &SurfaceBindingV1,
) -> Result<SdkExecutableBindingAvailabilityV1, CatalogValidationError> {
    let canonical_operation =
        ApplicationSurfaceOperation::from_tool_name(surface.operation().as_str()).map_or_else(
            || surface.operation().as_str(),
            |operation| operation.as_str(),
        );
    let operation_id = OperationId::new(format!("operation.application.{}", canonical_operation))
        .map_err(|_| CatalogValidationError::InvalidValue {
        field: "SDK MCP operation ID",
        reason: "surface spelling cannot form a canonical operation ID",
    })?;
    let availability =
        registry
            .get(&operation_id)
            .ok_or_else(|| CatalogValidationError::InvalidCapability {
                capability_id: surface.capability_id().clone(),
                reason: "SDK surface binding has no canonical MCP executable",
            })?;
    let Some(executable) = availability.binding() else {
        return Ok(SdkExecutableBindingAvailabilityV1::Unavailable {
            operation_id,
            disposition: unavailable_disposition(availability),
        });
    };
    let binding = SdkExecutableBindingV1::new(
        executable.clone(),
        surface.binding_id().clone(),
        surface.operation().clone(),
        SdkTransportBindingV1::McpTool {
            tool_name: format!("tracedecay_{}", surface.operation().as_str()),
        },
    )?;
    Ok(SdkExecutableBindingAvailabilityV1::available(binding))
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
    if let Some(code_search_operation) = operation.strip_prefix("application.code_") {
        return Ok(format!("code_{code_search_operation}"));
    }
    Ok(operation.replace('.', "_"))
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::collections::BTreeSet;

    use tracedecay_tool_catalog::{
        ApplicationSurfaceOperation, BindingSurface, CancellationContract, DeadlineBehavior,
        EffectClass, ExecutableUnavailableDispositionV1, IdempotencyContract, OperationId,
        ReceiptContract, ReconciliationContract, RouteExposureV1,
        SdkExecutableBindingAvailabilityV1, SdkTransportBindingV1, TerminalState,
    };

    use super::{
        application_http_executable_binding_registry, mounted_executable_binding_registries,
        preserves_shipped_mcp_sdk_transport, project_mcp_availability,
        sdk_executable_binding_registry,
    };
    use crate::{
        application_catalog_contributions, context_scout_surface_catalog_contribution,
        git_surface_catalog_contribution,
    };

    #[test]
    fn sdk_projection_borrows_process_static_work_registries() {
        let mounted = mounted_executable_binding_registries().expect("mounted registries");

        for operation_id in ["operation.work.create", "operation.workflow.get_run"] {
            let source = mounted
                .iter()
                .find(|source| {
                    source
                        .iter()
                        .any(|binding| binding.operation_id().as_str() == operation_id)
                })
                .unwrap_or_else(|| panic!("missing mounted registry for {operation_id}"));
            assert!(
                matches!(source, Cow::Borrowed(_)),
                "SDK projection must borrow the process-static registry containing {operation_id}",
            );
        }
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
        let mcp_registry = crate::mcp_executable_binding_registry().expect("MCP registry");
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
            if preserves_shipped_mcp_sdk_transport(operation_id, mcp_registry) {
                // The one shipped SDK method that rides its MCP tool; the
                // dedicated `session_lookup` test pins that transport.
                assert!(
                    matches!(
                        projected_binding.transport(),
                        SdkTransportBindingV1::McpTool { .. }
                    ),
                    "{} keeps its shipped MCP transport in the SDK",
                    operation_id.as_str()
                );
                continue;
            }
            assert!(
                matches!(
                    projected_binding.transport(),
                    SdkTransportBindingV1::Http { route_path: projected }
                        if projected == route_path
                ),
                "{} must keep its mounted route {route_path} in the SDK",
                operation_id.as_str()
            );
            let operation = operation_id
                .as_str()
                .strip_prefix("operation.")
                .expect("canonical operation ID");
            let expected_method = operation
                .strip_prefix("application.code_")
                .map(|suffix| format!("code_{suffix}"))
                .unwrap_or_else(|| operation.replace('.', "_"));
            assert_eq!(
                projected_binding.sdk_method().as_str(),
                expected_method,
                "{} must keep its canonical SDK method spelling",
                operation_id.as_str()
            );
        }
    }

    #[test]
    fn sdk_registry_selects_the_mounted_http_transport_for_every_code_search() {
        let registry = sdk_executable_binding_registry().expect("SDK registry");
        let mounted = application_http_executable_binding_registry()
            .expect("mounted application HTTP registry");
        let expected = crate::application_catalog_contributions()
            .expect("application catalog")
            .into_iter()
            .flat_map(|contribution| contribution.bindings().to_vec())
            .filter(|binding| {
                binding.surface() == BindingSurface::Http
                    && matches!(
                        binding.status(),
                        tracedecay_tool_catalog::BindingStatus::Current
                    )
                    && !binding.is_alias()
                    && binding.operation().as_str().starts_with("code_")
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
        let actual = mounted
            .iter()
            .filter(|availability| {
                availability
                    .operation_id()
                    .as_str()
                    .starts_with("operation.application.code_")
            })
            .map(|availability| availability.operation_id().as_str().to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(actual, expected, "every cataloged code-search HTTP route");

        for availability in mounted.iter().filter(|availability| {
            availability
                .operation_id()
                .as_str()
                .starts_with("operation.application.code_")
        }) {
            let mounted_binding = availability
                .binding()
                .expect("mounted code-search executable");
            let operation_id = mounted_binding.operation_id();
            let operation = operation_id
                .as_str()
                .strip_prefix("operation.application.")
                .expect("application operation ID");
            let binding = registry
                .get(operation_id)
                .and_then(|availability| availability.binding())
                .unwrap_or_else(|| panic!("{operation} must be SDK-callable"));
            assert_eq!(binding.binding(), mounted_binding);
            assert_eq!(binding.sdk_method().as_str(), operation);
            assert!(matches!(
                binding.transport(),
                SdkTransportBindingV1::Http { route_path }
                    if route_path == &format!("/application/code/{operation}")
            ));
        }
    }

    #[test]
    fn sdk_registry_selects_live_feedback_and_primitive_http_routes() {
        let registry = sdk_executable_binding_registry().expect("SDK registry");
        for (operation, route) in [
            ("feedback_diagnostics", "/application/feedback/diagnostics"),
            ("feedback_get", "/application/feedback/get"),
            ("feedback_expand", "/application/feedback/expand"),
            ("feedback_list", "/application/feedback/list"),
            ("feedback_impact", "/application/feedback/impact"),
            (
                "feedback_advisory_cycle",
                "/application/feedback/advisory_cycle",
            ),
            ("affected_tests", "/application/tests/affected"),
            ("test_results", "/application/tests/results"),
            ("qualified_name", "/application/primitives/qualified_name"),
            ("call_chain", "/application/primitives/call_chain"),
            ("file_dependents", "/application/primitives/file_dependents"),
            ("source_lines", "/application/primitives/source_lines"),
            ("source_body", "/application/primitives/source_body"),
            ("source_outline", "/application/primitives/source_outline"),
            ("module_api", "/application/primitives/module_api"),
            ("health_read", "/application/primitives/health_read"),
            ("health_delta", "/application/primitives/health_delta"),
            ("storage_status", "/application/primitives/storage_status"),
            (
                "diagnostics_read",
                "/application/primitives/diagnostics_read",
            ),
        ] {
            let operation_id = OperationId::new(format!("operation.application.{operation}"))
                .expect("operation ID");
            let binding = registry
                .get(&operation_id)
                .and_then(|availability| availability.binding())
                .unwrap_or_else(|| panic!("{operation} must be SDK-callable"));
            assert!(matches!(
                binding.transport(),
                SdkTransportBindingV1::Http { route_path } if route_path == route
            ));
            assert_eq!(
                binding.sdk_method().as_str(),
                format!("application_{operation}")
            );
            let expected_service =
                if operation == "test_results" || route.starts_with("/application/primitives/") {
                    "service.application.primitive"
                } else {
                    "service.application.feedback"
                };
            assert!(matches!(
                binding.binding().owner(),
                tracedecay_tool_catalog::ExecutionOwnerV1::DaemonOwned { service_id }
                    if service_id.as_str() == expected_service
            ));
        }

        let session_lookup = registry
            .get(&OperationId::new("operation.application.session_lookup").expect("operation ID"))
            .and_then(|availability| availability.binding())
            .expect("session lookup must remain SDK-callable");
        assert_eq!(session_lookup.sdk_method().as_str(), "session_lookup");
        assert!(matches!(
            session_lookup.transport(),
            SdkTransportBindingV1::McpTool { tool_name }
                if tool_name == "tracedecay_session_lookup"
        ));
    }

    #[test]
    fn sdk_registry_mounts_every_configuration_operation_with_canonical_lifecycle() {
        let registry = sdk_executable_binding_registry().expect("SDK registry");
        for operation in crate::configuration::configuration_surface_operation_names() {
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
        assert!(
            !mcp_bindings.is_empty(),
            "Context Scout must ship at least one current MCP-bound operation"
        );

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
                let operation =
                    ApplicationSurfaceOperation::from_tool_name(surface.operation().as_str())
                        .map_or_else(
                            || surface.operation().as_str(),
                            |operation| operation.as_str(),
                        );
                let operation_id = OperationId::new(format!("operation.application.{}", operation))
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
        let surface = contribution
            .bindings()
            .iter()
            .find(|binding| {
                binding.surface() == BindingSurface::Mcp
                    && binding.operation().as_str() == "git_status"
            })
            .expect("Git status MCP binding");
        let registry = crate::mcp_executable_binding_registry().expect("MCP registry");
        let availability = project_mcp_availability(registry, surface).expect("SDK projection");

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
