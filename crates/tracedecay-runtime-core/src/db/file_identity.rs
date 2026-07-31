use std::path::Path;

use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

/// Why a `SQLite` source could not yield a stable physical identity.
#[derive(Debug)]
#[allow(dead_code)]
pub(crate) enum SqliteFileIdentityError {
    Open,
    Inspect,
    Identify,
    Unavailable,
}

/// Stable 64-bit physical identity for a SQLite-backed source, derived from the
/// file's inode (Unix) or volume/file-index handle identity (Windows). Callers
/// layer their own generation/resume fingerprints on top; the hashed inputs must
/// stay byte-identical across authorities that persist this identity.
pub(crate) fn sqlite_generation_identity(path: &Path) -> Result<u64, SqliteFileIdentityError> {
    #[cfg(unix)]
    {
        let metadata = std::fs::metadata(path).map_err(|_| SqliteFileIdentityError::Inspect)?;
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
        let file = std::fs::File::open(path).map_err(|_| SqliteFileIdentityError::Open)?;
        crate::windows_file::stable_file_identity(&file, path)
            .map_err(|_| SqliteFileIdentityError::Identify)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(SqliteFileIdentityError::Unavailable)
    }
}
