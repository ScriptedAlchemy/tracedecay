//! Blocking local/remote client for the canonical HTTP and SSE lifecycle.
//!
//! Every envelope, problem, frame, and reconnect decision belongs to
//! [`crate::protocol`]; this client only performs blocking I/O for it.

use std::error::Error;
use std::fmt;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use reqwest::blocking::{Client as HttpClient, Response};
use reqwest::header::CONTENT_TYPE;
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::operations::{OperationTransport, TypedOperation};
use crate::protocol::{
    DEFAULT_TIMEOUT, Endpoint, OperationContract, PreparedRequest, ResponseBody, StreamOpening,
    StreamState, object, protocol, statusless_protocol, string, unsigned,
};
pub use crate::request_control::{OperationRequestOptions, PageOptions};
/// Selects loopback or remote HTTP policy without changing operation semantics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionMode {
    Local(ConnectionSettings),
    Remote(ConnectionSettings),
}

/// Shared authority settings for either connection mode.
#[derive(Clone, PartialEq, Eq)]
pub struct ConnectionSettings {
    pub(crate) base_url: String,
    pub(crate) project_id: String,
    pub(crate) token: String,
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

    pub(crate) fn settings(&self) -> &ConnectionSettings {
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
        let endpoint = Endpoint::new(&self.mode, self.origin.as_deref())?;
        let mut builder = HttpClient::builder().timeout(self.timeout);
        if endpoint.is_local() {
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
            timeout: DEFAULT_TIMEOUT,
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
            let contract = OperationContract::find(Operation::OPERATION_ID).ok_or_else(|| {
                ClientError::InvalidConfiguration(format!(
                    "{} is not an HTTP binding in the canonical SDK registry",
                    Operation::OPERATION_ID
                ))
            })?;
            let request = serde_json::to_value(request).map_err(|error| {
                statusless_protocol(format!("typed request could not be encoded: {error}"))
            })?;
            let prepared = self
                .endpoint
                .prepare_operation(contract, &request, &options)?;
            let admitted = crate::observe::finish(self.exchange(&prepared.request).and_then(
                |(status, content_type, body)| {
                    crate::observe::body_decode(|| {
                        prepared.admit_response(ResponseBody {
                            status,
                            content_type: content_type.as_deref(),
                            body: &body,
                        })
                    })
                },
            ))?;
            let payload = admitted.payload().cloned().unwrap_or(Value::Null);
            let result = Option::<Operation::Result>::deserialize(payload).map_err(|error| {
                statusless_protocol(format!(
                    "daemon returned a malformed {} result: {error}",
                    Operation::OPERATION_ID
                ))
            })?;
            Ok(TypedResponse {
                request_id: admitted.request_id,
                result,
                envelope: admitted.envelope,
            })
        })
    }

    /// Requests cancellation of a previously accepted operation.
    pub fn cancel_operation(
        &self,
        operation_id: &str,
    ) -> Result<OperationCancellation, ClientError> {
        crate::observe::finish((|| {
            let prepared = self.endpoint.prepare_cancel(operation_id)?;
            let (status, content_type, body) = self.exchange(&prepared)?;
            crate::observe::body_decode(|| {
                crate::protocol::decode_cancellation(ResponseBody {
                    status,
                    content_type: content_type.as_deref(),
                    body: &body,
                })
            })
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
                state: StreamState::new(operation_id, options)?,
                body: None,
                ready: std::collections::VecDeque::new(),
            };
            stream.connect()?;
            Ok(stream)
        })())
    }

    fn send(&self, prepared: &PreparedRequest) -> Result<Response, ClientError> {
        crate::observe::headers(|| {
            let mut request = self
                .http
                .request(prepared.method.clone(), prepared.url.clone())
                .headers(prepared.headers.clone());
            if let Some(body) = &prepared.body {
                request = request.body(body.clone());
            }
            request.send().map_err(ClientError::transport)
        })
    }

    /// Send and read one whole response body.
    fn exchange(
        &self,
        prepared: &PreparedRequest,
    ) -> Result<(u16, Option<String>, Vec<u8>), ClientError> {
        let response = self.send(prepared)?;
        let status = response.status().as_u16();
        let content_type = content_type(&response);
        crate::protocol::admit_json_head(status, content_type.as_deref())?;
        let body = response.bytes().map_err(ClientError::transport)?;
        Ok((status, content_type, body.to_vec()))
    }
}

fn content_type(response: &Response) -> Option<String> {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// One typed operation result with its lifecycle correlation identity and
/// validated canonical envelope.
#[derive(Clone, Debug, PartialEq)]
pub struct TypedResponse<Result> {
    pub request_id: String,
    /// Some terminal outcomes carry only their receipt. Absence of a payload
    /// never means that the operation completed; inspect the envelope receipt.
    pub result: Option<Result>,
    /// The validated success envelope, including operation/effect receipts.
    pub envelope: Value,
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

/// Resume authority and the subscriber's next undelivered sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamResume {
    pub token: String,
    /// One past the last consumed event or explicit gap, not the publication
    /// watermark in an open frame. Zero if only open has been consumed.
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
        crate::protocol::is_terminal_event(&self.event)
    }
}

/// Blocking SSE iterator with bounded frames and bounded, opt-in resume.
/// Dropping it closes its connection; it never cancels the operation.
pub struct OperationStream {
    client: Client,
    state: StreamState,
    body: Option<Response>,
    /// Events decoded from the last chunk but not yet yielded, plus the error
    /// that chunk ended with.
    ready: std::collections::VecDeque<Result<StreamEvent, ClientError>>,
}

/// Bytes requested from the body per read; frames span reads freely.
const READ_CHUNK_BYTES: usize = 8 * 1024;

impl OperationStream {
    fn connect(&mut self) -> Result<(), ClientError> {
        self.body = None;
        let prepared = self.client.endpoint.prepare_events(&self.state)?;
        let response = self.client.send(&prepared)?;
        let status = response.status().as_u16();
        match crate::protocol::stream_opening(status, content_type(&response).as_deref())? {
            StreamOpening::Events => {
                self.state.connected();
                self.body = Some(response);
                Ok(())
            }
            StreamOpening::Problem => {
                let body = crate::observe::body_decode(|| {
                    response.bytes().map_err(ClientError::transport)
                })?;
                Err(crate::protocol::refuse_stream_problem(status, &body))
            }
        }
    }

    /// Read until the body yields events, ends, or fails.
    fn read_more(&mut self) -> Result<(), ClientError> {
        let body = self
            .body
            .as_mut()
            .ok_or_else(|| ClientError::Transport("event stream reader is not connected".into()))?;
        let mut chunk = [0_u8; READ_CHUNK_BYTES];
        loop {
            let read = body
                .read(&mut chunk)
                .map_err(|error| ClientError::Transport(error.to_string()))?;
            if read == 0 {
                self.body = None;
                return Err(self.state.end_of_body());
            }
            let (events, error) = self.state.push_bytes(&chunk[..read]);
            let delivered = !events.is_empty();
            self.ready.extend(events.into_iter().map(Ok));
            if let Some(error) = error {
                self.body = None;
                self.ready.push_back(Err(error));
                return Ok(());
            }
            if delivered {
                if self.state.is_finished() {
                    self.body = None;
                }
                return Ok(());
            }
        }
    }
}

impl Iterator for OperationStream {
    type Item = Result<StreamEvent, ClientError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(ready) = self.ready.pop_front() {
                return Some(ready);
            }
            if self.state.is_finished() {
                return None;
            }
            let mut failure = match self.read_more() {
                Ok(()) => continue,
                Err(error) => error,
            };
            // Each reconnect attempt, including one that fails to connect,
            // spends budget; the state refuses anything but transport loss.
            loop {
                self.body = None;
                if let Err(error) = self.state.reconnect(failure) {
                    return Some(Err(error));
                }
                match self.connect() {
                    Ok(()) => break,
                    Err(error) => failure = error,
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
    pub(crate) fn new(status: u16, envelope: Value) -> Result<Self, ClientError> {
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
