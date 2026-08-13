//! End-to-end wire allocation bounds for host stdin versus MCP/daemon
//! JSON-RPC frames.
//!
//! Host-event stdin and durable spool admission stay at
//! [`DEFAULT_MAX_RECORD_BYTES`] (1 MiB). MCP/daemon JSON-RPC frames use the
//! separate [`MAX_MCP_JSONRPC_FRAME_BYTES`] cap so legitimate edit/tool
//! requests are not rejected by host-event record coupling.
//!
//! Oversized outcomes never retain the full payload. Line readers may keep a
//! bounded leading inspect prefix for safe JSON-RPC `id` recovery.
//!
//! Partial-frame state is owned by [`BoundedLineReader`], not by the read
//! future. Every daemon and MCP read loop races its next frame read against
//! shutdown, cancellation, request completion, or owner-open futures inside a
//! `tokio::select!`, so the read future is routinely dropped after it has
//! already consumed bytes from the underlying buffered reader. A reader that
//! accumulated into the future's own stack would silently drop those bytes and
//! resume mid-frame, truncating the request and desynchronizing JSON-RPC
//! framing for every later frame on the same transport. Keeping the
//! accumulator in the reader makes a dropped read future lossless: the next
//! call resumes from exactly the bytes already consumed.

use std::error::Error;
use std::fmt;
use std::io::{self, Read};

use tokio::io::{AsyncBufRead, AsyncBufReadExt};

use super::bounds::DEFAULT_MAX_RECORD_BYTES;

/// Host-event wire byte cap (hook stdin and other host-admission inputs).
///
/// Equal to the durable host-admission spool max record size (1 MiB).
pub const MAX_WIRE_MESSAGE_BYTES: usize = DEFAULT_MAX_RECORD_BYTES;

/// Bounded MCP/daemon JSON-RPC frame cap (16 MiB).
///
/// This aligns JSON-RPC envelopes with the existing bounded JSON record and
/// composer-envelope ceilings (`MAX_JSONL_RECORD_BYTES` and
/// `MAX_COMPOSER_ENVELOPE_BYTES`) while leaving durable host-event records at
/// exactly 1 MiB.
pub const MAX_MCP_JSONRPC_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Max leading bytes retained on oversized MCP/daemon frames for `id` peek.
pub const MCP_OVERSIZE_ID_INSPECT_BYTES: usize = 4096;

/// Stable reason code for oversized wire input (non-durable, non-retryable).
pub const WIRE_RECORD_TOO_LARGE: &str = "wire_record_too_large";

/// Typed read outcome for non-MCP bounded inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireReadOutcome<T> {
    Ready(T),
    Oversized,
}

enum BoundedLineOutcome<T> {
    Ready(T),
    Oversized { inspect_prefix: Vec<u8> },
}

#[derive(Debug)]
struct WireOversizedError {
    inspect_prefix: Vec<u8>,
}

impl fmt::Display for WireOversizedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(WIRE_RECORD_TOO_LARGE)
    }
}

impl Error for WireOversizedError {}

pub fn wire_oversized_io_error() -> io::Error {
    wire_oversized_io_error_with_prefix(Vec::new())
}

pub fn wire_oversized_io_error_with_prefix(mut inspect_prefix: Vec<u8>) -> io::Error {
    bound_inspect_prefix(&mut inspect_prefix);
    io::Error::new(
        io::ErrorKind::InvalidData,
        WireOversizedError { inspect_prefix },
    )
}

/// Returns true when `err` is the typed wire oversized disposition.
pub fn is_wire_oversized_io_error(err: &io::Error) -> bool {
    err.get_ref()
        .and_then(|inner| inner.downcast_ref::<WireOversizedError>())
        .is_some()
        || (err.kind() == io::ErrorKind::InvalidData && err.to_string() == WIRE_RECORD_TOO_LARGE)
}

/// Bounded leading prefix carried by a typed oversized IO error, if any.
pub fn wire_oversized_inspect_prefix(err: &io::Error) -> &[u8] {
    err.get_ref()
        .and_then(|inner| inner.downcast_ref::<WireOversizedError>())
        .map_or(&[], |inner| inner.inspect_prefix.as_slice())
}

fn bound_inspect_prefix(prefix: &mut Vec<u8>) {
    if prefix.len() > MCP_OVERSIZE_ID_INSPECT_BYTES {
        prefix.truncate(MCP_OVERSIZE_ID_INSPECT_BYTES);
        prefix.shrink_to_fit();
    }
}

fn capture_inspect_prefix(retained: &[u8], next: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(MCP_OVERSIZE_ID_INSPECT_BYTES);
    let from_retained = retained.len().min(MCP_OVERSIZE_ID_INSPECT_BYTES);
    out.extend_from_slice(&retained[..from_retained]);
    if out.len() < MCP_OVERSIZE_ID_INSPECT_BYTES {
        let need = MCP_OVERSIZE_ID_INSPECT_BYTES - out.len();
        out.extend_from_slice(&next[..need.min(next.len())]);
    }
    out
}

/// Read until EOF while retaining at most `max_bytes`.
///
/// Streams hostile tails through a fixed scratch buffer and discards them once
/// the cap is exceeded so the retained allocation never grows with attacker
/// input size beyond `max_bytes`.
pub fn read_bounded_to_end(
    reader: &mut impl Read,
    max_bytes: usize,
) -> io::Result<WireReadOutcome<Vec<u8>>> {
    let mut retained = Vec::new();
    let mut scratch = [0_u8; 8192];
    let mut oversized = false;
    loop {
        let read = reader.read(&mut scratch)?;
        if read == 0 {
            break;
        }
        if oversized {
            continue;
        }
        if retained.len().saturating_add(read) > max_bytes {
            oversized = true;
            retained.clear();
            retained.shrink_to_fit();
            continue;
        }
        retained.extend_from_slice(&scratch[..read]);
    }
    if oversized {
        Ok(WireReadOutcome::Oversized)
    } else {
        Ok(WireReadOutcome::Ready(retained))
    }
}

/// UTF-8 variant of [`read_bounded_to_end`].
pub fn read_bounded_to_string(
    reader: &mut impl Read,
    max_bytes: usize,
) -> io::Result<WireReadOutcome<String>> {
    match read_bounded_to_end(reader, max_bytes)? {
        WireReadOutcome::Oversized => Ok(WireReadOutcome::Oversized),
        WireReadOutcome::Ready(bytes) => {
            let text = String::from_utf8(bytes)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "wire_input_not_utf8"))?;
            Ok(WireReadOutcome::Ready(text))
        }
    }
}

/// Cancellation-safe bounded line reader over an [`AsyncBufRead`] source.
///
/// The partial-frame accumulator (`retained`), the overflow latch
/// (`oversized`), and the bounded `inspect_prefix` live in this struct rather
/// than on the read future's stack. Dropping an in-flight read — the normal
/// outcome of losing a `tokio::select!` race against shutdown, cancellation, or
/// a completed handler — therefore loses nothing: bytes already consumed from
/// `inner` stay accumulated here and the next `read_*` call resumes the same
/// frame. Callers that read under `select!` MUST use this type (or a transport
/// that owns one) rather than the free `read_bounded_*` functions, whose state
/// dies with the future.
///
/// State is reset only when a frame terminalizes (newline, EOF, or an oversized
/// verdict), so a reader is immediately reusable for the next frame.
pub struct BoundedLineReader<R> {
    inner: R,
    retained: Vec<u8>,
    oversized: bool,
    inspect_prefix: Vec<u8>,
}

impl<R> BoundedLineReader<R> {
    /// Wrap a buffered source. No frame state is carried across sources.
    pub const fn new(inner: R) -> Self {
        Self {
            inner,
            retained: Vec::new(),
            oversized: false,
            inspect_prefix: Vec::new(),
        }
    }

    /// Borrow the underlying source. Bypassing the reader for a read would
    /// strand any retained partial frame, so this is for non-read access only.
    pub const fn get_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// Recover the underlying source, discarding any retained partial frame.
    pub fn into_inner(self) -> R {
        self.inner
    }

    /// True while bytes of an unterminated frame are held across calls. This is
    /// exactly the state a non-resumable reader would have lost on cancellation.
    #[must_use]
    pub const fn has_partial_frame(&self) -> bool {
        !self.retained.is_empty() || self.oversized
    }

    fn finish_at_eof(&mut self) -> io::Result<BoundedLineOutcome<Option<String>>> {
        if self.oversized {
            return Ok(self.take_oversized());
        }
        if self.retained.is_empty() {
            return Ok(BoundedLineOutcome::Ready(None));
        }
        Ok(BoundedLineOutcome::Ready(Some(self.take_line()?)))
    }

    fn finish_frame(&mut self) -> io::Result<BoundedLineOutcome<Option<String>>> {
        if self.oversized {
            return Ok(self.take_oversized());
        }
        Ok(BoundedLineOutcome::Ready(Some(self.take_line()?)))
    }

    fn take_oversized(&mut self) -> BoundedLineOutcome<Option<String>> {
        self.oversized = false;
        BoundedLineOutcome::Oversized {
            inspect_prefix: std::mem::take(&mut self.inspect_prefix),
        }
    }

    fn take_line(&mut self) -> io::Result<String> {
        take_line_string(std::mem::take(&mut self.retained))
    }
}

impl<R> BoundedLineReader<R>
where
    R: AsyncBufRead + Unpin,
{
    /// Read one MCP/daemon JSON-RPC line with the dedicated frame ceiling.
    ///
    /// Oversized input is discarded through newline/EOF and returned as the
    /// typed IO error carrying only a bounded leading prefix for request-id
    /// inspection.
    pub async fn read_mcp_line(&mut self) -> io::Result<Option<String>> {
        match self.read_bounded(MAX_MCP_JSONRPC_FRAME_BYTES).await? {
            BoundedLineOutcome::Ready(line) => Ok(line),
            BoundedLineOutcome::Oversized { inspect_prefix } => {
                Err(wire_oversized_io_error_with_prefix(inspect_prefix))
            }
        }
    }

    /// Read one newline-delimited frame, retaining at most `max_bytes` of
    /// content (excluding the terminating newline). On overflow, discards until
    /// newline or EOF and reports [`WireReadOutcome::Oversized`].
    #[cfg(any(test, feature = "test-helpers"))]
    pub async fn read_line(
        &mut self,
        max_bytes: usize,
    ) -> io::Result<WireReadOutcome<Option<String>>> {
        match self.read_bounded(max_bytes).await? {
            BoundedLineOutcome::Ready(line) => Ok(WireReadOutcome::Ready(line)),
            BoundedLineOutcome::Oversized { .. } => Ok(WireReadOutcome::Oversized),
        }
    }

    async fn read_bounded(
        &mut self,
        max_bytes: usize,
    ) -> io::Result<BoundedLineOutcome<Option<String>>> {
        loop {
            // Every suspension point in this loop is a cancellation point, and
            // each one is safe: `fill_buf` consumes nothing when its future is
            // dropped, and everything consumed past it has already been folded
            // into `self`.
            let available = self.inner.fill_buf().await?;
            if available.is_empty() {
                return self.finish_at_eof();
            }

            let newline = available.iter().position(|byte| *byte == b'\n');
            let chunk_len = newline.map_or(available.len(), |index| index + 1);
            let content_len = newline.unwrap_or(chunk_len);

            if !self.oversized {
                let room = max_bytes.saturating_sub(self.retained.len());
                if content_len > room {
                    self.inspect_prefix =
                        capture_inspect_prefix(&self.retained, &available[..content_len]);
                    self.oversized = true;
                    self.retained.clear();
                    self.retained.shrink_to_fit();
                } else {
                    self.retained.extend_from_slice(&available[..content_len]);
                }
            }

            self.inner.consume(chunk_len);

            if newline.is_some() {
                return self.finish_frame();
            }
        }
    }
}

/// Read one newline-delimited frame, retaining at most `max_bytes` of content
/// (excluding the terminating newline). On overflow, discards until newline or
/// EOF and returns [`WireReadOutcome::Oversized`].
///
/// The returned future owns the partial-frame accumulator: dropping it mid-frame
/// loses whatever it consumed. Use [`BoundedLineReader`] when the read races
/// other futures.
#[cfg(any(test, feature = "test-helpers"))]
pub async fn read_bounded_line<R>(
    reader: &mut R,
    max_bytes: usize,
) -> io::Result<WireReadOutcome<Option<String>>>
where
    R: AsyncBufRead + Unpin,
{
    BoundedLineReader::new(reader).read_line(max_bytes).await
}

/// Read one MCP/daemon JSON-RPC line with the dedicated frame ceiling.
///
/// Oversized input is discarded through newline/EOF and returned as the typed
/// IO error carrying only a bounded leading prefix for request-id inspection.
///
/// The returned future owns the partial-frame accumulator, so it is NOT
/// cancellation-safe: callers that race it inside `tokio::select!` must either
/// pin one future across the whole wait or, preferably, hold a
/// [`BoundedLineReader`] whose state survives a dropped read.
pub async fn read_bounded_mcp_line<R>(reader: &mut R) -> io::Result<Option<String>>
where
    R: AsyncBufRead + Unpin,
{
    BoundedLineReader::new(reader).read_mcp_line().await
}

fn take_line_string(mut bytes: Vec<u8>) -> io::Result<String> {
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    String::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "wire_input_not_utf8"))
}

/// Map a bounded line read into the historical `Option<String>` transport shape.
#[cfg(any(test, feature = "test-helpers"))]
pub fn line_outcome_to_io(outcome: WireReadOutcome<Option<String>>) -> io::Result<Option<String>> {
    match outcome {
        WireReadOutcome::Ready(line) => Ok(line),
        WireReadOutcome::Oversized => Err(wire_oversized_io_error()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use tokio::io::{AsyncRead, AsyncWriteExt, BufReader, ReadBuf};

    use crate::admission::{HostAdmissionOutcome, HostAdmissionStatus};

    /// Generates `total` bytes in small chunks without pre-materializing the
    /// full hostile value for the product reader to copy from.
    struct ChunkedHostileReader {
        remaining: usize,
        chunk: Vec<u8>,
    }

    impl ChunkedHostileReader {
        fn new(total: usize, chunk_byte: u8, chunk_len: usize) -> Self {
            Self {
                remaining: total,
                chunk: vec![chunk_byte; chunk_len.max(1)],
            }
        }
    }

    impl Read for ChunkedHostileReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Ok(0);
            }
            let n = buf.len().min(self.chunk.len()).min(self.remaining);
            buf[..n].copy_from_slice(&self.chunk[..n]);
            self.remaining -= n;
            Ok(n)
        }
    }

    /// Async streaming hostile source, then a fixed trailing suffix.
    struct AsyncHostileThenSuffix {
        remaining: usize,
        chunk: Vec<u8>,
        suffix: &'static [u8],
        suffix_offset: usize,
    }

    impl AsyncHostileThenSuffix {
        fn new(total: usize, chunk_byte: u8, chunk_len: usize, suffix: &'static [u8]) -> Self {
            Self {
                remaining: total,
                chunk: vec![chunk_byte; chunk_len.max(1)],
                suffix,
                suffix_offset: 0,
            }
        }
    }

    impl AsyncRead for AsyncHostileThenSuffix {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            if self.remaining > 0 {
                let n = buf.remaining().min(self.chunk.len()).min(self.remaining);
                buf.put_slice(&self.chunk[..n]);
                self.remaining -= n;
                return Poll::Ready(Ok(()));
            }
            if self.suffix_offset < self.suffix.len() {
                let rest = &self.suffix[self.suffix_offset..];
                let n = buf.remaining().min(rest.len());
                buf.put_slice(&rest[..n]);
                self.suffix_offset += n;
            }
            Poll::Ready(Ok(()))
        }
    }

    #[test]
    fn sync_read_accepts_exact_cap_and_rejects_one_over_without_retaining_payload() {
        let max = 64;
        let ok = read_bounded_to_string(&mut Cursor::new(vec![b'a'; max]), max).unwrap();
        assert_eq!(ok, WireReadOutcome::Ready("a".repeat(max)));

        let mut hostile = ChunkedHostileReader::new(max + 1_048_576, b'x', 4096);
        let outcome = read_bounded_to_end(&mut hostile, max).unwrap();
        assert_eq!(outcome, WireReadOutcome::Oversized);
        assert!(hostile.remaining < max + 1_048_576);
    }

    #[test]
    fn wire_oversized_maps_to_typed_non_durable_outcome_without_payload() {
        let outcome = HostAdmissionOutcome::wire_record_too_large();
        assert_eq!(outcome.status, HostAdmissionStatus::Degraded);
        assert!(!outcome.retryable);
        assert_eq!(outcome.reason_code, Some(WIRE_RECORD_TOO_LARGE));
        let encoded = serde_json::to_string(&outcome).unwrap();
        assert!(!encoded.contains('x'));
        assert!(encoded.contains(WIRE_RECORD_TOO_LARGE));
    }

    #[tokio::test]
    async fn async_line_reader_streams_hostile_line_and_returns_oversized() {
        let max = 32;
        let stream =
            AsyncHostileThenSuffix::new(max + 256 * 1024, b'y', 1024, b"\n{\"ok\":true}\n");
        let mut reader = BufReader::new(stream);

        let first = read_bounded_line(&mut reader, max).await.unwrap();
        assert_eq!(first, WireReadOutcome::Oversized);

        let second = read_bounded_line(&mut reader, max).await.unwrap();
        assert_eq!(
            second,
            WireReadOutcome::Ready(Some(r#"{"ok":true}"#.to_string()))
        );
    }

    #[tokio::test]
    async fn async_line_reader_preserves_valid_line_under_cap() {
        let mut reader = BufReader::new(Cursor::new(b"{\"hello\":1}\n".to_vec()));
        let line = read_bounded_line(&mut reader, MAX_WIRE_MESSAGE_BYTES)
            .await
            .unwrap();
        assert_eq!(
            line,
            WireReadOutcome::Ready(Some(r#"{"hello":1}"#.to_string()))
        );
        let eof = read_bounded_line(&mut reader, MAX_WIRE_MESSAGE_BYTES)
            .await
            .unwrap();
        assert_eq!(eof, WireReadOutcome::Ready(None));
    }

    #[test]
    fn host_event_wire_cap_stays_one_mib_and_mcp_frame_is_larger() {
        assert_eq!(MAX_WIRE_MESSAGE_BYTES, DEFAULT_MAX_RECORD_BYTES);
        assert_eq!(MAX_WIRE_MESSAGE_BYTES, 1024 * 1024);
        assert_eq!(MAX_MCP_JSONRPC_FRAME_BYTES, 16 * 1024 * 1024);
        assert_eq!(MCP_OVERSIZE_ID_INSPECT_BYTES, 4096);
    }

    #[test]
    fn oversized_io_error_preserves_bounded_inspect_prefix() {
        let prefix = b"{\"jsonrpc\":\"2.0\",\"id\":7,".to_vec();
        let err = wire_oversized_io_error_with_prefix(prefix.clone());
        assert!(is_wire_oversized_io_error(&err));
        assert_eq!(wire_oversized_inspect_prefix(&err), prefix.as_slice());
        assert_eq!(err.to_string(), WIRE_RECORD_TOO_LARGE);
    }

    #[tokio::test]
    async fn mcp_line_reader_accepts_exact_cap_rejects_one_over_and_recovers_next() {
        let mut exact = vec![b'a'; MAX_MCP_JSONRPC_FRAME_BYTES];
        exact.push(b'\n');
        let mut exact_reader = BufReader::new(Cursor::new(exact));
        let line = read_bounded_mcp_line(&mut exact_reader)
            .await
            .expect("exact cap accepted")
            .expect("line");
        assert_eq!(line.len(), MAX_MCP_JSONRPC_FRAME_BYTES);

        let stream = AsyncHostileThenSuffix::new(
            MAX_MCP_JSONRPC_FRAME_BYTES + 1,
            b'z',
            8192,
            b"\n{\"jsonrpc\":\"2.0\",\"method\":\"ping\"}\n",
        );
        let mut oversized_reader = BufReader::new(stream);
        let error = read_bounded_mcp_line(&mut oversized_reader)
            .await
            .expect_err("one over rejected");
        assert!(is_wire_oversized_io_error(&error));
        assert_eq!(
            wire_oversized_inspect_prefix(&error).len(),
            MCP_OVERSIZE_ID_INSPECT_BYTES
        );
        assert!(
            wire_oversized_inspect_prefix(&error)
                .iter()
                .all(|byte| *byte == b'z')
        );

        assert_eq!(
            read_bounded_mcp_line(&mut oversized_reader)
                .await
                .expect("next frame")
                .as_deref(),
            Some(r#"{"jsonrpc":"2.0","method":"ping"}"#)
        );
    }

    #[tokio::test]
    async fn bounded_line_reader_resumes_a_frame_whose_read_future_was_cancelled() {
        // The daemon and MCP read loops race `read_line` against shutdown,
        // cancellation, and owner-open futures inside `tokio::select!`. When the
        // read loses that race its future is dropped mid-frame; `timeout` drops
        // it the same way here. Bytes already consumed from the buffered reader
        // must survive, or the next read starts mid-frame and every later frame
        // on this transport is misparsed.
        let (mut client, server) = tokio::io::duplex(64);
        let mut reader = BoundedLineReader::new(BufReader::new(server));

        client
            .write_all(b"part1")
            .await
            .expect("first socket write arrives without a newline");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), reader.read_mcp_line())
                .await
                .is_err(),
            "the frame is incomplete, so the read must still be pending when cancelled"
        );
        assert!(
            reader.has_partial_frame(),
            "the cancelled read must have consumed and retained the first chunk"
        );

        client
            .write_all(b"part2\n")
            .await
            .expect("second socket write completes the frame");
        assert_eq!(
            reader
                .read_mcp_line()
                .await
                .expect("resumed frame")
                .as_deref(),
            Some("part1part2"),
            "a cancelled read must not truncate the frame"
        );
        assert!(!reader.has_partial_frame());
    }

    #[tokio::test]
    async fn bounded_line_reader_keeps_the_oversized_verdict_across_cancellation() {
        // The overflow latch and the bounded inspect prefix are frame state too:
        // losing them on cancellation would resume a discarded hostile frame as
        // if it were a fresh one and admit its tail as a request.
        let max = 32;
        let (mut client, server) = tokio::io::duplex(1024);
        let mut reader = BoundedLineReader::new(BufReader::new(server));

        client
            .write_all(&vec![b'y'; max + 512])
            .await
            .expect("hostile prefix without a newline");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), reader.read_line(max))
                .await
                .is_err(),
            "the hostile frame is unterminated, so the read must be pending when cancelled"
        );
        assert!(
            reader.has_partial_frame(),
            "the oversized latch must survive the cancelled read"
        );

        client
            .write_all(b"\n{\"ok\":true}\n")
            .await
            .expect("frame terminator plus a legitimate follow-on frame");
        assert_eq!(
            reader.read_line(max).await.expect("oversized verdict"),
            WireReadOutcome::Oversized,
            "a cancelled read must not launder an oversized frame into an accepted one"
        );
        assert_eq!(
            reader.read_line(max).await.expect("next frame"),
            WireReadOutcome::Ready(Some(r#"{"ok":true}"#.to_string()))
        );
    }
}
