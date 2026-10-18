//! Blocking local/remote client for the canonical HTTP and SSE lifecycle.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::io::Read;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::blocking::{Client as HttpClient, Response};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, ORIGIN};
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tracedecay_tool_catalog::{
    CancellationPoint, ReceiptContract, ReconciliationContract, SdkTransportBindingV1,
};

use crate::operations::{OperationTransport, TypedOperation};
pub use crate::request_control::OperationRequestOptions;
use crate::stream::{self, Action, ConnectClass, StreamSession};

pub(crate) const MAX_OPAQUE_BYTES: usize = 4_096;
pub(crate) const MAX_REQUEST_ID_BYTES: usize = 512;
/// Selects loopback or remote HTTP policy without changing operation semantics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionMode {
    Local(ConnectionSettings),
    Remote(ConnectionSettings),
}

/// Shared authority settings for either connection mode.
#[derive(Clone, PartialEq, Eq)]
pub struct ConnectionSettings {
    base_url: String,
    project_id: String,
    token: String,
}

/// Diagnostics keep the endpoint and project but never the bearer token, and
/// URL userinfo is stripped so an accepted `user:secret@host` cannot leak.
impl fmt::Debug for ConnectionSettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let base_url = reqwest::Url::parse(&self.base_url)
            .ok()
            .and_then(|mut url| {
                url.set_username("").ok()?;
                url.set_password(None).ok()?;
                Some(url.to_string())
            })
            .unwrap_or_else(|| "[invalid URL]".to_owned());
        formatter
            .debug_struct("ConnectionSettings")
            .field("base_url", &base_url)
            .field("project_id", &self.project_id)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

/// Caller-owned bridge used by generated MCP-backed SDK operations.
///
/// The SDK owns canonical request/result types and selects the tool name from
/// its executable binding. Hosts own connection lifecycle and MCP framing.
pub trait McpToolTransport: fmt::Debug + Send + Sync {
    fn call_tool(&self, tool_name: &str, request: &Value) -> Result<Value, ClientError>;
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
    mcp_transport: Option<Arc<dyn McpToolTransport>>,
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

    /// Attach the caller's MCP transport for generated MCP tool operations.
    pub fn mcp_transport(mut self, transport: Arc<dyn McpToolTransport>) -> Self {
        self.mcp_transport = Some(transport);
        self
    }

    pub fn build(self) -> Result<Client, ClientError> {
        let endpoint = self.endpoint()?;
        let mut builder = HttpClient::builder().timeout(self.timeout);
        if matches!(&self.mode, ConnectionMode::Local(_)) {
            // Ambient proxies must never receive local daemon credentials.
            builder = builder.no_proxy();
        }
        let http = builder.build().map_err(ClientError::transport)?;
        Ok(Client {
            http,
            endpoint,
            mcp_transport: self.mcp_transport,
        })
    }

    /// Build the asynchronous client for the operation-event stream. It has no
    /// total request timeout, because a healthy stream outlives any fixed
    /// deadline; `timeout` bounds connection establishment.
    pub fn build_async_stream(self) -> Result<crate::async_stream::AsyncStreamClient, ClientError> {
        let endpoint = self.endpoint()?;
        let mut builder = reqwest::Client::builder().connect_timeout(self.timeout);
        if matches!(&self.mode, ConnectionMode::Local(_)) {
            builder = builder.no_proxy();
        }
        let http = builder.build().map_err(ClientError::transport)?;
        Ok(crate::async_stream::AsyncStreamClient::new(http, endpoint))
    }

    fn endpoint(&self) -> Result<Endpoint, ClientError> {
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
        let origin = self.origin.clone().unwrap_or(default_origin);
        let origin = HeaderValue::from_str(&origin)
            .map_err(|error| ClientError::InvalidConfiguration(error.to_string()))?;
        let mut authorization = HeaderValue::from_str(&format!("Bearer {}", settings.token))
            .map_err(|error| ClientError::InvalidConfiguration(error.to_string()))?;
        authorization.set_sensitive(true);
        let application_root = with_path_segments(
            base,
            ["projects", settings.project_id.as_str(), "application"],
        )?;
        Ok(Endpoint {
            application_root,
            authorization,
            origin,
        })
    }
}

/// The validated daemon application endpoint and credentials, shared by the
/// blocking and asynchronous clients.
#[derive(Clone, Debug)]
pub(crate) struct Endpoint {
    application_root: reqwest::Url,
    authorization: HeaderValue,
    origin: HeaderValue,
}

impl Endpoint {
    pub(crate) fn lifecycle_url(
        &self,
        operation_id: &str,
        suffix: &str,
    ) -> Result<reqwest::Url, ClientError> {
        with_path_segments(
            self.application_root.clone(),
            ["operations", operation_id, suffix],
        )
    }

    /// The events URL carrying the stream's resume frontier.
    pub(crate) fn events_url(&self, session: &StreamSession) -> Result<reqwest::Url, ClientError> {
        let mut url = self.lifecycle_url(session.operation_id(), "events")?;
        let (next_sequence, resume_token) = session.resume_query();
        {
            let mut query = url.query_pairs_mut();
            if let Some(next_sequence) = next_sequence {
                query.append_pair("next_sequence", &next_sequence.to_string());
            }
            if let Some(token) = resume_token {
                query.append_pair("resume_token", token);
            }
        }
        Ok(url)
    }

    pub(crate) fn headers(&self, accept: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static(accept));
        headers.insert(AUTHORIZATION, self.authorization.clone());
        headers.insert(ORIGIN, self.origin.clone());
        headers
    }
}

/// Blocking lifecycle client. Clone it to share the connection pool.
#[derive(Clone, Debug)]
pub struct Client {
    http: HttpClient,
    endpoint: Endpoint,
    mcp_transport: Option<Arc<dyn McpToolTransport>>,
}

impl Client {
    pub fn builder(mode: ConnectionMode) -> ClientBuilder {
        ClientBuilder {
            mode,
            origin: None,
            timeout: Duration::from_secs(30),
            mcp_transport: None,
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

    /// Invoke one generated MCP tool operation through the caller-owned bridge.
    ///
    /// The MCP host owns admission, deadline, and cancellation framing, so the
    /// typed HTTP lifecycle checks do not apply here: the transport returns the
    /// operation's typed result payload directly.
    pub fn execute_mcp<Operation>(
        &self,
        request: &Operation::Request,
    ) -> Result<Operation::Result, ClientError>
    where
        Operation: TypedOperation,
        Operation::Request: Serialize,
        Operation::Result: DeserializeOwned,
    {
        crate::observe::mcp_execute(|| {
            let request = serde_json::to_value(request).map_err(|error| {
                statusless_protocol(format!("typed request could not be encoded: {error}"))
            })?;
            let OperationTransport::McpTool { tool_name } = Operation::TRANSPORT else {
                return Err(ClientError::UnsupportedTransport {
                    operation_id: Operation::OPERATION_ID,
                    transport: "http",
                });
            };
            let transport =
                self.mcp_transport
                    .as_ref()
                    .ok_or(ClientError::MissingMcpTransport {
                        operation_id: Operation::OPERATION_ID,
                    })?;
            let response = transport.call_tool(tool_name, &request)?;
            serde_json::from_value(response).map_err(|error| {
                statusless_protocol(format!(
                    "MCP tool {tool_name} returned a malformed {} result: {error}",
                    Operation::OPERATION_ID
                ))
            })
        })
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
        crate::observe::http_execute(|| {
            let request = serde_json::to_value(request).map_err(|error| {
                statusless_protocol(format!("typed request could not be encoded: {error}"))
            })?;
            let OperationTransport::Http { route } = Operation::TRANSPORT else {
                return Err(ClientError::UnsupportedTransport {
                    operation_id: Operation::OPERATION_ID,
                    transport: "mcp_tool",
                });
            };
            let expected_request_id = crate::request_control::admit::<Operation>(&options)?;
            let response = self.request_route(
                Operation::OPERATION_ID,
                Operation::BINDING_ID,
                route,
                &request,
                options,
                expected_request_id.as_ref(),
            )?;
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
                return Err(protocol(
                    response.status(),
                    format!(
                        "daemon returned mismatched contracts for {}",
                        Operation::OPERATION_ID
                    ),
                ));
            }
            let outcome = response.envelope().get("outcome").ok_or_else(|| {
                protocol(
                    response.status(),
                    format!("daemon omitted the {} outcome", Operation::OPERATION_ID),
                )
            })?;
            let outcome_kind = outcome
                .get("outcome")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    protocol(
                        response.status(),
                        format!(
                            "daemon omitted the {} outcome kind",
                            Operation::OPERATION_ID
                        ),
                    )
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
                return Err(protocol(
                    response.status(),
                    format!(
                        "daemon returned outcome {outcome_kind} outside the {} receipt contract",
                        Operation::OPERATION_ID
                    ),
                ));
            }
            let execution = outcome
                .get("value")
                .and_then(|value| value.get("execution"))
                .ok_or_else(|| {
                    protocol(
                        response.status(),
                        format!(
                            "daemon omitted the {} execution receipt",
                            Operation::OPERATION_ID
                        ),
                    )
                })?;
            let termination = execution
                .get("termination")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    protocol(
                        response.status(),
                        format!(
                            "daemon omitted the {} terminal state",
                            Operation::OPERATION_ID
                        ),
                    )
                })?;
            if !Operation::TERMINAL_STATES
                .iter()
                .copied()
                .any(|state| state.as_str() == termination)
            {
                return Err(protocol(
                    response.status(),
                    format!(
                        "daemon returned terminal state {termination} outside the {} contract",
                        Operation::OPERATION_ID
                    ),
                ));
            }
            if let Some(cancellation) = execution.get("cancellation")
                && !cancellation.is_null()
            {
                if !Operation::CANCELLABLE {
                    return Err(protocol(
                        response.status(),
                        format!(
                            "daemon returned cancellation evidence for non-cancellable {}",
                            Operation::OPERATION_ID
                        ),
                    ));
                }
                let stage = cancellation
                    .get("stage")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        protocol(
                            response.status(),
                            format!(
                                "daemon returned malformed cancellation evidence for {}",
                                Operation::OPERATION_ID
                            ),
                        )
                    })?;
                if !Operation::CANCELLATION_POINTS
                    .iter()
                    .copied()
                    .any(|point| cancellation_point_name(point) == stage)
                {
                    return Err(protocol(
                        response.status(),
                        format!(
                            "daemon returned cancellation stage {stage} outside the {} contract",
                            Operation::OPERATION_ID
                        ),
                    ));
                }
            }
            let payload = response.payload().ok_or_else(|| {
                protocol(
                    response.status(),
                    format!(
                        "daemon omitted the {} result payload",
                        Operation::OPERATION_ID
                    ),
                )
            })?;
            let request_id = response
                .envelope()
                .get("request_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    protocol(
                        response.status(),
                        "daemon omitted the application request ID",
                    )
                })?
                .to_owned();
            if !crate::semantic::response_matches(
                Operation::RESULT_SEMANTICS,
                &request_id,
                expected_request_id.as_ref(),
                &request,
                payload,
            ) {
                return Err(protocol(
                    response.status(),
                    format!(
                        "daemon returned a semantically invalid {} result",
                        Operation::OPERATION_ID
                    ),
                ));
            }
            let result = Operation::Result::deserialize(payload).map_err(|error| {
                protocol(
                    response.status(),
                    format!(
                        "daemon returned a malformed {} result: {error}",
                        Operation::OPERATION_ID
                    ),
                )
            })?;
            Ok(TypedResponse {
                request_id,
                result,
                envelope: response.envelope,
            })
        })
    }

    fn request_route(
        &self,
        operation_id: &str,
        binding_id: &str,
        route: &str,
        request: &Value,
        options: OperationRequestOptions,
        expected_request_id: Option<&tracedecay_contracts::RequestId>,
    ) -> Result<ApplicationResponse, ClientError> {
        admit_canonical_sdk_http_binding(operation_id, binding_id, route)?;
        let route = route.strip_prefix("/application").ok_or_else(|| {
            ClientError::InvalidConfiguration(
                "typed operation route must begin with /application".into(),
            )
        })?;
        crate::observe::finish((|| {
            let url = with_path_segments(
                self.endpoint.application_root.clone(),
                route.split('/').filter(|segment| !segment.is_empty()),
            )?;
            let mut headers = self.endpoint.headers("application/json");
            crate::request_control::apply_http_headers(&mut headers, options)?;
            let response = crate::observe::headers(|| {
                self.http
                    .post(url)
                    .headers(headers)
                    .json(request)
                    .send()
                    .map_err(ClientError::transport)
            })?;
            crate::observe::body_decode(|| {
                self.decode_application_response(response, expected_request_id)
            })
        })())
    }

    /// Requests cancellation of a previously accepted operation.
    pub fn cancel_operation(
        &self,
        operation_id: &str,
    ) -> Result<OperationCancellation, ClientError> {
        validate_opaque(operation_id, MAX_REQUEST_ID_BYTES, "operation ID")?;
        crate::observe::finish((|| {
            let response = crate::observe::headers(|| {
                self.http
                    .post(self.endpoint.lifecycle_url(operation_id, "cancel")?)
                    .headers(self.endpoint.headers("application/json"))
                    .send()
                    .map_err(ClientError::transport)
            })?;
            let status = response.status();
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                return Err(ClientError::Authentication(status.as_u16()));
            }
            let body: Value = crate::observe::body_decode(|| {
                response.json().map_err(|error| {
                    protocol(
                        status.as_u16(),
                        format!("daemon returned malformed cancellation JSON: {error}"),
                    )
                })
            })?;
            if body.get("kind").and_then(Value::as_str) == Some("problem") {
                return Err(problem_error(
                    &body,
                    status.as_u16(),
                    "cancellation problem envelope has no value",
                ));
            }
            let value: OperationCancellation = serde_json::from_value(body).map_err(|error| {
                protocol(
                    status.as_u16(),
                    format!("daemon returned malformed cancellation JSON: {error}"),
                )
            })?;
            let valid_status = matches!(
                (status, value.status),
                (StatusCode::ACCEPTED, CancellationStatus::Requested)
                    | (StatusCode::OK, CancellationStatus::AlreadyRequested)
                    | (StatusCode::OK, CancellationStatus::AlreadyTerminal)
            );
            if !valid_status {
                return Err(protocol(
                    status.as_u16(),
                    "daemon returned a non-canonical cancellation response",
                ));
            }
            Ok(value)
        })())
    }

    /// Opens the canonical event stream, optionally from a resume frontier.
    pub fn stream_operation(
        &self,
        operation_id: &str,
        options: StreamOptions,
    ) -> Result<OperationStream, ClientError> {
        crate::observe::finish((|| {
            let mut stream = OperationStream {
                client: self.clone(),
                session: StreamSession::new(operation_id, &options)?,
                body: None,
            };
            // The first connection is not retried: its failure is the call's.
            match stream.connect() {
                Ok(()) => stream.session.connected(),
                Err(error) => return Err(error),
            }
            Ok(stream)
        })())
    }

    fn decode_application_response(
        &self,
        response: Response,
        expected_request_id: Option<&tracedecay_contracts::RequestId>,
    ) -> Result<ApplicationResponse, ClientError> {
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(ClientError::Authentication(status.as_u16()));
        }
        if media_type(response.headers()) != Some("application/json") {
            return Err(protocol(
                status.as_u16(),
                "daemon response is not application/json",
            ));
        }
        let mut body: Value = response.json().map_err(|error| {
            protocol(
                status.as_u16(),
                format!("daemon returned malformed application JSON: {error}"),
            )
        })?;
        match body.get("kind").and_then(Value::as_str) {
            Some("success") if status.is_success() => {
                let value = body
                    .get_mut("value")
                    .map(Value::take)
                    .ok_or_else(|| protocol(status.as_u16(), "success envelope has no value"))?;
                ApplicationResponse::new(value, status.as_u16())
            }
            Some("problem") if !status.is_success() => {
                let response_request_id = body
                    .get("value")
                    .and_then(|value| value.get("request_id"))
                    .and_then(Value::as_str);
                if expected_request_id
                    .is_some_and(|expected| response_request_id != Some(expected.as_str()))
                {
                    return Err(protocol(
                        status.as_u16(),
                        "daemon returned a different application request ID",
                    ));
                }
                Err(problem_error(
                    &body,
                    status.as_u16(),
                    "problem envelope has no value",
                ))
            }
            _ => Err(protocol(
                status.as_u16(),
                "daemon returned an inconsistent HTTP envelope",
            )),
        }
    }
}

type CanonicalSdkHttpBindings = BTreeMap<String, (String, String)>;

fn admit_canonical_sdk_http_binding(
    operation_id: &str,
    binding_id: &str,
    route: &str,
) -> Result<(), ClientError> {
    static BINDINGS: OnceLock<Result<CanonicalSdkHttpBindings, String>> = OnceLock::new();
    let bindings = BINDINGS
        .get_or_init(|| {
            let registry = tracedecay_contracts::sdk_executable_binding_registry()
                .map_err(|error| error.to_string())?;
            Ok(registry
                .iter()
                .filter_map(|availability| {
                    let binding = availability.binding()?;
                    let SdkTransportBindingV1::Http { route_path } = binding.transport() else {
                        return None;
                    };
                    Some((
                        binding.operation_id().as_str().to_owned(),
                        (
                            binding.binding_id().as_str().to_owned(),
                            route_path.to_owned(),
                        ),
                    ))
                })
                .collect())
        })
        .as_ref()
        .map_err(|error| {
            ClientError::InvalidConfiguration(format!(
                "canonical SDK registry is unavailable: {error}"
            ))
        })?;
    let admitted =
        bindings
            .get(operation_id)
            .is_some_and(|(canonical_binding_id, canonical_route)| {
                canonical_binding_id == binding_id && canonical_route == route
            });
    if admitted {
        Ok(())
    } else {
        Err(ClientError::InvalidConfiguration(format!(
            "{operation_id} is not an HTTP binding in the canonical SDK registry"
        )))
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
pub(crate) fn media_type(headers: &HeaderMap) -> Option<&str> {
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
pub(crate) fn problem_error(body: &Value, status: u16, missing_value: &'static str) -> ClientError {
    let Some(envelope) = body.get("value").cloned() else {
        return protocol(status, missing_value);
    };
    match ProblemError::new(status, envelope) {
        Ok(problem) => ClientError::Problem(Box::new(problem)),
        Err(error) => error,
    }
}

pub(crate) fn protocol(status: u16, message: impl Into<String>) -> ClientError {
    ClientError::Protocol {
        status: Some(status),
        message: message.into(),
    }
}

/// A protocol violation detected outside an HTTP status (request encoding,
/// MCP results, and SSE framing).
pub(crate) fn statusless_protocol(message: impl Into<String>) -> ClientError {
    ClientError::Protocol {
        status: None,
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

pub(crate) fn validate_receipt<'a>(
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
    /// Reconnect attempts allowed after the first connection. Each attempt,
    /// including one that fails to connect, spends this budget, and only
    /// transport failures are retried. After the budget is spent the iterator
    /// yields one `Err` and then `None`.
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
        stream::is_terminal_event(&self.event)
    }
}

/// Blocking SSE iterator with bounded frames and bounded, opt-in resume.
///
/// Framing, validation, and the reconnect policy live in
/// [`crate::stream::StreamSession`]; this type only performs the blocking I/O
/// it asks for. Each `next` call blocks its thread on the network, so a
/// Node-style event loop must use [`crate::async_stream`] instead.
pub struct OperationStream {
    client: Client,
    session: StreamSession,
    body: Option<Response>,
}

impl OperationStream {
    fn connect(&mut self) -> Result<(), ClientError> {
        let url = self.client.endpoint.events_url(&self.session)?;
        let response = crate::observe::headers(|| {
            self.client
                .http
                .get(url)
                .headers(self.client.endpoint.headers("text/event-stream"))
                .send()
                .map_err(ClientError::transport)
        })?;
        let status = response.status().as_u16();
        match stream::classify_connect(status, media_type(response.headers())) {
            ConnectClass::Stream => {
                self.body = Some(response);
                Ok(())
            }
            ConnectClass::ProblemBody => {
                let body = crate::observe::body_decode(|| {
                    response.bytes().map_err(ClientError::transport)
                })?;
                Err(stream::problem_from_body(status, &body))
            }
            ConnectClass::Refused(error) => Err(error),
        }
    }
}

impl Iterator for OperationStream {
    type Item = Result<StreamEvent, ClientError>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            let action = self.session.next_action();
            if !self.session.is_open() {
                self.body = None;
            }
            match action {
                Action::Deliver(item) => return Some(item),
                Action::Done => return None,
                Action::Connect { delay } => {
                    if !delay.is_zero() {
                        std::thread::sleep(delay);
                    }
                    match self.connect() {
                        Ok(()) => self.session.connected(),
                        Err(error) => self.session.connect_failed(error),
                    }
                }
                Action::Read => {
                    let read = match self.body.as_mut() {
                        Some(body) => body.read(&mut buffer),
                        None => Ok(0),
                    };
                    match read {
                        Ok(0) => self.session.end_of_body(),
                        Ok(read) => self.session.chunk(&buffer[..read]),
                        Err(error) => self
                            .session
                            .read_failed(ClientError::Transport(error.to_string())),
                    }
                }
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
        let mut canonical_envelope = envelope.clone();
        if let Some(object) = canonical_envelope.as_object_mut() {
            object.remove("binding_id");
        }
        let decoded: tracedecay_contracts::ApplicationProblemEnvelope =
            serde_json::from_value(canonical_envelope)
                .map_err(|error| protocol(status, format!("invalid problem envelope: {error}")))?;
        if !status_matches_problem(status, decoded.problem.kind()) {
            return Err(protocol(
                status,
                "HTTP status does not match the canonical application problem",
            ));
        }
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
                | "partial_effect"
                | "stale"
                | "unsupported"
                | "unavailable"
                | "execution_failed"
                | "reset_required"
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
        let terminality = string(problem.get("terminality"), status, "problem.terminality")?;
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
        let cancellation_stage = nullable_canonical_string(
            problem.get("cancellation_stage"),
            status,
            "problem.cancellation_stage",
            &[
                "before_admission",
                "before_read",
                "during_read",
                "before_effect",
                "effect_in_flight",
            ],
        )?;
        let unavailable_classification = nullable_canonical_string(
            problem.get("unavailable_classification"),
            status,
            "problem.unavailable_classification",
            &[
                "authority",
                "backend_unavailable",
                "backend_disconnected",
                "backend_retryable",
            ],
        )?;
        let execution_failure_classification = nullable_canonical_string(
            problem.get("execution_failure_classification"),
            status,
            "problem.execution_failure_classification",
            &["denied", "malformed_output", "permanent"],
        )?;
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
        let has_committed_receipt =
            matches!(problem.get("committed_receipt"), Some(value) if !value.is_null());
        let admitted_stage = cancellation_stage.is_some_and(|stage| stage != "before_admission");
        let admitted_unavailable =
            unavailable_classification.is_some_and(|classification| classification != "authority");
        let expected_admitted = matches!(
            kind,
            "partial_effect" | "reset_required" | "execution_failed"
        ) || admitted_stage
            || admitted_unavailable;
        if terminality
            != if expected_admitted {
                "admitted_terminal"
            } else {
                "pre_admission"
            }
            || (kind == "partial_effect") != has_committed_receipt
            || (kind == "unavailable") != unavailable_classification.is_some()
            || (kind == "execution_failed") != execution_failure_classification.is_some()
            || matches!(kind, "cancelled" | "timed_out") != cancellation_stage.is_some()
        {
            return Err(protocol(
                status,
                "problem terminal identity is inconsistent",
            ));
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

fn status_matches_problem(status: u16, kind: tracedecay_contracts::ApplicationProblemKind) -> bool {
    status == kind.http_status()
}

fn nullable_canonical_string<'a>(
    value: Option<&'a Value>,
    status: u16,
    field: &str,
    allowed: &[&str],
) -> Result<Option<&'a str>, ClientError> {
    match value {
        Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if allowed.contains(&value.as_str()) => Ok(Some(value)),
        _ => Err(protocol(status, format!("{field} is not canonical"))),
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
    MissingMcpTransport {
        operation_id: &'static str,
    },
    UnsupportedTransport {
        operation_id: &'static str,
        transport: &'static str,
    },
    Authentication(u16),
    Protocol {
        status: Option<u16>,
        message: String,
    },
    /// The peer sent an SSE line or frame larger than the SDK retains before
    /// decoding; the stream is closed without delivery or frontier advance.
    StreamFrameTooLarge {
        limit_bytes: usize,
    },
    Problem(Box<ProblemError>),
}

impl ClientError {
    pub(crate) fn transport(error: reqwest::Error) -> Self {
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
            Self::MissingMcpTransport { operation_id } => {
                write!(formatter, "{operation_id} requires an MCP tool transport")
            }
            Self::UnsupportedTransport {
                operation_id,
                transport,
            } => write!(
                formatter,
                "{operation_id} is not mounted on this execution path; use its {transport} transport"
            ),
            Self::Authentication(status) => {
                write!(formatter, "daemon authentication failed with HTTP {status}")
            }
            Self::Protocol { message, .. } => write!(formatter, "protocol failure: {message}"),
            Self::StreamFrameTooLarge { limit_bytes } => write!(
                formatter,
                "event stream frame exceeded the {limit_bytes}-byte limit"
            ),
            Self::Problem(problem) => problem.fmt(formatter),
        }
    }
}

impl Error for ClientError {}

/// Appends each segment as one percent-encoded path segment, so an identifier
/// containing `/`, `?`, `#`, or `%` can never add, split, or rewrite a route.
fn with_path_segments<'a>(
    mut url: reqwest::Url,
    segments: impl IntoIterator<Item = &'a str>,
) -> Result<reqwest::Url, ClientError> {
    url.path_segments_mut()
        .map_err(|()| {
            ClientError::InvalidConfiguration("base URL cannot carry path segments".into())
        })?
        .extend(segments);
    Ok(url)
}

/// `.` and `..` are excluded because they are dot segments, which the URL
/// path builder drops rather than encodes, so they could never round-trip as
/// identifiers.
pub(crate) fn validate_opaque(value: &str, maximum: usize, field: &str) -> Result<(), ClientError> {
    let valid = !value.is_empty()
        && value.trim() == value
        && value.len() <= maximum
        && !value.chars().any(char::is_control)
        && !value.contains('/')
        && value != "."
        && value != "..";
    if valid {
        Ok(())
    } else {
        Err(ClientError::InvalidRequest(format!(
            "{field} is not a canonical opaque value"
        )))
    }
}
