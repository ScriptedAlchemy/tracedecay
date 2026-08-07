//! Blocking local/remote client for the canonical HTTP and SSE lifecycle.

use std::error::Error;
use std::fmt;
use std::io::{BufRead, BufReader};
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::blocking::{Client as HttpClient, Response};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, ORIGIN};
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tracedecay_tool_catalog::{
    CancellationPoint, ReceiptContract, ReconciliationContract, TerminalState,
};

use crate::operations::TypedOperation;

const MAX_OPAQUE_BYTES: usize = 4_096;
const MAX_REQUEST_ID_BYTES: usize = 512;
const DEADLINE_HEADER: &str = "x-tracedecay-deadline-micros";

/// Per-invocation transport controls accepted by typed operations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OperationRequestOptions {
    /// Absolute UTC deadline in microseconds, forwarded to daemon admission.
    pub deadline_micros: Option<i64>,
}

/// Selects loopback or remote HTTP policy without changing operation semantics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionMode {
    Local(ConnectionSettings),
    Remote(ConnectionSettings),
}

/// Shared authority settings for either connection mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionSettings {
    base_url: String,
    project_id: String,
    token: String,
}

impl ConnectionMode {
    pub fn local(
        base_url: impl Into<String>,
        project_id: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        Self::Local(ConnectionSettings {
            base_url: base_url.into(),
            project_id: project_id.into(),
            token: token.into(),
        })
    }

    pub fn remote(
        base_url: impl Into<String>,
        project_id: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        Self::Remote(ConnectionSettings {
            base_url: base_url.into(),
            project_id: project_id.into(),
            token: token.into(),
        })
    }

    fn settings(&self) -> &ConnectionSettings {
        match self {
            Self::Local(settings) | Self::Remote(settings) => settings,
        }
    }
}

/// Builder for a [`Client`].
#[derive(Clone, Debug)]
pub struct ClientBuilder {
    mode: ConnectionMode,
    origin: Option<String>,
    timeout: Duration,
}

impl ClientBuilder {
    pub fn origin(mut self, origin: impl Into<String>) -> Self {
        self.origin = Some(origin.into());
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn build(self) -> Result<Client, ClientError> {
        let settings = self.mode.settings();
        validate_opaque(&settings.project_id, MAX_REQUEST_ID_BYTES, "project ID")?;
        validate_opaque(&settings.token, MAX_OPAQUE_BYTES, "bearer token")?;
        let mut base = reqwest::Url::parse(&settings.base_url)
            .map_err(|error| ClientError::InvalidConfiguration(error.to_string()))?;
        if base.scheme() != "http" && base.scheme() != "https" {
            return Err(ClientError::InvalidConfiguration(
                "base URL must use http or https".into(),
            ));
        }
        if base.query().is_some() || base.fragment().is_some() {
            return Err(ClientError::InvalidConfiguration(
                "base URL must not contain a query or fragment".into(),
            ));
        }
        let normalized_path = base.path().trim_end_matches('/').to_owned();
        base.set_path(&normalized_path);
        let default_origin = base.origin().ascii_serialization();
        let origin = self.origin.unwrap_or(default_origin);
        let origin_value = HeaderValue::from_str(&origin)
            .map_err(|error| ClientError::InvalidConfiguration(error.to_string()))?;
        let authorization = HeaderValue::from_str(&format!("Bearer {}", settings.token))
            .map_err(|error| ClientError::InvalidConfiguration(error.to_string()))?;
        let http = HttpClient::builder()
            .timeout(self.timeout)
            .build()
            .map_err(ClientError::transport)?;
        let application_root = format!(
            "{}/projects/{}/application",
            base.as_str().trim_end_matches('/'),
            settings.project_id
        );
        Ok(Client {
            http,
            application_root,
            authorization,
            origin: origin_value,
        })
    }
}

/// Blocking lifecycle client. Clone it to share the connection pool.
#[derive(Clone, Debug)]
pub struct Client {
    http: HttpClient,
    application_root: String,
    authorization: HeaderValue,
    origin: HeaderValue,
}

impl Client {
    pub fn builder(mode: ConnectionMode) -> ClientBuilder {
        ClientBuilder {
            mode,
            origin: None,
            timeout: Duration::from_secs(30),
        }
    }

    /// Invoke one operation admitted by canonical request and result schemas.
    pub fn execute<Operation>(
        &self,
        request: &Operation::Request,
    ) -> Result<TypedResponse<Operation::Result>, ClientError>
    where
        Operation: TypedOperation,
        Operation::Request: Serialize,
        Operation::Result: DeserializeOwned,
    {
        self.execute_with_options::<Operation>(request, OperationRequestOptions::default())
    }

    /// Invoke one typed operation with explicit transport controls.
    pub fn execute_with_options<Operation>(
        &self,
        request: &Operation::Request,
        options: OperationRequestOptions,
    ) -> Result<TypedResponse<Operation::Result>, ClientError>
    where
        Operation: TypedOperation,
        Operation::Request: Serialize,
        Operation::Result: DeserializeOwned,
    {
        let request = serde_json::to_value(request).map_err(|error| ClientError::Protocol {
            status: None,
            message: format!("typed request could not be encoded: {error}"),
        })?;
        let response = self.request_route(Operation::ROUTE, &request, options)?;
        let binding = response
            .envelope()
            .get("binding_id")
            .and_then(Value::as_str);
        let contract = response.envelope().get("contract");
        let schema_id = contract
            .and_then(|value| value.get("schema_id"))
            .and_then(Value::as_str);
        let schema_revision = contract
            .and_then(|value| value.get("schema_revision"))
            .and_then(Value::as_u64);
        if binding != Some(Operation::BINDING_ID)
            || schema_id != Some(Operation::RESULT_SCHEMA_ID)
            || schema_revision != Some(u64::from(Operation::RESULT_SCHEMA_REVISION))
        {
            return Err(ClientError::Protocol {
                status: Some(response.status()),
                message: format!(
                    "daemon returned mismatched contracts for {}",
                    Operation::OPERATION_ID
                ),
            });
        }
        let outcome = response
            .envelope()
            .get("outcome")
            .ok_or_else(|| ClientError::Protocol {
                status: Some(response.status()),
                message: format!("daemon omitted the {} outcome", Operation::OPERATION_ID),
            })?;
        let outcome_kind = outcome
            .get("outcome")
            .and_then(Value::as_str)
            .ok_or_else(|| ClientError::Protocol {
                status: Some(response.status()),
                message: format!(
                    "daemon omitted the {} outcome kind",
                    Operation::OPERATION_ID
                ),
            })?;
        let lifecycle_shape_is_legal = matches!(
            (Operation::RECEIPT, Operation::RECONCILIATION, outcome_kind),
            (
                ReceiptContract::Operation,
                ReconciliationContract::NotRequired,
                "evidence" | "preview"
            ) | (
                ReceiptContract::DurableEffect,
                ReconciliationContract::Required,
                "effect"
            )
        );
        if !lifecycle_shape_is_legal {
            return Err(ClientError::Protocol {
                status: Some(response.status()),
                message: format!(
                    "daemon returned outcome {outcome_kind} outside the {} receipt contract",
                    Operation::OPERATION_ID
                ),
            });
        }
        let execution = outcome
            .get("value")
            .and_then(|value| value.get("execution"))
            .ok_or_else(|| ClientError::Protocol {
                status: Some(response.status()),
                message: format!(
                    "daemon omitted the {} execution receipt",
                    Operation::OPERATION_ID
                ),
            })?;
        let termination = execution
            .get("termination")
            .and_then(Value::as_str)
            .ok_or_else(|| ClientError::Protocol {
                status: Some(response.status()),
                message: format!(
                    "daemon omitted the {} terminal state",
                    Operation::OPERATION_ID
                ),
            })?;
        if !Operation::TERMINAL_STATES
            .iter()
            .copied()
            .any(|state| terminal_state_name(state) == termination)
        {
            return Err(ClientError::Protocol {
                status: Some(response.status()),
                message: format!(
                    "daemon returned terminal state {termination} outside the {} contract",
                    Operation::OPERATION_ID
                ),
            });
        }
        if let Some(cancellation) = execution.get("cancellation")
            && !cancellation.is_null()
        {
            if !Operation::CANCELLABLE {
                return Err(ClientError::Protocol {
                    status: Some(response.status()),
                    message: format!(
                        "daemon returned cancellation evidence for non-cancellable {}",
                        Operation::OPERATION_ID
                    ),
                });
            }
            let stage = cancellation
                .get("stage")
                .and_then(Value::as_str)
                .ok_or_else(|| ClientError::Protocol {
                    status: Some(response.status()),
                    message: format!(
                        "daemon returned malformed cancellation evidence for {}",
                        Operation::OPERATION_ID
                    ),
                })?;
            if !Operation::CANCELLATION_POINTS
                .iter()
                .copied()
                .any(|point| cancellation_point_name(point) == stage)
            {
                return Err(ClientError::Protocol {
                    status: Some(response.status()),
                    message: format!(
                        "daemon returned cancellation stage {stage} outside the {} contract",
                        Operation::OPERATION_ID
                    ),
                });
            }
        }
        let payload = response
            .payload()
            .cloned()
            .ok_or_else(|| ClientError::Protocol {
                status: Some(response.status()),
                message: format!(
                    "daemon omitted the {} result payload",
                    Operation::OPERATION_ID
                ),
            })?;
        let result = serde_json::from_value(payload).map_err(|error| ClientError::Protocol {
            status: Some(response.status()),
            message: format!(
                "daemon returned a malformed {} result: {error}",
                Operation::OPERATION_ID
            ),
        })?;
        let request_id = response
            .envelope()
            .get("request_id")
            .and_then(Value::as_str)
            .ok_or_else(|| ClientError::Protocol {
                status: Some(response.status()),
                message: "daemon omitted the application request ID".into(),
            })?
            .to_owned();
        Ok(TypedResponse {
            request_id,
            result,
            envelope: response.envelope,
        })
    }

    fn request_route(
        &self,
        route: &str,
        request: &Value,
        options: OperationRequestOptions,
    ) -> Result<ApplicationResponse, ClientError> {
        let route = route.strip_prefix("/application").ok_or_else(|| {
            ClientError::InvalidConfiguration(
                "typed operation route must begin with /application".into(),
            )
        })?;
        let url = reqwest::Url::parse(&format!("{}{}", self.application_root, route))
            .map_err(|error| ClientError::InvalidConfiguration(error.to_string()))?;
        let mut headers = self.headers("application/json");
        if let Some(deadline_micros) = options.deadline_micros {
            if deadline_micros <= 0 {
                return Err(ClientError::InvalidRequest(
                    "deadline_micros must be positive".into(),
                ));
            }
            let value = HeaderValue::from_str(&deadline_micros.to_string())
                .map_err(|error| ClientError::InvalidConfiguration(error.to_string()))?;
            headers.insert(DEADLINE_HEADER, value);
        }
        let response = self
            .http
            .post(url)
            .headers(headers)
            .json(request)
            .send()
            .map_err(ClientError::transport)?;
        self.decode_application_response(response)
    }

    /// Requests cancellation of a previously accepted operation.
    pub fn cancel_operation(
        &self,
        operation_id: &str,
    ) -> Result<OperationCancellation, ClientError> {
        validate_opaque(operation_id, MAX_REQUEST_ID_BYTES, "operation ID")?;
        let response = self
            .http
            .post(self.lifecycle_url(operation_id, "cancel")?)
            .headers(self.headers("application/json"))
            .send()
            .map_err(ClientError::transport)?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(ClientError::Authentication(status.as_u16()));
        }
        let body: Value = response.json().map_err(|error| ClientError::Protocol {
            status: Some(status.as_u16()),
            message: format!("daemon returned malformed cancellation JSON: {error}"),
        })?;
        if body.get("kind").and_then(Value::as_str) == Some("problem") {
            return Err(problem_error(
                &body,
                status.as_u16(),
                "cancellation problem envelope has no value",
            ));
        }
        let value: OperationCancellation =
            serde_json::from_value(body).map_err(|error| ClientError::Protocol {
                status: Some(status.as_u16()),
                message: format!("daemon returned malformed cancellation JSON: {error}"),
            })?;
        let valid_status = matches!(
            (status, value.status),
            (StatusCode::ACCEPTED, CancellationStatus::Requested)
                | (StatusCode::OK, CancellationStatus::AlreadyRequested)
                | (StatusCode::OK, CancellationStatus::AlreadyTerminal)
        );
        if !valid_status {
            return Err(ClientError::Protocol {
                status: Some(status.as_u16()),
                message: "daemon returned a non-canonical cancellation response".into(),
            });
        }
        Ok(value)
    }

    /// Opens the canonical event stream, optionally from a resume frontier.
    pub fn stream_operation(
        &self,
        operation_id: &str,
        options: StreamOptions,
    ) -> Result<OperationStream, ClientError> {
        validate_opaque(operation_id, MAX_REQUEST_ID_BYTES, "operation ID")?;
        if let Some(resume) = &options.resume {
            validate_opaque(&resume.token, MAX_OPAQUE_BYTES, "resume token")?;
        }
        let mut stream = OperationStream {
            client: self.clone(),
            operation_id: operation_id.to_owned(),
            reader: None,
            options,
            reconnects: 0,
            next_sequence: None,
            resume_token: None,
            terminal: false,
            pending_event: None,
            pending_id: None,
            pending_data: Vec::new(),
        };
        if let Some(resume) = &stream.options.resume {
            stream.next_sequence = Some(resume.next_sequence);
            stream.resume_token = Some(resume.token.clone());
        }
        stream.connect()?;
        Ok(stream)
    }

    fn lifecycle_url(&self, operation_id: &str, suffix: &str) -> Result<reqwest::Url, ClientError> {
        reqwest::Url::parse(&format!(
            "{}/operations/{operation_id}/{suffix}",
            self.application_root
        ))
        .map_err(|error| ClientError::InvalidConfiguration(error.to_string()))
    }

    fn headers(&self, accept: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static(accept));
        headers.insert(AUTHORIZATION, self.authorization.clone());
        headers.insert(ORIGIN, self.origin.clone());
        headers
    }

    fn decode_application_response(
        &self,
        response: Response,
    ) -> Result<ApplicationResponse, ClientError> {
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(ClientError::Authentication(status.as_u16()));
        }
        if media_type(response.headers()) != Some("application/json") {
            return Err(ClientError::Protocol {
                status: Some(status.as_u16()),
                message: "daemon response is not application/json".into(),
            });
        }
        let body: Value = response.json().map_err(|error| ClientError::Protocol {
            status: Some(status.as_u16()),
            message: format!("daemon returned malformed application JSON: {error}"),
        })?;
        match body.get("kind").and_then(Value::as_str) {
            Some("success") if status.is_success() => {
                let value = body
                    .get("value")
                    .cloned()
                    .ok_or_else(|| ClientError::Protocol {
                        status: Some(status.as_u16()),
                        message: "success envelope has no value".into(),
                    })?;
                ApplicationResponse::new(value, status.as_u16())
            }
            Some("problem") if !status.is_success() => Err(problem_error(
                &body,
                status.as_u16(),
                "problem envelope has no value",
            )),
            _ => Err(ClientError::Protocol {
                status: Some(status.as_u16()),
                message: "daemon returned an inconsistent HTTP envelope".into(),
            }),
        }
    }
}

const fn terminal_state_name(state: TerminalState) -> &'static str {
    match state {
        TerminalState::Completed => "completed",
        TerminalState::Cancelled => "cancelled",
        TerminalState::TimedOut => "timed_out",
        TerminalState::Failed => "failed",
        TerminalState::Unavailable => "unavailable",
        TerminalState::EffectUnknown => "effect_unknown",
        TerminalState::Partial => "partial",
    }
}

const fn cancellation_point_name(point: CancellationPoint) -> &'static str {
    match point {
        CancellationPoint::BeforeAdmission => "before_admission",
        CancellationPoint::BeforeRead => "before_read",
        CancellationPoint::DuringRead => "during_read",
        CancellationPoint::BeforeEffect => "before_effect",
        CancellationPoint::EffectInFlight => "effect_in_flight",
        CancellationPoint::Reconciling => "reconciling",
        CancellationPoint::AfterCommit => "after_commit",
    }
}

/// One typed operation result with its lifecycle correlation identity and
/// validated canonical envelope.
#[derive(Clone, Debug, PartialEq)]
pub struct TypedResponse<Result> {
    pub request_id: String,
    pub result: Result,
    /// The validated success envelope, including operation/effect receipts.
    pub envelope: Value,
}

/// A decoded successful application envelope.
#[derive(Clone, Debug, PartialEq)]
pub struct ApplicationResponse {
    status: u16,
    envelope: Value,
}

impl ApplicationResponse {
    fn new(envelope: Value, status: u16) -> Result<Self, ClientError> {
        validate_success_envelope(&envelope, status)?;
        Ok(Self { status, envelope })
    }

    pub const fn status(&self) -> u16 {
        self.status
    }

    pub fn envelope(&self) -> &Value {
        &self.envelope
    }

    pub fn payload(&self) -> Option<&Value> {
        self.envelope.get("outcome")?.get("value")?.get("payload")
    }
}

/// The response media type, without its parameters.
fn media_type(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
}

/// Convert an already-identified `problem` envelope into its client error.
///
/// The caller supplies the message for a body that claims to be a problem but
/// carries no value, because only the caller knows which surface it read.
fn problem_error(body: &Value, status: u16, missing_value: &'static str) -> ClientError {
    let Some(envelope) = body.get("value").cloned() else {
        return ClientError::Protocol {
            status: Some(status),
            message: missing_value.into(),
        };
    };
    match ProblemError::new(status, envelope) {
        Ok(problem) => ClientError::Problem(Box::new(problem)),
        Err(error) => error,
    }
}

/// Whether an event name ends the operation's stream.
fn is_terminal_event(event: &str) -> bool {
    matches!(
        event,
        "completed" | "cancelled" | "timed_out" | "failed" | "partial" | "effect_unknown"
    )
}

fn protocol(status: u16, message: impl Into<String>) -> ClientError {
    ClientError::Protocol {
        status: Some(status),
        message: message.into(),
    }
}

fn object<'a>(
    value: Option<&'a Value>,
    status: u16,
    field: &str,
) -> Result<&'a serde_json::Map<String, Value>, ClientError> {
    value
        .and_then(Value::as_object)
        .ok_or_else(|| protocol(status, format!("{field} must be an object")))
}

fn string<'a>(value: Option<&'a Value>, status: u16, field: &str) -> Result<&'a str, ClientError> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| protocol(status, format!("{field} must be a non-empty string")))
}

fn unsigned(value: Option<&Value>, status: u16, field: &str) -> Result<u64, ClientError> {
    value
        .and_then(Value::as_u64)
        .ok_or_else(|| protocol(status, format!("{field} must be an unsigned integer")))
}

fn array(value: Option<&Value>, status: u16, field: &str) -> Result<(), ClientError> {
    value
        .and_then(Value::as_array)
        .map(|_| ())
        .ok_or_else(|| protocol(status, format!("{field} must be an array")))
}

fn nullable_unsigned(value: Option<&Value>, status: u16, field: &str) -> Result<(), ClientError> {
    match value {
        Some(Value::Null) => Ok(()),
        Some(Value::Number(number)) if number.as_u64().is_some() => Ok(()),
        _ => Err(protocol(
            status,
            format!("{field} must be null or an unsigned integer"),
        )),
    }
}

fn validate_receipt<'a>(
    value: Option<&'a Value>,
    status: u16,
    field: &str,
) -> Result<&'a str, ClientError> {
    let receipt = object(value, status, field)?;
    unsigned(receipt.get("started_at"), status, "execution.started_at")?;
    unsigned(receipt.get("ended_at"), status, "execution.ended_at")?;
    if !receipt.contains_key("effective_deadline") || !receipt.contains_key("cancellation") {
        return Err(protocol(status, "execution lifecycle fields are required"));
    }
    let budget = object(receipt.get("budget"), status, "execution.budget")?;
    unsigned(
        budget.get("units_consumed"),
        status,
        "budget.units_consumed",
    )?;
    unsigned(
        budget.get("bytes_consumed"),
        status,
        "budget.bytes_consumed",
    )?;
    unsigned(
        budget.get("elapsed_micros"),
        status,
        "budget.elapsed_micros",
    )?;
    string(receipt.get("termination"), status, "execution.termination")
}

fn validate_page(value: Option<&Value>, status: u16) -> Result<(), ClientError> {
    let page = object(value, status, "outcome.value.page")?;
    string(
        page.get("sort_contract_id"),
        status,
        "page.sort_contract_id",
    )?;
    if unsigned(page.get("sort_revision"), status, "page.sort_revision")? == 0 {
        return Err(protocol(status, "page.sort_revision must be positive"));
    }
    nullable_unsigned(page.get("total"), status, "page.total")?;
    unsigned(page.get("returned"), status, "page.returned")?;
    if !matches!(
        page.get("cursor"),
        Some(Value::Null) | Some(Value::String(_))
    ) {
        return Err(protocol(status, "page.cursor must be null or a string"));
    }
    nullable_unsigned(page.get("expires_at"), status, "page.expires_at")?;
    Ok(())
}

fn validate_success_envelope(envelope: &Value, status: u16) -> Result<(), ClientError> {
    let envelope = object(Some(envelope), status, "success envelope")?;
    string(envelope.get("binding_id"), status, "binding_id")?;
    let contract = object(envelope.get("contract"), status, "contract")?;
    string(contract.get("schema_id"), status, "contract.schema_id")?;
    if unsigned(
        contract.get("schema_revision"),
        status,
        "contract.schema_revision",
    )? == 0
    {
        return Err(protocol(
            status,
            "contract.schema_revision must be positive",
        ));
    }
    string(envelope.get("request_id"), status, "request_id")?;
    object(envelope.get("scope"), status, "scope")?;
    let outcome = object(envelope.get("outcome"), status, "outcome")?;
    let outcome_kind = string(outcome.get("outcome"), status, "outcome.outcome")?;
    let value = object(outcome.get("value"), status, "outcome.value")?;
    if !value.contains_key("payload") {
        return Err(protocol(status, "outcome.value.payload is required"));
    }
    validate_receipt(value.get("execution"), status, "outcome.value.execution")?;
    match outcome_kind {
        "evidence" => {
            object(value.get("temporal"), status, "outcome.value.temporal")?;
            object(value.get("authority"), status, "outcome.value.authority")?;
            array(
                value.get("evidence_authorities"),
                status,
                "outcome.value.evidence_authorities",
            )?;
            object(value.get("coverage"), status, "outcome.value.coverage")?;
            array(value.get("omissions"), status, "outcome.value.omissions")?;
            array(value.get("scores"), status, "outcome.value.scores")?;
            array(
                value.get("contributions"),
                status,
                "outcome.value.contributions",
            )?;
            validate_page(value.get("page"), status)?;
        }
        "preview" => {
            string(value.get("preview_id"), status, "outcome.value.preview_id")?;
            string(
                value.get("preview_digest"),
                status,
                "outcome.value.preview_digest",
            )?;
            string(
                value.get("effect_class"),
                status,
                "outcome.value.effect_class",
            )?;
            object(value.get("authority"), status, "outcome.value.authority")?;
            string(
                value.get("expected_state"),
                status,
                "outcome.value.expected_state",
            )?;
        }
        "effect" => {
            string(value.get("effect_id"), status, "outcome.value.effect_id")?;
            string(
                value.get("effect_class"),
                status,
                "outcome.value.effect_class",
            )?;
            string(
                value.get("idempotency_key"),
                status,
                "outcome.value.idempotency_key",
            )?;
            object(value.get("authority"), status, "outcome.value.authority")?;
            string(
                value.get("expected_state"),
                status,
                "outcome.value.expected_state",
            )?;
            string(
                value.get("reconciliation"),
                status,
                "outcome.value.reconciliation",
            )?;
            object(value.get("receipt"), status, "outcome.value.receipt")?;
        }
        _ => return Err(protocol(status, "outcome discriminator is not canonical")),
    }
    Ok(())
}

/// Canonical cancellation acknowledgement.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct OperationCancellation {
    pub status: CancellationStatus,
    #[serde(flatten)]
    pub details: serde_json::Map<String, Value>,
}

/// Canonical cancellation acknowledgement state.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CancellationStatus {
    Requested,
    AlreadyRequested,
    AlreadyTerminal,
}

/// Resume frontier supplied by an earlier stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamResume {
    pub token: String,
    pub next_sequence: u64,
}

/// Streaming and reconnection policy.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamOptions {
    pub resume: Option<StreamResume>,
    pub max_reconnects: usize,
}

/// One decoded SSE frame.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamEvent {
    pub event: String,
    pub id: Option<String>,
    pub data: Value,
}

impl StreamEvent {
    pub fn terminal(&self) -> bool {
        is_terminal_event(&self.event)
    }
}

/// Blocking SSE iterator with bounded, opt-in resume.
pub struct OperationStream {
    client: Client,
    operation_id: String,
    reader: Option<BufReader<Response>>,
    options: StreamOptions,
    reconnects: usize,
    next_sequence: Option<u64>,
    resume_token: Option<String>,
    terminal: bool,
    pending_event: Option<String>,
    pending_id: Option<String>,
    pending_data: Vec<String>,
}

impl OperationStream {
    fn connect(&mut self) -> Result<(), ClientError> {
        let mut url = self.client.lifecycle_url(&self.operation_id, "events")?;
        {
            let mut query = url.query_pairs_mut();
            if let Some(next_sequence) = self.next_sequence {
                query.append_pair("next_sequence", &next_sequence.to_string());
            }
            if let Some(token) = &self.resume_token {
                query.append_pair("resume_token", token);
            }
        }
        let response = self
            .client
            .http
            .get(url)
            .headers(self.client.headers("text/event-stream"))
            .send()
            .map_err(ClientError::transport)?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(ClientError::Authentication(status.as_u16()));
        }
        let media_type = media_type(response.headers());
        if !status.is_success() && media_type == Some("application/json") {
            let body: Value = response.json().map_err(|error| ClientError::Protocol {
                status: Some(status.as_u16()),
                message: format!("daemon returned malformed stream problem JSON: {error}"),
            })?;
            if body.get("kind").and_then(Value::as_str) == Some("problem") {
                return Err(problem_error(
                    &body,
                    status.as_u16(),
                    "stream problem envelope has no value",
                ));
            }
            return Err(ClientError::Protocol {
                status: Some(status.as_u16()),
                message: "daemon returned an unknown stream problem envelope".into(),
            });
        }
        if !status.is_success() || media_type != Some("text/event-stream") {
            return Err(ClientError::Protocol {
                status: Some(status.as_u16()),
                message: "daemon did not open a canonical event stream".into(),
            });
        }
        self.reader = Some(BufReader::new(response));
        Ok(())
    }

    fn read_event(&mut self) -> Result<Option<StreamEvent>, ClientError> {
        loop {
            let mut line = String::new();
            let read = self
                .reader
                .as_mut()
                .expect("stream reader is connected")
                .read_line(&mut line)
                .map_err(|error| ClientError::Transport(error.to_string()))?;
            if read == 0 {
                if self.pending_event.is_some() || !self.pending_data.is_empty() {
                    return Err(ClientError::Protocol {
                        status: None,
                        message: "event stream ended inside an SSE frame".into(),
                    });
                }
                return Ok(None);
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if self.pending_data.is_empty() {
                    self.pending_event = None;
                    continue;
                }
                let event_name = self
                    .pending_event
                    .take()
                    .unwrap_or_else(|| "message".into());
                let data_text = self.pending_data.join("\n");
                self.pending_data.clear();
                let data: Value =
                    serde_json::from_str(&data_text).map_err(|error| ClientError::Protocol {
                        status: None,
                        message: format!("SSE data is not JSON: {error}"),
                    })?;
                if data.get("event").and_then(Value::as_str) != Some(event_name.as_str()) {
                    return Err(ClientError::Protocol {
                        status: None,
                        message: "SSE event name disagrees with its JSON payload".into(),
                    });
                }
                let id = self.pending_id.take();
                if event_name == "open" {
                    if id.is_some() {
                        return Err(ClientError::Protocol {
                            status: None,
                            message: "SSE open event must not carry an ID".into(),
                        });
                    }
                    let open_data =
                        data.get("data").and_then(Value::as_object).ok_or_else(|| {
                            ClientError::Protocol {
                                status: None,
                                message: "SSE open data is malformed".into(),
                            }
                        })?;
                    if open_data.get("correlation_id").and_then(Value::as_str)
                        != Some(self.operation_id.as_str())
                    {
                        return Err(ClientError::Protocol {
                            status: None,
                            message: "SSE open correlation identity does not match operation"
                                .into(),
                        });
                    }
                    let frontier = open_data
                        .get("frontier")
                        .and_then(Value::as_object)
                        .ok_or_else(|| ClientError::Protocol {
                            status: None,
                            message: "SSE open frontier is malformed".into(),
                        })?;
                    let next_sequence = frontier
                        .get("next_sequence")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| ClientError::Protocol {
                            status: None,
                            message: "SSE open frontier has no next sequence".into(),
                        })?;
                    let retained = frontier
                        .get("retained_from_sequence")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| ClientError::Protocol {
                            status: None,
                            message: "SSE open frontier has no retained sequence".into(),
                        })?;
                    if retained > next_sequence {
                        return Err(ClientError::Protocol {
                            status: None,
                            message: "SSE open frontier sequence range is inconsistent".into(),
                        });
                    }
                    self.next_sequence = Some(next_sequence);
                    self.resume_token = match frontier.get("resume_token") {
                        Some(Value::Null) => None,
                        Some(Value::String(token)) if !token.is_empty() => Some(token.clone()),
                        _ => {
                            return Err(ClientError::Protocol {
                                status: None,
                                message: "SSE open frontier resume token is malformed".into(),
                            });
                        }
                    };
                } else {
                    if !matches!(event_name.as_str(), "item" | "progress" | "resume_gap")
                        && !is_terminal_event(&event_name)
                    {
                        return Err(ClientError::Protocol {
                            status: None,
                            message: "SSE event name is not canonical".into(),
                        });
                    }
                    let event_data =
                        data.get("data").and_then(Value::as_object).ok_or_else(|| {
                            ClientError::Protocol {
                                status: None,
                                message: "SSE event data is malformed".into(),
                            }
                        })?;
                    let sequence = event_data
                        .get("sequence")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| ClientError::Protocol {
                            status: None,
                            message: "SSE event has no canonical sequence".into(),
                        })?;
                    if id.as_deref() != Some(sequence.to_string().as_str()) {
                        return Err(ClientError::Protocol {
                            status: None,
                            message: "SSE ID disagrees with its canonical sequence".into(),
                        });
                    }
                    if self
                        .next_sequence
                        .is_some_and(|expected| expected != sequence)
                    {
                        return Err(ClientError::Protocol {
                            status: None,
                            message: "SSE event sequence disagrees with the stream frontier".into(),
                        });
                    }
                    match event_name.as_str() {
                        "item" if !event_data.contains_key("item") => {
                            return Err(ClientError::Protocol {
                                status: None,
                                message: "SSE item payload is missing".into(),
                            });
                        }
                        "progress"
                            if event_data
                                .get("completed")
                                .and_then(Value::as_u64)
                                .is_none()
                                || !matches!(event_data.get("total"), Some(Value::Null))
                                    && event_data
                                        .get("total")
                                        .and_then(Value::as_u64)
                                        .is_none() =>
                        {
                            return Err(ClientError::Protocol {
                                status: None,
                                message: "SSE progress payload is malformed".into(),
                            });
                        }
                        "resume_gap" => validate_resume_gap(event_data)?,
                        event if is_terminal_event(event) => {
                            validate_terminal(event_data, event)?;
                        }
                        _ => {}
                    }
                    self.next_sequence =
                        Some(
                            sequence
                                .checked_add(1)
                                .ok_or_else(|| ClientError::Protocol {
                                    status: None,
                                    message: "SSE sequence overflowed".into(),
                                })?,
                        );
                }
                let event = StreamEvent {
                    event: event_name,
                    id,
                    data,
                };
                self.terminal = event.terminal();
                return Ok(Some(event));
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = line.split_once(':').map_or((line, ""), |(field, value)| {
                (field, value.strip_prefix(' ').unwrap_or(value))
            });
            match field {
                "event" => self.pending_event = Some(value.to_owned()),
                "id" if !value.contains('\0') => self.pending_id = Some(value.to_owned()),
                "data" => self.pending_data.push(value.to_owned()),
                _ => {}
            }
        }
    }
}

fn validate_resume_gap(event_data: &serde_json::Map<String, Value>) -> Result<(), ClientError> {
    let gap = event_data
        .get("gap")
        .and_then(Value::as_object)
        .ok_or_else(|| ClientError::Protocol {
            status: None,
            message: "SSE resume gap is malformed".into(),
        })?;
    let first = gap
        .get("first_missing_sequence")
        .and_then(Value::as_u64)
        .ok_or_else(|| ClientError::Protocol {
            status: None,
            message: "SSE resume gap has no first sequence".into(),
        })?;
    let last = gap
        .get("last_missing_sequence")
        .and_then(Value::as_u64)
        .ok_or_else(|| ClientError::Protocol {
            status: None,
            message: "SSE resume gap has no last sequence".into(),
        })?;
    let frontier = gap
        .get("frontier")
        .and_then(Value::as_object)
        .ok_or_else(|| ClientError::Protocol {
            status: None,
            message: "SSE resume gap frontier is malformed".into(),
        })?;
    let next = frontier
        .get("next_sequence")
        .and_then(Value::as_u64)
        .ok_or_else(|| ClientError::Protocol {
            status: None,
            message: "SSE resume gap frontier has no next sequence".into(),
        })?;
    let retained = frontier
        .get("retained_from_sequence")
        .and_then(Value::as_u64)
        .ok_or_else(|| ClientError::Protocol {
            status: None,
            message: "SSE resume gap frontier has no retained sequence".into(),
        })?;
    let token_valid = matches!(
        frontier.get("resume_token"),
        Some(Value::Null | Value::String(_))
    );
    if first > last || retained > next || !token_valid {
        return Err(ClientError::Protocol {
            status: None,
            message: "SSE resume gap range or frontier is malformed".into(),
        });
    }
    Ok(())
}

fn validate_terminal(
    event_data: &serde_json::Map<String, Value>,
    event_name: &str,
) -> Result<(), ClientError> {
    let terminal = event_data
        .get("terminal")
        .and_then(Value::as_object)
        .ok_or_else(|| ClientError::Protocol {
            status: None,
            message: "SSE terminal payload is malformed".into(),
        })?;
    if terminal.get("termination").and_then(Value::as_str) != Some(event_name) {
        return Err(ClientError::Protocol {
            status: None,
            message: "SSE terminal outcome disagrees with its event".into(),
        });
    }
    let receipt_termination = validate_receipt(terminal.get("receipt"), 200, "terminal.receipt")?;
    if receipt_termination != event_name {
        return Err(ClientError::Protocol {
            status: None,
            message: "SSE terminal receipt disagrees with its event".into(),
        });
    }
    Ok(())
}

impl Iterator for OperationStream {
    type Item = Result<StreamEvent, ClientError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.terminal {
            return None;
        }
        loop {
            match self.read_event() {
                Ok(Some(event)) => return Some(Ok(event)),
                Ok(None) if self.reconnects < self.options.max_reconnects => {
                    if self.next_sequence.is_none() || self.resume_token.is_none() {
                        return Some(Err(ClientError::Protocol {
                            status: None,
                            message: "stream ended without a resumable frontier".into(),
                        }));
                    }
                    self.reconnects += 1;
                    if let Err(error) = self.connect() {
                        return Some(Err(error));
                    }
                }
                Ok(None) => {
                    return Some(Err(ClientError::Transport(
                        "event stream ended before a terminal event".into(),
                    )));
                }
                Err(error) => return Some(Err(error)),
            }
        }
    }
}

/// Parsed canonical application problem.
#[derive(Clone, Debug, PartialEq)]
pub struct ProblemError {
    pub status: u16,
    pub kind: String,
    pub code: String,
    pub message: String,
    pub retry: String,
    pub envelope: Value,
}

impl ProblemError {
    fn new(status: u16, envelope: Value) -> Result<Self, ClientError> {
        let envelope_object = object(Some(&envelope), status, "problem envelope")?;
        let contract = object(envelope_object.get("contract"), status, "problem contract")?;
        string(
            contract.get("schema_id"),
            status,
            "problem contract.schema_id",
        )?;
        if unsigned(
            contract.get("schema_revision"),
            status,
            "problem contract.schema_revision",
        )? == 0
        {
            return Err(protocol(
                status,
                "problem contract revision must be positive",
            ));
        }
        let request_id = string(
            envelope_object.get("request_id"),
            status,
            "problem request_id",
        )?;
        let problem = object(envelope_object.get("problem"), status, "problem record")?;
        if unsigned(problem.get("revision"), status, "problem.revision")? == 0 {
            return Err(protocol(status, "problem revision must be positive"));
        }
        let kind = string(problem.get("kind"), status, "problem.kind")?;
        if !matches!(
            kind,
            "invalid_request"
                | "not_found_or_not_authorized"
                | "conflict"
                | "stale"
                | "unsupported"
                | "unavailable"
                | "saturated"
                | "cancelled"
                | "timed_out"
        ) {
            return Err(protocol(status, "problem kind is not canonical"));
        }
        let code = string(problem.get("code"), status, "problem.code")?;
        let message = string(problem.get("message"), status, "problem.message")?;
        match problem.get("diagnostic") {
            Some(Value::Null) => {}
            Some(value) => validate_diagnostic(value, status, "problem.diagnostic")?,
            None => return Err(protocol(status, "problem.diagnostic is required")),
        }
        string(problem.get("owning_layer"), status, "problem.owning_layer")?;
        string(problem.get("terminality"), status, "problem.terminality")?;
        let retryable = problem
            .get("retryable")
            .and_then(Value::as_bool)
            .ok_or_else(|| protocol(status, "problem.retryable must be a boolean"))?;
        let retry = string(problem.get("retry"), status, "problem.retry")?;
        if !matches!(
            retry,
            "never" | "same_request" | "after_delay" | "after_revalidate" | "after_reconcile"
        ) {
            return Err(protocol(status, "problem retry directive is not canonical"));
        }
        if !matches!(
            problem.get("retry_scope"),
            Some(Value::Null | Value::String(_))
        ) {
            return Err(protocol(
                status,
                "problem.retry_scope must be null or a string",
            ));
        }
        let retry_after = match problem.get("retry_after_millis") {
            Some(Value::Null) => None,
            Some(value) => Some(unsigned(Some(value), status, "problem.retry_after_millis")?),
            None => return Err(protocol(status, "problem.retry_after_millis is required")),
        };
        if !matches!(
            problem.get("cancellation_stage"),
            Some(Value::Null | Value::String(_))
        ) {
            return Err(protocol(
                status,
                "problem.cancellation_stage must be null or a string",
            ));
        }
        if string(problem.get("request_id"), status, "problem.request_id")? != request_id {
            return Err(protocol(status, "problem request identity is inconsistent"));
        }
        string(problem.get("trace_id"), status, "problem.trace_id")?;
        let details = problem
            .get("details")
            .and_then(Value::as_array)
            .ok_or_else(|| protocol(status, "problem.details must be an array"))?;
        for detail in details {
            validate_diagnostic(detail, status, "problem.details item")?;
        }
        let legal_actions = problem
            .get("legal_actions")
            .and_then(Value::as_array)
            .ok_or_else(|| protocol(status, "problem.legal_actions must be an array"))?;
        if legal_actions.iter().any(|action| action.as_str().is_none()) {
            return Err(protocol(
                status,
                "problem.legal_actions must contain strings",
            ));
        }
        if !problem.contains_key("coverage") {
            return Err(protocol(status, "problem.coverage is required"));
        }
        if retryable != (retry != "never") {
            return Err(protocol(
                status,
                "problem retryable and retry directive are inconsistent",
            ));
        }
        if (retry == "after_delay") != retry_after.is_some() {
            return Err(protocol(
                status,
                "problem retry delay is inconsistent with its directive",
            ));
        }
        Ok(Self {
            status,
            kind: kind.to_owned(),
            code: code.to_owned(),
            message: message.to_owned(),
            retry: retry.to_owned(),
            envelope,
        })
    }
}

fn validate_diagnostic(value: &Value, status: u16, field: &str) -> Result<(), ClientError> {
    let diagnostic = object(Some(value), status, field)?;
    string(diagnostic.get("code"), status, &format!("{field}.code"))?;
    string(
        diagnostic.get("message"),
        status,
        &format!("{field}.message"),
    )?;
    Ok(())
}

impl fmt::Display for ProblemError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}: {}", self.kind, self.code, self.message)
    }
}

/// Transport, authentication, protocol, or canonical application failure.
#[derive(Clone, Debug, PartialEq)]
pub enum ClientError {
    InvalidConfiguration(String),
    InvalidRequest(String),
    Transport(String),
    Authentication(u16),
    Protocol {
        status: Option<u16>,
        message: String,
    },
    Problem(Box<ProblemError>),
}

impl ClientError {
    fn transport(error: reqwest::Error) -> Self {
        Self::Transport(error.to_string())
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfiguration(message) => {
                write!(formatter, "invalid client configuration: {message}")
            }
            Self::InvalidRequest(message) => write!(formatter, "invalid request: {message}"),
            Self::Transport(message) => write!(formatter, "transport failure: {message}"),
            Self::Authentication(status) => {
                write!(formatter, "daemon authentication failed with HTTP {status}")
            }
            Self::Protocol { message, .. } => write!(formatter, "protocol failure: {message}"),
            Self::Problem(problem) => problem.fmt(formatter),
        }
    }
}

impl Error for ClientError {}

fn validate_opaque(value: &str, maximum: usize, field: &str) -> Result<(), ClientError> {
    let valid = !value.is_empty()
        && value.trim() == value
        && value.len() <= maximum
        && !value.chars().any(char::is_control)
        && !value.contains('/');
    if valid {
        Ok(())
    } else {
        Err(ClientError::InvalidRequest(format!(
            "{field} is not a canonical opaque value"
        )))
    }
}
