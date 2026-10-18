//! Sans-IO operation-event stream.
//!
//! One implementation of the canonical lifecycle stream: SSE framing, frame
//! validation, sequence and frontier accounting, and the bounded reconnect
//! policy. It performs no I/O. A driver (the blocking [`crate::client`]
//! iterator or the asynchronous [`crate::async_stream`] stream) asks
//! [`StreamSession::next_action`] what to do, performs it, and reports the
//! outcome, so every transport observes the same terminal, retry, and
//! validation behavior.

use std::collections::VecDeque;
use std::time::Duration;

use serde_json::{Map, Value};
use tracedecay_tool_catalog::TerminalState;

use crate::client::{
    ClientError, MAX_OPAQUE_BYTES, MAX_REQUEST_ID_BYTES, StreamEvent, StreamOptions, problem_error,
    protocol, statusless_protocol, validate_opaque, validate_receipt,
};

/// Bound on one raw SSE line and on the event/id/data bytes retained for one
/// frame before JSON decoding. Every canonical frame is a single JSON object of
/// bounded metadata, request and operation identifiers (≤ 512 bytes), a
/// sequence, a frontier with a 32-byte resume key, or a terminal receipt, so
/// 64 KiB leaves wide headroom while capping what a peer can make the client
/// retain.
pub(crate) const MAX_SSE_FRAME_BYTES: usize = 64 * 1024;
/// The largest delay an SSE `retry:` field can request (the `setTimeout`
/// ceiling, so a host driver can always honor it).
const MAX_RETRY_MILLIS: u64 = 2_147_483_647;

/// What the driver does next.
#[derive(Debug)]
pub(crate) enum Action {
    /// Hand the caller this item. After an `Err` the stream is finished.
    Deliver(Result<StreamEvent, ClientError>),
    /// Open (or reopen) the event connection after waiting `delay`. Report the
    /// result with [`StreamSession::connected`] or
    /// [`StreamSession::connect_failed`].
    Connect { delay: Duration },
    /// Read the next body chunk. Report [`StreamSession::chunk`],
    /// [`StreamSession::end_of_body`], or [`StreamSession::read_failed`].
    Read,
    /// The stream is finished and the connection, if any, must be dropped.
    Done,
}

/// How a connect response is handled.
#[derive(Debug)]
pub(crate) enum ConnectClass {
    /// A canonical event stream: read its body.
    Stream,
    /// A JSON problem body: read it whole and pass it to
    /// [`problem_from_body`].
    ProblemBody,
    /// Refused before reading a body.
    Refused(ClientError),
}

/// Classifies an event-stream response by status and media type.
pub(crate) fn classify_connect(status: u16, media_type: Option<&str>) -> ConnectClass {
    let is_media = |expected: &str| media_type.is_some_and(|m| m.eq_ignore_ascii_case(expected));
    let success = (200..300).contains(&status);
    if status == 401 || status == 403 {
        ConnectClass::Refused(ClientError::Authentication(status))
    } else if !success && is_media("application/json") {
        ConnectClass::ProblemBody
    } else if !success || !is_media("text/event-stream") {
        ConnectClass::Refused(protocol(
            status,
            "daemon did not open a canonical event stream",
        ))
    } else {
        ConnectClass::Stream
    }
}

/// The client error for a JSON problem body returned instead of a stream.
pub(crate) fn problem_from_body(status: u16, body: &[u8]) -> ClientError {
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
        problem_error(&body, status, "stream problem envelope has no value")
    } else {
        protocol(status, "daemon returned an unknown stream problem envelope")
    }
}

/// One dispatched SSE frame.
#[derive(Debug)]
struct RawFrame {
    event: String,
    id: Option<String>,
    data: String,
    retry_millis: Option<u64>,
}

/// Incremental, bounded SSE decoder (WHATWG framing: LF, CR, and CRLF line
/// ends, a leading byte-order mark, comments, `retry`, and an `id` that
/// persists until replaced).
#[derive(Debug, Default)]
struct SseDecoder {
    line: Vec<u8>,
    skip_lf: bool,
    started: bool,
    event: Option<String>,
    frame_id: Option<String>,
    last_id: Option<String>,
    data: Option<String>,
    retry_millis: Option<u64>,
}

impl SseDecoder {
    fn feed(&mut self, bytes: &[u8], frames: &mut Vec<RawFrame>) -> Result<(), ClientError> {
        for &byte in bytes {
            if std::mem::take(&mut self.skip_lf) && byte == b'\n' {
                continue;
            }
            match byte {
                b'\r' | b'\n' => {
                    self.skip_lf = byte == b'\r';
                    self.finish_line(frames)?;
                }
                _ => {
                    // The terminator counts toward the bound, so a line is
                    // oversized once its content reaches the limit.
                    if self.line.len() + 1 >= MAX_SSE_FRAME_BYTES {
                        return Err(Self::too_large());
                    }
                    self.line.push(byte);
                }
            }
        }
        Ok(())
    }

    /// Ends the body: a final unterminated line is still a line, and a frame
    /// that never reached its blank line is a protocol error.
    fn finish(&mut self, frames: &mut Vec<RawFrame>) -> Result<(), ClientError> {
        if !self.line.is_empty() {
            self.finish_line(frames)?;
        }
        if self.event.is_some() || self.data.is_some() || self.frame_id.is_some() {
            return Err(statusless_protocol(
                "event stream ended inside an SSE frame",
            ));
        }
        Ok(())
    }

    fn too_large() -> ClientError {
        ClientError::StreamFrameTooLarge {
            limit_bytes: MAX_SSE_FRAME_BYTES,
        }
    }

    fn pending_frame_bytes(&self) -> usize {
        self.event.as_deref().map_or(0, str::len)
            + self.frame_id.as_deref().map_or(0, str::len)
            + self.data.as_deref().map_or(0, str::len)
    }

    fn finish_line(&mut self, frames: &mut Vec<RawFrame>) -> Result<(), ClientError> {
        let bytes = std::mem::take(&mut self.line);
        let mut line = std::str::from_utf8(&bytes)
            .map_err(|_| statusless_protocol("event stream is not valid UTF-8"))?;
        if !std::mem::replace(&mut self.started, true) {
            line = line.strip_prefix('\u{feff}').unwrap_or(line);
        }
        if line.is_empty() {
            self.dispatch(frames);
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = line.split_once(':').map_or((line, ""), |(field, value)| {
            (field, value.strip_prefix(' ').unwrap_or(value))
        });
        // Account before extending: the joining `\n` counts too.
        if matches!(field, "event" | "id" | "data")
            && self.pending_frame_bytes() + value.len() + 1 > MAX_SSE_FRAME_BYTES
        {
            return Err(Self::too_large());
        }
        match field {
            "event" => self.event = Some(value.to_owned()),
            "id" if !value.contains('\0') => {
                self.frame_id = Some(value.to_owned());
                self.last_id = Some(value.to_owned());
            }
            "data" => match &mut self.data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => self.data = Some(value.to_owned()),
            },
            "retry" if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => {
                self.retry_millis = Some(
                    value
                        .parse()
                        .map_or(MAX_RETRY_MILLIS, |millis: u64| millis.min(MAX_RETRY_MILLIS)),
                );
            }
            _ => {}
        }
        Ok(())
    }

    fn dispatch(&mut self, frames: &mut Vec<RawFrame>) {
        self.frame_id = None;
        let event = self.event.take();
        let Some(data) = self.data.take() else {
            return;
        };
        frames.push(RawFrame {
            event: event.unwrap_or_else(|| "message".into()),
            id: self.last_id.clone(),
            data,
            retry_millis: self.retry_millis,
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Connection {
    /// A connect is due.
    Closed,
    Open,
    /// The stream is finished; nothing more is read or reconnected.
    Finished,
}

/// The lifecycle state of one operation-event stream.
#[derive(Debug)]
pub(crate) struct StreamSession {
    operation_id: String,
    max_reconnects: usize,
    reconnects: usize,
    first_connect_pending: bool,
    connection: Connection,
    decoder: SseDecoder,
    /// Next sequence the server frontier expects, with its resume token.
    next_sequence: Option<u64>,
    resume_token: Option<String>,
    /// The last sequence accepted; later frames must be contiguous with it.
    last_sequence: Option<u64>,
    reconnect_delay: Duration,
    ready: VecDeque<StreamEvent>,
    failure: Option<ClientError>,
}

impl StreamSession {
    pub(crate) fn new(operation_id: &str, options: &StreamOptions) -> Result<Self, ClientError> {
        validate_opaque(operation_id, MAX_REQUEST_ID_BYTES, "operation ID")?;
        if let Some(resume) = &options.resume {
            validate_opaque(&resume.token, MAX_OPAQUE_BYTES, "resume token")?;
        }
        Ok(Self {
            operation_id: operation_id.to_owned(),
            max_reconnects: options.max_reconnects,
            reconnects: 0,
            first_connect_pending: true,
            connection: Connection::Closed,
            decoder: SseDecoder::default(),
            next_sequence: options.resume.as_ref().map(|resume| resume.next_sequence),
            resume_token: options.resume.as_ref().map(|resume| resume.token.clone()),
            last_sequence: options
                .resume
                .as_ref()
                .and_then(|resume| resume.next_sequence.checked_sub(1)),
            reconnect_delay: Duration::ZERO,
            ready: VecDeque::new(),
            failure: None,
        })
    }

    pub(crate) fn operation_id(&self) -> &str {
        &self.operation_id
    }

    /// The `next_sequence` and `resume_token` query parameters of the next
    /// connection.
    pub(crate) fn resume_query(&self) -> (Option<u64>, Option<&str>) {
        (self.next_sequence, self.resume_token.as_deref())
    }

    /// Whether a connection is open, so a driver drops its body when not.
    pub(crate) fn is_open(&self) -> bool {
        self.connection == Connection::Open
    }

    pub(crate) fn next_action(&mut self) -> Action {
        if let Some(event) = self.ready.pop_front() {
            return Action::Deliver(Ok(event));
        }
        if let Some(error) = self.failure.take() {
            self.connection = Connection::Finished;
            return Action::Deliver(Err(error));
        }
        match self.connection {
            Connection::Finished => Action::Done,
            Connection::Open => Action::Read,
            Connection::Closed => Action::Connect {
                delay: if self.first_connect_pending {
                    Duration::ZERO
                } else {
                    self.reconnect_delay
                },
            },
        }
    }

    pub(crate) fn connected(&mut self) {
        self.first_connect_pending = false;
        self.connection = Connection::Open;
        self.decoder = SseDecoder::default();
    }

    pub(crate) fn connect_failed(&mut self, error: ClientError) {
        // The first connection is never retried, and only transport failures
        // are: authentication, problem, and protocol errors surface at once.
        if self.first_connect_pending || !matches!(error, ClientError::Transport(_)) {
            self.fail(error);
        } else {
            self.disconnected(error);
        }
    }

    pub(crate) fn chunk(&mut self, bytes: &[u8]) {
        let mut frames = Vec::new();
        let decoded = self.decoder.feed(bytes, &mut frames);
        self.accept(frames, decoded);
    }

    pub(crate) fn end_of_body(&mut self) {
        let mut frames = Vec::new();
        let decoded = self.decoder.finish(&mut frames);
        self.accept(frames, decoded);
        if self.connection == Connection::Open && self.failure.is_none() {
            self.disconnected(ClientError::Transport(
                "event stream ended before a terminal event".into(),
            ));
        }
    }

    pub(crate) fn read_failed(&mut self, error: ClientError) {
        self.disconnected(error);
    }

    fn accept(&mut self, frames: Vec<RawFrame>, decoded: Result<(), ClientError>) {
        for frame in frames {
            match self.accept_frame(frame) {
                Ok(Some(event)) => {
                    let terminal = event.terminal();
                    self.ready.push_back(event);
                    if terminal {
                        self.connection = Connection::Finished;
                        return;
                    }
                }
                Ok(None) => {}
                Err(error) => return self.fail(error),
            }
        }
        if let Err(error) = decoded {
            self.fail(error);
        }
    }

    fn fail(&mut self, error: ClientError) {
        self.connection = Connection::Finished;
        self.failure.get_or_insert(error);
    }

    /// Records a lost connection and either schedules a reconnect or fails.
    fn disconnected(&mut self, cause: ClientError) {
        if self.reconnects >= self.max_reconnects {
            let cause = match cause {
                ClientError::Transport(message) => ClientError::Transport(format!(
                    "{message} after {} reconnects",
                    self.reconnects
                )),
                other => other,
            };
            return self.fail(cause);
        }
        if self.next_sequence.is_none() || self.resume_token.is_none() {
            return self.fail(statusless_protocol(
                "stream ended without a resumable frontier",
            ));
        }
        self.reconnects += 1;
        self.connection = Connection::Closed;
    }

    fn accept_frame(&mut self, frame: RawFrame) -> Result<Option<StreamEvent>, ClientError> {
        if let Some(millis) = frame.retry_millis {
            self.reconnect_delay = Duration::from_millis(millis);
        }
        let data: Value = serde_json::from_str(&frame.data)
            .map_err(|error| statusless_protocol(format!("SSE data is not JSON: {error}")))?;
        let event_name = frame.event;
        if data.get("event").and_then(Value::as_str) != Some(event_name.as_str()) {
            return Err(statusless_protocol(
                "SSE event name disagrees with its JSON payload",
            ));
        }
        if event_name == "open" {
            if frame.id.is_some() {
                return Err(statusless_protocol("SSE open event must not carry an ID"));
            }
            self.accept_open(&data)?;
            return Ok(Some(StreamEvent {
                event: event_name,
                id: None,
                data,
            }));
        }
        if !matches!(event_name.as_str(), "item" | "progress" | "resume_gap")
            && !is_terminal_event(&event_name)
        {
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
        if frame.id.as_deref() != Some(sequence.to_string().as_str()) {
            return Err(statusless_protocol(
                "SSE ID disagrees with its canonical sequence",
            ));
        }
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
            "resume_gap" => {}
            event if is_terminal_event(event) => validate_terminal(event_data, event)?,
            _ => {}
        }
        let (last_covered, gap_token) = if event_name == "resume_gap" {
            let (last, token) = validate_gap(sequence, event_data)?;
            (last, Some(token))
        } else {
            (sequence, None)
        };
        // Replays after a reconnect may repeat frames already delivered.
        if self.last_sequence.is_some_and(|last| sequence <= last) {
            return Ok(None);
        }
        if let Some(last) = self.last_sequence
            && Some(sequence) != last.checked_add(1)
        {
            return Err(statusless_protocol(
                "SSE event sequence is not contiguous with the delivered stream",
            ));
        }
        if let Some(token) = gap_token {
            self.resume_token = token;
        }
        self.last_sequence = Some(last_covered);
        self.next_sequence = Some(
            last_covered
                .checked_add(1)
                .ok_or_else(|| statusless_protocol("SSE sequence overflowed"))?,
        );
        Ok(Some(StreamEvent {
            event: event_name,
            id: frame.id,
            data,
        }))
    }

    fn accept_open(&mut self, data: &Value) -> Result<(), ClientError> {
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
        let next = unsigned_field(
            frontier,
            "next_sequence",
            "SSE open frontier has no next sequence",
        )?;
        let retained = unsigned_field(
            frontier,
            "retained_from_sequence",
            "SSE open frontier has no retained sequence",
        )?;
        if retained > next {
            return Err(statusless_protocol(
                "SSE open frontier sequence range is inconsistent",
            ));
        }
        self.next_sequence = Some(next);
        self.resume_token = match frontier.get("resume_token") {
            Some(Value::Null) => None,
            Some(Value::String(token)) if !token.is_empty() => Some(token.clone()),
            _ => {
                return Err(statusless_protocol(
                    "SSE open frontier resume token is malformed",
                ));
            }
        };
        Ok(())
    }
}

/// Validates a gap against the event that carries it. Returns the last
/// sequence the gap covers and the resume token of its frontier.
fn validate_gap(
    sequence: u64,
    event_data: &Map<String, Value>,
) -> Result<(u64, Option<String>), ClientError> {
    let gap = event_data
        .get("gap")
        .and_then(Value::as_object)
        .ok_or_else(|| statusless_protocol("SSE resume gap is malformed"))?;
    let first = unsigned_field(
        gap,
        "first_missing_sequence",
        "SSE resume gap has no first sequence",
    )?;
    let last = unsigned_field(
        gap,
        "last_missing_sequence",
        "SSE resume gap has no last sequence",
    )?;
    let frontier = gap
        .get("frontier")
        .and_then(Value::as_object)
        .ok_or_else(|| statusless_protocol("SSE resume gap frontier is malformed"))?;
    let next = unsigned_field(
        frontier,
        "next_sequence",
        "SSE resume gap frontier has no next sequence",
    )?;
    let retained = unsigned_field(
        frontier,
        "retained_from_sequence",
        "SSE resume gap frontier has no retained sequence",
    )?;
    let token = match frontier.get("resume_token") {
        Some(Value::Null) => Some(None),
        Some(Value::String(token)) => Some(Some(token.clone())),
        _ => None,
    };
    match token {
        Some(token) if first == sequence && first <= last && retained <= next => Ok((last, token)),
        _ => Err(statusless_protocol(
            "SSE resume gap range or frontier is malformed",
        )),
    }
}

fn unsigned_field(
    object: &Map<String, Value>,
    field: &str,
    missing: &'static str,
) -> Result<u64, ClientError> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| statusless_protocol(missing))
}

/// Whether an event name ends the operation's stream.
pub(crate) fn is_terminal_event(event: &str) -> bool {
    TerminalState::ALL
        .iter()
        .any(|state| state.as_str() == event)
}

fn validate_terminal(event_data: &Map<String, Value>, event_name: &str) -> Result<(), ClientError> {
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
