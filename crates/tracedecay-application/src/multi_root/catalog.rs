//! Canonical executable bindings for multi-root application operations.

use schemars::JsonSchema;
use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, BindingId, CancellationContract, CancellationPoint,
    CapabilityId, CapabilityManifestInputV1, CapabilityManifestV1, CatalogValidationError,
    CodecBindingKey, DeadlineBehavior, DeadlineContract, DeniedDisclosurePolicy, EffectClass,
    ExecutableBindingAvailabilityV1, ExecutableBindingRegistryV1, ExecutableBindingV1,
    IdempotencyContract, LifecycleClass, OperationId, PaginationContract, PrivacyClass, ProfileId,
    ReceiptContract, ReconciliationContract, RevalidationContract, RevalidationPoint,
    RouteExposureV1, RoutingContractV1, SchemaBodyAuthorityV1, SchemaId, SchemaRef, ScopeDimension,
    ScopeRequirement, ServiceId, StreamingContract, TerminalState, TerminalStateContract,
    UseCaseId,
};

use super::{
    AuthorizedScopeSet, MultiRootExecuteRequestV1, MultiRootQueryPageV1,
    MultiRootScopeSetCasRequestV1, MultiRootScopeSetCasResultV1, MultiRootScopeSetReadRequestV1,
};

const MULTI_ROOT_SERVICE_ID: &str = "service.multi_root";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MultiRootApplicationOperation {
    ScopeSetRead,
    ScopeSetCompareAndSwap,
    Execute,
}

impl MultiRootApplicationOperation {
    pub const ALL: [Self; 3] = [
        Self::ScopeSetRead,
        Self::ScopeSetCompareAndSwap,
        Self::Execute,
    ];

    pub const fn operation_key(self) -> &'static str {
        match self {
            Self::ScopeSetRead => "scope_set_read",
            Self::ScopeSetCompareAndSwap => "scope_set_compare_and_swap",
            Self::Execute => "execute",
        }
    }

    pub const fn operation_id(self) -> &'static str {
        match self {
            Self::ScopeSetRead => "operation.multi_root.scope_set_read",
            Self::ScopeSetCompareAndSwap => "operation.multi_root.scope_set_compare_and_swap",
            Self::Execute => "operation.multi_root.execute",
        }
    }

    pub const fn route_path(self) -> &'static str {
        match self {
            Self::ScopeSetRead => "/multi-root/scope-set/read",
            Self::ScopeSetCompareAndSwap => "/multi-root/scope-set/compare-and-swap",
            Self::Execute => "/multi-root/execute",
        }
    }

    pub const fn application_route_path(self) -> &'static str {
        match self {
            Self::ScopeSetRead => "/application/multi-root/scope-set/read",
            Self::ScopeSetCompareAndSwap => "/application/multi-root/scope-set/compare-and-swap",
            Self::Execute => "/application/multi-root/execute",
        }
    }

    const fn effect(self) -> EffectClass {
        match self {
            Self::ScopeSetCompareAndSwap => EffectClass::Administrative,
            Self::ScopeSetRead | Self::Execute => EffectClass::Read,
        }
    }
}

pub fn multi_root_operation_authority(
    operation: MultiRootApplicationOperation,
) -> Result<(CapabilityId, UseCaseId), CatalogValidationError> {
    let manifest = multi_root_capability_manifest(operation)?;
    Ok((
        manifest.capability_id().clone(),
        manifest.use_case_id().clone(),
    ))
}

/// The canonical manifest for one mounted multi-root operation.
///
/// Surface adapters project this exact contract; they do not maintain local
/// effect, cancellation, or pagination copies.
pub fn multi_root_capability_manifest(
    operation: MultiRootApplicationOperation,
) -> Result<CapabilityManifestV1, CatalogValidationError> {
    manifest(operation)
}

pub fn multi_root_executable_binding_registry()
-> Result<ExecutableBindingRegistryV1, CatalogValidationError> {
    ExecutableBindingRegistryV1::new(vec![
        available::<MultiRootScopeSetReadRequestV1, Option<AuthorizedScopeSet>>(
            MultiRootApplicationOperation::ScopeSetRead,
            "tracedecay_application::multi_root::MultiRootScopeSetReadRequestV1",
            "core::option::Option<tracedecay_application::multi_root::AuthorizedScopeSet>",
        )?,
        available::<MultiRootScopeSetCasRequestV1, MultiRootScopeSetCasResultV1>(
            MultiRootApplicationOperation::ScopeSetCompareAndSwap,
            "tracedecay_application::multi_root::MultiRootScopeSetCasRequestV1",
            "tracedecay_application::multi_root::MultiRootScopeSetCasResultV1",
        )?,
        available::<MultiRootExecuteRequestV1, MultiRootQueryPageV1<serde_json::Value>>(
            MultiRootApplicationOperation::Execute,
            "tracedecay_application::multi_root::MultiRootExecuteRequestV1",
            "tracedecay_application::multi_root::MultiRootQueryPageV1<serde_json::Value>",
        )?,
    ])
}

fn available<Request, Output>(
    operation: MultiRootApplicationOperation,
    request_rust_type_path: &'static str,
    result_rust_type_path: &'static str,
) -> Result<ExecutableBindingAvailabilityV1, CatalogValidationError>
where
    Request: JsonSchema,
    Output: JsonSchema,
{
    let manifest = manifest(operation)?;
    let request_schema = SchemaBodyAuthorityV1::for_type_at_path::<Request>(
        manifest.request_schema().clone(),
        request_rust_type_path,
    )?;
    let result_schema = SchemaBodyAuthorityV1::for_type_at_path::<Output>(
        manifest.result_schema().clone(),
        result_rust_type_path,
    )?;
    let binding = ExecutableBindingV1::daemon_owned(
        &manifest,
        operation_id(operation)?,
        service_id()?,
        request_schema,
        result_schema,
        codec_key(operation)?,
        RouteExposureV1::Public {
            binding_id: binding_id(operation)?,
            route_path: operation.application_route_path().to_owned(),
        },
    )?;
    Ok(ExecutableBindingAvailabilityV1::available(binding))
}

fn manifest(
    operation: MultiRootApplicationOperation,
) -> Result<CapabilityManifestV1, CatalogValidationError> {
    let read_only = operation.effect().is_read_only();
    CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id: catalog_id(
            CapabilityId::new(format!(
                "capability.multi_root.{}",
                operation.operation_key()
            )),
            "multi-root capability ID",
        )?,
        use_case_id: catalog_id(
            UseCaseId::new(format!("use-case.multi_root.{}", operation.operation_key())),
            "multi-root use-case ID",
        )?,
        routing: RoutingContractV1::new(
            1,
            format!("Multi-root {}", operation.operation_key()),
            format!(
                "Execute the canonical multi-root {} application use case.",
                operation.operation_key()
            ),
            vec![format!("Multi-root {}", operation.operation_key())],
        )?,
        request_schema: schema_ref(operation, "request")?,
        result_schema: schema_ref(operation, "result")?,
        effect: operation.effect(),
        scope: ScopeRequirement::new(vec![
            ScopeDimension::Project,
            ScopeDimension::Repository,
            ScopeDimension::Worktree,
        ])?,
        authority: AuthorityRequirement::CapabilityGrantWithRevalidation,
        denied_disclosure: DeniedDisclosurePolicy::Indistinguishable,
        privacy: PrivacyClass::ScopedMetadata,
        lifecycle: LifecycleClass::Stateless,
        streaming: StreamingContract::Unsupported,
        cancellation: CancellationContract::cooperative(if read_only {
            vec![
                CancellationPoint::BeforeAdmission,
                CancellationPoint::BeforeRead,
                CancellationPoint::DuringRead,
            ]
        } else {
            vec![
                CancellationPoint::BeforeAdmission,
                CancellationPoint::BeforeEffect,
                CancellationPoint::EffectInFlight,
                CancellationPoint::AfterCommit,
            ]
        })?,
        deadline: DeadlineContract::new(
            30_000,
            if read_only {
                DeadlineBehavior::ReturnOperationReceipt
            } else {
                DeadlineBehavior::ReturnEffectReceipt
            },
        )?,
        pagination: read_only
            .then(|| PaginationContract::new(100, 1_000, 60_000))
            .transpose()?,
        idempotency: if read_only {
            IdempotencyContract::NotRequired
        } else {
            IdempotencyContract::Required
        },
        inverse: if read_only {
            tracedecay_tool_catalog::InverseContract::NotApplicable
        } else {
            tracedecay_tool_catalog::InverseContract::Unavailable {
                reason: tracedecay_tool_catalog::InverseUnavailableReason::NoShippedInverse,
            }
        },
        authority_revalidation: RevalidationContract::required(vec![
            RevalidationPoint::Authority,
            RevalidationPoint::Scope,
            RevalidationPoint::Policy,
            RevalidationPoint::ExpectedState,
        ])?,
        reconciliation: if read_only {
            ReconciliationContract::NotRequired
        } else {
            ReconciliationContract::Required
        },
        receipt: if read_only {
            ReceiptContract::Operation
        } else {
            ReceiptContract::DurableEffect
        },
        terminal_states: TerminalStateContract::new(terminal_states(read_only))?,
        availability: AvailabilityContract::Available,
        binding_ids: vec![binding_id(operation)?],
        profile_eligibility: vec![catalog_id(
            ProfileId::new("profile.default"),
            "multi-root profile ID",
        )?],
        required_features: Vec::new(),
    })
}

fn operation_id(
    operation: MultiRootApplicationOperation,
) -> Result<OperationId, CatalogValidationError> {
    catalog_id(
        OperationId::new(operation.operation_id()),
        "multi-root operation ID",
    )
}

fn service_id() -> Result<ServiceId, CatalogValidationError> {
    catalog_id(
        ServiceId::new(MULTI_ROOT_SERVICE_ID),
        "multi-root service ID",
    )
}

fn codec_key(
    operation: MultiRootApplicationOperation,
) -> Result<CodecBindingKey, CatalogValidationError> {
    catalog_id(
        CodecBindingKey::new(format!(
            "codec.multi_root.{}.json.v1",
            operation.operation_key()
        )),
        "multi-root codec ID",
    )
}

fn binding_id(
    operation: MultiRootApplicationOperation,
) -> Result<BindingId, CatalogValidationError> {
    catalog_id(
        BindingId::new(format!(
            "binding.http.multi_root.{}.v1",
            operation.operation_key()
        )),
        "multi-root binding ID",
    )
}

fn schema_ref(
    operation: MultiRootApplicationOperation,
    direction: &'static str,
) -> Result<SchemaRef, CatalogValidationError> {
    let id = catalog_id(
        SchemaId::new(format!(
            "schema.tracedecay.multi-root.{}-{direction}.v1",
            operation.operation_key().replace('_', "-")
        )),
        "multi-root schema ID",
    )?;
    SchemaRef::new(id, 1)
}

fn terminal_states(read_only: bool) -> Vec<TerminalState> {
    let mut states = vec![
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial,
    ];
    if !read_only {
        states.push(TerminalState::EffectUnknown);
    }
    states
}

fn catalog_id<T>(
    result: Result<T, impl std::fmt::Display>,
    field: &'static str,
) -> Result<T, CatalogValidationError> {
    result.map_err(|_| CatalogValidationError::InvalidValue {
        field,
        reason: "must be a canonical catalog identifier",
    })
}

#[cfg(test)]
mod tests {
    use tracedecay_tool_catalog::RouteExposureV1;

    use super::{MultiRootApplicationOperation, multi_root_executable_binding_registry};

    #[test]
    fn executable_registry_is_the_single_route_and_contract_authority() {
        let registry = multi_root_executable_binding_registry().unwrap();

        for operation in MultiRootApplicationOperation::ALL {
            let operation_id =
                tracedecay_tool_catalog::OperationId::new(operation.operation_id()).unwrap();
            let binding = registry
                .get(&operation_id)
                .and_then(|availability| availability.binding())
                .unwrap();
            let RouteExposureV1::Public {
                binding_id,
                route_path,
            } = binding.exposure()
            else {
                panic!("multi-root binding must be public");
            };
            assert_eq!(route_path, operation.application_route_path());
            assert_eq!(
                binding_id.as_str(),
                format!("binding.http.multi_root.{}.v1", operation.operation_key())
            );
        }
    }
}
