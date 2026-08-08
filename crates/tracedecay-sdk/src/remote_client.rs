//! Enrolled HTTPS client for the canonical Remote Brain protocol.
//!
//! Every operation targets a versioned authenticated Remote Brain endpoint.
//! The only project-application operations exposed here are the two public
//! task-handoff endpoints: they reuse enrollment credential admission and do
//! not construct or tunnel arbitrary local application routes.

use std::fmt;
use std::time::Duration;

use reqwest::blocking::Client as HttpClient;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tracedecay_api::remote::{REMOTE_REPLACEMENT_CREDENTIAL_HEADER, REMOTE_REQUEST_ID_HEADER};
use tracedecay_application::remote::protocol::{
    CredentialRevocationRecoveryResponseV1, CredentialRevocationRequestV1,
    CredentialRotationRecoveryResponseV1, CredentialRotationRequestV1, REMOTE_PROTOCOL_VERSION_V1,
    RemoteProtocolBodyV1, RemoteProtocolRequestV1, RemoteProtocolResponseV1,
    remote_credential_revocation_result_contract_v1, remote_credential_rotation_result_contract_v1,
};
use tracedecay_application::{ApplicationOutcome, ApplicationProblemKind, EffectResult, RequestId};
use tracedecay_application::{
    HandoffOpenKindV1, HandoffOpenToken, IssueTaskHandoffRequestV1, IssueTaskHandoffResultV1,
    OpenTaskHandoffRequestV1, OpenTaskHandoffResultV1, handoff_open_consumption_input_digest,
    handoff_open_receipt_digest, issue_task_handoff_input_digest, open_task_handoff_input_digest,
};
use tracedecay_domain::{
    ActorId, CredentialRevocationReceiptV1, CredentialRotationReceiptV1,
    CurrentRemoteAuthorityStateV1, ManifestDigest, canonical_sha256, remote_node_actor_id,
    validate_remote_secret_length,
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

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteProtocolWireResponseV1 {
    pub protocol_version: u16,
    pub request_id: RequestId,
    pub authority: CurrentRemoteAuthorityStateV1,
    pub result: serde_json::Value,
}

pub type RemoteCredentialRotationResponseV1 = RemoteProtocolResponseV1<CredentialRotationReceiptV1>;
pub type RemoteCredentialRevocationResponseV1 =
    RemoteProtocolResponseV1<CredentialRevocationReceiptV1>;

impl EnrolledRemoteClient {
    pub fn new(
        endpoint: impl AsRef<str>,
        credential: impl AsRef<[u8]>,
        timeout: Duration,
    ) -> Result<Self, RemoteClientError> {
        Self::build(endpoint, credential, timeout, None)
    }

    /// Builds an enrolled HTTPS client with one explicit additional trust
    /// root. This enables hermetic deployments and tests without weakening
    /// HTTPS validation or relying on ambient machine trust.
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
        )
    }

    fn build(
        endpoint: impl AsRef<str>,
        credential: impl AsRef<[u8]>,
        timeout: Duration,
        root_certificate_pem: Option<&[u8]>,
    ) -> Result<Self, RemoteClientError> {
        let endpoint = reqwest::Url::parse(endpoint.as_ref())
            .map_err(|error| RemoteClientError::Configuration(error.to_string()))?;
        if endpoint.scheme() != "https"
            || endpoint.host_str().is_none()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || endpoint.username() != ""
            || endpoint.password().is_some()
        {
            return Err(RemoteClientError::Configuration(
                "Remote Brain endpoint must be a credential-free HTTPS URL".to_owned(),
            ));
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
        if let Some(pem) = root_certificate_pem {
            let certificate = reqwest::Certificate::from_pem(pem)
                .map_err(|error| RemoteClientError::Configuration(error.to_string()))?;
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

    pub fn execute<Request>(
        &self,
        route: &str,
        request: &RemoteProtocolRequestV1<Request>,
    ) -> Result<RemoteProtocolWireResponseV1, RemoteClientError>
    where
        Request: RemoteProtocolBodyV1 + Serialize,
    {
        request
            .validate_metadata()
            .map_err(|error| RemoteClientError::Protocol(error.to_string()))?;
        request
            .body
            .validate_remote_protocol_body(request.sent_at)
            .map_err(|error| RemoteClientError::Protocol(error.to_string()))?;
        let url = self
            .endpoint
            .join(route.trim_start_matches('/'))
            .map_err(|error| RemoteClientError::Configuration(error.to_string()))?;
        let response = self.execute_request(url, request, None)?;
        decode_response(response)
    }

    /// Issue a short-lived task-opening token as the actor authenticated by
    /// this client's enrollment credential.
    pub fn issue_task_handoff(
        &self,
        request: &RemoteProtocolRequestV1<IssueTaskHandoffRequestV1>,
    ) -> Result<RemoteProtocolResponseV1<IssueTaskHandoffResultV1>, RemoteClientError> {
        let input_digest = issue_task_handoff_input_digest(&request.body)
            .map_err(|error| RemoteClientError::Protocol(error.to_string()))?;
        let actor = remote_node_actor_id(&request.brain_id, &request.caller_node_id)
            .map_err(|error| RemoteClientError::Protocol(error.to_string()))?;
        let token_digest = HandoffOpenToken::new(request.body.token.clone())
            .and_then(|token| token.digest())
            .map_err(|_| RemoteClientError::Protocol("invalid task-handoff token".to_owned()))?;
        self.execute_task_handoff(
            "handoff/issue-task",
            request,
            "schema.handoff.issue_task_handoff.result",
            "issue_task_handoff",
            "use-case.handoff.issue_task_handoff",
            input_digest,
            actor,
            |result: &IssueTaskHandoffResultV1, _| {
                result.token_digest == token_digest
                    && result.issued_request_id == request.request_id
                    && result.session_id == request.body.session_id
                    && result.task_id == request.body.task_id
                    && result.version == request.body.version
                    && result.issued_at < result.expires_at
            },
        )
    }

    /// Open a task handoff as the independently authenticated recipient actor.
    pub fn open_task_handoff(
        &self,
        request: &RemoteProtocolRequestV1<OpenTaskHandoffRequestV1>,
    ) -> Result<RemoteProtocolResponseV1<OpenTaskHandoffResultV1>, RemoteClientError> {
        let input_digest = open_task_handoff_input_digest(&request.body)
            .map_err(|error| RemoteClientError::Protocol(error.to_string()))?;
        let actor = remote_node_actor_id(&request.brain_id, &request.caller_node_id)
            .map_err(|error| RemoteClientError::Protocol(error.to_string()))?;
        let token_digest = HandoffOpenToken::new(request.body.token.clone())
            .and_then(|token| token.digest())
            .map_err(|_| RemoteClientError::Protocol("invalid task-handoff token".to_owned()))?;
        self.execute_task_handoff(
            "handoff/open-task",
            request,
            "schema.handoff.open_task_handoff.result",
            "open_task_handoff",
            "use-case.handoff.open_task_handoff",
            input_digest,
            actor.clone(),
            |result, receipt| {
                canonical_open_task_payload_valid(
                    result,
                    &request.body,
                    &request.request_id,
                    &receipt.scope,
                    &actor,
                    &token_digest,
                )
            },
        )
    }

    fn execute_task_handoff<Request, Output>(
        &self,
        route: &str,
        request: &RemoteProtocolRequestV1<Request>,
        result_schema: &str,
        effect_identity: &str,
        use_case: &str,
        input_digest: ManifestDigest,
        actor: ActorId,
        validate_payload: impl Fn(&Output, &tracedecay_application::EffectReceipt) -> bool,
    ) -> Result<RemoteProtocolResponseV1<Output>, RemoteClientError>
    where
        Request: RemoteProtocolBodyV1 + Serialize,
        Output: Clone + DeserializeOwned + PartialEq + Serialize,
    {
        request
            .validate_metadata()
            .and_then(|()| request.body.validate_remote_protocol_body(request.sent_at))
            .map_err(|error| RemoteClientError::Protocol(error.to_string()))?;
        let contract = tracedecay_application::ResultContractRef::new(
            tracedecay_tool_catalog::SchemaId::new(result_schema)
                .map_err(|error| RemoteClientError::Protocol(error.to_string()))?,
            1,
        )
        .map_err(|error| RemoteClientError::Protocol(error.to_string()))?;
        let url = self
            .endpoint
            .join(route)
            .map_err(|error| RemoteClientError::Configuration(error.to_string()))?;
        let response = self.execute_request(url, request, None)?;
        decode_task_handoff_response(
            response,
            &request.request_id,
            &contract,
            effect_identity,
            use_case,
            &input_digest,
            &actor,
            validate_payload,
        )
    }

    /// Rotate through the canonical Remote Brain lifecycle route. The
    /// replacement secret is header-only and the request remains the canonical
    /// application contract re-exported by this SDK. The client never retains
    /// the replacement secret; after a confirmed success, construct the next
    /// client with that credential.
    pub fn rotate_credential(
        &self,
        request: &RemoteProtocolRequestV1<CredentialRotationRequestV1>,
        replacement_credential: impl AsRef<[u8]>,
    ) -> Result<RemoteCredentialRotationResponseV1, RemoteClientError> {
        request
            .validate_metadata()
            .and_then(|()| request.body.validate_remote_protocol_body(request.sent_at))
            .map_err(|error| RemoteClientError::Protocol(error.to_string()))?;
        let replacement = credential_header(replacement_credential.as_ref())?;
        let url = self
            .endpoint
            .join("credentials/rotate")
            .map_err(|error| RemoteClientError::Configuration(error.to_string()))?;
        let response = self.execute_request(url, request, Some(replacement))?;
        decode_typed_response(
            response,
            &request.request_id,
            &remote_credential_rotation_result_contract_v1(),
            "credential-rotation",
            "use-case.remote.credential-rotation",
            |receipt: &CredentialRotationReceiptV1| {
                receipt.enrollment_id == request.body.enrollment_id
                    && receipt.node_id == request.caller_node_id
                    && receipt.prior_revision == request.body.expected_revision
                    && request.body.expected_revision.checked_add(1)
                        == Some(receipt.current_revision)
                    && receipt.expires_at == request.body.expires_at
                    && receipt.rotated_at < receipt.expires_at
            },
        )
    }

    pub fn revoke_credential(
        &self,
        request: &RemoteProtocolRequestV1<CredentialRevocationRequestV1>,
    ) -> Result<RemoteCredentialRevocationResponseV1, RemoteClientError> {
        request
            .validate_metadata()
            .and_then(|()| request.body.validate_remote_protocol_body(request.sent_at))
            .map_err(|error| RemoteClientError::Protocol(error.to_string()))?;
        let url = self
            .endpoint
            .join("credentials/revoke")
            .map_err(|error| RemoteClientError::Configuration(error.to_string()))?;
        let response = self.execute_request(url, request, None)?;
        decode_typed_response(
            response,
            &request.request_id,
            &remote_credential_revocation_result_contract_v1(),
            "credential-revocation",
            "use-case.remote.credential-revocation",
            |receipt: &CredentialRevocationReceiptV1| {
                receipt.enrollment_id == request.body.enrollment_id
                    && receipt.prior_revision == request.body.expected_revision
                    && request.body.expected_revision.checked_add(1)
                        == Some(receipt.current_revision)
            },
        )
    }

    /// Recovers the durable receipt for a lost self-revocation response. The
    /// revoked secret authenticates this read-only exact-receipt lookup and is
    /// never re-admitted to a command.
    pub fn recover_self_revocation(
        &self,
        revocation_request_id: &RequestId,
    ) -> Result<CredentialRevocationReceiptV1, RemoteClientError> {
        let url = self
            .endpoint
            .join("credentials/revocation-status")
            .map_err(|error| RemoteClientError::Configuration(error.to_string()))?;
        let response = self
            .http
            .post(url)
            .header(AUTHORIZATION, self.authorization.clone())
            .header(REMOTE_REQUEST_ID_HEADER, revocation_request_id.as_str())
            .send()
            .map_err(|error| RemoteClientError::Transport(error.to_string()))?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(RemoteClientError::Protocol(
                "self-revocation receipt was unavailable".to_owned(),
            ));
        }
        let recovered: CredentialRevocationRecoveryResponseV1 = response
            .json()
            .map_err(|error| RemoteClientError::Protocol(error.to_string()))?;
        if recovered.protocol_version != REMOTE_PROTOCOL_VERSION_V1
            || &recovered.request_id != revocation_request_id
            || recovered.receipt.enrollment_id.as_str().is_empty()
            || recovered.receipt.prior_revision.checked_add(1)
                != Some(recovered.receipt.current_revision)
        {
            return Err(RemoteClientError::Protocol(
                "self-revocation receipt binding was invalid".to_owned(),
            ));
        }
        Ok(recovered.receipt)
    }

    /// Recovers the original durable rotation receipt with the replacement
    /// credential after the mutation response was lost.
    pub fn recover_rotation(
        &self,
        rotation_request_id: &RequestId,
    ) -> Result<CredentialRotationReceiptV1, RemoteClientError> {
        let url = self
            .endpoint
            .join("credentials/rotation-status")
            .map_err(|error| RemoteClientError::Configuration(error.to_string()))?;
        let response = self
            .http
            .post(url)
            .header(AUTHORIZATION, self.authorization.clone())
            .header(REMOTE_REQUEST_ID_HEADER, rotation_request_id.as_str())
            .send()
            .map_err(|error| RemoteClientError::Transport(error.to_string()))?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(RemoteClientError::Protocol(
                "rotation receipt was unavailable".to_owned(),
            ));
        }
        let recovered: CredentialRotationRecoveryResponseV1 = response
            .json()
            .map_err(|error| RemoteClientError::Protocol(error.to_string()))?;
        if recovered.protocol_version != REMOTE_PROTOCOL_VERSION_V1
            || &recovered.request_id != rotation_request_id
            || recovered.receipt.prior_revision.checked_add(1)
                != Some(recovered.receipt.current_revision)
            || recovered.receipt.rotated_at >= recovered.receipt.expires_at
        {
            return Err(RemoteClientError::Protocol(
                "rotation receipt binding was invalid".to_owned(),
            ));
        }
        Ok(recovered.receipt)
    }

    fn execute_request<Request>(
        &self,
        url: reqwest::Url,
        request: &RemoteProtocolRequestV1<Request>,
        replacement: Option<HeaderValue>,
    ) -> Result<reqwest::blocking::Response, RemoteClientError>
    where
        Request: Serialize,
    {
        let mut builder = self
            .http
            .post(url)
            .header(AUTHORIZATION, self.authorization.clone())
            .header(REMOTE_REQUEST_ID_HEADER, request.request_id.as_str())
            .header(CONTENT_TYPE, "application/json");
        if let Some(replacement) = replacement {
            builder = builder.header(REMOTE_REPLACEMENT_CREDENTIAL_HEADER, replacement);
        }
        builder
            .json(&serde_json::json!({ "request": request }))
            .send()
            .map_err(|error| RemoteClientError::Transport(error.to_string()))
    }
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

fn decode_response(
    response: reqwest::blocking::Response,
) -> Result<RemoteProtocolWireResponseV1, RemoteClientError> {
    response
        .json::<serde_json::Value>()
        .map_err(|error| RemoteClientError::Protocol(error.to_string()))
        .and_then(|value| {
            serde_json::from_value(value.get("response").cloned().unwrap_or(value))
                .map_err(|error| RemoteClientError::Protocol(error.to_string()))
        })
}

fn decode_typed_response<T: Clone + DeserializeOwned + PartialEq>(
    response: reqwest::blocking::Response,
    expected_request_id: &RequestId,
    expected_contract: &tracedecay_application::ResultContractRef,
    expected_identity: &str,
    expected_use_case: &str,
    validate_payload: impl Fn(&T) -> bool,
) -> Result<RemoteProtocolResponseV1<T>, RemoteClientError> {
    let status = response.status();
    let value = response
        .json::<serde_json::Value>()
        .map_err(|error| RemoteClientError::Protocol(error.to_string()))?;
    let response: RemoteProtocolResponseV1<T> =
        serde_json::from_value(value.get("response").cloned().unwrap_or(value))
            .map_err(|error| RemoteClientError::Protocol(error.to_string()))?;
    if response.protocol_version != REMOTE_PROTOCOL_VERSION_V1
        || &response.request_id != expected_request_id
        || response.authority.validate().is_err()
    {
        return Err(RemoteClientError::Protocol(
            "response version or request identity did not match the request".to_owned(),
        ));
    }
    match &response.result {
        Ok(envelope)
            if &envelope.request_id == expected_request_id
                && &envelope.contract == expected_contract
                && status == reqwest::StatusCode::OK
                && matches!(
                    &envelope.outcome,
                    ApplicationOutcome::Effect(effect)
                        if canonical_effect_valid(
                            effect,
                            expected_request_id,
                            expected_identity,
                            expected_use_case,
                        ) && effect.payload.as_ref().is_some_and(|payload| validate_payload(payload))
                ) => {}
        Err(problem)
            if &problem.request_id == expected_request_id
                && &problem.problem.request_id == expected_request_id
                && &problem.contract == expected_contract
                && status_matches_problem(status, problem.problem.kind()) => {}
        _ => {
            return Err(RemoteClientError::Protocol(
                "response status, result identity, or problem revision was invalid".to_owned(),
            ));
        }
    }
    Ok(response)
}

fn decode_task_handoff_response<T: Clone + DeserializeOwned + PartialEq + Serialize>(
    response: reqwest::blocking::Response,
    expected_request_id: &RequestId,
    expected_contract: &tracedecay_application::ResultContractRef,
    expected_identity: &str,
    expected_use_case: &str,
    input_digest: &ManifestDigest,
    actor: &ActorId,
    validate_payload: impl Fn(&T, &tracedecay_application::EffectReceipt) -> bool,
) -> Result<RemoteProtocolResponseV1<T>, RemoteClientError> {
    let status = response.status();
    let value = response
        .json::<serde_json::Value>()
        .map_err(|error| RemoteClientError::Protocol(error.to_string()))?;
    let response: RemoteProtocolResponseV1<T> =
        serde_json::from_value(value.get("response").cloned().unwrap_or(value))
            .map_err(|error| RemoteClientError::Protocol(error.to_string()))?;
    if response.protocol_version != REMOTE_PROTOCOL_VERSION_V1
        || &response.request_id != expected_request_id
        || response.authority.validate().is_err()
    {
        return Err(RemoteClientError::Protocol(
            "response version or request identity did not match the request".to_owned(),
        ));
    }
    match &response.result {
        Ok(envelope)
            if &envelope.request_id == expected_request_id
                && &envelope.contract == expected_contract
                && status == reqwest::StatusCode::OK
                && matches!(
                    &envelope.outcome,
                    ApplicationOutcome::Effect(effect)
                        if canonical_handoff_effect_valid(
                            effect,
                            expected_request_id,
                            expected_identity,
                            expected_use_case,
                            input_digest,
                            actor,
                        ) && effect.receipt.scope == envelope.scope
                            && effect.payload.as_ref().is_some_and(|payload| validate_payload(payload, &effect.receipt))
                ) => {}
        Err(problem)
            if &problem.request_id == expected_request_id
                && &problem.problem.request_id == expected_request_id
                && &problem.contract == expected_contract
                && status_matches_problem(status, problem.problem.kind()) => {}
        _ => {
            return Err(RemoteClientError::Protocol(
                "response status, task-handoff identity, or problem revision was invalid"
                    .to_owned(),
            ));
        }
    }
    Ok(response)
}

fn canonical_open_task_payload_valid(
    result: &OpenTaskHandoffResultV1,
    request: &OpenTaskHandoffRequestV1,
    request_id: &RequestId,
    scope: &tracedecay_application::ResolvedScope,
    actor: &ActorId,
    token_digest: &ManifestDigest,
) -> bool {
    let Ok(input_digest) = handoff_open_consumption_input_digest(
        HandoffOpenKindV1::Task,
        &request.session_id,
        scope,
        actor,
        token_digest,
    ) else {
        return false;
    };
    let Ok(receipt_digest) = handoff_open_receipt_digest(
        &result.receipt.binding_digest,
        token_digest,
        request_id,
        &input_digest,
        result.receipt.consumed_at,
    ) else {
        return false;
    };
    result.receipt.token_digest == *token_digest
        && result.receipt.binding_digest.validate().is_ok()
        && result.receipt.request_id == *request_id
        && result.receipt.input_digest == input_digest
        && result.receipt.receipt_digest == receipt_digest
}

fn canonical_handoff_effect_valid<T: Clone + PartialEq + Serialize>(
    effect: &EffectResult<T>,
    request_id: &RequestId,
    identity: &str,
    use_case: &str,
    input_digest: &ManifestDigest,
    actor: &ActorId,
) -> bool {
    let Some(suffix) = input_digest.as_str().strip_prefix("sha256:") else {
        return false;
    };
    let expected_effect_id = format!("effect.handoff.{identity}.{suffix}");
    let expected_idempotency = format!("handoff.{identity}.{suffix}");
    let Ok(expected_state) = canonical_sha256(&(
        "tracedecay.handoff.expected-state.v1",
        identity,
        input_digest,
    )) else {
        return false;
    };
    let Some(payload) = effect.payload.as_ref() else {
        return false;
    };
    let Ok(committed_state) =
        canonical_sha256(&("tracedecay.handoff.committed-state.v1", identity, payload))
    else {
        return false;
    };
    let Ok(catalog_digest) = canonical_sha256(&("tracedecay.handoff.catalog.v1", identity)) else {
        return false;
    };
    let Ok(privacy_digest) = canonical_sha256(&(
        "tracedecay.handoff.privacy.v1",
        &effect.receipt.scope,
        effect.authority.disclosure,
    )) else {
        return false;
    };
    if effect.effect_id.as_str() != expected_effect_id
        || effect.idempotency_key.as_str() != expected_idempotency
        || effect.effect_class != tracedecay_tool_catalog::EffectClass::Administrative
        || effect.receipt.effect_class != tracedecay_tool_catalog::EffectClass::Administrative
        || effect.receipt.request_id != *request_id
        || effect.receipt.operation.as_str() != use_case
        || effect.receipt.idempotency_key != effect.idempotency_key
        || effect.receipt.input_digest != *input_digest
        || effect.expected_state != expected_state
        || effect.receipt.expected_state != expected_state
        || effect.receipt.committed_state.as_ref() != Some(&committed_state)
        || &effect.receipt.actor != actor
        || effect.receipt.outcome != tracedecay_application::EffectTermination::Completed
        || effect.reconciliation != tracedecay_application::ReconciliationState::Reconciled
        || effect
            .authority
            .validate_for(&effect.receipt.scope)
            .is_err()
        || effect.authority.policy.digest != effect.receipt.policy_digest
        || effect.receipt.catalog_digest != catalog_digest
        || effect.receipt.privacy_digest != privacy_digest
        || effect.receipt.configuration_digest.validate().is_err()
    {
        return false;
    }
    EffectResult::new(
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

fn canonical_effect_valid<T: Clone + PartialEq>(
    effect: &EffectResult<T>,
    request_id: &RequestId,
    identity: &str,
    use_case: &str,
) -> bool {
    let expected_effect_id = format!("effect.remote.{identity}.{}", request_id.as_str());
    let expected_idempotency = format!("remote.{identity}.{}", request_id.as_str());
    if effect.effect_id.as_str() != expected_effect_id.as_str()
        || effect.idempotency_key.as_str() != expected_idempotency.as_str()
        || effect.receipt.request_id != *request_id
        || effect.receipt.operation.as_str() != use_case
    {
        return false;
    }
    EffectResult::new(
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

fn status_matches_problem(status: reqwest::StatusCode, kind: ApplicationProblemKind) -> bool {
    let expected = match kind {
        ApplicationProblemKind::InvalidRequest => reqwest::StatusCode::BAD_REQUEST,
        ApplicationProblemKind::NotFoundOrNotAuthorized => reqwest::StatusCode::NOT_FOUND,
        ApplicationProblemKind::Conflict | ApplicationProblemKind::Stale => {
            reqwest::StatusCode::CONFLICT
        }
        ApplicationProblemKind::Unsupported => reqwest::StatusCode::UNPROCESSABLE_ENTITY,
        ApplicationProblemKind::Unavailable => reqwest::StatusCode::SERVICE_UNAVAILABLE,
        ApplicationProblemKind::Saturated => reqwest::StatusCode::TOO_MANY_REQUESTS,
        ApplicationProblemKind::Cancelled => reqwest::StatusCode::REQUEST_TIMEOUT,
        ApplicationProblemKind::TimedOut => reqwest::StatusCode::GATEWAY_TIMEOUT,
    };
    status == expected
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handoff_effect(
        actor: tracedecay_domain::ActorId,
        input_digest: tracedecay_domain::ManifestDigest,
    ) -> EffectResult<String> {
        use std::collections::BTreeSet;

        use tracedecay_application::{
            AuthorityReceipt, CancellationContext, CapabilityGrantSnapshot, Deadline,
            DisclosureClass, EffectId, EffectReceipt, EffectTermination, IdempotencyKey,
            OperationBudgetUsage, OperationReceipt, PolicyDecisionRef, ReconciliationState,
            RequestContext, ResolvedScope,
        };
        use tracedecay_domain::{
            CapabilityId, ComponentVersion, ProjectId, RepositoryId, UseCaseId, UtcMicros,
            WorktreeId,
        };
        use tracedecay_tool_catalog::EffectClass;

        let request_id = RequestId::new("request.sdk.handoff").unwrap();
        let scope = ResolvedScope::new(
            ProjectId::new("project.sdk.handoff").unwrap(),
            RepositoryId::new("repository.sdk.handoff").unwrap(),
            WorktreeId::new("worktree.sdk.handoff").unwrap(),
            None,
        )
        .unwrap();
        let grant_digest = canonical_sha256(&("sdk-handoff-grant", &actor)).unwrap();
        let grant = CapabilityGrantSnapshot::new(
            tracedecay_application::CapabilityGrantId::new("grant.sdk.handoff").unwrap(),
            1,
            grant_digest,
            actor.clone(),
            UtcMicros(1),
            UtcMicros(100),
            scope.clone(),
            BTreeSet::from([CapabilityId::new("capability.handoff.open_task_handoff").unwrap()]),
            BTreeSet::from([UseCaseId::new("use-case.handoff.open_task_handoff").unwrap()]),
            DisclosureClass::Evidence,
        )
        .unwrap();
        let context = RequestContext::new(
            actor.clone(),
            scope.clone(),
            grant,
            request_id.clone(),
            Deadline::new(UtcMicros(90)).unwrap(),
            CancellationContext::active("cancel.sdk.handoff").unwrap(),
        )
        .unwrap();
        let policy = PolicyDecisionRef::new(
            "policy.sdk.handoff",
            1,
            canonical_sha256(&"sdk-handoff-policy").unwrap(),
            ComponentVersion::new("sdk.handoff.policy.v1").unwrap(),
        )
        .unwrap();
        let authority = AuthorityReceipt::from_context(&context, policy, UtcMicros(10)).unwrap();
        let suffix = input_digest
            .as_str()
            .strip_prefix("sha256:")
            .unwrap()
            .to_owned();
        let identity = "open_task_handoff";
        let idempotency = IdempotencyKey::new(format!("handoff.{identity}.{suffix}")).unwrap();
        let expected_state = canonical_sha256(&(
            "tracedecay.handoff.expected-state.v1",
            identity,
            &input_digest,
        ))
        .unwrap();
        let payload = "opened".to_owned();
        let committed_state =
            canonical_sha256(&("tracedecay.handoff.committed-state.v1", identity, &payload))
                .unwrap();
        let receipt = EffectReceipt {
            operation: UseCaseId::new("use-case.handoff.open_task_handoff").unwrap(),
            request_id: request_id.clone(),
            actor,
            scope,
            effect_class: EffectClass::Administrative,
            idempotency_key: idempotency.clone(),
            input_digest,
            expected_state: expected_state.clone(),
            policy_digest: authority.policy.digest.clone(),
            configuration_digest: canonical_sha256(&"sdk-handoff-config").unwrap(),
            catalog_digest: canonical_sha256(&(
                "tracedecay.handoff.catalog.v1",
                "open_task_handoff",
            ))
            .unwrap(),
            privacy_digest: canonical_sha256(&(
                "tracedecay.handoff.privacy.v1",
                context.scope(),
                DisclosureClass::Evidence,
            ))
            .unwrap(),
            outcome: EffectTermination::Completed,
            committed_state: Some(committed_state),
            external_proof: None,
        };
        EffectResult::new(
            EffectId::new(format!("effect.handoff.{identity}.{suffix}")).unwrap(),
            EffectClass::Administrative,
            idempotency,
            authority,
            expected_state,
            OperationReceipt::completed(
                UtcMicros(10),
                UtcMicros(11),
                Deadline::new(UtcMicros(90)).unwrap(),
                OperationBudgetUsage::default(),
            )
            .unwrap(),
            ReconciliationState::Reconciled,
            receipt,
            Some(payload),
        )
        .unwrap()
    }

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
        assert!(!status_matches_problem(
            reqwest::StatusCode::OK,
            ApplicationProblemKind::Stale,
        ));
    }

    #[test]
    fn task_handoff_effect_rejects_cross_request_and_actor_attribution() {
        let actor = tracedecay_domain::ActorId::new("actor.sdk.handoff.a").unwrap();
        let other_actor = tracedecay_domain::ActorId::new("actor.sdk.handoff.b").unwrap();
        let input_digest = canonical_sha256(&"sdk-handoff-input-a").unwrap();
        let other_input_digest = canonical_sha256(&"sdk-handoff-input-b").unwrap();
        let effect = handoff_effect(actor.clone(), input_digest.clone());
        let mut wrong_authority = effect.clone();
        wrong_authority.authority.authorized_scope_digest =
            canonical_sha256(&"wrong-sdk-handoff-scope").unwrap();
        let request_id = RequestId::new("request.sdk.handoff").unwrap();

        assert!(canonical_handoff_effect_valid(
            &effect,
            &request_id,
            "open_task_handoff",
            "use-case.handoff.open_task_handoff",
            &input_digest,
            &actor,
        ));
        assert!(!canonical_handoff_effect_valid(
            &effect,
            &request_id,
            "open_task_handoff",
            "use-case.handoff.open_task_handoff",
            &other_input_digest,
            &actor,
        ));
        assert!(!canonical_handoff_effect_valid(
            &effect,
            &request_id,
            "open_task_handoff",
            "use-case.handoff.open_task_handoff",
            &input_digest,
            &other_actor,
        ));
        assert!(!canonical_handoff_effect_valid(
            &wrong_authority,
            &request_id,
            "open_task_handoff",
            "use-case.handoff.open_task_handoff",
            &input_digest,
            &actor,
        ));
    }

    #[test]
    fn open_task_payload_rejects_tampered_input_and_receipt_digests() {
        let actor = tracedecay_domain::ActorId::new("actor.sdk.handoff.recipient").unwrap();
        let scope = tracedecay_application::ResolvedScope::new(
            tracedecay_domain::ProjectId::new("project.sdk.handoff").unwrap(),
            tracedecay_domain::RepositoryId::new("repository.sdk.handoff").unwrap(),
            tracedecay_domain::WorktreeId::new("worktree.sdk.handoff").unwrap(),
            None,
        )
        .unwrap();
        let request_id = RequestId::new("request.sdk.handoff.open").unwrap();
        let request = OpenTaskHandoffRequestV1 {
            token: "sdk-handoff-open-token-01234567890123456789".to_owned(),
            session_id: tracedecay_application::HandoffSessionId::new("session.sdk.handoff")
                .unwrap(),
        };
        let token_digest = HandoffOpenToken::new(request.token.clone())
            .and_then(|token| token.digest())
            .unwrap();
        let input_digest = handoff_open_consumption_input_digest(
            HandoffOpenKindV1::Task,
            &request.session_id,
            &scope,
            &actor,
            &token_digest,
        )
        .unwrap();
        let binding_digest = canonical_sha256(&"opaque-sdk-handoff-binding").unwrap();
        let consumed_at = tracedecay_domain::UtcMicros(20);
        let receipt_digest = handoff_open_receipt_digest(
            &binding_digest,
            &token_digest,
            &request_id,
            &input_digest,
            consumed_at,
        )
        .unwrap();
        let result = OpenTaskHandoffResultV1 {
            surface: tracedecay_application::TaskHandoffSurfaceV1 {
                task_id: tracedecay_domain::TaskId::new("task.sdk.handoff").unwrap(),
                version: tracedecay_domain::WorkVersion::initial(),
            },
            receipt: tracedecay_application::HandoffOpenReceiptV1 {
                binding_digest,
                token_digest: token_digest.clone(),
                request_id: request_id.clone(),
                input_digest,
                consumed_at,
                receipt_digest,
            },
        };

        assert!(canonical_open_task_payload_valid(
            &result,
            &request,
            &request_id,
            &scope,
            &actor,
            &token_digest,
        ));
        let mut tampered_input = result.clone();
        tampered_input.receipt.input_digest = canonical_sha256(&"tampered-input").unwrap();
        assert!(!canonical_open_task_payload_valid(
            &tampered_input,
            &request,
            &request_id,
            &scope,
            &actor,
            &token_digest,
        ));
        let mut tampered_receipt = result;
        tampered_receipt.receipt.receipt_digest = canonical_sha256(&"tampered-receipt").unwrap();
        assert!(!canonical_open_task_payload_valid(
            &tampered_receipt,
            &request,
            &request_id,
            &scope,
            &actor,
            &token_digest,
        ));
    }
}
