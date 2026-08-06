//! Thin HTTP boundary for the authenticated remote Brain protocol.
//!
//! HTTP carries versioned application payloads and opaque credential headers.
//! The application-owned credential authority authenticates a request-scoped
//! session before this adapter reads any body bytes.

use std::fmt;
use std::hint::black_box;
use std::marker::PhantomData;
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, FromRequestParts, State};
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_application::remote::auth::OpaqueRemoteCredential;
use tracedecay_application::remote::credential_admission::{
    RemoteAuthenticatedSessionV1, RemoteCredentialAdmissionPortV1, RemoteSessionBoundProtocolBodyV1,
};
use tracedecay_application::remote::protocol::{
    EnrollmentRequestV1, RemoteEnrollmentProtocolPortV1, RemoteProtocolExecutionControlV1,
    RemoteProtocolFailureV1, RemoteProtocolPortV1, RemoteProtocolRequestV1,
    RemoteProtocolResponseV1, RemoteProtocolServiceV1, remote_protocol_problem,
};
use tracedecay_application::remote::query::{RemoteQueryRequestV1, RemoteQueryResultV1};
use tracedecay_application::remote::recovery::{
    BackupOperationStateV1, BackupRequestV1, PromotionCasReceiptV1, PromotionConfirmationV1,
    StagedRestoreConfirmationV1, StagedRestoreProgressV1,
};
use tracedecay_application::remote::replay::{RemoteReplayOutcomeV1, RemoteReplayRequestV1};
use tracedecay_application::{
    ApplicationProblemKind, CancellationSignal, RequestId, ResultContractRef,
};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::SchemaId;

const BEARER_PREFIX: &[u8] = b"Bearer ";
const MAX_REMOTE_HTTP_BODY_BYTES: usize = 1024 * 1024;
pub const REMOTE_ENROLLMENT_CREDENTIAL_HEADER: &str = "x-tracedecay-enrollment-credential";

/// Parsed HTTP credential header. It cannot be cloned, serialized, or logged.
pub struct RemoteAuthorizationHeader {
    credential: OpaqueRemoteCredential,
}

impl RemoteAuthorizationHeader {
    /// Consume an owned authorization header so the adapter does not retain a
    /// second plaintext copy after admission.
    pub fn from_owned_bytes(mut header: Vec<u8>) -> Result<Self, RemoteHttpBoundaryError> {
        if !header.starts_with(BEARER_PREFIX) {
            zeroize_rejected(&mut header);
            return Err(RemoteHttpBoundaryError::MissingOrInvalidAuthorization);
        }
        header.drain(..BEARER_PREFIX.len());
        let credential = match OpaqueRemoteCredential::new(header.into_boxed_slice()) {
            Ok(credential) => credential,
            Err(_) => return Err(RemoteHttpBoundaryError::MissingOrInvalidAuthorization),
        };
        Ok(Self { credential })
    }

    pub fn into_credential(self) -> OpaqueRemoteCredential {
        self.credential
    }
}

impl fmt::Debug for RemoteAuthorizationHeader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteAuthorizationHeader([REDACTED])")
    }
}

fn zeroize_rejected(bytes: &mut [u8]) {
    bytes.fill(0);
    black_box(bytes);
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RemoteHttpBoundaryError {
    #[error("remote authorization is missing or invalid")]
    MissingOrInvalidAuthorization,
}

/// Wire request body. Secret material is supplied only through HTTP headers.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteHttpRequestV1<T> {
    pub request: RemoteProtocolRequestV1<T>,
}

/// HTTP response is a transparent presentation of the canonical response.
#[derive(Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteHttpResponseV1<T> {
    pub response: RemoteProtocolResponseV1<T>,
}

impl<T> From<RemoteProtocolResponseV1<T>> for RemoteHttpResponseV1<T> {
    fn from(response: RemoteProtocolResponseV1<T>) -> Self {
        Self { response }
    }
}

struct RemoteProtocolRouterStateV1<Port> {
    service: Arc<RemoteProtocolServiceV1<Port>>,
    credential_admission: Arc<dyn RemoteCredentialAdmissionPortV1>,
    clock: fn() -> UtcMicros,
}

impl<Port> Clone for RemoteProtocolRouterStateV1<Port> {
    fn clone(&self) -> Self {
        Self {
            service: Arc::clone(&self.service),
            credential_admission: Arc::clone(&self.credential_admission),
            clock: self.clock,
        }
    }
}

/// Request-scoped proof created before Axum may consume the body.
///
/// It intentionally implements neither `Clone`, `Debug`, nor serialization.
struct RemotePreBodyAdmissionV1<Request> {
    session: RemoteAuthenticatedSessionV1,
    credential: OpaqueRemoteCredential,
    request: PhantomData<fn() -> Request>,
}

impl<Port, Request> FromRequestParts<RemoteProtocolRouterStateV1<Port>>
    for RemotePreBodyAdmissionV1<Request>
where
    Port: Send + Sync,
    Request: RemoteSessionBoundProtocolBodyV1,
{
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &RemoteProtocolRouterStateV1<Port>,
    ) -> Result<Self, Self::Rejection> {
        let authorization = authorization_header(&parts.headers)
            .map_err(|_| concealed_authentication_response())?;
        let credential = authorization.into_credential();
        let session = state
            .credential_admission
            .admit_before_body(&credential, Request::CREDENTIAL_USE, (state.clock)())
            .map_err(|_| concealed_authentication_response())?;
        Ok(Self {
            session,
            credential,
            request: PhantomData,
        })
    }
}

struct RemoteEnrollmentPreBodyAdmissionV1 {
    session: RemoteAuthenticatedSessionV1,
    grant_credential: OpaqueRemoteCredential,
    enrollment_credential: OpaqueRemoteCredential,
}

struct CancelRemoteRequestOnDropV1 {
    cancellation: CancellationSignal,
    clock: fn() -> UtcMicros,
    armed: bool,
}

impl CancelRemoteRequestOnDropV1 {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancelRemoteRequestOnDropV1 {
    fn drop(&mut self) {
        if self.armed {
            self.cancellation.cancel((self.clock)());
        }
    }
}

impl<Port> FromRequestParts<RemoteProtocolRouterStateV1<Port>>
    for RemoteEnrollmentPreBodyAdmissionV1
where
    Port: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &RemoteProtocolRouterStateV1<Port>,
    ) -> Result<Self, Self::Rejection> {
        let authorization = authorization_header(&parts.headers)
            .map_err(|_| concealed_authentication_response())?;
        let grant_credential = authorization.into_credential();
        let session = state
            .credential_admission
            .admit_before_body(
                &grant_credential,
                <EnrollmentRequestV1 as RemoteSessionBoundProtocolBodyV1>::CREDENTIAL_USE,
                (state.clock)(),
            )
            .map_err(|_| concealed_authentication_response())?;
        let enrollment_credential = enrollment_credential(&parts.headers)
            .map_err(|_| concealed_authentication_response())?;
        Ok(Self {
            session,
            grant_credential,
            enrollment_credential,
        })
    }
}

/// Build the sole Remote Brain HTTP router.
///
/// The central composition root supplies the production protocol port, the
/// fingerprint-indexed final credential authority, and the canonical runtime
/// clock. Authentication occurs in a parts-only extractor before Axum polls or
/// deserializes the JSON body. The typed body is then bound to that exact
/// request-scoped session before delegation.
pub fn remote_protocol_router<Port>(
    port: Port,
    credential_admission: Arc<dyn RemoteCredentialAdmissionPortV1>,
    clock: fn() -> UtcMicros,
) -> Router
where
    Port: RemoteEnrollmentProtocolPortV1
        + RemoteProtocolPortV1<RemoteReplayRequestV1, Output = RemoteReplayOutcomeV1>
        + RemoteProtocolPortV1<RemoteQueryRequestV1, Output = RemoteQueryResultV1>
        + RemoteProtocolPortV1<BackupRequestV1, Output = BackupOperationStateV1>
        + RemoteProtocolPortV1<StagedRestoreConfirmationV1, Output = StagedRestoreProgressV1>
        + RemoteProtocolPortV1<PromotionConfirmationV1, Output = PromotionCasReceiptV1>
        + Send
        + Sync
        + 'static,
{
    let state = RemoteProtocolRouterStateV1 {
        service: Arc::new(RemoteProtocolServiceV1::new(port)),
        credential_admission,
        clock,
    };
    Router::new()
        .route("/enrollment", post(enrollment_route::<Port>))
        .route(
            "/replay",
            post(protocol_route::<Port, RemoteReplayRequestV1>),
        )
        .route("/query", post(protocol_route::<Port, RemoteQueryRequestV1>))
        .route("/backup", post(protocol_route::<Port, BackupRequestV1>))
        .route(
            "/restore",
            post(protocol_route::<Port, StagedRestoreConfirmationV1>),
        )
        .route(
            "/failover",
            post(protocol_route::<Port, PromotionConfirmationV1>),
        )
        .layer(DefaultBodyLimit::max(MAX_REMOTE_HTTP_BODY_BYTES))
        .with_state(state)
}

async fn protocol_route<Port, Request>(
    State(state): State<RemoteProtocolRouterStateV1<Port>>,
    admission: RemotePreBodyAdmissionV1<Request>,
    payload: Result<Json<RemoteHttpRequestV1<Request>>, JsonRejection>,
) -> Response
where
    Port: RemoteProtocolPortV1<Request> + Send + Sync + 'static,
    Request: DeserializeOwned + RemoteSessionBoundProtocolBodyV1 + Send + 'static,
    Port::Output: Serialize + Send + 'static,
{
    let Json(request) = match payload {
        Ok(payload) => payload,
        Err(_) => return invalid_remote_request_response(),
    };
    let RemotePreBodyAdmissionV1 {
        mut session,
        credential,
        ..
    } = admission;
    if Request::bind_authenticated_session(&session, &request.request).is_err() {
        return concealed_authentication_response();
    }
    if Request::REAUTHORIZE_BEFORE_EXECUTION {
        session = match state
            .credential_admission
            .reauthorize_publication(&session, (state.clock)())
        {
            Ok(session) => session,
            Err(_) => return concealed_authentication_response(),
        };
        if Request::bind_authenticated_session(&session, &request.request).is_err() {
            return concealed_authentication_response();
        }
    }
    let Some(enrollment_deadline) = session.enrollment_expires_at() else {
        return concealed_authentication_response();
    };
    let deadline = request
        .request
        .body
        .execution_expires_at()
        .map_or(enrollment_deadline, |request_deadline| {
            request_deadline.min(enrollment_deadline)
        });
    let cancellation = match CancellationSignal::active(format!(
        "cancel.remote.http.{}",
        request.request.request_id.as_str()
    )) {
        Ok(cancellation) => cancellation,
        Err(_) => return invalid_remote_request_response(),
    };
    let mut cancel_on_drop = CancelRemoteRequestOnDropV1 {
        cancellation: cancellation.clone(),
        clock: state.clock,
        armed: true,
    };
    let control = RemoteProtocolExecutionControlV1 {
        deadline,
        cancellation,
    };
    let service = Arc::clone(&state.service);
    let execution = tokio::task::spawn_blocking(move || {
        service.execute_controlled(request.request, credential, control)
    })
    .await;
    cancel_on_drop.disarm();
    match execution {
        Ok(Ok(response)) => remote_protocol_response(response.into()),
        Ok(Err(_)) | Err(_) => invalid_remote_request_response(),
    }
}

async fn enrollment_route<Port>(
    State(state): State<RemoteProtocolRouterStateV1<Port>>,
    admission: RemoteEnrollmentPreBodyAdmissionV1,
    payload: Result<Json<RemoteHttpRequestV1<EnrollmentRequestV1>>, JsonRejection>,
) -> Response
where
    Port: RemoteEnrollmentProtocolPortV1 + Send + Sync + 'static,
{
    let Json(request) = match payload {
        Ok(payload) => payload,
        Err(_) => return invalid_remote_request_response(),
    };
    if <EnrollmentRequestV1 as RemoteSessionBoundProtocolBodyV1>::bind_authenticated_session(
        &admission.session,
        &request.request,
    )
    .is_err()
    {
        return concealed_authentication_response();
    }
    match state.service.execute_enrollment(
        request.request,
        admission.grant_credential,
        admission.enrollment_credential,
    ) {
        Ok(response) => remote_protocol_response(response.into()),
        Err(_) => invalid_remote_request_response(),
    }
}

fn authorization_header(
    headers: &HeaderMap,
) -> Result<RemoteAuthorizationHeader, RemoteHttpBoundaryError> {
    let authorization = headers
        .get(AUTHORIZATION)
        .ok_or(RemoteHttpBoundaryError::MissingOrInvalidAuthorization)?;
    RemoteAuthorizationHeader::from_owned_bytes(authorization.as_bytes().to_vec())
}

fn enrollment_credential(
    headers: &HeaderMap,
) -> Result<OpaqueRemoteCredential, RemoteHttpBoundaryError> {
    let replacement = headers
        .get(REMOTE_ENROLLMENT_CREDENTIAL_HEADER)
        .ok_or(RemoteHttpBoundaryError::MissingOrInvalidAuthorization)?;
    OpaqueRemoteCredential::new(replacement.as_bytes().to_vec().into_boxed_slice())
        .map_err(|_| RemoteHttpBoundaryError::MissingOrInvalidAuthorization)
}

fn remote_protocol_response<T: Serialize>(response: RemoteHttpResponseV1<T>) -> Response {
    let status = match &response.response.result {
        Ok(_) => StatusCode::OK,
        Err(problem) => match problem.problem.kind() {
            ApplicationProblemKind::InvalidRequest => StatusCode::BAD_REQUEST,
            ApplicationProblemKind::NotFoundOrNotAuthorized => StatusCode::NOT_FOUND,
            ApplicationProblemKind::Conflict | ApplicationProblemKind::Stale => {
                StatusCode::CONFLICT
            }
            ApplicationProblemKind::Unsupported => StatusCode::UNPROCESSABLE_ENTITY,
            ApplicationProblemKind::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            ApplicationProblemKind::Saturated => StatusCode::TOO_MANY_REQUESTS,
            ApplicationProblemKind::Cancelled => StatusCode::REQUEST_TIMEOUT,
            ApplicationProblemKind::TimedOut => StatusCode::GATEWAY_TIMEOUT,
        },
    };
    (status, Json(response)).into_response()
}

fn concealed_authentication_response() -> Response {
    crate::application_problem_response(remote_protocol_problem(
        remote_result_contract(),
        concealed_request_id(),
        RemoteProtocolFailureV1::CallerAuthenticationFailed,
    ))
}

fn concealed_request_id() -> RequestId {
    RequestId::new("request.remote.unauthenticated")
        .expect("static concealed remote request id is canonical")
}

fn invalid_remote_request_response() -> Response {
    crate::application_problem_response(crate::http::invalid_request_problem(
        RequestId::new("request.remote.invalid").expect("static remote request id is canonical"),
        "remote.invalid_request",
        "The remote protocol request is malformed",
    ))
}

fn remote_result_contract() -> ResultContractRef {
    ResultContractRef::new(
        SchemaId::new("schema.tracedecay.remote.protocol-result.v1")
            .expect("static remote result schema is canonical"),
        1,
    )
    .expect("static remote result contract is canonical")
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
