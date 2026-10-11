//! Strict LSP `Content-Length` framing shared by the stdio bridge and the
//! analyzer client.
//!
//! Framing preserves opaque LSP JSON-RPC payload bytes. It opens no
//! project/profile database, selects no root, starts no analyzer, merges no
//! diagnostics, and makes no capability or routing decisions.

use std::io;

use futures_util::StreamExt;
use tokio::io::AsyncRead;
use tokio::time::Instant;
use tokio_util::bytes::{Buf, BytesMut};
use tokio_util::codec::{Decoder, Encoder, FramedRead};
use tracedecay_daemon_protocol::{FramePoll, LspFrame, MAX_LSP_FRAME_BYTES};

/// A framing header is metadata, not an unbounded side channel.
pub const MAX_LSP_HEADER_BYTES: usize = 16 * 1024;

/// Strict LSP `Content-Length` envelope failures.
///
/// Framing failures are reported before any payload reaches the daemon
/// session. JSON-RPC parsing is a daemon-session concern, so a
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

#[tracing::instrument(name = "lsp.rpc.frame_decode", level = "trace", skip_all)]
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

#[tracing::instrument(name = "lsp.rpc.frame_encode", level = "trace", skip_all)]
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
