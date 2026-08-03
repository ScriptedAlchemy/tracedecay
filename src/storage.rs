//! Compatibility façade for runtime storage layout.

pub use tracedecay_runtime_core::storage::{
    ActiveProjectContext, BRANCH_META_FILENAME, BRANCH_META_QUARANTINE_PREFIX, ENROLLMENT_FILENAME,
    EnrollmentMarker, GraphScopeId, IDENTITY_CUTOVER_BACKUP_MANIFEST_FILENAME, PrivateStoreIo,
    ProjectIdentity, ProjectPath, QueryTarget, REPOSITORY_IDENTITY_FILENAME,
    REPOSITORY_IDENTITY_SCHEMA_VERSION, RepositoryIdentityMarker, SESSIONS_DB_FILENAME,
    STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION, StorageMode, StoreArtifactPath,
    StoreKind, StoreLayout, StoreManifest, default_profile_project_id, default_profile_root,
    default_profile_sharded_layout, enrollment_marker_path, has_enrollment_marker,
    profile_sharded_data_root, profile_sharded_layout, read_enrollment_marker,
    read_repository_identity_marker,
    read_store_manifest, remove_enrollment_marker, repository_identity_path, resolve_layout,
    resolve_layout_for_current_profile, resolve_lcm_payload_root, resolve_project_session_db_path,
    resolve_response_handle_root, write_enrollment_marker, write_repository_identity_marker,
    write_store_manifest, write_store_manifest_to_path,
};
pub(crate) use tracedecay_runtime_core::storage::{
    acquire_sidecar_lock_blocking, matching_legacy_profile_layouts, resolve_persisted_layout,
    retire_identity_cutover_manifest, try_acquire_sidecar_lock,
};
#[cfg(test)]
pub(crate) use tracedecay_runtime_core::storage::has_sqlite_database_header;
