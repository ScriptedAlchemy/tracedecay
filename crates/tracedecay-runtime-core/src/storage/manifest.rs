use std::fs;
use std::path::{Path, PathBuf};

use crate::errors::{Result, TraceDecayError};

use super::{
    PrivateStoreIo, ProfileShardValidationError, SESSIONS_DB_FILENAME, STORE_MANIFEST_FILENAME,
    STORE_MANIFEST_SCHEMA_VERSION, StorageMode, StoreKind, StoreLayout, StoreManifest,
    ValidatedProfileShard, has_sqlite_database_header, profile_sharded_data_root,
};

pub fn write_store_manifest(layout: &StoreLayout) -> Result<StoreManifest> {
    let path = layout
        .manifest_path
        .as_ref()
        .ok_or_else(|| TraceDecayError::Config {
            message: format!(
                "store manifest path is not defined for {:?} storage",
                layout.storage_mode
            ),
        })?;
    let manifest = StoreManifest::from_layout(layout);
    write_store_manifest_payload(path, &manifest)?;
    Ok(manifest)
}

/// Writes `manifest` to `path` without rebuilding it from a [`StoreLayout`].
pub fn write_store_manifest_to_path(path: &Path, manifest: &StoreManifest) -> Result<()> {
    write_store_manifest_payload(path, manifest)
}

fn write_store_manifest_payload(path: &Path, manifest: &StoreManifest) -> Result<()> {
    let text = serde_json::to_string_pretty(manifest).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to serialize store manifest '{}': {e}",
            path.display()
        ),
    })?;
    let temp_path = path.with_extension("json.tmp");
    PrivateStoreIo::write_file_atomically(path, &temp_path, text.as_bytes()).map_err(|e| {
        TraceDecayError::Config {
            message: format!("failed to write store manifest '{}': {e}", path.display()),
        }
    })
}

pub fn read_store_manifest(path: &Path) -> Result<StoreManifest> {
    let text = fs::read_to_string(path).map_err(|e| TraceDecayError::Config {
        message: format!("failed to read store manifest '{}': {e}", path.display()),
    })?;
    serde_json::from_str(&text).map_err(|e| TraceDecayError::Config {
        message: format!("failed to parse store manifest '{}': {e}", path.display()),
    })
}

impl ValidatedProfileShard {
    /// Resolves an already-registered profile shard without creating artifacts
    /// or letting the database library touch an invalid database family.
    pub fn resolve_existing(
        profile_root: &Path,
        project_id: &str,
    ) -> std::result::Result<Self, ProfileShardValidationError> {
        let expected_relpath_text = format!("projects/{project_id}");
        let expected_relpath = PathBuf::from(&expected_relpath_text);
        let canonical_profile_root =
            profile_root
                .canonicalize()
                .map_err(|_| ProfileShardValidationError::Unavailable {
                    path: profile_root.to_path_buf(),
                })?;

        let store_root = profile_sharded_data_root(profile_root, project_id);
        require_regular_artifact(&store_root, RequiredArtifactKind::Directory)?;
        let canonical_store_root =
            store_root
                .canonicalize()
                .map_err(|_| ProfileShardValidationError::Unavailable {
                    path: store_root.clone(),
                })?;
        if canonical_store_root != canonical_profile_root.join(&expected_relpath) {
            return Err(ProfileShardValidationError::NonCanonical {
                reason: format!(
                    "store root resolves outside '{}'",
                    expected_relpath.display()
                ),
            });
        }

        let manifest_path = store_root.join(STORE_MANIFEST_FILENAME);
        require_regular_artifact(&manifest_path, RequiredArtifactKind::File)?;
        validate_profile_shard_manifest(project_id, &canonical_store_root, &manifest_path)?;

        let sessions_db_path = store_root.join(SESSIONS_DB_FILENAME);
        require_regular_artifact(&sessions_db_path, RequiredArtifactKind::File)?;
        match has_sqlite_database_header(&sessions_db_path) {
            Ok(true) => {}
            Ok(false) => {
                return Err(ProfileShardValidationError::NonCanonical {
                    reason: format!("'{}' is not a SQLite database", sessions_db_path.display()),
                });
            }
            Err(_) => {
                return Err(ProfileShardValidationError::Unavailable {
                    path: sessions_db_path,
                });
            }
        }
        let sessions_db_path = sessions_db_path.canonicalize().map_err(|_| {
            ProfileShardValidationError::Unavailable {
                path: sessions_db_path.clone(),
            }
        })?;

        Ok(Self {
            store_root: canonical_store_root,
            sessions_db_path,
        })
    }

    pub fn store_root(&self) -> &Path {
        &self.store_root
    }

    pub fn sessions_db_path(&self) -> &Path {
        &self.sessions_db_path
    }
}

#[derive(Clone, Copy)]
enum RequiredArtifactKind {
    Directory,
    File,
}

impl RequiredArtifactKind {
    fn description(self) -> &'static str {
        match self {
            Self::Directory => "directory",
            Self::File => "file",
        }
    }

    fn matches(self, file_type: fs::FileType) -> bool {
        match self {
            Self::Directory => file_type.is_dir(),
            Self::File => file_type.is_file(),
        }
    }
}

fn require_regular_artifact(
    path: &Path,
    kind: RequiredArtifactKind,
) -> std::result::Result<(), ProfileShardValidationError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| ProfileShardValidationError::Unavailable {
            path: path.to_path_buf(),
        })?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() || !kind.matches(file_type) {
        return Err(ProfileShardValidationError::NonCanonical {
            reason: format!(
                "'{}' is not a regular {}",
                path.display(),
                kind.description()
            ),
        });
    }
    Ok(())
}

fn validate_profile_shard_manifest(
    project_id: &str,
    store_root: &Path,
    manifest_path: &Path,
) -> std::result::Result<(), ProfileShardValidationError> {
    let manifest = read_store_manifest(manifest_path).map_err(|error| {
        ProfileShardValidationError::NonCanonical {
            reason: format!("store manifest is invalid: {error}"),
        }
    })?;
    let invalid = |reason: String| ProfileShardValidationError::NonCanonical { reason };
    if manifest.schema_version != STORE_MANIFEST_SCHEMA_VERSION {
        return Err(invalid(format!(
            "manifest schema must be {STORE_MANIFEST_SCHEMA_VERSION}, found {}",
            manifest.schema_version
        )));
    }
    if manifest.project_id.as_deref() != Some(project_id) {
        return Err(invalid(
            "manifest project id does not match the registry".to_string(),
        ));
    }
    if manifest.store_kind != StoreKind::CodeProject {
        return Err(invalid(
            "manifest store kind must be 'code_project'".to_string(),
        ));
    }
    if manifest.storage_mode != StorageMode::ProfileSharded {
        return Err(invalid(
            "manifest storage mode must be 'profile_sharded'".to_string(),
        ));
    }
    if manifest.sessions_db_relpath != Path::new(SESSIONS_DB_FILENAME) {
        return Err(invalid(format!(
            "manifest sessions database path must be '{SESSIONS_DB_FILENAME}'"
        )));
    }
    let manifest_data_root = manifest
        .data_root
        .canonicalize()
        .map_err(|_| invalid("manifest data root is unavailable".to_string()))?;
    if manifest_data_root != store_root {
        return Err(invalid(
            "manifest data root does not match the registered store".to_string(),
        ));
    }
    Ok(())
}
