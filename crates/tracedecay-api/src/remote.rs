//! Thin HTTP boundary for the authenticated remote Brain protocol.
//!
//! HTTP carries versioned application payloads and an opaque authorization
//! header. Authority authentication remains the rustls/transport owner's
//! responsibility; this adapter does not accept trust flags, URLs, database
//! locations, or storage bytes.

use std::fmt;
use std::hint::black_box;
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_application::remote::auth::OpaqueRemoteCredential;
use tracedecay_application::remote::protocol::{
    EnrollmentRequestV1, REMOTE_PROTOCOL_VERSION_V1, RemoteEnrollmentProtocolPortV1,
    RemoteProtocolFailureV1, RemoteProtocolPortV1, RemoteProtocolRequestV1,
    RemoteProtocolResponseV1, RemoteProtocolServiceV1, remote_protocol_problem,
};
use tracedecay_application::remote::recovery::{
    BackupOperationStateV1, BackupRequestV1, PromotionCasReceiptV1, PromotionConfirmationV1,
    StagedRestoreConfirmationV1, StagedRestoreProgressV1,
};
use tracedecay_application::remote::replay::{RemoteReplayOutcomeV1, RemoteReplayRequestV1};
use tracedecay_application::{ApplicationProblemKind, RequestId, ResultContractRef};
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

/// Current authorization plus a newly generated credential for enrollment or
/// rotation. Neither value can enter a serializable request body.
pub struct RemoteCredentialPairHeaders {
    current: RemoteAuthorizationHeader,
    replacement: OpaqueRemoteCredential,
}

impl RemoteCredentialPairHeaders {
    pub fn from_owned_bytes(
        current_authorization: Vec<u8>,
        replacement: Vec<u8>,
    ) -> Result<Self, RemoteHttpBoundaryError> {
        Ok(Self {
            current: RemoteAuthorizationHeader::from_owned_bytes(current_authorization)?,
            replacement: OpaqueRemoteCredential::new(replacement.into_boxed_slice())
                .map_err(|_| RemoteHttpBoundaryError::MissingOrInvalidAuthorization)?,
        })
    }

    pub fn into_credentials(self) -> (OpaqueRemoteCredential, OpaqueRemoteCredential) {
        (self.current.into_credential(), self.replacement)
    }
}

impl fmt::Debug for RemoteCredentialPairHeaders {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteCredentialPairHeaders([REDACTED])")
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
    #[error("remote protocol version is unsupported")]
    UnsupportedProtocolVersion,
    #[error("remote request metadata is invalid")]
    InvalidRequest,
}

/// Wire request body. Secret material is supplied separately through
/// `RemoteAuthorizationHeader`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteHttpRequestV1<T> {
    pub request: RemoteProtocolRequestV1<T>,
}

impl<T> RemoteHttpRequestV1<T> {
    pub fn validate(&self) -> Result<(), RemoteHttpBoundaryError> {
        if self.request.protocol_version != REMOTE_PROTOCOL_VERSION_V1 {
            return Err(RemoteHttpBoundaryError::UnsupportedProtocolVersion);
        }
        self.request
            .validate_metadata()
            .map_err(|_| RemoteHttpBoundaryError::InvalidRequest)
    }

    /// Join the public body and secret header only after both have passed the
    /// HTTP boundary. The resulting admission object has no serialization or
    /// debug implementation.
    pub fn admit(
        self,
        authorization: RemoteAuthorizationHeader,
    ) -> Result<RemoteHttpAdmissionV1<T>, RemoteHttpBoundaryError> {
        self.validate()?;
        Ok(RemoteHttpAdmissionV1 {
            request: self.request,
            credential: authorization.into_credential(),
        })
    }

    pub fn admit_with_replacement(
        self,
        credentials: RemoteCredentialPairHeaders,
    ) -> Result<RemoteHttpCredentialRotationAdmissionV1<T>, RemoteHttpBoundaryError> {
        self.validate()?;
        let (current, replacement) = credentials.into_credentials();
        Ok(RemoteHttpCredentialRotationAdmissionV1 {
            request: self.request,
            current,
            replacement,
        })
    }
}

/// Non-serializable input handed to the application owner.
pub struct RemoteHttpAdmissionV1<T> {
    pub request: RemoteProtocolRequestV1<T>,
    pub credential: OpaqueRemoteCredential,
}

/// Non-serializable enrollment/rotation input with both opaque credentials.
pub struct RemoteHttpCredentialRotationAdmissionV1<T> {
    pub request: RemoteProtocolRequestV1<T>,
    pub current: OpaqueRemoteCredential,
    pub replacement: OpaqueRemoteCredential,
}

/// HTTP response is a transparent presentation of the versioned canonical
/// application response.
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

pub struct RemoteHttpProtocolTransportV1<Port> {
    service: RemoteProtocolServiceV1<Port>,
}

impl<Port> RemoteHttpProtocolTransportV1<Port> {
    pub const fn new(port: Port) -> Self {
        Self {
            service: RemoteProtocolServiceV1::new(port),
        }
    }

    pub fn execute<Request>(
        &self,
        request: RemoteHttpRequestV1<Request>,
        authorization: RemoteAuthorizationHeader,
    ) -> Result<RemoteHttpResponseV1<Port::Output>, RemoteHttpBoundaryError>
    where
        Port: RemoteProtocolPortV1<Request>,
    {
        let admission = request.admit(authorization)?;
        let response = self
            .service
            .execute(admission.request, admission.credential)
            .map_err(|_| RemoteHttpBoundaryError::InvalidRequest)?;
        Ok(response.into())
    }

    pub fn execute_enrollment(
        &self,
        request: RemoteHttpRequestV1<EnrollmentRequestV1>,
        credentials: RemoteCredentialPairHeaders,
    ) -> Result<
        RemoteHttpResponseV1<tracedecay_domain::EnrollmentCredentialRecordV1>,
        RemoteHttpBoundaryError,
    >
    where
        Port: RemoteEnrollmentProtocolPortV1,
    {
        let admission = request.admit_with_replacement(credentials)?;
        Ok(self
            .service
            .port()
            .execute_enrollment(admission.request, admission.current, admission.replacement)
            .into())
    }
}

struct RemoteProtocolRouterStateV1<Port> {
    transport: Arc<RemoteHttpProtocolTransportV1<Port>>,
}

impl<Port> Clone for RemoteProtocolRouterStateV1<Port> {
    fn clone(&self) -> Self {
        Self {
            transport: Arc::clone(&self.transport),
        }
    }
}

/// Build the complete Remote Brain HTTP seam. The query body and result remain
/// canonical caller-selected types; this router never accepts an untyped JSON
/// operation fallback.
pub fn remote_protocol_router<Port, Query>(port: Port) -> Router
where
    Port: RemoteEnrollmentProtocolPortV1
        + RemoteProtocolPortV1<RemoteReplayRequestV1, Output = RemoteReplayOutcomeV1>
        + RemoteProtocolPortV1<Query>
        + RemoteProtocolPortV1<BackupRequestV1, Output = BackupOperationStateV1>
        + RemoteProtocolPortV1<StagedRestoreConfirmationV1, Output = StagedRestoreProgressV1>
        + RemoteProtocolPortV1<PromotionConfirmationV1, Output = PromotionCasReceiptV1>
        + Send
        + Sync
        + 'static,
    Query: DeserializeOwned + Send + 'static,
    <Port as RemoteProtocolPortV1<Query>>::Output: Serialize,
{
    let state = RemoteProtocolRouterStateV1 {
        transport: Arc::new(RemoteHttpProtocolTransportV1::new(port)),
    };
    Router::new()
        .route("/enrollment", post(enrollment_route::<Port>))
        .route(
            "/replay",
            post(protocol_route::<Port, RemoteReplayRequestV1>),
        )
        .route("/query", post(protocol_route::<Port, Query>))
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
    headers: HeaderMap,
    payload: Result<Json<RemoteHttpRequestV1<Request>>, JsonRejection>,
) -> Response
where
    Port: RemoteProtocolPortV1<Request> + Send + Sync + 'static,
    Request: DeserializeOwned + Send + 'static,
    Port::Output: Serialize,
{
    let Json(request) = match payload {
        Ok(payload) => payload,
        Err(_) => return invalid_remote_request_response(),
    };
    let request_id = request.request.request_id.clone();
    let authorization = match authorization_header(&headers) {
        Ok(authorization) => authorization,
        Err(_) => return concealed_authentication_response(request_id),
    };
    match state.transport.execute(request, authorization) {
        Ok(response) => remote_protocol_response(response),
        Err(_) => invalid_remote_request_response(),
    }
}

async fn enrollment_route<Port>(
    State(state): State<RemoteProtocolRouterStateV1<Port>>,
    headers: HeaderMap,
    payload: Result<Json<RemoteHttpRequestV1<EnrollmentRequestV1>>, JsonRejection>,
) -> Response
where
    Port: RemoteEnrollmentProtocolPortV1 + Send + Sync + 'static,
{
    let Json(request) = match payload {
        Ok(payload) => payload,
        Err(_) => return invalid_remote_request_response(),
    };
    let request_id = request.request.request_id.clone();
    let credentials = match enrollment_credentials(&headers) {
        Ok(credentials) => credentials,
        Err(_) => return concealed_authentication_response(request_id),
    };
    match state.transport.execute_enrollment(request, credentials) {
        Ok(response) => remote_protocol_response(response),
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

fn enrollment_credentials(
    headers: &HeaderMap,
) -> Result<RemoteCredentialPairHeaders, RemoteHttpBoundaryError> {
    let current = headers
        .get(AUTHORIZATION)
        .ok_or(RemoteHttpBoundaryError::MissingOrInvalidAuthorization)?;
    let replacement = headers
        .get(REMOTE_ENROLLMENT_CREDENTIAL_HEADER)
        .ok_or(RemoteHttpBoundaryError::MissingOrInvalidAuthorization)?;
    RemoteCredentialPairHeaders::from_owned_bytes(
        current.as_bytes().to_vec(),
        replacement.as_bytes().to_vec(),
    )
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

fn concealed_authentication_response(request_id: RequestId) -> Response {
    crate::application_problem_response(remote_protocol_problem(
        remote_result_contract(),
        request_id,
        RemoteProtocolFailureV1::CallerAuthenticationFailed,
    ))
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
mod tests {
    use std::future::Future;
    use std::pin::pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll, Wake, Waker};

    use super::*;
    use tracedecay_application::remote::protocol::{
        RemoteProtocolFailureV1, RemoteProtocolPortV1, remote_protocol_problem,
    };
    use tracedecay_application::{
        ApplicationEnvelope, ApplicationResult, AuthorityReceipt, CapabilityGrantId, Deadline,
        DisclosureClass, EvidenceCoverage, EvidenceDomain, EvidencePacket, OperationReceipt,
        PageState, PolicyDecisionRef, RequestId, ResolvedScope, ResultContractRef,
        RetrievalEvidence, TemporalState,
    };
    use tracedecay_domain::{
        AuthorityEpoch, BrainId, BrainNodeId, ComponentVersion, CurrentRemoteAuthorityStateV1,
        CurrentRemoteAuthorityV1, EnrollmentCredentialRecordV1, EntityVersionId, ManifestDigest,
        ProjectId, ProjectionGenerationId, RefId, RemoteAuthorityUnavailableReasonV1,
        RemoteWriterFenceV1, RepositoryId, ShardId, UtcMicros, WorktreeId,
    };
    use tracedecay_tool_catalog::{SchemaId, SortContractId};

    struct CountingProtocolPort(Arc<AtomicUsize>);

    impl RemoteProtocolPortV1<()> for CountingProtocolPort {
        type Output = ();

        fn execute(
            &self,
            request: RemoteProtocolRequestV1<()>,
            _credential: OpaqueRemoteCredential,
        ) -> RemoteProtocolResponseV1<Self::Output> {
            self.0.fetch_add(1, Ordering::SeqCst);
            let request_id = request.request_id;
            RemoteProtocolResponseV1::new(
                request_id.clone(),
                CurrentRemoteAuthorityStateV1::Unavailable {
                    reason: RemoteAuthorityUnavailableReasonV1::AuthorityUnreachable,
                    observed_at: UtcMicros(20),
                },
                Err(remote_protocol_problem(
                    ResultContractRef::new(SchemaId::new("remote.result").unwrap(), 1).unwrap(),
                    request_id,
                    RemoteProtocolFailureV1::AuthorityUnavailable,
                )),
            )
            .unwrap()
        }
    }

    #[test]
    fn authorization_header_is_always_redacted() {
        let header = RemoteAuthorizationHeader::from_owned_bytes(
            b"Bearer 0123456789abcdef0123456789abcdef".to_vec(),
        )
        .unwrap();
        assert_eq!(
            format!("{header:?}"),
            "RemoteAuthorizationHeader([REDACTED])"
        );
    }

    #[test]
    fn malformed_authorization_fails_closed() {
        assert_eq!(
            RemoteAuthorizationHeader::from_owned_bytes(
                b"Basic 0123456789abcdef0123456789abcdef".to_vec()
            )
            .unwrap_err(),
            RemoteHttpBoundaryError::MissingOrInvalidAuthorization
        );
        assert_eq!(
            RemoteAuthorizationHeader::from_owned_bytes(b"Bearer short".to_vec()).unwrap_err(),
            RemoteHttpBoundaryError::MissingOrInvalidAuthorization
        );
    }

    #[test]
    fn credential_pair_debug_never_exposes_either_secret() {
        let headers = RemoteCredentialPairHeaders::from_owned_bytes(
            b"Bearer 0123456789abcdef0123456789abcdef".to_vec(),
            b"fedcba9876543210fedcba9876543210".to_vec(),
        )
        .unwrap();
        assert_eq!(
            format!("{headers:?}"),
            "RemoteCredentialPairHeaders([REDACTED])"
        );
    }

    #[test]
    fn public_http_payload_never_contains_the_credential() {
        let request: RemoteHttpRequestV1<()> = serde_json::from_value(serde_json::json!({
            "request": {
                "protocol_version": 1,
                "request_id": "request.remote",
                "brain_id": "brain.remote",
                "caller_node_id": "node.remote",
                "enrollment_revision": 1,
                "expected_authority": null,
                "sent_at": 10,
                "body": null
            }
        }))
        .unwrap();
        let json = serde_json::to_string(&request).unwrap();
        assert!(!json.contains("credential"));
        assert!(!json.contains("authorization"));
    }

    #[test]
    fn concrete_http_transport_admits_and_delegates_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let transport =
            RemoteHttpProtocolTransportV1::new(CountingProtocolPort(Arc::clone(&calls)));
        let request = RemoteHttpRequestV1 {
            request: RemoteProtocolRequestV1::new(
                RequestId::new("request.remote.transport").unwrap(),
                BrainId::new("brain.remote").unwrap(),
                BrainNodeId::new("node.remote").unwrap(),
                1,
                None,
                UtcMicros(10),
                (),
            )
            .unwrap(),
        };
        let authorization = RemoteAuthorizationHeader::from_owned_bytes(
            b"Bearer 0123456789abcdef0123456789abcdef".to_vec(),
        )
        .unwrap();

        let response = transport.execute(request, authorization).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            response.response.request_id.as_str(),
            "request.remote.transport"
        );
    }

    #[derive(Clone, Copy)]
    enum RouteOutcome {
        Valid,
        Denied,
        Stale,
        Unavailable,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct TestQuery {
        term: String,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct TestQueryResult {
        matched: bool,
    }

    struct RoutePort {
        outcome: RouteOutcome,
    }

    impl RemoteEnrollmentProtocolPortV1 for RoutePort {
        fn execute_enrollment(
            &self,
            request: RemoteProtocolRequestV1<EnrollmentRequestV1>,
            _grant_credential: OpaqueRemoteCredential,
            _enrollment_credential: OpaqueRemoteCredential,
        ) -> RemoteProtocolResponseV1<EnrollmentCredentialRecordV1> {
            problem_route_response(request.request_id, self.outcome)
        }
    }

    macro_rules! route_port {
        ($request:ty, $output:ty) => {
            impl RemoteProtocolPortV1<$request> for RoutePort {
                type Output = $output;

                fn execute(
                    &self,
                    request: RemoteProtocolRequestV1<$request>,
                    _credential: OpaqueRemoteCredential,
                ) -> RemoteProtocolResponseV1<Self::Output> {
                    problem_route_response(request.request_id, self.outcome)
                }
            }
        };
    }

    route_port!(RemoteReplayRequestV1, RemoteReplayOutcomeV1);
    route_port!(BackupRequestV1, BackupOperationStateV1);
    route_port!(StagedRestoreConfirmationV1, StagedRestoreProgressV1);
    route_port!(PromotionConfirmationV1, PromotionCasReceiptV1);

    impl RemoteProtocolPortV1<TestQuery> for RoutePort {
        type Output = TestQueryResult;

        fn execute(
            &self,
            request: RemoteProtocolRequestV1<TestQuery>,
            _credential: OpaqueRemoteCredential,
        ) -> RemoteProtocolResponseV1<Self::Output> {
            let request_id = request.request_id;
            let result = match self.outcome {
                RouteOutcome::Valid => Ok(success_envelope(
                    request_id.clone(),
                    TestQueryResult { matched: true },
                )),
                outcome => problem_result(request_id.clone(), outcome),
            };
            RemoteProtocolResponseV1::new(request_id, available_authority(), result).unwrap()
        }
    }

    fn problem_route_response<T>(
        request_id: RequestId,
        outcome: RouteOutcome,
    ) -> RemoteProtocolResponseV1<T> {
        RemoteProtocolResponseV1::new(
            request_id.clone(),
            match outcome {
                RouteOutcome::Unavailable => CurrentRemoteAuthorityStateV1::Unavailable {
                    reason: RemoteAuthorityUnavailableReasonV1::AuthorityUnreachable,
                    observed_at: UtcMicros(20),
                },
                RouteOutcome::Valid | RouteOutcome::Denied | RouteOutcome::Stale => {
                    available_authority()
                }
            },
            problem_result(request_id, outcome),
        )
        .unwrap()
    }

    fn problem_result<T>(request_id: RequestId, outcome: RouteOutcome) -> ApplicationResult<T> {
        let failure = match outcome {
            RouteOutcome::Valid | RouteOutcome::Unavailable => {
                RemoteProtocolFailureV1::AuthorityUnavailable
            }
            RouteOutcome::Denied => RemoteProtocolFailureV1::CallerAuthenticationFailed,
            RouteOutcome::Stale => RemoteProtocolFailureV1::StaleAuthorityFence,
        };
        Err(remote_protocol_problem(
            ResultContractRef::new(SchemaId::new("remote.result").unwrap(), 1).unwrap(),
            request_id,
            failure,
        ))
    }

    fn success_envelope<T>(request_id: RequestId, payload: T) -> ApplicationEnvelope<T> {
        let scope = ResolvedScope::new(
            ProjectId::new("project.remote").unwrap(),
            RepositoryId::new("repository.remote").unwrap(),
            WorktreeId::new("worktree.remote").unwrap(),
            Some(RefId::new("refs/heads/main").unwrap()),
        )
        .unwrap();
        let digest = ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap();
        let authority = AuthorityReceipt {
            grant_id: CapabilityGrantId::new("grant.remote").unwrap(),
            grant_revision: 1,
            grant_digest: digest.clone(),
            authorized_scope_digest: scope.scope_digest.clone(),
            disclosure: DisclosureClass::Evidence,
            policy: PolicyDecisionRef::new(
                "policy.remote",
                1,
                digest,
                ComponentVersion::new("policy.remote.v1").unwrap(),
            )
            .unwrap(),
            revalidated_at: UtcMicros(2),
        };
        let evidence = RetrievalEvidence {
            payload: Some(payload),
            temporal: TemporalState::current(UtcMicros(2)),
            evidence_authorities: Vec::new(),
            coverage: EvidenceCoverage::complete(vec![EvidenceDomain::Symbol], 1, 1, 1).unwrap(),
            omissions: Vec::new(),
            scores: Vec::new(),
            contributions: Vec::new(),
            page: PageState::first_page(SortContractId::new("sort.remote").unwrap(), 1, Some(1), 1)
                .unwrap(),
            finished_at: UtcMicros(3),
            budget: Default::default(),
            cancellation: None,
        };
        let execution = OperationReceipt::completed(
            UtcMicros(1),
            UtcMicros(3),
            Deadline::new(UtcMicros(10)).unwrap(),
            Default::default(),
        )
        .unwrap();
        ApplicationEnvelope::evidence(
            remote_result_contract(),
            request_id,
            scope,
            EvidencePacket::from_retrieval(evidence, authority, execution).unwrap(),
        )
    }

    fn available_authority() -> CurrentRemoteAuthorityStateV1 {
        CurrentRemoteAuthorityStateV1::Available(CurrentRemoteAuthorityV1 {
            fence: RemoteWriterFenceV1 {
                brain_id: BrainId::new("brain.remote").unwrap(),
                shard_id: ShardId::new("shard.remote").unwrap(),
                generation_id: ProjectionGenerationId::new("generation.remote").unwrap(),
                placement_revision: EntityVersionId::new("placement.remote").unwrap(),
                authority_epoch: AuthorityEpoch(1),
                authority_node_id: BrainNodeId::new("node.authority").unwrap(),
            },
            credential_revision: 1,
            observed_at: UtcMicros(20),
        })
    }

    fn query_request() -> RemoteHttpRequestV1<TestQuery> {
        RemoteHttpRequestV1 {
            request: RemoteProtocolRequestV1::new(
                RequestId::new("request.remote.query").unwrap(),
                BrainId::new("brain.remote").unwrap(),
                BrainNodeId::new("node.remote").unwrap(),
                1,
                None,
                UtcMicros(10),
                TestQuery {
                    term: "needle".into(),
                },
            )
            .unwrap(),
        }
    }

    fn route_status(outcome: RouteOutcome, authorized: bool) -> StatusCode {
        let state = RemoteProtocolRouterStateV1 {
            transport: Arc::new(RemoteHttpProtocolTransportV1::new(RoutePort { outcome })),
        };
        let mut headers = HeaderMap::new();
        if authorized {
            headers.insert(
                AUTHORIZATION,
                "Bearer 0123456789abcdef0123456789abcdef".parse().unwrap(),
            );
        }
        block_on(protocol_route::<RoutePort, TestQuery>(
            State(state),
            headers,
            Ok(Json(query_request())),
        ))
        .status()
    }

    #[test]
    fn router_maps_valid_denied_stale_and_unavailable_results() {
        assert_eq!(route_status(RouteOutcome::Valid, true), StatusCode::OK);
        assert_eq!(
            route_status(RouteOutcome::Denied, true),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            route_status(RouteOutcome::Stale, true),
            StatusCode::CONFLICT
        );
        assert_eq!(
            route_status(RouteOutcome::Unavailable, true),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            route_status(RouteOutcome::Valid, false),
            StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn canonical_router_is_typed_and_malformed_requests_are_bad_requests() {
        drop(remote_protocol_router::<RoutePort, TestQuery>(RoutePort {
            outcome: RouteOutcome::Valid,
        }));
        assert_eq!(
            invalid_remote_request_response().status(),
            StatusCode::BAD_REQUEST
        );
    }

    struct NoopWake;

    impl Wake for NoopWake {
        fn wake(self: Arc<Self>) {}
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = Waker::from(Arc::new(NoopWake));
        let mut context = Context::from_waker(&waker);
        let mut future = pin!(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(output) => return output,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }
}
