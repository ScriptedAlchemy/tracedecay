//! Transport-independent lifecycle protocol shared by every SDK transport.
//!
//! This module performs no I/O. It prepares canonical requests and validates
//! the bytes a transport read back, so the blocking Rust client and the
//! asynchronous Node binding admit exactly the same envelopes, problems,
//! stream frames, terminal states, and reconnects.

use std::time::Duration;

use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, ORIGIN};
use reqwest::{Method, StatusCode, Url};
use serde::Deserialize as _;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tracedecay_contracts::RequestId;
use tracedecay_contracts::retained_surfaces::{SdkRequestIdControlV1, SdkResultSemanticsV1};
use tracedecay_tool_catalog::{
    CancellationPoint, ReceiptContract, ReconciliationContract, SdkTransportBindingV1,
    TerminalState,
};

use crate::client::{
    CancellationStatus, ClientError, ConnectionMode, OperationCancellation, ProblemError,
    StreamEvent, StreamOptions,
};
use crate::operations::{APPLICATION_REQUEST_ID_HEADER, OperationTransport, TypedOperation};
use crate::request_control::{OperationRequestOptions, PageOptions};

pub(crate) const MAX_OPAQUE_BYTES: usize = 4_096;
pub(crate) const MAX_REQUEST_ID_BYTES: usize = 512;
const MAX_PAGE_SIZE: u32 = 1_000;
const DEADLINE_HEADER: &str = "x-tracedecay-deadline-micros";
/// Bound on one raw SSE line, terminator included, and on the event/id/data
/// bytes retained for one frame before JSON decoding. Every canonical frame is
/// a single JSON object of bounded metadata, request and operation identifiers
/// (≤ 512 bytes), a test name (≤ 1 KiB), a sequence, a frontier or a terminal
/// receipt, so 64 KiB leaves wide headroom while capping what a peer can make
/// the client retain.
pub const MAX_SSE_FRAME_BYTES: usize = 64 * 1024;
/// Default whole-request bound for transports that do not override it.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Lifecycle metadata of one generated operation, readable without its Rust
/// request and result types. The table in [`crate::operations`] is generated
/// from the canonical registry, so dynamic callers (the Node binding) and
/// typed callers resolve the same contract.
#[derive(Clone, Copy, Debug)]
pub struct OperationContract {
    pub operation_id: &'static str,
    pub transport: OperationTransport,
    pub binding_id: &'static str,
    pub request_id_control: SdkRequestIdControlV1,
    pub result_semantics: SdkResultSemanticsV1,
    pub cancellable: bool,
    pub cancellation_points: &'static [CancellationPoint],
    pub reconciliation: ReconciliationContract,
    pub receipt: ReceiptContract,
    pub terminal_states: &'static [TerminalState],
    pub result_schema_id: &'static str,
    pub result_schema_revision: u32,
    canonical_request: fn(&Value) -> Result<Value, String>,
    validate_result: fn(&Value) -> Result<(), String>,
}

impl OperationContract {
    pub const fn of<Operation>() -> Self
    where
        Operation: TypedOperation,
        Operation::Request: DeserializeOwned,
    {
        Self {
            operation_id: Operation::OPERATION_ID,
            transport: Operation::TRANSPORT,
            binding_id: Operation::BINDING_ID,
            request_id_control: Operation::REQUEST_ID_CONTROL,
            result_semantics: Operation::RESULT_SEMANTICS,
            cancellable: Operation::CANCELLABLE,
            cancellation_points: Operation::CANCELLATION_POINTS,
            reconciliation: Operation::RECONCILIATION,
            receipt: Operation::RECEIPT,
            terminal_states: Operation::TERMINAL_STATES,
            result_schema_id: Operation::RESULT_SCHEMA_ID,
            result_schema_revision: Operation::RESULT_SCHEMA_REVISION,
            canonical_request: canonical_request::<Operation>,
            validate_result: validate_result::<Operation>,
        }
    }

    /// The generated contract for `operation_id`, if the registry has one.
    pub fn find(operation_id: &str) -> Option<&'static Self> {
        crate::operations::OPERATION_CONTRACTS
            .iter()
            .find(|contract| contract.operation_id == operation_id)
    }

    /// Decode a caller-supplied JSON request through the operation's Rust
    /// request type and return its canonical encoding.
    pub fn canonical_request(&self, request: &Value) -> Result<Value, ClientError> {
        (self.canonical_request)(request).map_err(|error| {
            ClientError::InvalidRequest(format!(
                "{} request does not match its contract: {error}",
                self.operation_id
            ))
        })
    }

    fn http_route(&self) -> Result<&'static str, ClientError> {
        match self.transport {
            OperationTransport::Http { route } => Ok(route),
            OperationTransport::McpTool { .. } => Err(ClientError::UnsupportedTransport {
                operation_id: self.operation_id,
                transport: "mcp_tool",
            }),
        }
    }

    fn admit_request_id(
        &self,
        options: &OperationRequestOptions,
    ) -> Result<Option<RequestId>, ClientError> {
        match (self.request_id_control, options.request_id.as_ref()) {
            (SdkRequestIdControlV1::Required, None) => Err(ClientError::InvalidRequest(format!(
                "{} requires a stable request_id replay handle",
                self.operation_id
            ))),
            (SdkRequestIdControlV1::ServerMinted, Some(_)) => Err(ClientError::InvalidRequest(
                format!("{} uses a server-minted request ID", self.operation_id),
            )),
            _ => Ok(options.request_id.clone()),
        }
    }
}

fn canonical_request<Operation>(request: &Value) -> Result<Value, String>
where
    Operation: TypedOperation,
    Operation::Request: DeserializeOwned,
{
    let typed = Operation::Request::deserialize(request).map_err(|error| error.to_string())?;
    serde_json::to_value(typed).map_err(|error| error.to_string())
}

fn validate_result<Operation: TypedOperation>(payload: &Value) -> Result<(), String> {
    Operation::Result::deserialize(payload)
        .map(drop)
        .map_err(|error| error.to_string())
}

/// One HTTP exchange a transport must perform verbatim.
#[derive(Clone, Debug)]
pub struct PreparedRequest {
    pub method: Method,
    pub url: Url,
    pub headers: HeaderMap,
    pub body: Option<Vec<u8>>,
}

/// A prepared typed operation plus what its response must echo.
#[derive(Clone, Debug)]
pub struct PreparedOperation {
    pub request: PreparedRequest,
    pub contract: &'static OperationContract,
    canonical_request: Value,
    expected_request_id: Option<RequestId>,
}

/// A response the transport fully read.
#[derive(Clone, Copy, Debug)]
pub struct ResponseBody<'a> {
    pub status: u16,
    pub content_type: Option<&'a str>,
    pub body: &'a [u8],
}

/// A validated typed success envelope.
#[derive(Clone, Debug, PartialEq)]
pub struct AdmittedResponse {
    pub request_id: String,
    pub envelope: Value,
}

impl AdmittedResponse {
    pub fn payload(&self) -> Option<&Value> {
        self.envelope.get("outcome")?.get("value")?.get("payload")
    }
}

/// Validated daemon authority: application root, bearer, and origin.
#[derive(Clone, Debug)]
pub struct Endpoint {
    application_root: Url,
    authorization: HeaderValue,
    origin: HeaderValue,
    local: bool,
}

impl Endpoint {
    pub fn new(mode: &ConnectionMode, origin: Option<&str>) -> Result<Self, ClientError> {
        let settings = mode.settings();
        validate_opaque(&settings.project_id, MAX_REQUEST_ID_BYTES, "project ID")?;
        validate_opaque(&settings.token, MAX_OPAQUE_BYTES, "bearer token")?;
        let mut base = Url::parse(&settings.base_url)
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
        let origin = HeaderValue::from_str(origin.unwrap_or(&default_origin))
            .map_err(|error| ClientError::InvalidConfiguration(error.to_string()))?;
        let mut authorization = HeaderValue::from_str(&format!("Bearer {}", settings.token))
            .map_err(|error| ClientError::InvalidConfiguration(error.to_string()))?;
        authorization.set_sensitive(true);
        let application_root = with_path_segments(
            base,
            ["projects", settings.project_id.as_str(), "application"],
        )?;
        Ok(Self {
            application_root,
            authorization,
            origin,
            local: matches!(mode, ConnectionMode::Local(_)),
        })
    }

    /// Loopback authority: transports must never route it through an ambient
    /// proxy, which would receive the local daemon credential.
    pub const fn is_local(&self) -> bool {
        self.local
    }

    /// Prepare one typed HTTP operation from a JSON request.
    pub fn prepare_operation(
        &self,
        contract: &'static OperationContract,
        request: &Value,
        options: &OperationRequestOptions,
    ) -> Result<PreparedOperation, ClientError> {
        let route = contract.http_route()?;
        let canonical_request = contract.canonical_request(request)?;
        let expected_request_id = contract.admit_request_id(options)?;
        admit_canonical_sdk_http_binding(contract.operation_id, contract.binding_id, route)?;
        let route = route.strip_prefix("/application").ok_or_else(|| {
            ClientError::InvalidConfiguration(
                "typed operation route must begin with /application".into(),
            )
        })?;
        let mut url = with_path_segments(
            self.application_root.clone(),
            route.split('/').filter(|segment| !segment.is_empty()),
        )?;
        apply_page(&mut url, options.page.as_ref())?;
        let mut headers = self.headers("application/json");
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        apply_request_headers(&mut headers, options)?;
        let body = serde_json::to_vec(&canonical_request).map_err(|error| {
            statusless_protocol(format!("typed request could not be encoded: {error}"))
        })?;
        Ok(PreparedOperation {
            request: PreparedRequest {
                method: Method::POST,
                url,
                headers,
                body: Some(body),
            },
            contract,
            canonical_request,
            expected_request_id,
        })
    }

    pub fn prepare_cancel(&self, operation_id: &str) -> Result<PreparedRequest, ClientError> {
        validate_opaque(operation_id, MAX_REQUEST_ID_BYTES, "operation ID")?;
        Ok(PreparedRequest {
            method: Method::POST,
            url: self.lifecycle_url(operation_id, "cancel")?,
            headers: self.headers("application/json"),
            body: None,
        })
    }

    /// Prepare the next event-stream connection from the stream's cursor.
    pub fn prepare_events(&self, stream: &StreamState) -> Result<PreparedRequest, ClientError> {
        let mut url = self.lifecycle_url(&stream.operation_id, "events")?;
        // A first connection carries no cursor and so no query at all.
        if stream.next_sequence.is_some() || stream.resume_token.is_some() {
            let mut query = url.query_pairs_mut();
            if let Some(next_sequence) = stream.next_sequence {
                query.append_pair("next_sequence", &next_sequence.to_string());
            }
            if let Some(token) = &stream.resume_token {
                query.append_pair("resume_token", token);
            }
        }
        Ok(PreparedRequest {
            method: Method::GET,
            url,
            headers: self.headers("text/event-stream"),
            body: None,
        })
    }

    fn lifecycle_url(&self, operation_id: &str, suffix: &str) -> Result<Url, ClientError> {
        with_path_segments(
            self.application_root.clone(),
            ["operations", operation_id, suffix],
        )
    }

    fn headers(&self, accept: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static(accept));
        headers.insert(AUTHORIZATION, self.authorization.clone());
        headers.insert(ORIGIN, self.origin.clone());
        headers
    }
}

fn apply_page(url: &mut Url, page: Option<&PageOptions>) -> Result<(), ClientError> {
    let Some(page) = page else {
        return Ok(());
    };
    if let Some(size) = page.size
        && !(1..=MAX_PAGE_SIZE).contains(&size)
    {
        return Err(ClientError::InvalidRequest(format!(
            "page size must be an integer between 1 and {MAX_PAGE_SIZE}"
        )));
    }
    if let Some(cursor) = &page.cursor {
        if !is_bounded_opaque(cursor, MAX_OPAQUE_BYTES) {
            return Err(ClientError::InvalidRequest(
                "page cursor is not a canonical opaque cursor".into(),
            ));
        }
    }
    if page.size.is_none() && page.cursor.is_none() {
        return Ok(());
    }
    let mut query = url.query_pairs_mut();
    if let Some(size) = page.size {
        query.append_pair("page_size", &size.to_string());
    }
    if let Some(cursor) = &page.cursor {
        query.append_pair("cursor", cursor);
    }
    Ok(())
}

fn apply_request_headers(
    headers: &mut HeaderMap,
    options: &OperationRequestOptions,
) -> Result<(), ClientError> {
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
    if let Some(request_id) = &options.request_id {
        let value = HeaderValue::from_str(request_id.as_str())
            .map_err(|error| ClientError::InvalidRequest(error.to_string()))?;
        headers.insert(APPLICATION_REQUEST_ID_HEADER, value);
    }
    Ok(())
}

type CanonicalSdkHttpBindings = std::collections::BTreeMap<String, (String, String)>;

fn admit_canonical_sdk_http_binding(
    operation_id: &str,
    binding_id: &str,
    route: &str,
) -> Result<(), ClientError> {
    static BINDINGS: std::sync::OnceLock<Result<CanonicalSdkHttpBindings, String>> =
        std::sync::OnceLock::new();
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

impl PreparedOperation {
    /// Validate the daemon's response against this operation's contract.
    pub fn admit_response(
        &self,
        response: ResponseBody<'_>,
    ) -> Result<AdmittedResponse, ClientError> {
        let contract = self.contract;
        let status = response.status;
        let (envelope, shape) = decode_application_response(
            response,
            contract.binding_id,
            self.expected_request_id.as_ref(),
        )?;
        let SuccessShape {
            outcome_kind,
            termination,
        } = shape;
        let contract_ref = envelope.get("contract");
        let binding = envelope.get("binding_id").and_then(Value::as_str);
        let schema_id = contract_ref
            .and_then(|value| value.get("schema_id"))
            .and_then(Value::as_str);
        let schema_revision = contract_ref
            .and_then(|value| value.get("schema_revision"))
            .and_then(Value::as_u64);
        if binding != Some(contract.binding_id)
            || schema_id != Some(contract.result_schema_id)
            || schema_revision != Some(u64::from(contract.result_schema_revision))
        {
            return Err(protocol(
                status,
                format!(
                    "daemon returned mismatched contracts for {}",
                    contract.operation_id
                ),
            ));
        }
        let outcome = &envelope["outcome"];
        let lifecycle_shape_is_legal = matches!(
            (contract.receipt, contract.reconciliation, outcome_kind),
            (
                ReceiptContract::Operation,
                ReconciliationContract::NotRequired,
                OutcomeKind::Evidence | OutcomeKind::Preview
            ) | (
                ReceiptContract::DurableEffect,
                ReconciliationContract::Required,
                OutcomeKind::Effect
            )
        );
        if !lifecycle_shape_is_legal {
            return Err(protocol(
                status,
                format!(
                    "daemon returned outcome {} outside the {} receipt contract",
                    outcome_kind.as_str(),
                    contract.operation_id
                ),
            ));
        }
        let execution = &outcome["value"]["execution"];
        if !contract.terminal_states.contains(&termination) {
            return Err(protocol(
                status,
                format!(
                    "daemon returned terminal state {} outside the {} contract",
                    termination.as_str(),
                    contract.operation_id
                ),
            ));
        }
        if let Some(cancellation) = execution.get("cancellation")
            && !cancellation.is_null()
        {
            if !contract.cancellable {
                return Err(protocol(
                    status,
                    format!(
                        "daemon returned cancellation evidence for non-cancellable {}",
                        contract.operation_id
                    ),
                ));
            }
            let stage = cancellation
                .get("stage")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    protocol(
                        status,
                        format!(
                            "daemon returned malformed cancellation evidence for {}",
                            contract.operation_id
                        ),
                    )
                })?;
            if !contract
                .cancellation_points
                .iter()
                .any(|point| cancellation_point_name(*point) == stage)
            {
                return Err(protocol(
                    status,
                    format!(
                        "daemon returned cancellation stage {stage} outside the {} contract",
                        contract.operation_id
                    ),
                ));
            }
        }
        let request_id = string(envelope.get("request_id"), status, "request_id")?.to_owned();
        let payload = &outcome["value"]["payload"];
        if !crate::semantic::response_matches(
            contract.result_semantics,
            &request_id,
            self.expected_request_id.as_ref(),
            &self.canonical_request,
            &envelope["scope"],
            payload,
        ) {
            return Err(protocol(
                status,
                format!(
                    "daemon returned a semantically invalid {} result",
                    contract.operation_id
                ),
            ));
        }
        // A completed evidence read always carries its payload; only a
        // non-completed receipt may stand without one.
        let receipt_without_payload = outcome_kind == OutcomeKind::Evidence
            && termination != TerminalState::Completed
            && payload.is_null();
        if !receipt_without_payload {
            (contract.validate_result)(payload).map_err(|error| {
                protocol(
                    status,
                    format!(
                        "daemon returned a malformed {} result: {error}",
                        contract.operation_id
                    ),
                )
            })?;
        }
        Ok(AdmittedResponse {
            request_id,
            envelope,
        })
    }
}

/// Decode a success or problem body read from an operation route into the
/// structurally valid success envelope.
fn decode_application_response(
    response: ResponseBody<'_>,
    binding_id: &str,
    expected_request_id: Option<&RequestId>,
) -> Result<(Value, SuccessShape), ClientError> {
    let status = response.status;
    admit_json_head(status, response.content_type)?;
    let mut body: Value = serde_json::from_slice(response.body).map_err(|error| {
        protocol(
            status,
            format!("daemon returned malformed application JSON: {error}"),
        )
    })?;
    let success = StatusCode::from_u16(status).is_ok_and(|status| status.is_success());
    match body.get("kind").and_then(Value::as_str) {
        Some("success") if success => {
            let value = body
                .get_mut("value")
                .map(Value::take)
                .ok_or_else(|| protocol(status, "success envelope has no value"))?;
            let shape = validate_success_envelope(&value, status)?;
            Ok((value, shape))
        }
        Some("problem") if !success => {
            let problem = problem_error(&body, status, "problem envelope has no value");
            if let ClientError::Problem(problem) = &problem {
                let envelope = &problem.envelope;
                let response_request_id = envelope.get("request_id").and_then(Value::as_str);
                if expected_request_id
                    .is_some_and(|expected| response_request_id != Some(expected.as_str()))
                {
                    return Err(protocol(
                        status,
                        "daemon returned a different application request ID",
                    ));
                }
                // A denial never names the binding it refused; any other
                // problem must be bound to the operation that was invoked.
                let problem_binding = envelope.get("binding_id").and_then(Value::as_str);
                let binding_is_legal = if problem.kind == "not_found_or_not_authorized" {
                    problem_binding.is_none()
                } else {
                    problem_binding == Some(binding_id)
                };
                if !binding_is_legal {
                    return Err(protocol(
                        status,
                        "daemon returned a problem bound to a different operation",
                    ));
                }
            }
            Err(problem)
        }
        _ => Err(protocol(
            status,
            "daemon returned an inconsistent HTTP envelope",
        )),
    }
}

/// Refuse a JSON exchange from its response head, before a transport reads
/// a body it would discard: authentication failures and non-JSON media are
/// decided by the head alone, so an endless refused body is never awaited.
pub fn admit_json_head(status: u16, content_type: Option<&str>) -> Result<(), ClientError> {
    let status = refuse_authentication(status)?;
    if media_type(content_type) != Some("application/json") {
        return Err(protocol(status, "daemon response is not application/json"));
    }
    Ok(())
}

/// Decode a cancellation acknowledgement.
pub fn decode_cancellation(
    response: ResponseBody<'_>,
) -> Result<OperationCancellation, ClientError> {
    let status = response.status;
    admit_json_head(status, response.content_type)?;
    let body: Value = serde_json::from_slice(response.body).map_err(|error| {
        protocol(
            status,
            format!("daemon returned malformed cancellation JSON: {error}"),
        )
    })?;
    if body.get("kind").and_then(Value::as_str) == Some("problem") {
        return Err(problem_error(
            &body,
            status,
            "cancellation problem envelope has no value",
        ));
    }
    let value: OperationCancellation = serde_json::from_value(body).map_err(|error| {
        protocol(
            status,
            format!("daemon returned malformed cancellation JSON: {error}"),
        )
    })?;
    let valid_status = matches!(
        (status, value.status),
        (202, CancellationStatus::Requested)
            | (200, CancellationStatus::AlreadyRequested)
            | (200, CancellationStatus::AlreadyTerminal)
    );
    if !valid_status {
        return Err(protocol(
            status,
            "daemon returned a non-canonical cancellation response",
        ));
    }
    Ok(value)
}

/// How a transport must treat the response head of an event-stream request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamOpening {
    /// Read the body as SSE through [`StreamState::push_bytes`].
    Events,
    /// Read the whole JSON body and pass it to [`refuse_stream_problem`].
    Problem,
}

/// Classify an event-stream response head before reading its body.
pub fn stream_opening(
    status: u16,
    content_type: Option<&str>,
) -> Result<StreamOpening, ClientError> {
    let status = refuse_authentication(status)?;
    let success = StatusCode::from_u16(status).is_ok_and(|status| status.is_success());
    let media_type = media_type(content_type);
    if !success && media_type == Some("application/json") {
        return Ok(StreamOpening::Problem);
    }
    if !success || media_type != Some("text/event-stream") {
        return Err(protocol(
            status,
            "daemon did not open a canonical event stream",
        ));
    }
    Ok(StreamOpening::Events)
}

/// The typed refusal carried by a non-success event-stream JSON body.
pub fn refuse_stream_problem(status: u16, body: &[u8]) -> ClientError {
    let body: Value = match serde_json::from_slice(body) {
        Ok(body) => body,
        Err(error) => {
            return protocol(
                status,
                format!("daemon returned malformed stream problem JSON: {error}"),
            );
        }
    };
    if body.get("kind").and_then(Value::as_str) == Some("problem") {
        return problem_error(&body, status, "stream problem envelope has no value");
    }
    protocol(status, "daemon returned an unknown stream problem envelope")
}

/// Resumable event-stream state: SSE framing, canonical frame validation,
/// the delivered cursor, and the bounded reconnect budget.
///
/// A transport feeds every body chunk to [`Self::push_bytes`], reports end of
/// body through [`Self::end_of_body`], and asks [`Self::reconnect`] before
/// each new connection. It never interprets frames itself.
#[derive(Clone, Debug)]
pub struct StreamState {
    operation_id: String,
    max_reconnects: usize,
    reconnects: usize,
    next_sequence: Option<u64>,
    resume_token: Option<String>,
    terminal: bool,
    line: Vec<u8>,
    pending_event: Option<String>,
    pending_id: Option<String>,
    /// Data lines of the open frame, already joined with `\n`.
    pending_data: Option<String>,
}

impl StreamState {
    pub fn new(operation_id: &str, options: StreamOptions) -> Result<Self, ClientError> {
        validate_opaque(operation_id, MAX_REQUEST_ID_BYTES, "operation ID")?;
        let (next_sequence, resume_token) = match options.resume {
            Some(resume) => {
                validate_opaque(&resume.token, MAX_OPAQUE_BYTES, "resume token")?;
                (Some(resume.next_sequence), Some(resume.token))
            }
            None => (None, None),
        };
        Ok(Self {
            operation_id: operation_id.to_owned(),
            max_reconnects: options.max_reconnects,
            reconnects: 0,
            next_sequence,
            resume_token,
            terminal: false,
            line: Vec::new(),
            pending_event: None,
            pending_id: None,
            pending_data: None,
        })
    }

    /// Whether the stream delivered its terminal event or failed for good.
    pub const fn is_finished(&self) -> bool {
        self.terminal
    }

    /// Start reading a freshly opened body; partial frames never carry over.
    pub fn connected(&mut self) {
        self.line.clear();
        self.pending_event = None;
        self.pending_id = None;
        self.pending_data = None;
    }

    /// Feed body bytes and collect every event they complete. Events decoded
    /// before a failure in the same chunk are returned with it, so a caller
    /// delivers them before the error. A terminal event ends the stream and
    /// discards anything after it.
    pub fn push_bytes(&mut self, mut bytes: &[u8]) -> (Vec<StreamEvent>, Option<ClientError>) {
        let mut events = Vec::new();
        while !bytes.is_empty() && !self.terminal {
            let newline = bytes.iter().position(|byte| *byte == b'\n');
            let take = newline.map_or(bytes.len(), |index| index + 1);
            if self.line.len() + take > MAX_SSE_FRAME_BYTES {
                return (events, Some(self.refuse_oversized_frame()));
            }
            self.line.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if newline.is_none() {
                break;
            }
            let raw = std::mem::take(&mut self.line);
            let line = match std::str::from_utf8(&raw) {
                Ok(line) => line.trim_end_matches(['\r', '\n']),
                Err(_) => {
                    return (
                        events,
                        Some(self.fail(statusless_protocol("event stream is not valid UTF-8"))),
                    );
                }
            };
            match self.push_line(line) {
                Ok(Some(event)) => {
                    self.terminal = event.terminal();
                    events.push(event);
                }
                Ok(None) => {}
                Err(error) => return (events, Some(self.fail(error))),
            }
        }
        (events, None)
    }

    /// The body ended. Returns the disconnect to pass to [`Self::reconnect`],
    /// or a protocol failure that ends the stream.
    pub fn end_of_body(&mut self) -> ClientError {
        if !self.line.is_empty() || self.pending_event.is_some() || self.pending_data.is_some() {
            return self.fail(statusless_protocol(
                "event stream ended inside an SSE frame",
            ));
        }
        ClientError::Transport("event stream ended before a terminal event".into())
    }

    /// Decide whether a failed or ended connection may be retried. Only
    /// transport failures are retried, each attempt (including one that fails
    /// to connect) spends budget, and a retry requires a resumable frontier.
    /// Returns the error that ends the stream when no retry is allowed.
    pub fn reconnect(&mut self, failure: ClientError) -> Result<(), ClientError> {
        if self.terminal {
            return Err(failure);
        }
        if !matches!(failure, ClientError::Transport(_)) {
            return Err(self.fail(failure));
        }
        if self.reconnects >= self.max_reconnects {
            return Err(self.fail(failure));
        }
        if self.next_sequence.is_none() || self.resume_token.is_none() {
            return Err(self.fail(statusless_protocol(
                "stream ended without a resumable frontier",
            )));
        }
        self.reconnects += 1;
        Ok(())
    }

    /// End the stream after `error`, so later reads yield nothing.
    pub fn fail(&mut self, error: ClientError) -> ClientError {
        self.terminal = true;
        self.line.clear();
        error
    }

    /// Bytes currently retained for the open frame.
    fn pending_frame_bytes(&self) -> usize {
        self.pending_event.as_deref().map_or(0, str::len)
            + self.pending_id.as_deref().map_or(0, str::len)
            + self.pending_data.as_deref().map_or(0, str::len)
    }

    /// Closes the stream without delivering the frame or touching the resume
    /// frontier; the remainder of an oversized frame is never drained.
    fn refuse_oversized_frame(&mut self) -> ClientError {
        self.pending_event = None;
        self.pending_id = None;
        self.pending_data = None;
        self.fail(ClientError::StreamFrameTooLarge {
            limit_bytes: MAX_SSE_FRAME_BYTES,
        })
    }

    fn push_line(&mut self, line: &str) -> Result<Option<StreamEvent>, ClientError> {
        if line.is_empty() {
            let Some(data_text) = self.pending_data.take() else {
                self.pending_event = None;
                return Ok(None);
            };
            let event_name = self
                .pending_event
                .take()
                .unwrap_or_else(|| "message".into());
            let id = self.pending_id.take();
            return self.decode_frame(event_name, id, &data_text).map(Some);
        }
        if line.starts_with(':') {
            return Ok(None);
        }
        let (field, value) = line.split_once(':').map_or((line, ""), |(field, value)| {
            (field, value.strip_prefix(' ').unwrap_or(value))
        });
        match field {
            // Account before extending: the joining `\n` counts too.
            "event" | "id" | "data"
                if self.pending_frame_bytes() + value.len() + 1 > MAX_SSE_FRAME_BYTES =>
            {
                return Err(self.refuse_oversized_frame());
            }
            "event" => self.pending_event = Some(value.to_owned()),
            "id" if !value.contains('\0') => self.pending_id = Some(value.to_owned()),
            "data" => match &mut self.pending_data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => self.pending_data = Some(value.to_owned()),
            },
            _ => {}
        }
        Ok(None)
    }

    fn decode_frame(
        &mut self,
        event_name: String,
        id: Option<String>,
        data_text: &str,
    ) -> Result<StreamEvent, ClientError> {
        let data: Value = serde_json::from_str(data_text)
            .map_err(|error| statusless_protocol(format!("SSE data is not JSON: {error}")))?;
        if data.get("event").and_then(Value::as_str) != Some(event_name.as_str()) {
            return Err(statusless_protocol(
                "SSE event name disagrees with its JSON payload",
            ));
        }
        if event_name == "open" {
            if id.is_some() {
                return Err(statusless_protocol("SSE open event must not carry an ID"));
            }
            let open_data = data
                .get("data")
                .and_then(Value::as_object)
                .ok_or_else(|| statusless_protocol("SSE open data is malformed"))?;
            if open_data.get("correlation_id").and_then(Value::as_str)
                != Some(self.operation_id.as_str())
            {
                return Err(statusless_protocol(
                    "SSE open correlation identity does not match operation",
                ));
            }
            let frontier = open_data
                .get("frontier")
                .and_then(Value::as_object)
                .ok_or_else(|| statusless_protocol("SSE open frontier is malformed"))?;
            let next_sequence = frontier
                .get("next_sequence")
                .and_then(Value::as_u64)
                .ok_or_else(|| statusless_protocol("SSE open frontier has no next sequence"))?;
            let retained = frontier
                .get("retained_from_sequence")
                .and_then(Value::as_u64)
                .ok_or_else(|| statusless_protocol("SSE open frontier has no retained sequence"))?;
            if retained > next_sequence {
                return Err(statusless_protocol(
                    "SSE open frontier sequence range is inconsistent",
                ));
            }
            let resume_token = resume_token(frontier.get("resume_token"), "SSE open frontier")?;
            // The frontier is a publication watermark. Receiving open does
            // not consume any event in that published history.
            self.next_sequence.get_or_insert(0);
            self.resume_token = resume_token;
            return Ok(StreamEvent {
                event: event_name,
                id,
                data,
            });
        }
        let terminal = is_terminal_event(&event_name);
        if !matches!(event_name.as_str(), "item" | "progress" | "resume_gap") && !terminal {
            return Err(statusless_protocol("SSE event name is not canonical"));
        }
        let event_data = data
            .get("data")
            .and_then(Value::as_object)
            .ok_or_else(|| statusless_protocol("SSE event data is malformed"))?;
        let sequence = event_data
            .get("sequence")
            .and_then(Value::as_u64)
            .ok_or_else(|| statusless_protocol("SSE event has no canonical sequence"))?;
        if id.as_deref() != Some(sequence.to_string().as_str()) {
            return Err(statusless_protocol(
                "SSE ID disagrees with its canonical sequence",
            ));
        }
        let mut last_sequence = sequence;
        let mut gap_token = None;
        match event_name.as_str() {
            "item" if !event_data.contains_key("item") => {
                return Err(statusless_protocol("SSE item payload is missing"));
            }
            "progress"
                if event_data
                    .get("completed")
                    .and_then(Value::as_u64)
                    .is_none()
                    || !matches!(event_data.get("total"), Some(Value::Null))
                        && event_data.get("total").and_then(Value::as_u64).is_none() =>
            {
                return Err(statusless_protocol("SSE progress payload is malformed"));
            }
            "resume_gap" => {
                let (last, token) = validate_resume_gap(event_data, sequence)?;
                last_sequence = last;
                gap_token = Some(token);
            }
            event if is_terminal_event(event) => validate_terminal(event_data, event)?,
            _ => {}
        }
        let next_sequence = last_sequence
            .checked_add(1)
            .ok_or_else(|| statusless_protocol("SSE sequence overflowed"))?;
        let expected = self.next_sequence.unwrap_or(0);
        // A completed operation repeats its terminal receipt when the
        // requested cursor is exactly one past that receipt.
        let terminal_replay = terminal && next_sequence == expected;
        if sequence != expected && !terminal_replay {
            return Err(statusless_protocol(
                "SSE event sequence disagrees with the delivered cursor",
            ));
        }
        self.next_sequence = Some(next_sequence);
        if let Some(token) = gap_token {
            self.resume_token = token;
        }
        Ok(StreamEvent {
            event: event_name,
            id,
            data,
        })
    }
}

fn resume_token(value: Option<&Value>, field: &str) -> Result<Option<String>, ClientError> {
    match value {
        Some(Value::Null) => Ok(None),
        Some(Value::String(token)) if !token.is_empty() => Ok(Some(token.clone())),
        _ => Err(statusless_protocol(format!(
            "{field} resume token is malformed"
        ))),
    }
}

fn validate_resume_gap(
    event_data: &serde_json::Map<String, Value>,
    sequence: u64,
) -> Result<(u64, Option<String>), ClientError> {
    let gap = event_data
        .get("gap")
        .and_then(Value::as_object)
        .ok_or_else(|| statusless_protocol("SSE resume gap is malformed"))?;
    let first = gap
        .get("first_missing_sequence")
        .and_then(Value::as_u64)
        .ok_or_else(|| statusless_protocol("SSE resume gap has no first sequence"))?;
    let last = gap
        .get("last_missing_sequence")
        .and_then(Value::as_u64)
        .ok_or_else(|| statusless_protocol("SSE resume gap has no last sequence"))?;
    let frontier = gap
        .get("frontier")
        .and_then(Value::as_object)
        .ok_or_else(|| statusless_protocol("SSE resume gap frontier is malformed"))?;
    let next = frontier
        .get("next_sequence")
        .and_then(Value::as_u64)
        .ok_or_else(|| statusless_protocol("SSE resume gap frontier has no next sequence"))?;
    let retained = frontier
        .get("retained_from_sequence")
        .and_then(Value::as_u64)
        .ok_or_else(|| statusless_protocol("SSE resume gap frontier has no retained sequence"))?;
    if first != sequence || first > last || retained > next || last >= next {
        return Err(statusless_protocol(
            "SSE resume gap range or frontier is malformed",
        ));
    }
    Ok((
        last,
        resume_token(frontier.get("resume_token"), "SSE resume gap")?,
    ))
}

fn validate_terminal(
    event_data: &serde_json::Map<String, Value>,
    event_name: &str,
) -> Result<(), ClientError> {
    let terminal = event_data
        .get("terminal")
        .and_then(Value::as_object)
        .ok_or_else(|| statusless_protocol("SSE terminal payload is malformed"))?;
    if terminal.get("termination").and_then(Value::as_str) != Some(event_name) {
        return Err(statusless_protocol(
            "SSE terminal outcome disagrees with its event",
        ));
    }
    let receipt_termination = validate_receipt(terminal.get("receipt"), 200, "terminal.receipt")?;
    if receipt_termination != event_name {
        return Err(statusless_protocol(
            "SSE terminal receipt disagrees with its event",
        ));
    }
    Ok(())
}

/// Whether an event name ends the operation's stream.
pub(crate) fn is_terminal_event(event: &str) -> bool {
    TerminalState::ALL
        .iter()
        .any(|state| state.as_str() == event)
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

fn refuse_authentication(status: u16) -> Result<u16, ClientError> {
    if status == 401 || status == 403 {
        Err(ClientError::Authentication(status))
    } else {
        Ok(status)
    }
}

/// The media type, without its parameters.
fn media_type(content_type: Option<&str>) -> Option<&str> {
    content_type
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

pub(crate) fn object<'a>(
    value: Option<&'a Value>,
    status: u16,
    field: &str,
) -> Result<&'a serde_json::Map<String, Value>, ClientError> {
    value
        .and_then(Value::as_object)
        .ok_or_else(|| protocol(status, format!("{field} must be an object")))
}

pub(crate) fn string<'a>(
    value: Option<&'a Value>,
    status: u16,
    field: &str,
) -> Result<&'a str, ClientError> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| protocol(status, format!("{field} must be a non-empty string")))
}

pub(crate) fn unsigned(
    value: Option<&Value>,
    status: u16,
    field: &str,
) -> Result<u64, ClientError> {
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

fn validate_receipt<'a>(
    value: Option<&'a Value>,
    status: u16,
    field: &str,
) -> Result<&'a str, ClientError> {
    let receipt = object(value, status, field)?;
    let started_at = unsigned(receipt.get("started_at"), status, "execution.started_at")?;
    let ended_at = unsigned(receipt.get("ended_at"), status, "execution.ended_at")?;
    if ended_at < started_at {
        return Err(protocol(status, "execution ended before it started"));
    }
    if !receipt.contains_key("effective_deadline") {
        return Err(protocol(status, "execution lifecycle fields are required"));
    }
    let cancellation = match receipt.get("cancellation") {
        Some(Value::Null) => None,
        Some(Value::Object(cancellation)) => Some(cancellation),
        _ => {
            return Err(protocol(
                status,
                "execution.cancellation must be null or an object",
            ));
        }
    };
    let budget = object(receipt.get("budget"), status, "execution.budget")?;
    for field in ["units_consumed", "bytes_consumed", "elapsed_micros"] {
        unsigned(budget.get(field), status, &format!("budget.{field}"))?;
    }
    let termination = string(receipt.get("termination"), status, "execution.termination")?;
    if matches!(termination, "cancelled" | "timed_out") && cancellation.is_none() {
        return Err(protocol(
            status,
            "a cancelled or timed-out execution must carry cancellation evidence",
        ));
    }
    Ok(termination)
}

/// Decode the evidence page through its canonical contract type, which also
/// validates every cursor identifier, then apply the range invariant the
/// contract's constructor enforces.
fn validate_page(value: Option<&Value>, status: u16) -> Result<(), ClientError> {
    let page = value.ok_or_else(|| protocol(status, "outcome.value.page is required"))?;
    let page = tracedecay_contracts::PageState::deserialize(page).map_err(|error| {
        protocol(
            status,
            format!("outcome.value.page is not canonical: {error}"),
        )
    })?;
    if page.sort_revision == 0 || page.total.is_some_and(|total| page.returned > total) {
        return Err(protocol(status, "outcome.value.page range is inconsistent"));
    }
    Ok(())
}

fn validate_success_envelope(envelope: &Value, status: u16) -> Result<SuccessShape, ClientError> {
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
    let outcome_kind = match string(outcome.get("outcome"), status, "outcome.outcome")? {
        "evidence" => OutcomeKind::Evidence,
        "preview" => OutcomeKind::Preview,
        "effect" => OutcomeKind::Effect,
        _ => return Err(protocol(status, "outcome discriminator is not canonical")),
    };
    let value = object(outcome.get("value"), status, "outcome.value")?;
    let Some(payload) = value.get("payload") else {
        return Err(protocol(status, "outcome.value.payload is required"));
    };
    let termination = validate_receipt(value.get("execution"), status, "outcome.value.execution")?;
    let termination = TerminalState::ALL
        .into_iter()
        .find(|state| state.as_str() == termination)
        .ok_or_else(|| protocol(status, "execution.termination is not canonical"))?;
    match outcome_kind {
        OutcomeKind::Evidence => {
            if termination == TerminalState::Completed && payload.is_null() {
                return Err(protocol(
                    status,
                    "a completed evidence read must carry its payload",
                ));
            }
            object(value.get("temporal"), status, "outcome.value.temporal")?;
            object(value.get("authority"), status, "outcome.value.authority")?;
            for field in [
                "evidence_authorities",
                "omissions",
                "scores",
                "contributions",
            ] {
                array(value.get(field), status, &format!("outcome.value.{field}"))?;
            }
            object(value.get("coverage"), status, "outcome.value.coverage")?;
            validate_page(value.get("page"), status)?;
        }
        OutcomeKind::Preview => {
            for field in [
                "preview_id",
                "preview_digest",
                "effect_class",
                "expected_state",
            ] {
                string(value.get(field), status, &format!("outcome.value.{field}"))?;
            }
            object(value.get("authority"), status, "outcome.value.authority")?;
        }
        OutcomeKind::Effect => {
            for field in [
                "effect_id",
                "effect_class",
                "idempotency_key",
                "expected_state",
                "reconciliation",
            ] {
                string(value.get(field), status, &format!("outcome.value.{field}"))?;
            }
            object(value.get("authority"), status, "outcome.value.authority")?;
            object(value.get("receipt"), status, "outcome.value.receipt")?;
        }
    }
    Ok(SuccessShape {
        outcome_kind,
        termination,
    })
}

/// The application outcome discriminator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutcomeKind {
    Evidence,
    Preview,
    Effect,
}

impl OutcomeKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Evidence => "evidence",
            Self::Preview => "preview",
            Self::Effect => "effect",
        }
    }
}

/// What structural validation established about a success envelope.
#[derive(Clone, Copy, Debug)]
struct SuccessShape {
    outcome_kind: OutcomeKind,
    termination: TerminalState,
}

/// Appends each segment as one percent-encoded path segment, so an identifier
/// containing `/`, `?`, `#`, or `%` can never add, split, or rewrite a route.
fn with_path_segments<'a>(
    mut url: Url,
    segments: impl IntoIterator<Item = &'a str>,
) -> Result<Url, ClientError> {
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
    let valid =
        is_bounded_opaque(value, maximum) && !value.contains('/') && value != "." && value != "..";
    if valid {
        Ok(())
    } else {
        Err(ClientError::InvalidRequest(format!(
            "{field} is not a canonical opaque value"
        )))
    }
}

/// Non-empty, untrimmed-free, bounded, and free of control characters.
fn is_bounded_opaque(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.len() <= maximum
        && !value.chars().any(char::is_control)
}
