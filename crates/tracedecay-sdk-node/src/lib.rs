//! Node-API binding for `@tracedecay/sdk`.
//!
//! The TypeScript facade owns typed request/result shapes; every lifecycle
//! decision (request preparation, envelope and problem admission, SSE framing,
//! the delivered cursor, and reconnects) runs in [`tracedecay_sdk::protocol`],
//! the same code the Rust client uses. This crate only performs async I/O.
//!
//! Ownership at the boundary:
//! - Network waits run on the binding's Tokio runtime, never on the Node
//!   event loop; each JS call resolves a Promise.
//! - `interrupt()` drops the in-flight future, which closes its connection.
//!   It never asks the daemon to cancel the operation; that is
//!   `prepareCancel`.
//! - A stream reads its body only when JS asks for the next event, so a slow
//!   consumer applies TCP backpressure instead of growing a buffer. Closing or
//!   dropping a stream releases its connection.
//! - Errors cross as one JSON object (`kind`, `message`, optional `status` and
//!   problem `envelope`), which the facade maps onto its typed error classes.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::VecDeque;

use napi::bindgen_prelude::Result;
use napi::{Error, Status};
use napi_derive::napi;
use reqwest::Response;
use reqwest::header::CONTENT_TYPE;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracedecay_sdk::client::{
    ClientError, ConnectionMode, OperationRequestOptions, PageOptions, StreamEvent, StreamOptions,
    StreamResume,
};
use tracedecay_sdk::protocol::{
    Endpoint, OperationContract, PreparedOperation, PreparedRequest, ResponseBody, StreamOpening,
    StreamState, admit_json_head, decode_cancellation, refuse_stream_problem, stream_opening,
};

/// Failure delivered to JavaScript.
enum Failure {
    Client(ClientError),
    /// `interrupt()` or `close()` stopped the call before it finished.
    Interrupted,
}

impl From<ClientError> for Failure {
    fn from(error: ClientError) -> Self {
        Self::Client(error)
    }
}

impl From<Failure> for Error {
    fn from(failure: Failure) -> Self {
        let payload = match failure {
            Failure::Interrupted => {
                json!({"kind": "interrupted", "message": "the call was interrupted"})
            }
            Failure::Client(error) => client_error_payload(&error),
        };
        if javascript_numbers(&payload, None).is_err() {
            return Error::new(
                Status::GenericFailure,
                json!({
                    "kind": "protocol",
                    "message": "daemon problem contains an integer outside JavaScript's safe range",
                    "status": payload.get("status"),
                })
                .to_string(),
            );
        }
        Error::new(Status::GenericFailure, payload.to_string())
    }
}

fn client_error_payload(error: &ClientError) -> Value {
    let message = error.to_string();
    match error {
        ClientError::InvalidConfiguration(_) => {
            json!({"kind": "invalid_configuration", "message": message})
        }
        ClientError::InvalidRequest(_) => json!({"kind": "invalid_request", "message": message}),
        ClientError::Transport(_) => json!({"kind": "transport", "message": message}),
        ClientError::MissingMcpTransport { .. } | ClientError::UnsupportedTransport { .. } => {
            json!({"kind": "unsupported_transport", "message": message})
        }
        ClientError::Authentication(status) => {
            json!({"kind": "authentication", "message": message, "status": status})
        }
        ClientError::Protocol { status, .. } => {
            json!({"kind": "protocol", "message": message, "status": status})
        }
        ClientError::StreamFrameTooLarge { limit_bytes } => {
            json!({"kind": "protocol", "message": message, "limitBytes": limit_bytes})
        }
        ClientError::Problem(problem) => json!({
            "kind": "problem",
            "message": message,
            "status": problem.status,
            "envelope": problem.envelope,
        }),
    }
}

fn invalid_request(message: impl Into<String>) -> Failure {
    Failure::Client(ClientError::InvalidRequest(message.into()))
}

/// JSON numbers lose their integer kind when JavaScript parses them. Inspect
/// that kind before crossing the binding, using the typed Rust projection
/// where a schema declares a float (even if its wire spelling is integral).
/// Unknown fields retain their wire numeric kind and are checked as well.
fn javascript_numbers(
    value: &Value,
    canonical: Option<&Value>,
) -> std::result::Result<(), &'static str> {
    const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
    match value {
        Value::Number(number) => {
            let number = canonical.and_then(Value::as_number).unwrap_or(number);
            if number
                .as_u64()
                .is_some_and(|value| value > MAX_SAFE_INTEGER)
                || number
                    .as_i64()
                    .is_some_and(|value| value < -(MAX_SAFE_INTEGER as i64))
            {
                return Err("integer is outside JavaScript's safe range");
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                javascript_numbers(value, canonical.and_then(|value| value.get(index)))?;
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                javascript_numbers(value, canonical.and_then(|value| value.get(key)))?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn invalid_response(status: Option<u16>, message: impl Into<String>) -> ClientError {
    ClientError::Protocol {
        status,
        message: message.into(),
    }
}

fn parse<'a, T: Deserialize<'a>>(text: &'a str, what: &str) -> std::result::Result<T, Failure> {
    serde_json::from_str(text).map_err(|error| invalid_request(format!("{what}: {error}")))
}

/// Connection settings accepted from the facade.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClientConfig {
    base_url: String,
    project_id: String,
    token: String,
    origin: Option<String>,
}

/// Per-call transport controls accepted from the facade.
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CallOptions {
    deadline_micros: Option<i64>,
    request_id: Option<String>,
    page: Option<PageConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PageConfig {
    size: Option<u32>,
    cursor: Option<String>,
}

impl CallOptions {
    fn into_request_options(self) -> std::result::Result<OperationRequestOptions, Failure> {
        let request_id = self
            .request_id
            .map(tracedecay_contracts::RequestId::new)
            .transpose()
            .map_err(|error| invalid_request(format!("requestId is not canonical: {error}")))?;
        Ok(OperationRequestOptions {
            deadline_micros: self.deadline_micros,
            request_id,
            page: self.page.map(|page| PageOptions {
                size: page.size,
                cursor: page.cursor,
            }),
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StreamConfig {
    /// Decimal `u64`; the facade normalizes number, bigint, and string input.
    next_sequence: Option<String>,
    resume_token: Option<String>,
    max_reconnects: Option<u32>,
}

impl StreamConfig {
    fn into_stream_options(self) -> std::result::Result<StreamOptions, Failure> {
        let resume = match (self.resume_token, self.next_sequence) {
            (Some(token), Some(next_sequence)) => Some(StreamResume {
                token,
                next_sequence: next_sequence.parse().map_err(|_| {
                    invalid_request("resume nextSequence exceeds the canonical u64 range")
                })?,
            }),
            (None, None) => None,
            _ => {
                return Err(invalid_request(
                    "a resume requires both resumeToken and nextSequence",
                ));
            }
        };
        Ok(StreamOptions {
            resume,
            max_reconnects: self.max_reconnects.unwrap_or(0) as usize,
        })
    }
}

/// One daemon authority with its connection pool.
#[napi]
pub struct NativeClient {
    http: reqwest::Client,
    endpoint: Endpoint,
}

#[napi]
impl NativeClient {
    #[napi(constructor)]
    pub fn new(config_json: String) -> Result<Self> {
        let config: ClientConfig = parse(&config_json, "client configuration")?;
        // Loopback policy: the bearer token never reaches an ambient proxy.
        let mode = ConnectionMode::local(config.base_url, config.project_id, config.token);
        let endpoint = Endpoint::new(&mode, config.origin.as_deref()).map_err(Failure::from)?;
        let http = reqwest::Client::builder()
            .no_proxy()
            .build()
            .map_err(|error| Failure::Client(ClientError::Transport(error.to_string())))?;
        Ok(Self { http, endpoint })
    }

    /// Validate and prepare one typed HTTP operation. Refusals happen here,
    /// before any I/O.
    #[napi]
    pub fn prepare_operation(
        &self,
        operation_id: String,
        request_json: String,
        options_json: String,
    ) -> Result<NativeCall> {
        let contract = OperationContract::find(&operation_id).ok_or_else(|| {
            Failure::Client(ClientError::InvalidConfiguration(format!(
                "{operation_id} is not an HTTP binding in the canonical SDK registry"
            )))
        })?;
        let request: Value = parse(&request_json, "operation request")?;
        let options =
            parse::<CallOptions>(&options_json, "operation options")?.into_request_options()?;
        let prepared = self
            .endpoint
            .prepare_operation(contract, &request, &options)
            .map_err(Failure::from)?;
        javascript_numbers(&request, Some(prepared.canonical_request()))
            .map_err(invalid_request)?;
        Ok(NativeCall::new(
            self.http.clone(),
            CallKind::Operation(Box::new(prepared)),
        ))
    }

    /// Prepare a cancellation request for an accepted operation.
    #[napi]
    pub fn prepare_cancel(&self, operation_id: String) -> Result<NativeCall> {
        let prepared = self
            .endpoint
            .prepare_cancel(&operation_id)
            .map_err(Failure::from)?;
        Ok(NativeCall::new(
            self.http.clone(),
            CallKind::Cancel(Box::new(prepared)),
        ))
    }

    /// Prepare an event stream; the first `next()` opens the connection.
    #[napi]
    pub fn prepare_stream(
        &self,
        operation_id: String,
        options_json: String,
    ) -> Result<NativeStream> {
        let options =
            parse::<StreamConfig>(&options_json, "stream options")?.into_stream_options()?;
        let state = StreamState::new(&operation_id, options).map_err(Failure::from)?;
        Ok(NativeStream {
            http: self.http.clone(),
            endpoint: self.endpoint.clone(),
            interrupt: CancellationToken::new(),
            inner: Mutex::new(StreamInner {
                state,
                body: None,
                opened: false,
                ready: VecDeque::new(),
                failure: None,
            }),
        })
    }
}

enum CallKind {
    Operation(Box<PreparedOperation>),
    Cancel(Box<PreparedRequest>),
}

/// One prepared request/response exchange.
#[napi]
pub struct NativeCall {
    http: reqwest::Client,
    kind: CallKind,
    interrupt: CancellationToken,
}

impl NativeCall {
    fn new(http: reqwest::Client, kind: CallKind) -> Self {
        Self {
            http,
            kind,
            interrupt: CancellationToken::new(),
        }
    }

    async fn exchange(&self) -> std::result::Result<String, Failure> {
        let prepared = match &self.kind {
            CallKind::Operation(operation) => &operation.request,
            CallKind::Cancel(request) => request.as_ref(),
        };
        let response = send(&self.http, prepared).await?;
        let status = response.status().as_u16();
        let content_type = content_type(&response);
        // Dropping a refused response closes it unread.
        admit_json_head(status, content_type.as_deref())?;
        let body = response.bytes().await.map_err(transport)?;
        let body = ResponseBody {
            status,
            content_type: content_type.as_deref(),
            body: &body,
        };
        let (value, projection) = match &self.kind {
            CallKind::Operation(operation) => {
                let admitted = operation.admit_response(body)?;
                let payload = match admitted.payload() {
                    Some(payload) if !payload.is_null() => operation
                        .contract
                        .canonical_result(payload)
                        .map_err(|message| invalid_response(Some(status), message))?,
                    _ => Value::Null,
                };
                (
                    admitted.envelope,
                    Some(json!({"outcome": {"value": {"payload": payload}}})),
                )
            }
            CallKind::Cancel(_) => {
                let cancellation = decode_cancellation(body)?;
                let mut value = Value::Object(cancellation.details);
                value["status"] = json!(cancellation_status(cancellation.status));
                (value, None)
            }
        };
        javascript_numbers(&value, projection.as_ref())
            .map_err(|message| invalid_response(Some(status), message))?;
        Ok(value.to_string())
    }
}

#[napi]
impl NativeCall {
    /// Perform the exchange. Resolves the validated JSON value as text.
    #[napi]
    pub async fn run(&self) -> Result<String> {
        tokio::select! {
            biased;
            () = self.interrupt.cancelled() => Err(Failure::Interrupted.into()),
            result = self.exchange() => result.map_err(Error::from),
        }
    }

    /// Stop the exchange and close its connection.
    #[napi]
    pub fn interrupt(&self) {
        self.interrupt.cancel();
    }
}

const fn cancellation_status(status: tracedecay_sdk::client::CancellationStatus) -> &'static str {
    use tracedecay_sdk::client::CancellationStatus;
    match status {
        CancellationStatus::Requested => "requested",
        CancellationStatus::AlreadyRequested => "already_requested",
        CancellationStatus::AlreadyTerminal => "already_terminal",
    }
}

struct StreamInner {
    state: StreamState,
    body: Option<Response>,
    opened: bool,
    ready: VecDeque<StreamEvent>,
    /// The failure that ended the chunk whose events are in `ready`.
    failure: Option<ClientError>,
}

/// A resumable operation event stream, read one event per `next()`.
#[napi]
pub struct NativeStream {
    http: reqwest::Client,
    endpoint: Endpoint,
    interrupt: CancellationToken,
    inner: Mutex<StreamInner>,
}

impl NativeStream {
    async fn connect(&self, inner: &mut StreamInner) -> std::result::Result<(), ClientError> {
        inner.body = None;
        let prepared = self.endpoint.prepare_events(&inner.state)?;
        let response = send(&self.http, &prepared).await?;
        let status = response.status().as_u16();
        match stream_opening(status, content_type(&response).as_deref())? {
            StreamOpening::Events => {
                inner.state.connected();
                inner.body = Some(response);
                Ok(())
            }
            StreamOpening::Problem => {
                let body = response.bytes().await.map_err(transport)?;
                Err(refuse_stream_problem(status, &body))
            }
        }
    }

    /// Read until an event is ready, the stream ends, or it fails.
    async fn advance(
        &self,
        inner: &mut StreamInner,
    ) -> std::result::Result<Option<StreamEvent>, ClientError> {
        if !inner.opened {
            inner.opened = true;
            // The first connection is never retried.
            if let Err(error) = self.connect(inner).await {
                return Err(inner.state.fail(error));
            }
        }
        loop {
            if let Some(event) = inner.ready.pop_front() {
                return Ok(Some(event));
            }
            if let Some(failure) = inner.failure.take() {
                return Err(failure);
            }
            if inner.state.is_finished() {
                inner.body = None;
                return Ok(None);
            }
            let failure = match self.read_chunk(inner).await {
                Ok(()) => continue,
                Err(error) => error,
            };
            self.reconnect(inner, failure).await?;
        }
    }

    async fn read_chunk(&self, inner: &mut StreamInner) -> std::result::Result<(), ClientError> {
        let body = inner
            .body
            .as_mut()
            .ok_or_else(|| ClientError::Transport("event stream reader is not connected".into()))?;
        match body.chunk().await.map_err(transport)? {
            None => {
                inner.body = None;
                Err(inner.state.end_of_body())
            }
            Some(chunk) => {
                let (events, error) = inner.state.push_bytes(&chunk);
                inner.ready.extend(events);
                // A terminal or malformed frame ends transport ownership now,
                // even if JS pauses before asking for the buffered event.
                if inner.state.is_finished() || error.is_some() {
                    inner.body = None;
                }
                // Deliver what preceded the failure, then fail.
                inner.failure = error;
                Ok(())
            }
        }
    }

    async fn reconnect(
        &self,
        inner: &mut StreamInner,
        mut failure: ClientError,
    ) -> std::result::Result<(), ClientError> {
        loop {
            inner.body = None;
            inner.state.reconnect(failure)?;
            match self.connect(inner).await {
                Ok(()) => return Ok(()),
                Err(error) => failure = error,
            }
        }
    }
}

#[napi]
impl NativeStream {
    /// The next canonical event as JSON text (`{event, id, data}`), or `null`
    /// once the stream delivered its terminal event.
    #[napi]
    pub async fn next(&self) -> Result<Option<String>> {
        let mut inner = tokio::select! {
            biased;
            () = self.interrupt.cancelled() => return Err(Failure::Interrupted.into()),
            inner = self.inner.lock() => inner,
        };
        let advanced = tokio::select! {
            biased;
            () = self.interrupt.cancelled() => Err(Failure::Interrupted),
            advanced = self.advance(&mut inner) => advanced.map_err(Failure::from),
        };
        if advanced.is_err() {
            inner.body = None;
        }
        let Some(event) = advanced? else {
            return Ok(None);
        };
        let value = json!({"event": event.event, "id": event.id, "data": event.data});
        if let Err(message) = javascript_numbers(&value, None) {
            inner.body = None;
            inner.ready.clear();
            return Err(Failure::from(inner.state.fail(invalid_response(None, message))).into());
        }
        Ok(Some(value.to_string()))
    }

    /// Release the connection. A pending `next()` rejects as interrupted;
    /// the daemon operation is not cancelled.
    #[napi]
    pub fn close(&self) {
        self.interrupt.cancel();
        if let Ok(mut inner) = self.inner.try_lock() {
            inner.body = None;
        }
    }
}

async fn send(
    http: &reqwest::Client,
    prepared: &PreparedRequest,
) -> std::result::Result<Response, ClientError> {
    let mut request = http
        .request(prepared.method.clone(), prepared.url.clone())
        .headers(prepared.headers.clone());
    if let Some(body) = &prepared.body {
        request = request.body(body.clone());
    }
    request.send().await.map_err(transport)
}

fn transport(error: reqwest::Error) -> ClientError {
    ClientError::Transport(error.to_string())
}

fn content_type(response: &Response) -> Option<String> {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}
