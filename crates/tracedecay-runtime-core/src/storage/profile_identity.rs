//! Canonical on-disk profile-identity record and its fail-closed reader.
//!
//! The daemon remains the write/mint authority that publishes this file.
//! Backup, restore, and other readers consume this same record shape and
//! validation chain so corrupt or invalid material cannot fork a second
//! interpretation.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{BrainId, UserProfileId};

use crate::db::DatabaseAuthority;
use tracedecay_domain::errors::{Result, TraceDecayError};

/// On-disk schema version accepted by [`read_existing_profile_identity_record`].
pub const PROFILE_IDENTITY_SCHEMA_VERSION: u32 = 1;
/// Operator-facing name used in fail-closed identity diagnostics.
pub const PROFILE_IDENTITY_RECORD_NAME: &str = "profile identity record";

/// Exact final profile-identity record stored as `profile-identity.json`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileIdentityRecordV1 {
    pub schema_version: u32,
    pub brain_id: BrainId,
    pub profile_id: UserProfileId,
}

/// Reads the persisted profile-identity record, failing closed on anything
/// other than an absent path.
///
/// Returns `Ok(None)` when the file is missing. Symlinks, non-regular files,
/// non-0600 mode, unknown fields, unsupported schema, and invalid ids are
/// rejected. Callers that require a record map `None` themselves.
pub fn read_existing_profile_identity_record(
    path: &Path,
) -> Result<Option<ProfileIdentityRecordV1>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(profile_identity_io("inspect", path, &error)),
    };
    validate_private_identity_file(path, &metadata)?;
    let encoded = DatabaseAuthority::read_record_strict(path, PROFILE_IDENTITY_RECORD_NAME)?
        .ok_or_else(|| TraceDecayError::Config {
            message: format!(
                "{PROFILE_IDENTITY_RECORD_NAME} '{}' disappeared while being read",
                path.display()
            ),
        })?;
    let record = serde_json::from_str::<ProfileIdentityRecordV1>(&encoded).map_err(|error| {
        TraceDecayError::Config {
            message: format!(
                "invalid {PROFILE_IDENTITY_RECORD_NAME} '{}': {error}",
                path.display()
            ),
        }
    })?;
    validate_record(path, &record)?;
    Ok(Some(record))
}

fn validate_record(path: &Path, record: &ProfileIdentityRecordV1) -> Result<()> {
    if record.schema_version != PROFILE_IDENTITY_SCHEMA_VERSION {
        return Err(TraceDecayError::Config {
            message: format!(
                "unsupported {PROFILE_IDENTITY_RECORD_NAME} schema_version={} in '{}'",
                record.schema_version,
                path.display()
            ),
        });
    }
    record
        .brain_id
        .validate()
        .map_err(|error| invalid_identity(path, "brain_id", error))?;
    record
        .profile_id
        .validate()
        .map_err(|error| invalid_identity(path, "profile_id", error))
}

fn validate_private_identity_file(path: &Path, metadata: &std::fs::Metadata) -> Result<()> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(TraceDecayError::Config {
            message: format!(
                "{PROFILE_IDENTITY_RECORD_NAME} '{}' must be a private regular file",
                path.display()
            ),
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o777 != 0o600 {
            return Err(TraceDecayError::Config {
                message: format!(
                    "{PROFILE_IDENTITY_RECORD_NAME} '{}' must have permissions 0600",
                    path.display()
                ),
            });
        }
    }
    Ok(())
}

fn invalid_identity(path: &Path, field: &str, error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "invalid {field} in {PROFILE_IDENTITY_RECORD_NAME} '{}': {error}",
            path.display()
        ),
    }
}

fn profile_identity_io(operation: &str, path: &Path, error: &std::io::Error) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "failed to {operation} {PROFILE_IDENTITY_RECORD_NAME} '{}': {error}",
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::PROFILE_IDENTITY_FILENAME;

    /// Publishes the fixture record through the same private record authority
    /// the daemon mints with, so the reader admits the file on every platform
    /// and the negative cases below exercise parsing rather than admission.
    fn write_identity(path: &std::path::Path, body: &[u8]) {
        let temporary = path.with_extension("json.tmp");
        DatabaseAuthority::publish_record_atomically(
            &temporary,
            path,
            body,
            PROFILE_IDENTITY_RECORD_NAME,
        )
        .unwrap();
    }

    #[test]
    fn missing_record_is_absent_not_corrupt() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join(PROFILE_IDENTITY_FILENAME);
        assert!(
            read_existing_profile_identity_record(&path)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn unknown_fields_and_schema_versions_fail_closed() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join(PROFILE_IDENTITY_FILENAME);
        write_identity(
            &path,
            br#"{"schema_version":1,"brain_id":"brain.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","profile_id":"profile.bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","derived_from_path":true}"#,
        );
        assert!(
            read_existing_profile_identity_record(&path)
                .unwrap_err()
                .to_string()
                .contains("unknown field")
        );

        write_identity(
            &path,
            br#"{"schema_version":2,"brain_id":"brain.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","profile_id":"profile.bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}"#,
        );
        assert!(
            read_existing_profile_identity_record(&path)
                .unwrap_err()
                .to_string()
                .contains("unsupported profile identity record schema_version=2")
        );
    }

    #[cfg(unix)]
    #[test]
    fn insecure_or_symlinked_identity_fails_closed() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join(PROFILE_IDENTITY_FILENAME);
        write_identity(
            &path,
            br#"{"schema_version":1,"brain_id":"brain.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","profile_id":"profile.bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}"#,
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            read_existing_profile_identity_record(&path)
                .unwrap_err()
                .to_string()
                .contains("permissions 0600")
        );

        std::fs::remove_file(&path).unwrap();
        let unrelated = temporary.path().join("unrelated");
        std::fs::write(&unrelated, b"keep").unwrap();
        std::os::unix::fs::symlink(&unrelated, &path).unwrap();
        assert!(
            read_existing_profile_identity_record(&path)
                .unwrap_err()
                .to_string()
                .contains("private regular file")
        );
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"keep");
    }
}
