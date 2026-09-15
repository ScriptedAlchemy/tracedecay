//! HTTP operation event (SSE) and cancellation routes over the daemon operation stream.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::extract::{Extension, Path as AxumPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt;
use tracedecay_api::{HttpApplicationControls, application_problem_response, sse_response};
use tracedecay_application::operation_stream::{
    OperationCancelOutcome, OperationEventAuthority, OperationEventError, OperationId,
    OperationRequestControls,
};
use tracedecay_contracts::feedback::observations::{
    FeedbackArgumentRejectionClassV1, FeedbackDeliveryRouteV1, FeedbackOperationV1,
    FeedbackOutcomeV1, FeedbackRejectedArgumentV1, FeedbackSourceEventV1, FeedbackSseLifecycleV1,
};
use tracedecay_contracts::{
    ApplicationProblem, ApplicationProblemEnvelope, ApplicationProblemKind, CancellationContext,
    CancellationStage, Deadline, LegalAction, OperationTermination, ProblemOwningLayer,
    RequestContext, RequestId, ResultContractRef, ResumeToken, RetryDirective, SafeDiagnostic,
    StreamEvent, StreamEventKind,
};
use tracedecay_domain::{ManifestDigest, ProjectId, UtcMicros, canonical_sha256};
use tracedecay_tool_catalog::SchemaId;

use super::problems::{application_contract_error_response, current_micros};
use super::request_control::{HttpCancellationRegistry, application_http_context};

#[derive(Clone)]
pub(super) struct HttpOperationEventState {
    pub(super) authority: OperationEventAuthority,
    pub(super) active_project_id: ProjectId,
    pub(super) cancellations: HttpCancellationRegistry,
    pub(super) executor: Option<Arc<dyn tracedecay_daemon_protocol::DaemonInvocationExecutor>>,
}

pub(super) struct SseDisconnectObserver {
    executor: Arc<dyn tracedecay_daemon_protocol::DaemonInvocationExecutor>,
    subject: ManifestDigest,
    terminal: Arc<AtomicBool>,
}

impl Drop for SseDisconnectObserver {
    fn drop(&mut self) {
        if self.terminal.load(Ordering::Relaxed) {
            return;
        }
        let executor = Arc::clone(&self.executor);
        let subject = self.subject.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = executor
                    .observe_feedback(
                        subject,
                        current_micros().unwrap_or(UtcMicros(1)),
                        FeedbackSourceEventV1::SseLifecycle {
                            lifecycle: FeedbackSseLifecycleV1::Disconnected,
                            sequence: None,
                            item_count: 0,
                            duration_micros: None,
                        },
                    )
                    .await;
            });
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HttpOperationEventQuery {
    #[serde(default)]
    next_sequence: Option<u64>,
    #[serde(default)]
    resume_token: Option<ResumeToken>,
}

pub(super) fn operation_event_next_sequence(
    explicit_next_sequence: Option<u64>,
    headers: &HeaderMap,
) -> Result<u64, OperationEventError> {
    let invalid_cursor = || {
        OperationEventError::InvalidContext(
            "operation-event resume cursor is invalid or conflicting".to_owned(),
        )
    };
    let mut last_event_ids = headers.get_all("last-event-id").iter();
    let last_event_next_sequence = match last_event_ids.next() {
        None => None,
        Some(value) => {
            if last_event_ids.next().is_some() {
                return Err(invalid_cursor());
            }
            let value = value.to_str().map_err(|_| invalid_cursor())?;
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(invalid_cursor());
            }
            let event_id = value.parse::<u64>().map_err(|_| invalid_cursor())?;
            Some(event_id.checked_add(1).ok_or_else(invalid_cursor)?)
        }
    };

    match (explicit_next_sequence, last_event_next_sequence) {
        (Some(explicit), Some(from_header)) if explicit != from_header => Err(invalid_cursor()),
        (Some(explicit), _) => Ok(explicit),
        (None, Some(from_header)) => Ok(from_header),
        (None, None) => Ok(0),
    }
}

#[derive(Serialize)]
pub(super) struct HttpOperationCancelResponse {
    status: &'static str,
}

#[derive(Deserialize)]
pub(super) struct HttpOperationPath {
    operation_id: String,
}

pub(super) fn http_operation_event_router(
    authority: OperationEventAuthority,
    active_project_id: ProjectId,
    cancellations: HttpCancellationRegistry,
    executor: Option<Arc<dyn tracedecay_daemon_protocol::DaemonInvocationExecutor>>,
) -> axum::Router {
    axum::Router::new()
        .route(
            "/operations/{operation_id}/events",
            get(http_operation_events),
        )
        .route(
            "/operations/{operation_id}/cancel",
            post(http_operation_cancel),
        )
        .with_state(HttpOperationEventState {
            authority,
            active_project_id,
            cancellations: Arc::clone(&cancellations),
            executor,
        })
        .layer(axum::middleware::from_fn_with_state(
            cancellations,
            application_http_context,
        ))
}

pub(super) async fn resolve_authenticated_http_request_context(
    state: &HttpOperationEventState,
    operation_id: &OperationId,
    request_id: RequestId,
    deadline: Deadline,
    cancellation: CancellationContext,
    observed_at: UtcMicros,
    resume_token: Option<&ResumeToken>,
) -> Result<RequestContext, OperationEventError> {
    hotpath::future!(
        state.authority.resolve_request_context(
            operation_id,
            &state.active_project_id,
            OperationRequestControls::new(
                request_id,
                deadline,
                cancellation,
                observed_at,
                resume_token,
            ),
        ),
        label = "application_surface.http.events.resolve_context"
    )
    .await
}

pub(super) fn sse_observation_subject(
    request_id: &RequestId,
    operation_id: &str,
) -> Option<ManifestDigest> {
    canonical_sha256(&(
        "tracedecay.feedback.sse-observation.v1",
        request_id.as_str(),
        operation_id,
    ))
    .ok()
}

pub(super) async fn emit_http_feedback_observation(
    state: &HttpOperationEventState,
    subject: Option<&ManifestDigest>,
    observed_at: UtcMicros,
    event: FeedbackSourceEventV1,
) {
    if let (Some(subject), Some(executor)) = (subject, state.executor.as_ref()) {
        let _ = executor
            .observe_feedback(subject.clone(), observed_at, event)
            .await;
    }
}

pub(super) fn feedback_sse_stream_event<T>(
    event: &StreamEvent<T>,
) -> Option<(FeedbackSseLifecycleV1, u32, bool)> {
    match &event.kind {
        StreamEventKind::Item(_) => Some((FeedbackSseLifecycleV1::EventDelivered, 1, false)),
        StreamEventKind::Progress { .. } => None,
        StreamEventKind::Gap(_) => Some((FeedbackSseLifecycleV1::Gap, 0, false)),
        StreamEventKind::Terminal(terminal) => Some((
            match terminal.termination {
                OperationTermination::Completed => FeedbackSseLifecycleV1::Completed,
                OperationTermination::Cancelled => FeedbackSseLifecycleV1::Cancelled,
                OperationTermination::TimedOut => FeedbackSseLifecycleV1::TimedOut,
                OperationTermination::Failed | OperationTermination::EffectUnknown => {
                    FeedbackSseLifecycleV1::Failed
                }
                OperationTermination::Unavailable => FeedbackSseLifecycleV1::Unavailable,
                OperationTermination::Partial => FeedbackSseLifecycleV1::Partial,
            },
            0,
            true,
        )),
    }
}

pub(super) async fn http_operation_events_through_executor(
    executor: &dyn tracedecay_daemon_protocol::DaemonInvocationExecutor,
    operation_id: &OperationId,
    request_id: &RequestId,
    controls: &HttpApplicationControls,
    next_sequence: u64,
) -> Response {
    let context = match tracedecay_contracts::ApplicationInvocationContext::new(
        request_id.clone(),
        tracedecay_contracts::InvocationTarget::CurrentProject,
        controls.deadline.clone(),
        controls.cancellation.clone(),
    ) {
        Ok(context) => context,
        Err(error) => {
            return operation_event_problem(
                request_id,
                OperationEventError::InvalidContext(error.to_string()),
            );
        }
    };
    let request = match tracedecay_contracts::ApplicationRequest::operation_events(
        operation_id.request_id().clone(),
        256,
        next_sequence.checked_sub(1),
    ) {
        Ok(request) => request,
        Err(error) => {
            return operation_event_problem(
                request_id,
                OperationEventError::InvalidContext(error.to_string()),
            );
        }
    };
    let invocation = match tracedecay_contracts::ApplicationInvocation::new(context, request) {
        Ok(invocation) => invocation,
        Err(error) => {
            return operation_event_problem(
                request_id,
                OperationEventError::InvalidContext(error.to_string()),
            );
        }
    };
    let response = hotpath::future!(
        tracedecay_contracts::ApplicationInvocationExecutor::invoke(executor, invocation),
        label = "application_surface.http.events.invoke"
    )
    .await;
    let tracedecay_contracts::ApplicationResponse::Stream(response) = (match response {
        Ok(response) => response,
        Err(error) => return operation_event_invocation_failure(request_id, error),
    }) else {
        return operation_event_problem(request_id, OperationEventError::ResumeUnavailable);
    };
    sse_response(
        request_id.clone(),
        response.stream.frontier,
        tokio_stream::iter(response.stream.events),
    )
    .into_response()
}

pub(super) enum OperationEventInvocationFailure {
    Stream(OperationEventError),
    Application(ApplicationProblem),
}

pub(super) fn operation_event_failure_from_invocation(
    error: tracedecay_contracts::InvocationError,
) -> OperationEventInvocationFailure {
    match error {
        tracedecay_contracts::InvocationError::Denied => {
            OperationEventInvocationFailure::Stream(OperationEventError::NotFoundOrNotAuthorized)
        }
        tracedecay_contracts::InvocationError::Cancelled
        | tracedecay_contracts::InvocationError::DeadlineExceeded => {
            OperationEventInvocationFailure::Stream(OperationEventError::RequestNotAdmitted)
        }
        tracedecay_contracts::InvocationError::Conflict => {
            OperationEventInvocationFailure::Stream(OperationEventError::InvalidFrontier)
        }
        tracedecay_contracts::InvocationError::InvalidRequest
        | tracedecay_contracts::InvocationError::Unavailable
        | tracedecay_contracts::InvocationError::Unreachable { .. } => {
            OperationEventInvocationFailure::Stream(OperationEventError::ResumeUnavailable)
        }
        tracedecay_contracts::InvocationError::Problem(problem) => match problem.kind() {
            tracedecay_contracts::ApplicationProblemKind::NotFoundOrNotAuthorized => {
                OperationEventInvocationFailure::Stream(
                    OperationEventError::NotFoundOrNotAuthorized,
                )
            }
            tracedecay_contracts::ApplicationProblemKind::Cancelled
            | tracedecay_contracts::ApplicationProblemKind::TimedOut => {
                OperationEventInvocationFailure::Stream(OperationEventError::RequestNotAdmitted)
            }
            tracedecay_contracts::ApplicationProblemKind::Conflict
            | tracedecay_contracts::ApplicationProblemKind::Stale => {
                OperationEventInvocationFailure::Stream(OperationEventError::InvalidFrontier)
            }
            tracedecay_contracts::ApplicationProblemKind::InvalidRequest
            | tracedecay_contracts::ApplicationProblemKind::Unsupported
            | tracedecay_contracts::ApplicationProblemKind::Unavailable
            | tracedecay_contracts::ApplicationProblemKind::Saturated => {
                OperationEventInvocationFailure::Stream(OperationEventError::ResumeUnavailable)
            }
            tracedecay_contracts::ApplicationProblemKind::PartialEffect
            | tracedecay_contracts::ApplicationProblemKind::ExecutionFailed
            | tracedecay_contracts::ApplicationProblemKind::ResetRequired => {
                OperationEventInvocationFailure::Application(*problem)
            }
        },
    }
}

pub(super) fn operation_event_invocation_failure(
    request_id: &RequestId,
    error: tracedecay_contracts::InvocationError,
) -> Response {
    match operation_event_failure_from_invocation(error) {
        OperationEventInvocationFailure::Stream(error) => {
            operation_event_problem(request_id, error)
        }
        OperationEventInvocationFailure::Application(problem) => {
            operation_event_application_problem(request_id, problem)
        }
    }
}

#[hotpath::measure(label = "application_surface.http.events")]
pub(super) async fn http_operation_events(
    State(state): State<HttpOperationEventState>,
    AxumPath(HttpOperationPath { operation_id }): AxumPath<HttpOperationPath>,
    Extension(request_id): Extension<RequestId>,
    Extension(controls): Extension<HttpApplicationControls>,
    headers: HeaderMap,
    Query(query): Query<HttpOperationEventQuery>,
) -> Response {
    let observation_subject = sse_observation_subject(&request_id, &operation_id);
    let operation_id = if let Ok(operation_id) = RequestId::new(operation_id) {
        OperationId::from_request(operation_id)
    } else {
        emit_http_feedback_observation(
            &state,
            observation_subject.as_ref(),
            current_micros().unwrap_or(UtcMicros(1)),
            FeedbackSourceEventV1::SurfaceArgumentRejected {
                operation: FeedbackOperationV1::SseStream,
                route: Some(FeedbackDeliveryRouteV1::Http),
                argument: FeedbackRejectedArgumentV1::RequestHandle,
                rejection: FeedbackArgumentRejectionClassV1::InvalidShape,
                schema_revision: 1,
                outcome: FeedbackOutcomeV1::Rejected,
            },
        )
        .await;
        return operation_event_problem(&request_id, OperationEventError::NotFoundOrNotAuthorized);
    };
    let next_sequence = match operation_event_next_sequence(query.next_sequence, &headers) {
        Ok(next_sequence) => next_sequence,
        Err(error) => return operation_event_problem(&request_id, error),
    };
    let observed_at = match current_micros() {
        Ok(observed_at) => observed_at,
        Err(error) => {
            return operation_event_problem(
                &request_id,
                OperationEventError::InvalidContext(error.to_string()),
            );
        }
    };
    // Same owner rule as cancellation: this authority answers for the
    // operations it began, and only an operation it does not own is delegated
    // to the daemon executor. A resume token is always redeemed locally — the
    // token names this authority's own retained frontier.
    let context = match resolve_authenticated_http_request_context(
        &state,
        &operation_id,
        request_id.clone(),
        controls.deadline.clone(),
        controls.cancellation.context(),
        observed_at,
        query.resume_token.as_ref(),
    )
    .await
    {
        Ok(context) => context,
        Err(error) => {
            if matches!(error, OperationEventError::NotFoundOrNotAuthorized)
                && query.resume_token.is_none()
                && let Some(executor) = state.executor.as_deref()
            {
                return http_operation_events_through_executor(
                    executor,
                    &operation_id,
                    &request_id,
                    &controls,
                    next_sequence,
                )
                .await;
            }
            emit_http_feedback_observation(
                &state,
                observation_subject.as_ref(),
                observed_at,
                FeedbackSourceEventV1::SurfaceArgumentRejected {
                    operation: FeedbackOperationV1::SseStream,
                    route: Some(FeedbackDeliveryRouteV1::Http),
                    argument: FeedbackRejectedArgumentV1::RequestHandle,
                    rejection: FeedbackArgumentRejectionClassV1::Unauthorized,
                    schema_revision: 1,
                    outcome: FeedbackOutcomeV1::Denied,
                },
            )
            .await;
            return operation_event_problem(&request_id, error);
        }
    };
    emit_http_feedback_observation(
        &state,
        observation_subject.as_ref(),
        observed_at,
        FeedbackSourceEventV1::Dispatch {
            operation: FeedbackOperationV1::SseStream,
            outcome: FeedbackOutcomeV1::Admitted,
            capacity: 1,
            admitted: 1,
        },
    )
    .await;
    let subscription = match hotpath::future!(
        state.authority.subscribe(
            &operation_id,
            &context,
            observed_at,
            next_sequence,
            query.resume_token.as_ref(),
        ),
        label = "application_surface.http.events.subscribe"
    )
    .await
    {
        Ok(subscription) => subscription,
        Err(error) => {
            if matches!(&error, OperationEventError::Saturated) {
                emit_http_feedback_observation(
                    &state,
                    observation_subject.as_ref(),
                    observed_at,
                    FeedbackSourceEventV1::Dispatch {
                        operation: FeedbackOperationV1::SseStream,
                        outcome: FeedbackOutcomeV1::AtCapacity,
                        capacity: 1,
                        admitted: 0,
                    },
                )
                .await;
            }
            let lifecycle = if matches!(
                &error,
                OperationEventError::FrontierExpired | OperationEventError::ResumeExpired
            ) {
                FeedbackSseLifecycleV1::Expired
            } else {
                FeedbackSseLifecycleV1::Failed
            };
            emit_http_feedback_observation(
                &state,
                observation_subject.as_ref(),
                observed_at,
                FeedbackSourceEventV1::SseLifecycle {
                    lifecycle,
                    sequence: None,
                    item_count: 0,
                    duration_micros: None,
                },
            )
            .await;
            return operation_event_problem(&request_id, error);
        }
    };
    emit_http_feedback_observation(
        &state,
        observation_subject.as_ref(),
        observed_at,
        FeedbackSourceEventV1::SseLifecycle {
            lifecycle: FeedbackSseLifecycleV1::Opened,
            sequence: None,
            item_count: 0,
            duration_micros: None,
        },
    )
    .await;
    let (correlation_id, frontier, stream) = subscription.into_sse_parts();
    let observer = observation_subject
        .zip(state.executor.clone())
        .map(|(subject, executor)| {
            Arc::new(SseDisconnectObserver {
                executor,
                subject,
                terminal: Arc::new(AtomicBool::new(false)),
            })
        });
    let observed_stream = stream.then(move |event| {
        let observer = observer.clone();
        async move {
            if let (Some(observer), Some((lifecycle, item_count, is_terminal))) =
                (observer, feedback_sse_stream_event(&event))
            {
                if is_terminal {
                    observer.terminal.store(true, Ordering::Relaxed);
                }
                let _ = observer
                    .executor
                    .observe_feedback(
                        observer.subject.clone(),
                        current_micros().unwrap_or(UtcMicros(1)),
                        FeedbackSourceEventV1::SseLifecycle {
                            lifecycle,
                            sequence: Some(event.sequence),
                            item_count,
                            duration_micros: None,
                        },
                    )
                    .await;
            }
            event
        }
    });
    sse_response(correlation_id, frontier, observed_stream).into_response()
}

pub(super) async fn http_operation_cancel_through_executor(
    state: &HttpOperationEventState,
    executor: &dyn tracedecay_daemon_protocol::DaemonInvocationExecutor,
    operation_id: &OperationId,
    request_id: &RequestId,
    controls: &HttpApplicationControls,
    observed_at: UtcMicros,
) -> Response {
    let context = match tracedecay_contracts::ApplicationInvocationContext::new(
        request_id.clone(),
        tracedecay_contracts::InvocationTarget::CurrentProject,
        controls.deadline.clone(),
        controls.cancellation.clone(),
    ) {
        Ok(context) => context,
        Err(error) => {
            return operation_event_problem(
                request_id,
                OperationEventError::InvalidContext(error.to_string()),
            );
        }
    };
    let request = match tracedecay_contracts::ApplicationRequest::operation_cancel(
        operation_id.request_id().clone(),
    ) {
        Ok(request) => request,
        Err(error) => {
            return operation_event_problem(
                request_id,
                OperationEventError::InvalidContext(error.to_string()),
            );
        }
    };
    let invocation = match tracedecay_contracts::ApplicationInvocation::new(context, request) {
        Ok(invocation) => invocation,
        Err(error) => {
            return operation_event_problem(
                request_id,
                OperationEventError::InvalidContext(error.to_string()),
            );
        }
    };
    let response = hotpath::future!(
        tracedecay_contracts::ApplicationInvocationExecutor::invoke(executor, invocation),
        label = "application_surface.http.cancel.invoke"
    )
    .await;
    let tracedecay_contracts::ApplicationResponse::Cancellation(response) = (match response {
        Ok(response) => response,
        Err(error) => return operation_event_invocation_failure(request_id, error),
    }) else {
        return operation_event_problem(request_id, OperationEventError::ResumeUnavailable);
    };
    if response.cancelled {
        if let Some(cancellation) = state
            .cancellations
            .lock()
            .ok()
            .and_then(|active| active.get(operation_id.request_id()).cloned())
        {
            let _ = cancellation.cancel(observed_at);
        }
        (
            StatusCode::ACCEPTED,
            Json(HttpOperationCancelResponse {
                status: "requested",
            }),
        )
            .into_response()
    } else {
        (
            StatusCode::OK,
            Json(HttpOperationCancelResponse {
                status: "already_terminal",
            }),
        )
            .into_response()
    }
}

#[hotpath::measure(label = "application_surface.http.cancel")]
pub(super) async fn http_operation_cancel(
    State(state): State<HttpOperationEventState>,
    AxumPath(HttpOperationPath { operation_id }): AxumPath<HttpOperationPath>,
    Extension(request_id): Extension<RequestId>,
    Extension(controls): Extension<HttpApplicationControls>,
) -> Response {
    let observation_subject = sse_observation_subject(&request_id, &operation_id);
    let operation_id = if let Ok(operation_id) = RequestId::new(operation_id) {
        OperationId::from_request(operation_id)
    } else {
        emit_http_feedback_observation(
            &state,
            observation_subject.as_ref(),
            current_micros().unwrap_or(UtcMicros(1)),
            FeedbackSourceEventV1::SurfaceArgumentRejected {
                operation: FeedbackOperationV1::SseStream,
                route: Some(FeedbackDeliveryRouteV1::Http),
                argument: FeedbackRejectedArgumentV1::RequestHandle,
                rejection: FeedbackArgumentRejectionClassV1::InvalidShape,
                schema_revision: 1,
                outcome: FeedbackOutcomeV1::Rejected,
            },
        )
        .await;
        return operation_event_problem(&request_id, OperationEventError::NotFoundOrNotAuthorized);
    };
    let observed_at = match current_micros() {
        Ok(observed_at) => observed_at,
        Err(error) => {
            return operation_event_problem(
                &request_id,
                OperationEventError::InvalidContext(error.to_string()),
            );
        }
    };
    // The canonical owner of an operation is whichever authority began it. The
    // daemon mounts these routes with its *own* process-global authority and an
    // invocation client pointed back at its own socket, so delegating first
    // sent every cancel on a round trip out of the process and back to reach
    // in-memory state this handler already holds — and reported the typed
    // `operation_event.unavailable` whenever that socket was momentarily
    // unreachable. Resolve locally first; delegate only for an operation this
    // authority does not own.
    let context = match resolve_authenticated_http_request_context(
        &state,
        &operation_id,
        request_id.clone(),
        controls.deadline.clone(),
        controls.cancellation.context(),
        observed_at,
        None,
    )
    .await
    {
        Ok(context) => context,
        Err(error) => {
            if matches!(error, OperationEventError::NotFoundOrNotAuthorized)
                && let Some(executor) = state.executor.as_deref()
            {
                return http_operation_cancel_through_executor(
                    &state,
                    executor,
                    &operation_id,
                    &request_id,
                    &controls,
                    observed_at,
                )
                .await;
            }
            emit_http_feedback_observation(
                &state,
                observation_subject.as_ref(),
                observed_at,
                FeedbackSourceEventV1::Cancellation {
                    operation: FeedbackOperationV1::SseStream,
                    outcome: FeedbackOutcomeV1::Denied,
                },
            )
            .await;
            return operation_event_problem(&request_id, error);
        }
    };
    let target_cancellation = state
        .cancellations
        .lock()
        .ok()
        .and_then(|active| active.get(operation_id.request_id()).cloned());
    match hotpath::future!(
        state.authority.cancel(&operation_id, &context, observed_at),
        label = "application_surface.http.cancel.authority"
    )
    .await
    {
        Ok(OperationCancelOutcome::Requested) => {
            if let Some(cancellation) = target_cancellation {
                let _ = cancellation.cancel(observed_at);
            }
            emit_http_feedback_observation(
                &state,
                observation_subject.as_ref(),
                observed_at,
                FeedbackSourceEventV1::Cancellation {
                    operation: FeedbackOperationV1::SseStream,
                    outcome: FeedbackOutcomeV1::Accepted,
                },
            )
            .await;
            (
                StatusCode::ACCEPTED,
                Json(HttpOperationCancelResponse {
                    status: "requested",
                }),
            )
                .into_response()
        }
        Ok(OperationCancelOutcome::AlreadyRequested) => {
            if let Some(cancellation) = target_cancellation {
                let _ = cancellation.cancel(observed_at);
            }
            emit_http_feedback_observation(
                &state,
                observation_subject.as_ref(),
                observed_at,
                FeedbackSourceEventV1::Cancellation {
                    operation: FeedbackOperationV1::SseStream,
                    outcome: FeedbackOutcomeV1::Duplicate,
                },
            )
            .await;
            (
                StatusCode::OK,
                Json(HttpOperationCancelResponse {
                    status: "already_requested",
                }),
            )
                .into_response()
        }
        Ok(OperationCancelOutcome::AlreadyTerminal) => {
            emit_http_feedback_observation(
                &state,
                observation_subject.as_ref(),
                observed_at,
                FeedbackSourceEventV1::Cancellation {
                    operation: FeedbackOperationV1::SseStream,
                    outcome: FeedbackOutcomeV1::Completed,
                },
            )
            .await;
            (
                StatusCode::OK,
                Json(HttpOperationCancelResponse {
                    status: "already_terminal",
                }),
            )
                .into_response()
        }
        Err(error) => {
            emit_http_feedback_observation(
                &state,
                observation_subject.as_ref(),
                observed_at,
                FeedbackSourceEventV1::Cancellation {
                    operation: FeedbackOperationV1::SseStream,
                    outcome: if matches!(&error, OperationEventError::Saturated) {
                        FeedbackOutcomeV1::AtCapacity
                    } else {
                        FeedbackOutcomeV1::Failed
                    },
                },
            )
            .await;
            operation_event_problem(&request_id, error)
        }
    }
}

pub(super) fn operation_event_problem(
    request_id: &RequestId,
    error: OperationEventError,
) -> Response {
    let problem = match error {
        OperationEventError::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        OperationEventError::FrontierExpired | OperationEventError::ResumeExpired => {
            ApplicationProblem::Stale {
                diagnostic: SafeDiagnostic {
                    code: "operation_event.resume_expired".to_owned(),
                    message: "The operation-event resume frontier has expired".to_owned(),
                },
                retry: RetryDirective::AfterRevalidate,
                legal_actions: vec![LegalAction::Refresh],
            }
        }
        OperationEventError::InvalidFrontier => ApplicationProblem::Conflict {
            diagnostic: SafeDiagnostic {
                code: "operation_event.invalid_frontier".to_owned(),
                message: "The requested operation-event frontier is invalid".to_owned(),
            },
            retry: RetryDirective::AfterRevalidate,
            legal_actions: vec![LegalAction::Refresh],
        },
        OperationEventError::RequestNotAdmitted => ApplicationProblem::TimedOut {
            stage: CancellationStage::BeforeAdmission,
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        },
        OperationEventError::Saturated => ApplicationProblem::Saturated {
            diagnostic: SafeDiagnostic {
                code: "operation_event.saturated".to_owned(),
                message: "Operation-event capacity is temporarily saturated".to_owned(),
            },
            retry: RetryDirective::AfterDelay,
            legal_actions: vec![LegalAction::Retry],
        },
        // Permanently invalid input: the same request can never succeed, so the
        // client must correct it rather than retry.
        OperationEventError::InvalidContext(_)
        | OperationEventError::InvalidProgress
        | OperationEventError::InvalidTerminal(_)
        | OperationEventError::InvalidTestRunEvent => ApplicationProblem::InvalidRequest {
            diagnostic: SafeDiagnostic {
                code: "operation_event.invalid_request".to_owned(),
                message: "The operation-event request is invalid".to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::CorrectRequest],
        },
        // Idempotency facts: the identity or terminal receipt is already
        // published, so the client re-reads current state instead of retrying
        // the same publish.
        OperationEventError::AlreadyBound | OperationEventError::TerminalAlreadyPublished => {
            ApplicationProblem::Conflict {
                diagnostic: SafeDiagnostic {
                    code: "operation_event.already_published".to_owned(),
                    message: "The operation-event identity is already published".to_owned(),
                },
                retry: RetryDirective::AfterRevalidate,
                legal_actions: vec![LegalAction::Refresh],
            }
        }
        // A misconfigured authority is a deterministic, process-lifetime
        // failure. It is not the caller's request that is wrong and no amount
        // of retrying will change the outcome.
        OperationEventError::InvalidConfiguration => ApplicationProblem::Unsupported {
            diagnostic: SafeDiagnostic {
                code: "operation_event.unsupported".to_owned(),
                message: "The operation-event authority is not configured for this operation"
                    .to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::ContactAdministrator],
        },
        // Genuinely transient: the resume-token authority could not answer.
        OperationEventError::ResumeUnavailable => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "operation_event.unavailable".to_owned(),
            message: "The operation-event service is unavailable".to_owned(),
        }),
    };
    let Ok(schema_id) = SchemaId::new("schema.tracedecay.operation-event.problem.v1") else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(contract) = ResultContractRef::new(schema_id, 1) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let envelope = match ApplicationProblemEnvelope::new(contract, request_id.clone(), problem) {
        Ok(envelope) => envelope,
        Err(error) => return application_contract_error_response(error),
    };
    let envelope = envelope.with_owning_layer(ProblemOwningLayer::Runtime);
    let envelope = if envelope.problem.kind() == ApplicationProblemKind::Saturated {
        let Ok(envelope) = envelope.with_retry_after_millis(Some(250)) else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        envelope
    } else {
        envelope
    };
    application_problem_response(envelope)
}

pub(super) fn operation_event_application_problem(
    request_id: &RequestId,
    problem: ApplicationProblem,
) -> Response {
    let Ok(schema_id) = SchemaId::new("schema.tracedecay.operation-event.problem.v1") else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(contract) = ResultContractRef::new(schema_id, 1) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match ApplicationProblemEnvelope::new(contract, request_id.clone(), problem) {
        Ok(envelope) => {
            application_problem_response(envelope.with_owning_layer(ProblemOwningLayer::Runtime))
        }
        Err(error) => application_contract_error_response(error),
    }
}
