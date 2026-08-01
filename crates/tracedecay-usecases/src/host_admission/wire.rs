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

use std::error::Error;
use std::fmt;
use std::io::{self, Read};

use tokio::io::{AsyncBufRead, AsyncBufReadExt};

use super::spool::DEFAULT_MAX_RECORD_BYTES;

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
pub(crate) fn read_bounded_to_end(
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

/// Read one newline-delimited frame, retaining at most `max_bytes` of content
/// (excluding the terminating newline). On overflow, discards until newline or
/// EOF and returns [`WireReadOutcome::Oversized`].
#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
pub async fn read_bounded_line<R>(
    reader: &mut R,
    max_bytes: usize,
) -> io::Result<WireReadOutcome<Option<String>>>
where
    R: AsyncBufRead + Unpin,
{
    match read_bounded_line_with_inspect(reader, max_bytes).await? {
        BoundedLineOutcome::Ready(line) => Ok(WireReadOutcome::Ready(line)),
        BoundedLineOutcome::Oversized { .. } => Ok(WireReadOutcome::Oversized),
    }
}

/// Read one MCP/daemon JSON-RPC line with the dedicated frame ceiling.
///
/// Oversized input is discarded through newline/EOF and returned as the typed
/// IO error carrying only a bounded leading prefix for request-id inspection.
pub async fn read_bounded_mcp_line<R>(reader: &mut R) -> io::Result<Option<String>>
where
    R: AsyncBufRead + Unpin,
{
    match read_bounded_line_with_inspect(reader, MAX_MCP_JSONRPC_FRAME_BYTES).await? {
        BoundedLineOutcome::Ready(line) => Ok(line),
        BoundedLineOutcome::Oversized { inspect_prefix } => {
            Err(wire_oversized_io_error_with_prefix(inspect_prefix))
        }
    }
}

async fn read_bounded_line_with_inspect<R>(
    reader: &mut R,
    max_bytes: usize,
) -> io::Result<BoundedLineOutcome<Option<String>>>
where
    R: AsyncBufRead + Unpin,
{
    let mut retained = Vec::new();
    let mut oversized = false;
    let mut inspect_prefix = Vec::new();

    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if oversized {
                return Ok(BoundedLineOutcome::Oversized { inspect_prefix });
            }
            if retained.is_empty() {
                return Ok(BoundedLineOutcome::Ready(None));
            }
            let line = take_line_string(std::mem::take(&mut retained))?;
            return Ok(BoundedLineOutcome::Ready(Some(line)));
        }

        let newline = available.iter().position(|byte| *byte == b'\n');
        let chunk_len = newline.map_or(available.len(), |index| index + 1);
        let content_len = newline.unwrap_or(chunk_len);

        if !oversized {
            let room = max_bytes.saturating_sub(retained.len());
            if content_len > room {
                inspect_prefix = capture_inspect_prefix(&retained, &available[..content_len]);
                oversized = true;
                retained.clear();
                retained.shrink_to_fit();
            } else {
                retained.extend_from_slice(&available[..content_len]);
            }
        }

        reader.consume(chunk_len);

        if newline.is_some() {
            if oversized {
                return Ok(BoundedLineOutcome::Oversized { inspect_prefix });
            }
            let line = take_line_string(std::mem::take(&mut retained))?;
            return Ok(BoundedLineOutcome::Ready(Some(line)));
        }
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
#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
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

    use tokio::io::{AsyncRead, BufReader, ReadBuf};

    use crate::host_admission::{HostAdmissionOutcome, HostAdmissionStatus};

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
}
