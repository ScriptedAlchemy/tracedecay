use std::path::Path;

use tracedecay_domain::canonical_text::sha256_hex;

/// Read a source file to a UTF-8 string, transparently handling UTF-16 LE/BE
/// (detected via BOM). Returns an IO error only when the file genuinely cannot
/// be read or decoded.
#[tracing::instrument(name = "source.read", level = "trace", skip_all)]
pub fn read_source_file(path: &Path) -> std::io::Result<String> {
    let bytes = read_file_bytes(path)?;

    let decode_unit = if bytes.starts_with(&[0xFF, 0xFE]) {
        Some(u16::from_le_bytes as fn([u8; 2]) -> u16)
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        Some(u16::from_be_bytes as fn([u8; 2]) -> u16)
    } else {
        None
    };
    if let Some(decode_unit) = decode_unit {
        let (units, remainder) = bytes[2..].as_chunks::<2>();
        if !remainder.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "incomplete UTF-16 code unit",
            ));
        }
        let u16s: Vec<u16> = units.iter().copied().map(decode_unit).collect();
        return String::from_utf16(&u16s)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e));
    }

    // Strip UTF-8 BOM if present, then validate
    let start = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        3
    } else {
        0
    };
    String::from_utf8(bytes[start..].to_vec())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
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

/// Compute SHA-256 content hash of file content.
pub fn content_hash(content: &str) -> String {
    sha256_hex(content.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::read_source_file;
    use std::io::ErrorKind;

    fn read_bytes(bytes: &[u8]) -> std::io::Result<String> {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), bytes).unwrap();
        read_source_file(file.path())
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
        for bytes in [&b"\x80"[..], &b"\xEF\xBB\xBF\x80"[..]] {
            assert_eq!(
                read_bytes(bytes).unwrap_err().kind(),
                ErrorKind::InvalidData
            );
        }
    }
}
