use std::path::Path;

use tracedecay_domain::canonical_text::sha256_hex;

/// How [`read_decoded_source_file`] classified the bytes it decoded.
///
/// Reference line offsets use the canonical decoded text. Snippet and
/// text-only scans that historically used `read_to_string` follow this tag:
/// UTF-8 keeps the BOM character, and UTF-16 has no raw UTF-8 body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceTextEncoding {
    Utf8 { leading_bom: bool },
    Utf16Le,
    Utf16Be,
}

/// One canonical decode of a source file.
pub struct DecodedSourceFile {
    pub text: String,
    pub encoding: SourceTextEncoding,
}

/// Read and decode a source file once.
///
/// UTF-16 LE/BE is detected via BOM. UTF-8 BOMs are stripped from [`DecodedSourceFile::text`]
/// and reported on [`SourceTextEncoding`]. Returns an IO error only when the
/// file genuinely cannot be read or decoded.
#[tracing::instrument(name = "source.read", level = "trace", skip_all)]
pub fn read_decoded_source_file(path: &Path) -> std::io::Result<DecodedSourceFile> {
    decode_source_bytes(&read_file_bytes(path)?)
}

/// Canonical decoded text of a source file.
///
/// Callers that also need the encoding classification use [`read_decoded_source_file`].
pub fn read_source_file(path: &Path) -> std::io::Result<String> {
    read_decoded_source_file(path).map(|decoded| decoded.text)
}

fn decode_source_bytes(bytes: &[u8]) -> std::io::Result<DecodedSourceFile> {
    let decode_unit = if bytes.starts_with(&[0xFF, 0xFE]) {
        Some((
            SourceTextEncoding::Utf16Le,
            u16::from_le_bytes as fn([u8; 2]) -> u16,
        ))
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        Some((
            SourceTextEncoding::Utf16Be,
            u16::from_be_bytes as fn([u8; 2]) -> u16,
        ))
    } else {
        None
    };
    if let Some((encoding, decode_unit)) = decode_unit {
        let (units, remainder) = bytes[2..].as_chunks::<2>();
        if !remainder.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "incomplete UTF-16 code unit",
            ));
        }
        let u16s: Vec<u16> = units.iter().copied().map(decode_unit).collect();
        let text = String::from_utf16(&u16s)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        return Ok(DecodedSourceFile { text, encoding });
    }

    // Strip UTF-8 BOM if present, then validate. The tag preserves the
    // historical snippet distinction: raw UTF-8 kept the BOM character.
    let leading_bom = bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
    let start = if leading_bom { 3 } else { 0 };
    let text = String::from_utf8(bytes[start..].to_vec())
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    Ok(DecodedSourceFile {
        text,
        encoding: SourceTextEncoding::Utf8 { leading_bom },
    })
}

/// Reads a file's bytes, absorbing transient Windows file locks.
///
/// On Windows, antivirus scanners and the search indexer briefly open
/// freshly written files with exclusive access; an unlucky open during that
/// window fails with a sharing violation or "Access is denied" (os error 5)
/// even though the file is readable milliseconds later. Callers treat read
/// errors as "skip this file" or fail the whole sync, so a genuinely
/// readable file must not be lost to that window, retry briefly before
/// giving up. Other platforms read directly: `PermissionDenied` there is a
/// real ACL problem that retrying cannot fix.
fn read_file_bytes(path: &Path) -> std::io::Result<Vec<u8>> {
    const RETRY_DELAYS_MS: [u64; 4] = [10, 20, 40, 80];
    if !cfg!(windows) {
        return std::fs::read(path);
    }
    let mut delays = RETRY_DELAYS_MS.iter();
    loop {
        match std::fs::read(path) {
            Err(err) if is_transient_windows_file_lock(&err) => match delays.next() {
                Some(&delay_ms) => {
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                }
                None => return Err(err),
            },
            result => return result,
        }
    }
}

/// True for Windows errors that indicate another process is briefly holding
/// the file: `ERROR_SHARING_VIOLATION` (32) and `ERROR_LOCK_VIOLATION` (33)
/// map through as raw OS errors, while Defender-style scans surface as plain
/// `PermissionDenied` (`ERROR_ACCESS_DENIED`, os error 5).
fn is_transient_windows_file_lock(err: &std::io::Error) -> bool {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const ERROR_LOCK_VIOLATION: i32 = 33;
    matches!(
        err.raw_os_error(),
        Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
    ) || err.kind() == std::io::ErrorKind::PermissionDenied
}

/// Get filesystem mtime (seconds since epoch) and size for pre-filter.
pub fn file_stat(path: &Path) -> Option<(i64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let secs = mtime.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    Some((secs, meta.len()))
}

/// Compute SHA-256 content hash of file content.
pub fn content_hash(content: &str) -> String {
    sha256_hex(content.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::{SourceTextEncoding, read_decoded_source_file, read_source_file};
    use std::io::ErrorKind;

    fn read_bytes(bytes: &[u8]) -> std::io::Result<String> {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), bytes).unwrap();
        read_source_file(file.path())
    }

    fn encoding_of(bytes: &[u8]) -> SourceTextEncoding {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), bytes).unwrap();
        read_decoded_source_file(file.path()).unwrap().encoding
    }

    #[test]
    fn source_utf16_refuses_incomplete_units() {
        for bytes in [
            &b"\xFF\xFE\x01"[..],
            &b"\xFE\xFF\x01"[..],
            &b"\xFF\xFE\x41\x00\x01"[..],
            &b"\xFE\xFF\x00\x41\x01"[..],
        ] {
            let error = read_bytes(bytes).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidData);
            assert_eq!(error.to_string(), "incomplete UTF-16 code unit");
        }
    }

    #[test]
    fn source_utf16_preserves_strict_surrogate_decoding() {
        for bytes in [
            &b"\xFF\xFE\x00\xD8"[..],
            &b"\xFE\xFF\xD8\x00"[..],
            &b"\xFF\xFE\x00\xDC"[..],
            &b"\xFE\xFF\xDC\x00"[..],
        ] {
            assert_eq!(
                read_bytes(bytes).unwrap_err().kind(),
                ErrorKind::InvalidData
            );
        }
        for bytes in [
            &b"\xFF\xFE\x41\x00\x3D\xD8\x00\xDE\x0A\x00"[..],
            &b"\xFE\xFF\x00\x41\xD8\x3D\xDE\x00\x00\x0A"[..],
        ] {
            assert_eq!(read_bytes(bytes).unwrap(), "A😀\n");
        }
        for bytes in [&b"\xFF\xFE"[..], &b"\xFE\xFF"[..]] {
            assert_eq!(read_bytes(bytes).unwrap(), "");
        }
    }

    #[test]
    fn source_utf8_preserves_bom_and_strict_decoding() {
        for bytes in [&b"\xEF\xBB\xBFhi\n"[..], &b"hi\n"[..]] {
            assert_eq!(read_bytes(bytes).unwrap(), "hi\n");
        }
        assert_eq!(read_bytes(b"").unwrap(), "");
        assert_eq!(
            encoding_of(b"hi\n"),
            SourceTextEncoding::Utf8 { leading_bom: false }
        );
        assert_eq!(
            encoding_of(b"\xEF\xBB\xBFhi\n"),
            SourceTextEncoding::Utf8 { leading_bom: true }
        );
        assert_eq!(encoding_of(b"\xFF\xFEhi"), SourceTextEncoding::Utf16Le);
        assert_eq!(encoding_of(b"\xFE\xFFhi"), SourceTextEncoding::Utf16Be);
        for bytes in [&b"\x80"[..], &b"\xEF\xBB\xBF\x80"[..]] {
            assert_eq!(
                read_bytes(bytes).unwrap_err().kind(),
                ErrorKind::InvalidData
            );
        }
    }
}
