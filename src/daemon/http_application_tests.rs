use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::http::StatusCode;
use axum::routing::post;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::http_application::{DaemonHttpApplicationRegistry, DaemonHttpApplicationService};

const AUTH_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const PROJECT_ID: &str = "project.http-lifecycle";

async fn request(
    service: &DaemonHttpApplicationService,
    authorization: Option<&str>,
    origin: Option<&str>,
) -> String {
    let mut stream = tokio::net::TcpStream::connect(service.endpoint())
        .await
        .expect("connect daemon HTTP application service");
    let mut request = format!(
        "POST /projects/{PROJECT_ID}/application/git/apply HTTP/1.1\r\n\
         Host: {}\r\n\
         Content-Length: 0\r\n\
         Connection: close\r\n",
        service.endpoint()
    );
    if let Some(authorization) = authorization {
        request.push_str(&format!("Authorization: {authorization}\r\n"));
    }
    if let Some(origin) = origin {
        request.push_str(&format!("Origin: {origin}\r\n"));
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
        "/git/apply",
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
async fn daemon_http_requires_bearer_before_git_apply_dispatch() {
    let (service, calls) = service_with_probe().await;
    let response = request(&service, None, Some(service.origin())).await;

    assert_eq!(status(&response), StatusCode::UNAUTHORIZED);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    service.shutdown().await.expect("shutdown HTTP service");
}

#[tokio::test]
async fn daemon_http_requires_exact_local_origin_before_git_apply_dispatch() {
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
                "/git/apply",
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
                    "/git/apply",
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
