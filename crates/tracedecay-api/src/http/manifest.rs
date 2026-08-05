use std::collections::BTreeSet;

use thiserror::Error;
use tracedecay_application::{
    ApplicationOutcome, Deadline, OpaqueCursor, OperationTermination, PageRequest,
};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::{
    BindingSurface, CancellationContract, CapabilityManifestV1, CatalogSnapshotV1, FeatureId,
    ProfileId, ReceiptContract, SurfaceBindingV1, SurfaceOperationName, TerminalState,
};

/// Catalog-owned semantics for one concrete HTTP binding.
///
/// The adapter borrows the binding and manifest instead of copying their
/// lifecycle fields into an HTTP-owned operation descriptor. Request/result
/// schemas, deadlines, cancellation, pagination, receipts, and terminal states
/// therefore remain one immutable application contract.
#[derive(Clone, Debug)]
pub struct HttpManifestContract {
    binding: SurfaceBindingV1,
    capability: CapabilityManifestV1,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum HttpManifestContractError {
    #[error("binding is not an HTTP binding")]
    NotHttp,
    #[error("binding and capability identities do not match")]
    CapabilityMismatch,
    #[error("capability does not declare the binding")]
    BindingNotDeclared,
    #[error("capability is unavailable")]
    Unavailable,
    #[error("deadline is outside the representable HTTP range")]
    DeadlineOutOfRange,
    #[error("operation does not support pagination")]
    PaginationUnsupported,
    #[error("page request exceeds the capability manifest")]
    InvalidPage,
    #[error("application outcome receipt does not match the capability manifest")]
    ReceiptMismatch,
    #[error("application outcome terminal state is not declared by the capability manifest")]
    TerminalStateNotDeclared,
}

impl HttpManifestContract {
    pub fn new(
        binding: &SurfaceBindingV1,
        capability: &CapabilityManifestV1,
    ) -> Result<Self, HttpManifestContractError> {
        Self::new_for_surface(binding, capability, BindingSurface::Http)
    }

    pub fn resolve(
        catalog: &CatalogSnapshotV1,
        profile_id: &ProfileId,
        surface: BindingSurface,
        operation: &SurfaceOperationName,
        protocol_revision: u32,
        negotiated_features: &BTreeSet<FeatureId>,
    ) -> Option<Self> {
        if !matches!(surface, BindingSurface::Http | BindingSurface::Dashboard) {
            return None;
        }
        let capability = catalog.resolve_binding(
            profile_id,
            surface,
            operation,
            protocol_revision,
            negotiated_features,
        )?;
        let binding = capability.binding_ids().iter().find_map(|binding_id| {
            catalog
                .binding(binding_id)
                .filter(|binding| binding.surface() == surface && binding.operation() == operation)
        })?;
        Self::new_for_surface(binding, capability, surface).ok()
    }

    fn new_for_surface(
        binding: &SurfaceBindingV1,
        capability: &CapabilityManifestV1,
        expected_surface: BindingSurface,
    ) -> Result<Self, HttpManifestContractError> {
        if binding.surface() != expected_surface
            || !matches!(
                expected_surface,
                BindingSurface::Http | BindingSurface::Dashboard
            )
        {
            return Err(HttpManifestContractError::NotHttp);
        }
        if binding.capability_id() != capability.capability_id() {
            return Err(HttpManifestContractError::CapabilityMismatch);
        }
        if capability
            .binding_ids()
            .binary_search(binding.binding_id())
            .is_err()
        {
            return Err(HttpManifestContractError::BindingNotDeclared);
        }
        if !capability.availability().is_callable() {
            return Err(HttpManifestContractError::Unavailable);
        }
        Ok(Self {
            binding: binding.clone(),
            capability: capability.clone(),
        })
    }

    pub fn binding(&self) -> &SurfaceBindingV1 {
        &self.binding
    }

    pub fn capability(&self) -> &CapabilityManifestV1 {
        &self.capability
    }

    pub fn maximum_deadline_micros(&self) -> Result<i64, HttpManifestContractError> {
        i64::try_from(self.capability.deadline().maximum_millis())
            .ok()
            .and_then(|millis| millis.checked_mul(1_000))
            .ok_or(HttpManifestContractError::DeadlineOutOfRange)
    }

    /// Resolve a caller deadline against the capability's declared maximum.
    ///
    /// Omission selects the manifest maximum. An earlier caller deadline stays
    /// earlier, including an already-expired deadline.
    pub fn effective_deadline(
        &self,
        observed_at: UtcMicros,
        caller_deadline: Option<&Deadline>,
    ) -> Result<Deadline, HttpManifestContractError> {
        let maximum = observed_at
            .0
            .checked_add(self.maximum_deadline_micros()?)
            .ok_or(HttpManifestContractError::DeadlineOutOfRange)?;
        let expires_at =
            caller_deadline.map_or(maximum, |deadline| deadline.expires_at.0.min(maximum));
        Deadline::new(UtcMicros(expires_at))
            .map_err(|_| HttpManifestContractError::DeadlineOutOfRange)
    }

    /// Build a canonical application page from HTTP query controls.
    ///
    /// The HTTP adapter supplies no independent default or upper bound.
    pub fn page_request(
        &self,
        requested_page_size: Option<u32>,
        cursor: Option<OpaqueCursor>,
    ) -> Result<Option<PageRequest>, HttpManifestContractError> {
        let Some(pagination) = self.capability.pagination() else {
            return if requested_page_size.is_none() && cursor.is_none() {
                Ok(None)
            } else {
                Err(HttpManifestContractError::PaginationUnsupported)
            };
        };
        let page_size = requested_page_size.unwrap_or(pagination.default_page_size());
        if page_size > pagination.maximum_page_size() {
            return Err(HttpManifestContractError::InvalidPage);
        }
        PageRequest::new(page_size, cursor)
            .map(Some)
            .map_err(|_| HttpManifestContractError::InvalidPage)
    }

    pub fn cancellation(&self) -> &CancellationContract {
        self.capability.cancellation()
    }

    pub const fn receipt(&self) -> ReceiptContract {
        self.capability.receipt()
    }

    pub fn allows_termination(&self, termination: OperationTermination) -> bool {
        self.capability
            .terminal_states()
            .contains(operation_terminal_state(termination))
    }

    /// Validate the receipt family and terminal state observed in an admitted
    /// application result before an HTTP adapter serializes it as success.
    pub fn validate_observed_receipt(
        &self,
        receipt: ReceiptContract,
        termination: OperationTermination,
    ) -> Result<(), HttpManifestContractError> {
        if receipt != self.receipt() {
            return Err(HttpManifestContractError::ReceiptMismatch);
        }
        if !self.allows_termination(termination) {
            return Err(HttpManifestContractError::TerminalStateNotDeclared);
        }
        Ok(())
    }

    pub fn validate_outcome<T>(
        &self,
        outcome: &ApplicationOutcome<T>,
    ) -> Result<(), HttpManifestContractError> {
        let (receipt, termination) = match outcome {
            ApplicationOutcome::Evidence(packet) => {
                (ReceiptContract::Operation, packet.execution.termination)
            }
            ApplicationOutcome::Preview(preview) => {
                (ReceiptContract::Operation, preview.execution.termination)
            }
            ApplicationOutcome::Effect(effect) => {
                (ReceiptContract::DurableEffect, effect.execution.termination)
            }
        };
        self.validate_observed_receipt(receipt, termination)
    }
}

const fn operation_terminal_state(termination: OperationTermination) -> TerminalState {
    match termination {
        OperationTermination::Completed => TerminalState::Completed,
        OperationTermination::Cancelled => TerminalState::Cancelled,
        OperationTermination::TimedOut => TerminalState::TimedOut,
        OperationTermination::Failed => TerminalState::Failed,
        OperationTermination::Unavailable => TerminalState::Unavailable,
        OperationTermination::Partial => TerminalState::Partial,
        OperationTermination::EffectUnknown => TerminalState::EffectUnknown,
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_application::OperationTermination;
    use tracedecay_tool_catalog::{
        AuthorityRequirement, AvailabilityContract, BindingId, BindingStatus, BindingSurface,
        CancellationContract, CancellationPoint, CapabilityId, CapabilityManifestInputV1,
        CapabilityManifestV1, DeadlineBehavior, DeadlineContract, DeniedDisclosurePolicy,
        EffectClass, IdempotencyContract, InverseContract, LifecycleClass, PaginationContract,
        PrivacyClass, ProtocolRevisionRange, ReceiptContract, ReconciliationContract,
        RevalidationContract, RoutingContractV1, SchemaId, SchemaRef, ScopeRequirement,
        StreamingContract, SurfaceBindingInputV1, SurfaceBindingV1, SurfaceOperationName,
        TerminalState, TerminalStateContract, UnavailabilityReason, UseCaseId,
    };

    use super::{HttpManifestContract, HttpManifestContractError};

    fn binding(binding_id: &str, capability_id: &str, surface: BindingSurface) -> SurfaceBindingV1 {
        SurfaceBindingV1::new(SurfaceBindingInputV1 {
            binding_id: BindingId::new(binding_id).unwrap(),
            capability_id: CapabilityId::new(capability_id).unwrap(),
            surface,
            operation: SurfaceOperationName::new("source_read").unwrap(),
            protocol_revisions: ProtocolRevisionRange::new(1, 1).unwrap(),
            required_features: Vec::new(),
            status: BindingStatus::Current,
            alias_of: None,
        })
        .unwrap()
    }

    fn manifest(
        capability_id: &str,
        binding_ids: Vec<&str>,
        availability: AvailabilityContract,
        terminal_states: Vec<TerminalState>,
    ) -> CapabilityManifestV1 {
        CapabilityManifestV1::new(CapabilityManifestInputV1 {
            capability_id: CapabilityId::new(capability_id).unwrap(),
            use_case_id: UseCaseId::new("source.read").unwrap(),
            routing: RoutingContractV1::new(
                1,
                "Read source",
                "Read bounded source evidence.",
                Vec::new(),
            )
            .unwrap(),
            request_schema: SchemaRef::new(SchemaId::new("source.read.request").unwrap(), 1)
                .unwrap(),
            result_schema: SchemaRef::new(SchemaId::new("source.read.result").unwrap(), 1).unwrap(),
            effect: EffectClass::Read,
            scope: ScopeRequirement::none(),
            authority: AuthorityRequirement::None,
            denied_disclosure: DeniedDisclosurePolicy::Explicit,
            privacy: PrivacyClass::PublicMetadata,
            lifecycle: LifecycleClass::Stateless,
            streaming: StreamingContract::Unsupported,
            cancellation: CancellationContract::cooperative(vec![
                CancellationPoint::BeforeAdmission,
                CancellationPoint::DuringRead,
            ])
            .unwrap(),
            deadline: DeadlineContract::new(2_500, DeadlineBehavior::ReturnOperationReceipt)
                .unwrap(),
            pagination: Some(PaginationContract::new(12, 40, 60_000).unwrap()),
            idempotency: IdempotencyContract::NotRequired,
            inverse: InverseContract::NotApplicable,
            authority_revalidation: RevalidationContract::NotRequired,
            reconciliation: ReconciliationContract::NotRequired,
            receipt: ReceiptContract::Operation,
            terminal_states: TerminalStateContract::new(terminal_states).unwrap(),
            availability,
            binding_ids: binding_ids
                .into_iter()
                .map(|id| BindingId::new(id).unwrap())
                .collect(),
            profile_eligibility: Vec::new(),
            required_features: Vec::new(),
        })
        .unwrap()
    }

    fn base_terminal_states() -> Vec<TerminalState> {
        vec![
            TerminalState::Completed,
            TerminalState::Cancelled,
            TerminalState::TimedOut,
            TerminalState::Failed,
            TerminalState::Partial,
        ]
    }

    #[test]
    fn rejects_non_http_binding() {
        let binding = binding(
            "binding.cli.source_read",
            "source.read",
            BindingSurface::Cli,
        );
        let capability = manifest(
            "source.read",
            vec!["binding.cli.source_read"],
            AvailabilityContract::Available,
            base_terminal_states(),
        );

        assert_eq!(
            HttpManifestContract::new(&binding, &capability).unwrap_err(),
            HttpManifestContractError::NotHttp
        );
    }

    #[test]
    fn rejects_binding_with_a_different_capability_identity() {
        let binding = binding(
            "binding.http.source_read",
            "source.other",
            BindingSurface::Http,
        );
        let capability = manifest(
            "source.read",
            vec!["binding.http.source_read"],
            AvailabilityContract::Available,
            base_terminal_states(),
        );

        assert_eq!(
            HttpManifestContract::new(&binding, &capability).unwrap_err(),
            HttpManifestContractError::CapabilityMismatch
        );
    }

    #[test]
    fn rejects_binding_not_declared_by_the_capability() {
        let binding = binding(
            "binding.http.source_read",
            "source.read",
            BindingSurface::Http,
        );
        let capability = manifest(
            "source.read",
            vec!["binding.http.other"],
            AvailabilityContract::Available,
            base_terminal_states(),
        );

        assert_eq!(
            HttpManifestContract::new(&binding, &capability).unwrap_err(),
            HttpManifestContractError::BindingNotDeclared
        );
    }

    #[test]
    fn rejects_an_unavailable_capability() {
        let binding = binding(
            "binding.http.source_read",
            "source.read",
            BindingSurface::Http,
        );
        let capability = manifest(
            "source.read",
            vec!["binding.http.source_read"],
            AvailabilityContract::Unavailable {
                reason: UnavailabilityReason::NotImplemented,
            },
            base_terminal_states(),
        );

        assert_eq!(
            HttpManifestContract::new(&binding, &capability).unwrap_err(),
            HttpManifestContractError::Unavailable
        );
    }

    #[test]
    fn derives_lifecycle_constraints_from_the_capability_manifest() {
        let binding = binding(
            "binding.http.source_read",
            "source.read",
            BindingSurface::Http,
        );
        let capability = manifest(
            "source.read",
            vec!["binding.http.source_read"],
            AvailabilityContract::Available,
            base_terminal_states(),
        );
        let contract = HttpManifestContract::new(&binding, &capability).unwrap();

        assert_eq!(contract.maximum_deadline_micros().unwrap(), 2_500_000);
        assert_eq!(
            contract.cancellation().points(),
            &[
                CancellationPoint::BeforeAdmission,
                CancellationPoint::DuringRead,
            ]
        );
        assert_eq!(contract.receipt(), ReceiptContract::Operation);

        let default_page = contract.page_request(None, None).unwrap().unwrap();
        assert_eq!(default_page.page_size, 12);
        let bounded_page = contract.page_request(Some(40), None).unwrap().unwrap();
        assert_eq!(bounded_page.page_size, 40);
        assert!(contract.page_request(Some(41), None).is_err());
    }

    #[test]
    fn maps_every_operation_termination_to_the_manifest_terminal_state() {
        let binding = binding(
            "binding.http.source_read",
            "source.read",
            BindingSurface::Http,
        );
        let mut terminal_states = base_terminal_states();
        terminal_states.push(TerminalState::Unavailable);
        let capability = manifest(
            "source.read",
            vec!["binding.http.source_read"],
            AvailabilityContract::Available,
            terminal_states,
        );
        let contract = HttpManifestContract::new(&binding, &capability).unwrap();

        let expectations = [
            (OperationTermination::Completed, true),
            (OperationTermination::Cancelled, true),
            (OperationTermination::TimedOut, true),
            (OperationTermination::Failed, true),
            (OperationTermination::Unavailable, true),
            (OperationTermination::Partial, true),
            (OperationTermination::EffectUnknown, false),
        ];
        for (termination, expected) in expectations {
            assert_eq!(
                contract.allows_termination(termination),
                expected,
                "unexpected contract decision for {termination:?}"
            );
        }
    }
}
