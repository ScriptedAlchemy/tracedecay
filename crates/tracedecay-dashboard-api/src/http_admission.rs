//! Listener credential checks and per-request identity, deadline, and cancellation.
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use serde_json::json;

use crate::access::{self, DashboardAccessToken};
use crate::request_deadline::dashboard_http_request_deadline_micros;

#[derive(Clone)]
struct DashboardHttpAdmission {
    port: u16,
    access: DashboardAccessToken,
}

static DASHBOARD_HTTP_REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Transport-owned controls attached to one admitted dashboard HTTP request.
/// Graph adapters pass these values intact to canonical application admission;
/// they never manufacture an actor, grant, scope, or projection generation.
#[derive(Clone, Debug)]
pub struct DashboardHttpRequestControlV1 {
    request_id: tracedecay_contracts::RequestId,
    deadline: tracedecay_contracts::Deadline,
    cancellation: tracedecay_contracts::CancellationSignal,
    observed_at: tracedecay_domain::UtcMicros,
}

pub(crate) const DASHBOARD_REQUEST_CONTROL_MISSING_CODE: &str =
    "dashboard_request_admission_unavailable";
pub(crate) const DASHBOARD_REQUEST_CONTROL_MISSING_DETAIL: &str =
    "dashboard HTTP request admission is unavailable";

/// The admitted request's control, required by every canonical read.
///
/// [`with_dashboard_http_admission`] inserts [`DashboardHttpRequestControlV1`]
/// on every served request, so a missing control means a router was mounted
/// without that layer. That wiring fault is answered here, once, as a 503
/// problem; handlers never observe the missing case and never invent a
/// response for it.
#[derive(Clone, Debug)]
pub struct RequestControl(pub DashboardHttpRequestControlV1);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for RequestControl {
    type Rejection = RequestControlMissing;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<DashboardHttpRequestControlV1>()
            .cloned()
            .map(Self)
            .ok_or(RequestControlMissing)
    }
}

/// Rejection of [`RequestControl`]: the one response for a read served
/// outside the admission layer.
#[derive(Debug)]
pub struct RequestControlMissing;

impl IntoResponse for RequestControlMissing {
    fn into_response(self) -> Response {
        crate::observe::record_error_class("admission_unavailable");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status": "unavailable",
                "error": DASHBOARD_REQUEST_CONTROL_MISSING_CODE,
                "detail": DASHBOARD_REQUEST_CONTROL_MISSING_DETAIL,
            })),
        )
            .into_response()
    }
}

#[cfg(test)]
impl DashboardHttpRequestControlV1 {
    /// One admitted-request control for tests: unbounded deadline, a fresh
    /// active cancellation, `observed_at` = 1. `label` names the request and
    /// cancellation identities so a failing assertion says which test minted it.
    pub(crate) fn test_fixture(label: &str) -> Self {
        Self::test_fixture_with(
            label,
            tracedecay_contracts::CancellationSignal::active(format!("cancel.{label}"))
                .expect("test cancellation"),
            tracedecay_domain::UtcMicros(1),
            tracedecay_domain::UtcMicros(i64::MAX),
        )
    }

    pub(crate) fn test_fixture_with(
        label: &str,
        cancellation: tracedecay_contracts::CancellationSignal,
        observed_at: tracedecay_domain::UtcMicros,
        deadline_at: tracedecay_domain::UtcMicros,
    ) -> Self {
        Self {
            request_id: tracedecay_contracts::RequestId::new(format!("request.{label}"))
                .expect("test request identity"),
            deadline: tracedecay_contracts::Deadline::new(deadline_at).expect("test deadline"),
            cancellation,
            observed_at,
        }
    }
}

impl DashboardHttpRequestControlV1 {
    #[cfg(feature = "test-transport")]
    pub fn from_parts_for_test(
        request_id: tracedecay_contracts::RequestId,
        deadline: tracedecay_contracts::Deadline,
        cancellation: tracedecay_contracts::CancellationSignal,
        observed_at: tracedecay_domain::UtcMicros,
    ) -> Self {
        Self {
            request_id,
            deadline,
            cancellation,
            observed_at,
        }
    }

    pub fn request_id(&self) -> tracedecay_contracts::RequestId {
        self.request_id.clone()
    }

    pub fn deadline(&self) -> tracedecay_contracts::Deadline {
        self.deadline.clone()
    }

    pub fn cancellation(&self) -> &tracedecay_contracts::CancellationSignal {
        &self.cancellation
    }

    pub const fn observed_at(&self) -> tracedecay_domain::UtcMicros {
        self.observed_at
    }
}

struct DashboardHttpCancellationGuard {
    cancellation: tracedecay_contracts::CancellationSignal,
    completed: bool,
}

impl Drop for DashboardHttpCancellationGuard {
    fn drop(&mut self) {
        if !self.completed {
            let _ = self
                .cancellation
                .cancel(tracedecay_session_memory::context::application_observed_at());
        }
    }
}

pub fn with_dashboard_http_admission(
    app: Router,
    addr: std::net::SocketAddr,
    access: DashboardAccessToken,
) -> Router {
    app.layer(middleware::from_fn_with_state(
        DashboardHttpAdmission {
            port: addr.port(),
            access,
        },
        admit_dashboard_http_request,
    ))
}

async fn admit_dashboard_http_request(
    State(admission): State<DashboardHttpAdmission>,
    headers: HeaderMap,
    request: Request<Body>,
    next: Next,
) -> Response {
    let admitted = {
        let _span = tracing::trace_span!("dashboard_api.http.admission").entered();
        admit_dashboard_http_control(admission, headers, request)
    };
    let (request, mut cancellation_guard) = match admitted {
        Ok(admitted) => admitted,
        Err(response) => return *response,
    };
    let response = next.run(request).await;
    cancellation_guard.completed = true;
    crate::observe::observe_response(&response);
    response
}

fn admit_dashboard_http_control(
    admission: DashboardHttpAdmission,
    headers: HeaderMap,
    mut request: Request<Body>,
) -> std::result::Result<(Request<Body>, DashboardHttpCancellationGuard), Box<Response>> {
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return Err(Box::new(dashboard_request_forbidden("missing Host header")));
    };
    let Ok(authority) = host.parse::<axum::http::uri::Authority>() else {
        return Err(Box::new(dashboard_request_forbidden("invalid Host header")));
    };
    let authority_host = authority.host().trim_matches(['[', ']']);
    let loopback_host = authority_host.eq_ignore_ascii_case("localhost")
        || matches!(authority_host, "127.0.0.1" | "::1");
    if !loopback_host || authority.port_u16() != Some(admission.port) {
        return Err(Box::new(dashboard_request_forbidden(
            "Host must name the bound loopback dashboard",
        )));
    }

    if let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    {
        let Ok(origin_uri) = origin.parse::<Uri>() else {
            return Err(Box::new(dashboard_request_forbidden(
                "invalid Origin header",
            )));
        };
        let same_origin = origin_uri.scheme_str() == Some("http")
            && origin_uri.authority().is_some_and(|origin_authority| {
                origin_authority
                    .as_str()
                    .eq_ignore_ascii_case(authority.as_str())
            });
        if !same_origin {
            return Err(Box::new(dashboard_request_forbidden(
                "Origin must match the dashboard",
            )));
        }
    }

    match access::classify_credential(&admission.access, request.method(), request.uri(), &headers)
    {
        access::DashboardCredential::Admitted => {}
        access::DashboardCredential::Launch { location } => {
            return Err(Box::new(dashboard_launch_redirect(&location)));
        }
        access::DashboardCredential::Missing => {
            return Err(Box::new(dashboard_request_unauthenticated()));
        }
    }

    let observed_at = tracedecay_session_memory::context::application_observed_at();
    let sequence = DASHBOARD_HTTP_REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let identity = format!("dashboard.http.{}.{}", observed_at.0, sequence);
    let request_id = match tracedecay_contracts::RequestId::new(format!("request.{identity}")) {
        Ok(request_id) => request_id,
        Err(error) => return Err(Box::new(internal_error_response(error))),
    };
    let cancellation =
        match tracedecay_contracts::CancellationSignal::active(format!("cancel.{identity}")) {
            Ok(cancellation) => cancellation,
            Err(error) => return Err(Box::new(internal_error_response(error))),
        };
    let request_deadline_micros = dashboard_http_request_deadline_micros(request.uri().path());
    let deadline_at =
        tracedecay_domain::UtcMicros(observed_at.0.saturating_add(request_deadline_micros));
    let deadline = match tracedecay_contracts::Deadline::new(deadline_at) {
        Ok(deadline) => deadline,
        Err(error) => return Err(Box::new(internal_error_response(error))),
    };
    request
        .extensions_mut()
        .insert(DashboardHttpRequestControlV1 {
            request_id,
            deadline,
            cancellation: cancellation.clone(),
            observed_at,
        });
    Ok((
        request,
        DashboardHttpCancellationGuard {
            cancellation,
            completed: false,
        },
    ))
}

fn internal_error_response(error: impl std::fmt::Display) -> Response {
    crate::observe::record_error_class("admission_failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "error": "dashboard_request_admission_failed",
            "detail": error.to_string(),
        })),
    )
        .into_response()
}

fn dashboard_request_unauthenticated() -> Response {
    crate::observe::record_error_class("unauthenticated");
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": "dashboard_request_unauthenticated",
            "detail": "open the launch URL that `tracedecay dashboard` printed",
        })),
    )
        .into_response()
}

fn dashboard_launch_redirect(location: &str) -> Response {
    let mut response = axum::response::Redirect::to(location).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        header::HeaderValue::from_static("no-referrer"),
    );
    response
}

fn dashboard_request_forbidden(detail: &'static str) -> Response {
    crate::observe::record_error_class("forbidden");
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": "dashboard_request_forbidden",
            "detail": detail,
        })),
    )
        .into_response()
}
