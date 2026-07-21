//! Shared adapter-to-daemon dispatch contracts.
//!
//! This module deliberately owns request correlation and transport-neutral
//! admission/reconnect seams only. It does not invoke application services,
//! query stores, or render results.

use std::collections::BTreeMap;
use std::sync::Mutex;

use tracedecay_application::{
    ApplicationProblem, ApplicationProblemKind, CancellationContext, Deadline, LegalAction,
    OpaqueCursor, OperationTermination, PageRequest, RequestId, RetryDirective, SafeDiagnostic,
    StreamEvent, StreamEventKind, StreamTermination,
};
use tracedecay_domain::ProjectId;
use tracedecay_tool_catalog::{
    BindingId, BindingSurface, CatalogSnapshotV1, FeatureId, ProfileId, SchemaRef,
    SurfaceOperationName,
};

/// A path-free project selector accepted by adapters before daemon resolution.
///
/// Paths, labels, and other mutable local spellings are deliberately absent:
/// the daemon resolves this selector to its authoritative scope before any
/// application operation is admitted.
pub enum ScopeSelector {
    CurrentProject,
    Project(ProjectId),
}

/// Presentation-only format requested by an adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestedOutputFormat {
    Markdown,
    Json,
}

/// The shared cancellation reference carried into an application invocation.
pub type CancellationRef = CancellationContext;

/// The transport-neutral invocation constructed by CLI and MCP adapters.
///
/// `requested_format` is intentionally carried only until
/// [`BoundInvocation::into_application_invocation`] is called. The resulting
/// application invocation has no presentation-format field.
pub struct CanonicalInvocation<T> {
    pub request: T,
    pub scope: ScopeSelector,
    pub page: PageRequest,
    pub deadline: Option<Deadline>,
    pub cancellation: CancellationRef,
    pub requested_format: RequestedOutputFormat,
}

/// Common invocation controls after transport syntax validation.
pub struct InvocationControls {
    pub scope: ScopeSelector,
    pub page: PageRequest,
    pub deadline: Option<Deadline>,
    pub cancellation: CancellationRef,
    pub requested_format: RequestedOutputFormat,
}

/// Transport-decoded input to the one canonical binding dispatcher.
pub struct DispatchInput<T> {
    pub request_id: RequestId,
    pub binding: BindingResolution,
    pub request: T,
    pub controls: InvocationControls,
}

/// A non-disclosing binding-resolution failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchError {
    UnknownOrNotAuthorized,
}

impl<T> CanonicalInvocation<T> {
    pub fn new(
        request: T,
        scope: ScopeSelector,
        page: PageRequest,
        deadline: Option<Deadline>,
        cancellation: CancellationRef,
        requested_format: RequestedOutputFormat,
    ) -> Self {
        Self {
            request,
            scope,
            page,
            deadline,
            cancellation,
            requested_format,
        }
    }
}

/// A canonical invocation after the adapter has resolved its catalog binding.
pub struct BoundInvocation<T> {
    pub binding_id: BindingId,
    pub request_schema: SchemaRef,
    pub result_schema: SchemaRef,
    pub invocation: CanonicalInvocation<T>,
}

impl<T> BoundInvocation<T> {
    pub fn new(binding: ResolvedBinding, invocation: CanonicalInvocation<T>) -> Self {
        Self {
            binding_id: binding.binding_id,
            request_schema: binding.request_schema,
            result_schema: binding.result_schema,
            invocation,
        }
    }

    /// Separates presentation from the application call boundary.
    pub fn into_application_invocation(self) -> (ApplicationInvocation<T>, RequestedOutputFormat) {
        let Self {
            binding_id,
            request_schema: _,
            result_schema: _,
            invocation,
        } = self;
        let CanonicalInvocation {
            request,
            scope,
            page,
            deadline,
            cancellation,
            requested_format,
        } = invocation;

        (
            ApplicationInvocation {
                binding_id,
                request,
                scope,
                page,
                deadline,
                cancellation,
            },
            requested_format,
        )
    }
}

/// The data permitted to cross from an adapter into the application boundary.
///
/// This type deliberately omits presentation format and transport request
/// framing.
pub struct ApplicationInvocation<T> {
    pub binding_id: BindingId,
    pub request: T,
    pub scope: ScopeSelector,
    pub page: PageRequest,
    pub deadline: Option<Deadline>,
    pub cancellation: CancellationRef,
}

/// Catalog inputs needed to resolve a surface operation to one binding ID.
pub struct BindingResolution {
    pub profile_id: ProfileId,
    pub operation: SurfaceOperationName,
    pub protocol_revision: u32,
    pub negotiated_features: std::collections::BTreeSet<FeatureId>,
}

/// Catalog binding plus the canonical schema references indexed for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedBinding {
    pub binding_id: BindingId,
    pub request_schema: SchemaRef,
    pub result_schema: SchemaRef,
}

/// Resolves a visible, callable surface binding without exposing why lookup
/// failed. `None` intentionally conflates unknown, hidden, unavailable, and
/// incompatible operations.
pub trait BindingResolver {
    fn resolve_binding(
        &self,
        surface: BindingSurface,
        request: &BindingResolution,
    ) -> Option<ResolvedBinding>;
}

/// Metadata-only resolver backed by one immutable catalog snapshot.
pub struct CatalogBindingResolver<'a> {
    catalog: &'a CatalogSnapshotV1,
}

impl<'a> CatalogBindingResolver<'a> {
    pub fn new(catalog: &'a CatalogSnapshotV1) -> Self {
        Self { catalog }
    }
}

impl BindingResolver for CatalogBindingResolver<'_> {
    fn resolve_binding(
        &self,
        surface: BindingSurface,
        request: &BindingResolution,
    ) -> Option<ResolvedBinding> {
        let capability = self.catalog.resolve_binding(
            &request.profile_id,
            surface,
            &request.operation,
            request.protocol_revision,
            &request.negotiated_features,
        )?;

        let binding_id = capability.binding_ids().iter().find_map(|binding_id| {
            let binding = self.catalog.binding(binding_id)?;
            (binding.surface() == surface && binding.operation() == &request.operation)
                .then(|| binding_id.clone())
        })?;
        let request_schema = self
            .catalog
            .schema(
                capability.request_schema().schema_id(),
                capability.request_schema().revision(),
            )?
            .clone();
        let result_schema = self
            .catalog
            .schema(
                capability.result_schema().schema_id(),
                capability.result_schema().revision(),
            )?
            .clone();

        Some(ResolvedBinding {
            binding_id,
            request_schema,
            result_schema,
        })
    }
}

/// Resolve one transport binding and construct the canonical invocation.
///
/// The surface is selected by adapter code, never decoded from user input.
pub fn resolve_dispatch<T>(
    resolver: &impl BindingResolver,
    surface: BindingSurface,
    input: DispatchInput<T>,
) -> Result<DispatchedInvocation<T>, DispatchError> {
    let DispatchInput {
        request_id,
        binding,
        request,
        controls,
    } = input;
    let resolved = resolver
        .resolve_binding(surface, &binding)
        .ok_or(DispatchError::UnknownOrNotAuthorized)?;
    let invocation = CanonicalInvocation::new(
        request,
        controls.scope,
        controls.page,
        controls.deadline,
        controls.cancellation,
        controls.requested_format,
    );

    Ok(DispatchedInvocation::new(
        request_id,
        BoundInvocation::new(resolved, invocation),
    ))
}

/// An invocation paired with the request identity used for daemon correlation.
pub struct DispatchedInvocation<T> {
    pub request_id: RequestId,
    pub invocation: BoundInvocation<T>,
}

impl<T> DispatchedInvocation<T> {
    pub fn new(request_id: RequestId, invocation: BoundInvocation<T>) -> Self {
        Self {
            request_id,
            invocation,
        }
    }

    pub fn correlation(&self, class: DaemonAdmissionClass) -> RequestCorrelation {
        RequestCorrelation {
            request_id: self.request_id.clone(),
            binding_id: self.invocation.binding_id.clone(),
            class,
        }
    }
}

/// The daemon admission lanes recognized by the multiplexed-client seam.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DaemonAdmissionClass {
    General,
    ReservedControl,
}

/// Typed pre-admission states. These states must not be stringified or
/// collapsed into a terminal application receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionOutcome {
    Admitted,
    CancelledBeforeAdmission,
    TimedOutBeforeAdmission,
    Unavailable,
    Saturated { class: DaemonAdmissionClass },
}

impl AdmissionOutcome {
    /// Maps only pre-admission states to the canonical Plan 09 problem shape.
    /// An admitted request has no pre-admission problem.
    pub fn into_application_problem(self) -> Option<ApplicationProblem> {
        match self {
            Self::Admitted => None,
            Self::CancelledBeforeAdmission => {
                Some(ApplicationProblem::cancelled_before_admission())
            }
            Self::TimedOutBeforeAdmission => Some(ApplicationProblem::timed_out_before_admission()),
            Self::Unavailable => Some(ApplicationProblem::unavailable(SafeDiagnostic {
                code: "daemon_unavailable".to_owned(),
                message: "The owning TraceDecay daemon is unavailable".to_owned(),
            })),
            Self::Saturated { class } => Some(ApplicationProblem::Saturated {
                diagnostic: SafeDiagnostic {
                    code: match class {
                        DaemonAdmissionClass::General => "daemon_general_capacity_saturated",
                        DaemonAdmissionClass::ReservedControl => {
                            "daemon_control_capacity_saturated"
                        }
                    }
                    .to_owned(),
                    message: "The owning TraceDecay daemon has no admission capacity".to_owned(),
                },
                retry: RetryDirective::AfterDelay,
                legal_actions: vec![LegalAction::Retry],
            }),
        }
    }
}

/// A policy seam for class-aware daemon admission.
pub trait AdmissionPolicy {
    fn admit(&self, correlation: &RequestCorrelation) -> AdmissionOutcome;
}

/// One request's identity while an adapter waits for or reconnects to the
/// daemon. No request payload is retained here.
#[derive(Clone, Debug)]
pub struct RequestCorrelation {
    pub request_id: RequestId,
    pub binding_id: BindingId,
    pub class: DaemonAdmissionClass,
}

/// Typed reconnect state for a client transport hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconnectOutcome {
    Reconnected,
    Unavailable,
}

/// A reconnect hook supplied by the daemon transport implementation.
pub trait ReconnectHook {
    fn reconnect(&self) -> ReconnectOutcome;
}

/// Receipt lookup states after a reconnect.
///
/// `Terminal` preserves the Plan 09 operation termination exactly, including
/// `Partial` and `EffectUnknown`; adapters must not substitute a locally
/// observed disconnect or timeout for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceiptInspection {
    Pending,
    Terminal(OperationTermination),
    Unavailable,
    Saturated { class: DaemonAdmissionClass },
    UnknownRequest,
}

/// A receipt-inspection hook supplied by the daemon transport implementation.
pub trait ReceiptInspector {
    fn inspect_receipt(&self, correlation: &RequestCorrelation) -> ReceiptInspection;
}

/// Failures confined to the local correlation registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CorrelationRegistryError {
    DuplicateRequest,
    CapacityExceeded,
    RegistryUnavailable,
}

const MAX_IN_FLIGHT_CORRELATIONS: usize = 4_096;

/// A request-correlation registry shared by CLI and MCP adapters.
///
/// It intentionally has no socket, store, query, or application-service
/// dependency. A later transport owns dispatch I/O and supplies the hooks
/// above.
#[derive(Default)]
pub struct MultiplexedDaemonClient {
    correlations: Mutex<BTreeMap<RequestId, RequestCorrelation>>,
}

impl MultiplexedDaemonClient {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &self,
        correlation: RequestCorrelation,
    ) -> Result<(), CorrelationRegistryError> {
        let mut correlations = self
            .correlations
            .lock()
            .map_err(|_| CorrelationRegistryError::RegistryUnavailable)?;
        if correlations.contains_key(&correlation.request_id) {
            return Err(CorrelationRegistryError::DuplicateRequest);
        }
        if correlations.len() >= MAX_IN_FLIGHT_CORRELATIONS {
            return Err(CorrelationRegistryError::CapacityExceeded);
        }
        correlations.insert(correlation.request_id.clone(), correlation);
        Ok(())
    }

    /// Releases client-side correlation state after the caller has observed a
    /// canonical terminal receipt. This does not delete the daemon receipt.
    pub fn finish(
        &self,
        request_id: &RequestId,
    ) -> Result<Option<RequestCorrelation>, CorrelationRegistryError> {
        let mut correlations = self
            .correlations
            .lock()
            .map_err(|_| CorrelationRegistryError::RegistryUnavailable)?;
        Ok(correlations.remove(request_id))
    }

    pub fn correlation(
        &self,
        request_id: &RequestId,
    ) -> Result<Option<RequestCorrelation>, CorrelationRegistryError> {
        let correlations = self
            .correlations
            .lock()
            .map_err(|_| CorrelationRegistryError::RegistryUnavailable)?;
        Ok(correlations.get(request_id).cloned())
    }

    pub fn admit(
        &self,
        policy: &impl AdmissionPolicy,
        correlation: &RequestCorrelation,
    ) -> AdmissionOutcome {
        policy.admit(correlation)
    }

    /// Reconnects and asks the transport for the daemon's canonical receipt
    /// state. A reconnect failure never fabricates a terminal outcome.
    pub fn reconnect_and_inspect(
        &self,
        request_id: &RequestId,
        reconnect: &impl ReconnectHook,
        inspector: &impl ReceiptInspector,
    ) -> Result<ReceiptInspection, CorrelationRegistryError> {
        let Some(correlation) = self.correlation(request_id)? else {
            return Ok(ReceiptInspection::UnknownRequest);
        };

        match reconnect.reconnect() {
            ReconnectOutcome::Reconnected => Ok(inspector.inspect_receipt(&correlation)),
            ReconnectOutcome::Unavailable => Ok(ReceiptInspection::Unavailable),
        }
    }
}

/// The canonical problem category for adapter presentation.
pub fn canonical_problem_kind(problem: &ApplicationProblem) -> ApplicationProblemKind {
    problem.kind()
}

/// Returns the one public shape shared by unknown, absent, and unauthorized
/// bindings. It deliberately contains no request, argument, or resource value.
pub fn concealed_not_found_or_not_authorized() -> ApplicationProblem {
    ApplicationProblem::not_found_or_not_authorized(tracedecay_application::RetryDirective::Never)
}

/// The opaque cursor bytes to carry unchanged across adapter boundaries.
pub fn canonical_cursor(page: &PageRequest) -> Option<&OpaqueCursor> {
    page.cursor.as_ref()
}

/// Extracts a receipt-bearing terminal event without flattening stream state.
pub fn canonical_stream_termination<T>(event: &StreamEvent<T>) -> Option<&StreamTermination> {
    match &event.kind {
        StreamEventKind::Terminal(termination) => Some(termination),
        StreamEventKind::Item(_) | StreamEventKind::Progress { .. } | StreamEventKind::Gap(_) => {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AdmissionOutcome;
    use tracedecay_application::{ApplicationProblem, ApplicationProblemKind, RetryDirective};

    #[test]
    fn pre_admission_outcomes_keep_canonical_problem_categories() {
        for (outcome, expected) in [
            (
                AdmissionOutcome::CancelledBeforeAdmission,
                ApplicationProblemKind::Cancelled,
            ),
            (
                AdmissionOutcome::TimedOutBeforeAdmission,
                ApplicationProblemKind::TimedOut,
            ),
            (
                AdmissionOutcome::Unavailable,
                ApplicationProblemKind::Unavailable,
            ),
        ] {
            assert_eq!(
                outcome
                    .into_application_problem()
                    .expect("pre-admission problem")
                    .kind(),
                expected
            );
        }
        assert!(
            AdmissionOutcome::Admitted
                .into_application_problem()
                .is_none()
        );
    }

    #[test]
    fn saturation_mapping_preserves_retry_without_resource_detail() {
        let problem = AdmissionOutcome::Saturated {
            class: super::DaemonAdmissionClass::General,
        }
        .into_application_problem()
        .expect("saturation problem");

        assert!(matches!(
            problem,
            ApplicationProblem::Saturated {
                retry: RetryDirective::AfterDelay,
                legal_actions,
                ..
            } if legal_actions == vec![tracedecay_application::LegalAction::Retry]
        ));
    }
}
