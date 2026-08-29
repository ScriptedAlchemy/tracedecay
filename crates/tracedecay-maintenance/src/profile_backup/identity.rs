//! Identity binding for complete profile backups.
//!
//! A backup manifest pins the exact brain/profile identity it was produced
//! from and the durable identity of every project store it contains, so a
//! restore can only proceed against material whose identity inventory still
//! matches its content.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{BrainId, UserProfileId};
use tracedecay_runtime_core::db::DatabaseAuthority;
use tracedecay_runtime_core::storage::PROFILE_IDENTITY_FILENAME;

use super::{ProfileBackupEntry, ProfileBackupError, checked_join};

const PROFILE_IDENTITY_SCHEMA_VERSION: u32 = 1;
const PROFILE_IDENTITY_RECORD_NAME: &str = "profile identity record";

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ProfileIdentityRecordV1 {
    schema_version: u32,
    brain_id: BrainId,
    profile_id: UserProfileId,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileBackupProjectIdentity {
    pub project_id: String,
    pub project_root: std::path::PathBuf,
    pub store_relpath: String,
}

/// Reads the persisted profile-identity record through runtime-core file
/// primitives (never minting a new record). The daemon remains the write
/// authority that publishes this file.
pub(super) fn read_required_profile_identity(
    profile_root: &Path,
    corrupt_material: bool,
) -> Result<(String, String), ProfileBackupError> {
    read_persisted_profile_identity(profile_root).map_err(|error| {
        if corrupt_material {
            ProfileBackupError::corrupt(error)
        } else {
            ProfileBackupError::invalid(error)
        }
    })
}

fn read_persisted_profile_identity(profile_root: &Path) -> Result<(String, String), String> {
    let path = profile_root.join(PROFILE_IDENTITY_FILENAME);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "required {PROFILE_IDENTITY_RECORD_NAME} '{}' is missing",
                path.display()
            ));
        }
        Err(error) => {
            return Err(format!(
                "failed to inspect {PROFILE_IDENTITY_RECORD_NAME} '{}': {error}",
                path.display()
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "{PROFILE_IDENTITY_RECORD_NAME} '{}' must be a private regular file",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return Err(format!(
                "{PROFILE_IDENTITY_RECORD_NAME} '{}' must have permissions 0600",
                path.display()
            ));
        }
    }
    let encoded = DatabaseAuthority::read_record_strict(&path, PROFILE_IDENTITY_RECORD_NAME)
        .map_err(|error| {
            format!(
                "profile identity of '{}' is not the exact final shape: {error}",
                profile_root.display()
            )
        })?
        .ok_or_else(|| {
            format!(
                "required {PROFILE_IDENTITY_RECORD_NAME} '{}' is missing",
                path.display()
            )
        })?;
    let record = serde_json::from_str::<ProfileIdentityRecordV1>(&encoded).map_err(|error| {
        format!(
            "invalid {PROFILE_IDENTITY_RECORD_NAME} '{}': {error}",
            path.display()
        )
    })?;
    if record.schema_version != PROFILE_IDENTITY_SCHEMA_VERSION {
        return Err(format!(
            "unsupported {PROFILE_IDENTITY_RECORD_NAME} schema_version={} in '{}'",
            record.schema_version,
            path.display()
        ));
    }
    record
        .brain_id
        .validate()
        .map_err(|error| format!("invalid brain_id in {PROFILE_IDENTITY_RECORD_NAME}: {error}"))?;
    record.profile_id.validate().map_err(|error| {
        format!("invalid profile_id in {PROFILE_IDENTITY_RECORD_NAME}: {error}")
    })?;
    Ok((
        record.brain_id.as_str().to_owned(),
        record.profile_id.as_str().to_owned(),
    ))
}

/// Collects the durable identity of every project store named by `entries`,
/// validating each store manifest against its final enrollment shape.
pub(super) fn collect_project_identities(
    profile_root: &Path,
    entries: &[ProfileBackupEntry],
) -> Result<Vec<ProfileBackupProjectIdentity>, ProfileBackupError> {
    let mut projects = Vec::new();
    for entry in entries.iter().filter(|entry| entry.present) {
        let Some(project_id) = restored_store_manifest_project_id(&entry.logical_path) else {
            continue;
        };
        let manifest_path = checked_join(profile_root, &entry.logical_path)?;
        let manifest = tracedecay_runtime_core::storage::read_store_manifest(&manifest_path)
            .map_err(|error| {
                ProfileBackupError::corrupt(format!(
                    "read project store manifest '{}': {error}",
                    manifest_path.display()
                ))
            })?;
        if manifest.project_id.as_deref() != Some(project_id)
            || manifest.storage_mode
                != tracedecay_runtime_core::storage::StorageMode::ProfileSharded
        {
            return Err(ProfileBackupError::corrupt(format!(
                "project store manifest '{}' does not match its final enrollment identity",
                manifest_path.display()
            )));
        }
        projects.push(ProfileBackupProjectIdentity {
            project_id: project_id.to_owned(),
            project_root: manifest.project_root,
            store_relpath: format!("projects/{project_id}"),
        });
    }
    projects.sort_by(|left, right| left.project_id.cmp(&right.project_id));
    Ok(projects)
}

pub(super) fn restored_store_manifest_project_id(logical_path: &str) -> Option<&str> {
    let mut components = logical_path.split('/');
    match (
        components.next(),
        components.next(),
        components.next(),
        components.next(),
    ) {
        (
            Some("projects"),
            Some(project_id),
            Some(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
            None,
        ) if !project_id.is_empty() => Some(project_id),
        _ => None,
    }
}

/// Validates the manifest's project-identity inventory shape and its
/// consistency with the entry inventory.
pub(super) fn validate_project_identities(
    projects: &[ProfileBackupProjectIdentity],
    entries: &[ProfileBackupEntry],
) -> Result<(), ProfileBackupError> {
    let mut previous_project = None;
    for project in projects {
        if project.project_id.is_empty()
            || project.store_relpath.is_empty()
            || project.project_root.as_os_str().is_empty()
            || !project.project_root.is_absolute()
            || Path::new(&project.store_relpath)
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
            || project.store_relpath != format!("projects/{}", project.project_id)
            || previous_project.is_some_and(|value: &str| value >= project.project_id.as_str())
        {
            return Err(ProfileBackupError::corrupt(
                "invalid complete-profile backup project identity",
            ));
        }
        let store_manifest = format!(
            "{}/{}",
            project.store_relpath,
            tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME
        );
        if !entries
            .iter()
            .any(|entry| entry.present && entry.logical_path == store_manifest)
        {
            return Err(ProfileBackupError::corrupt(format!(
                "backup project '{}' is missing required {}",
                project.project_id,
                tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME
            )));
        }
        previous_project = Some(project.project_id.as_str());
    }
    Ok(())
}
