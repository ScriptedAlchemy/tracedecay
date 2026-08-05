//! Canonical executable bindings for multi-root application operations.

use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, CancellationContract, CancellationPoint,
    CapabilityId, CapabilityManifestInputV1, CapabilityManifestV1, CatalogValidationError,
    DeadlineBehavior, DeadlineContract, DeniedDisclosurePolicy, EffectClass,
    ExecutableBindingAvailabilityV1, ExecutableBindingRegistryV1,
    ExecutableUnavailableDispositionV1, IdempotencyContract, LifecycleClass, OperationId,
    PaginationContract, PrivacyClass, ProfileId, ReceiptContract, ReconciliationContract,
    RevalidationContract, RevalidationPoint, RoutingContractV1, SchemaId, SchemaRef,
    ScopeDimension, ScopeRequirement, StreamingContract, TerminalState, TerminalStateContract,
    UnavailabilityReason, UseCaseId,
};

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
    let manifest = manifest(operation)?;
    Ok((
        manifest.capability_id().clone(),
        manifest.use_case_id().clone(),
    ))
}

pub fn multi_root_executable_binding_registry()
-> Result<ExecutableBindingRegistryV1, CatalogValidationError> {
    ExecutableBindingRegistryV1::new(vec![
        unavailable(MultiRootApplicationOperation::ScopeSetRead)?,
        unavailable(MultiRootApplicationOperation::ScopeSetCompareAndSwap)?,
        unavailable(MultiRootApplicationOperation::Execute)?,
    ])
}

fn unavailable(
    operation: MultiRootApplicationOperation,
) -> Result<ExecutableBindingAvailabilityV1, CatalogValidationError> {
    Ok(ExecutableBindingAvailabilityV1::Unavailable {
        operation_id: operation_id(operation)?,
        disposition: ExecutableUnavailableDispositionV1::CapabilityDisabled,
    })
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
        cancellation: CancellationContract::cooperative(vec![CancellationPoint::BeforeAdmission])?,
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
        availability: AvailabilityContract::Unavailable {
            reason: UnavailabilityReason::NotImplemented,
        },
        binding_ids: Vec::new(),
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
    use tracedecay_tool_catalog::{
        ExecutableBindingAvailabilityV1, ExecutableUnavailableDispositionV1,
    };

    use super::{MultiRootApplicationOperation, multi_root_executable_binding_registry};

    #[test]
    fn executable_registry_reports_every_multi_root_route_as_unavailable() {
        let registry = multi_root_executable_binding_registry().unwrap();

        for operation in MultiRootApplicationOperation::ALL {
            let operation_id =
                tracedecay_tool_catalog::OperationId::new(operation.operation_id()).unwrap();
            assert!(matches!(
                registry.get(&operation_id),
                Some(ExecutableBindingAvailabilityV1::Unavailable {
                    disposition: ExecutableUnavailableDispositionV1::CapabilityDisabled,
                    ..
                })
            ));
        }
    }
}
