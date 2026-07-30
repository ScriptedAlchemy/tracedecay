//! Blocking local/remote client for the canonical HTTP and SSE lifecycle.

use std::error::Error;
use std::fmt;
use std::io::{BufRead, BufReader};
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::blocking::{Client as HttpClient, Response};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, ORIGIN};
use serde::Deserialize;
use serde_json::Value;
use tracedecay_api::HttpApplicationOperation;

const MAX_PAGE_SIZE: u32 = 1_000;
const MAX_OPAQUE_BYTES: usize = 4_096;
const MAX_REQUEST_ID_BYTES: usize = 512;

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
            mode: self.mode,
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
    mode: ConnectionMode,
}

impl Client {
    pub fn builder(mode: ConnectionMode) -> ClientBuilder {
        ClientBuilder {
            mode,
            origin: None,
            timeout: Duration::from_secs(30),
        }
    }

    pub fn connection_mode(&self) -> &ConnectionMode {
        &self.mode
    }

    /// Invokes one member of the closed canonical 64-operation HTTP inventory.
    pub fn call(
        &self,
        operation: HttpApplicationOperation,
        request: &Value,
        options: RequestOptions,
    ) -> Result<ApplicationResponse, ClientError> {
        let mut url = reqwest::Url::parse(&format!(
            "{}{}",
            self.application_root,
            operation.route_path()
        ))
        .map_err(|error| ClientError::InvalidConfiguration(error.to_string()))?;
        apply_page_options(&mut url, options.page.as_ref())?;
        let response = self
            .http
            .post(url)
            .headers(self.headers("application/json"))
            .json(request)
            .send()
            .map_err(ClientError::transport)?;
        self.decode_application_response(response)
    }

    /// Requests cancellation of a previously accepted operation.
    pub fn cancel_operation(
        &self,
        operation_id: &str,
        _options: Option<RequestOptions>,
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
        let value: OperationCancellation = response.json().map_err(ClientError::transport)?;
        let valid_status = matches!(
            (status, value.status.as_str()),
            (StatusCode::ACCEPTED, "requested")
                | (StatusCode::OK, "already_requested")
                | (StatusCode::OK, "already_terminal")
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
        let media_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim);
        if media_type != Some("application/json") {
            return Err(ClientError::Protocol {
                status: Some(status.as_u16()),
                message: "daemon response is not application/json".into(),
            });
        }
        let body: Value = response.json().map_err(ClientError::transport)?;
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
            Some("problem") if !status.is_success() => {
                let value = body
                    .get("value")
                    .cloned()
                    .ok_or_else(|| ClientError::Protocol {
                        status: Some(status.as_u16()),
                        message: "problem envelope has no value".into(),
                    })?;
                Err(ClientError::Problem(ProblemError::new(
                    status.as_u16(),
                    value,
                )?))
            }
            _ => Err(ClientError::Protocol {
                status: Some(status.as_u16()),
                message: "daemon returned an inconsistent HTTP envelope".into(),
            }),
        }
    }
}

/// Canonical query paging controls.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageOptions {
    pub size: Option<u32>,
    pub cursor: Option<String>,
}

/// Per-request lifecycle controls.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestOptions {
    pub page: Option<PageOptions>,
}

/// A decoded successful application envelope.
#[derive(Clone, Debug, PartialEq)]
pub struct ApplicationResponse {
    status: u16,
    envelope: Value,
}

impl ApplicationResponse {
    fn new(envelope: Value, status: u16) -> Result<Self, ClientError> {
        let valid = envelope.get("request_id").and_then(Value::as_str).is_some()
            && envelope.get("outcome").and_then(Value::as_object).is_some();
        if !valid {
            return Err(ClientError::Protocol {
                status: Some(status),
                message: "daemon returned a malformed success envelope".into(),
            });
        }
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

/// Canonical cancellation acknowledgement.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct OperationCancellation {
    pub status: String,
    #[serde(flatten)]
    pub details: serde_json::Map<String, Value>,
}

/// Resume frontier supplied by an earlier stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamResume {
    pub token: String,
    pub next_sequence: u64,
}

/// Streaming and reconnection policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamOptions {
    pub resume: Option<StreamResume>,
    pub max_reconnects: usize,
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            resume: None,
            max_reconnects: 0,
        }
    }
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
        matches!(
            self.event.as_str(),
            "completed" | "cancelled" | "timed_out" | "failed" | "partial" | "effect_unknown"
        )
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
        let media_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim);
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
                    if let Some(frontier) = data.get("data").and_then(|value| value.get("frontier"))
                    {
                        self.next_sequence = frontier.get("next_sequence").and_then(Value::as_u64);
                        self.resume_token = frontier
                            .get("resume_token")
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                    }
                } else if let Some(sequence) = data
                    .get("data")
                    .and_then(|value| value.get("sequence"))
                    .and_then(Value::as_u64)
                {
                    if id.as_deref() != Some(sequence.to_string().as_str()) {
                        return Err(ClientError::Protocol {
                            status: None,
                            message: "SSE ID disagrees with its canonical sequence".into(),
                        });
                    }
                    self.next_sequence = sequence.checked_add(1);
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
    pub retry: Option<String>,
    pub envelope: Value,
}

impl ProblemError {
    fn new(status: u16, envelope: Value) -> Result<Self, ClientError> {
        let (kind, code, message, retry) = {
            let problem = envelope
                .get("problem")
                .ok_or_else(|| ClientError::Protocol {
                    status: Some(status),
                    message: "problem envelope has no problem record".into(),
                })?;
            let field = |name: &str| {
                problem
                    .get(name)
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| ClientError::Protocol {
                        status: Some(status),
                        message: format!("problem record has no {name}"),
                    })
            };
            (
                field("kind")?,
                field("code")?,
                field("message")?,
                problem
                    .get("retry")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            )
        };
        Ok(Self {
            status,
            kind,
            code,
            message,
            retry,
            envelope,
        })
    }
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
    Problem(ProblemError),
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

fn apply_page_options(
    url: &mut reqwest::Url,
    page: Option<&PageOptions>,
) -> Result<(), ClientError> {
    let Some(page) = page else {
        return Ok(());
    };
    if let Some(size) = page.size {
        if size == 0 || size > MAX_PAGE_SIZE {
            return Err(ClientError::InvalidRequest(format!(
                "page size must be between 1 and {MAX_PAGE_SIZE}"
            )));
        }
        url.query_pairs_mut()
            .append_pair("page_size", &size.to_string());
    }
    if let Some(cursor) = &page.cursor {
        validate_opaque(cursor, MAX_OPAQUE_BYTES, "page cursor")?;
        url.query_pairs_mut().append_pair("cursor", cursor);
    }
    Ok(())
}
