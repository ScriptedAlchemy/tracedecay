use std::path::Path;

use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

/// Why a `SQLite` source could not yield a stable physical identity.
#[derive(Debug)]
#[allow(dead_code)]
pub enum SqliteFileIdentityError {
    Open,
    Inspect,
    Identify,
    Unavailable,
}

/// Stable 64-bit physical identity for a SQLite-backed source, derived from the
/// file's inode (Unix) or volume/file-index handle identity (Windows). Callers
/// layer their own generation/resume fingerprints on top; the hashed inputs must
/// stay byte-identical across authorities that persist this identity.
pub fn sqlite_generation_identity(path: &Path) -> Result<u64, SqliteFileIdentityError> {
    let file = std::fs::File::open(path).map_err(|_| SqliteFileIdentityError::Open)?;
    file_generation_identity(&file, path)
}

/// Stable physical identity for an already-open file.
///
/// Deriving identity from the handle rather than reopening the path ensures a
/// concurrent replacement cannot bind a cursor to a different file than the
/// one the caller will read.
pub fn file_generation_identity(
    file: &std::fs::File,
    path: &Path,
) -> Result<u64, SqliteFileIdentityError> {
    #[cfg(unix)]
    {
        let _ = path;
        let metadata = file
            .metadata()
            .map_err(|_| SqliteFileIdentityError::Inspect)?;
        let mut hasher = Sha256::new();
        hasher.update(metadata.dev().to_le_bytes());
        hasher.update(metadata.ino().to_le_bytes());
        let digest = hasher.finalize();
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(&digest[..8]);
        Ok(u64::from_le_bytes(bytes).max(1))
    }
    #[cfg(windows)]
    {
        crate::windows_file::stable_file_identity(file, path)
            .map_err(|_| SqliteFileIdentityError::Identify)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        let _ = path;
        Err(SqliteFileIdentityError::Unavailable)
    }
}

/// Computes a cursor anchor from bytes already captured by the parser.
///
/// `leading` is the first `min(byte_offset, 4KiB)` bytes and `trailing` is the
/// final 4KiB before frontiers beyond that window. Callers use this after
/// parsing so the committed anchor cannot be sampled from different live file
/// contents than the rows it acknowledges.
pub fn resume_fingerprint_from_windows(
    byte_offset: u64,
    leading: &[u8],
    trailing: &[u8],
) -> Result<u64, SqliteFileIdentityError> {
    const WINDOW: usize = 4 * 1024;
    let expected_leading = usize::try_from(byte_offset.min(WINDOW as u64))
        .map_err(|_| SqliteFileIdentityError::Inspect)?;
    if leading.len() != expected_leading
        || (byte_offset > WINDOW as u64 && trailing.len() != WINDOW)
    {
        return Err(SqliteFileIdentityError::Inspect);
    }
    let mut hasher = Sha256::new();
    hasher.update(byte_offset.to_le_bytes());
    hasher.update(leading);
    if byte_offset > WINDOW as u64 {
        hasher.update(trailing);
    }
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    Ok(u64::from_le_bytes(bytes).max(1))
}

/// Computes an anchor from a retained leading window and a captured range
/// containing the bytes immediately before `frontier`.
pub fn resume_fingerprint_from_capture(
    frontier: u64,
    leading: &[u8],
    captured_start: u64,
    captured: &[u8],
) -> Result<u64, SqliteFileIdentityError> {
    const WINDOW: u64 = 4 * 1024;
    let leading_len =
        usize::try_from(frontier.min(WINDOW)).map_err(|_| SqliteFileIdentityError::Inspect)?;
    let trailing = if frontier > WINDOW {
        let trailing_start = frontier
            .checked_sub(WINDOW)
            .and_then(|offset| offset.checked_sub(captured_start))
            .ok_or(SqliteFileIdentityError::Inspect)?;
        let trailing_end = frontier
            .checked_sub(captured_start)
            .ok_or(SqliteFileIdentityError::Inspect)?;
        let start =
            usize::try_from(trailing_start).map_err(|_| SqliteFileIdentityError::Inspect)?;
        let end = usize::try_from(trailing_end).map_err(|_| SqliteFileIdentityError::Inspect)?;
        captured
            .get(start..end)
            .ok_or(SqliteFileIdentityError::Inspect)?
    } else {
        &[]
    };
    resume_fingerprint_from_windows(
        frontier,
        leading
            .get(..leading_len)
            .ok_or(SqliteFileIdentityError::Inspect)?,
        trailing,
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::{
        file_generation_identity, resume_fingerprint_from_capture, resume_fingerprint_from_windows,
    };

    fn fingerprint(bytes: &[u8], frontier: usize) -> u64 {
        let leading = &bytes[..frontier.min(4096)];
        let trailing = if frontier > 4096 {
            &bytes[frontier - 4096..frontier]
        } else {
            &[]
        };
        resume_fingerprint_from_windows(frontier as u64, leading, trailing).expect("fingerprint")
    }

    #[test]
    fn open_handle_identity_distinguishes_path_replacement() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("events.jsonl");
        let replacement = directory.path().join("replacement.jsonl");
        std::fs::write(&path, b"first\n").expect("first file");
        let first = std::fs::File::open(&path).expect("open first file");
        let first_identity = file_generation_identity(&first, &path).expect("first identity");

        std::fs::write(&replacement, b"replacement\n").expect("replacement file");
        std::fs::rename(&replacement, &path).expect("replace path");
        let second = std::fs::File::open(&path).expect("open replacement");
        let second_identity = file_generation_identity(&second, &path).expect("second identity");

        assert_ne!(first_identity, second_identity);
        assert_eq!(
            file_generation_identity(&first, &path).expect("retained handle identity"),
            first_identity
        );
    }

    #[test]
    fn resume_fingerprint_preserves_append_and_detects_in_place_rewrite() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("events.jsonl");
        std::fs::write(&path, b"first\nsecond\n").expect("initial file");
        let original = std::fs::read(&path).expect("read initial file");
        let frontier = original.len();
        let original_fingerprint = fingerprint(&original, frontier);

        let mut appended = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("append handle");
        std::io::Write::write_all(&mut appended, b"third\n").expect("append");
        let appended_file = std::fs::read(&path).expect("read appended file");
        assert_eq!(fingerprint(&appended_file, frontier), original_fingerprint);

        std::fs::write(&path, b"other\nvalues\nthird\n").expect("rewrite in place");
        let rewritten = std::fs::read(&path).expect("read rewritten file");
        assert_ne!(fingerprint(&rewritten, frontier), original_fingerprint);
    }

    #[test]
    fn captured_anchor_retains_the_bytes_that_were_parsed() {
        let original = b"first\nsecond\n";
        let frontier = original.len() as u64;
        let captured =
            resume_fingerprint_from_capture(frontier, original, 0, original).expect("anchor");

        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("events.jsonl");
        std::fs::write(&path, b"other\nvalues\n").expect("rewritten source");
        let rewritten = std::fs::read(path).expect("read rewritten source");

        assert_ne!(captured, fingerprint(&rewritten, frontier as usize));
    }
}
