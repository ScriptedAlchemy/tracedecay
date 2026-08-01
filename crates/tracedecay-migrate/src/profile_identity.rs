use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracedecay_domain::{BrainId, UserProfileId};
use tracedecay_runtime_core::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::path_safety::canonicalize_existing_prefix;
use tracedecay_runtime_core::storage::PROFILE_IDENTITY_FILENAME;

const PROFILE_IDENTITY_SCHEMA_VERSION: u32 = 1;
const PROFILE_IDENTITY_RECORD_NAME: &str = "profile identity record";

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct LocalProfileIdentityRecordV1 {
    schema_version: u32,
    brain_id: BrainId,
    profile_id: UserProfileId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LocalProfileIdentityAuthorityV1 {
    profile_root: PathBuf,
    record: LocalProfileIdentityRecordV1,
}

impl LocalProfileIdentityAuthorityV1 {
    pub(crate) fn profile_root(&self) -> &Path {
        &self.profile_root
    }

    pub(crate) fn brain_id(&self) -> &BrainId {
        &self.record.brain_id
    }

    pub(crate) fn profile_id(&self) -> &UserProfileId {
        &self.record.profile_id
    }
}

pub(crate) fn load_or_create(profile_root: &Path) -> Result<LocalProfileIdentityAuthorityV1> {
    let profile_root = canonical_identity_path(profile_root)?;
    let root_existed = profile_root.exists();
    std::fs::create_dir_all(&profile_root)
        .map_err(|error| profile_identity_io("create", &profile_root, &error))?;
    if !root_existed {
        restrict_new_profile_root(&profile_root)?;
    }
    validate_private_profile_root(&profile_root)?;

    let path = profile_root.join(PROFILE_IDENTITY_FILENAME);
    if let Some(record) = read_existing_record(&path)? {
        return Ok(LocalProfileIdentityAuthorityV1 {
            profile_root,
            record,
        });
    }
    let record = new_record()?;
    let bytes = serde_json::to_vec_pretty(&record).map_err(|error| TraceDecayError::Config {
        message: format!("failed to encode {PROFILE_IDENTITY_RECORD_NAME}: {error}"),
    })?;
    let temporary = path.with_file_name(format!(
        ".{PROFILE_IDENTITY_FILENAME}.{}.tmp",
        record.profile_id.as_str()
    ));
    tracedecay_runtime_core::db::DatabaseAuthority::publish_record_atomically(
        &temporary,
        &path,
        &bytes,
        PROFILE_IDENTITY_RECORD_NAME,
    )?;
    let persisted = read_existing_record(&path)?.ok_or_else(|| TraceDecayError::Config {
        message: format!(
            "{PROFILE_IDENTITY_RECORD_NAME} '{}' disappeared after publication",
            path.display()
        ),
    })?;
    if persisted != record {
        return Err(TraceDecayError::Config {
            message: format!(
                "{PROFILE_IDENTITY_RECORD_NAME} '{}' changed during publication",
                path.display()
            ),
        });
    }
    Ok(LocalProfileIdentityAuthorityV1 {
        profile_root,
        record,
    })
}

fn read_existing_record(path: &Path) -> Result<Option<LocalProfileIdentityRecordV1>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(profile_identity_io("inspect", path, &error)),
    };
    validate_private_identity_file(path, &metadata)?;
    let encoded = tracedecay_runtime_core::db::DatabaseAuthority::read_record_strict(
        path,
        PROFILE_IDENTITY_RECORD_NAME,
    )?
    .ok_or_else(|| TraceDecayError::Config {
        message: format!(
            "{PROFILE_IDENTITY_RECORD_NAME} '{}' disappeared while being read",
            path.display()
        ),
    })?;
    let record =
        serde_json::from_str::<LocalProfileIdentityRecordV1>(&encoded).map_err(|error| {
            TraceDecayError::Config {
                message: format!(
                    "invalid {PROFILE_IDENTITY_RECORD_NAME} '{}': {error}",
                    path.display()
                ),
            }
        })?;
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
        .map_err(|error| invalid_identity(path, "profile_id", error))?;
    Ok(Some(record))
}

fn new_record() -> Result<LocalProfileIdentityRecordV1> {
    Ok(LocalProfileIdentityRecordV1 {
        schema_version: PROFILE_IDENTITY_SCHEMA_VERSION,
        brain_id: BrainId::new(random_identity("brain")?).map_err(|error| {
            TraceDecayError::Config {
                message: format!("failed to construct local brain identity: {error}"),
            }
        })?,
        profile_id: UserProfileId::new(random_identity("profile")?).map_err(|error| {
            TraceDecayError::Config {
                message: format!("failed to construct local user profile identity: {error}"),
            }
        })?,
    })
}

fn random_identity(prefix: &str) -> Result<String> {
    let mut bytes = [0_u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|error| TraceDecayError::Config {
        message: format!("failed to generate local {prefix} identity: {error}"),
    })?;
    Ok(format!("{prefix}.{}", hex::encode(bytes)))
}

/// Absolutizes `path` and canonicalizes through its deepest existing
/// ancestor.
///
/// Known divergence, deliberately preserved: the daemon's identically-named
/// resolver additionally applies
/// [`tracedecay_runtime_core::path_safety::collapse_relative_components`] to
/// the result. This one must not — the identity strings it produces are
/// already written into migrated profile records, and collapsing `.`/`..`
/// here would rewrite them. Only the canonicalization algorithm is shared.
fn canonical_identity_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| profile_identity_io("resolve", path, &error))?
            .join(path)
    };
    canonicalize_existing_prefix(&absolute).ok_or_else(|| TraceDecayError::Config {
        message: format!("failed to resolve identity path '{}'", path.display()),
    })
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

#[cfg(unix)]
fn restrict_new_profile_root(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| profile_identity_io("restrict", path, &error))
}

#[cfg(not(unix))]
fn restrict_new_profile_root(_path: &Path) -> Result<()> {
    Ok(())
}

fn validate_private_profile_root(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| profile_identity_io("inspect", path, &error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(TraceDecayError::Config {
            message: format!(
                "profile identity root '{}' must be a private regular directory",
                path.display()
            ),
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o777 != 0o700 {
            return Err(TraceDecayError::Config {
                message: format!(
                    "profile identity root '{}' must have permissions 0700",
                    path.display()
                ),
            });
        }
    }
    Ok(())
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
