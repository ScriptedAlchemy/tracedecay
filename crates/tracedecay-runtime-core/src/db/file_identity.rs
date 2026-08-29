use std::path::Path;

#[cfg(any(unix, windows))]
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;

/// Why a `SQLite` source could not yield a stable physical identity.
#[derive(Debug)]
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
        stable_file_identity(&file, path).map_err(|_| SqliteFileIdentityError::Identify)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(SqliteFileIdentityError::Unavailable)
    }
}

#[cfg(windows)]
fn stable_file_identity(file: &std::fs::File, path: &Path) -> std::io::Result<u64> {
    let metadata = file.metadata()?;
    let mut hasher = Sha256::new();
    if let Ok(information) = tracedecay_private_fs::windows_file::information(file) {
        hasher.update(b"windows-file-id");
        hasher.update(information.volume_serial_number.to_le_bytes());
        hasher.update(information.file_index.to_le_bytes());
    } else {
        hasher.update(b"windows-file-id-fallback");
        hasher.update(metadata.creation_time().to_le_bytes());
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        hasher.update(crate::os_str_bytes::native_os_str_bytes(
            canonical.as_os_str(),
        ));
    }
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    Ok(u64::from_le_bytes(bytes).max(1))
}
