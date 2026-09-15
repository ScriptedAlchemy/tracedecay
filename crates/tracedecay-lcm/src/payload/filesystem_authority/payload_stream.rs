//! Two-pass verified payload streaming.
//!
//! A payload file is proven — stable identity, byte count, SHA-256, and UTF-8
//! scalar count — through one open handle before any byte leaves this module,
//! then re-read through that same handle in caller-sized windows. Peak
//! transient memory is one window, never the payload. The proven identity is
//! re-checked against the path before the first window and after the last,
//! and the emitted bytes are re-hashed against the proof, so a payload that is
//! replaced, removed, or rewritten between proof and emission is refused
//! instead of served.

use std::fmt;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use sha2::Digest;
use tracedecay_domain::canonical_text::encode_lowercase_hex;

use super::verified_read::ContentScanner;
use super::{
    LcmError, MAX_VERIFIED_PAYLOAD_FILE_BYTES, PayloadFileIdentity, open_verified_payload_file,
    same_payload_file_identity, verify_opened_payload_file,
};

/// Failure of a verified payload stream, split so the consumer's own typed
/// interruption or sink outcome survives the round trip through this module
/// instead of being flattened into an `LcmError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadStreamError<E> {
    /// The payload was missing, oversized, replaced, rewritten, or unreadable.
    Payload(LcmError),
    /// The consumer's checkpoint or sink refused.
    Consumer(E),
}

impl<E> From<LcmError> for PayloadStreamError<E> {
    fn from(error: LcmError) -> Self {
        Self::Payload(error)
    }
}

/// A payload file whose content proof was taken through the handle it holds.
///
/// Constructed only by [`VerifiedPayloadStream::open`], which runs the proof,
/// so holding a value is evidence that the handle's bytes matched the expected
/// hash, byte count, and character count at open time. [`Self::emit`] consumes
/// the stream: one proof authorizes one emission.
pub struct VerifiedPayloadStream {
    file: fs::File,
    path: PathBuf,
    identity: PayloadFileIdentity,
    content_hash: String,
    byte_count: u64,
}

impl fmt::Debug for VerifiedPayloadStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedPayloadStream")
            .field("identity", &self.identity)
            .field("content_hash", &self.content_hash)
            .field("byte_count", &self.byte_count)
            .field("path", &"<redacted>")
            .finish()
    }
}

impl VerifiedPayloadStream {
    /// Opens `path` and proves its content against the expected hash, byte
    /// count, and character count by reading it through `window` once. Returns
    /// `None` when the file does not exist. `window` must be non-empty unless
    /// `expected_bytes` is zero.
    pub(in crate::payload) fn open<E>(
        path: &Path,
        expected_hash: &str,
        expected_bytes: u64,
        expected_chars: u64,
        window: &mut [u8],
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Option<Self>, PayloadStreamError<E>> {
        checkpoint().map_err(PayloadStreamError::Consumer)?;
        if expected_bytes > MAX_VERIFIED_PAYLOAD_FILE_BYTES {
            return Err(LcmError::PayloadIntegrityMismatch.into());
        }
        ensure_window(window, expected_bytes)?;
        let Some((mut file, opened, _lstat, identity)) = open_verified_payload_file(path)? else {
            return Ok(None);
        };
        if opened.len() != expected_bytes {
            return Err(LcmError::PayloadIntegrityMismatch.into());
        }
        let mut scanner = ContentScanner::default();
        read_exact_windows(&mut file, expected_bytes, window, checkpoint, |chunk| {
            scanner.update(chunk).map_err(PayloadStreamError::Payload)
        })?;
        let (content_hash, char_count) = scanner.finish()?;
        if content_hash != expected_hash || char_count != expected_chars {
            return Err(LcmError::PayloadIntegrityMismatch.into());
        }
        let stream = Self {
            file,
            path: path.to_path_buf(),
            identity,
            content_hash,
            byte_count: expected_bytes,
        };
        stream.revalidate()?;
        checkpoint().map_err(PayloadStreamError::Consumer)?;
        Ok(Some(stream))
    }

    /// Lowercase hex SHA-256 proven over the handle at open time.
    pub fn content_hash(&self) -> &str {
        &self.content_hash
    }

    #[hotpath::skip]
    pub const fn byte_count(&self) -> u64 {
        self.byte_count
    }

    /// Re-reads the proven handle from the start, handing each filled window
    /// to `emit`, and returns the number of bytes emitted. `checkpoint` runs
    /// before every read. The path is required to still name the proven
    /// identity before the first window and after the last, and the bytes
    /// handed out are re-hashed against the proof, so a replaced or rewritten
    /// payload fails with [`LcmError::InvalidPayloadRef`] or
    /// [`LcmError::PayloadIntegrityMismatch`]; a rewrite of the same inode
    /// is only detectable after emission, and consumers must discard what they
    /// received on any error.
    pub fn emit<E>(
        mut self,
        window: &mut [u8],
        checkpoint: &mut impl FnMut() -> Result<(), E>,
        emit: &mut impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<u64, PayloadStreamError<E>> {
        checkpoint().map_err(PayloadStreamError::Consumer)?;
        ensure_window(window, self.byte_count)?;
        self.revalidate()?;
        let mut hasher = sha2::Sha256::new();
        let mut emitted = 0_u64;
        read_exact_windows(
            &mut self.file,
            self.byte_count,
            window,
            checkpoint,
            |chunk| {
                hasher.update(chunk);
                emit(chunk).map_err(PayloadStreamError::Consumer)?;
                emitted += chunk.len() as u64;
                Ok(())
            },
        )?;
        if encode_lowercase_hex(&hasher.finalize()) != self.content_hash {
            return Err(LcmError::PayloadIntegrityMismatch.into());
        }
        self.revalidate()?;
        checkpoint().map_err(PayloadStreamError::Consumer)?;
        Ok(emitted)
    }

    /// The path must still resolve to the proven identity at the proven size.
    fn revalidate(&self) -> Result<(), LcmError> {
        let (current, _lstat, current_identity) =
            verify_opened_payload_file(&self.file, &self.path)?;
        same_payload_file_identity(&current_identity, &self.identity)?;
        if current.len() != self.byte_count {
            return Err(LcmError::PayloadIntegrityMismatch);
        }
        Ok(())
    }
}

fn ensure_window(window: &[u8], byte_count: u64) -> Result<(), LcmError> {
    if window.is_empty() && byte_count > 0 {
        return Err(LcmError::Io(
            "payload stream window must hold at least one byte".to_string(),
        ));
    }
    Ok(())
}

/// Reads exactly `expected_bytes` from the start of `file` through `window`,
/// running `checkpoint` before each read and `consume` on each filled window.
/// A file that ends early or has grown past `expected_bytes` is refused.
fn read_exact_windows<E>(
    file: &mut fs::File,
    expected_bytes: u64,
    window: &mut [u8],
    checkpoint: &mut impl FnMut() -> Result<(), E>,
    mut consume: impl FnMut(&[u8]) -> Result<(), PayloadStreamError<E>>,
) -> Result<(), PayloadStreamError<E>> {
    file.seek(SeekFrom::Start(0)).map_err(io_error)?;
    let mut remaining = expected_bytes;
    while remaining > 0 {
        checkpoint().map_err(PayloadStreamError::Consumer)?;
        let want = usize::try_from(remaining)
            .map_or(window.len(), |remaining| remaining.min(window.len()));
        let count = file.read(&mut window[..want]).map_err(io_error)?;
        if count == 0 {
            return Err(LcmError::PayloadIntegrityMismatch.into());
        }
        consume(&window[..count])?;
        remaining -= count as u64;
    }
    let mut probe = [0_u8; 1];
    if file.read(&mut probe).map_err(io_error)? != 0 {
        return Err(LcmError::PayloadIntegrityMismatch.into());
    }
    Ok(())
}

fn io_error(error: std::io::Error) -> LcmError {
    LcmError::Io(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::sha256_hex;

    const WINDOW: usize = 4 * 1024;

    fn ok() -> Result<(), LcmError> {
        Ok(())
    }

    fn payload(len: usize) -> Vec<u8> {
        (0..len).map(|index| b'a' + (index % 26) as u8).collect()
    }

    fn open(
        path: &Path,
        content: &[u8],
        window: &mut [u8],
    ) -> Result<Option<VerifiedPayloadStream>, PayloadStreamError<LcmError>> {
        VerifiedPayloadStream::open(
            path,
            &sha256_hex(content),
            content.len() as u64,
            content.len() as u64,
            window,
            &mut ok,
        )
    }

    fn collect(
        stream: VerifiedPayloadStream,
        window: &mut [u8],
    ) -> Result<(Vec<u8>, Vec<usize>), PayloadStreamError<LcmError>> {
        let mut output = Vec::new();
        let mut chunk_sizes = Vec::new();
        stream.emit(window, &mut ok, &mut |chunk| {
            output.extend_from_slice(chunk);
            chunk_sizes.push(chunk.len());
            Ok(())
        })?;
        Ok((output, chunk_sizes))
    }

    #[test]
    fn proven_stream_emits_byte_identical_content_in_window_sized_pieces() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload.payload");
        let content = payload(3 * WINDOW + 17);
        fs::write(&path, &content).unwrap();
        let mut window = vec![0_u8; WINDOW];

        let stream = open(&path, &content, &mut window).unwrap().unwrap();
        assert_eq!(stream.content_hash(), sha256_hex(&content));
        assert_eq!(stream.byte_count(), content.len() as u64);
        assert!(!format!("{stream:?}").contains("payload.payload"));

        let (output, chunk_sizes) = collect(stream, &mut window).unwrap();
        assert_eq!(output, content);
        assert_eq!(chunk_sizes, vec![WINDOW, WINDOW, WINDOW, 17]);
    }

    #[test]
    fn empty_payload_needs_no_window_and_emits_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("empty.payload");
        fs::write(&path, b"").unwrap();
        let stream = open(&path, b"", &mut []).unwrap().unwrap();
        let (output, chunk_sizes) = collect(stream, &mut []).unwrap();
        assert!(output.is_empty());
        assert!(chunk_sizes.is_empty());
    }

    #[test]
    fn missing_file_is_none_and_wrong_expectations_are_refused_before_any_emit() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload.payload");
        let mut window = vec![0_u8; WINDOW];
        assert!(open(&path, b"absent", &mut window).unwrap().is_none());

        let content = payload(WINDOW + 5);
        fs::write(&path, &content).unwrap();
        let hash = sha256_hex(&content);
        let bytes = content.len() as u64;
        for (expected_hash, expected_bytes, expected_chars) in [
            (sha256_hex(b"other"), bytes, bytes),
            (hash.clone(), bytes + 1, bytes),
            (hash.clone(), bytes, bytes + 1),
        ] {
            assert_eq!(
                VerifiedPayloadStream::open(
                    &path,
                    &expected_hash,
                    expected_bytes,
                    expected_chars,
                    &mut window,
                    &mut ok,
                )
                .err(),
                Some(PayloadStreamError::Payload(
                    LcmError::PayloadIntegrityMismatch
                ))
            );
        }
        assert_eq!(
            VerifiedPayloadStream::open(&path, &hash, bytes, bytes, &mut [], &mut ok).err(),
            Some(PayloadStreamError::Payload(LcmError::Io(
                "payload stream window must hold at least one byte".to_string()
            )))
        );
        assert_eq!(fs::read(&path).unwrap(), content);
    }

    #[test]
    fn invalid_utf8_is_refused_by_the_proof() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload.payload");
        let mut content = payload(WINDOW - 1);
        content.extend_from_slice(&[0xe9, 0x9b]);
        fs::write(&path, &content).unwrap();
        let mut window = vec![0_u8; WINDOW];
        assert_eq!(
            VerifiedPayloadStream::open(
                &path,
                &sha256_hex(&content),
                content.len() as u64,
                WINDOW as u64,
                &mut window,
                &mut ok,
            )
            .err(),
            Some(PayloadStreamError::Payload(
                LcmError::PayloadIntegrityMismatch
            ))
        );
    }

    #[test]
    fn replaced_file_between_proof_and_emission_is_refused_before_the_first_window() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload.payload");
        let displaced = temp.path().join("displaced.payload");
        let content = payload(2 * WINDOW);
        fs::write(&path, &content).unwrap();
        let mut window = vec![0_u8; WINDOW];
        let stream = open(&path, &content, &mut window).unwrap().unwrap();

        fs::rename(&path, &displaced).unwrap();
        fs::write(
            &path,
            payload(2 * WINDOW)
                .iter()
                .rev()
                .copied()
                .collect::<Vec<_>>(),
        )
        .unwrap();

        let mut emitted = 0;
        let error = stream
            .emit(&mut window, &mut ok, &mut |chunk| {
                emitted += chunk.len();
                Ok(())
            })
            .unwrap_err();
        assert_eq!(
            error,
            PayloadStreamError::Payload(LcmError::InvalidPayloadRef)
        );
        assert_eq!(
            emitted, 0,
            "no window may cross before the identity recheck"
        );
        assert_eq!(fs::read(&displaced).unwrap(), content);
    }

    #[test]
    fn removed_file_between_proof_and_emission_is_refused_before_the_first_window() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload.payload");
        let content = payload(WINDOW + 3);
        fs::write(&path, &content).unwrap();
        let mut window = vec![0_u8; WINDOW];
        let stream = open(&path, &content, &mut window).unwrap().unwrap();

        fs::remove_file(&path).unwrap();

        let mut emitted = 0;
        let error = stream
            .emit(&mut window, &mut ok, &mut |chunk| {
                emitted += chunk.len();
                Ok(())
            })
            .unwrap_err();
        assert!(
            matches!(error, PayloadStreamError::Payload(LcmError::Io(_))),
            "{error:?}"
        );
        assert_eq!(emitted, 0);
    }

    #[test]
    fn same_inode_rewrite_between_proof_and_emission_fails_the_emission_hash() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload.payload");
        let content = payload(2 * WINDOW);
        fs::write(&path, &content).unwrap();
        let mut window = vec![0_u8; WINDOW];
        let stream = open(&path, &content, &mut window).unwrap().unwrap();

        let mut rewritten = content.clone();
        rewritten[WINDOW + 1] ^= 0x20;
        fs::write(&path, &rewritten).unwrap();

        let error = collect(stream, &mut window).unwrap_err();
        assert_eq!(
            error,
            PayloadStreamError::Payload(LcmError::PayloadIntegrityMismatch)
        );
    }

    #[test]
    fn truncated_or_grown_file_between_proof_and_emission_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload.payload");
        let content = payload(2 * WINDOW);
        fs::write(&path, &content).unwrap();
        let mut window = vec![0_u8; WINDOW];

        let stream = open(&path, &content, &mut window).unwrap().unwrap();
        fs::write(&path, &content[..WINDOW]).unwrap();
        let mut emitted = 0;
        let error = stream
            .emit(&mut window, &mut ok, &mut |chunk| {
                emitted += chunk.len();
                Ok(())
            })
            .unwrap_err();
        assert_eq!(
            error,
            PayloadStreamError::Payload(LcmError::PayloadIntegrityMismatch)
        );
        assert_eq!(
            emitted, 0,
            "a size change is refused before the first window"
        );

        fs::write(&path, &content).unwrap();
        let stream = open(&path, &content, &mut window).unwrap().unwrap();
        let mut grown = content.clone();
        grown.extend_from_slice(b"!");
        fs::write(&path, &grown).unwrap();
        assert_eq!(
            collect(stream, &mut window).unwrap_err(),
            PayloadStreamError::Payload(LcmError::PayloadIntegrityMismatch)
        );
    }

    #[test]
    fn checkpoint_interrupts_the_proof_between_windows_without_exposing_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload.payload");
        let content = payload(8 * WINDOW);
        fs::write(&path, &content).unwrap();
        let mut window = vec![0_u8; WINDOW];
        let mut checkpoints = 0_u32;
        let result = VerifiedPayloadStream::open(
            &path,
            &sha256_hex(&content),
            content.len() as u64,
            content.len() as u64,
            &mut window,
            &mut || {
                checkpoints += 1;
                if checkpoints == 4 {
                    Err("interrupted during proof")
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(
            result.err(),
            Some(PayloadStreamError::Consumer("interrupted during proof"))
        );
        assert_eq!(checkpoints, 4);
    }

    #[test]
    fn checkpoint_interrupts_emission_between_windows_and_reports_the_consumer_error() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload.payload");
        let content = payload(8 * WINDOW);
        fs::write(&path, &content).unwrap();
        let mut window = vec![0_u8; WINDOW];
        let stream = open(&path, &content, &mut window).unwrap().unwrap();

        let mut checkpoints = 0_u32;
        let mut emitted = 0;
        let error = stream
            .emit(
                &mut window,
                &mut || {
                    checkpoints += 1;
                    if checkpoints == 4 {
                        Err("interrupted during emission")
                    } else {
                        Ok(())
                    }
                },
                &mut |chunk| {
                    emitted += chunk.len();
                    Ok(())
                },
            )
            .unwrap_err();
        assert_eq!(
            error,
            PayloadStreamError::Consumer("interrupted during emission")
        );
        // One checkpoint precedes the identity recheck, then one per window.
        assert_eq!(emitted, 2 * WINDOW);
    }

    #[test]
    fn sink_refusal_stops_emission_and_is_reported_verbatim() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload.payload");
        let content = payload(4 * WINDOW);
        fs::write(&path, &content).unwrap();
        let mut window = vec![0_u8; WINDOW];
        let stream = open(&path, &content, &mut window).unwrap().unwrap();

        let mut windows = 0;
        let error = stream
            .emit(&mut window, &mut || Ok(()), &mut |_chunk| {
                windows += 1;
                if windows == 2 {
                    Err("sink budget")
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert_eq!(error, PayloadStreamError::Consumer("sink budget"));
        assert_eq!(windows, 2);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_payload_is_refused_at_open() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside");
        let content = payload(WINDOW);
        fs::write(&outside, &content).unwrap();
        let link = temp.path().join("link.payload");
        symlink(&outside, &link).unwrap();
        let mut window = vec![0_u8; WINDOW];
        assert_eq!(
            open(&link, &content, &mut window).err(),
            Some(PayloadStreamError::Payload(LcmError::InvalidPayloadRef))
        );
    }
}
