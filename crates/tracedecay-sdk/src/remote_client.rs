//! Enrolled HTTPS client for the mounted Remote Brain protocol.
//!
//! The client exposes only the daemon's authenticated `/remote/*` routes. It
//! validates the canonical request and response envelopes and never accepts an
//! unwrapped response, an arbitrary route, or a credential in a JSON body.

use std::fmt;
use std::time::Duration;

use reqwest::blocking::Client as HttpClient;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use serde::{Serialize, de::DeserializeOwned};
use tracedecay_api::remote::REMOTE_ENROLLMENT_CREDENTIAL_HEADER;
use tracedecay_application::remote::capture::RemoteCaptureReceiptV1;
use tracedecay_application::remote::capture_protocol::RemoteCaptureRequestV1;
use tracedecay_application::remote::protocol::{
    EnrollmentRequestV1, REMOTE_PROTOCOL_VERSION_V1, RemoteProtocolBodyV1, RemoteProtocolRequestV1,
    RemoteProtocolResponseV1, remote_capture_result_contract_v1,
    remote_enrollment_result_contract_v1, remote_replay_result_contract_v1,
};
use tracedecay_application::remote::query::{
    RemoteQueryRequestV1, RemoteQueryResultV1, remote_exact_observation_query_result_contract_v1,
};
use tracedecay_application::remote::recovery::{
    BackupOperationStateV1, BackupRequestV1, PromotionCasReceiptV1, PromotionConfirmationV1,
    StagedRestoreConfirmationV1, StagedRestoreProgressV1, remote_backup_result_contract_v1,
    remote_promotion_result_contract_v1, remote_restore_result_contract_v1,
};
use tracedecay_application::remote::replay::{RemoteReplayOutcomeV1, RemoteReplayRequestV1};
use tracedecay_application::remote::transfer::{
    RemoteFrameTransferReceiptV1, RemoteFrameTransferRequestV1,
    remote_frame_transfer_result_contract_v1,
};
use tracedecay_application::{
    ApplicationEnvelope, ApplicationOutcome, ApplicationProblemKind, ApplicationResult,
    EffectResult, OperationTermination, RequestId, ResultContractRef,
};
use tracedecay_domain::{
    CurrentRemoteAuthorityStateV1, EnrollmentCredentialRecordV1, validate_remote_secret_length,
};

pub struct EnrolledRemoteClient {
    http: HttpClient,
    endpoint: reqwest::Url,
    authorization: HeaderValue,
}

impl fmt::Debug for EnrolledRemoteClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnrolledRemoteClient")
            .field("endpoint", &self.endpoint)
            .field("authorization", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub enum RemoteClientError {
    Configuration(String),
    Transport(String),
    Protocol(String),
}

impl fmt::Display for RemoteClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(message) => {
                write!(
                    formatter,
                    "Remote Brain endpoint configuration is invalid: {message}"
                )
            }
            Self::Transport(message) => {
                write!(formatter, "Remote Brain transport failed: {message}")
            }
            Self::Protocol(message) => {
                write!(
                    formatter,
                    "Remote Brain protocol response was invalid: {message}"
                )
            }
        }
    }
}

impl std::error::Error for RemoteClientError {}

impl EnrolledRemoteClient {
    pub fn new(
        endpoint: impl AsRef<str>,
        credential: impl AsRef<[u8]>,
        timeout: Duration,
    ) -> Result<Self, RemoteClientError> {
        Self::build(endpoint, credential, timeout, None, false)
    }

    /// Builds a client with one explicit additional HTTPS trust root.
    pub fn new_with_root_certificate(
        endpoint: impl AsRef<str>,
        credential: impl AsRef<[u8]>,
        timeout: Duration,
        root_certificate_pem: impl AsRef<[u8]>,
    ) -> Result<Self, RemoteClientError> {
        Self::build(
            endpoint,
            credential,
            timeout,
            Some(root_certificate_pem.as_ref()),
            false,
        )
    }

    /// Targets the local daemon's application listener, which nests the same
    /// Remote Brain router at `/remote` as the external TLS listener. Same
    /// operations, envelopes, credential header, and response validation;
    /// plaintext HTTP is admitted for loopback hosts only.
    pub fn new_local_daemon(
        endpoint: impl AsRef<str>,
        credential: impl AsRef<[u8]>,
        timeout: Duration,
    ) -> Result<Self, RemoteClientError> {
        Self::build(endpoint, credential, timeout, None, true)
    }

    fn build(
        endpoint: impl AsRef<str>,
        credential: impl AsRef<[u8]>,
        timeout: Duration,
        root_certificate_pem: Option<&[u8]>,
        allow_loopback_http: bool,
    ) -> Result<Self, RemoteClientError> {
        let endpoint = reqwest::Url::parse(endpoint.as_ref())
            .map_err(|error| RemoteClientError::Configuration(error.to_string()))?;
        let scheme_admitted = match endpoint.scheme() {
            "https" => true,
            "http" => allow_loopback_http && host_is_loopback(&endpoint),
            _ => false,
        };
        if !scheme_admitted
            || endpoint.host_str().is_none()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || endpoint.username() != ""
            || endpoint.password().is_some()
        {
            return Err(RemoteClientError::Configuration(if allow_loopback_http {
                "local daemon Remote Brain endpoint must be a credential-free loopback HTTP or HTTPS URL"
                    .to_owned()
            } else {
                "Remote Brain endpoint must be a credential-free HTTPS URL".to_owned()
            }));
        }
        let credential = credential.as_ref();
        if validate_remote_secret_length(credential).is_err() {
            return Err(RemoteClientError::Configuration(
                "Remote Brain credential length is invalid".to_owned(),
            ));
        }
        let authorization =
            HeaderValue::from_bytes([b"Bearer ".as_slice(), credential].concat().as_slice())
                .map_err(|error| RemoteClientError::Configuration(error.to_string()))?;
        let mut builder = HttpClient::builder().timeout(timeout);
        if endpoint.scheme() == "http" {
            // reqwest's system-proxy default would forward the plaintext
            // request — Bearer credential included — to an HTTP_PROXY/
            // ALL_PROXY host; loopback traffic never uses a proxy.
            builder = builder.no_proxy();
        }
        if let Some(pem) = root_certificate_pem {
            let mut certificates = reqwest::Certificate::from_pem_bundle(pem)
                .map_err(|error| RemoteClientError::Configuration(error.to_string()))?;
            if certificates.len() != 1 {
                return Err(RemoteClientError::Configuration(
                    "explicit trust root must contain exactly one PEM certificate".to_owned(),
                ));
            }
            let certificate = certificates.pop().ok_or_else(|| {
                RemoteClientError::Configuration(
                    "explicit trust root did not contain a PEM certificate".to_owned(),
                )
            })?;
            builder = builder.add_root_certificate(certificate);
        }
        let http = builder
            .build()
            .map_err(|error| RemoteClientError::Transport(error.to_string()))?;
        Ok(Self {
            http,
            endpoint,
            authorization,
        })
    }

    /// Enrolls with this client's one-time grant credential. The new
    /// enrollment credential is sent only through the canonical header.
    pub fn enroll(
        &self,
        request: &RemoteProtocolRequestV1<EnrollmentRequestV1>,
        enrollment_credential: impl AsRef<[u8]>,
    ) -> Result<RemoteProtocolResponseV1<EnrollmentCredentialRecordV1>, RemoteClientError> {
        request
            .validate_initial_enrollment_metadata()
            .and_then(|()| request.body.validate_remote_protocol_body(request.sent_at))
            .map_err(protocol_error)?;
        let enrollment_credential = credential_header(enrollment_credential.as_ref())?;
        self.execute_mounted(
            "enrollment",
            request,
            Some((REMOTE_ENROLLMENT_CREDENTIAL_HEADER, enrollment_credential)),
            remote_enrollment_result_contract_v1(),
            RemoteSuccessKind::Effect,
            |record: &EnrollmentCredentialRecordV1| {
                record.validate().is_ok()
                    && record.enrollment_id == request.body.enrollment_id
                    && record.brain_id == request.body.brain_id
                    && record.node_id == request.body.node_id
                    && record.expires_at == request.body.expires_at
                    && record.capabilities == request.body.capabilities
                    && record.scope == request.body.scope
            },
        )
    }

    pub fn capture(
        &self,
        request: &RemoteProtocolRequestV1<RemoteCaptureRequestV1>,
    ) -> Result<RemoteProtocolResponseV1<RemoteCaptureReceiptV1>, RemoteClientError> {
        self.execute_authenticated(
            "capture",
            request,
            remote_capture_result_contract_v1(),
            RemoteSuccessKind::Effect,
            |receipt: &RemoteCaptureReceiptV1| receipt.validate_for(&request.body.sequence).is_ok(),
        )
    }

    pub fn replay(
        &self,
        request: &RemoteProtocolRequestV1<RemoteReplayRequestV1>,
    ) -> Result<RemoteProtocolResponseV1<RemoteReplayOutcomeV1>, RemoteClientError> {
        self.execute_authenticated(
            "replay",
            request,
            remote_replay_result_contract_v1(),
            RemoteSuccessKind::Effect,
            |outcome: &RemoteReplayOutcomeV1| replay_result_valid(outcome, &request.body),
        )
    }

    pub fn transfer_frame(
        &self,
        request: &RemoteProtocolRequestV1<RemoteFrameTransferRequestV1>,
    ) -> Result<RemoteProtocolResponseV1<RemoteFrameTransferReceiptV1>, RemoteClientError> {
        self.execute_authenticated(
            "frames/transfer",
            request,
            remote_frame_transfer_result_contract_v1().map_err(protocol_error)?,
            RemoteSuccessKind::Effect,
            |receipt: &RemoteFrameTransferReceiptV1| receipt.validate_for(&request.body).is_ok(),
        )
    }

    pub fn query(
        &self,
        request: &RemoteProtocolRequestV1<RemoteQueryRequestV1>,
    ) -> Result<RemoteProtocolResponseV1<RemoteQueryResultV1>, RemoteClientError> {
        self.execute_authenticated(
            "query",
            request,
            remote_exact_observation_query_result_contract_v1(),
            RemoteSuccessKind::Evidence,
            |result: &RemoteQueryResultV1| result.validate().is_ok(),
        )
    }

    pub fn backup(
        &self,
        request: &RemoteProtocolRequestV1<BackupRequestV1>,
    ) -> Result<RemoteProtocolResponseV1<BackupOperationStateV1>, RemoteClientError> {
        self.execute_authenticated(
            "backup",
            request,
            remote_backup_result_contract_v1().map_err(protocol_error)?,
            RemoteSuccessKind::Effect,
            |_| true,
        )
    }

    pub fn restore(
        &self,
        request: &RemoteProtocolRequestV1<StagedRestoreConfirmationV1>,
    ) -> Result<RemoteProtocolResponseV1<StagedRestoreProgressV1>, RemoteClientError> {
        self.execute_authenticated(
            "restore",
            request,
            remote_restore_result_contract_v1().map_err(protocol_error)?,
            RemoteSuccessKind::Effect,
            |_| true,
        )
    }

    pub fn failover(
        &self,
        request: &RemoteProtocolRequestV1<PromotionConfirmationV1>,
    ) -> Result<RemoteProtocolResponseV1<PromotionCasReceiptV1>, RemoteClientError> {
        self.execute_authenticated(
            "failover",
            request,
            remote_promotion_result_contract_v1().map_err(protocol_error)?,
            RemoteSuccessKind::Effect,
            |receipt: &PromotionCasReceiptV1| {
                receipt.preview_id == request.body.preview_id
                    && receipt.previous_epoch == request.body.expected_authority_epoch
                    && receipt.installed_epoch > receipt.previous_epoch
                    && receipt.installed_placement_revision > 0
                    && receipt.old_authority_fenced
            },
        )
    }

    fn execute_authenticated<Request, Output>(
        &self,
        route: &'static str,
        request: &RemoteProtocolRequestV1<Request>,
        result_contract: ResultContractRef,
        success_kind: RemoteSuccessKind,
        validate_payload: impl Fn(&Output) -> bool,
    ) -> Result<RemoteProtocolResponseV1<Output>, RemoteClientError>
    where
        Request: RemoteProtocolBodyV1 + Serialize,
        Output: Clone + DeserializeOwned,
    {
        request
            .validate_metadata()
            .and_then(|()| request.body.validate_remote_protocol_body(request.sent_at))
            .map_err(protocol_error)?;
        self.execute_mounted(
            route,
            request,
            None,
            result_contract,
            success_kind,
            validate_payload,
        )
    }

    fn execute_mounted<Request, Output>(
        &self,
        route: &'static str,
        request: &RemoteProtocolRequestV1<Request>,
        additional_credential: Option<(&'static str, HeaderValue)>,
        result_contract: ResultContractRef,
        success_kind: RemoteSuccessKind,
        validate_payload: impl Fn(&Output) -> bool,
    ) -> Result<RemoteProtocolResponseV1<Output>, RemoteClientError>
    where
        Request: Serialize,
        Output: Clone + DeserializeOwned,
    {
        let url = self
            .endpoint
            .join(route)
            .map_err(|error| RemoteClientError::Configuration(error.to_string()))?;
        let mut builder = self
            .http
            .post(url)
            .header(AUTHORIZATION, self.authorization.clone())
            .header(CONTENT_TYPE, "application/json");
        if let Some((name, credential)) = additional_credential {
            builder = builder.header(name, credential);
        }
        let response = builder
            .json(&serde_json::json!({ "request": request }))
            .send()
            .map_err(|error| RemoteClientError::Transport(error.to_string()))?;
        decode_typed_response(
            response,
            &request.request_id,
            &result_contract,
            success_kind,
            validate_payload,
        )
    }
}

/// Whether the endpoint host is a loopback address; any other host requires
/// HTTPS.
fn host_is_loopback(endpoint: &reqwest::Url) -> bool {
    let Some(host) = endpoint.host_str() else {
        return false;
    };
    let address = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(address) = address.parse::<std::net::IpAddr>() {
        return address.is_loopback();
    }
    host.eq_ignore_ascii_case("localhost")
}

fn credential_header(credential: &[u8]) -> Result<HeaderValue, RemoteClientError> {
    if validate_remote_secret_length(credential).is_err() {
        return Err(RemoteClientError::Configuration(
            "Remote Brain credential length is invalid".to_owned(),
        ));
    }
    HeaderValue::from_bytes(credential)
        .map_err(|error| RemoteClientError::Configuration(error.to_string()))
}

fn protocol_error(error: impl fmt::Display) -> RemoteClientError {
    RemoteClientError::Protocol(error.to_string())
}

#[derive(Clone, Copy)]
enum RemoteSuccessKind {
    Evidence,
    Effect,
}

fn decode_typed_response<T: Clone + DeserializeOwned>(
    response: reqwest::blocking::Response,
    expected_request_id: &RequestId,
    expected_contract: &ResultContractRef,
    success_kind: RemoteSuccessKind,
    validate_payload: impl Fn(&T) -> bool,
) -> Result<RemoteProtocolResponseV1<T>, RemoteClientError> {
    let status = response.status();
    let value = response
        .json::<serde_json::Value>()
        .map_err(protocol_error)?;
    let payload = canonical_response_payload(value)?;
    let serde_json::Value::Object(mut fields) = payload else {
        return Err(RemoteClientError::Protocol(
            "remote response payload was not an object".to_owned(),
        ));
    };
    if fields.len() != 4 {
        return Err(RemoteClientError::Protocol(
            "remote response fields did not match the canonical envelope".to_owned(),
        ));
    }
    let protocol_version: u16 = take_response_field(&mut fields, "protocol_version")?;
    let request_id: RequestId = take_response_field(&mut fields, "request_id")?;
    let authority: CurrentRemoteAuthorityStateV1 = take_response_field(&mut fields, "authority")?;
    let result: ApplicationResult<T> = take_response_field(&mut fields, "result")?;
    if protocol_version != REMOTE_PROTOCOL_VERSION_V1 || &request_id != expected_request_id {
        return Err(RemoteClientError::Protocol(
            "response version or request identity did not match the request".to_owned(),
        ));
    }
    match &result {
        Ok(envelope)
            if &envelope.request_id == expected_request_id
                && &envelope.contract == expected_contract
                && canonical_success_valid(envelope, success_kind, &validate_payload)
                && status == reqwest::StatusCode::OK => {}
        Err(problem)
            if &problem.request_id == expected_request_id
                && &problem.contract == expected_contract
                && status_matches_problem(status, problem.problem.kind()) => {}
        _ => {
            return Err(RemoteClientError::Protocol(
                "response status, result identity, or result contract was invalid".to_owned(),
            ));
        }
    }
    RemoteProtocolResponseV1::new(request_id, authority, result).map_err(protocol_error)
}

fn canonical_success_valid<T: Clone>(
    envelope: &ApplicationEnvelope<T>,
    expected_kind: RemoteSuccessKind,
    validate_payload: &impl Fn(&T) -> bool,
) -> bool {
    if envelope.scope.validate().is_err() {
        return false;
    }
    match (&envelope.outcome, expected_kind) {
        (ApplicationOutcome::Evidence(packet), RemoteSuccessKind::Evidence) => {
            packet.authority.validate_for(&envelope.scope).is_ok()
                && packet.coverage.validate().is_ok()
                && packet.execution.validate().is_ok()
                && packet.execution.termination == OperationTermination::Completed
                && packet.payload.as_ref().is_some_and(validate_payload)
        }
        (ApplicationOutcome::Effect(effect), RemoteSuccessKind::Effect) => {
            effect.authority.validate_for(&envelope.scope).is_ok()
                && effect.receipt.request_id == envelope.request_id
                && effect.receipt.scope == envelope.scope
                && effect.payload.as_ref().is_some_and(validate_payload)
                && EffectResult::new(
                    effect.effect_id.clone(),
                    effect.effect_class,
                    effect.idempotency_key.clone(),
                    effect.authority.clone(),
                    effect.expected_state.clone(),
                    effect.execution.clone(),
                    effect.reconciliation,
                    effect.receipt.clone(),
                    effect.payload.clone(),
                )
                .is_ok()
        }
        _ => false,
    }
}

fn replay_result_valid(result: &RemoteReplayOutcomeV1, request: &RemoteReplayRequestV1) -> bool {
    let operation_receipt = match result {
        RemoteReplayOutcomeV1::Acknowledged {
            receipt,
            operation_receipt,
            ..
        } if operation_receipt.transaction.as_ref() == Some(receipt) => operation_receipt,
        RemoteReplayOutcomeV1::Rejected { operation_receipt }
        | RemoteReplayOutcomeV1::Quarantined { operation_receipt }
            if operation_receipt.transaction.is_none() =>
        {
            operation_receipt
        }
        _ => return false,
    };
    operation_receipt.event_id == request.event_id && operation_receipt.validate().is_ok()
}

fn canonical_response_payload(
    value: serde_json::Value,
) -> Result<serde_json::Value, RemoteClientError> {
    let serde_json::Value::Object(mut wrapper) = value else {
        return Err(RemoteClientError::Protocol(
            "remote HTTP response was not an object".to_owned(),
        ));
    };
    if wrapper.len() != 1 {
        return Err(RemoteClientError::Protocol(
            "remote HTTP response did not match the canonical wrapper".to_owned(),
        ));
    }
    wrapper.remove("response").ok_or_else(|| {
        RemoteClientError::Protocol(
            "remote HTTP response did not contain the canonical response field".to_owned(),
        )
    })
}

fn take_response_field<T: DeserializeOwned>(
    fields: &mut serde_json::Map<String, serde_json::Value>,
    name: &'static str,
) -> Result<T, RemoteClientError> {
    let value = fields
        .remove(name)
        .ok_or_else(|| RemoteClientError::Protocol(format!("remote response omitted {name}")))?;
    serde_json::from_value(value).map_err(protocol_error)
}

fn status_matches_problem(status: reqwest::StatusCode, kind: ApplicationProblemKind) -> bool {
    let expected = match kind {
        ApplicationProblemKind::InvalidRequest => reqwest::StatusCode::BAD_REQUEST,
        ApplicationProblemKind::NotFoundOrNotAuthorized => reqwest::StatusCode::NOT_FOUND,
        ApplicationProblemKind::Conflict
        | ApplicationProblemKind::PartialEffect
        | ApplicationProblemKind::Stale => reqwest::StatusCode::CONFLICT,
        ApplicationProblemKind::Unsupported => reqwest::StatusCode::UNPROCESSABLE_ENTITY,
        ApplicationProblemKind::Unavailable | ApplicationProblemKind::ResetRequired => {
            reqwest::StatusCode::SERVICE_UNAVAILABLE
        }
        ApplicationProblemKind::ExecutionFailed => reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        ApplicationProblemKind::Saturated => reqwest::StatusCode::TOO_MANY_REQUESTS,
        ApplicationProblemKind::Cancelled => reqwest::StatusCode::REQUEST_TIMEOUT,
        ApplicationProblemKind::TimedOut => reqwest::StatusCode::GATEWAY_TIMEOUT,
    };
    status == expected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enrolled_remote_client_requires_https() {
        let error = EnrolledRemoteClient::new(
            "http://remote.example",
            "credential",
            Duration::from_secs(1),
        )
        .expect_err("plaintext endpoint must fail");

        assert!(matches!(error, RemoteClientError::Configuration(_)));
    }

    #[test]
    fn local_daemon_target_admits_loopback_http_only() {
        let credential = "0123456789abcdef0123456789abcdef";
        for endpoint in [
            "http://127.0.0.1:39181/remote/",
            "http://[::1]:39181/remote/",
            "http://localhost:39181/remote/",
            "https://remote.example/remote/",
        ] {
            EnrolledRemoteClient::new_local_daemon(endpoint, credential, Duration::from_secs(1))
                .unwrap_or_else(|error| {
                    panic!("local daemon target must admit {endpoint}: {error}")
                });
        }

        for endpoint in [
            "http://remote.example/remote/",
            "http://10.0.0.7:39181/remote/",
            "ftp://127.0.0.1/remote/",
        ] {
            let error = EnrolledRemoteClient::new_local_daemon(
                endpoint,
                credential,
                Duration::from_secs(1),
            )
            .expect_err("plaintext beyond loopback must fail closed");
            assert!(matches!(error, RemoteClientError::Configuration(_)));
        }
    }

    #[test]
    fn enrolled_remote_target_still_refuses_loopback_http() {
        let error = EnrolledRemoteClient::new(
            "http://127.0.0.1:39181/remote/",
            "0123456789abcdef0123456789abcdef",
            Duration::from_secs(1),
        )
        .expect_err("the enrolled remote target must stay HTTPS-only");
        assert!(matches!(error, RemoteClientError::Configuration(_)));
    }

    #[test]
    fn enrolled_remote_client_rejects_url_credentials() {
        let error = EnrolledRemoteClient::new(
            "https://secret@remote.example",
            "credential",
            Duration::from_secs(1),
        )
        .expect_err("URL credentials must fail");

        assert!(matches!(error, RemoteClientError::Configuration(_)));
    }

    #[test]
    fn enrolled_remote_client_rejects_an_invalid_explicit_trust_root() {
        let error = EnrolledRemoteClient::new_with_root_certificate(
            "https://remote.example",
            "0123456789abcdef0123456789abcdef",
            Duration::from_secs(1),
            b"not a PEM certificate",
        )
        .expect_err("invalid trust root must fail");

        assert!(matches!(error, RemoteClientError::Configuration(_)));
    }

    #[test]
    fn enrolled_remote_client_debug_redacts_the_credential() {
        let credential = "0123456789abcdef0123456789abcdef";
        let client = EnrolledRemoteClient::new(
            "https://remote.example/remote/",
            credential,
            Duration::from_secs(1),
        )
        .unwrap();

        let rendered = format!("{client:?}");
        assert!(rendered.contains("[REDACTED]"));
        assert!(!rendered.contains(credential));
    }

    #[test]
    fn typed_remote_failures_require_the_canonical_http_status() {
        assert!(status_matches_problem(
            reqwest::StatusCode::CONFLICT,
            ApplicationProblemKind::Stale,
        ));
        assert!(status_matches_problem(
            reqwest::StatusCode::CONFLICT,
            ApplicationProblemKind::PartialEffect,
        ));
        assert!(status_matches_problem(
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            ApplicationProblemKind::ResetRequired,
        ));
        assert!(!status_matches_problem(
            reqwest::StatusCode::OK,
            ApplicationProblemKind::Stale,
        ));
    }

    #[test]
    fn remote_response_requires_the_mounted_http_wrapper_exactly() {
        let payload = serde_json::json!({"response": {"protocol_version": 1}});
        assert_eq!(
            canonical_response_payload(payload).unwrap(),
            serde_json::json!({"protocol_version": 1})
        );
        assert!(canonical_response_payload(serde_json::json!({"protocol_version": 1})).is_err());
        assert!(
            canonical_response_payload(serde_json::json!({
                "response": {"protocol_version": 1},
                "legacy": {}
            }))
            .is_err()
        );
    }
}
