//! Typed HTTP routes for subscribing to and explicitly cancelling operations.
//!
//! The owning application/runtime port supplies authorization, retention,
//! replay, gaps, and cancellation semantics. This adapter only validates
//! bounded transport inputs and frames canonical application results.

use std::future::Future;
use std::pin::Pin;

use axum::extract::rejection::QueryRejection;
use axum::extract::{Extension, Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracedecay_application::{
    ApplicationOutcome, ApplicationProblem, ApplicationProblemEnvelope, RequestContext, RequestId,
    ResumeToken, RetryDirective, StreamEvent, StreamFrontier,
};

use crate::http::{adapter_problem, application_problem_response, invalid_request_response};
use crate::{CanonicalInvocationResult, HttpApplicationControls, sse_response};

/// Default maximum number of retained events requested when opening a stream.
pub const DEFAULT_OPERATION_EVENT_PAGE_SIZE: u16 = 256;
/// Largest retained-event page an HTTP client may request.
pub const MAX_OPERATION_EVENT_PAGE_SIZE: u16 = 256;
const MAX_OPERATION_EVENT_SEQUENCE: u64 = i64::MAX as u64;

/// Exact authenticated input forwarded when an operation stream is opened.
#[derive(Clone, Debug)]
pub struct OperationEventRequest {
    pub context: RequestContext,
    pub controls: HttpApplicationControls,
    pub operation_id: RequestId,
    pub next_sequence: u64,
    pub resume_token: Option<ResumeToken>,
    pub max_events: u16,
}

/// Exact authenticated input forwarded for explicit operation cancellation.
#[derive(Clone, Debug)]
pub struct OperationCancelRequest {
    pub context: RequestContext,
    pub controls: HttpApplicationControls,
    pub operation_id: RequestId,
}

/// Idempotent result of an explicit cancellation request.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationCancellationStatus {
    Requested,
    AlreadyRequested,
    AlreadyTerminal,
}

impl OperationCancellationStatus {
    const fn http_status(self) -> axum::http::StatusCode {
        match self {
            Self::Requested => axum::http::StatusCode::ACCEPTED,
            Self::AlreadyRequested | Self::AlreadyTerminal => axum::http::StatusCode::OK,
        }
    }
}

/// Dynamically dispatched canonical event stream owned outside this adapter.
pub type OperationEventStream = Pin<Box<dyn Stream<Item = StreamEvent<Value>> + Send + 'static>>;

/// Initial replay frontier and the live canonical event stream.
pub struct OperationEventSubscription {
    pub frontier: StreamFrontier,
    pub events: OperationEventStream,
}

impl OperationEventSubscription {
    /// Erase the concrete owner stream without buffering or changing events.
    pub fn new<S>(frontier: StreamFrontier, events: S) -> Self
    where
        S: Stream<Item = StreamEvent<Value>> + Send + 'static,
    {
        Self {
            frontier,
            events: Box::pin(events),
        }
    }
}

/// Owner future for an authenticated operation-event subscription.
pub type OperationEventFuture = Pin<
    Box<
        dyn Future<Output = Result<OperationEventSubscription, ApplicationProblemEnvelope>>
            + Send
            + 'static,
    >,
>;
/// Owner future for an explicit typed cancellation result.
pub type OperationCancelFuture = Pin<
    Box<
        dyn Future<Output = CanonicalInvocationResult<OperationCancellationStatus>>
            + Send
            + 'static,
    >,
>;

/// Application/runtime authority adapted by the operation HTTP routes.
pub trait OperationEventOwner: Clone + Send + Sync + 'static {
    fn operation_events(&self, request: OperationEventRequest) -> OperationEventFuture;

    fn cancel_operation(&self, request: OperationCancelRequest) -> OperationCancelFuture;
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationEventQuery {
    #[serde(default)]
    next_sequence: u64,
    #[serde(default)]
    resume_token: Option<ResumeToken>,
    #[serde(default = "default_operation_event_page_size")]
    max_events: u16,
}

const fn default_operation_event_page_size() -> u16 {
    DEFAULT_OPERATION_EVENT_PAGE_SIZE
}

/// Build the operation-event and explicit-cancellation router.
pub fn operation_event_router<O>(owner: O) -> Router
where
    O: OperationEventOwner,
{
    Router::new()
        .route(
            "/operations/{operation_id}/events",
            get(operation_events::<O>),
        )
        .route(
            "/operations/{operation_id}/cancel",
            post(cancel_operation::<O>),
        )
        .with_state(owner)
}

async fn operation_events<O>(
    State(owner): State<O>,
    Path(operation_id): Path<String>,
    Extension(context): Extension<RequestContext>,
    Extension(controls): Extension<HttpApplicationControls>,
    query: Result<Query<OperationEventQuery>, QueryRejection>,
) -> Response
where
    O: OperationEventOwner,
{
    let request_id = context.request_id().clone();
    let operation_id = match RequestId::new(operation_id) {
        Ok(operation_id) => operation_id,
        Err(_) => return concealed_operation_response(request_id),
    };
    let Query(query) = match query {
        Ok(query) => query,
        Err(_) => {
            return invalid_request_response(
                request_id,
                "operation_event.invalid_query",
                "The operation-event query is invalid",
            );
        }
    };
    if query.max_events == 0
        || query.max_events > MAX_OPERATION_EVENT_PAGE_SIZE
        || query.next_sequence > MAX_OPERATION_EVENT_SEQUENCE
    {
        return invalid_request_response(
            request_id,
            "operation_event.invalid_page",
            "The operation-event page is outside the supported bounds",
        );
    }

    match owner
        .operation_events(OperationEventRequest {
            context,
            controls,
            operation_id,
            next_sequence: query.next_sequence,
            resume_token: query.resume_token,
            max_events: query.max_events,
        })
        .await
    {
        Ok(subscription) => {
            sse_response(request_id, subscription.frontier, subscription.events).into_response()
        }
        Err(problem) => application_problem_response(problem),
    }
}

async fn cancel_operation<O>(
    State(owner): State<O>,
    Path(operation_id): Path<String>,
    Extension(context): Extension<RequestContext>,
    Extension(controls): Extension<HttpApplicationControls>,
) -> Response
where
    O: OperationEventOwner,
{
    let request_id = context.request_id().clone();
    let operation_id = match RequestId::new(operation_id) {
        Ok(operation_id) => operation_id,
        Err(_) => return concealed_operation_response(request_id),
    };
    let result = owner
        .cancel_operation(OperationCancelRequest {
            context,
            controls,
            operation_id,
        })
        .await;
    let status = match &result.result {
        Ok(application) => match &application.outcome {
            ApplicationOutcome::Evidence(packet) => packet.payload.as_ref(),
            ApplicationOutcome::Preview(preview) => preview.payload.as_ref(),
            ApplicationOutcome::Effect(effect) => effect.payload.as_ref(),
        }
        .copied()
        .map(OperationCancellationStatus::http_status),
        Err(_) => None,
    };
    match status {
        Some(status) => (status, Json(result.into_http_json())).into_response(),
        None => result.into_http_response(),
    }
}

fn concealed_operation_response(request_id: RequestId) -> Response {
    application_problem_response(adapter_problem(
        request_id,
        ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fmt;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use futures_util::Stream;
    use serde_json::json;
    use tower::ServiceExt;
    use tracedecay_application::{
        ApplicationEnvelope, ApplicationProblem, ApplicationProblemEnvelope, AuthorityReceipt,
        CancellationContext, CancellationSignal, CapabilityGrantSnapshot, Deadline,
        DisclosureClass, EvidenceCoverage, EvidenceDomain, EvidencePacket, OperationReceipt,
        PageState, PolicyDecisionRef, RequestContext, RequestId, ResolvedScope, ResultContractRef,
        ResumeToken, RetrievalEvidence, RetryDirective, StreamEvent, StreamEventKind,
        StreamFrontier, StreamGap, StreamTermination, TemporalState,
    };
    use tracedecay_domain::{
        ActorId, ComponentVersion, ManifestDigest, ProjectId, RefId, RepositoryId, UtcMicros,
        WorktreeId,
    };
    use tracedecay_tool_catalog::{BindingId, CapabilityId, SchemaId, SortContractId, UseCaseId};

    use super::{
        OperationCancelFuture, OperationCancelRequest, OperationCancellationStatus,
        OperationEventFuture, OperationEventOwner, OperationEventRequest,
        OperationEventSubscription, operation_event_router,
    };
    use crate::{CanonicalInvocationResult, HttpApplicationControls};

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("fixture identity")
    }

    fn context() -> RequestContext {
        let scope = ResolvedScope::new(
            id::<ProjectId>("project.operation-http"),
            id::<RepositoryId>("repository.operation-http"),
            id::<WorktreeId>("worktree.operation-http"),
            Some(id::<RefId>("refs/heads/main")),
        )
        .expect("scope");
        let grant = CapabilityGrantSnapshot::new(
            id("grant.operation-http"),
            1,
            ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("digest"),
            id::<ActorId>("actor.issuer"),
            UtcMicros(1),
            UtcMicros(10_000),
            scope.clone(),
            BTreeSet::from([CapabilityId::new("capability.operation.events").expect("capability")]),
            BTreeSet::from([UseCaseId::new("use-case.operation.events").expect("use case")]),
            DisclosureClass::Evidence,
        )
        .expect("grant");
        RequestContext::new(
            id::<ActorId>("actor.requester"),
            scope,
            grant,
            RequestId::new("request.operation-http").expect("request id"),
            Deadline::new(UtcMicros(9_000)).expect("deadline"),
            CancellationContext::active("cancel.operation-http").expect("cancellation"),
        )
        .expect("context")
    }

    fn controls() -> HttpApplicationControls {
        HttpApplicationControls {
            deadline: Deadline::new(UtcMicros(8_000)).expect("deadline"),
            cancellation: CancellationSignal::active("cancel.operation-http.transport")
                .expect("cancellation"),
        }
    }

    fn frontier(next_sequence: u64) -> StreamFrontier {
        StreamFrontier {
            next_sequence,
            retained_from_sequence: 0,
            resume_token: None,
        }
    }

    fn problem(request_id: &RequestId, problem: ApplicationProblem) -> ApplicationProblemEnvelope {
        ApplicationProblemEnvelope::new(
            ResultContractRef::new(
                SchemaId::new("schema.operation-http.problem").expect("schema"),
                1,
            )
            .expect("contract"),
            request_id.clone(),
            problem,
        )
    }

    fn cancellation_result(
        request: &RequestContext,
        status: OperationCancellationStatus,
    ) -> CanonicalInvocationResult<OperationCancellationStatus> {
        let digest = ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).expect("digest");
        let authority = AuthorityReceipt {
            grant_id: request.grant().grant_id.clone(),
            grant_revision: request.grant().revision,
            grant_digest: request.grant().digest.clone(),
            authorized_scope_digest: request.scope().scope_digest.clone(),
            disclosure: DisclosureClass::Evidence,
            policy: PolicyDecisionRef::new(
                "policy.operation-http",
                1,
                digest,
                ComponentVersion::new("policy.operation-http.v1").expect("component"),
            )
            .expect("policy"),
            revalidated_at: UtcMicros(2),
        };
        let retrieval = RetrievalEvidence {
            payload: Some(status),
            temporal: TemporalState::current(UtcMicros(2)),
            evidence_authorities: Vec::new(),
            coverage: EvidenceCoverage::complete(vec![EvidenceDomain::Symbol], 1, 1, 1)
                .expect("coverage"),
            omissions: Vec::new(),
            scores: Vec::new(),
            contributions: Vec::new(),
            page: PageState::first_page(
                SortContractId::new("sort.operation-http").expect("sort"),
                1,
                Some(1),
                1,
            )
            .expect("page"),
            finished_at: UtcMicros(3),
            budget: Default::default(),
            cancellation: None,
        };
        let execution = OperationReceipt::completed(
            UtcMicros(1),
            UtcMicros(3),
            request.deadline().clone(),
            Default::default(),
        )
        .expect("receipt");
        let application = ApplicationEnvelope::evidence(
            ResultContractRef::new(
                SchemaId::new("schema.operation-http.cancel").expect("schema"),
                1,
            )
            .expect("contract"),
            request.request_id().clone(),
            request.scope().clone(),
            EvidencePacket::from_retrieval(retrieval, authority, execution).expect("packet"),
        );
        CanonicalInvocationResult::new(
            BindingId::new("binding.operation-http.cancel").expect("binding"),
            Ok(application),
        )
    }

    #[derive(Clone)]
    struct RecordingOwner {
        event_requests: Arc<Mutex<Vec<OperationEventRequest>>>,
        event_reply:
            Arc<Mutex<Option<Result<OperationEventSubscription, ApplicationProblemEnvelope>>>>,
        cancel_requests: Arc<Mutex<Vec<OperationCancelRequest>>>,
        cancel_reply: Arc<Mutex<Option<CanonicalInvocationResult<OperationCancellationStatus>>>>,
    }

    impl Default for RecordingOwner {
        fn default() -> Self {
            Self {
                event_requests: Arc::default(),
                event_reply: Arc::default(),
                cancel_requests: Arc::default(),
                cancel_reply: Arc::default(),
            }
        }
    }

    impl RecordingOwner {
        fn with_events(
            events: impl Stream<Item = StreamEvent<serde_json::Value>> + Send + 'static,
            frontier: StreamFrontier,
        ) -> Self {
            Self {
                event_reply: Arc::new(Mutex::new(Some(Ok(OperationEventSubscription::new(
                    frontier, events,
                ))))),
                ..Self::default()
            }
        }

        fn with_event_problem(problem: ApplicationProblemEnvelope) -> Self {
            Self {
                event_reply: Arc::new(Mutex::new(Some(Err(problem)))),
                ..Self::default()
            }
        }

        fn with_cancel_result(
            result: CanonicalInvocationResult<OperationCancellationStatus>,
        ) -> Self {
            Self {
                cancel_reply: Arc::new(Mutex::new(Some(result))),
                ..Self::default()
            }
        }
    }

    impl OperationEventOwner for RecordingOwner {
        fn operation_events(&self, request: OperationEventRequest) -> OperationEventFuture {
            self.event_requests
                .lock()
                .expect("event requests")
                .push(request);
            let reply = self
                .event_reply
                .lock()
                .expect("event reply")
                .take()
                .unwrap_or_else(|| {
                    Ok(OperationEventSubscription::new(
                        frontier(42),
                        futures_util::stream::empty(),
                    ))
                });
            Box::pin(async move { reply })
        }

        fn cancel_operation(&self, request: OperationCancelRequest) -> OperationCancelFuture {
            self.cancel_requests
                .lock()
                .expect("cancel requests")
                .push(request);
            let reply = self
                .cancel_reply
                .lock()
                .expect("cancel reply")
                .take()
                .expect("scripted cancellation reply");
            Box::pin(async move { reply })
        }
    }

    #[tokio::test]
    async fn resume_parameters_are_forwarded_exactly_to_the_owner() {
        let owner = RecordingOwner::default();
        let observed = Arc::clone(&owner.event_requests);
        let app = operation_event_router(owner);
        let resume_token = ResumeToken::new("resume.operation-http").expect("resume token");

        let response = app
            .oneshot(
                Request::get(format!(
                    "/operations/request.origin/events?next_sequence=41&resume_token={}&max_events=37",
                    resume_token.as_str()
                ))
                .extension(context())
                .extension(controls())
                .body(Body::empty())
                .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), 200);
        let recorded = observed.lock().expect("event requests");
        let request = recorded.first().expect("one event request");
        assert_eq!(request.operation_id.as_str(), "request.origin");
        assert_eq!(request.next_sequence, 41);
        assert_eq!(request.resume_token.as_ref(), Some(&resume_token));
        assert_eq!(request.max_events, 37);
        assert_eq!(
            request.context.request_id().as_str(),
            "request.operation-http"
        );
        assert_eq!(request.controls.deadline.expires_at, UtcMicros(8_000));
        assert_eq!(
            serde_json::to_value(&request.context).expect("context"),
            serde_json::to_value(context()).expect("expected context")
        );
        assert_eq!(
            serde_json::to_value(json!({
                "next_sequence": request.next_sequence,
                "max_events": request.max_events,
            }))
            .expect("query"),
            json!({"next_sequence": 41, "max_events": 37})
        );
    }

    #[tokio::test]
    async fn resume_gap_is_framed_as_a_canonical_sse_event() {
        let gap = StreamEvent {
            sequence: 7,
            kind: StreamEventKind::Gap(StreamGap {
                first_missing_sequence: 7,
                last_missing_sequence: 9,
                frontier: StreamFrontier {
                    next_sequence: 10,
                    retained_from_sequence: 10,
                    resume_token: None,
                },
            }),
        };
        let app = operation_event_router(RecordingOwner::with_events(
            futures_util::stream::iter([gap]),
            frontier(10),
        ));

        let response = app
            .oneshot(
                Request::get("/operations/request.origin/events")
                    .extension(context())
                    .extension(controls())
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let body = String::from_utf8(
            to_bytes(response.into_body(), 32 * 1024)
                .await
                .expect("SSE body")
                .to_vec(),
        )
        .expect("UTF-8 SSE");

        assert!(body.contains("event: resume_gap"));
        assert!(body.contains("id: 7"));
        assert!(body.contains("\"first_missing_sequence\":7"));
        assert!(body.contains("\"last_missing_sequence\":9"));
    }

    #[tokio::test]
    async fn terminal_event_closes_the_route_before_stale_callbacks() {
        let request = context();
        let terminal = StreamEvent::terminal(
            1,
            StreamTermination::completed(
                OperationReceipt::completed(
                    UtcMicros(1),
                    UtcMicros(2),
                    request.deadline().clone(),
                    Default::default(),
                )
                .expect("receipt"),
            ),
        )
        .expect("terminal event");
        let stale = StreamEvent::item(2, json!({"stale": true})).expect("stale event");
        let app = operation_event_router(RecordingOwner::with_events(
            futures_util::stream::iter([terminal, stale]),
            frontier(1),
        ));

        let response = app
            .oneshot(
                Request::get("/operations/request.origin/events")
                    .extension(request)
                    .extension(controls())
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let body = String::from_utf8(
            to_bytes(response.into_body(), 32 * 1024)
                .await
                .expect("SSE body")
                .to_vec(),
        )
        .expect("UTF-8 SSE");

        assert_eq!(body.matches("event: completed").count(), 1);
        assert!(!body.contains("\"stale\":true"));
    }

    #[tokio::test]
    async fn explicit_cancellation_returns_a_typed_canonical_envelope() {
        for (outcome, expected_status, expected_wire) in [
            (
                OperationCancellationStatus::Requested,
                StatusCode::ACCEPTED,
                "requested",
            ),
            (
                OperationCancellationStatus::AlreadyRequested,
                StatusCode::OK,
                "already_requested",
            ),
            (
                OperationCancellationStatus::AlreadyTerminal,
                StatusCode::OK,
                "already_terminal",
            ),
        ] {
            let request_context = context();
            let owner =
                RecordingOwner::with_cancel_result(cancellation_result(&request_context, outcome));
            let cancel_requests = Arc::clone(&owner.cancel_requests);
            let app = operation_event_router(owner);

            let response = app
                .oneshot(
                    Request::post("/operations/request.origin/cancel")
                        .extension(request_context)
                        .extension(controls())
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            let status = response.status();
            let body: serde_json::Value = serde_json::from_slice(
                &to_bytes(response.into_body(), 32 * 1024)
                    .await
                    .expect("JSON body"),
            )
            .expect("canonical JSON");

            assert_eq!(status, expected_status);
            assert_eq!(body["kind"], "success");
            assert_eq!(body["value"]["outcome"]["value"]["payload"], expected_wire);
            assert_eq!(
                cancel_requests
                    .lock()
                    .expect("cancel requests")
                    .first()
                    .expect("cancel request")
                    .operation_id
                    .as_str(),
                "request.origin"
            );
        }
    }

    #[tokio::test]
    async fn malformed_and_unauthorized_operation_ids_are_concealed_identically() {
        let request_context = context();
        let denied = problem(
            request_context.request_id(),
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
        );
        let unauthorized = operation_event_router(RecordingOwner::with_event_problem(denied))
            .oneshot(
                Request::get("/operations/request.unknown/events")
                    .extension(request_context.clone())
                    .extension(controls())
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("unauthorized response");
        let malformed = operation_event_router(RecordingOwner::default())
            .oneshot(
                Request::get("/operations/%20bad/events")
                    .extension(request_context)
                    .extension(controls())
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("malformed response");

        assert_eq!(unauthorized.status(), StatusCode::NOT_FOUND);
        assert_eq!(malformed.status(), StatusCode::NOT_FOUND);
        let unauthorized_body: serde_json::Value = serde_json::from_slice(
            &to_bytes(unauthorized.into_body(), 32 * 1024)
                .await
                .expect("unauthorized body"),
        )
        .expect("unauthorized JSON");
        let malformed_body: serde_json::Value = serde_json::from_slice(
            &to_bytes(malformed.into_body(), 32 * 1024)
                .await
                .expect("malformed body"),
        )
        .expect("malformed JSON");
        for body in [unauthorized_body, malformed_body] {
            assert_eq!(
                body["value"]["problem"]["kind"],
                "not_found_or_not_authorized"
            );
            assert!(body["value"].get("binding_id").is_none());
        }
    }

    struct DropAwarePendingStream {
        drops: Arc<AtomicUsize>,
    }

    impl Stream for DropAwarePendingStream {
        type Item = StreamEvent<serde_json::Value>;

        fn poll_next(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            Poll::Pending
        }
    }

    impl Drop for DropAwarePendingStream {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn disconnect_drops_the_subscription_without_requesting_cancellation() {
        let drops = Arc::new(AtomicUsize::new(0));
        let owner = RecordingOwner::with_events(
            DropAwarePendingStream {
                drops: Arc::clone(&drops),
            },
            frontier(0),
        );
        let cancel_requests = Arc::clone(&owner.cancel_requests);
        let app = operation_event_router(owner);

        let response = app
            .oneshot(
                Request::get("/operations/request.origin/events")
                    .extension(context())
                    .extension(controls())
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);

        drop(response);

        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(cancel_requests.lock().expect("cancel requests").is_empty());
    }

    #[tokio::test]
    async fn event_page_inputs_are_bounded_before_owner_dispatch() {
        for query in [
            "max_events=0",
            "max_events=257",
            "next_sequence=9223372036854775808",
        ] {
            let owner = RecordingOwner::default();
            let event_requests = Arc::clone(&owner.event_requests);
            let response = operation_event_router(owner)
                .oneshot(
                    Request::get(format!("/operations/request.origin/events?{query}"))
                        .extension(context())
                        .extension(controls())
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");

            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
            assert!(
                event_requests.lock().expect("event requests").is_empty(),
                "{query}"
            );
        }
    }
}
