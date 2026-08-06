use super::*;
use sha2::Digest;
use std::io::{Read, Seek, SeekFrom};

pub(in crate::runtime::lcm::payload) fn read_verified_payload_text(
    path: &Path,
    expected_hash: &str,
    expected_bytes: u64,
    expected_chars: u64,
) -> Result<Option<(String, VerifiedPayloadAuthority)>, LcmError> {
    read_verified_payload_text_with_checkpoint(
        path,
        expected_hash,
        expected_bytes,
        expected_chars,
        &mut || Ok(()),
    )
}

pub(in crate::runtime::lcm::payload) fn read_verified_payload_text_with_checkpoint(
    path: &Path,
    expected_hash: &str,
    expected_bytes: u64,
    expected_chars: u64,
    checkpoint: &mut impl FnMut() -> Result<(), LcmError>,
) -> Result<Option<(String, VerifiedPayloadAuthority)>, LcmError> {
    super::read_verified_payload_file_with_checkpoint(
        path,
        expected_hash,
        expected_bytes,
        expected_chars,
        checkpoint,
    )
    .map(|verified| verified.map(|(content, authority)| (validated_string(content), authority)))
}

pub(super) fn read_stable_payload_bytes_with<F>(
    file: &mut fs::File,
    path: &Path,
    expected_identity: &PayloadFileIdentity,
    after_read: F,
) -> Result<Vec<u8>, LcmError>
where
    F: FnOnce() -> Result<(), LcmError>,
{
    read_stable_payload_bytes_bounded_with(
        file,
        path,
        expected_identity,
        MAX_VERIFIED_PAYLOAD_FILE_BYTES,
        after_read,
    )
}

pub(super) fn read_stable_payload_bytes_bounded_with<F>(
    file: &mut fs::File,
    path: &Path,
    expected_identity: &PayloadFileIdentity,
    max_bytes: u64,
    after_read: F,
) -> Result<Vec<u8>, LcmError>
where
    F: FnOnce() -> Result<(), LcmError>,
{
    read_stable_payload_bytes_bounded_with_checkpoint(
        file,
        path,
        expected_identity,
        max_bytes,
        after_read,
        &mut || Ok(()),
    )
}

pub(super) fn read_stable_payload_bytes_bounded_with_checkpoint<F>(
    file: &mut fs::File,
    path: &Path,
    expected_identity: &PayloadFileIdentity,
    max_bytes: u64,
    after_read: F,
    checkpoint: &mut impl FnMut() -> Result<(), LcmError>,
) -> Result<Vec<u8>, LcmError>
where
    F: FnOnce() -> Result<(), LcmError>,
{
    checkpoint()?;
    let max_bytes = max_bytes.min(MAX_VERIFIED_PAYLOAD_FILE_BYTES);
    let before = file
        .metadata()
        .map_err(|error| LcmError::Io(error.to_string()))?;
    ensure_regular_non_reparse_file(&before)?;
    let before_identity = payload_file_identity(file, &before)?;
    same_payload_file_identity(&before_identity, expected_identity)?;
    if before.len() > max_bytes {
        return Err(LcmError::PayloadIntegrityMismatch);
    }

    let initial_capacity = usize::try_from(before.len())
        .unwrap_or(MAX_PAYLOAD_READ_PREALLOC_BYTES)
        .min(MAX_PAYLOAD_READ_PREALLOC_BYTES);
    let mut content = Vec::with_capacity(initial_capacity);
    file.seek(SeekFrom::Start(0))
        .map_err(|error| LcmError::Io(error.to_string()))?;
    let mut bounded = file.take(max_bytes.saturating_add(1));
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        checkpoint()?;
        let count = bounded
            .read(&mut chunk)
            .map_err(|error| LcmError::Io(error.to_string()))?;
        if count == 0 {
            break;
        }
        content.extend_from_slice(&chunk[..count]);
    }
    if u64::try_from(content.len()).map_or(true, |length| length > max_bytes) {
        return Err(LcmError::PayloadIntegrityMismatch);
    }
    after_read()?;
    checkpoint()?;

    let (after, _lstat, after_identity) = verify_opened_payload_file(file, path)?;
    same_payload_file_identity(&after_identity, expected_identity)?;
    if before.len() != after.len() || after.len() != content.len() as u64 {
        return Err(LcmError::PayloadIntegrityMismatch);
    }
    Ok(content)
}

pub(super) fn authority_for_content(
    path: &Path,
    identity: PayloadFileIdentity,
    content: &[u8],
) -> Result<VerifiedPayloadAuthority, LcmError> {
    authority_for_content_with_checkpoint(path, identity, content, &mut || Ok(()))
}

pub(super) fn authority_for_content_with_checkpoint(
    path: &Path,
    identity: PayloadFileIdentity,
    content: &[u8],
    checkpoint: &mut impl FnMut() -> Result<(), LcmError>,
) -> Result<VerifiedPayloadAuthority, LcmError> {
    let (content_hash, char_count) = scan_utf8_content(content, checkpoint)?;
    Ok(VerifiedPayloadAuthority {
        locator: path.to_path_buf(),
        identity,
        content_hash,
        byte_count: content.len() as u64,
        char_count,
    })
}

pub(super) fn validated_string(content: Vec<u8>) -> String {
    // SAFETY: every production caller obtains these bytes from
    // `authority_for_content_with_checkpoint`, whose single-pass scanner has
    // already validated the complete byte sequence as UTF-8.
    unsafe { String::from_utf8_unchecked(content) }
}

fn scan_utf8_content(
    content: &[u8],
    checkpoint: &mut impl FnMut() -> Result<(), LcmError>,
) -> Result<(String, u64), LcmError> {
    checkpoint()?;
    let mut hasher = sha2::Sha256::new();
    let mut char_count = 0_u64;
    let mut utf8 = Utf8State::default();
    for chunk in content.chunks(64 * 1024) {
        checkpoint()?;
        hasher.update(chunk);
        char_count = char_count
            .checked_add(utf8.validate(chunk)?)
            .ok_or(LcmError::PayloadIntegrityMismatch)?;
    }
    utf8.finish()?;
    checkpoint()?;
    Ok((hex::encode(hasher.finalize()), char_count))
}

#[derive(Default)]
struct Utf8State {
    continuation_bytes: u8,
    next_min: u8,
    next_max: u8,
}

impl Utf8State {
    fn validate(&mut self, chunk: &[u8]) -> Result<u64, LcmError> {
        let mut chars = 0_u64;
        for &byte in chunk {
            if self.continuation_bytes > 0 {
                if byte < self.next_min || byte > self.next_max {
                    return Err(LcmError::PayloadIntegrityMismatch);
                }
                self.continuation_bytes -= 1;
                self.next_min = 0x80;
                self.next_max = 0xbf;
                continue;
            }
            chars += 1;
            match byte {
                0x00..=0x7f => {}
                0xc2..=0xdf => self.start(1, 0x80, 0xbf),
                0xe0 => self.start(2, 0xa0, 0xbf),
                0xe1..=0xec | 0xee..=0xef => self.start(2, 0x80, 0xbf),
                0xed => self.start(2, 0x80, 0x9f),
                0xf0 => self.start(3, 0x90, 0xbf),
                0xf1..=0xf3 => self.start(3, 0x80, 0xbf),
                0xf4 => self.start(3, 0x80, 0x8f),
                _ => return Err(LcmError::PayloadIntegrityMismatch),
            }
        }
        Ok(chars)
    }

    fn start(&mut self, continuation_bytes: u8, next_min: u8, next_max: u8) {
        self.continuation_bytes = continuation_bytes;
        self.next_min = next_min;
        self.next_max = next_max;
    }

    fn finish(self) -> Result<(), LcmError> {
        if self.continuation_bytes == 0 {
            Ok(())
        } else {
            Err(LcmError::PayloadIntegrityMismatch)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn utf8_scalar_split_at_verification_window_is_accepted_once() {
        let mut content = vec![b'x'; 64 * 1024 - 1];
        content.extend_from_slice("雪".as_bytes());
        let mut checkpoints = 0;
        let (_, chars) = scan_utf8_content(&content, &mut || {
            checkpoints += 1;
            Ok(())
        })
        .expect("valid split scalar");
        assert_eq!(chars, 64 * 1024);
        assert!(checkpoints >= 4);
    }

    #[test]
    fn incomplete_utf8_suffix_is_rejected() {
        let error =
            scan_utf8_content(&[b'a', 0xe9, 0x9b], &mut || Ok(())).expect_err("incomplete scalar");
        assert_eq!(error, LcmError::PayloadIntegrityMismatch);
    }

    #[test]
    fn overlong_and_surrogate_utf8_are_rejected() {
        for invalid in [&[0xc0, 0x80][..], &[0xed, 0xa0, 0x80][..]] {
            assert_eq!(
                scan_utf8_content(invalid, &mut || Ok(())),
                Err(LcmError::PayloadIntegrityMismatch)
            );
        }
    }

    #[test]
    fn sixteen_mib_validation_is_cooperatively_cancellable() {
        let content = vec![b'x'; 16 * 1024 * 1024];
        let mut checkpoints = 0;
        let error = scan_utf8_content(&content, &mut || {
            checkpoints += 1;
            if checkpoints == 12 {
                Err(LcmError::Cancelled)
            } else {
                Ok(())
            }
        })
        .expect_err("cancel validation");
        assert_eq!(error, LcmError::Cancelled);
        assert_eq!(checkpoints, 12);
    }

    #[test]
    fn verified_read_checks_control_during_whole_file_io() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload.payload");
        let content = vec![b'x'; 512 * 1024];
        fs::write(&path, &content).unwrap();
        let hash = super::super::super::util::sha256_hex(&content);
        let mut checkpoints = 0;
        let error = read_verified_payload_file_with_checkpoint(
            &path,
            &hash,
            content.len() as u64,
            content.len() as u64,
            &mut || {
                checkpoints += 1;
                if checkpoints >= 5 {
                    Err(LcmError::Cancelled)
                } else {
                    Ok(())
                }
            },
        )
        .expect_err("control must interrupt a multi-chunk verified read");
        assert_eq!(error, LcmError::Cancelled);
        assert!(
            checkpoints >= 5,
            "verification did not reach an in-file checkpoint"
        );
    }

    #[test]
    fn verified_read_preserves_deadline_interruption() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload.payload");
        let content = vec![b'x'; 256 * 1024];
        fs::write(&path, &content).unwrap();
        let hash = super::super::super::util::sha256_hex(&content);
        let mut checkpoints = 0;
        let error = read_verified_payload_file_with_checkpoint(
            &path,
            &hash,
            content.len() as u64,
            content.len() as u64,
            &mut || {
                checkpoints += 1;
                if checkpoints >= 4 {
                    Err(LcmError::DeadlineExceeded)
                } else {
                    Ok(())
                }
            },
        )
        .expect_err("deadline must interrupt a multi-chunk verified read");
        assert_eq!(error, LcmError::DeadlineExceeded);
    }
}
