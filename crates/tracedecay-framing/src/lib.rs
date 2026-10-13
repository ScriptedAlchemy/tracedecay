//! Bounded wire-frame limits, readers, and oversized-frame errors.
//!
//! This crate is the single framing authority below `tracedecay-daemon-protocol`.
//! It owns host-event and MCP/daemon JSON-RPC frame ceilings, the durable
//! record-cap derivation, cancellation-safe bounded readers, and the
//! oversized-frame error family. It does not own protocol envelopes,
//! handshake identity, or transport endpoints.
//!
//! Host-event stdin and durable spool admission stay at
//! [`MAX_WIRE_MESSAGE_BYTES`] (1 MiB). MCP/daemon JSON-RPC frames use the
//! separate [`MAX_MCP_JSONRPC_FRAME_BYTES`] cap so legitimate edit/tool
//! requests are not rejected by host-event record coupling.
//!
//! Oversized outcomes never retain the full payload. Line readers may keep a
//! bounded leading inspect prefix for safe JSON-RPC `id` recovery.
//!
//! Partial-frame state is owned by [`BoundedLineReader`], not by the read
//! future. Every daemon and MCP read loop races its next frame read against
//! shutdown, cancellation, request completion, or owner-open futures inside a
//! `select!`, so the read future is routinely dropped after it has
//! already consumed bytes from the underlying buffered reader. A reader that
//! accumulated into the future's own stack would silently drop those bytes and
//! resume mid-frame, truncating the request and desynchronizing JSON-RPC
//! framing for every later frame on the same transport. Keeping the
//! accumulator in the reader makes a dropped read future lossless: the next
//! call resumes from exactly the bytes already consumed.

#![deny(clippy::all)]
#![warn(clippy::pedantic)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::items_after_statements)]
#![forbid(unsafe_code)]

use std::error::Error;
use std::fmt;
use std::io::{self, Read};
use std::pin::Pin;
use std::task::{Poll, ready};

/// Wire frame/record ceiling (1 MiB).
///
/// This is the authority for host-event stdin, hook payloads, and other
/// newline-delimited wire records. Durable host-admission spool budgets
/// derive their per-record cap from this constant.
pub const MAX_WIRE_MESSAGE_BYTES: usize = 1024 * 1024;

/// Durable host-admission / transcript-discovery per-record cap.
///
/// Derived from [`MAX_WIRE_MESSAGE_BYTES`] so a discovery sweep cannot queue
/// more than one admission pass will accept.
pub const MAX_WIRE_RECORD_BYTES: usize = MAX_WIRE_MESSAGE_BYTES;

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

/// Read until EOF or the first byte past `max_bytes`, retaining at most
/// `max_bytes`.
///
/// The outcome is known the moment the cap is crossed, so the reader stops
/// there instead of draining to EOF: a writer that crosses the cap and keeps
/// its end open (a hook host holding stdin) must not be able to hold the
/// caller, and total work is bounded by the cap, not by the input size.
#[tracing::instrument(name = "sessions.admission.read_end", level = "trace", skip_all)]
pub fn read_bounded_to_end(
    reader: &mut impl Read,
    max_bytes: usize,
) -> io::Result<WireReadOutcome<Vec<u8>>> {
    let mut retained = Vec::new();
    let mut scratch = [0_u8; 8192];
    loop {
        let read = reader.read(&mut scratch)?;
        if read == 0 {
            return Ok(WireReadOutcome::Ready(retained));
        }
        if retained.len().saturating_add(read) > max_bytes {
            return Ok(WireReadOutcome::Oversized);
        }
        retained.extend_from_slice(&scratch[..read]);
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

/// Partial-frame state of one bounded line read.
///
/// Filling accumulates content up to the cap. Crossing the cap switches to
/// Discarding, which keeps only the bounded inspect prefix until the frame ends.
enum LineFrame {
    Filling(Vec<u8>),
    Discarding { inspect_prefix: Vec<u8> },
}

impl Default for LineFrame {
    fn default() -> Self {
        Self::Filling(Vec::new())
    }
}

type FrameOutcome = io::Result<BoundedLineOutcome<Option<String>>>;

impl LineFrame {
    /// Folds one buffered chunk into the frame; an empty chunk means EOF.
    ///
    /// Returns how many leading bytes of `chunk` the frame took, which the
    /// caller must consume from its source, and the outcome once the frame
    /// ends. A frame ends at the first newline or at EOF, and ending resets
    /// the state for the next frame.
    fn step(&mut self, chunk: &[u8], max_bytes: usize) -> (usize, Option<FrameOutcome>) {
        if chunk.is_empty() {
            let outcome = match std::mem::take(self) {
                Self::Filling(retained) if retained.is_empty() => {
                    Ok(BoundedLineOutcome::Ready(None))
                }
                frame => frame.finish(),
            };
            return (0, Some(outcome));
        }

        let newline = chunk.iter().position(|byte| *byte == b'\n');
        let chunk_len = newline.map_or(chunk.len(), |index| index + 1);
        let content = &chunk[..newline.unwrap_or(chunk_len)];

        if let Self::Filling(retained) = self {
            if content.len() > max_bytes.saturating_sub(retained.len()) {
                let inspect_prefix = capture_inspect_prefix(retained, content);
                *self = Self::Discarding { inspect_prefix };
            } else {
                retained.extend_from_slice(content);
            }
        }

        let outcome = newline.map(|_| std::mem::take(self).finish());
        (chunk_len, outcome)
    }

    fn finish(self) -> FrameOutcome {
        match self {
            Self::Filling(retained) => {
                Ok(BoundedLineOutcome::Ready(Some(take_line_string(retained)?)))
            }
            Self::Discarding { inspect_prefix } => {
                Ok(BoundedLineOutcome::Oversized { inspect_prefix })
            }
        }
    }

    const fn is_empty(&self) -> bool {
        matches!(self, Self::Filling(retained) if retained.is_empty())
    }
}

/// Cancellation-safe bounded line reader.
///
/// The partial frame lives in this struct rather than on the read future's
/// stack. Dropping an in-flight read, the normal outcome of losing a `select!`
/// race against shutdown, cancellation, or a completed handler, therefore
/// loses nothing: bytes already consumed from `inner` stay accumulated here and
/// the next read resumes the same frame. Callers that read under `select!`
/// MUST use this type (or a transport that owns one).
///
/// The framing itself is I/O-free (`LineFrame::step`). Each front end only
/// moves buffered chunks into it, so the native ([`Self::read_mcp_line`]),
/// blocking ([`Self::read_mcp_line_blocking`]), and Tokio
/// ([`Self::read_mcp_line_tokio`]) readers frame identically.
pub struct BoundedLineReader<R> {
    inner: R,
    frame: LineFrame,
}

impl<R> BoundedLineReader<R> {
    /// Wrap a buffered source. No frame state is carried across sources.
    pub const fn new(inner: R) -> Self {
        Self {
            inner,
            frame: LineFrame::Filling(Vec::new()),
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
        !self.frame.is_empty()
    }
}

impl<R> BoundedLineReader<R>
where
    R: asupersync::io::AsyncBufRead + Unpin,
{
    /// Read one MCP/daemon JSON-RPC line with the dedicated frame ceiling.
    ///
    /// Oversized input is discarded through newline/EOF and returned as the
    /// typed IO error carrying only a bounded leading prefix for request-id
    /// inspection.
    pub async fn read_mcp_line(&mut self) -> io::Result<Option<String>> {
        mcp_line(self.read_bounded(MAX_MCP_JSONRPC_FRAME_BYTES).await)
    }

    #[tracing::instrument(name = "sessions.admission.read_line", level = "trace", skip_all)]
    async fn read_bounded(&mut self, max_bytes: usize) -> FrameOutcome {
        // Each chunk is folded into `self.frame` and consumed in one
        // synchronous step, so a dropped future never strands consumed bytes.
        std::future::poll_fn(|cx| {
            loop {
                let chunk = ready!(Pin::new(&mut self.inner).poll_fill_buf(cx))?;
                let (used, outcome) = self.frame.step(chunk, max_bytes);
                Pin::new(&mut self.inner).consume(used);
                if let Some(outcome) = outcome {
                    return Poll::Ready(outcome);
                }
            }
        })
        .await
    }
}

impl<R> BoundedLineReader<R>
where
    R: io::BufRead,
{
    /// Blocking [`Self::read_mcp_line`] for readers owned by a dedicated
    /// thread, such as a stdin pump.
    #[tracing::instrument(name = "sessions.admission.read_line", level = "trace", skip_all)]
    pub fn read_mcp_line_blocking(&mut self) -> io::Result<Option<String>> {
        loop {
            let chunk = match self.inner.fill_buf() {
                Ok(chunk) => chunk,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            let (used, outcome) = self.frame.step(chunk, MAX_MCP_JSONRPC_FRAME_BYTES);
            self.inner.consume(used);
            if let Some(outcome) = outcome {
                return mcp_line(outcome);
            }
        }
    }
}

// Transitional: the daemon and MCP servers still read on Tokio. The server
// unit of #3736 moves them to `read_mcp_line` and deletes this front end.
impl<R> BoundedLineReader<R>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    /// Tokio [`Self::read_mcp_line`].
    pub async fn read_mcp_line_tokio(&mut self) -> io::Result<Option<String>> {
        mcp_line(self.read_bounded_tokio(MAX_MCP_JSONRPC_FRAME_BYTES).await)
    }

    #[tracing::instrument(name = "sessions.admission.read_line", level = "trace", skip_all)]
    async fn read_bounded_tokio(&mut self, max_bytes: usize) -> FrameOutcome {
        std::future::poll_fn(|cx| {
            loop {
                let chunk = ready!(Pin::new(&mut self.inner).poll_fill_buf(cx))?;
                let (used, outcome) = self.frame.step(chunk, max_bytes);
                Pin::new(&mut self.inner).consume(used);
                if let Some(outcome) = outcome {
                    return Poll::Ready(outcome);
                }
            }
        })
        .await
    }
}

fn mcp_line(outcome: FrameOutcome) -> io::Result<Option<String>> {
    match outcome? {
        BoundedLineOutcome::Ready(line) => Ok(line),
        BoundedLineOutcome::Oversized { inspect_prefix } => {
            Err(wire_oversized_io_error_with_prefix(inspect_prefix))
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
    R: tokio::io::AsyncBufRead + Unpin,
{
    match BoundedLineReader::new(reader)
        .read_bounded_tokio(max_bytes)
        .await?
    {
        BoundedLineOutcome::Ready(line) => Ok(WireReadOutcome::Ready(line)),
        BoundedLineOutcome::Oversized { .. } => Ok(WireReadOutcome::Oversized),
    }
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
    use std::collections::VecDeque;
    use std::io::Cursor;
    use std::task::{Context, Waker};

    use tokio::io::{AsyncRead, BufReader, ReadBuf};

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

    /// Buffered source that replays scripted chunks; `None` is one `Pending`
    /// poll, standing in for a socket whose next bytes have not arrived yet.
    /// EOF follows the last step. Implements both async buffered traits so the
    /// native and Tokio front ends run the same script.
    struct Script {
        steps: VecDeque<Option<&'static [u8]>>,
        current: &'static [u8],
    }

    impl Script {
        fn new(steps: impl IntoIterator<Item = Option<&'static [u8]>>) -> Self {
            Self {
                steps: steps.into_iter().collect(),
                current: &[],
            }
        }

        fn fill(&mut self) -> Poll<&'static [u8]> {
            while self.current.is_empty() {
                match self.steps.pop_front() {
                    Some(Some(bytes)) => self.current = bytes,
                    Some(None) => return Poll::Pending,
                    None => break,
                }
            }
            Poll::Ready(self.current)
        }

        fn take(&mut self, amount: usize) {
            self.current = &self.current[amount..];
        }

        fn read_chunk(&mut self, limit: usize) -> Poll<&'static [u8]> {
            let available = ready!(self.fill());
            let chunk = &available[..available.len().min(limit)];
            self.take(chunk.len());
            Poll::Ready(chunk)
        }
    }

    impl asupersync::io::AsyncRead for Script {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut asupersync::io::ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            buf.put_slice(ready!(self.get_mut().read_chunk(buf.remaining())));
            Poll::Ready(Ok(()))
        }
    }

    impl asupersync::io::AsyncBufRead for Script {
        fn poll_fill_buf(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<&[u8]>> {
            self.get_mut().fill().map(Ok)
        }

        fn consume(self: Pin<&mut Self>, amount: usize) {
            self.get_mut().take(amount);
        }
    }

    impl AsyncRead for Script {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            buf.put_slice(ready!(self.get_mut().read_chunk(buf.remaining())));
            Poll::Ready(Ok(()))
        }
    }

    impl tokio::io::AsyncBufRead for Script {
        fn poll_fill_buf(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<&[u8]>> {
            self.get_mut().fill().map(Ok)
        }

        fn consume(self: Pin<&mut Self>, amount: usize) {
            self.get_mut().take(amount);
        }
    }

    fn poll_once<F: Future>(future: F) -> Poll<F::Output> {
        std::pin::pin!(future).poll(&mut Context::from_waker(Waker::noop()))
    }

    fn ready_now<F: Future>(future: F) -> F::Output {
        match poll_once(future) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("the scripted source has no pending step left"),
        }
    }

    fn step_all(frame: &mut LineFrame, chunk: &[u8], max: usize) -> Vec<FrameOutcome> {
        let mut outcomes = Vec::new();
        let mut rest = chunk;
        while !rest.is_empty() {
            let (used, outcome) = frame.step(rest, max);
            rest = &rest[used..];
            outcomes.extend(outcome);
        }
        outcomes
    }

    fn ready_line(outcome: FrameOutcome) -> Option<String> {
        match outcome.unwrap() {
            BoundedLineOutcome::Ready(line) => line,
            BoundedLineOutcome::Oversized { .. } => panic!("expected a ready frame"),
        }
    }

    #[test]
    fn step_consumes_through_the_newline_and_leaves_the_next_frame() {
        let mut frame = LineFrame::default();
        let (used, outcome) = frame.step(b"ab\r\ncd", 8);
        assert_eq!(used, 4, "the newline ends the frame; `cd` stays unconsumed");
        assert_eq!(ready_line(outcome.unwrap()).as_deref(), Some("ab"));
        assert!(frame.is_empty());

        let (used, outcome) = frame.step(b"cd", 8);
        assert_eq!((used, outcome.is_none()), (2, true));
        let (used, outcome) = frame.step(b"", 8);
        assert_eq!(used, 0);
        assert_eq!(
            ready_line(outcome.unwrap()).as_deref(),
            Some("cd"),
            "EOF ends an unterminated frame"
        );
        assert_eq!(ready_line(frame.step(b"", 8).1.unwrap()), None);
    }

    #[test]
    fn step_accepts_exactly_the_cap_across_chunks() {
        let mut frame = LineFrame::default();
        assert!(step_all(&mut frame, b"abcd", 8).is_empty());
        let outcomes = step_all(&mut frame, b"efgh\n", 8);
        assert_eq!(outcomes.len(), 1);
        assert_eq!(
            ready_line(outcomes.into_iter().next().unwrap()).as_deref(),
            Some("abcdefgh")
        );
    }

    #[test]
    fn step_discards_an_oversized_frame_keeping_only_the_inspect_prefix() {
        let mut frame = LineFrame::default();
        assert!(step_all(&mut frame, b"{\"id\":7,", 8).is_empty());
        assert!(step_all(&mut frame, b"\"x\":\"yyyy", 8).is_empty());
        assert!(
            matches!(&frame, LineFrame::Discarding { inspect_prefix } if inspect_prefix == b"{\"id\":7,\"x\":\"yyyy"),
            "the verdict is latched and only the leading prefix is kept"
        );
        let outcomes = step_all(&mut frame, b"yyyy\"}\nnext\n", 8);
        assert_eq!(outcomes.len(), 2);
        let mut outcomes = outcomes.into_iter();
        assert!(matches!(
            outcomes.next().unwrap().unwrap(),
            BoundedLineOutcome::Oversized { inspect_prefix } if inspect_prefix == b"{\"id\":7,\"x\":\"yyyy"
        ));
        assert_eq!(
            ready_line(outcomes.next().unwrap()).as_deref(),
            Some("next")
        );
    }

    #[test]
    fn step_reports_invalid_utf8_and_resets_for_the_next_frame() {
        let mut frame = LineFrame::default();
        let (used, outcome) = frame.step(b"\xff\nok\n", 8);
        assert_eq!(used, 2);
        assert_eq!(
            outcome.unwrap().err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            ready_line(frame.step(b"ok\n", 8).1.unwrap()).as_deref(),
            Some("ok")
        );
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

    /// A writer that crosses the cap and never closes its end. Panics instead
    /// of looping forever so a reader that drains to EOF fails the test rather
    /// than hanging it.
    struct NeverClosingWriter {
        reads: usize,
        budget: usize,
    }

    impl Read for NeverClosingWriter {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.reads += 1;
            assert!(
                self.reads <= self.budget,
                "bounded reader kept draining a writer that never reaches EOF"
            );
            buf.fill(b'z');
            Ok(buf.len())
        }
    }

    #[test]
    fn sync_read_stops_at_the_cap_when_the_writer_never_closes() {
        let max = 8192 * 4;
        // One read per scratch buffer up to the cap, then exactly one more
        // that crosses it and must end the call.
        let budget = max / 8192 + 1;
        let mut writer = NeverClosingWriter { reads: 0, budget };
        let outcome = read_bounded_to_end(&mut writer, max).unwrap();
        assert_eq!(outcome, WireReadOutcome::Oversized);
        assert_eq!(writer.reads, budget);
    }

    #[test]
    fn tokio_line_reader_preserves_valid_line_under_cap() {
        let mut reader = BufReader::new(Cursor::new(b"{\"hello\":1}\n".to_vec()));
        let line = ready_now(read_bounded_line(&mut reader, MAX_WIRE_MESSAGE_BYTES)).unwrap();
        assert_eq!(
            line,
            WireReadOutcome::Ready(Some(r#"{"hello":1}"#.to_string()))
        );
        let eof = ready_now(read_bounded_line(&mut reader, MAX_WIRE_MESSAGE_BYTES)).unwrap();
        assert_eq!(eof, WireReadOutcome::Ready(None));
    }

    #[test]
    fn tokio_mcp_line_reader_accepts_exact_cap_rejects_one_over_and_recovers_next() {
        let mut exact = vec![b'a'; MAX_MCP_JSONRPC_FRAME_BYTES];
        exact.push(b'\n');
        let mut exact_reader = BoundedLineReader::new(BufReader::new(Cursor::new(exact)));
        let line = ready_now(exact_reader.read_mcp_line_tokio())
            .expect("exact cap accepted")
            .expect("line");
        assert_eq!(line.len(), MAX_MCP_JSONRPC_FRAME_BYTES);

        let stream = AsyncHostileThenSuffix::new(
            MAX_MCP_JSONRPC_FRAME_BYTES + 1,
            b'z',
            8192,
            b"\n{\"jsonrpc\":\"2.0\",\"method\":\"ping\"}\n",
        );
        let mut oversized_reader = BoundedLineReader::new(BufReader::new(stream));
        let error =
            ready_now(oversized_reader.read_mcp_line_tokio()).expect_err("one over rejected");
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
            ready_now(oversized_reader.read_mcp_line_tokio())
                .expect("next frame")
                .as_deref(),
            Some(r#"{"jsonrpc":"2.0","method":"ping"}"#)
        );
    }

    #[test]
    fn blocking_mcp_line_reader_frames_lines_and_recovers_after_an_oversized_frame() {
        let mut hostile = vec![b'z'; MAX_MCP_JSONRPC_FRAME_BYTES + 1];
        hostile.extend_from_slice(b"\nfirst\r\nlast");
        let mut reader = BoundedLineReader::new(io::BufReader::new(Cursor::new(hostile)));

        let error = reader
            .read_mcp_line_blocking()
            .expect_err("one over rejected");
        assert!(is_wire_oversized_io_error(&error));
        assert_eq!(
            wire_oversized_inspect_prefix(&error),
            vec![b'z'; MCP_OVERSIZE_ID_INSPECT_BYTES].as_slice()
        );
        assert_eq!(
            reader.read_mcp_line_blocking().unwrap().as_deref(),
            Some("first")
        );
        assert_eq!(
            reader.read_mcp_line_blocking().unwrap().as_deref(),
            Some("last"),
            "EOF ends the unterminated final frame"
        );
        assert_eq!(reader.read_mcp_line_blocking().unwrap(), None);
    }

    // The daemon and MCP read loops race their frame read against shutdown,
    // cancellation, and owner-open futures inside `select!`. When the read
    // loses that race its future is dropped mid-frame. Bytes already consumed
    // from the source must survive, or the next read starts mid-frame and
    // every later frame on the transport is misparsed.
    fn half_frame_then_rest() -> Script {
        Script::new([Some(&b"part1"[..]), None, Some(&b"part2\n"[..])])
    }

    // The overflow latch and the inspect prefix are frame state too: losing
    // them on cancellation would resume a discarded hostile frame as if it
    // were a fresh one and admit its tail as a request.
    fn oversized_half_frame_then_rest() -> Script {
        Script::new([
            Some(&[b'y'; 20][..]),
            None,
            Some(&b"yyyy\n{\"ok\":true}\n"[..]),
        ])
    }

    #[test]
    fn native_reader_resumes_a_frame_whose_read_future_was_dropped() {
        let mut reader = BoundedLineReader::new(half_frame_then_rest());
        assert!(poll_once(reader.read_mcp_line()).is_pending());
        assert!(
            reader.has_partial_frame(),
            "the dropped read must have consumed and retained the first chunk"
        );
        assert_eq!(
            ready_now(reader.read_mcp_line()).unwrap().as_deref(),
            Some("part1part2"),
            "a dropped read must not truncate the frame"
        );
        assert!(!reader.has_partial_frame());
    }

    #[test]
    fn native_reader_keeps_the_oversized_verdict_across_a_dropped_read() {
        let mut reader = BoundedLineReader::new(oversized_half_frame_then_rest());
        assert!(poll_once(reader.read_bounded(16)).is_pending());
        assert!(reader.has_partial_frame());
        assert!(matches!(
            ready_now(reader.read_bounded(16)).unwrap(),
            BoundedLineOutcome::Oversized { inspect_prefix } if inspect_prefix == [b'y'; 20]
        ));
        assert_eq!(
            ready_line(ready_now(reader.read_bounded(16))).as_deref(),
            Some(r#"{"ok":true}"#)
        );
    }

    #[test]
    fn tokio_reader_resumes_a_frame_whose_read_future_was_dropped() {
        let mut reader = BoundedLineReader::new(half_frame_then_rest());
        assert!(poll_once(reader.read_mcp_line_tokio()).is_pending());
        assert!(reader.has_partial_frame());
        assert_eq!(
            ready_now(reader.read_mcp_line_tokio()).unwrap().as_deref(),
            Some("part1part2"),
            "a dropped read must not truncate the frame"
        );
        assert!(!reader.has_partial_frame());
    }

    #[test]
    fn tokio_reader_keeps_the_oversized_verdict_across_a_dropped_read() {
        let mut reader = BoundedLineReader::new(oversized_half_frame_then_rest());
        assert!(poll_once(reader.read_bounded_tokio(16)).is_pending());
        assert!(reader.has_partial_frame());
        assert!(matches!(
            ready_now(reader.read_bounded_tokio(16)).unwrap(),
            BoundedLineOutcome::Oversized { inspect_prefix } if inspect_prefix == [b'y'; 20]
        ));
        assert_eq!(
            ready_line(ready_now(reader.read_bounded_tokio(16))).as_deref(),
            Some(r#"{"ok":true}"#)
        );
    }
}
