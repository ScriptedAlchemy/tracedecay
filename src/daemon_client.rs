//! Shared adapter-to-daemon dispatch contracts.
//!
//! This module deliberately owns request correlation and transport-neutral
//! admission/reconnect seams only. It does not invoke application services,
//! query stores, or render results.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::sync::Mutex as AsyncMutex;

use tracedecay_application::{
    ApplicationEnvelope, ApplicationInvocation, ApplicationInvocationExecutor,
    ApplicationInvocationFuture, ApplicationProblem, ApplicationProblemKind, ApplicationRequest,
    ApplicationResponse, CancellationSignal, CancellationStage, Deadline, InvocationError,
    InvocationTarget, LegalAction, OpaqueCursor, PageRequest, RequestId, RetryDirective,
    SafeDiagnostic, StreamEvent, StreamEventKind, StreamTermination,
};
use tracedecay_domain::{ManifestDigest, UtcMicros};
use tracedecay_lsp::{FramePoll, FrameSend};
use tracedecay_tool_catalog::{
    BindingId, BindingSurface, CatalogSnapshotV1, FeatureId, ProfileId, SchemaRef,
    SurfaceOperationName,
};

use crate::application::feedback::observations::{
    Plan26DeliveryRouteV1, Plan26FeedbackSourceEventV1,
};
use crate::request_identity::{
    ConnectionLocalRequestSequence, GlobalRequestSurface, mint_global_request_id,
};

pub type ScopeSelector = InvocationTarget;

/// Presentation-only format requested by an adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestedOutputFormat {
    Markdown,
    Json,
}

/// The shared cancellation reference carried into an application invocation.
pub type CancellationRef = CancellationSignal;

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
    pub fn into_application_invocation(self) -> (AdapterInvocation<T>, RequestedOutputFormat) {
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
            AdapterInvocation {
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
pub struct AdapterInvocation<T> {
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
        surface,
        BoundInvocation::new(resolved, invocation),
    ))
}

/// The daemon admission lanes used by the live invocation error mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DaemonAdmissionClass {
    General,
    ReservedControl,
}

/// An invocation paired with the request identity used for daemon dispatch.
pub struct DispatchedInvocation<T> {
    pub request_id: RequestId,
    pub surface: BindingSurface,
    pub invocation: BoundInvocation<T>,
}

impl<T> DispatchedInvocation<T> {
    pub fn new(
        request_id: RequestId,
        surface: BindingSurface,
        invocation: BoundInvocation<T>,
    ) -> Self {
        Self {
            request_id,
            surface,
            invocation,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvocationCancellationPolicy {
    ReadOnly,
    AuthoritativeEffect,
}

impl InvocationCancellationPolicy {
    pub(crate) const fn may_interrupt(self, stage: CancellationStage) -> bool {
        match self {
            Self::ReadOnly => true,
            Self::AuthoritativeEffect => matches!(stage, CancellationStage::BeforeAdmission),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DaemonInvocationError {
    Cancelled {
        stage: CancellationStage,
    },
    TimedOut {
        stage: CancellationStage,
    },
    #[allow(dead_code)] // reserved backpressure state — staged
    Saturated {
        class: DaemonAdmissionClass,
    },
    #[allow(dead_code)] // reserved backpressure state — staged
    Backpressured {
        stage: CancellationStage,
    },
    Unavailable,
}

impl DaemonInvocationError {
    pub(crate) fn into_application_problem(self) -> ApplicationProblem {
        match self {
            Self::Cancelled { .. } => ApplicationProblem::cancelled_before_admission(),
            Self::TimedOut { .. } => ApplicationProblem::timed_out_before_admission(),
            Self::Saturated { class } => ApplicationProblem::Saturated {
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
            },
            Self::Backpressured { stage } => ApplicationProblem::Saturated {
                diagnostic: SafeDiagnostic {
                    code: format!("daemon_backpressured_{}", cancellation_stage_name(stage)),
                    message: "The owning TraceDecay daemon applied request backpressure".to_owned(),
                },
                retry: RetryDirective::AfterDelay,
                legal_actions: vec![LegalAction::Retry],
            },
            Self::Unavailable => ApplicationProblem::unavailable(SafeDiagnostic {
                code: "daemon_unavailable".to_owned(),
                message: "The owning TraceDecay daemon is unavailable".to_owned(),
            }),
        }
    }
}

const fn cancellation_stage_name(stage: CancellationStage) -> &'static str {
    match stage {
        CancellationStage::BeforeAdmission => "before_admission",
        CancellationStage::BeforeRead => "before_read",
        CancellationStage::DuringRead => "during_read",
        CancellationStage::BeforeEffect => "before_effect",
        CancellationStage::EffectInFlight => "effect_in_flight",
        CancellationStage::Reconciling => "reconciling",
        CancellationStage::AfterCommit => "after_commit",
    }
}

pub type DaemonInvocationExecutorFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Production execution boundary for the daemon's closed invocation protocol.
///
/// Socket clients and daemon-local project servers implement this same port.
/// Request correlation is already present on `DaemonInvocationRequest`; effect
/// idempotency remains owned by each operation payload and is never reminted
/// by this transport boundary.
pub trait DaemonInvocationExecutor: ApplicationInvocationExecutor + Send + Sync {
    fn invoke_controlled(
        &self,
        request: crate::daemon::DaemonInvocationRequest,
        deadline: Deadline,
        cancellation: CancellationSignal,
        policy: InvocationCancellationPolicy,
    ) -> DaemonInvocationExecutorFuture<
        '_,
        Result<crate::daemon::DaemonInvocationResponse, DaemonInvocationError>,
    >;

    fn observe_plan26_feedback(
        &self,
        subject_digest: ManifestDigest,
        observed_at: UtcMicros,
        event: Plan26FeedbackSourceEventV1,
    ) -> DaemonInvocationExecutorFuture<'_, crate::errors::Result<()>>;
}

/// Authenticated socket client for the daemon's closed invocation protocol.
///
/// This client shares the daemon connection/authentication path with MCP but
/// sends only versioned invocation envelopes. It deliberately cannot issue an
/// arbitrary daemon method or reconstruct a Git/feedback application request.
#[derive(Clone)]
pub struct DaemonInvocationClient {
    connection: crate::daemon::DaemonConnection,
    handshake: crate::daemon::DaemonHandshake,
    state: Arc<AsyncMutex<Option<DaemonInvocationConnection>>>,
}

struct DaemonInvocationConnection {
    reader: BufReader<ReadHalf<crate::daemon::transport::BrokerStream>>,
    writer: WriteHalf<crate::daemon::transport::BrokerStream>,
}

impl DaemonInvocationClient {
    pub fn for_current(handshake: crate::daemon::DaemonHandshake) -> crate::errors::Result<Self> {
        Ok(Self {
            connection: crate::daemon::current_daemon_connection()?,
            handshake,
            state: Arc::new(AsyncMutex::new(None)),
        })
    }

    #[cfg(test)]
    pub(crate) fn for_connection_for_test(
        connection: crate::daemon::DaemonConnection,
        handshake: crate::daemon::DaemonHandshake,
    ) -> Self {
        Self {
            connection,
            handshake,
            state: Arc::new(AsyncMutex::new(None)),
        }
    }

    pub(crate) async fn invoke(
        &self,
        request: crate::daemon::DaemonInvocationRequest,
    ) -> crate::errors::Result<crate::daemon::DaemonInvocationResponse> {
        let request_id = request.request_id.clone();
        let request_label = request.operation().as_str();
        let mut state = self.state.lock().await;
        if state.is_none() {
            let stream = crate::daemon::connect_to_daemon_connection(&self.connection).await?;
            let (reader, mut writer) = stream.into_split();
            crate::daemon::write_daemon_preamble(&mut writer, &self.connection, &self.handshake)
                .await?;
            *state = Some(DaemonInvocationConnection {
                reader: BufReader::new(reader),
                writer,
            });
        }
        let result = async {
            let connection = state.as_mut().ok_or_else(|| crate::errors::TraceDecayError::Config {
                message: "daemon invocation connection was not initialized".to_owned(),
            })?;
            connection
                .writer
                .write_all(serde_json::to_string(&request)?.as_bytes())
                .await?;
            connection.writer.write_all(b"\n").await?;
            connection.writer.flush().await?;

            let Some(line) = crate::daemon::next_daemon_response_line(
                &mut connection.reader,
                &self.connection,
                request_label,
                crate::daemon::DAEMON_TOOL_LIVENESS_POLL_INTERVAL,
            )
            .await?
            else {
                return Err(crate::errors::TraceDecayError::Config {
                    message: format!(
                        "daemon closed the invocation connection after '{request_label}' was sent; the outcome is unknown"
                    ),
                });
            };
            let response: crate::daemon::DaemonInvocationResponse =
                serde_json::from_str(&line).map_err(|_| crate::errors::TraceDecayError::Config {
                    message: "daemon returned an invalid invocation response".to_owned(),
                })?;
            if response.protocol != crate::daemon::DAEMON_INVOCATION_PROTOCOL
                || response.revision != crate::daemon::DAEMON_INVOCATION_REVISION
                || response.request_id != request_id
            {
                return Err(crate::errors::TraceDecayError::Config {
                    message: "daemon invocation response did not match the request".to_owned(),
                });
            }
            Ok(response)
        }
        .await;
        if result.is_err() {
            *state = None;
        }
        result
    }

    pub async fn observe_plan26_feedback(
        &self,
        subject_digest: ManifestDigest,
        observed_at: UtcMicros,
        event: Plan26FeedbackSourceEventV1,
    ) -> crate::errors::Result<()> {
        let request_id = mint_global_request_id(GlobalRequestSurface::FeedbackObservation)
            .map_err(|error| crate::errors::TraceDecayError::Config {
                message: error.to_string(),
            })?;
        let response = self
            .invoke(
                crate::daemon::DaemonInvocationRequest::feedback_observation(
                    request_id.as_str(),
                    subject_digest,
                    observed_at,
                    event,
                ),
            )
            .await?;
        if matches!(
            response.outcome,
            crate::daemon::DaemonInvocationOutcome::ObservationAccepted
        ) {
            Ok(())
        } else {
            Err(crate::errors::TraceDecayError::Config {
                message: "daemon did not accept the feedback observation".to_owned(),
            })
        }
    }

    pub async fn evaluate_and_publish_semantic_profile(
        &self,
        candidate: crate::application::semantic_runtime::SemanticEvaluationProfileCandidateV1,
    ) -> crate::errors::Result<SemanticEvaluationPublicationResultV1> {
        let request_id =
            mint_global_request_id(GlobalRequestSurface::SemanticEvaluation).map_err(|error| {
                crate::errors::TraceDecayError::Config {
                    message: error.to_string(),
                }
            })?;
        let response = self
            .invoke(
                crate::daemon::DaemonInvocationRequest::semantic_evaluate_and_publish(
                    request_id.as_str(),
                    candidate,
                ),
            )
            .await?;
        match response.outcome {
            crate::daemon::DaemonInvocationOutcome::SemanticEvaluatedProfilePublished {
                scope,
                profile_digest,
                report_digest,
                report,
                source_generation,
                snapshot_digest,
            } => Ok(SemanticEvaluationPublicationResultV1 {
                project_id: scope.project_id.as_str().to_owned(),
                profile_digest: profile_digest.as_str().to_owned(),
                report_digest: report_digest.as_str().to_owned(),
                report,
                source_generation: source_generation.as_str().to_owned(),
                snapshot_digest: snapshot_digest.as_str().to_owned(),
            }),
            crate::daemon::DaemonInvocationOutcome::Problem { problem } => {
                Err(crate::errors::TraceDecayError::Config {
                    message: format!("semantic evaluation publication rejected: {problem:?}"),
                })
            }
            _ => Err(crate::errors::TraceDecayError::Config {
                message: "daemon returned an invalid semantic evaluation response".to_owned(),
            }),
        }
    }

    pub(crate) async fn invoke_controlled(
        &self,
        request: crate::daemon::DaemonInvocationRequest,
        deadline: Deadline,
        cancellation: CancellationSignal,
        policy: InvocationCancellationPolicy,
    ) -> Result<crate::daemon::DaemonInvocationResponse, DaemonInvocationError> {
        if cancellation.is_cancelled() {
            return Err(DaemonInvocationError::Cancelled {
                stage: CancellationStage::BeforeAdmission,
            });
        }
        let remaining = deadline_remaining(&deadline).ok_or(DaemonInvocationError::TimedOut {
            stage: CancellationStage::BeforeAdmission,
        })?;
        let client = self.clone();
        tokio::spawn(async move {
            let stage = match policy {
                InvocationCancellationPolicy::ReadOnly => CancellationStage::DuringRead,
                InvocationCancellationPolicy::AuthoritativeEffect => {
                    CancellationStage::EffectInFlight
                }
            };
            if !policy.may_interrupt(stage) {
                return client
                    .invoke(request)
                    .await
                    .map_err(|_| DaemonInvocationError::Unavailable);
            }
            let outcome = {
                let invocation = client.invoke(request);
                tokio::pin!(invocation);
                let cancellation_wait = wait_for_cancellation(cancellation);
                tokio::pin!(cancellation_wait);
                tokio::select! {
                    result = &mut invocation => result.map_err(|_| DaemonInvocationError::Unavailable),
                    () = &mut cancellation_wait => Err(DaemonInvocationError::Cancelled { stage }),
                    () = tokio::time::sleep(remaining) => {
                        Err(DaemonInvocationError::TimedOut { stage })
                    }
                }
            };
            if matches!(
                outcome,
                Err(
                    DaemonInvocationError::Cancelled { .. }
                        | DaemonInvocationError::TimedOut { .. }
                )
            ) {
                *client.state.lock().await = None;
            }
            outcome
        })
        .await
        .map_err(|_| DaemonInvocationError::Unavailable)?
    }
}

impl DaemonInvocationExecutor for DaemonInvocationClient {
    fn invoke_controlled(
        &self,
        request: crate::daemon::DaemonInvocationRequest,
        deadline: Deadline,
        cancellation: CancellationSignal,
        policy: InvocationCancellationPolicy,
    ) -> DaemonInvocationExecutorFuture<
        '_,
        Result<crate::daemon::DaemonInvocationResponse, DaemonInvocationError>,
    > {
        Box::pin(DaemonInvocationClient::invoke_controlled(
            self,
            request,
            deadline,
            cancellation,
            policy,
        ))
    }

    fn observe_plan26_feedback(
        &self,
        subject_digest: ManifestDigest,
        observed_at: UtcMicros,
        event: Plan26FeedbackSourceEventV1,
    ) -> DaemonInvocationExecutorFuture<'_, crate::errors::Result<()>> {
        Box::pin(DaemonInvocationClient::observe_plan26_feedback(
            self,
            subject_digest,
            observed_at,
            event,
        ))
    }
}

impl ApplicationInvocationExecutor for DaemonInvocationClient {
    fn invoke(
        &self,
        invocation: ApplicationInvocation,
    ) -> ApplicationInvocationFuture<'_, Result<ApplicationResponse, InvocationError>> {
        Box::pin(async move {
            let (context, request) = invocation.into_parts();
            let (request_id, target, deadline, cancellation) = context.into_parts();
            match request {
                ApplicationRequest::Surface { binding, payload } => {
                    let (_binding_id, surface, operation, result_contract, _page) =
                        binding.into_parts();
                    let operation =
                        crate::application_surface::ApplicationSurfaceOperation::from_tool_name(
                            operation.as_str(),
                        )
                        .ok_or(InvocationError::InvalidRequest)?;
                    let typed = crate::application_surface::parse_application_surface_request(
                        operation, payload,
                    )
                    .map_err(|_| InvocationError::InvalidRequest)?;
                    let observed_at = invocation_now_micros();
                    let cancellation_context = cancellation.context();
                    let scope = match target {
                        InvocationTarget::CurrentProject => None,
                        InvocationTarget::Resolved(scope) => Some(scope),
                    };
                    let policy = if operation
                        == crate::application_surface::ApplicationSurfaceOperation::ConfigurationSet
                    {
                        InvocationCancellationPolicy::AuthoritativeEffect
                    } else {
                        InvocationCancellationPolicy::ReadOnly
                    };
                    let request = match (operation, typed) {
                        (
                            crate::application_surface::ApplicationSurfaceOperation::ConfigurationGet
                            | crate::application_surface::ApplicationSurfaceOperation::ConfigurationSet,
                            crate::application_surface::ApplicationSurfaceRequest::Configuration(
                                request,
                            ),
                        ) => crate::daemon::DaemonInvocationRequest::configuration(
                            request_id.as_str(),
                            operation,
                            request,
                            observed_at,
                            deadline.clone(),
                            cancellation_context,
                        )
                        .with_resolved_scope(scope),
                        (
                            crate::application_surface::ApplicationSurfaceOperation::FeedbackGet,
                            crate::application_surface::ApplicationSurfaceRequest::Feedback(
                                request,
                            ),
                        ) => crate::daemon::DaemonInvocationRequest::feedback(
                            request_id.as_str(),
                            operation,
                            request.request_handle,
                            observed_at,
                            deadline.clone(),
                            cancellation_context,
                        )
                        .with_resolved_scope(scope),
                        _ => return Err(InvocationError::InvalidRequest),
                    }
                    .with_delivery_route(application_delivery_route(surface));
                    let response = self
                        .invoke_controlled(request, deadline, cancellation, policy)
                        .await
                        .map_err(map_invocation_error)?;
                    application_response(request_id, result_contract, response.outcome)
                }
                ApplicationRequest::FeedbackObservation {
                    configuration_digest,
                    observed_at,
                    event,
                } => {
                    let event = serde_json::from_value(event)
                        .map_err(|_| InvocationError::InvalidRequest)?;
                    self.observe_plan26_feedback(configuration_digest, observed_at, event)
                        .await
                        .map_err(|_| InvocationError::Unavailable)?;
                    Ok(ApplicationResponse::ObservationAccepted)
                }
                ApplicationRequest::OperationEvents { .. }
                | ApplicationRequest::OperationCancel { .. } => Err(InvocationError::Unavailable),
            }
        })
    }
}

/// Retained name for its nine call sites across the daemon and application
/// surface; the saturating clamp is the one shared definition.
pub(crate) fn invocation_now_micros() -> UtcMicros {
    tracedecay_application::clock::now_micros()
}

pub(crate) fn application_delivery_route(surface: BindingSurface) -> Plan26DeliveryRouteV1 {
    match surface {
        BindingSurface::Cli => Plan26DeliveryRouteV1::Cli,
        BindingSurface::Mcp => Plan26DeliveryRouteV1::Mcp,
        BindingSurface::Http | BindingSurface::Dashboard => Plan26DeliveryRouteV1::Http,
        BindingSurface::Lsp => Plan26DeliveryRouteV1::Lsp,
    }
}

pub(crate) fn map_invocation_error(error: DaemonInvocationError) -> InvocationError {
    match error {
        DaemonInvocationError::Cancelled { .. } => InvocationError::Cancelled,
        DaemonInvocationError::TimedOut { .. } => InvocationError::DeadlineExceeded,
        DaemonInvocationError::Saturated { .. } | DaemonInvocationError::Backpressured { .. } => {
            InvocationError::Unavailable
        }
        DaemonInvocationError::Unavailable => InvocationError::Unavailable,
    }
}

pub(crate) fn application_response(
    request_id: RequestId,
    result_contract: tracedecay_application::ResultContractRef,
    outcome: crate::daemon::DaemonInvocationOutcome,
) -> Result<ApplicationResponse, InvocationError> {
    let envelope = match outcome {
        crate::daemon::DaemonInvocationOutcome::Feedback { scope, result } => {
            ApplicationEnvelope::evidence(
                result_contract,
                request_id,
                scope,
                result.into_application(),
            )
        }
        crate::daemon::DaemonInvocationOutcome::Configuration { scope, outcome } => {
            ApplicationEnvelope {
                contract: result_contract,
                request_id,
                scope,
                outcome,
            }
        }
        crate::daemon::DaemonInvocationOutcome::ApplicationProblem { problem } => {
            return Err(invocation_error_from_problem(&problem));
        }
        crate::daemon::DaemonInvocationOutcome::Problem { problem } => {
            return Err(match problem {
                crate::daemon::DaemonInvocationProblem::InvalidRequest
                | crate::daemon::DaemonInvocationProblem::UnsupportedRevision => {
                    InvocationError::InvalidRequest
                }
                crate::daemon::DaemonInvocationProblem::NotFoundOrNotAuthorized => {
                    InvocationError::Denied
                }
                crate::daemon::DaemonInvocationProblem::Unavailable => InvocationError::Unavailable,
            });
        }
        _ => return Err(InvocationError::Unavailable),
    };
    Ok(ApplicationResponse::unary(envelope))
}

fn invocation_error_from_problem(problem: &ApplicationProblem) -> InvocationError {
    match problem.kind() {
        ApplicationProblemKind::NotFoundOrNotAuthorized => InvocationError::Denied,
        ApplicationProblemKind::Cancelled => InvocationError::Cancelled,
        ApplicationProblemKind::TimedOut => InvocationError::DeadlineExceeded,
        ApplicationProblemKind::InvalidRequest => InvocationError::InvalidRequest,
        ApplicationProblemKind::Conflict | ApplicationProblemKind::Stale => {
            InvocationError::Conflict
        }
        ApplicationProblemKind::Unavailable
        | ApplicationProblemKind::Unsupported
        | ApplicationProblemKind::Saturated => InvocationError::Unavailable,
    }
}

#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
pub struct SemanticEvaluationPublicationResultV1 {
    pub project_id: String,
    pub profile_digest: String,
    pub report_digest: String,
    pub report: crate::search_eval::DirectEvaluationReportV1,
    pub source_generation: String,
    pub snapshot_digest: String,
}

pub(crate) fn deadline_remaining(deadline: &Deadline) -> Option<Duration> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_micros()).ok())
        .unwrap_or(i64::MAX);
    let remaining = deadline.expires_at.0.checked_sub(now)?;
    (remaining > 0).then(|| Duration::from_micros(remaining as u64))
}

pub(crate) async fn wait_for_cancellation(cancellation: CancellationSignal) {
    while !cancellation.is_cancelled() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Typed client for one daemon-owned LSP session. Every method maps to a
/// closed invocation operation; no method exposes a generic local socket.
pub struct DaemonLspSessionClient {
    invocation: DaemonInvocationClient,
    session: crate::daemon::DaemonLspSessionAccess,
    scope_set_id: Option<tracedecay_domain::ScopeSetId>,
    scope_set_digest: Option<tracedecay_domain::ManifestDigest>,
    next_request: ConnectionLocalRequestSequence,
    detached: bool,
}

impl DaemonLspSessionClient {
    pub async fn open(
        invocation: DaemonInvocationClient,
        client_revision: impl Into<String>,
        requested_root_uri: Option<String>,
        workspace_folders: Vec<String>,
    ) -> crate::errors::Result<Self> {
        let response = invocation
            .invoke(crate::daemon::DaemonInvocationRequest::lsp_open(
                "lsp.1",
                client_revision,
                requested_root_uri,
                workspace_folders,
            ))
            .await?;
        let crate::daemon::DaemonInvocationOutcome::LspOpened {
            session,
            scope_set_id,
            scope_set_digest,
            ..
        } = response.outcome
        else {
            return Err(invocation_outcome_error(response.outcome));
        };
        Ok(Self {
            invocation,
            session,
            scope_set_id,
            scope_set_digest,
            next_request: ConnectionLocalRequestSequence::starting_at(2),
            detached: false,
        })
    }

    pub fn scope_set_id(&self) -> Option<&tracedecay_domain::ScopeSetId> {
        self.scope_set_id.as_ref()
    }

    pub fn scope_set_digest(&self) -> Option<&tracedecay_domain::ManifestDigest> {
        self.scope_set_digest.as_ref()
    }

    pub async fn try_send_client_frame(&mut self, frame: &str) -> crate::errors::Result<FrameSend> {
        let request_id = self.next_request_id()?;
        let response = self
            .invoke(crate::daemon::DaemonInvocationRequest::lsp_frame(
                request_id,
                self.session.clone(),
                frame,
            ))
            .await?;
        match response.outcome {
            crate::daemon::DaemonInvocationOutcome::LspFrameAccepted {
                backpressured,
                closed,
            } => Ok(if closed {
                FrameSend::Closed
            } else if backpressured {
                FrameSend::Backpressured
            } else {
                FrameSend::Sent
            }),
            outcome => Err(invocation_outcome_error(outcome)),
        }
    }

    pub async fn poll_daemon_frame(&mut self) -> crate::errors::Result<FramePoll> {
        let request_id = self.next_request_id()?;
        let response = self
            .invoke(crate::daemon::DaemonInvocationRequest::lsp_poll(
                request_id,
                self.session.clone(),
            ))
            .await?;
        match response.outcome {
            crate::daemon::DaemonInvocationOutcome::LspFrame { frame, closed } => {
                Ok(match (frame, closed) {
                    (Some(frame), _) => FramePoll::Frame(frame.into_bytes()),
                    (None, true) => FramePoll::Closed,
                    (None, false) => FramePoll::Pending,
                })
            }
            outcome => Err(invocation_outcome_error(outcome)),
        }
    }

    pub async fn acknowledge_daemon_frame(&mut self) -> crate::errors::Result<()> {
        let request_id = self.next_request_id()?;
        let response = self
            .invoke(crate::daemon::DaemonInvocationRequest::lsp_acknowledge(
                request_id,
                self.session.clone(),
            ))
            .await?;
        match response.outcome {
            crate::daemon::DaemonInvocationOutcome::LspAcknowledged { .. } => Ok(()),
            outcome => Err(invocation_outcome_error(outcome)),
        }
    }

    pub async fn reconnect(&mut self) -> crate::errors::Result<()> {
        let request_id = self.next_request_id()?;
        let response = self
            .invoke(crate::daemon::DaemonInvocationRequest::lsp_reconnect(
                request_id,
                self.session.clone(),
            ))
            .await?;
        match response.outcome {
            crate::daemon::DaemonInvocationOutcome::LspReconnected { session } => {
                self.session = session;
                Ok(())
            }
            outcome => Err(invocation_outcome_error(outcome)),
        }
    }

    pub async fn detach(&mut self) -> crate::errors::Result<()> {
        let request_id = self.next_request_id()?;
        let response = self
            .invoke(crate::daemon::DaemonInvocationRequest::lsp_detach(
                request_id,
                self.session.clone(),
            ))
            .await?;
        match response.outcome {
            crate::daemon::DaemonInvocationOutcome::LspDetached => {
                self.detached = true;
                Ok(())
            }
            outcome => Err(invocation_outcome_error(outcome)),
        }
    }

    async fn invoke(
        &self,
        request: crate::daemon::DaemonInvocationRequest,
    ) -> crate::errors::Result<crate::daemon::DaemonInvocationResponse> {
        self.invocation.invoke(request).await
    }

    fn next_request_id(&mut self) -> crate::errors::Result<String> {
        self.next_request.next_string("lsp.").map_err(|error| {
            crate::errors::TraceDecayError::Config {
                message: error.to_string(),
            }
        })
    }
}

impl Drop for DaemonLspSessionClient {
    fn drop(&mut self) {
        if self.detached {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let invocation = self.invocation.clone();
        let session = self.session.clone();
        let Ok(request_id) = self.next_request_id() else {
            return;
        };
        runtime.spawn(async move {
            let _ = invocation
                .invoke(crate::daemon::DaemonInvocationRequest::lsp_detach(
                    request_id, session,
                ))
                .await;
        });
    }
}

fn invocation_outcome_error(
    outcome: crate::daemon::DaemonInvocationOutcome,
) -> crate::errors::TraceDecayError {
    let message = match outcome {
        crate::daemon::DaemonInvocationOutcome::Problem { problem } => match problem {
            crate::daemon::DaemonInvocationProblem::InvalidRequest => {
                "daemon rejected the invocation input"
            }
            crate::daemon::DaemonInvocationProblem::UnsupportedRevision => {
                "daemon does not support this invocation revision"
            }
            crate::daemon::DaemonInvocationProblem::NotFoundOrNotAuthorized => {
                "daemon invocation was not found or is not authorized"
            }
            crate::daemon::DaemonInvocationProblem::Unavailable => {
                "daemon invocation authority is unavailable"
            }
        },
        _ => "daemon returned an unexpected invocation response",
    };
    crate::errors::TraceDecayError::Config {
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DaemonInvocationError, InvocationCancellationPolicy, SemanticEvaluationPublicationResultV1,
    };
    use tracedecay_application::{
        ApplicationProblem, ApplicationProblemKind, CancellationStage, RetryDirective,
    };

    #[test]
    fn daemon_invocation_errors_keep_canonical_problem_categories() {
        for (error, expected) in [
            (
                DaemonInvocationError::Cancelled {
                    stage: CancellationStage::BeforeAdmission,
                },
                ApplicationProblemKind::Cancelled,
            ),
            (
                DaemonInvocationError::TimedOut {
                    stage: CancellationStage::BeforeAdmission,
                },
                ApplicationProblemKind::TimedOut,
            ),
            (
                DaemonInvocationError::Unavailable,
                ApplicationProblemKind::Unavailable,
            ),
        ] {
            assert_eq!(error.into_application_problem().kind(), expected);
        }
    }

    #[test]
    fn saturation_mapping_preserves_retry_without_resource_detail() {
        let problem = DaemonInvocationError::Saturated {
            class: super::DaemonAdmissionClass::General,
        }
        .into_application_problem();

        assert!(matches!(
            problem,
            ApplicationProblem::Saturated {
                retry: RetryDirective::AfterDelay,
                legal_actions,
                ..
            } if legal_actions == vec![tracedecay_application::LegalAction::Retry]
        ));
    }

    #[test]
    fn effect_dispatch_waits_for_the_authoritative_commit_receipt() {
        assert!(
            !InvocationCancellationPolicy::AuthoritativeEffect
                .may_interrupt(CancellationStage::EffectInFlight)
        );
        assert!(
            InvocationCancellationPolicy::ReadOnly.may_interrupt(CancellationStage::DuringRead)
        );
    }

    #[test]
    fn semantic_evaluation_result_retains_the_direct_report() {
        let result = SemanticEvaluationPublicationResultV1 {
            project_id: "project-1".to_owned(),
            profile_digest: format!("sha256:{}", "1".repeat(64)),
            report_digest: format!("sha256:{}", "2".repeat(64)),
            report: crate::search_eval::DirectEvaluationReportV1 {
                command: "compare".to_owned(),
                status: crate::search_eval::DirectEvaluationStatusV1::Pass,
                workload_digest: format!("sha256:{}", "3".repeat(64)),
                corpus_digest: format!("sha256:{}", "4".repeat(64)),
                fixture_source_repository_commit: "fixture-commit".to_owned(),
                fixture_source_repository_tree: "fixture-tree".to_owned(),
                profiles: Vec::new(),
            },
            source_generation: "generation-1".to_owned(),
            snapshot_digest: format!("sha256:{}", "5".repeat(64)),
        };

        let encoded = serde_json::to_value(result).expect("serialize evaluation result");
        assert_eq!(encoded["report"]["status"], "pass");
        assert_eq!(encoded["report"]["command"], "compare");
    }
}
