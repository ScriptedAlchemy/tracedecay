use std::fmt;
use std::path::Path;

#[cfg(any(unix, windows))]
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;

/// The path-free filesystem operation that failed while deriving a stable
/// `SQLite` identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SqliteFileIdentityOperation {
    Open,
    Inspect,
    Identify,
    PlatformSupport,
}

impl SqliteFileIdentityOperation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Inspect => "inspect",
            Self::Identify => "identify",
            Self::PlatformSupport => "platform_support",
        }
    }
}

/// Privacy-safe classification of an identity I/O failure. This deliberately
/// excludes paths, raw OS error strings, and platform error codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SqliteFileIdentityErrorCategory {
    NotFound,
    PermissionDenied,
    InvalidInput,
    InvalidData,
    ResourceUnavailable,
    Interrupted,
    Unsupported,
    Other,
}

impl SqliteFileIdentityErrorCategory {
    fn from_io(error: &std::io::Error) -> Self {
        match error.kind() {
            std::io::ErrorKind::NotFound => Self::NotFound,
            std::io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            std::io::ErrorKind::InvalidInput
            | std::io::ErrorKind::NotADirectory
            | std::io::ErrorKind::IsADirectory => Self::InvalidInput,
            std::io::ErrorKind::InvalidData => Self::InvalidData,
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
                Self::ResourceUnavailable
            }
            std::io::ErrorKind::Interrupted => Self::Interrupted,
            std::io::ErrorKind::Unsupported => Self::Unsupported,
            _ => Self::Other,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::PermissionDenied => "permission_denied",
            Self::InvalidInput => "invalid_input",
            Self::InvalidData => "invalid_data",
            Self::ResourceUnavailable => "resource_unavailable",
            Self::Interrupted => "interrupted",
            Self::Unsupported => "unsupported",
            Self::Other => "other",
        }
    }
}

/// Why a `SQLite` source could not yield a stable physical identity.
///
/// The serialized representation is intentionally limited to the stable
/// operation and privacy-safe category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SqliteFileIdentityError {
    operation: SqliteFileIdentityOperation,
    category: SqliteFileIdentityErrorCategory,
}

impl SqliteFileIdentityError {
    fn io(operation: SqliteFileIdentityOperation, error: &std::io::Error) -> Self {
        Self {
            operation,
            category: SqliteFileIdentityErrorCategory::from_io(error),
        }
    }

    #[cfg(not(any(unix, windows)))]
    const fn unsupported() -> Self {
        Self {
            operation: SqliteFileIdentityOperation::PlatformSupport,
            category: SqliteFileIdentityErrorCategory::Unsupported,
        }
    }

    #[must_use]
    pub const fn operation(self) -> SqliteFileIdentityOperation {
        self.operation
    }

    #[must_use]
    pub const fn category(self) -> SqliteFileIdentityErrorCategory {
        self.category
    }
}

impl fmt::Display for SqliteFileIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} failed ({})",
            self.operation.as_str(),
            self.category.as_str()
        )
    }
}

/// Stable 64-bit physical identity for a SQLite-backed source, derived from the
/// file's inode (Unix) or volume/file-index handle identity (Windows). Callers
/// layer their own generation/resume fingerprints on top; the hashed inputs must
/// stay byte-identical across authorities that persist this identity.
pub fn sqlite_generation_identity(path: &Path) -> Result<u64, SqliteFileIdentityError> {
    #[cfg(unix)]
    {
        let metadata = std::fs::metadata(path).map_err(|error| {
            SqliteFileIdentityError::io(SqliteFileIdentityOperation::Inspect, &error)
        })?;
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
        let file = std::fs::File::open(path).map_err(|error| {
            SqliteFileIdentityError::io(SqliteFileIdentityOperation::Open, &error)
        })?;
        stable_file_identity(&file, path).map_err(|error| {
            SqliteFileIdentityError::io(SqliteFileIdentityOperation::Identify, &error)
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(SqliteFileIdentityError::unsupported())
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

#[cfg(test)]
mod tests {
    use super::{
        SqliteFileIdentityErrorCategory, SqliteFileIdentityOperation, sqlite_generation_identity,
    };

    #[test]
    fn missing_sqlite_identity_is_typed_and_serializes_without_its_path() {
        let directory = tempfile::tempdir().expect("create identity test directory");
        let secret_path = directory.path().join("private-provider-session.sqlite3");

        let error = sqlite_generation_identity(&secret_path)
            .expect_err("a missing SQLite authority cannot have a physical identity");

        assert!(matches!(
            error.operation(),
            SqliteFileIdentityOperation::Inspect | SqliteFileIdentityOperation::Open
        ));
        assert_eq!(error.category(), SqliteFileIdentityErrorCategory::NotFound);
        let serialized = serde_json::to_string(&error).expect("serialize typed identity failure");
        #[cfg(unix)]
        assert_eq!(
            serialized,
            r#"{"operation":"inspect","category":"not_found"}"#
        );
        #[cfg(windows)]
        assert_eq!(serialized, r#"{"operation":"open","category":"not_found"}"#);
    }

    #[test]
    fn filesystem_errors_collapse_to_stable_privacy_safe_categories() {
        for (kind, expected) in [
            (
                std::io::ErrorKind::PermissionDenied,
                SqliteFileIdentityErrorCategory::PermissionDenied,
            ),
            (
                std::io::ErrorKind::InvalidInput,
                SqliteFileIdentityErrorCategory::InvalidInput,
            ),
            (
                std::io::ErrorKind::InvalidData,
                SqliteFileIdentityErrorCategory::InvalidData,
            ),
            (
                std::io::ErrorKind::WouldBlock,
                SqliteFileIdentityErrorCategory::ResourceUnavailable,
            ),
            (
                std::io::ErrorKind::Interrupted,
                SqliteFileIdentityErrorCategory::Interrupted,
            ),
            (
                std::io::ErrorKind::Unsupported,
                SqliteFileIdentityErrorCategory::Unsupported,
            ),
            (
                std::io::ErrorKind::Other,
                SqliteFileIdentityErrorCategory::Other,
            ),
        ] {
            let error = std::io::Error::new(kind, "private raw operating-system detail");
            assert_eq!(SqliteFileIdentityErrorCategory::from_io(&error), expected);
        }
    }

    #[test]
    fn invalid_identity_data_serializes_without_raw_io_detail() {
        let raw = std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "private provider/session bytes were malformed",
        );
        let error = super::SqliteFileIdentityError::io(SqliteFileIdentityOperation::Identify, &raw);

        assert_eq!(
            error.category(),
            SqliteFileIdentityErrorCategory::InvalidData
        );
        assert_eq!(
            serde_json::to_string(&error).expect("serialize invalid-data classification"),
            r#"{"operation":"identify","category":"invalid_data"}"#
        );
        assert!(!error.to_string().contains("provider/session"));
        assert!(!error.to_string().contains("malformed"));
    }
}
