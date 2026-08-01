//! The migration manifest: what a migration intends to do, and the crash
//! checkpoint that lets an interrupted attempt resume without guessing.
//!
//! A manifest is the migration's durable plan. Every artifact advances through
//! [`ArtifactState`] one step at a time, and each step is persisted before the
//! next begins, so a crash leaves a manifest that says exactly how far the
//! attempt got. The state machine is forward-only: an artifact never moves
//! backwards, which is what makes recovery a resume rather than a rollback.
//!
//! This module owns no storage authority. It does not open a database, hold a
//! lifecycle lease, or choose file permissions — writing the checkpoint goes
//! through the caller's [`CheckpointWriter`], so the root crate keeps ownership
//! of owner-private file semantics and atomic publication.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::inventory::MigrationInventory;

mod runtime;

pub use runtime::*;

pub const MIGRATION_MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationManifest {
    pub migration_id: String,
    pub schema_version: u32,
    pub tracedecay_version: String,
    pub created_at_unix: i64,
    pub confirmation_token: String,
    pub command_args: Vec<String>,
    pub env_overrides: Vec<String>,
    pub source: MigrationEndpoint,
    pub destination: MigrationDestination,
    pub validation_summaries: Vec<String>,
    pub protocol: MigrationProtocol,
    pub inventory: MigrationInventory,
    pub artifacts: Vec<MigrationArtifact>,
    #[serde(default)]
    pub backup_artifacts: Vec<MigrationArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationProtocol {
    pub manifest_path: PathBuf,
    pub temp_manifest_path: PathBuf,
    pub lock_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactState {
    Planned,
    Locked,
    Copied,
    Verified,
    Applied,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationArtifact {
    pub kind: String,
    pub source_path: PathBuf,
    pub target_path: Option<PathBuf>,
    pub state: ArtifactState,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationEndpoint {
    pub project_root: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationDestination {
    pub profile_root: Option<PathBuf>,
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreArtifactPath {
    pub root: PathBuf,
    pub relative_path: PathBuf,
    pub absolute_path: PathBuf,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreArtifactPathValidationError {
    PathTraversal,
    NonNormalComponent,
    NulByte,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationPlanOptions {
    pub manifest_path: PathBuf,
    pub migration_id: String,
    pub tracedecay_version: String,
    pub created_at_unix: i64,
    pub confirmation_token: String,
    pub target_profile_root: PathBuf,
    pub project_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationApplyReport {
    pub migration_id: String,
    pub project_root: PathBuf,
    pub profile_root: PathBuf,
    pub project_id: String,
    pub artifact_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationRollbackReport {
    pub migration_id: String,
    pub artifact_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationExportReport {
    pub project_id: String,
    pub source_profile_root: PathBuf,
    pub source_data_root: PathBuf,
    pub target_dir: PathBuf,
    pub artifact_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationCleanupSourcesReport {
    pub migration_id: String,
    pub removed_artifacts: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationRollbackState {
    NotApplied,
    PartialApply,
    CutoverIncomplete,
    DivergentTargets,
    AppliedReady,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactStateTransitionError {
    from: ArtifactState,
    to: ArtifactState,
}

/// Owner-private filesystem writes a migration checkpoint depends on.
///
/// The checkpoint must be durable and must never widen the store's
/// permissions, but deciding *how* — the mode bits, the temp-file dance, the
/// atomic rename — belongs to whoever owns the store. This package states the
/// requirement and lets the root crate satisfy it.
pub trait CheckpointWriter {
    /// Writes `contents` to `path` with owner-only visibility.
    fn write_file(&self, path: &Path, contents: &[u8]) -> io::Result<()>;

    /// Publishes `contents` at `path` atomically, staging through `temp_path`
    /// so a crash never leaves a partially written checkpoint readable.
    fn write_file_atomically(
        &self,
        path: &Path,
        temp_path: &Path,
        contents: &[u8],
    ) -> io::Result<()>;
}

impl MigrationManifest {
    pub fn new(
        migration_id: impl Into<String>,
        tracedecay_version: impl Into<String>,
        created_at_unix: i64,
        confirmation_token: impl Into<String>,
        protocol: MigrationProtocol,
        inventory: MigrationInventory,
    ) -> Self {
        let migration_id = migration_id.into();
        let confirmation_token = confirmation_token.into();
        Self {
            migration_id,
            schema_version: MIGRATION_MANIFEST_SCHEMA_VERSION,
            tracedecay_version: tracedecay_version.into(),
            created_at_unix,
            confirmation_token,
            command_args: Vec::new(),
            env_overrides: Vec::new(),
            source: MigrationEndpoint::default(),
            destination: MigrationDestination::default(),
            validation_summaries: Vec::new(),
            protocol,
            inventory,
            artifacts: Vec::new(),
            backup_artifacts: Vec::new(),
        }
    }
}

/// Persists the manifest as the migration's crash checkpoint.
///
/// The lock file is written first and removed last so a concurrent reader can
/// tell an in-progress checkpoint from a settled one, and the manifest itself
/// is published atomically. A missing confirmation token, an unsafe
/// `migration_id`, or protocol paths that were not derived from the manifest
/// path are refused before anything is written.
pub fn save_manifest_with_writer(
    writer: &dyn CheckpointWriter,
    manifest: &MigrationManifest,
) -> io::Result<()> {
    if manifest.confirmation_token.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "confirmation_token is required before saving a migration manifest",
        ));
    }
    validate_migration_id(&manifest.migration_id).map_err(|message| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "invalid migration_id '{}': {message}",
                manifest.migration_id
            ),
        )
    })?;
    let protocol = &manifest.protocol;
    validate_protocol_paths(protocol, &manifest.migration_id)?;
    let bytes = serde_json::to_vec_pretty(manifest).map_err(io::Error::other)?;
    let mut lock_written = false;
    let result = (|| {
        writer.write_file(&protocol.lock_path, manifest.migration_id.as_bytes())?;
        lock_written = true;
        writer.write_file_atomically(
            &protocol.manifest_path,
            &protocol.temp_manifest_path,
            &bytes,
        )
    })();
    if lock_written {
        let cleanup_result = fs::remove_file(&protocol.lock_path);
        if result.is_ok()
            && let Err(err) = cleanup_result
            && err.kind() != io::ErrorKind::NotFound
        {
            return Err(err);
        }
    }
    result
}

pub fn load_manifest(path: impl AsRef<Path>) -> io::Result<MigrationManifest> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

fn validate_protocol_paths(protocol: &MigrationProtocol, migration_id: &str) -> io::Result<()> {
    let expected = MigrationProtocol::for_manifest(&protocol.manifest_path, migration_id);
    if protocol.temp_manifest_path != expected.temp_manifest_path
        || protocol.lock_path != expected.lock_path
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "migration manifest protocol paths must be derived from manifest_path and migration_id",
        ));
    }
    Ok(())
}

/// A `migration_id` becomes a path segment in backup and scratch layouts, so it
/// must stay a single safe segment.
pub fn validate_migration_id(migration_id: &str) -> Result<(), &'static str> {
    if migration_id.is_empty() {
        return Err("migration_id must not be empty");
    }
    if migration_id.contains('/') || migration_id.contains('\\') || migration_id.contains("..") {
        return Err("migration_id must be a single safe path segment");
    }
    if !migration_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    {
        return Err("migration_id contains unsupported characters");
    }
    Ok(())
}

impl MigrationProtocol {
    pub fn for_manifest(manifest_path: impl AsRef<Path>, migration_id: &str) -> Self {
        let manifest_path = manifest_path.as_ref().to_path_buf();
        let file_name = manifest_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("migration-manifest.json");
        let parent = manifest_path.parent().unwrap_or_else(|| Path::new(""));
        Self {
            temp_manifest_path: parent.join(format!(".{file_name}.{migration_id}.tmp")),
            lock_path: parent.join(format!("{file_name}.lock")),
            manifest_path,
        }
    }
}

impl MigrationArtifact {
    pub fn new(
        kind: impl Into<String>,
        source_path: PathBuf,
        target_path: Option<PathBuf>,
    ) -> Self {
        Self {
            kind: kind.into(),
            source_path,
            target_path,
            state: ArtifactState::Planned,
        }
    }

    pub fn transition_to(
        &mut self,
        next: ArtifactState,
    ) -> Result<(), ArtifactStateTransitionError> {
        if self.state.can_transition_to(&next) {
            self.state = next;
            Ok(())
        } else {
            Err(ArtifactStateTransitionError {
                from: self.state.clone(),
                to: next,
            })
        }
    }
}

impl StoreArtifactPath {
    pub fn from_relative(
        root: &Path,
        relative_path: &Path,
        size_bytes: u64,
    ) -> Result<Self, StoreArtifactPathValidationError> {
        validate_artifact_relpath(relative_path)?;
        let absolute_path = root.join(relative_path);
        reject_symlink_components(root, relative_path)?;
        Ok(Self {
            root: root.to_path_buf(),
            relative_path: relative_path.to_path_buf(),
            absolute_path,
            size_bytes,
        })
    }
}

impl ArtifactState {
    /// The forward-only checkpoint ladder. Every state may fail, but no state
    /// may move backwards or skip a rung, which is what lets an interrupted
    /// migration resume from its recorded position instead of restarting.
    fn can_transition_to(&self, next: &Self) -> bool {
        matches!(
            (self, next),
            (Self::Planned, Self::Locked)
                | (Self::Locked, Self::Copied)
                | (Self::Copied, Self::Verified)
                | (Self::Verified, Self::Applied)
                | (
                    Self::Planned | Self::Locked | Self::Copied | Self::Verified,
                    Self::Failed
                )
        )
    }
}

impl fmt::Display for ArtifactStateTransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid migration artifact state transition from {:?} to {:?}",
            self.from, self.to
        )
    }
}

impl std::error::Error for ArtifactStateTransitionError {}

fn validate_artifact_relpath(relative_path: &Path) -> Result<(), StoreArtifactPathValidationError> {
    if relative_path.to_string_lossy().contains('\0') {
        return Err(StoreArtifactPathValidationError::NulByte);
    }
    if relative_path.is_absolute() {
        return Err(StoreArtifactPathValidationError::PathTraversal);
    }
    for component in relative_path.components() {
        match component {
            Component::Normal(_) => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(StoreArtifactPathValidationError::PathTraversal);
            }
            Component::CurDir => return Err(StoreArtifactPathValidationError::NonNormalComponent),
        }
    }
    Ok(())
}

fn reject_symlink_components(
    root: &Path,
    relative_path: &Path,
) -> Result<(), StoreArtifactPathValidationError> {
    let mut current = root.to_path_buf();
    for component in relative_path.components() {
        current.push(component.as_os_str());
        if current
            .symlink_metadata()
            .is_ok_and(|meta| meta.file_type().is_symlink())
        {
            return Err(StoreArtifactPathValidationError::Symlink);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeSet;

    use tracedecay_store::{
        BrainId, CodeShardScopeV1, LocatorDigest, ProjectId, RepositoryId, StoreAuthorityEpochV1,
        StoreIncarnationV1, StoreRuntimeBindingV1, StoreShardIdV1, UserProfileId,
        VerifiedStoreLocatorV1, WorktreeId,
    };

    use super::*;
    use crate::inventory::MigrationInventory;

    /// Records what a checkpoint save asked the host to do, in order, so the
    /// crash-visible sequence can be asserted without owning a real store.
    #[derive(Default)]
    struct RecordingWriter {
        calls: RefCell<Vec<String>>,
        fail_atomic: bool,
    }

    impl CheckpointWriter for RecordingWriter {
        fn write_file(&self, path: &Path, _contents: &[u8]) -> io::Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("write:{}", path.display()));
            Ok(())
        }

        fn write_file_atomically(
            &self,
            path: &Path,
            temp_path: &Path,
            _contents: &[u8],
        ) -> io::Result<()> {
            self.calls.borrow_mut().push(format!(
                "atomic:{} via {}",
                path.display(),
                temp_path.display()
            ));
            if self.fail_atomic {
                return Err(io::Error::other("staging failed"));
            }
            Ok(())
        }
    }

    fn inventory() -> MigrationInventory {
        MigrationInventory {
            stores: Vec::new(),
            skipped: Vec::new(),
            global_db: None,
        }
    }

    fn manifest_at(dir: &Path, migration_id: &str) -> MigrationManifest {
        let manifest_path = dir.join("migration-manifest.json");
        MigrationManifest::new(
            migration_id,
            "0.0.0-test",
            1_700_000_000,
            format!("confirm-{migration_id}"),
            MigrationProtocol::for_manifest(&manifest_path, migration_id),
            inventory(),
        )
    }

    #[test]
    fn checkpoint_ladder_advances_one_rung_at_a_time() {
        for (from, to) in [
            (ArtifactState::Planned, ArtifactState::Locked),
            (ArtifactState::Locked, ArtifactState::Copied),
            (ArtifactState::Copied, ArtifactState::Verified),
            (ArtifactState::Verified, ArtifactState::Applied),
        ] {
            let mut artifact = MigrationArtifact::new("graph_db", PathBuf::from("/s"), None);
            artifact.state = from.clone();
            artifact
                .transition_to(to.clone())
                .unwrap_or_else(|error| panic!("{from:?} -> {to:?} must be legal: {error}"));
            assert_eq!(artifact.state, to);
        }
    }

    /// Forward-only recovery depends on this: a checkpoint that could move
    /// backwards or skip a rung would let a resume re-run work it already
    /// published, or publish work it never staged.
    #[test]
    fn checkpoint_ladder_refuses_backward_and_skipping_transitions() {
        for (from, to) in [
            (ArtifactState::Applied, ArtifactState::Verified),
            (ArtifactState::Verified, ArtifactState::Copied),
            (ArtifactState::Copied, ArtifactState::Locked),
            (ArtifactState::Locked, ArtifactState::Planned),
            (ArtifactState::Planned, ArtifactState::Copied),
            (ArtifactState::Planned, ArtifactState::Verified),
            (ArtifactState::Planned, ArtifactState::Applied),
            (ArtifactState::Locked, ArtifactState::Applied),
            (ArtifactState::Applied, ArtifactState::Failed),
            (ArtifactState::Failed, ArtifactState::Locked),
        ] {
            let mut artifact = MigrationArtifact::new("graph_db", PathBuf::from("/s"), None);
            artifact.state = from.clone();
            assert!(
                artifact.transition_to(to.clone()).is_err(),
                "{from:?} -> {to:?} must be rejected"
            );
            assert_eq!(
                artifact.state, from,
                "a rejected transition must not mutate"
            );
        }
    }

    #[test]
    fn any_unpublished_state_may_fail() {
        for from in [
            ArtifactState::Planned,
            ArtifactState::Locked,
            ArtifactState::Copied,
            ArtifactState::Verified,
        ] {
            let mut artifact = MigrationArtifact::new("graph_db", PathBuf::from("/s"), None);
            artifact.state = from.clone();
            assert!(
                artifact.transition_to(ArtifactState::Failed).is_ok(),
                "{from:?} must be able to fail"
            );
        }
    }

    #[test]
    fn saving_a_checkpoint_takes_the_lock_before_publishing_atomically() {
        let dir = tempfile::tempdir().expect("temp dir");
        let manifest = manifest_at(dir.path(), "mig-1");
        let writer = RecordingWriter::default();

        save_manifest_with_writer(&writer, &manifest).expect("save checkpoint");

        let calls = writer.calls.borrow().clone();
        assert_eq!(
            calls.len(),
            2,
            "expected a lock write then an atomic publish"
        );
        assert!(
            calls[0].starts_with("write:"),
            "lock is written first: {calls:?}"
        );
        assert!(
            calls[0].ends_with("migration-manifest.json.lock"),
            "the first write is the lock: {calls:?}"
        );
        assert!(
            calls[1].starts_with("atomic:"),
            "the manifest is published atomically: {calls:?}"
        );
    }

    #[test]
    fn a_failed_publish_surfaces_rather_than_reporting_a_saved_checkpoint() {
        let dir = tempfile::tempdir().expect("temp dir");
        let manifest = manifest_at(dir.path(), "mig-1");
        let writer = RecordingWriter {
            fail_atomic: true,
            ..RecordingWriter::default()
        };

        let error = save_manifest_with_writer(&writer, &manifest)
            .expect_err("publish failure must surface");
        assert!(error.to_string().contains("staging failed"));
    }

    #[test]
    fn saving_refuses_a_manifest_with_no_confirmation_token() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut manifest = manifest_at(dir.path(), "mig-1");
        manifest.confirmation_token = String::new();

        let error = save_manifest_with_writer(&RecordingWriter::default(), &manifest)
            .expect_err("an unconfirmed migration must not checkpoint");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("confirmation_token"));
    }

    /// The protocol paths are derived, not supplied. Accepting caller-tampered
    /// paths would let a checkpoint publish outside the manifest's own
    /// directory.
    #[test]
    fn saving_refuses_protocol_paths_that_were_not_derived() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut manifest = manifest_at(dir.path(), "mig-1");
        manifest.protocol.lock_path = dir.path().join("somewhere-else.lock");

        let error = save_manifest_with_writer(&RecordingWriter::default(), &manifest)
            .expect_err("tampered protocol paths must be refused");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("must be derived"));
    }

    #[test]
    fn migration_ids_stay_a_single_safe_path_segment() {
        for legal in ["mig-1", "migration_2026.07", "ABC123"] {
            assert!(
                validate_migration_id(legal).is_ok(),
                "{legal} must be legal"
            );
        }
        for illegal in [
            "",
            "../escape",
            "nested/id",
            "back\\slash",
            "space id",
            "semi;colon",
        ] {
            assert!(
                validate_migration_id(illegal).is_err(),
                "{illegal:?} must be refused"
            );
        }
    }

    #[test]
    fn saving_refuses_an_unsafe_migration_id() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut manifest = manifest_at(dir.path(), "mig-1");
        manifest.migration_id = "../escape".to_owned();

        let error = save_manifest_with_writer(&RecordingWriter::default(), &manifest)
            .expect_err("an unsafe migration_id must not reach the filesystem");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn a_saved_checkpoint_reloads_with_the_same_plan() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut manifest = manifest_at(dir.path(), "mig-1");
        manifest.artifacts.push(MigrationArtifact::new(
            "graph_db",
            dir.path().join("graph.db"),
            Some(dir.path().join("target/graph.db")),
        ));
        manifest.artifacts[0]
            .transition_to(ArtifactState::Locked)
            .expect("lock the artifact");

        let bytes = serde_json::to_vec_pretty(&manifest).expect("serialize");
        std::fs::write(&manifest.protocol.manifest_path, &bytes).expect("write manifest");

        let reloaded = load_manifest(&manifest.protocol.manifest_path).expect("reload");
        assert_eq!(reloaded.migration_id, manifest.migration_id);
        assert_eq!(reloaded.schema_version, MIGRATION_MANIFEST_SCHEMA_VERSION);
        assert_eq!(reloaded.artifacts.len(), 1);
        assert_eq!(reloaded.artifacts[0].state, ArtifactState::Locked);
        assert_eq!(reloaded.artifacts[0].kind, "graph_db");
    }

    #[test]
    fn protocol_paths_derive_from_the_manifest_path_and_id() {
        let protocol = MigrationProtocol::for_manifest("/profile/migration-manifest.json", "mig-1");
        assert_eq!(
            protocol.temp_manifest_path,
            PathBuf::from("/profile/.migration-manifest.json.mig-1.tmp")
        );
        assert_eq!(
            protocol.lock_path,
            PathBuf::from("/profile/migration-manifest.json.lock")
        );
    }

    #[test]
    fn store_artifact_paths_refuse_traversal_and_non_normal_components() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert_eq!(
            StoreArtifactPath::from_relative(dir.path(), Path::new("../escape"), 0),
            Err(StoreArtifactPathValidationError::PathTraversal)
        );
        assert_eq!(
            StoreArtifactPath::from_relative(dir.path(), Path::new("/absolute"), 0),
            Err(StoreArtifactPathValidationError::PathTraversal)
        );
        assert_eq!(
            StoreArtifactPath::from_relative(dir.path(), Path::new("./here"), 0),
            Err(StoreArtifactPathValidationError::NonNormalComponent)
        );

        let ok = StoreArtifactPath::from_relative(dir.path(), Path::new("nested/graph.db"), 42)
            .expect("a normal relative path is accepted");
        assert_eq!(ok.absolute_path, dir.path().join("nested/graph.db"));
        assert_eq!(ok.size_bytes, 42);
    }

    #[cfg(unix)]
    #[test]
    fn store_artifact_paths_refuse_a_symlinked_component() {
        let dir = tempfile::tempdir().expect("temp dir");
        let real = dir.path().join("real");
        std::fs::create_dir(&real).expect("create real dir");
        std::os::unix::fs::symlink(&real, dir.path().join("link")).expect("symlink");

        assert_eq!(
            StoreArtifactPath::from_relative(dir.path(), Path::new("link/graph.db"), 0),
            Err(StoreArtifactPathValidationError::Symlink)
        );
    }

    #[test]
    fn final_v2_target_is_one_authoritative_schema_with_permanent_families() {
        let target = FinalTargetSchemaManifest::authoritative();

        assert_eq!(target.schema_id, FINAL_V2_SCHEMA_ID);
        assert_eq!(
            target
                .families
                .iter()
                .map(|family| family.family)
                .collect::<Vec<_>>(),
            vec![
                FinalSchemaFamily::BranchStackRevisions,
                FinalSchemaFamily::BranchStackRevisionNodes,
                FinalSchemaFamily::BranchStackRevisionEdges,
                FinalSchemaFamily::BranchStackPreviews,
                FinalSchemaFamily::BranchStackConsumedApprovals,
                FinalSchemaFamily::BranchStackJournals,
                FinalSchemaFamily::BranchStackReceipts,
                FinalSchemaFamily::BranchStackQuarantine,
                FinalSchemaFamily::RemoteObservationTransactions,
                FinalSchemaFamily::RemoteAdmittedEncryptionMetadata,
                FinalSchemaFamily::RemoteReplayDeduplication,
                FinalSchemaFamily::RemoteBackupStaging,
                FinalSchemaFamily::RemoteAuthorityCas,
                FinalSchemaFamily::ExternalSourceAuthorityRevisions,
                FinalSchemaFamily::ExternalSourceProjectionPublications,
            ]
        );
        target.validate().expect("canonical final target");

        let contract = LastReleasedToFinalV2MigrationContract::authoritative();
        assert_eq!(contract.source_schema_id, LAST_RELEASED_SCHEMA_ID);
        assert_eq!(contract.target_schema.schema_id, FINAL_V2_SCHEMA_ID);
        contract
            .validate()
            .expect("one direct released-to-final cutover");
    }

    #[test]
    fn final_families_declare_identity_cas_one_use_and_destructive_safety() {
        let target = FinalTargetSchemaManifest::authoritative();

        assert!(
            target
                .family(FinalSchemaFamily::BranchStackRevisions)
                .invariants
                .contains(&FinalSchemaInvariant::ExactProjectAndSourceGeneration)
        );
        assert!(
            target
                .family(FinalSchemaFamily::BranchStackConsumedApprovals)
                .invariants
                .contains(&FinalSchemaInvariant::OneUse)
        );
        assert!(
            target
                .family(FinalSchemaFamily::RemoteAuthorityCas)
                .invariants
                .contains(&FinalSchemaInvariant::CompareAndSwap)
        );
        assert!(
            target
                .family(FinalSchemaFamily::RemoteBackupStaging)
                .invariants
                .contains(&FinalSchemaInvariant::VerifiedBackupBeforeDestruction)
        );
        assert!(
            target
                .family(FinalSchemaFamily::ExternalSourceAuthorityRevisions)
                .invariants
                .contains(&FinalSchemaInvariant::CompareAndSwap)
        );
        assert!(
            target
                .family(FinalSchemaFamily::ExternalSourceProjectionPublications)
                .invariants
                .contains(&FinalSchemaInvariant::DurableReplayDeduplication)
        );
    }

    fn final_source(generation: &str) -> ExactMigrationSourceIdentity {
        fn id<T>(value: &str) -> T
        where
            T: TryFrom<String>,
            <T as TryFrom<String>>::Error: std::fmt::Debug,
        {
            T::try_from(value.to_owned()).unwrap()
        }
        let material = if generation.ends_with('8') { 8 } else { 7 };
        let shard_id = StoreShardIdV1::code(
            id::<BrainId>("brain.final-v2"),
            id::<UserProfileId>("profile.final-v2"),
            id::<ProjectId>("project.final-v2"),
            id::<RepositoryId>("repository.final-v2"),
            CodeShardScopeV1::Worktree {
                worktree_id: id::<WorktreeId>("worktree.final-v2"),
            },
        );
        let incarnation = StoreIncarnationV1::new(1).unwrap();
        let binding = StoreRuntimeBindingV1::new(
            shard_id.clone(),
            incarnation,
            StoreAuthorityEpochV1::new(1).unwrap(),
        );
        let locator = VerifiedStoreLocatorV1::new(
            shard_id,
            incarnation,
            LocatorDigest::new(format!("sha256:{material:064x}")).unwrap(),
        );
        ExactMigrationSourceIdentity::new(ExactMigrationSourceIdentityRequest {
            profile_id: "profile.final-v2".to_owned(),
            repository_id: "repository.final-v2".to_owned(),
            project_id: "project.final-v2".to_owned(),
            store_id: "store.final-v2".to_owned(),
            runtime_binding: binding,
            verified_locator: locator,
            material_digest: [material; 32],
            schema_id: LAST_RELEASED_SCHEMA_ID.to_owned(),
        })
        .expect("source identity")
    }

    fn verified_backup(source: ExactMigrationSourceIdentity) -> VerifiedBackupIdentity {
        VerifiedBackupIdentity::new("backup.final-v2", source, "archive.final-v2", [7; 32], 100)
            .expect("verified backup")
    }

    fn transformed(source: ExactMigrationSourceIdentity) -> FinalV2TransformReceipt {
        FinalV2TransformReceipt {
            schema: FinalV2SchemaEvidence {
                source: source.clone(),
                schema_id: FINAL_V2_SCHEMA_ID.to_owned(),
                project_schema_version: FINAL_PROJECT_SCHEMA_VERSION,
                lcm_schema_version: FINAL_LCM_SCHEMA_VERSION,
                store_manifest_schema_version: FINAL_STORE_MANIFEST_SCHEMA_VERSION,
                repository_identity_schema_version: FINAL_REPOSITORY_IDENTITY_SCHEMA_VERSION,
                profile_identity_schema_version: FINAL_PROFILE_IDENTITY_SCHEMA_VERSION,
                durable_families: ReleasedDurableFamily::all(),
            },
            preservation: FinalV2PreservationReceipt {
                source,
                preserved_families: ReleasedDurableFamily::all(),
                before_digest: [4; 32],
                after_digest: [4; 32],
            },
            rebuilt_derived_families: BTreeSet::new(),
        }
    }

    fn publication_grant(source: ExactMigrationSourceIdentity) -> PublicationCasGrant {
        PublicationCasGrant::new(
            "authority-cas.final-v2",
            "migration.final-v2",
            "checkpoint.final-v2",
            source.clone(),
            transformed(source).schema,
            0,
            1,
        )
        .expect("publication grant")
    }

    #[test]
    fn checkpoint_derives_publication_boundary_from_receipt() {
        let source = final_source("source-generation.7");
        let backup = verified_backup(source.clone());
        let mut checkpoint = DurableMigrationCheckpoint::before_publication(
            "checkpoint.final-v2",
            "migration.final-v2",
            source.clone(),
            backup,
            90,
        )
        .expect("pre-publication checkpoint");
        assert!(!checkpoint.is_published());
        checkpoint
            .record_transformation(transformed(source.clone()))
            .expect("verified transformation");

        let grant = publication_grant(source.clone());
        let receipt = CutoverPublicationReceipt::from_cas_grant(
            "publication.final-v2",
            source,
            FINAL_V2_SCHEMA_ID,
            &grant,
            120,
        )
        .expect("publication receipt");
        checkpoint
            .record_publication(receipt, &grant)
            .expect("publish exact source generation");
        assert!(checkpoint.is_published());
        checkpoint.validate().expect("durable published checkpoint");
    }

    #[test]
    fn publication_receipt_must_match_the_exact_project_and_source_generation() {
        let source = final_source("source-generation.7");
        let backup = verified_backup(source.clone());
        let mut checkpoint = DurableMigrationCheckpoint::before_publication(
            "checkpoint.final-v2",
            "migration.final-v2",
            source,
            backup,
            90,
        )
        .expect("pre-publication checkpoint");
        checkpoint
            .record_transformation(transformed(checkpoint.source.clone()))
            .expect("verified transformation");
        let grant = publication_grant(checkpoint.source.clone());
        let wrong_source = final_source("source-generation.8");
        let wrong_grant = publication_grant(wrong_source.clone());
        let wrong_generation = CutoverPublicationReceipt::from_cas_grant(
            "publication.final-v2",
            wrong_source,
            FINAL_V2_SCHEMA_ID,
            &wrong_grant,
            120,
        )
        .expect("well-formed mismatched receipt");

        assert_eq!(
            checkpoint.record_publication(wrong_generation, &grant),
            Err(MigrationContractError::PublicationInvalid)
        );
        assert!(!checkpoint.is_published());
    }

    #[test]
    fn archive_expiry_needs_a_matching_caller_declared_policy_receipt() {
        let source = final_source("source-generation.7");
        let backup = verified_backup(source.clone());
        let mut checkpoint = DurableMigrationCheckpoint::before_publication(
            "checkpoint.final-v2",
            "migration.final-v2",
            source.clone(),
            backup,
            90,
        )
        .expect("pre-publication checkpoint");
        checkpoint
            .record_transformation(transformed(source.clone()))
            .expect("verified transformation");
        let grant = publication_grant(source.clone());
        checkpoint
            .record_publication(
                CutoverPublicationReceipt::from_cas_grant(
                    "publication.final-v2",
                    source.clone(),
                    FINAL_V2_SCHEMA_ID,
                    &grant,
                    120,
                )
                .expect("publication receipt"),
                &grant,
            )
            .expect("publication");
        let policy = ArchiveExpiryPolicyReceipt::new(
            "retention-policy.final-v2",
            source,
            "archive.final-v2",
            125,
            200,
        )
        .expect("caller-declared policy receipt");

        assert_eq!(
            checkpoint.archive_expiry_eligibility(&policy, 199),
            Err(MigrationContractError::ArchiveNotYetEligible)
        );
        let eligibility = checkpoint
            .archive_expiry_eligibility(&policy, 200)
            .expect("policy makes the exact archive eligible");
        assert_eq!(eligibility.archive_id, "archive.final-v2");
        assert_eq!(eligibility.policy_receipt_id, "retention-policy.final-v2");
    }
}
