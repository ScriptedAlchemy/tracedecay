use std::path::{Path, PathBuf};

use tracedecay_application::ProfileIdentityReadPort;
use tracedecay_domain::{BrainId, UserProfileId};

use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::storage::{
    PROFILE_IDENTITY_FILENAME, PROFILE_IDENTITY_RECORD_NAME, PROFILE_IDENTITY_SCHEMA_VERSION,
    ProfileIdentityRecordV1, read_existing_profile_identity_record,
};

use crate::authority::canonical_identity_path;

/// Durable random identity for one local `TraceDecay` profile root.
///
/// The profile root is retained with the decoded record so callers cannot
/// accidentally pair its identities with another physical profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalProfileIdentityAuthorityV1 {
    profile_root: PathBuf,
    record: ProfileIdentityRecordV1,
}

impl LocalProfileIdentityAuthorityV1 {
    pub fn profile_root(&self) -> &Path {
        &self.profile_root
    }

    pub fn brain_id(&self) -> &BrainId {
        &self.record.brain_id
    }

    pub fn profile_id(&self) -> &UserProfileId {
        &self.record.profile_id
    }
}

impl ProfileIdentityReadPort for LocalProfileIdentityAuthorityV1 {
    fn profile_root(&self) -> &Path {
        LocalProfileIdentityAuthorityV1::profile_root(self)
    }

    fn brain_id(&self) -> &BrainId {
        LocalProfileIdentityAuthorityV1::brain_id(self)
    }

    fn profile_id(&self) -> &UserProfileId {
        LocalProfileIdentityAuthorityV1::profile_id(self)
    }
}

pub fn load_or_create(profile_root: &Path) -> Result<LocalProfileIdentityAuthorityV1> {
    load_or_create_pinned(profile_root, None)
}

pub fn load_existing(profile_root: &Path) -> Result<LocalProfileIdentityAuthorityV1> {
    let profile_root = canonical_identity_path(profile_root)?;
    validate_private_profile_root(&profile_root)?;
    let path = profile_root.join(PROFILE_IDENTITY_FILENAME);
    let record = read_existing_profile_identity_record(&path)?.ok_or_else(|| {
        TraceDecayError::Config {
            message: format!(
                "ResetRequired: {PROFILE_IDENTITY_RECORD_NAME} '{}' is missing from an existing profile",
                path.display()
            ),
        }
    })?;
    Ok(LocalProfileIdentityAuthorityV1 {
        profile_root,
        record,
    })
}

#[hotpath::measure(label = "daemon.profile_identity.load_or_create")]
pub(crate) fn load_or_create_pinned(
    profile_root: &Path,
    expected: Option<(&BrainId, &UserProfileId)>,
) -> Result<LocalProfileIdentityAuthorityV1> {
    let profile_root = canonical_identity_path(profile_root)?;
    if expected.is_some() {
        validate_private_profile_root(&profile_root)?;
        let path = profile_root.join(PROFILE_IDENTITY_FILENAME);
        let record = read_existing_profile_identity_record(&path)?.ok_or_else(|| {
            TraceDecayError::Config {
                message: format!(
                    "{PROFILE_IDENTITY_RECORD_NAME} '{}' is missing after its identity was pinned",
                    path.display()
                ),
            }
        })?;
        validate_expected_identity(&path, &record, expected)?;
        return Ok(LocalProfileIdentityAuthorityV1 {
            profile_root,
            record,
        });
    }

    let root_existed = profile_root.exists();
    std::fs::create_dir_all(&profile_root)
        .map_err(|error| profile_identity_io("create", &profile_root, &error))?;
    if !root_existed {
        restrict_new_profile_root(&profile_root)?;
    }
    validate_private_profile_root(&profile_root)?;

    let path = profile_root.join(PROFILE_IDENTITY_FILENAME);
    if let Some(record) = read_existing_profile_identity_record(&path)? {
        validate_expected_identity(&path, &record, expected)?;
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
    let persisted =
        read_existing_profile_identity_record(&path)?.ok_or_else(|| TraceDecayError::Config {
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

fn validate_expected_identity(
    path: &Path,
    record: &ProfileIdentityRecordV1,
    expected: Option<(&BrainId, &UserProfileId)>,
) -> Result<()> {
    let Some((brain_id, profile_id)) = expected else {
        return Ok(());
    };
    if &record.brain_id != brain_id || &record.profile_id != profile_id {
        return Err(TraceDecayError::Config {
            message: format!(
                "{PROFILE_IDENTITY_RECORD_NAME} '{}' does not match the identity pinned by the daemon authority",
                path.display()
            ),
        });
    }
    Ok(())
}

fn new_record() -> Result<ProfileIdentityRecordV1> {
    Ok(ProfileIdentityRecordV1 {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_random_private_and_stable_across_reload() {
        let temporary = tempfile::tempdir().unwrap();
        let profile_root = temporary.path().join("profile");

        let first = load_or_create(&profile_root).unwrap();
        let second = load_or_create(&profile_root).unwrap();

        assert_eq!(first, second);
        assert!(first.brain_id().as_str().starts_with("brain."));
        assert!(first.profile_id().as_str().starts_with("profile."));
        assert_eq!(first.brain_id().as_str().len(), "brain.".len() + 32);
        assert_eq!(first.profile_id().as_str().len(), "profile.".len() + 32);
        assert_eq!(
            std::fs::read_dir(&profile_root).unwrap().count(),
            1,
            "atomic publication left a temporary file"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                std::fs::metadata(&profile_root)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(profile_root.join(PROFILE_IDENTITY_FILENAME))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn corrupt_identity_fails_closed_without_regeneration() {
        let temporary = tempfile::tempdir().unwrap();
        let profile_root = temporary.path().join("profile");
        let original = load_or_create(&profile_root).unwrap();
        let path = profile_root.join(PROFILE_IDENTITY_FILENAME);
        std::fs::write(&path, b"{\"schema_version\":1,\"brain_id\":").unwrap();
        let corrupt = std::fs::read(&path).unwrap();

        let error = load_or_create(&profile_root).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("invalid profile identity record")
        );
        assert_eq!(std::fs::read(&path).unwrap(), corrupt);
        assert_eq!(
            original.profile_root(),
            profile_root.canonicalize().unwrap()
        );
    }

    #[test]
    fn pinned_identity_cannot_be_regenerated_or_replaced() {
        let temporary = tempfile::tempdir().unwrap();
        let profile_root = temporary.path().join("profile");
        let original = load_or_create(&profile_root).unwrap();
        let path = profile_root.join(PROFILE_IDENTITY_FILENAME);
        std::fs::remove_file(&path).unwrap();

        let missing = load_or_create_pinned(
            &profile_root,
            Some((original.brain_id(), original.profile_id())),
        )
        .unwrap_err();
        assert!(
            missing
                .to_string()
                .contains("missing after its identity was pinned")
        );
        assert!(!path.exists());

        let replacement = load_or_create(&profile_root).unwrap();
        assert_ne!(replacement, original);
        let mismatch = load_or_create_pinned(
            &profile_root,
            Some((original.brain_id(), original.profile_id())),
        )
        .unwrap_err();
        assert!(
            mismatch
                .to_string()
                .contains("does not match the identity pinned")
        );
    }

    #[test]
    fn pinned_identity_recheck_does_not_recreate_a_missing_profile_root() {
        let temporary = tempfile::tempdir().unwrap();
        let profile_root = temporary.path().join("profile");
        let original = load_or_create(&profile_root).unwrap();
        std::fs::remove_dir_all(&profile_root).unwrap();

        let error = load_or_create_pinned(
            &profile_root,
            Some((original.brain_id(), original.profile_id())),
        )
        .unwrap_err();

        assert!(error.to_string().contains("failed to inspect"));
        assert!(!profile_root.exists());
    }

    #[test]
    fn unknown_fields_and_schema_versions_are_rejected() {
        let temporary = tempfile::tempdir().unwrap();
        let profile_root = temporary.path().join("profile");
        let identity = load_or_create(&profile_root).unwrap();
        let path = profile_root.join(PROFILE_IDENTITY_FILENAME);
        let unknown = serde_json::json!({
            "schema_version": 1,
            "brain_id": identity.brain_id(),
            "profile_id": identity.profile_id(),
            "derived_from_path": true,
        });
        std::fs::write(&path, serde_json::to_vec(&unknown).unwrap()).unwrap();
        assert!(
            load_or_create(&profile_root)
                .unwrap_err()
                .to_string()
                .contains("unknown field")
        );

        let unsupported = serde_json::json!({
            "schema_version": 2,
            "brain_id": identity.brain_id(),
            "profile_id": identity.profile_id(),
        });
        std::fs::write(&path, serde_json::to_vec(&unsupported).unwrap()).unwrap();
        assert!(
            load_or_create(&profile_root)
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
        let profile_root = temporary.path().join("profile");
        load_or_create(&profile_root).unwrap();
        let path = profile_root.join(PROFILE_IDENTITY_FILENAME);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            load_or_create(&profile_root)
                .unwrap_err()
                .to_string()
                .contains("permissions 0600")
        );

        std::fs::remove_file(&path).unwrap();
        let unrelated = temporary.path().join("unrelated");
        std::fs::write(&unrelated, b"keep").unwrap();
        std::os::unix::fs::symlink(&unrelated, &path).unwrap();
        assert!(
            load_or_create(&profile_root)
                .unwrap_err()
                .to_string()
                .contains("private regular file")
        );
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"keep");
    }
}
