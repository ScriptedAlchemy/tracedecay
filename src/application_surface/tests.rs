use std::collections::BTreeSet;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;
use tracedecay_application::{
    CancellationContext, CancellationSignal, CancellationState, CapabilityGrantId,
    CapabilityGrantSnapshot, Deadline, DisclosureClass, OperationReceipt, PageRequest,
    RequestContext, RequestId, ResolvedScope, StreamEvent,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RefId, RepositoryId, UtcMicros, WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::{
    ApplicationSurfaceOperation, ApplicationSurfaceRequest, DEFAULT_DEADLINE_MICROS,
    FeedbackSurfaceRequest, HttpCancellationRegistry, HttpDisconnectCancellation,
    HttpOperationEventState, application_surface_dispatch_input_with_controls, current_micros,
    http_operation_event_router, parse_application_surface_request, plan26_sse_stream_event,
    resolve_authenticated_http_request_context,
};
use crate::application::operation_stream::{
    OperationEventAuthority, OperationEventError, OperationId, OperationKind, OperationStreamConfig,
};
use crate::application::primitives::Pr12PrimitiveRequest;
use crate::daemon_client::RequestedOutputFormat;

fn operation_context(project_id: &ProjectId) -> RequestContext {
    let observed_at = current_micros().expect("current time");
    let expires_at = UtcMicros(observed_at.0.saturating_add(60_000_000));
    let scope = ResolvedScope::new(
        project_id.clone(),
        RepositoryId::new("repository.http-adapter").expect("repository"),
        WorktreeId::new("worktree.http-adapter").expect("worktree"),
        Some(RefId::new("refs/heads/http-adapter").expect("reference")),
    )
    .expect("scope");
    let capability = CapabilityId::new("capability.git.commit-index").expect("capability");
    let use_case = UseCaseId::new("use-case.git.preview").expect("use case");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.http-adapter").expect("grant"),
        1,
        ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).expect("digest"),
        ActorId::new("actor.tracedecay-daemon").expect("issuer"),
        observed_at,
        expires_at,
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Metadata,
    )
    .expect("grant");
    RequestContext::new(
        ActorId::new("actor.tracedecay-client").expect("actor"),
        scope,
        grant,
        RequestId::new("request.http-adapter").expect("request"),
        Deadline::new(expires_at).expect("deadline"),
        CancellationContext::active("cancel.http-adapter").expect("cancellation"),
    )
    .expect("context")
}

async fn response_text(response: axum::response::Response) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body")
            .to_vec(),
    )
    .expect("UTF-8 response")
}

#[test]
fn dispatch_controls_retain_the_callers_deadline_and_live_cancellation_identity() {
    let deadline = Deadline::new(UtcMicros(91)).expect("deadline");
    let cancellation =
        CancellationSignal::active("cancel.application-surface").expect("cancellation");
    let caller = cancellation.clone();
    let input = application_surface_dispatch_input_with_controls(
        ApplicationSurfaceOperation::FeedbackList,
        RequestId::new("request.application-surface").expect("request"),
        ApplicationSurfaceRequest::Feedback(
            FeedbackSurfaceRequest::new("feedback-handle.fixture".to_owned()).expect("handle"),
        ),
        PageRequest::first(7).expect("page"),
        Some(deadline.clone()),
        cancellation,
        RequestedOutputFormat::Json,
    )
    .expect("dispatch input");

    caller.cancel(UtcMicros(41));
    assert_eq!(input.controls.deadline, Some(deadline));
    assert!(matches!(
        input.controls.cancellation.context().state,
        CancellationState::Cancelled {
            requested_at: UtcMicros(41)
        }
    ));
}

#[test]
fn sse_item_maps_to_content_free_delivery_lifecycle() {
    let event = StreamEvent::item(7, "content-is-not-observed").expect("stream item");
    assert_eq!(
        plan26_sse_stream_event(&event),
        Some((
            crate::application::feedback::observations::Plan26SseLifecycleV1::EventDelivered,
            1,
            false,
        ))
    );
}

#[test]
fn dropped_http_request_cancels_the_registered_transport_token() {
    let request_id = RequestId::new("request.http.disconnect").expect("request");
    let cancellation = CancellationSignal::active("cancel.http.disconnect").expect("cancellation");
    let registry: HttpCancellationRegistry = Arc::default();
    registry
        .lock()
        .expect("registry")
        .insert(request_id.clone(), cancellation.clone());

    drop(HttpDisconnectCancellation::new(
        registry,
        request_id,
        cancellation.clone(),
    ));

    assert!(cancellation.is_cancelled());
}

fn open_resume_token(body: &str) -> String {
    body.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .find(|value| value["event"] == "open")
        .and_then(|value| {
            value["data"]["frontier"]["resume_token"]
                .as_str()
                .map(str::to_owned)
        })
        .expect("open event has a real resume token")
}

fn sse_event_count(body: &str, event: &str) -> usize {
    body.lines()
        .filter_map(|line| line.strip_prefix("event:"))
        .filter(|name| name.trim() == event)
        .count()
}

fn completed_receipt(context: &RequestContext) -> OperationReceipt {
    let started_at = current_micros().expect("current time");
    OperationReceipt::completed(
        started_at,
        UtcMicros(started_at.0.saturating_add(1)),
        context.deadline().clone(),
        OperationBudgetUsage::default(),
    )
    .expect("completed receipt")
}

#[tokio::test]
async fn authenticated_context_reuses_exact_scope_and_transport_controls() {
    let project_id = ProjectId::new("project.http-adapter").expect("project");
    let authority = OperationEventAuthority::default();
    let original = operation_context(&project_id);
    let operation_id = OperationId::from_request(original.request_id().clone());
    let _emitter = authority
        .begin(
            &original,
            OperationKind::GitPreview,
            current_micros().expect("current time"),
        )
        .await
        .expect("begin operation");
    let state = HttpOperationEventState {
        authority,
        active_project_id: project_id,
        cancellations: Arc::default(),
        client: None,
    };
    let observed_at = current_micros().expect("current time");
    let request_id = RequestId::new("request.http.subscription").expect("HTTP request");
    let cancellation =
        CancellationContext::active("cancel.http.subscription").expect("HTTP cancellation");

    let resolved = resolve_authenticated_http_request_context(
        &state,
        &operation_id,
        request_id.clone(),
        cancellation.clone(),
        observed_at,
        None,
    )
    .await
    .expect("resolved context");

    assert_eq!(resolved.actor(), original.actor());
    assert_eq!(resolved.scope(), original.scope());
    assert_eq!(
        resolved.grant().allowed_capabilities,
        original.grant().allowed_capabilities
    );
    assert_eq!(
        resolved.grant().allowed_use_cases,
        original.grant().allowed_use_cases
    );
    assert_eq!(resolved.request_id(), &request_id);
    assert_eq!(resolved.cancellation(), &cancellation);
    assert_eq!(
        resolved.deadline().expires_at,
        UtcMicros(observed_at.0.saturating_add(DEFAULT_DEADLINE_MICROS))
    );
}

#[tokio::test]
async fn sse_disconnect_does_not_cancel_but_explicit_cancel_does() {
    let project_id = ProjectId::new("project.http-adapter").expect("project");
    let authority = OperationEventAuthority::default();
    let context = operation_context(&project_id);
    let operation_id = OperationId::from_request(context.request_id().clone());
    let emitter = authority
        .begin(
            &context,
            OperationKind::GitPreview,
            current_micros().expect("current time"),
        )
        .await
        .expect("begin operation");
    let app = http_operation_event_router(authority, project_id, Arc::default(), None);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/operations/{operation_id}/events?next_sequence=0"))
                .body(Body::empty())
                .expect("SSE request"),
        )
        .await
        .expect("SSE response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    drop(response);
    assert!(!emitter.is_cancelled());

    let cancelled = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/operations/{operation_id}/cancel"))
                .body(Body::empty())
                .expect("cancel request"),
        )
        .await
        .expect("cancel response");
    assert_eq!(cancelled.status(), StatusCode::ACCEPTED);
    assert!(emitter.is_cancelled());
}

#[tokio::test]
async fn sse_scope_denial_is_concealed_at_the_active_project_mount() {
    let operation_project = ProjectId::new("project.http-adapter").expect("project");
    let authority = OperationEventAuthority::default();
    let context = operation_context(&operation_project);
    let operation_id = OperationId::from_request(context.request_id().clone());
    let _emitter = authority
        .begin(
            &context,
            OperationKind::GitPreview,
            current_micros().expect("current time"),
        )
        .await
        .expect("begin operation");
    let app = http_operation_event_router(
        authority,
        ProjectId::new("project.other").expect("other project"),
        Arc::default(),
        None,
    );

    let denied = app
        .oneshot(
            Request::builder()
                .uri(format!("/operations/{operation_id}/events"))
                .body(Body::empty())
                .expect("denied request"),
        )
        .await
        .expect("denied response");
    assert_eq!(denied.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn resolver_conceals_cross_project_scope_with_one_typed_denial() {
    let operation_project = ProjectId::new("project.http-adapter").expect("project");
    let authority = OperationEventAuthority::default();
    let context = operation_context(&operation_project);
    let operation_id = OperationId::from_request(context.request_id().clone());
    let _emitter = authority
        .begin(
            &context,
            OperationKind::GitPreview,
            current_micros().expect("current time"),
        )
        .await
        .expect("begin operation");
    let state = HttpOperationEventState {
        authority,
        active_project_id: ProjectId::new("project.other").expect("other project"),
        cancellations: Arc::default(),
        client: None,
    };

    let denied = resolve_authenticated_http_request_context(
        &state,
        &operation_id,
        RequestId::new("request.http.denied").expect("request"),
        CancellationContext::active("cancel.http.denied").expect("cancellation"),
        current_micros().expect("current time"),
        None,
    )
    .await;

    assert_eq!(
        denied.expect_err("cross-project scope must be concealed"),
        OperationEventError::NotFoundOrNotAuthorized
    );
}

#[test]
fn storage_status_empty_request_uses_typed_default() {
    let request = parse_application_surface_request(
        ApplicationSurfaceOperation::StorageStatus,
        serde_json::json!({}),
    )
    .expect("empty storage-status request");
    assert!(matches!(
        request,
        ApplicationSurfaceRequest::Primitive(Pr12PrimitiveRequest::StorageStatus(request))
            if !request.include_details
    ));
}

#[tokio::test]
async fn sse_resume_replays_retained_history_with_one_terminal_receipt() {
    let project_id = ProjectId::new("project.http-adapter").expect("project");
    let authority = OperationEventAuthority::new(OperationStreamConfig {
        retained_event_capacity: 2,
        max_operations: 8,
        max_subscribers_per_operation: 2,
    })
    .expect("operation authority");
    let context = operation_context(&project_id);
    let operation_id = OperationId::from_request(context.request_id().clone());
    let emitter = authority
        .begin(
            &context,
            OperationKind::GitPreview,
            current_micros().expect("current time"),
        )
        .await
        .expect("begin operation");
    let app = http_operation_event_router(authority, project_id, Arc::default(), None);

    let slow_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/operations/{operation_id}/events"))
                .body(Body::empty())
                .expect("initial SSE request"),
        )
        .await
        .expect("initial SSE response");
    assert_eq!(slow_response.status(), StatusCode::OK);

    for completed in 1..=4 {
        emitter
            .progress(completed, Some(4))
            .await
            .expect("publish progress");
    }
    let receipt = completed_receipt(&context);
    let terminal = emitter
        .terminal(receipt.clone())
        .await
        .expect("publish terminal");
    assert_eq!(
        emitter
            .terminal(receipt)
            .await
            .expect("idempotent terminal"),
        terminal
    );

    let slow_body = response_text(slow_response).await;
    let resume_token = open_resume_token(&slow_body);
    assert_eq!(sse_event_count(&slow_body, "resume_gap"), 1);
    assert_eq!(sse_event_count(&slow_body, "completed"), 1);

    let tokenless = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/operations/{operation_id}/events?next_sequence=3"))
                .body(Body::empty())
                .expect("tokenless resume request"),
        )
        .await
        .expect("tokenless resume response");
    assert_eq!(tokenless.status(), StatusCode::CONFLICT);

    let resumed = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/operations/{operation_id}/events?next_sequence=3&resume_token={resume_token}"
                ))
                .body(Body::empty())
                .expect("resume SSE request"),
        )
        .await
        .expect("resume SSE response");
    assert_eq!(resumed.status(), StatusCode::OK);
    let resumed_body = response_text(resumed).await;
    assert_eq!(sse_event_count(&resumed_body, "resume_gap"), 1);
    assert!(resumed_body.contains("\"first_missing_sequence\":3"));
    assert!(resumed_body.contains("\"last_missing_sequence\":3"));
    assert_eq!(sse_event_count(&resumed_body, "completed"), 1);
}

#[tokio::test]
async fn sse_resume_after_memory_restart_returns_canonical_expired_problem() {
    let project_id = ProjectId::new("project.http-adapter").expect("project");
    let authority = OperationEventAuthority::default();
    let context = operation_context(&project_id);
    let operation_id = OperationId::from_request(context.request_id().clone());
    let emitter = authority
        .begin(
            &context,
            OperationKind::GitPreview,
            current_micros().expect("current time"),
        )
        .await
        .expect("begin operation");
    emitter
        .terminal(completed_receipt(&context))
        .await
        .expect("publish terminal");
    let live_app = http_operation_event_router(authority, project_id.clone(), Arc::default(), None);
    let initial = live_app
        .oneshot(
            Request::builder()
                .uri(format!("/operations/{operation_id}/events"))
                .body(Body::empty())
                .expect("initial SSE request"),
        )
        .await
        .expect("initial SSE response");
    let resume_token = open_resume_token(&response_text(initial).await);

    let restarted_app = http_operation_event_router(
        OperationEventAuthority::default(),
        project_id,
        Arc::default(),
        None,
    );
    let expired = restarted_app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/operations/{operation_id}/events?next_sequence=1&resume_token={resume_token}"
                ))
                .body(Body::empty())
                .expect("expired resume request"),
        )
        .await
        .expect("expired resume response");

    assert_eq!(expired.status(), StatusCode::CONFLICT);
    let problem =
        serde_json::from_str::<Value>(&response_text(expired).await).expect("expired problem JSON");
    assert_eq!(problem["kind"], "problem");
    assert_eq!(problem["value"]["problem"]["kind"], "stale");
    assert_eq!(problem["value"]["problem"]["revision"], 1);
    assert_eq!(problem["value"]["problem"]["owning_layer"], "runtime");
    assert_eq!(problem["value"]["problem"]["terminality"], "pre_admission");
    assert_eq!(problem["value"]["problem"]["retry"], "after_revalidate");
    assert_eq!(problem["value"]["problem"]["retry_scope"], "fresh_request");
    assert_eq!(
        problem["value"]["problem"]["request_id"],
        problem["value"]["problem"]["trace_id"]
    );
    assert_eq!(
        problem["value"]["problem"]["code"],
        "operation_event.resume_expired"
    );
}
