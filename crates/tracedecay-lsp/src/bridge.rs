//! Transport-only stdio bridge for a daemon-owned LSP session.
//!
//! The bridge preserves opaque LSP JSON-RPC payload bytes while moving them
//! between host stdio and one typed daemon session. It intentionally opens no
//! project/profile database, selects no root, starts no analyzer, merges no
//! diagnostics, and makes no capability or routing decisions.

use std::collections::VecDeque;
use std::io::{self, ErrorKind, Read, Write};

use futures_util::StreamExt;
use tokio::io::AsyncRead;
use tokio::time::Instant;
use tokio_util::bytes::{Buf, BytesMut};
use tokio_util::codec::{Decoder, Encoder, FramedRead};

/// Hard JSON-RPC payload limit for one bridged frame.
pub const MAX_LSP_FRAME_BYTES: usize = 4 * 1024 * 1024;
/// A framing header is metadata, not an unbounded side channel.
pub const MAX_LSP_HEADER_BYTES: usize = 16 * 1024;
const STDIO_READ_BUFFER_BYTES: usize = 8 * 1024;

/// An opaque LSP JSON-RPC payload. Framing adapters remove and restore the
/// `Content-Length` envelope; the bridge never parses the JSON body.
pub type LspFrame = Vec<u8>;

/// Strict LSP `Content-Length` envelope failures.
///
/// The bridge deliberately reports framing failures before any payload reaches
/// the daemon session. JSON-RPC parsing is a daemon-session concern, so a
/// syntactically valid frame can still contain an invalid JSON-RPC request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContentLengthCodecError {
    HeaderTooLarge { limit: usize },
    MalformedHeader,
    MissingContentLength,
    DuplicateContentLength,
    InvalidContentLength,
    FrameTooLarge { size: usize, limit: usize },
    UnexpectedEof,
}

/// Incremental strict LSP `Content-Length` decoder and encoder.
///
/// LSP requires CRLF-delimited headers. We accept the optional standard
/// `Content-Type` header, but reject duplicate or unknown headers rather than
/// silently choosing an interpretation. This prevents framing desynchronizing
/// a reconnecting bridge after malformed input.
#[derive(Clone, Copy, Debug, Default)]
pub struct ContentLengthCodec;

impl ContentLengthCodec {
    pub const fn new() -> Self {
        Self
    }

    /// Encodes an opaque JSON-RPC payload into the standard LSP envelope.
    pub fn encode(frame: &[u8]) -> Result<Vec<u8>, ContentLengthCodecError> {
        let mut encoded = BytesMut::new();
        encode_frame(frame, &mut encoded)?;
        Ok(encoded.to_vec())
    }
}

impl Decoder for ContentLengthCodec {
    type Error = AsyncContentLengthError;
    type Item = LspFrame;

    fn decode(&mut self, input: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        decode_frame(input).map_err(Into::into)
    }

    fn decode_eof(&mut self, input: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if let Some(frame) = self.decode(input)? {
            return Ok(Some(frame));
        }
        if input.is_empty() {
            Ok(None)
        } else {
            Err(ContentLengthCodecError::UnexpectedEof.into())
        }
    }
}

impl Encoder<&[u8]> for ContentLengthCodec {
    type Error = AsyncContentLengthError;

    fn encode(&mut self, frame: &[u8], output: &mut BytesMut) -> Result<(), Self::Error> {
        encode_frame(frame, output).map_err(Into::into)
    }
}

fn decode_frame(input: &mut BytesMut) -> Result<Option<LspFrame>, ContentLengthCodecError> {
    let Some(header_end) = find_header_end(input) else {
        if has_invalid_line_ending(input) || input.len() > MAX_LSP_HEADER_BYTES {
            return Err(if input.len() > MAX_LSP_HEADER_BYTES {
                ContentLengthCodecError::HeaderTooLarge {
                    limit: MAX_LSP_HEADER_BYTES,
                }
            } else {
                ContentLengthCodecError::MalformedHeader
            });
        }
        return Ok(None);
    };
    if header_end > MAX_LSP_HEADER_BYTES {
        return Err(ContentLengthCodecError::HeaderTooLarge {
            limit: MAX_LSP_HEADER_BYTES,
        });
    }

    let body_len = parse_content_length(&input[..header_end])?;
    if body_len > MAX_LSP_FRAME_BYTES {
        return Err(ContentLengthCodecError::FrameTooLarge {
            size: body_len,
            limit: MAX_LSP_FRAME_BYTES,
        });
    }
    let body_start = header_end + 4;
    let Some(message_len) = body_start.checked_add(body_len) else {
        return Err(ContentLengthCodecError::FrameTooLarge {
            size: body_len,
            limit: MAX_LSP_FRAME_BYTES,
        });
    };
    if input.len() < message_len {
        return Ok(None);
    }
    let mut message = input.split_to(message_len);
    message.advance(body_start);
    Ok(Some(message.to_vec()))
}

fn encode_frame(frame: &[u8], output: &mut BytesMut) -> Result<(), ContentLengthCodecError> {
    if frame.len() > MAX_LSP_FRAME_BYTES {
        return Err(ContentLengthCodecError::FrameTooLarge {
            size: frame.len(),
            limit: MAX_LSP_FRAME_BYTES,
        });
    }
    output.extend_from_slice(format!("Content-Length: {}\r\n\r\n", frame.len()).as_bytes());
    output.extend_from_slice(frame);
    Ok(())
}

fn find_header_end(input: &[u8]) -> Option<usize> {
    input.windows(4).position(|window| window == b"\r\n\r\n")
}

fn has_invalid_line_ending(input: &[u8]) -> bool {
    input.iter().enumerate().any(|(index, byte)| match byte {
        b'\n' => index == 0 || input[index - 1] != b'\r',
        b'\r' => input.get(index + 1).is_some_and(|next| *next != b'\n'),
        _ => false,
    })
}

fn parse_content_length(header: &[u8]) -> Result<usize, ContentLengthCodecError> {
    let header =
        std::str::from_utf8(header).map_err(|_| ContentLengthCodecError::MalformedHeader)?;
    let mut content_length = None;
    let mut content_type_seen = false;
    for line in header.split("\r\n") {
        if line.is_empty() {
            return Err(ContentLengthCodecError::MalformedHeader);
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(ContentLengthCodecError::MalformedHeader);
        };
        if !name.is_ascii()
            || !value.is_ascii()
            || name.is_empty()
            || name
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
            || value
                .bytes()
                .any(|byte| byte.is_ascii_control() && byte != b'\t')
        {
            return Err(ContentLengthCodecError::MalformedHeader);
        }
        if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err(ContentLengthCodecError::DuplicateContentLength);
            }
            let value = value.trim();
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(ContentLengthCodecError::InvalidContentLength);
            }
            content_length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| ContentLengthCodecError::InvalidContentLength)?,
            );
        } else if name.eq_ignore_ascii_case("content-type") {
            if content_type_seen || value.trim().is_empty() {
                return Err(ContentLengthCodecError::MalformedHeader);
            }
            content_type_seen = true;
        } else {
            return Err(ContentLengthCodecError::MalformedHeader);
        }
    }
    content_length.ok_or(ContentLengthCodecError::MissingContentLength)
}

/// Failure from the deadline-aware Tokio framing adapter.
#[derive(Debug)]
pub enum AsyncContentLengthError {
    Io(io::Error),
    Codec(ContentLengthCodecError),
    DeadlineElapsed,
}

impl From<io::Error> for AsyncContentLengthError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ContentLengthCodecError> for AsyncContentLengthError {
    fn from(error: ContentLengthCodecError) -> Self {
        Self::Codec(error)
    }
}

/// Reads one framed payload before `deadline`.
///
/// An idle deadline is retryable, while a partial frame at the deadline is a
/// typed failure so callers cannot silently discard an incomplete envelope.
pub async fn read_content_length_frame_until<R>(
    reader: &mut FramedRead<R, ContentLengthCodec>,
    deadline: Instant,
) -> Result<FramePoll, AsyncContentLengthError>
where
    R: AsyncRead + Unpin,
{
    match tokio::time::timeout_at(deadline, reader.next()).await {
        Ok(Some(result)) => result.map(FramePoll::Frame),
        Ok(None) => Ok(FramePoll::Closed),
        Err(_) if reader.read_buffer().is_empty() => Ok(FramePoll::Pending),
        Err(_) => Err(AsyncContentLengthError::DeadlineElapsed),
    }
}

/// I/O failure emitted by the concrete non-blocking stdio framing adapter.
#[derive(Debug)]
pub enum ContentLengthStdioError {
    Io(io::Error),
    Codec(ContentLengthCodecError),
    PendingFrameChanged,
    InternalState,
}

impl From<io::Error> for ContentLengthStdioError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ContentLengthCodecError> for ContentLengthStdioError {
    fn from(error: ContentLengthCodecError) -> Self {
        Self::Codec(error)
    }
}

impl From<AsyncContentLengthError> for ContentLengthStdioError {
    fn from(error: AsyncContentLengthError) -> Self {
        match error {
            AsyncContentLengthError::Io(error) => Self::Io(error),
            AsyncContentLengthError::Codec(error) => Self::Codec(error),
            AsyncContentLengthError::DeadlineElapsed => Self::InternalState,
        }
    }
}

#[derive(Debug)]
struct PendingStdioWrite {
    frame: LspFrame,
    encoded: Vec<u8>,
    offset: usize,
    flushed: bool,
}

/// Concrete non-blocking `Read`/`Write` implementation for an LSP stdio
/// process. The caller supplies handles configured for non-blocking operation;
/// `WouldBlock` is translated to [`FramePoll::Pending`] or
/// [`FrameSend::Backpressured`].
///
/// Partial output is retained exactly once and can only be resumed with the
/// same payload. That invariant prevents a retry from writing a second header
/// in the middle of the first frame.
pub struct ContentLengthStdioTransport<R, W> {
    reader: R,
    writer: W,
    decoder: ContentLengthCodec,
    input: BytesMut,
    pending_write: Option<PendingStdioWrite>,
}

impl<R, W> ContentLengthStdioTransport<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            decoder: ContentLengthCodec::new(),
            input: BytesMut::new(),
            pending_write: None,
        }
    }

    pub fn into_parts(self) -> (R, W) {
        (self.reader, self.writer)
    }
}

impl<R, W> StdioFrameTransport for ContentLengthStdioTransport<R, W>
where
    R: Read,
    W: Write,
{
    type Error = ContentLengthStdioError;

    fn poll_frame(&mut self) -> Result<FramePoll, Self::Error> {
        if let Some(frame) = self.decoder.decode(&mut self.input)? {
            return Ok(FramePoll::Frame(frame));
        }

        let mut buffer = [0_u8; STDIO_READ_BUFFER_BYTES];
        match self.reader.read(&mut buffer) {
            Ok(0) if self.input.is_empty() => Ok(FramePoll::Closed),
            Ok(0) => Err(ContentLengthCodecError::UnexpectedEof.into()),
            Ok(read) => {
                self.input.extend_from_slice(&buffer[..read]);
                Ok(self
                    .decoder
                    .decode(&mut self.input)?
                    .map_or(FramePoll::Pending, FramePoll::Frame))
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => Ok(FramePoll::Pending),
            Err(error) => Err(error.into()),
        }
    }

    fn try_send_frame(&mut self, frame: &[u8]) -> Result<FrameSend, Self::Error> {
        match self.pending_write.as_ref() {
            Some(pending) if pending.frame.as_slice() != frame => {
                return Err(ContentLengthStdioError::PendingFrameChanged);
            }
            Some(_) => {}
            None => {
                self.pending_write = Some(PendingStdioWrite {
                    frame: frame.to_vec(),
                    encoded: ContentLengthCodec::encode(frame)?,
                    offset: 0,
                    flushed: false,
                });
            }
        }

        let Some(pending) = self.pending_write.as_mut() else {
            return Err(ContentLengthStdioError::InternalState);
        };
        if pending.offset < pending.encoded.len() {
            match self.writer.write(&pending.encoded[pending.offset..]) {
                Ok(0) => return Ok(FrameSend::Backpressured),
                Ok(written) => pending.offset += written,
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    return Ok(FrameSend::Backpressured);
                }
                Err(error) => return Err(error.into()),
            }
        }
        if pending.offset < pending.encoded.len() {
            return Ok(FrameSend::Backpressured);
        }
        if !pending.flushed {
            match self.writer.flush() {
                Ok(()) => pending.flushed = true,
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    return Ok(FrameSend::Backpressured);
                }
                Err(error) => return Err(error.into()),
            }
        }
        self.pending_write = None;
        Ok(FrameSend::Sent)
    }
}

/// Result of a non-blocking receive attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FramePoll {
    Frame(LspFrame),
    Pending,
    Closed,
}

/// Result of a non-blocking send attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameSend {
    Sent,
    Backpressured,
    Closed,
}

/// The host-side stdio framing adapter.
///
/// Implementations own strict `Content-Length` parsing and stdout framing.
/// Both methods must be non-blocking so one quiet direction cannot starve the
/// other direction.
pub trait StdioFrameTransport {
    type Error;

    fn poll_frame(&mut self) -> Result<FramePoll, Self::Error>;
    fn try_send_frame(&mut self, frame: &[u8]) -> Result<FrameSend, Self::Error>;
}

/// The daemon-side typed session transport supplied by the daemon client
/// layer. It is not a raw daemon-socket tunnel.
pub trait DaemonLspSessionTransport {
    type Error;

    fn try_send_client_frame(&mut self, frame: &[u8]) -> Result<FrameSend, Self::Error>;
    fn poll_daemon_frame(&mut self) -> Result<FramePoll, Self::Error>;

    /// The bridge calls this only after stdio accepted a daemon-to-client
    /// frame. Implementations may use it to advance bounded publication
    /// delivery state; a default keeps existing transports conservative.
    fn acknowledge_daemon_frame(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Direction associated with a bridge close or frame-limit failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BridgeDirection {
    ClientToDaemon,
    DaemonToClient,
}

/// A transport or protocol-boundary failure from the bridge.
#[derive(Debug, Eq, PartialEq)]
pub enum StdioLspBridgeError<StdioError, DaemonError> {
    Stdio(StdioError),
    Daemon(DaemonError),
    FrameTooLarge {
        direction: BridgeDirection,
        size: usize,
        limit: usize,
    },
}

/// Counts frames forwarded by one bounded, fair bridge pump.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BridgePumpOutcome {
    pub client_to_daemon: usize,
    pub daemon_to_client: usize,
    pub backpressured: bool,
    pub closed: bool,
}

/// A framing bridge between one host process and one daemon LSP session.
///
/// At most one frame per direction is retained while its receiver is
/// backpressured. This prevents loss without allowing an unbounded bridge-local
/// queue. Session admission, cancellation, and JSON-RPC routing remain daemon
/// duties.
pub struct StdioLspBridge<Stdio, Daemon> {
    stdio: Stdio,
    daemon: Daemon,
    client_pending: VecDeque<LspFrame>,
    daemon_pending: VecDeque<LspFrame>,
    closed: bool,
}

impl<Stdio, Daemon> StdioLspBridge<Stdio, Daemon>
where
    Stdio: StdioFrameTransport,
    Daemon: DaemonLspSessionTransport,
{
    pub fn new(stdio: Stdio, daemon: Daemon) -> Self {
        Self {
            stdio,
            daemon,
            client_pending: VecDeque::with_capacity(1),
            daemon_pending: VecDeque::with_capacity(1),
            closed: false,
        }
    }

    /// Polls and forwards at most one frame in each direction.
    pub fn pump_once(
        &mut self,
    ) -> Result<BridgePumpOutcome, StdioLspBridgeError<Stdio::Error, Daemon::Error>> {
        if self.closed {
            return Ok(BridgePumpOutcome {
                closed: true,
                ..BridgePumpOutcome::default()
            });
        }

        let mut outcome = BridgePumpOutcome::default();
        self.fill_client_slot()?;
        if self.closed {
            self.discard_pending();
            outcome.closed = true;
            return Ok(outcome);
        }
        self.fill_daemon_slot()?;
        if self.closed {
            self.discard_pending();
            outcome.closed = true;
            return Ok(outcome);
        }
        self.flush_client_slot(&mut outcome)?;
        if self.closed {
            self.discard_pending();
            outcome.closed = true;
            return Ok(outcome);
        }
        self.flush_daemon_slot(&mut outcome)?;
        if self.closed {
            self.discard_pending();
        }
        outcome.closed = self.closed;
        Ok(outcome)
    }

    pub fn into_parts(self) -> (Stdio, Daemon) {
        (self.stdio, self.daemon)
    }

    fn fill_client_slot(&mut self) -> Result<(), StdioLspBridgeError<Stdio::Error, Daemon::Error>> {
        if !self.client_pending.is_empty() {
            return Ok(());
        }
        match self
            .stdio
            .poll_frame()
            .map_err(StdioLspBridgeError::Stdio)?
        {
            FramePoll::Frame(frame) => {
                self.validate_size(BridgeDirection::ClientToDaemon, &frame)?;
                self.client_pending.push_back(frame);
            }
            FramePoll::Pending => {}
            FramePoll::Closed => self.closed = true,
        }
        Ok(())
    }

    fn fill_daemon_slot(&mut self) -> Result<(), StdioLspBridgeError<Stdio::Error, Daemon::Error>> {
        if !self.daemon_pending.is_empty() {
            return Ok(());
        }
        match self
            .daemon
            .poll_daemon_frame()
            .map_err(StdioLspBridgeError::Daemon)?
        {
            FramePoll::Frame(frame) => {
                self.validate_size(BridgeDirection::DaemonToClient, &frame)?;
                self.daemon_pending.push_back(frame);
            }
            FramePoll::Pending => {}
            FramePoll::Closed => self.closed = true,
        }
        Ok(())
    }

    fn flush_client_slot(
        &mut self,
        outcome: &mut BridgePumpOutcome,
    ) -> Result<(), StdioLspBridgeError<Stdio::Error, Daemon::Error>> {
        let Some(frame) = self.client_pending.front() else {
            return Ok(());
        };
        match self
            .daemon
            .try_send_client_frame(frame)
            .map_err(StdioLspBridgeError::Daemon)?
        {
            FrameSend::Sent => {
                self.client_pending.pop_front();
                outcome.client_to_daemon = 1;
            }
            FrameSend::Backpressured => outcome.backpressured = true,
            FrameSend::Closed => self.closed = true,
        }
        Ok(())
    }

    fn flush_daemon_slot(
        &mut self,
        outcome: &mut BridgePumpOutcome,
    ) -> Result<(), StdioLspBridgeError<Stdio::Error, Daemon::Error>> {
        let Some(frame) = self.daemon_pending.front() else {
            return Ok(());
        };
        match self
            .stdio
            .try_send_frame(frame)
            .map_err(StdioLspBridgeError::Stdio)?
        {
            FrameSend::Sent => {
                // Stdio has already accepted the bytes. Remove the bridge copy
                // before acknowledging so an acknowledgement failure cannot
                // cause a caller to retry and duplicate the frame on stdout.
                self.daemon_pending.pop_front();
                outcome.daemon_to_client = 1;
                self.daemon
                    .acknowledge_daemon_frame()
                    .map_err(StdioLspBridgeError::Daemon)?;
            }
            FrameSend::Backpressured => outcome.backpressured = true,
            FrameSend::Closed => self.closed = true,
        }
        Ok(())
    }

    fn validate_size(
        &mut self,
        direction: BridgeDirection,
        frame: &[u8],
    ) -> Result<(), StdioLspBridgeError<Stdio::Error, Daemon::Error>> {
        if frame.len() <= MAX_LSP_FRAME_BYTES {
            return Ok(());
        }
        self.closed = true;
        Err(StdioLspBridgeError::FrameTooLarge {
            direction,
            size: frame.len(),
            limit: MAX_LSP_FRAME_BYTES,
        })
    }

    fn discard_pending(&mut self) {
        self.client_pending.clear();
        self.daemon_pending.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Stdio {
        input: VecDeque<FramePoll>,
        output: Vec<LspFrame>,
        block_send: bool,
    }

    impl StdioFrameTransport for Stdio {
        type Error = ();

        fn poll_frame(&mut self) -> Result<FramePoll, Self::Error> {
            Ok(self.input.pop_front().unwrap_or(FramePoll::Pending))
        }

        fn try_send_frame(&mut self, frame: &[u8]) -> Result<FrameSend, Self::Error> {
            if self.block_send {
                return Ok(FrameSend::Backpressured);
            }
            self.output.push(frame.to_vec());
            Ok(FrameSend::Sent)
        }
    }

    #[derive(Default)]
    struct Daemon {
        input: VecDeque<FramePoll>,
        output: Vec<LspFrame>,
        block_send: bool,
        fail_ack: bool,
    }

    impl DaemonLspSessionTransport for Daemon {
        type Error = ();

        fn try_send_client_frame(&mut self, frame: &[u8]) -> Result<FrameSend, Self::Error> {
            if self.block_send {
                return Ok(FrameSend::Backpressured);
            }
            self.output.push(frame.to_vec());
            Ok(FrameSend::Sent)
        }

        fn poll_daemon_frame(&mut self) -> Result<FramePoll, Self::Error> {
            Ok(self.input.pop_front().unwrap_or(FramePoll::Pending))
        }

        fn acknowledge_daemon_frame(&mut self) -> Result<(), Self::Error> {
            if self.fail_ack { Err(()) } else { Ok(()) }
        }
    }

    #[test]
    fn forwards_both_directions_without_inspecting_payloads() {
        let mut stdio = Stdio::default();
        stdio.input.push_back(FramePoll::Frame(vec![0, 1, 2]));
        let mut daemon = Daemon::default();
        daemon.input.push_back(FramePoll::Frame(vec![3, 4, 5]));
        let mut bridge = StdioLspBridge::new(stdio, daemon);

        assert_eq!(
            bridge.pump_once().unwrap(),
            BridgePumpOutcome {
                client_to_daemon: 1,
                daemon_to_client: 1,
                backpressured: false,
                closed: false,
            }
        );
        let (stdio, daemon) = bridge.into_parts();
        assert_eq!(stdio.output, vec![vec![3, 4, 5]]);
        assert_eq!(daemon.output, vec![vec![0, 1, 2]]);
    }

    #[test]
    fn retains_exactly_one_frame_across_backpressure() {
        let mut stdio = Stdio::default();
        stdio.input.push_back(FramePoll::Frame(vec![1]));
        stdio.input.push_back(FramePoll::Frame(vec![2]));
        let daemon = Daemon {
            block_send: true,
            ..Daemon::default()
        };
        let mut bridge = StdioLspBridge::new(stdio, daemon);

        assert!(bridge.pump_once().unwrap().backpressured);
        bridge.daemon.block_send = false;
        assert_eq!(bridge.pump_once().unwrap().client_to_daemon, 1);
        assert_eq!(bridge.daemon.output, vec![vec![1]]);
        assert_eq!(bridge.pump_once().unwrap().client_to_daemon, 1);
        assert_eq!(bridge.daemon.output, vec![vec![1], vec![2]]);
    }

    #[test]
    fn oversized_frame_closes_before_daemon_dispatch() {
        let mut stdio = Stdio::default();
        stdio
            .input
            .push_back(FramePoll::Frame(vec![0; MAX_LSP_FRAME_BYTES + 1]));
        let mut bridge = StdioLspBridge::new(stdio, Daemon::default());

        assert_eq!(
            bridge.pump_once(),
            Err(StdioLspBridgeError::FrameTooLarge {
                direction: BridgeDirection::ClientToDaemon,
                size: MAX_LSP_FRAME_BYTES + 1,
                limit: MAX_LSP_FRAME_BYTES,
            })
        );
        assert!(bridge.daemon.output.is_empty());
        assert!(bridge.pump_once().unwrap().closed);
    }

    #[test]
    fn peer_close_never_sends_a_frame_to_the_closed_transport() {
        let mut stdio = Stdio::default();
        stdio.input.push_back(FramePoll::Closed);
        let mut daemon = Daemon::default();
        daemon.input.push_back(FramePoll::Frame(vec![1]));
        let mut bridge = StdioLspBridge::new(stdio, daemon);

        assert!(bridge.pump_once().unwrap().closed);
        let (stdio, daemon) = bridge.into_parts();
        assert!(stdio.output.is_empty());
        assert!(daemon.output.is_empty());
    }

    #[test]
    fn strict_content_length_codec_handles_split_and_back_to_back_frames() {
        let first = ContentLengthCodec::encode(br#"{"jsonrpc":"2.0"}"#).unwrap();
        let second = ContentLengthCodec::encode(br#"{"method":"initialized"}"#).unwrap();
        let mut input = BytesMut::new();
        input.extend_from_slice(&first[..11]);
        assert_eq!(decode_frame(&mut input), Ok(None));
        input.extend_from_slice(&first[11..]);
        input.extend_from_slice(&second);
        assert_eq!(
            decode_frame(&mut input),
            Ok(Some(br#"{"jsonrpc":"2.0"}"#.to_vec()))
        );
        assert_eq!(
            decode_frame(&mut input),
            Ok(Some(br#"{"method":"initialized"}"#.to_vec()))
        );
        assert_eq!(decode_frame(&mut input), Ok(None));
    }

    #[test]
    fn strict_content_length_codec_rejects_ambiguous_or_malformed_headers() {
        for (wire, expected) in [
            (
                b"Content-Type: application/vscode-jsonrpc\r\n\r\n{}".as_slice(),
                ContentLengthCodecError::MissingContentLength,
            ),
            (
                b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}".as_slice(),
                ContentLengthCodecError::DuplicateContentLength,
            ),
            (
                b"Content-Length: two\r\n\r\n{}".as_slice(),
                ContentLengthCodecError::InvalidContentLength,
            ),
            (
                b"Content-Length: 2\n\n{}".as_slice(),
                ContentLengthCodecError::MalformedHeader,
            ),
            (
                b"Content-Length: 2\rX-Test: value\r\n\r\n{}".as_slice(),
                ContentLengthCodecError::MalformedHeader,
            ),
        ] {
            let mut input = BytesMut::from(wire);
            assert_eq!(decode_frame(&mut input), Err(expected));
        }
    }

    #[derive(Default)]
    struct Reader {
        chunks: VecDeque<Vec<u8>>,
    }

    impl Read for Reader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let Some(mut chunk) = self.chunks.pop_front() else {
                return Err(io::Error::from(ErrorKind::WouldBlock));
            };
            let length = chunk.len().min(buffer.len());
            buffer[..length].copy_from_slice(&chunk[..length]);
            if length < chunk.len() {
                self.chunks.push_front(chunk.split_off(length));
            }
            Ok(length)
        }
    }

    #[derive(Default)]
    struct Writer {
        output: Vec<u8>,
        max_write: usize,
        block_flush: bool,
    }

    impl Write for Writer {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            if self.max_write == 0 {
                return Err(io::Error::from(ErrorKind::WouldBlock));
            }
            let length = self.max_write.min(buffer.len());
            self.output.extend_from_slice(&buffer[..length]);
            Ok(length)
        }

        fn flush(&mut self) -> io::Result<()> {
            if self.block_flush {
                Err(io::Error::from(ErrorKind::WouldBlock))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn concrete_stdio_transport_preserves_partial_output_without_duplicate_headers() {
        let payload = br#"{"jsonrpc":"2.0","method":"initialized"}"#;
        let mut transport = ContentLengthStdioTransport::new(
            Reader::default(),
            Writer {
                max_write: 3,
                ..Writer::default()
            },
        );

        assert!(matches!(
            transport.try_send_frame(payload),
            Ok(FrameSend::Backpressured)
        ));
        transport.writer.max_write = usize::MAX;
        assert!(matches!(
            transport.try_send_frame(payload),
            Ok(FrameSend::Sent)
        ));
        let (_, writer) = transport.into_parts();
        assert_eq!(writer.output, ContentLengthCodec::encode(payload).unwrap());
    }

    #[test]
    fn concrete_stdio_transport_decodes_split_stdio_bytes() {
        let encoded = ContentLengthCodec::encode(br#"{"id":1}"#).unwrap();
        let mut reader = Reader::default();
        reader.chunks.push_back(encoded[..8].to_vec());
        reader.chunks.push_back(encoded[8..].to_vec());
        let mut transport = ContentLengthStdioTransport::new(reader, Writer::default());

        assert!(matches!(transport.poll_frame(), Ok(FramePoll::Pending)));
        assert!(matches!(
            transport.poll_frame(),
            Ok(FramePoll::Frame(frame)) if frame == br#"{"id":1}"#
        ));
    }

    #[test]
    fn stdout_frame_is_not_retained_after_daemon_acknowledgement_failure() {
        let stdio = Stdio::default();
        let mut daemon = Daemon {
            fail_ack: true,
            ..Daemon::default()
        };
        daemon.input.push_back(FramePoll::Frame(vec![1]));
        let mut bridge = StdioLspBridge::new(stdio, daemon);

        assert_eq!(bridge.pump_once(), Err(StdioLspBridgeError::Daemon(())));
        assert!(bridge.daemon_pending.is_empty());
        assert_eq!(bridge.stdio.output, vec![vec![1]]);
    }
}
