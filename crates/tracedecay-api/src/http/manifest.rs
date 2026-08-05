use std::collections::BTreeSet;

use thiserror::Error;
use tracedecay_application::{
    ApplicationOutcome, CancellationStage, Deadline, OpaqueCursor, OperationReceipt,
    OperationTermination, PageRequest,
};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::{
    BindingSurface, CancellationContract, CancellationPoint, CapabilityManifestV1,
    CatalogSnapshotV1, DeadlineBehavior, FeatureId, ProfileId, ReceiptContract, SurfaceBindingV1,
    SurfaceOperationName, TerminalState,
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
    #[error("the caller or admission authority must supply an explicit deadline")]
    CallerDeadlineRequired,
    #[error("the observed effective deadline exceeds the capability manifest")]
    EffectiveDeadlineExceedsManifest,
    #[error("operation receipt deadline differs from the admitted effective deadline")]
    EffectiveDeadlineMismatch,
    #[error("the admitted terminal state conflicts with the manifest deadline behavior")]
    DeadlineBehaviorMismatch,
    #[error("operation does not support pagination")]
    PaginationUnsupported,
    #[error("page request exceeds the capability manifest")]
    InvalidPage,
    #[error(
        "opaque cursor requires authenticated cursor authority within {cursor_ttl_millis} milliseconds"
    )]
    CursorAuthorityRequired { cursor_ttl_millis: u64 },
    #[error("application outcome contains an invalid operation receipt")]
    InvalidReceipt,
    #[error("application outcome receipt does not match the capability manifest")]
    ReceiptMismatch,
    #[error("application outcome terminal state is not declared by the capability manifest")]
    TerminalStateNotDeclared,
    #[error("application outcome records cancellation for a non-cancellable capability")]
    CancellationNotSupported,
    #[error("application outcome cancellation stage is not declared by the capability manifest")]
    CancellationStageNotDeclared,
}

impl HttpManifestContract {
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

    pub fn deadline_behavior(&self) -> DeadlineBehavior {
        self.capability.deadline().behavior()
    }

    /// Resolve a caller deadline against the capability's declared maximum.
    ///
    /// A maximum duration does not define omission semantics, so the caller or
    /// admission authority must supply an explicit deadline. A deadline beyond
    /// the manifest ceiling is bounded to the declared maximum.
    pub fn effective_deadline(
        &self,
        observed_at: UtcMicros,
        caller_deadline: Option<&Deadline>,
    ) -> Result<Deadline, HttpManifestContractError> {
        let caller_deadline =
            caller_deadline.ok_or(HttpManifestContractError::CallerDeadlineRequired)?;
        let maximum = observed_at
            .0
            .checked_add(self.maximum_deadline_micros()?)
            .ok_or(HttpManifestContractError::DeadlineOutOfRange)?;
        let expires_at = caller_deadline.expires_at.0.min(maximum);
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
        if cursor.is_some() {
            return Err(HttpManifestContractError::CursorAuthorityRequired {
                cursor_ttl_millis: pagination.cursor_ttl_millis(),
            });
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
        admitted_effective_deadline: &Deadline,
    ) -> Result<(), HttpManifestContractError> {
        let (receipt_contract, execution) = match outcome {
            ApplicationOutcome::Evidence(packet) => (ReceiptContract::Operation, &packet.execution),
            ApplicationOutcome::Preview(preview) => {
                (ReceiptContract::Operation, &preview.execution)
            }
            ApplicationOutcome::Effect(effect) => {
                (ReceiptContract::DurableEffect, &effect.execution)
            }
        };
        self.validate_observed_receipt(receipt_contract, execution.termination)?;
        self.validate_deadline_behavior(receipt_contract, execution.termination)?;
        self.validate_operation_receipt(execution, admitted_effective_deadline)
    }

    fn validate_deadline_behavior(
        &self,
        receipt: ReceiptContract,
        termination: OperationTermination,
    ) -> Result<(), HttpManifestContractError> {
        match (self.deadline_behavior(), receipt, termination) {
            (DeadlineBehavior::RejectBeforeAdmission, _, OperationTermination::TimedOut) => {
                Err(HttpManifestContractError::DeadlineBehaviorMismatch)
            }
            (DeadlineBehavior::RejectBeforeAdmission, _, _)
            | (DeadlineBehavior::ReturnOperationReceipt, ReceiptContract::Operation, _)
            | (DeadlineBehavior::ReturnEffectReceipt, ReceiptContract::DurableEffect, _) => Ok(()),
            _ => Err(HttpManifestContractError::DeadlineBehaviorMismatch),
        }
    }

    fn validate_operation_receipt(
        &self,
        receipt: &OperationReceipt,
        admitted_effective_deadline: &Deadline,
    ) -> Result<(), HttpManifestContractError> {
        receipt
            .validate()
            .map_err(|_| HttpManifestContractError::InvalidReceipt)?;
        if receipt.effective_deadline != *admitted_effective_deadline {
            return Err(HttpManifestContractError::EffectiveDeadlineMismatch);
        }

        let maximum_deadline = receipt
            .started_at
            .0
            .checked_add(self.maximum_deadline_micros()?)
            .ok_or(HttpManifestContractError::DeadlineOutOfRange)?;
        if receipt.effective_deadline.expires_at.0 > maximum_deadline {
            return Err(HttpManifestContractError::EffectiveDeadlineExceedsManifest);
        }
        if receipt.effective_deadline.expires_at <= receipt.started_at
            && receipt.termination != OperationTermination::TimedOut
        {
            return Err(HttpManifestContractError::InvalidReceipt);
        }

        let Some(cancellation) = &receipt.cancellation else {
            return Ok(());
        };
        if cancellation.observed_at < receipt.started_at
            || cancellation.observed_at > receipt.ended_at
        {
            return Err(HttpManifestContractError::InvalidReceipt);
        }
        if receipt.termination == OperationTermination::TimedOut {
            return Ok(());
        }
        let cancellation_point = cancellation_point(cancellation.stage);
        match self.cancellation() {
            CancellationContract::NotCancellable => {
                Err(HttpManifestContractError::CancellationNotSupported)
            }
            CancellationContract::Cooperative { .. }
                if !self.cancellation().observes(cancellation_point) =>
            {
                Err(HttpManifestContractError::CancellationStageNotDeclared)
            }
            CancellationContract::Cooperative { .. } => Ok(()),
        }
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

const fn cancellation_point(stage: CancellationStage) -> CancellationPoint {
    match stage {
        CancellationStage::BeforeAdmission => CancellationPoint::BeforeAdmission,
        CancellationStage::BeforeRead => CancellationPoint::BeforeRead,
        CancellationStage::DuringRead => CancellationPoint::DuringRead,
        CancellationStage::BeforeEffect => CancellationPoint::BeforeEffect,
        CancellationStage::EffectInFlight => CancellationPoint::EffectInFlight,
        CancellationStage::Reconciling => CancellationPoint::Reconciling,
        CancellationStage::AfterCommit => CancellationPoint::AfterCommit,
    }
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;
