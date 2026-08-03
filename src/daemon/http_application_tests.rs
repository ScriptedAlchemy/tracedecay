use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::http::StatusCode;
use axum::routing::post;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracedecay_application::{
    CancellationContext, CancellationObservation, CancellationStage, CapabilityGrantId,
    CapabilityGrantSnapshot, Deadline, DisclosureClass, OperationBudgetUsage, OperationReceipt,
    OperationTermination, RequestContext, RequestId, ResolvedScope,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RefId, RepositoryId, UtcMicros, WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::http_application::{DaemonHttpApplicationRegistry, DaemonHttpApplicationService};
use crate::application::operation_stream::{
    OperationEventAuthority, OperationId, OperationKind, OperationStreamConfig,
};

const AUTH_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const PROJECT_ID: &str = "project.http-lifecycle";

async fn request(
    service: &DaemonHttpApplicationService,
    authorization: Option<&str>,
    origin: Option<&str>,
) -> String {
    request_path(
        service,
        "POST",
        &format!("/projects/{PROJECT_ID}/application/tests/results"),
        authorization,
        origin,
    )
    .await
}

async fn request_path(
    service: &DaemonHttpApplicationService,
    method: &str,
    path: &str,
    authorization: Option<&str>,
    origin: Option<&str>,
) -> String {
    let mut stream = tokio::net::TcpStream::connect(service.endpoint())
        .await
        .expect("connect daemon HTTP application service");
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\n\
         Host: {}\r\n\
         Content-Length: 0\r\n\
         Connection: close\r\n",
        service.endpoint()
    );
    if let Some(authorization) = authorization {
        request.push_str("Authorization: ");
        request.push_str(authorization);
        request.push_str("\r\n");
    }
    if let Some(origin) = origin {
        request.push_str("Origin: ");
        request.push_str(origin);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write HTTP request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .await
        .expect("read HTTP response");
    response
}

fn current_micros() -> UtcMicros {
    UtcMicros(
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_micros(),
        )
        .unwrap_or(i64::MAX),
    )
}

fn operation_context(project_id: &ProjectId) -> RequestContext {
    let observed_at = current_micros();
    let expires_at = UtcMicros(observed_at.0.saturating_add(60_000_000));
    let scope = ResolvedScope::new(
        project_id.clone(),
        RepositoryId::new("repository.http-lifecycle").expect("repository"),
        WorktreeId::new("worktree.http-lifecycle").expect("worktree"),
        Some(RefId::new("refs/heads/http-lifecycle").expect("reference")),
    )
    .expect("scope");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.http-lifecycle").expect("grant"),
        1,
        ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).expect("digest"),
        ActorId::new("actor.tracedecay-daemon").expect("issuer"),
        observed_at,
        expires_at,
        scope.clone(),
        BTreeSet::from([CapabilityId::new("capability.git.commit-index").expect("capability")]),
        BTreeSet::from([UseCaseId::new("use-case.git.preview").expect("use case")]),
        DisclosureClass::Metadata,
    )
    .expect("grant");
    RequestContext::new(
        ActorId::new("actor.tracedecay-client").expect("actor"),
        scope,
        grant,
        RequestId::new("request.http-lifecycle.operation").expect("request"),
        Deadline::new(expires_at).expect("deadline"),
        CancellationContext::active("cancel.http-lifecycle.operation").expect("cancellation"),
    )
    .expect("context")
}

fn cancelled_receipt(context: &RequestContext) -> OperationReceipt {
    let observed_at = current_micros();
    let receipt = OperationReceipt {
        started_at: observed_at,
        ended_at: UtcMicros(observed_at.0.saturating_add(1)),
        effective_deadline: context.deadline().clone(),
        cancellation: Some(CancellationObservation {
            stage: CancellationStage::DuringRead,
            observed_at,
        }),
        budget: OperationBudgetUsage::default(),
        termination: OperationTermination::Cancelled,
    };
    receipt.validate().expect("cancelled receipt");
    receipt
}

fn resume_token(response: &str) -> &str {
    let marker = "\"resume_token\":\"";
    let start = response.find(marker).expect("SSE resume token") + marker.len();
    let remaining = &response[start..];
    &remaining[..remaining.find('"').expect("SSE resume token terminator")]
}

async fn service_with_canonical_application(
    authority: OperationEventAuthority,
    project_id: &ProjectId,
) -> (
    DaemonHttpApplicationService,
    tokio::task::JoinHandle<()>,
    tempfile::TempDir,
) {
    let project = tempfile::tempdir().expect("canonical application project");
    let broker = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind observation broker");
    let broker_endpoint =
        super::DaemonEndpoint::loopback(broker.local_addr().expect("observation broker address"))
            .expect("loopback observation endpoint");
    let broker_task = tokio::spawn(async move {
        while let Ok((stream, _)) = broker.accept().await {
            drop(stream);
        }
    });
    let handshake = super::DaemonHandshake::for_current_client(
        Some(project.path().to_path_buf()),
        None,
        false,
        false,
    )
    .expect("canonical application handshake");
    let client = crate::daemon_client::DaemonInvocationClient::for_connection_for_test(
        super::DaemonConnection {
            endpoint: broker_endpoint,
            auth_token: None,
            authority_record: None,
        },
        handshake,
    );
    let canonical =
        crate::application_surface::http_application_router(client, authority, project_id.clone())
            .expect("canonical HTTP application router");
    let registry = DaemonHttpApplicationRegistry::default();
    registry
        .mount(project_id.as_str(), canonical)
        .await
        .expect("mount canonical HTTP application router");
    let service = DaemonHttpApplicationService::bind(registry, AUTH_TOKEN)
        .await
        .expect("bind daemon HTTP application service");
    (service, broker_task, project)
}

fn status(response: &str) -> StatusCode {
    let code = response
        .split_whitespace()
        .nth(1)
        .expect("HTTP status")
        .parse::<u16>()
        .expect("numeric HTTP status");
    StatusCode::from_u16(code).expect("known HTTP status")
}

async fn service_with_probe() -> (DaemonHttpApplicationService, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let probe_calls = Arc::clone(&calls);
    let canonical = Router::new().route(
        "/tests/results",
        post(move || {
            let calls = Arc::clone(&probe_calls);
            async move {
                calls.fetch_add(1, Ordering::Relaxed);
                StatusCode::NO_CONTENT
            }
        }),
    );
    let registry = DaemonHttpApplicationRegistry::default();
    registry
        .mount(PROJECT_ID, canonical)
        .await
        .expect("mount canonical application router");
    let service = DaemonHttpApplicationService::bind(registry, AUTH_TOKEN)
        .await
        .expect("bind daemon HTTP application service");
    (service, calls)
}

#[tokio::test]
async fn daemon_http_requires_bearer_before_application_dispatch() {
    let (service, calls) = service_with_probe().await;
    let response = request(&service, None, Some(service.origin())).await;

    assert_eq!(status(&response), StatusCode::UNAUTHORIZED);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    service.shutdown().await.expect("shutdown HTTP service");
}

#[tokio::test]
async fn daemon_http_rejects_bearer_tokens_that_differ_by_content_or_length() {
    let (service, calls) = service_with_probe().await;
    let origin = service.origin().to_owned();
    let different = format!("Bearer {}", "f".repeat(AUTH_TOKEN.len()));
    let different_response = request(&service, Some(&different), Some(&origin)).await;
    assert_eq!(status(&different_response), StatusCode::UNAUTHORIZED);

    let short = "Bearer f";
    let short_response = request(&service, Some(short), Some(&origin)).await;
    assert_eq!(status(&short_response), StatusCode::UNAUTHORIZED);

    let empty_response = request(&service, Some(""), Some(&origin)).await;
    assert_eq!(status(&empty_response), StatusCode::UNAUTHORIZED);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    service.shutdown().await.expect("shutdown HTTP service");
}

#[tokio::test]
async fn daemon_http_requires_exact_local_origin_before_application_dispatch() {
    let (service, calls) = service_with_probe().await;
    let authorization = format!("Bearer {AUTH_TOKEN}");
    let response = request(
        &service,
        Some(&authorization),
        Some("http://attacker.invalid"),
    )
    .await;

    assert_eq!(status(&response), StatusCode::FORBIDDEN);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    service.shutdown().await.expect("shutdown HTTP service");
}

#[tokio::test]
async fn daemon_http_dispatches_authenticated_project_route_to_canonical_router() {
    let (service, calls) = service_with_probe().await;
    let authorization = format!("Bearer {AUTH_TOKEN}");
    let origin = service.origin().to_owned();
    let response = request(&service, Some(&authorization), Some(&origin)).await;

    assert_eq!(status(&response), StatusCode::NO_CONTENT);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    service.shutdown().await.expect("shutdown HTTP service");
}

#[tokio::test]
async fn daemon_http_authenticated_operations_cancel_and_resume_through_canonical_owner() {
    let project_id = ProjectId::new(PROJECT_ID).expect("project");
    let authority = OperationEventAuthority::new(OperationStreamConfig {
        retained_event_capacity: 4,
        max_operations: 4,
        max_subscribers_per_operation: 2,
    })
    .expect("operation authority");
    let context = operation_context(&project_id);
    let operation_id = OperationId::from_request(context.request_id().clone());
    let emitter = authority
        .begin(&context, OperationKind::GitPreview, current_micros())
        .await
        .expect("begin operation");
    let (service, broker_task, _project) =
        service_with_canonical_application(authority, &project_id).await;
    let operation_path = format!("/projects/{PROJECT_ID}/application/operations/{operation_id}");
    let origin = service.origin().to_owned();
    let authorization = format!("Bearer {AUTH_TOKEN}");

    let unauthenticated = request_path(
        &service,
        "GET",
        &format!("{operation_path}/events"),
        None,
        Some(&origin),
    )
    .await;
    assert_eq!(status(&unauthenticated), StatusCode::UNAUTHORIZED);

    let cancelled = request_path(
        &service,
        "POST",
        &format!("{operation_path}/cancel"),
        Some(&authorization),
        Some(&origin),
    )
    .await;
    assert_eq!(
        status(&cancelled),
        StatusCode::ACCEPTED,
        "unexpected cancel response: {cancelled}"
    );
    assert!(cancelled.contains("\"status\":\"requested\""));
    assert!(emitter.is_cancelled());
    emitter
        .progress(1, Some(1))
        .await
        .expect("publish progress");
    emitter
        .terminal(cancelled_receipt(&context))
        .await
        .expect("publish terminal");

    let initial = request_path(
        &service,
        "GET",
        &format!("{operation_path}/events?next_sequence=0"),
        Some(&authorization),
        Some(&origin),
    )
    .await;
    assert_eq!(status(&initial), StatusCode::OK);
    assert!(initial.contains("content-type: text/event-stream"));
    assert!(initial.contains("event: open"));
    assert!(initial.contains("event: progress"));
    assert_eq!(initial.matches("event: cancelled").count(), 1);
    let token = resume_token(&initial);

    let resumed = request_path(
        &service,
        "GET",
        &format!("{operation_path}/events?next_sequence=1&resume_token={token}"),
        Some(&authorization),
        Some(&origin),
    )
    .await;
    assert_eq!(status(&resumed), StatusCode::OK);
    assert!(resumed.contains("event: open"));
    assert!(resumed.contains("event: progress"));
    assert_eq!(resumed.matches("event: cancelled").count(), 1);

    service.shutdown().await.expect("shutdown HTTP service");
    broker_task.abort();
}

#[tokio::test]
async fn daemon_http_accepts_project_router_mounted_after_listener_start() {
    let registry = DaemonHttpApplicationRegistry::default();
    let service = DaemonHttpApplicationService::bind(registry.clone(), AUTH_TOKEN)
        .await
        .expect("bind daemon HTTP application service");
    let calls = Arc::new(AtomicUsize::new(0));
    let probe_calls = Arc::clone(&calls);
    registry
        .mount(
            PROJECT_ID,
            Router::new().route(
                "/tests/results",
                post(move || {
                    let calls = Arc::clone(&probe_calls);
                    async move {
                        calls.fetch_add(1, Ordering::Relaxed);
                        StatusCode::NO_CONTENT
                    }
                }),
            ),
        )
        .await
        .expect("mount project router after listener start");
    let authorization = format!("Bearer {AUTH_TOKEN}");
    let origin = service.origin().to_owned();

    let response = request(&service, Some(&authorization), Some(&origin)).await;

    assert_eq!(status(&response), StatusCode::NO_CONTENT);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    service.shutdown().await.expect("shutdown HTTP service");
}

#[tokio::test]
async fn daemon_http_cold_entry_resolves_project_before_canonical_dispatch() {
    let registry = DaemonHttpApplicationRegistry::default();
    let resolver_calls = Arc::new(AtomicUsize::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let resolved_calls = Arc::clone(&calls);
    let observed_resolver_calls = Arc::clone(&resolver_calls);
    registry
        .install_resolver(move |project_id| {
            let calls = Arc::clone(&resolved_calls);
            let resolver_calls = Arc::clone(&observed_resolver_calls);
            async move {
                resolver_calls.fetch_add(1, Ordering::Relaxed);
                assert_eq!(project_id.as_str(), PROJECT_ID);
                Ok(Some(Router::new().route(
                    "/tests/results",
                    post(move || {
                        let calls = Arc::clone(&calls);
                        async move {
                            calls.fetch_add(1, Ordering::Relaxed);
                            StatusCode::NO_CONTENT
                        }
                    }),
                )))
            }
        })
        .expect("install cold project resolver");
    let service = DaemonHttpApplicationService::bind(registry, AUTH_TOKEN)
        .await
        .expect("bind daemon HTTP application service");
    let authorization = format!("Bearer {AUTH_TOKEN}");
    let origin = service.origin().to_owned();

    let first = request(&service, Some(&authorization), Some(&origin)).await;
    let second = request(&service, Some(&authorization), Some(&origin)).await;

    assert_eq!(status(&first), StatusCode::NO_CONTENT);
    assert_eq!(status(&second), StatusCode::NO_CONTENT);
    assert_eq!(resolver_calls.load(Ordering::Relaxed), 1);
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    service.shutdown().await.expect("shutdown HTTP service");
}

#[tokio::test]
async fn daemon_http_shutdown_releases_loopback_listener() {
    let (service, _) = service_with_probe().await;
    let endpoint = service.endpoint();
    service.shutdown().await.expect("shutdown HTTP service");

    assert!(tokio::net::TcpStream::connect(endpoint).await.is_err());
}

#[tokio::test]
async fn daemon_http_shutdown_marks_registry_inactive() {
    let registry = DaemonHttpApplicationRegistry::default();
    let service = DaemonHttpApplicationService::bind(registry.clone(), AUTH_TOKEN)
        .await
        .expect("bind daemon HTTP application service");
    assert!(registry.is_active());

    service.shutdown().await.expect("shutdown HTTP service");

    assert!(!registry.is_active());
}
