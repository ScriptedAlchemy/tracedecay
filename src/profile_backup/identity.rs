//! Identity binding for complete profile backups.
//!
//! A backup manifest pins the exact brain/profile identity it was produced
//! from and the durable identity of every project store it contains, so a
//! restore can only proceed against material whose identity inventory still
//! matches its content.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{ProfileBackupEntry, ProfileBackupError, checked_join};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileBackupProjectIdentity {
    pub project_id: String,
    pub project_root: std::path::PathBuf,
    pub store_relpath: String,
}

/// Reads the profile identity the backup binds to, through the canonical
/// profile-identity authority (never minting a new record).
pub(super) fn read_required_profile_identity(
    profile_root: &Path,
    corrupt_material: bool,
) -> Result<(String, String), ProfileBackupError> {
    crate::daemon::profile_identity::read_required(profile_root)
        .map(|(brain_id, profile_id)| {
            (brain_id.as_str().to_owned(), profile_id.as_str().to_owned())
        })
        .map_err(|error| {
            let message = format!(
                "profile identity of '{}' is not the exact final shape: {error}",
                profile_root.display()
            );
            if corrupt_material {
                ProfileBackupError::corrupt(message)
            } else {
                ProfileBackupError::invalid(message)
            }
        })
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
