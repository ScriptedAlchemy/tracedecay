//! Digest-addressed model artifact store with verified resumable import
//! (Plan 31 "Model and offline lifecycle", packet
//! `pr10/prep-artifact-manifest`).
//!
//! Layout under the caller-owned root (Plan-02-owned user store at
//! integration; keyed by signed artifact digest, never an ambient cache):
//!
//! ```text
//! <root>/staging/<opaque-id>/members/*        resumable package staging
//! <root>/staging/<opaque-id>/import.meta.json signed package identity
//! <root>/artifacts/<signed-envelope-digest>/  verified package members
//! <root>/inventory.json                       staged|verified|installed|...
//! <root>/receipts/gc.jsonl                    append-only GC receipts
//! <root>/.artifact-store-recovery.json        crash-recovery transaction
//! ```
//!
//! Import accepts caller-provided bytes only. It stages under a random local
//! directory, resumes only because the manifest supplies immutable length and
//! digest identity, streams length + SHA-256 verification, verifies the
//! manifest signature against the admitted trust roots BEFORE atomic rename,
//! fsyncs file and directory, then publishes the inventory record. Corrupt,
//! revoked, quarantined, or runtime-incompatible artifacts disable the
//! semantic capability with a typed error — never a substitution, never a
//! query-time download or import.
//!
//! QUARANTINE: not reachable from production code yet; no network types, no
//! query/retrieval wiring, profile-independent retention, and GC.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use cap_fs_ext::{
    DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsMaybeDirExt, OpenOptionsSyncExt,
    ambient_authority,
};
use cap_std::fs::{Dir, DirBuilder, File as CapFile, OpenOptions as CapOpenOptions};
#[cfg(unix)]
use cap_std::fs::{DirBuilderExt, OpenOptionsExt};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::manifest::{
    ArtifactMemberRoleV1, ArtifactPackageMemberV1, ModelArtifactManifestV1, ResourceCeilingV1,
    Sha256DigestHex,
};
use super::trust_roots::{Ed25519Verifier, TrustRootSetV1};

const RECOVERY_SCHEMA_V1: &str = "tracedecay.artifact-store-recovery.v1";
const STAGING_SCHEMA_V1: &str = "tracedecay.artifact-store-staging.v1";

/// Inventory record states (Plan 31: `staged | verified | installed |
/// revoked | quarantined | retained_for_rollback`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ArtifactInventoryStateV1 {
    Staged,
    Verified,
    Installed,
    Revoked,
    Quarantined,
    RetainedForRollback,
}

/// One digest-addressed inventory record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactInventoryRecordV1 {
    /// Digest of the complete signed manifest envelope. This is the package
    /// identity and prevents same-model/different-tokenizer collisions.
    pub artifact_digest: Sha256DigestHex,
    /// Digest of signed payload bytes, retained for audit correlation.
    pub manifest_digest: Sha256DigestHex,
    pub trust_binding: ArtifactTrustBindingV1,
    pub members: Vec<ArtifactPackageMemberV1>,
    pub state: ArtifactInventoryStateV1,
    pub recorded_at_unix: u64,
    pub quarantine_reason: Option<QuarantineReasonV1>,
}

/// Trust evidence copied from the verified detached signature into durable
/// inventory. The key ID and rotation epoch are both required.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactTrustBindingV1 {
    pub root_id: String,
    pub rotation_epoch: u32,
}

/// Stable, non-sensitive quarantine classification. Never retain input paths,
/// raw handles, filesystem errors, or package bytes in inventory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuarantineReasonV1 {
    IdentityMismatch,
    MemberLengthMismatch,
    MemberDigestMismatch,
    SizeExpansion,
    RecoveryFailure,
}

/// Durable profile-independent inventory. Plan 20 owns active/rollback
/// profile pointers and their compare-and-swap semantics.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactInventoryV1 {
    pub records: BTreeMap<String, ArtifactInventoryRecordV1>,
}

/// Host runtime evidence checked against the manifest's compatibility pins at
/// admission time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeEnvironmentV1 {
    pub os: String,
    pub arch: String,
    pub runtime: String,
    pub build_revision: String,
    pub available_resident_bytes: u64,
    pub available_threads: u32,
}

/// Import failures. Every variant is typed; staging is discarded or
/// quarantined on failure and never exposed to runtime discovery.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ArtifactImportErrorV1 {
    #[error("artifact manifest rejected")]
    ManifestRejected,
    #[error("artifact trust binding rejected")]
    TrustRejected,
    #[error("detached signature does not verify over canonical manifest bytes")]
    SignatureInvalid,
    #[error("staged write exceeds the declared member length")]
    SizeExpansionBeyondDeclared,
    #[error("staged member length does not match its declared pin")]
    LengthMismatch,
    #[error("staged member digest does not match its declared pin")]
    DigestMismatch,
    #[error("artifact package member set is incomplete or inconsistent")]
    MemberMismatch,
    #[error("artifact import session is unavailable")]
    StagingUnavailable,
    #[error("staging session identity does not match the manifest pins")]
    ResumeIdentityMismatch,
    #[error("artifact import session handle is invalid")]
    UnsafeStagingHandle,
    #[error("artifact store path is unsafe")]
    UnsafeStorePath,
    #[error("artifact store is busy")]
    StoreBusy,
    #[error("artifact store operation failed")]
    StorageFailure,
}

impl From<io::Error> for ArtifactImportErrorV1 {
    fn from(_: io::Error) -> Self {
        ArtifactImportErrorV1::StorageFailure
    }
}

/// Semantic-capability disable causes. Admission returns these typed errors;
/// there is no alternative-model field and no fallback selection — a disabled
/// semantic stage preserves the lexical/graph baseline exactly (Plan 31).
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SemanticCapabilityDisabledV1 {
    #[error("artifact is not installed")]
    MissingArtifact,
    #[error("installed artifact bytes fail verification")]
    CorruptArtifact,
    #[error("artifact is revoked")]
    RevokedArtifact,
    #[error("artifact is quarantined")]
    QuarantinedArtifact,
    #[error("manifest trust binding rejected")]
    UntrustedRoot,
    #[error("manifest signature invalid")]
    SignatureInvalid,
    #[error("runtime is incompatible")]
    IncompatibleRuntime,
    #[error("platform is incompatible")]
    IncompatiblePlatform,
    #[error("resource ceiling cannot be honored")]
    ResourceCeilingExceeded,
    #[error("artifact identity does not match verified inventory")]
    IdentityMismatch,
    #[error("artifact store operation failed")]
    StorageFailure,
}

impl From<io::Error> for SemanticCapabilityDisabledV1 {
    fn from(_: io::Error) -> Self {
        SemanticCapabilityDisabledV1::StorageFailure
    }
}

/// An artifact admitted for runtime use. The disk path intentionally stays
/// store-private; later runtime wiring receives a store-owned handle instead
/// of an ambient filesystem path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AdmittedArtifactV1 {
    artifact_digest: Sha256DigestHex,
    manifest_digest: Sha256DigestHex,
    manifest: ModelArtifactManifestV1,
}

impl AdmittedArtifactV1 {
    pub(super) fn artifact_digest(&self) -> &Sha256DigestHex {
        &self.artifact_digest
    }

    pub(super) fn manifest_digest(&self) -> &Sha256DigestHex {
        &self.manifest_digest
    }

    pub(super) fn manifest(&self) -> &ModelArtifactManifestV1 {
        &self.manifest
    }

    #[cfg(test)]
    pub(super) fn test_fixture(manifest: ModelArtifactManifestV1) -> Self {
        Self {
            artifact_digest: manifest.signed_identity_digest(),
            manifest_digest: manifest.canonical_digest(),
            manifest,
        }
    }

    #[cfg(test)]
    pub(super) fn test_fixture_with_identities(
        manifest: ModelArtifactManifestV1,
        artifact_digest: Sha256DigestHex,
        manifest_digest: Sha256DigestHex,
    ) -> Self {
        Self {
            artifact_digest,
            manifest_digest,
            manifest,
        }
    }
}

/// Retention policy for garbage collection. Collection removes only
/// unreferenced records past the grace window and appends one receipt per
/// removal. Installed, revoked, and rollback-retained records are preserved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetentionPolicyV1 {
    /// Minimum age (seconds since `recorded_at_unix`) before an unreferenced
    /// `Verified` or `Quarantined` artifact is collectible.
    pub grace_seconds: u64,
}

/// Append-only GC receipt (one JSON line per removed artifact).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcReceiptV1 {
    pub artifact_digest: Sha256DigestHex,
    pub removed_at_unix: u64,
    pub prior_state: ArtifactInventoryStateV1,
}

/// Resume identity persisted beside staged bytes. It persists the complete
/// signed envelope and every package member, so recovery can never infer a
/// missing identity from an ambient cache or path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct StagingMetaV1 {
    schema: String,
    manifest: ModelArtifactManifestV1,
    signed_manifest_digest: Sha256DigestHex,
    verified_at_unix: u64,
    members: Vec<StagedMemberV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct StagedMemberV1 {
    member: ArtifactPackageMemberV1,
    bytes_written: u64,
}

/// An open import session over one staging directory.
pub struct ImportSession {
    staging_id: String,
    staging_path: PathBuf,
    staging_dir: Dir,
    members_dir: Dir,
    meta: StagingMetaV1,
}

impl std::fmt::Debug for ImportSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ImportSession")
            .field("handle", &"<private>")
            .field("bytes_written", &self.bytes_written())
            .finish()
    }
}

impl ImportSession {
    pub fn staging_id(&self) -> String {
        self.staging_id.clone()
    }

    pub fn bytes_written(&self) -> u64 {
        self.meta
            .members
            .iter()
            .find(|member| member.member.role == ArtifactMemberRoleV1::Model)
            .map(|member| member.bytes_written)
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct RecoveryJournalV1 {
    schema: String,
    #[serde(flatten)]
    action: RecoveryActionV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum RecoveryActionV1 {
    Install {
        record: ArtifactInventoryRecordV1,
        staging_id: String,
    },
    Gc {
        recorded_at_unix: u64,
        records: Vec<ArtifactInventoryRecordV1>,
    },
}

struct ArtifactStoreLock<'a> {
    _memory: MutexGuard<'a, ()>,
    _file: File,
}

/// The digest-addressed, profile-independent model artifact store.
pub struct ModelArtifactStore {
    root: PathBuf,
    root_dir: Dir,
    staging_dir: Dir,
    artifacts_dir: Dir,
    receipts_dir: Dir,
    trust_roots: TrustRootSetV1,
    verifier: Arc<dyn Ed25519Verifier>,
    retention: RetentionPolicyV1,
    operation_lock: Arc<Mutex<()>>,
}

impl ModelArtifactStore {
    /// Open (creating if needed) a store rooted at `root` with the admitted
    /// trust-root set, the signature verifier backend, and retention policy.
    pub fn open(
        root: impl Into<PathBuf>,
        trust_roots: TrustRootSetV1,
        verifier: Arc<dyn Ed25519Verifier>,
        retention: RetentionPolicyV1,
    ) -> Result<Self, ArtifactImportErrorV1> {
        trust_roots
            .validate()
            .map_err(|_| ArtifactImportErrorV1::TrustRejected)?;
        let root = root.into();
        let root_dir = open_root_from_trusted_parent(&root)?;
        let staging_dir = open_or_create_component_dir(&root_dir, "staging")?;
        let artifacts_dir = open_or_create_component_dir(&root_dir, "artifacts")?;
        let receipts_dir = open_or_create_component_dir(&root_dir, "receipts")?;
        let store = Self {
            root,
            root_dir,
            staging_dir,
            artifacts_dir,
            receipts_dir,
            trust_roots,
            verifier,
            retention,
            operation_lock: Arc::new(Mutex::new(())),
        };
        {
            let _lock = store.acquire_lock()?;
            store.recover_locked()?;
        }
        Ok(store)
    }

    fn inventory_path(&self) -> PathBuf {
        self.root.join("inventory.json")
    }

    fn recovery_path(&self) -> PathBuf {
        self.root.join(".artifact-store-recovery.json")
    }

    fn staging_root(&self) -> PathBuf {
        self.root.join("staging")
    }

    fn artifacts_root(&self) -> PathBuf {
        self.root.join("artifacts")
    }

    fn receipts_root(&self) -> PathBuf {
        self.root.join("receipts")
    }

    fn staging_dir_for(&self, staging_id: &str) -> Result<PathBuf, ArtifactImportErrorV1> {
        if !is_valid_staging_id(staging_id) {
            return Err(ArtifactImportErrorV1::UnsafeStagingHandle);
        }
        Ok(self.staging_root().join(staging_id))
    }

    fn artifact_dir(&self, digest: &Sha256DigestHex) -> PathBuf {
        self.artifacts_root().join(digest.as_str())
    }

    fn artifact_path(&self, digest: &Sha256DigestHex) -> PathBuf {
        self.member_path(digest, ArtifactMemberRoleV1::Model)
    }

    fn member_path(&self, digest: &Sha256DigestHex, role: ArtifactMemberRoleV1) -> PathBuf {
        self.artifact_dir(digest).join(member_file_name(role))
    }

    fn acquire_lock(&self) -> Result<ArtifactStoreLock<'_>, ArtifactImportErrorV1> {
        let memory = self
            .operation_lock
            .lock()
            .map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
        let file = open_cap_file(
            &self.root_dir,
            ".artifact-store.lock",
            true,
            true,
            true,
            false,
            false,
        )?
        .into_std();
        file.lock_exclusive()
            .map_err(|_| ArtifactImportErrorV1::StoreBusy)?;
        Ok(ArtifactStoreLock {
            _memory: memory,
            _file: file,
        })
    }

    /// Load the inventory (absent file = empty inventory).
    pub fn inventory(&self) -> Result<ArtifactInventoryV1, ArtifactImportErrorV1> {
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        self.load_inventory_locked()
    }

    fn save_inventory(&self, inventory: &ArtifactInventoryV1) -> Result<(), ArtifactImportErrorV1> {
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        self.save_inventory_locked(inventory)?;
        Ok(())
    }

    fn load_inventory_locked(&self) -> Result<ArtifactInventoryV1, ArtifactImportErrorV1> {
        let Some(bytes) = read_optional_cap_file(&self.root_dir, "inventory.json")? else {
            return Ok(ArtifactInventoryV1::default());
        };
        serde_json::from_slice(&bytes).map_err(|_| ArtifactImportErrorV1::StorageFailure)
    }

    fn save_inventory_locked(
        &self,
        inventory: &ArtifactInventoryV1,
    ) -> Result<(), ArtifactImportErrorV1> {
        let bytes =
            serde_json::to_vec(inventory).map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
        atomic_write_cap_file(&self.root_dir, &self.root, "inventory.json", &bytes)
    }

    /// Verify manifest structure, trust-root admission, and the detached
    /// Ed25519 signature over the canonical payload bytes. Runs BEFORE any
    /// byte is staged, so a bad signature never reaches disk.
    #[allow(dead_code)] // public import gate retained for artifact runtime prep
    pub fn verify_manifest(
        &self,
        manifest: &ModelArtifactManifestV1,
        now_unix: u64,
    ) -> Result<(), ArtifactImportErrorV1> {
        self.verify_manifest_binding(manifest, now_unix).map(|_| ())
    }

    fn verify_manifest_binding(
        &self,
        manifest: &ModelArtifactManifestV1,
        now_unix: u64,
    ) -> Result<ArtifactTrustBindingV1, ArtifactImportErrorV1> {
        manifest
            .validate()
            .map_err(|_| ArtifactImportErrorV1::ManifestRejected)?;
        let root = self
            .trust_roots
            .resolve(&manifest.signature.trust_root_id, now_unix)
            .map_err(|_| ArtifactImportErrorV1::TrustRejected)?;
        if root.rotation_epoch != manifest.signature.trust_root_epoch {
            return Err(ArtifactImportErrorV1::TrustRejected);
        }
        self.verifier
            .verify_ed25519(
                &root.public_key.to_bytes(),
                &manifest.canonical_bytes(),
                &manifest.signature.signature.to_bytes(),
            )
            .map_err(|_| ArtifactImportErrorV1::SignatureInvalid)?;
        Ok(ArtifactTrustBindingV1 {
            root_id: root.root_id.clone(),
            rotation_epoch: root.rotation_epoch,
        })
    }

    /// Begin a resumable import of caller-provided bytes for a verified
    /// manifest. Stages under a random local directory; no network access.
    pub fn begin_import(
        &self,
        manifest: &ModelArtifactManifestV1,
        now_unix: u64,
    ) -> Result<ImportSession, ArtifactImportErrorV1> {
        let binding = self.verify_manifest_binding(manifest, now_unix)?;
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        if self
            .load_inventory_locked()?
            .records
            .contains_key(&manifest.signed_identity_digest().to_string())
        {
            return Err(ArtifactImportErrorV1::StagingUnavailable);
        }
        let (staging_id, staging_dir) = (0..16)
            .find_map(|_| {
                let staging_id = random_staging_id().ok()?;
                let mut builder = DirBuilder::new();
                #[cfg(unix)]
                builder.mode(0o700);
                match self.staging_dir.create_dir_with(&staging_id, &builder) {
                    Ok(()) => {
                        let staging_dir = self.staging_dir.open_dir_nofollow(&staging_id).ok()?;
                        Some((staging_id, staging_dir))
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => None,
                    Err(_) => None,
                }
            })
            .ok_or(ArtifactImportErrorV1::StorageFailure)?;
        let mut builder = DirBuilder::new();
        #[cfg(unix)]
        builder.mode(0o700);
        staging_dir
            .create_dir_with("members", &builder)
            .map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
        let members_dir = staging_dir
            .open_dir_nofollow("members")
            .map_err(|_| ArtifactImportErrorV1::UnsafeStorePath)?;
        let meta = StagingMetaV1 {
            schema: STAGING_SCHEMA_V1.to_string(),
            manifest: manifest.clone(),
            signed_manifest_digest: manifest.signed_identity_digest(),
            verified_at_unix: now_unix,
            members: manifest
                .payload
                .members
                .iter()
                .cloned()
                .map(|member| StagedMemberV1 {
                    member,
                    bytes_written: 0,
                })
                .collect(),
        };
        for member in &meta.members {
            let _file = open_cap_file(
                &members_dir,
                member_file_name(member.member.role),
                false,
                true,
                false,
                true,
                false,
            )?;
        }
        write_staging_meta(&staging_dir, &self.staging_dir_for(&staging_id)?, &meta)?;
        let record = self.record_for(
            manifest,
            binding,
            ArtifactInventoryStateV1::Staged,
            now_unix,
            None,
        );
        let mut inventory = self.load_inventory_locked()?;
        inventory
            .records
            .insert(record.artifact_digest.to_string(), record);
        self.save_inventory_locked(&inventory)?;
        sync_cap_dir(&self.staging_dir)?;
        Ok(ImportSession {
            staging_path: self.staging_dir_for(&staging_id)?,
            staging_id,
            staging_dir,
            members_dir,
            meta,
        })
    }

    /// Resume an interrupted import. Permitted only because the manifest pins
    /// immutable length and digest identity; a sidecar mismatch discards the
    /// staging directory and reports a typed error.
    pub fn resume_import(
        &self,
        manifest: &ModelArtifactManifestV1,
        staging_id: &str,
        now_unix: u64,
    ) -> Result<ImportSession, ArtifactImportErrorV1> {
        let staging_path = self.staging_dir_for(staging_id)?;
        self.verify_manifest_binding(manifest, now_unix)?;
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        let staging_dir = self
            .staging_dir
            .open_dir_nofollow(staging_id)
            .map_err(|_| ArtifactImportErrorV1::StagingUnavailable)?;
        let members_dir = staging_dir
            .open_dir_nofollow("members")
            .map_err(|_| ArtifactImportErrorV1::UnsafeStorePath)?;
        let meta = read_staging_meta(&staging_dir)?;
        let session = ImportSession {
            staging_id: staging_id.to_string(),
            staging_path,
            staging_dir,
            members_dir,
            meta,
        };
        self.ensure_session_active_locked(&session)?;
        if !self.staging_meta_matches(&session.meta, manifest)
            || !self.staging_member_lengths_match(&session)?
        {
            self.quarantine_staging_locked(
                &session,
                QuarantineReasonV1::IdentityMismatch,
                now_unix,
            )?;
            let staging_id = session.staging_id.clone();
            drop(session);
            self.remove_staging_dir_path(&staging_id)?;
            return Err(ArtifactImportErrorV1::ResumeIdentityMismatch);
        }
        Ok(session)
    }

    /// Append caller-provided bytes to the staged payload. Writes beyond the
    /// declared length are rejected as size expansion and quarantine the
    /// staged bytes (recorded against the declared digest) without exposing
    /// them to runtime discovery.
    pub fn stage_chunk(
        &self,
        session: &mut ImportSession,
        bytes: &[u8],
        now_unix: u64,
    ) -> Result<(), ArtifactImportErrorV1> {
        self.stage_member_chunk(session, ArtifactMemberRoleV1::Model, bytes, now_unix)
    }

    /// Append caller-provided bytes to one explicitly declared package member.
    /// The role selects a store-owned filename; a manifest path is identity
    /// metadata only and can never influence local traversal.
    pub fn stage_member_chunk(
        &self,
        session: &mut ImportSession,
        role: ArtifactMemberRoleV1,
        bytes: &[u8],
        now_unix: u64,
    ) -> Result<(), ArtifactImportErrorV1> {
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        self.ensure_session_dir(session)?;
        self.ensure_session_active_locked(session)?;
        let member_index = session
            .meta
            .members
            .iter()
            .position(|member| member.member.role == role)
            .ok_or(ArtifactImportErrorV1::MemberMismatch)?;
        let member = &session.meta.members[member_index];
        let attempted = member.bytes_written.saturating_add(bytes.len() as u64);
        if attempted > member.member.byte_length {
            self.quarantine_staging_locked(session, QuarantineReasonV1::SizeExpansion, now_unix)?;
            return Err(ArtifactImportErrorV1::SizeExpansionBeyondDeclared);
        }
        let mut file = open_cap_file(
            &session.members_dir,
            member_file_name(role),
            false,
            true,
            false,
            false,
            true,
        )?;
        file.write_all(bytes)?;
        file.sync_all()?;
        session.meta.members[member_index].bytes_written = attempted;
        write_staging_meta(&session.staging_dir, &session.staging_path, &session.meta)?;
        Ok(())
    }

    /// Finalize: stream length + SHA-256 verification of the staged bytes,
    /// fsync, atomic rename into the digest-addressed layout, fsync the
    /// directory, then publish the `Installed` inventory record. Digest or
    /// length mismatch quarantines the import.
    pub fn finalize_import(
        &self,
        session: ImportSession,
        manifest: &ModelArtifactManifestV1,
        now_unix: u64,
    ) -> Result<ArtifactInventoryRecordV1, ArtifactImportErrorV1> {
        let binding = self.verify_manifest_binding(manifest, now_unix)?;
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        self.ensure_session_dir(&session)?;
        self.ensure_session_active_locked(&session)?;
        if !self.staging_meta_matches(&session.meta, manifest) {
            self.quarantine_staging_locked(
                &session,
                QuarantineReasonV1::IdentityMismatch,
                now_unix,
            )?;
            return Err(ArtifactImportErrorV1::ResumeIdentityMismatch);
        }

        for staged in &session.meta.members {
            let file = open_cap_file(
                &session.members_dir,
                member_file_name(staged.member.role),
                true,
                false,
                false,
                false,
                false,
            )?;
            let length = file
                .metadata()
                .map_err(|_| ArtifactImportErrorV1::StorageFailure)?
                .len();
            if length != staged.member.byte_length || staged.bytes_written != length {
                self.quarantine_staging_locked(
                    &session,
                    QuarantineReasonV1::MemberLengthMismatch,
                    now_unix,
                )?;
                return Err(ArtifactImportErrorV1::LengthMismatch);
            }
            let actual = sha256_open_file(file)?;
            if actual != staged.member.digest {
                self.quarantine_staging_locked(
                    &session,
                    QuarantineReasonV1::MemberDigestMismatch,
                    now_unix,
                )?;
                return Err(ArtifactImportErrorV1::DigestMismatch);
            }
        }

        let mut record = self.record_for(
            manifest,
            binding,
            ArtifactInventoryStateV1::Verified,
            now_unix,
            None,
        );
        let mut inventory = self.load_inventory_locked()?;
        inventory
            .records
            .insert(record.artifact_digest.to_string(), record.clone());
        self.save_inventory_locked(&inventory)?;
        self.write_recovery_locked(&RecoveryJournalV1 {
            schema: RECOVERY_SCHEMA_V1.to_string(),
            action: RecoveryActionV1::Install {
                record: record.clone(),
                staging_id: session.staging_id.clone(),
            },
        })?;

        let ImportSession {
            staging_id,
            staging_path: _,
            staging_dir,
            members_dir,
            meta: _,
        } = session;
        drop(members_dir);
        let destination = record.artifact_digest.as_str();
        match self.artifacts_dir.symlink_metadata(destination) {
            Ok(_) => self.verify_artifact_record(&record)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                staging_dir.rename("members", &self.artifacts_dir, destination)?;
                sync_cap_dir(&staging_dir)?;
                sync_cap_dir(&self.artifacts_dir)?;
            }
            Err(_) => return Err(ArtifactImportErrorV1::StorageFailure),
        }

        record.state = ArtifactInventoryStateV1::Installed;
        inventory
            .records
            .insert(record.artifact_digest.to_string(), record.clone());
        self.save_inventory_locked(&inventory)?;
        self.remove_staging_dir_path(&staging_id)?;
        self.clear_recovery_locked()?;
        Ok(record)
    }

    fn quarantine_staging_locked(
        &self,
        session: &ImportSession,
        reason: QuarantineReasonV1,
        now_unix: u64,
    ) -> Result<(), ArtifactImportErrorV1> {
        self.ensure_session_dir(session)?;
        let binding = ArtifactTrustBindingV1 {
            root_id: session.meta.manifest.signature.trust_root_id.clone(),
            rotation_epoch: session.meta.manifest.signature.trust_root_epoch,
        };
        let record = self.record_for(
            &session.meta.manifest,
            binding,
            ArtifactInventoryStateV1::Quarantined,
            now_unix,
            Some(reason),
        );
        let mut inventory = self.load_inventory_locked()?;
        inventory
            .records
            .insert(record.artifact_digest.to_string(), record);
        self.save_inventory_locked(&inventory)
    }

    /// Mark an installed artifact revoked. Revoked artifacts are never
    /// admitted and are protected from GC (revocation evidence is retained).
    pub fn revoke_artifact(
        &self,
        digest: &Sha256DigestHex,
        now_unix: u64,
    ) -> Result<(), ArtifactImportErrorV1> {
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        let mut inventory = self.load_inventory_locked()?;
        if let Some(record) = inventory.records.get_mut(&digest.to_string()) {
            record.state = ArtifactInventoryStateV1::Revoked;
            record.recorded_at_unix = now_unix;
        }
        self.save_inventory_locked(&inventory)
    }

    /// Retain an installed artifact explicitly for rollback; retained
    /// artifacts are never collected.
    pub fn retain_for_rollback(
        &self,
        digest: &Sha256DigestHex,
        now_unix: u64,
    ) -> Result<(), ArtifactImportErrorV1> {
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        let mut inventory = self.load_inventory_locked()?;
        if let Some(record) = inventory.records.get_mut(&digest.to_string())
            && record.state == ArtifactInventoryStateV1::Installed
        {
            record.state = ArtifactInventoryStateV1::RetainedForRollback;
            record.recorded_at_unix = now_unix;
        }
        self.save_inventory_locked(&inventory)
    }

    /// Admit an installed artifact for runtime use against host evidence.
    /// Re-verifies trust + signature + on-disk digest; any corrupt, revoked,
    /// quarantined, or incompatible artifact disables the semantic capability
    /// with a typed error and no substitution.
    pub(super) fn admit_for_runtime(
        &self,
        digest: &Sha256DigestHex,
        manifest: &ModelArtifactManifestV1,
        env: &RuntimeEnvironmentV1,
        now_unix: u64,
    ) -> Result<AdmittedArtifactV1, SemanticCapabilityDisabledV1> {
        let binding =
            self.verify_manifest_binding(manifest, now_unix)
                .map_err(|error| match error {
                    ArtifactImportErrorV1::SignatureInvalid => {
                        SemanticCapabilityDisabledV1::SignatureInvalid
                    }
                    ArtifactImportErrorV1::ManifestRejected
                    | ArtifactImportErrorV1::TrustRejected => {
                        SemanticCapabilityDisabledV1::UntrustedRoot
                    }
                    _ => SemanticCapabilityDisabledV1::StorageFailure,
                })?;
        let _lock = self
            .acquire_lock()
            .map_err(|_| SemanticCapabilityDisabledV1::StorageFailure)?;
        self.recover_locked()
            .map_err(|_| SemanticCapabilityDisabledV1::StorageFailure)?;
        let inventory = self
            .load_inventory_locked()
            .map_err(|_| SemanticCapabilityDisabledV1::StorageFailure)?;
        let record = inventory
            .records
            .get(&digest.to_string())
            .ok_or(SemanticCapabilityDisabledV1::MissingArtifact)?;
        match record.state {
            ArtifactInventoryStateV1::Installed | ArtifactInventoryStateV1::RetainedForRollback => {
            }
            ArtifactInventoryStateV1::Revoked => {
                return Err(SemanticCapabilityDisabledV1::RevokedArtifact);
            }
            ArtifactInventoryStateV1::Quarantined => {
                return Err(SemanticCapabilityDisabledV1::QuarantinedArtifact);
            }
            ArtifactInventoryStateV1::Staged | ArtifactInventoryStateV1::Verified => {
                return Err(SemanticCapabilityDisabledV1::MissingArtifact);
            }
        }
        if record.artifact_digest != *digest
            || *digest != manifest.signed_identity_digest()
            || record.manifest_digest != manifest.canonical_digest()
            || record.trust_binding != binding
            || record.members != manifest.payload.members
        {
            return Err(SemanticCapabilityDisabledV1::IdentityMismatch);
        }
        self.verify_artifact_record(record)
            .map_err(|_| SemanticCapabilityDisabledV1::CorruptArtifact)?;
        check_compatibility(&manifest.payload.runtime, env)?;
        check_resource_ceiling(&manifest.payload.resource_ceiling, env)?;
        Ok(AdmittedArtifactV1 {
            artifact_digest: digest.clone(),
            manifest_digest: manifest.canonical_digest(),
            manifest: manifest.clone(),
        })
    }

    /// Garbage-collect unreferenced artifacts past the grace window.
    /// `RetainedForRollback`, `Revoked`, and `Installed` records are never
    /// collected here; each removal appends one receipt to
    /// `receipts/gc.jsonl`.
    pub fn gc(&self, now_unix: u64) -> Result<Vec<GcReceiptV1>, ArtifactImportErrorV1> {
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        let mut inventory = self.load_inventory_locked()?;
        let records: Vec<ArtifactInventoryRecordV1> = inventory
            .records
            .values()
            .filter(|r| {
                let collectible_state = matches!(
                    r.state,
                    ArtifactInventoryStateV1::Verified | ArtifactInventoryStateV1::Quarantined
                );
                collectible_state
                    && now_unix.saturating_sub(r.recorded_at_unix) >= self.retention.grace_seconds
            })
            .cloned()
            .collect();
        if records.is_empty() {
            return Ok(Vec::new());
        }
        self.write_recovery_locked(&RecoveryJournalV1 {
            schema: RECOVERY_SCHEMA_V1.to_string(),
            action: RecoveryActionV1::Gc {
                recorded_at_unix: now_unix,
                records: records.clone(),
            },
        })?;
        for record in &records {
            self.remove_artifact_record(record)?;
            inventory
                .records
                .remove(&record.artifact_digest.to_string());
        }
        self.save_inventory_locked(&inventory)?;
        let receipts: Vec<GcReceiptV1> = records
            .into_iter()
            .map(|record| GcReceiptV1 {
                artifact_digest: record.artifact_digest,
                removed_at_unix: now_unix,
                prior_state: record.state,
            })
            .collect();
        self.append_receipts_locked(&receipts)?;
        self.clear_recovery_locked()?;
        Ok(receipts)
    }

    fn record_for(
        &self,
        manifest: &ModelArtifactManifestV1,
        trust_binding: ArtifactTrustBindingV1,
        state: ArtifactInventoryStateV1,
        recorded_at_unix: u64,
        quarantine_reason: Option<QuarantineReasonV1>,
    ) -> ArtifactInventoryRecordV1 {
        ArtifactInventoryRecordV1 {
            artifact_digest: manifest.signed_identity_digest(),
            manifest_digest: manifest.canonical_digest(),
            trust_binding,
            members: manifest.payload.members.clone(),
            state,
            recorded_at_unix,
            quarantine_reason,
        }
    }

    fn ensure_session_dir(&self, session: &ImportSession) -> Result<(), ArtifactImportErrorV1> {
        if self.staging_dir_for(&session.staging_id)? != session.staging_path {
            return Err(ArtifactImportErrorV1::UnsafeStagingHandle);
        }
        session
            .staging_dir
            .dir_metadata()
            .map_err(|_| ArtifactImportErrorV1::UnsafeStorePath)?;
        session
            .members_dir
            .dir_metadata()
            .map_err(|_| ArtifactImportErrorV1::UnsafeStorePath)?;
        Ok(())
    }

    fn ensure_session_active_locked(
        &self,
        session: &ImportSession,
    ) -> Result<(), ArtifactImportErrorV1> {
        let inventory = self.load_inventory_locked()?;
        let state = inventory
            .records
            .get(&session.meta.signed_manifest_digest.to_string())
            .map(|record| record.state);
        if matches!(
            state,
            Some(ArtifactInventoryStateV1::Quarantined | ArtifactInventoryStateV1::Revoked)
        ) {
            return Err(ArtifactImportErrorV1::StagingUnavailable);
        }
        Ok(())
    }

    fn staging_meta_matches(
        &self,
        meta: &StagingMetaV1,
        manifest: &ModelArtifactManifestV1,
    ) -> bool {
        meta.schema == STAGING_SCHEMA_V1
            && meta.manifest == *manifest
            && meta.signed_manifest_digest == manifest.signed_identity_digest()
            && meta
                .members
                .iter()
                .map(|member| &member.member)
                .eq(manifest.payload.members.iter())
    }

    fn staging_member_lengths_match(
        &self,
        session: &ImportSession,
    ) -> Result<bool, ArtifactImportErrorV1> {
        self.ensure_session_dir(session)?;
        for staged in &session.meta.members {
            let file = match open_cap_file(
                &session.members_dir,
                member_file_name(staged.member.role),
                true,
                false,
                false,
                false,
                false,
            ) {
                Ok(file) => file,
                Err(ArtifactImportErrorV1::StagingUnavailable) => return Ok(false),
                Err(error) => return Err(error),
            };
            let metadata = file
                .metadata()
                .map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
            if metadata.len() != staged.bytes_written
                || staged.bytes_written > staged.member.byte_length
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn write_recovery_locked(
        &self,
        journal: &RecoveryJournalV1,
    ) -> Result<(), ArtifactImportErrorV1> {
        let bytes =
            serde_json::to_vec(journal).map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
        atomic_write_cap_file(
            &self.root_dir,
            &self.root,
            ".artifact-store-recovery.json",
            &bytes,
        )
    }

    fn clear_recovery_locked(&self) -> Result<(), ArtifactImportErrorV1> {
        remove_cap_file_if_exists(&self.root_dir, ".artifact-store-recovery.json")?;
        sync_cap_dir(&self.root_dir)?;
        Ok(())
    }

    fn recover_locked(&self) -> Result<(), ArtifactImportErrorV1> {
        if let Some(bytes) =
            read_optional_cap_file(&self.root_dir, ".artifact-store-recovery.json")?
        {
            let journal: RecoveryJournalV1 = serde_json::from_slice(&bytes)
                .map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
            if journal.schema != RECOVERY_SCHEMA_V1 {
                return Err(ArtifactImportErrorV1::StorageFailure);
            }
            match journal.action {
                RecoveryActionV1::Install { record, staging_id } => {
                    self.recover_install_locked(record, &staging_id)?;
                }
                RecoveryActionV1::Gc {
                    recorded_at_unix,
                    records,
                } => {
                    self.recover_gc_locked(records, recorded_at_unix)?;
                }
            }
            self.clear_recovery_locked()?;
        }
        self.recover_staged_imports_locked()
    }

    fn recover_install_locked(
        &self,
        record: ArtifactInventoryRecordV1,
        staging_id: &str,
    ) -> Result<(), ArtifactImportErrorV1> {
        if !is_valid_staging_id(staging_id) {
            return Err(ArtifactImportErrorV1::UnsafeStagingHandle);
        }
        let staging_exists = self.staging_dir.open_dir_nofollow(staging_id).is_ok();
        match self
            .artifacts_dir
            .symlink_metadata(record.artifact_digest.as_str())
        {
            Ok(_) => {
                self.verify_artifact_record(&record)?;
                let mut installed = record;
                installed.state = ArtifactInventoryStateV1::Installed;
                installed.quarantine_reason = None;
                let mut inventory = self.load_inventory_locked()?;
                inventory
                    .records
                    .insert(installed.artifact_digest.to_string(), installed);
                self.save_inventory_locked(&inventory)?;
                if staging_exists {
                    self.remove_staging_dir_path(staging_id)?;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut inventory = self.load_inventory_locked()?;
                if staging_exists {
                    inventory
                        .records
                        .insert(record.artifact_digest.to_string(), record);
                } else {
                    let mut quarantined = record;
                    quarantined.state = ArtifactInventoryStateV1::Quarantined;
                    quarantined.quarantine_reason = Some(QuarantineReasonV1::RecoveryFailure);
                    inventory
                        .records
                        .insert(quarantined.artifact_digest.to_string(), quarantined);
                }
                self.save_inventory_locked(&inventory)?;
            }
            Err(_) => return Err(ArtifactImportErrorV1::StorageFailure),
        }
        Ok(())
    }

    fn recover_gc_locked(
        &self,
        records: Vec<ArtifactInventoryRecordV1>,
        recorded_at_unix: u64,
    ) -> Result<(), ArtifactImportErrorV1> {
        let mut inventory = self.load_inventory_locked()?;
        for record in &records {
            self.remove_artifact_record(record)?;
            inventory
                .records
                .remove(&record.artifact_digest.to_string());
        }
        self.save_inventory_locked(&inventory)?;
        let receipts = records
            .into_iter()
            .map(|record| GcReceiptV1 {
                artifact_digest: record.artifact_digest,
                removed_at_unix: recorded_at_unix,
                prior_state: record.state,
            })
            .collect::<Vec<_>>();
        self.append_receipts_locked(&receipts)
    }

    fn recover_staged_imports_locked(&self) -> Result<(), ArtifactImportErrorV1> {
        self.recover_staged_ids_locked(self.staged_ids_locked()?)
    }

    fn staged_ids_locked(&self) -> Result<Vec<String>, ArtifactImportErrorV1> {
        let entries = self
            .staging_dir
            .entries()
            .map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
        let mut staging_ids = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
            let file_type = entry
                .file_type()
                .map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
            if !file_type.is_dir() || file_type.is_symlink() {
                continue;
            }
            let Some(staging_id) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !is_valid_staging_id(&staging_id) {
                continue;
            }
            staging_ids.push(staging_id);
        }
        Ok(staging_ids)
    }

    fn recover_staged_ids_locked(
        &self,
        staging_ids: Vec<String>,
    ) -> Result<(), ArtifactImportErrorV1> {
        for staging_id in staging_ids {
            let staging_dir = match self.staging_dir.open_dir_nofollow(&staging_id) {
                Ok(dir) => dir,
                Err(_) => continue,
            };
            let members_dir = staging_dir.open_dir_nofollow("members").ok();
            let meta = match read_staging_meta(&staging_dir) {
                Ok(meta) if meta.schema == STAGING_SCHEMA_V1 => meta,
                Ok(_) | Err(_) => continue,
            };
            let binding = match self.verify_manifest_binding(&meta.manifest, meta.verified_at_unix)
            {
                Ok(binding) => binding,
                Err(_) => continue,
            };
            let mut record = self.record_for(
                &meta.manifest,
                binding,
                ArtifactInventoryStateV1::Staged,
                meta.verified_at_unix,
                None,
            );
            let mut inventory = self.load_inventory_locked()?;
            let existing_state = inventory
                .records
                .get(&record.artifact_digest.to_string())
                .map(|record| record.state);
            if matches!(
                existing_state,
                Some(ArtifactInventoryStateV1::Quarantined | ArtifactInventoryStateV1::Revoked)
            ) {
                drop(members_dir);
                drop(staging_dir);
                self.remove_staging_dir_path(&staging_id)?;
            } else if self
                .artifacts_dir
                .symlink_metadata(record.artifact_digest.as_str())
                .is_ok()
                && self.verify_artifact_record(&record).is_ok()
            {
                record.state = ArtifactInventoryStateV1::Installed;
                inventory
                    .records
                    .insert(record.artifact_digest.to_string(), record);
                self.save_inventory_locked(&inventory)?;
                drop(members_dir);
                drop(staging_dir);
                self.remove_staging_dir_path(&staging_id)?;
            } else {
                if members_dir.is_none() {
                    record.state = ArtifactInventoryStateV1::Quarantined;
                    record.quarantine_reason = Some(QuarantineReasonV1::RecoveryFailure);
                }
                inventory
                    .records
                    .insert(record.artifact_digest.to_string(), record);
                self.save_inventory_locked(&inventory)?;
            }
        }
        Ok(())
    }

    fn verify_artifact_record(
        &self,
        record: &ArtifactInventoryRecordV1,
    ) -> Result<(), ArtifactImportErrorV1> {
        let directory = self
            .artifacts_dir
            .open_dir_nofollow(record.artifact_digest.as_str())
            .map_err(|_| ArtifactImportErrorV1::UnsafeStorePath)?;
        for member in &record.members {
            let file = open_cap_file(
                &directory,
                member_file_name(member.role),
                true,
                false,
                false,
                false,
                false,
            )?;
            let metadata = file
                .metadata()
                .map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
            if metadata.len() != member.byte_length || sha256_open_file(file)? != member.digest {
                return Err(ArtifactImportErrorV1::DigestMismatch);
            }
        }
        Ok(())
    }

    fn remove_artifact_record(
        &self,
        record: &ArtifactInventoryRecordV1,
    ) -> Result<(), ArtifactImportErrorV1> {
        match self
            .artifacts_dir
            .symlink_metadata(record.artifact_digest.as_str())
        {
            Ok(metadata) if metadata.is_dir() => self
                .artifacts_dir
                .remove_dir_all(record.artifact_digest.as_str())?,
            Ok(_) => return Err(ArtifactImportErrorV1::UnsafeStorePath),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(ArtifactImportErrorV1::StorageFailure),
        }
        sync_cap_dir(&self.artifacts_dir)?;
        Ok(())
    }

    fn remove_staging_dir_path(&self, staging_id: &str) -> Result<(), ArtifactImportErrorV1> {
        if !is_valid_staging_id(staging_id) {
            return Err(ArtifactImportErrorV1::UnsafeStagingHandle);
        }
        match self.staging_dir.symlink_metadata(staging_id) {
            Ok(metadata) if metadata.is_dir() => self.staging_dir.remove_dir_all(staging_id)?,
            Ok(_) => self.staging_dir.remove_file_or_symlink(staging_id)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(ArtifactImportErrorV1::StorageFailure),
        }
        sync_cap_dir(&self.staging_dir)?;
        Ok(())
    }

    fn append_receipts_locked(
        &self,
        receipts: &[GcReceiptV1],
    ) -> Result<(), ArtifactImportErrorV1> {
        if receipts.is_empty() {
            return Ok(());
        }
        let mut durable = read_receipt_frames(
            read_optional_cap_file(&self.receipts_dir, "gc.jsonl")?
                .as_deref()
                .unwrap_or_default(),
        )?;
        for receipt in receipts {
            if !durable.contains(receipt) {
                durable.push(receipt.clone());
            }
        }
        let mut bytes = Vec::new();
        for receipt in &durable {
            serde_json::to_writer(&mut bytes, receipt)
                .map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
            bytes.push(b'\n');
        }
        atomic_write_cap_file(
            &self.receipts_dir,
            &self.receipts_root(),
            "gc.jsonl",
            &bytes,
        )
    }
}

fn check_compatibility(
    required: &super::manifest::RuntimeCompatibilityV1,
    env: &RuntimeEnvironmentV1,
) -> Result<(), SemanticCapabilityDisabledV1> {
    if required.runtime != env.runtime || required.build_revision != env.build_revision {
        return Err(SemanticCapabilityDisabledV1::IncompatibleRuntime);
    }
    if !required
        .platforms
        .iter()
        .any(|p| p.os == env.os && p.arch == env.arch)
    {
        return Err(SemanticCapabilityDisabledV1::IncompatiblePlatform);
    }
    Ok(())
}

fn check_resource_ceiling(
    ceiling: &ResourceCeilingV1,
    env: &RuntimeEnvironmentV1,
) -> Result<(), SemanticCapabilityDisabledV1> {
    if env.available_resident_bytes < ceiling.max_resident_bytes {
        return Err(SemanticCapabilityDisabledV1::ResourceCeilingExceeded);
    }
    if env.available_threads < ceiling.max_threads {
        return Err(SemanticCapabilityDisabledV1::ResourceCeilingExceeded);
    }
    Ok(())
}

fn sha256_open_file(mut file: impl Read) -> Result<Sha256DigestHex, ArtifactImportErrorV1> {
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Sha256DigestHex::new(hex::encode(hasher.finalize()))
        .map_err(|_| ArtifactImportErrorV1::StorageFailure)
}

fn write_staging_meta(
    dir: &Dir,
    ambient_path: &Path,
    meta: &StagingMetaV1,
) -> Result<(), ArtifactImportErrorV1> {
    let bytes = serde_json::to_vec(meta).map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
    atomic_write_cap_file(dir, ambient_path, "import.meta.json", &bytes)
}

fn read_staging_meta(dir: &Dir) -> Result<StagingMetaV1, ArtifactImportErrorV1> {
    let bytes = read_optional_cap_file(dir, "import.meta.json")?
        .ok_or(ArtifactImportErrorV1::StagingUnavailable)?;
    serde_json::from_slice(&bytes).map_err(|_| ArtifactImportErrorV1::StorageFailure)
}

fn read_receipt_frames(bytes: &[u8]) -> Result<Vec<GcReceiptV1>, ArtifactImportErrorV1> {
    let mut receipts = Vec::new();
    for frame in bytes.split_inclusive(|byte| *byte == b'\n') {
        if !frame.ends_with(b"\n") {
            break;
        }
        let payload = &frame[..frame.len() - 1];
        if payload.is_empty() {
            continue;
        }
        match serde_json::from_slice(payload) {
            Ok(receipt) => receipts.push(receipt),
            Err(_) => break,
        }
    }
    Ok(receipts)
}

fn open_cap_file(
    dir: &Dir,
    name: &str,
    read: bool,
    write: bool,
    create: bool,
    create_new: bool,
    append: bool,
) -> Result<CapFile, ArtifactImportErrorV1> {
    if !is_component(name) {
        return Err(ArtifactImportErrorV1::UnsafeStorePath);
    }
    let mut options = CapOpenOptions::new();
    options
        .read(read)
        .write(write)
        .create(create)
        .create_new(create_new)
        .append(append);
    #[cfg(unix)]
    options.mode(0o600);
    options.follow(FollowSymlinks::No);
    if write {
        options.sync(true);
    }
    dir.open_with(name, &options)
        .map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => ArtifactImportErrorV1::StagingUnavailable,
            _ => ArtifactImportErrorV1::UnsafeStorePath,
        })
}

fn read_optional_cap_file(dir: &Dir, name: &str) -> Result<Option<Vec<u8>>, ArtifactImportErrorV1> {
    match open_cap_file(dir, name, true, false, false, false, false) {
        Ok(mut file) => {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
            Ok(Some(bytes))
        }
        Err(ArtifactImportErrorV1::StagingUnavailable) => Ok(None),
        Err(error) => Err(error),
    }
}

fn atomic_write_cap_file(
    dir: &Dir,
    ambient_parent: &Path,
    name: &str,
    bytes: &[u8],
) -> Result<(), ArtifactImportErrorV1> {
    if !is_component(name) {
        return Err(ArtifactImportErrorV1::UnsafeStorePath);
    }
    #[cfg(windows)]
    {
        // `Dir` holds the parent without FILE_SHARE_DELETE, so the maintained
        // fsys wrapper can safely perform replace-existing + write-through by
        // ambient path without a parent replacement/reparse race.
        dir.dir_metadata()
            .map_err(|_| ArtifactImportErrorV1::UnsafeStorePath)?;
        fsys::quick::write(ambient_parent.join(name), bytes)
            .map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
        return sync_cap_dir(dir);
    }
    #[cfg(not(windows))]
    {
        let temporary = format!(".{name}.{}.tmp", random_staging_id()?);
        {
            let mut file = open_cap_file(dir, &temporary, false, true, false, true, false)?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        replace_cap_file(dir, ambient_parent, &temporary, name)?;
        sync_cap_dir(dir)
    }
}

#[cfg(not(windows))]
fn replace_cap_file(
    dir: &Dir,
    _ambient_parent: &Path,
    temporary: &str,
    destination: &str,
) -> Result<(), ArtifactImportErrorV1> {
    dir.rename(temporary, dir, destination)
        .map_err(|_| ArtifactImportErrorV1::StorageFailure)
}

fn remove_cap_file_if_exists(dir: &Dir, name: &str) -> Result<(), ArtifactImportErrorV1> {
    match dir.symlink_metadata(name) {
        Ok(metadata) if metadata.is_file() => dir
            .remove_file(name)
            .map_err(|_| ArtifactImportErrorV1::StorageFailure),
        Ok(_) => Err(ArtifactImportErrorV1::UnsafeStorePath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(ArtifactImportErrorV1::StorageFailure),
    }
}

fn sync_cap_dir(dir: &Dir) -> Result<(), ArtifactImportErrorV1> {
    #[cfg(windows)]
    {
        // MoveFileExW WRITE_THROUGH is the Windows namespace durability
        // authority; directory FlushFileBuffers is not supported reliably.
        dir.dir_metadata()
            .map(|_| ())
            .map_err(|_| ArtifactImportErrorV1::StorageFailure)
    }
    #[cfg(not(windows))]
    {
        let mut options = CapOpenOptions::new();
        options.read(true).maybe_dir(true);
        dir.open_with(".", &options)
            .and_then(|file| file.sync_all())
            .map_err(|_| ArtifactImportErrorV1::StorageFailure)
    }
}

fn is_component(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\\')
}

fn open_root_from_trusted_parent(root: &Path) -> Result<Dir, ArtifactImportErrorV1> {
    let root_name = root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| is_component(name))
        .ok_or(ArtifactImportErrorV1::UnsafeStorePath)?;
    let trusted_parent = root
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = Dir::open_ambient_dir(trusted_parent, ambient_authority())
        .map_err(|_| ArtifactImportErrorV1::UnsafeStorePath)?;
    open_or_create_component_dir(&parent, root_name)
}

fn open_or_create_component_dir(parent: &Dir, name: &str) -> Result<Dir, ArtifactImportErrorV1> {
    if !is_component(name) {
        return Err(ArtifactImportErrorV1::UnsafeStorePath);
    }
    match parent.open_dir_nofollow(name) {
        Ok(dir) => Ok(dir),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = DirBuilder::new();
            #[cfg(unix)]
            builder.mode(0o700);
            parent
                .create_dir_with(name, &builder)
                .map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
            parent
                .open_dir_nofollow(name)
                .map_err(|_| ArtifactImportErrorV1::UnsafeStorePath)
        }
        Err(_) => Err(ArtifactImportErrorV1::UnsafeStorePath),
    }
}

fn member_file_name(role: ArtifactMemberRoleV1) -> &'static str {
    match role {
        ArtifactMemberRoleV1::Model => "model",
        ArtifactMemberRoleV1::Tokenizer => "tokenizer",
        ArtifactMemberRoleV1::Config => "config",
        ArtifactMemberRoleV1::QueryInstruction => "query-instruction",
        ArtifactMemberRoleV1::DocumentInstruction => "document-instruction",
    }
}

fn is_valid_staging_id(staging_id: &str) -> bool {
    staging_id.len() == 32
        && staging_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn random_staging_id() -> Result<String, ArtifactImportErrorV1> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
    Ok(hex::encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::super::manifest::*;
    use super::super::trust_roots::test_support::*;
    use super::super::trust_roots::*;
    use super::*;
    use std::sync::{Arc, mpsc};
    use std::thread;
    use std::time::Duration;

    const NOW: u64 = 1_500;

    fn key_bytes() -> [u8; 32] {
        [1u8; 32]
    }

    fn trust_set() -> TrustRootSetV1 {
        TrustRootSetV1 {
            release_roots: vec![TrustRootV1 {
                root_id: "release-1".to_string(),
                public_key: Ed25519PublicKeyHex::new(hex::encode(key_bytes())).unwrap(),
                not_before_unix: 1_000,
                not_after_unix: 2_000,
                rotation_epoch: 1,
                status: TrustRootStatusV1::Active,
            }],
            local_roots: vec![],
            revocations: vec![],
        }
    }

    fn model_bytes() -> Vec<u8> {
        b"deterministic fake model weights".to_vec()
    }

    fn member_bytes(role: ArtifactMemberRoleV1, model: &[u8]) -> &[u8] {
        match role {
            ArtifactMemberRoleV1::Model => model,
            ArtifactMemberRoleV1::Tokenizer => b"tokenizer",
            ArtifactMemberRoleV1::Config => b"config",
            ArtifactMemberRoleV1::QueryInstruction | ArtifactMemberRoleV1::DocumentInstruction => {
                unreachable!()
            }
        }
    }

    fn manifest_for(bytes: &[u8]) -> ModelArtifactManifestV1 {
        let payload = ManifestSignedPayloadV1 {
            schema: MODEL_ARTIFACT_MANIFEST_SCHEMA_V1.to_string(),
            artifact_id: "test-embed".to_string(),
            signing_root_id: "release-1".to_string(),
            signing_root_epoch: 1,
            profile_kind: ArtifactProfileKindV1::Embedding,
            spdx_license: "MIT".to_string(),
            model_member: ArtifactMemberPinV1 {
                digest: Sha256DigestHex::of_bytes(bytes),
                byte_length: bytes.len() as u64,
            },
            tokenizer_digest: Sha256DigestHex::of_bytes(b"tokenizer"),
            config_digest: Sha256DigestHex::of_bytes(b"config"),
            query_instruction_digest: None,
            document_instruction_digest: None,
            members: vec![
                ArtifactPackageMemberV1 {
                    role: ArtifactMemberRoleV1::Model,
                    path: "model.onnx".to_string(),
                    digest: Sha256DigestHex::of_bytes(bytes),
                    byte_length: bytes.len() as u64,
                },
                ArtifactPackageMemberV1 {
                    role: ArtifactMemberRoleV1::Tokenizer,
                    path: "tokenizer.json".to_string(),
                    digest: Sha256DigestHex::of_bytes(b"tokenizer"),
                    byte_length: b"tokenizer".len() as u64,
                },
                ArtifactPackageMemberV1 {
                    role: ArtifactMemberRoleV1::Config,
                    path: "config.json".to_string(),
                    digest: Sha256DigestHex::of_bytes(b"config"),
                    byte_length: b"config".len() as u64,
                },
            ],
            dimensions: 384,
            metric: SemanticMetricV1::Cosine,
            normalization: EmbeddingNormalizationV1::L2,
            pooling: EmbeddingPoolingV1::Mean,
            truncation: TruncationPolicyV1 {
                side: TruncationSideV1::Right,
                max_length: 512,
            },
            precision: EmbeddingPrecisionV1::Fp32,
            runtime: RuntimeCompatibilityV1 {
                runtime: "fastembed-ort".to_string(),
                build_revision: "rev-1".to_string(),
                platforms: vec![PlatformTargetV1 {
                    os: "linux".to_string(),
                    arch: "x86_64".to_string(),
                }],
            },
            device: DeviceClassV1::Cpu,
            resource_ceiling: ResourceCeilingV1 {
                max_model_bytes: 1_000_000,
                max_tokenizer_bytes: 100_000,
                max_resident_bytes: 1_000_000_000,
                max_threads: 4,
                max_batch_size: 32,
                max_sequence_length: 512,
                load_deadline_ms: 30_000,
            },
            upstream: UpstreamSourceV1 {
                name: "test/model".to_string(),
                version: "1".to_string(),
                revision: "r1".to_string(),
            },
        };
        let mut manifest = ModelArtifactManifestV1 {
            payload,
            signature: DetachedSignatureV1 {
                algorithm: SignatureAlgorithmV1::Ed25519,
                trust_root_id: "release-1".to_string(),
                trust_root_epoch: 1,
                signature: Ed25519SignatureHex::new(hex::encode([0u8; 64])).unwrap(),
            },
        };
        let signature = fake_sign(&key_bytes(), &manifest.canonical_bytes());
        manifest.signature.signature = Ed25519SignatureHex::new(hex::encode(signature)).unwrap();
        manifest
    }

    fn env() -> RuntimeEnvironmentV1 {
        RuntimeEnvironmentV1 {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            runtime: "fastembed-ort".to_string(),
            build_revision: "rev-1".to_string(),
            available_resident_bytes: 2_000_000_000,
            available_threads: 8,
        }
    }

    fn store() -> (tempfile::TempDir, ModelArtifactStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelArtifactStore::open(
            dir.path().join("store"),
            trust_set(),
            Arc::new(FakeEd25519Verifier),
            RetentionPolicyV1 { grace_seconds: 100 },
        )
        .unwrap();
        (dir, store)
    }

    fn import_ok(
        store: &ModelArtifactStore,
        bytes: &[u8],
    ) -> (ModelArtifactManifestV1, Sha256DigestHex) {
        let manifest = manifest_for(bytes);
        let mut session = store.begin_import(&manifest, NOW).unwrap();
        store.stage_chunk(&mut session, bytes, NOW).unwrap();
        for role in [
            ArtifactMemberRoleV1::Tokenizer,
            ArtifactMemberRoleV1::Config,
        ] {
            store
                .stage_member_chunk(&mut session, role, member_bytes(role, bytes), NOW)
                .unwrap();
        }
        let record = store.finalize_import(session, &manifest, NOW).unwrap();
        assert_eq!(record.state, ArtifactInventoryStateV1::Installed);
        (manifest, record.artifact_digest)
    }

    #[test]
    fn valid_signature_import_places_atomically_and_admits() {
        let (_dir, store) = store();
        let (manifest, digest) = import_ok(&store, &model_bytes());
        let admitted = store
            .admit_for_runtime(&digest, &manifest, &env(), NOW)
            .unwrap();
        assert_eq!(admitted.artifact_digest(), &digest);
        assert!(store.artifact_path(&digest).exists());
        // Staging drained; layout is digest-addressed.
        assert_eq!(
            std::fs::read_dir(store.root.join("staging"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(
            std::fs::read(store.artifact_path(&digest)).unwrap(),
            model_bytes()
        );
    }

    #[test]
    fn runtime_admission_rejects_tampered_inventory_record_digest() {
        let (_dir, store) = store();
        let (manifest, digest) = import_ok(&store, &model_bytes());
        let mut inventory = store.inventory().unwrap();
        inventory
            .records
            .get_mut(digest.as_str())
            .unwrap()
            .artifact_digest = Sha256DigestHex::of_bytes(b"tampered-record-digest");
        store.save_inventory(&inventory).unwrap();

        assert_eq!(
            store
                .admit_for_runtime(&digest, &manifest, &env(), NOW)
                .unwrap_err(),
            SemanticCapabilityDisabledV1::IdentityMismatch
        );
    }

    #[test]
    fn runtime_admission_rejects_tampered_inventory_map_key() {
        let (_dir, store) = store();
        let (manifest, digest) = import_ok(&store, &model_bytes());
        let tampered_key = Sha256DigestHex::of_bytes(b"tampered-map-key");
        let mut inventory = store.inventory().unwrap();
        let record = inventory.records.remove(digest.as_str()).unwrap();
        inventory.records.insert(tampered_key.to_string(), record);
        store.save_inventory(&inventory).unwrap();

        assert_eq!(
            store
                .admit_for_runtime(&tampered_key, &manifest, &env(), NOW)
                .unwrap_err(),
            SemanticCapabilityDisabledV1::IdentityMismatch
        );
    }

    #[test]
    fn invalid_signature_is_rejected_before_any_bytes_are_staged() {
        let (_dir, store) = store();
        let mut manifest = manifest_for(&model_bytes());
        let wrong_key_sig = fake_sign(&[9u8; 32], &manifest.canonical_bytes());
        manifest.signature.signature =
            Ed25519SignatureHex::new(hex::encode(wrong_key_sig)).unwrap();
        assert_eq!(
            store.begin_import(&manifest, NOW).unwrap_err(),
            ArtifactImportErrorV1::SignatureInvalid
        );
        assert_eq!(
            std::fs::read_dir(store.root.join("staging"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn unknown_and_revoked_trust_roots_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut roots = trust_set();
        let store = ModelArtifactStore::open(
            dir.path().join("store"),
            roots.clone(),
            Arc::new(FakeEd25519Verifier),
            RetentionPolicyV1 { grace_seconds: 100 },
        )
        .unwrap();
        let mut unknown = manifest_for(&model_bytes());
        unknown.signature.trust_root_id = "no-such-root".to_string();
        assert!(matches!(
            store.begin_import(&unknown, NOW).unwrap_err(),
            ArtifactImportErrorV1::ManifestRejected | ArtifactImportErrorV1::TrustRejected
        ));

        roots.revocations.push(RevocationRecordV1 {
            root_id: "release-1".to_string(),
            revoked_at_unix: 1_200,
            reason: "rotation drill".to_string(),
        });
        let revoked_store = ModelArtifactStore::open(
            dir.path().join("store2"),
            roots,
            Arc::new(FakeEd25519Verifier),
            RetentionPolicyV1 { grace_seconds: 100 },
        )
        .unwrap();
        let manifest = manifest_for(&model_bytes());
        assert!(matches!(
            revoked_store.begin_import(&manifest, NOW).unwrap_err(),
            ArtifactImportErrorV1::TrustRejected
        ));
    }

    #[test]
    fn corrupted_bytes_are_rejected_at_finalize_and_quarantined() {
        let (_dir, store) = store();
        let manifest = manifest_for(&model_bytes());
        let mut session = store.begin_import(&manifest, NOW).unwrap();
        // Same length, different bytes -> digest mismatch.
        let mut corrupted = model_bytes();
        corrupted[0] ^= 0xFF;
        store.stage_chunk(&mut session, &corrupted, NOW).unwrap();
        assert!(matches!(
            store.finalize_import(session, &manifest, NOW).unwrap_err(),
            ArtifactImportErrorV1::DigestMismatch
        ));
        let inventory = store.inventory().unwrap();
        let record = inventory
            .records
            .get(&manifest.signed_identity_digest().to_string())
            .unwrap();
        assert_eq!(record.state, ArtifactInventoryStateV1::Quarantined);
        assert!(
            !store
                .artifact_dir(&manifest.signed_identity_digest())
                .exists()
        );
    }

    #[test]
    fn every_signed_package_member_is_verified_before_installation() {
        let (_dir, store) = store();
        let bytes = model_bytes();
        let manifest = manifest_for(&bytes);
        let mut session = store.begin_import(&manifest, NOW).unwrap();
        store.stage_chunk(&mut session, &bytes, NOW).unwrap();
        store
            .stage_member_chunk(
                &mut session,
                ArtifactMemberRoleV1::Tokenizer,
                member_bytes(ArtifactMemberRoleV1::Tokenizer, &bytes),
                NOW,
            )
            .unwrap();
        store
            .stage_member_chunk(&mut session, ArtifactMemberRoleV1::Config, b"confix", NOW)
            .unwrap();

        assert_eq!(
            store.finalize_import(session, &manifest, NOW).unwrap_err(),
            ArtifactImportErrorV1::DigestMismatch
        );
        let inventory = store.inventory().unwrap();
        assert_eq!(
            inventory
                .records
                .get(&manifest.signed_identity_digest().to_string())
                .unwrap()
                .state,
            ArtifactInventoryStateV1::Quarantined
        );
    }

    #[test]
    fn wrong_length_and_size_expansion_are_rejected() {
        let (_dir, store) = store();
        let manifest = manifest_for(&model_bytes());

        // Short write -> length mismatch at finalize.
        let mut short = store.begin_import(&manifest, NOW).unwrap();
        store
            .stage_chunk(&mut short, &model_bytes()[..4], NOW)
            .unwrap();
        assert!(matches!(
            store.finalize_import(short, &manifest, NOW).unwrap_err(),
            ArtifactImportErrorV1::LengthMismatch
        ));

        // Over-long write -> size expansion rejected at stage time.
        let over_bytes = b"separate model for expansion".to_vec();
        let over_manifest = manifest_for(&over_bytes);
        let mut over = store.begin_import(&over_manifest, NOW).unwrap();
        let mut too_much = over_bytes;
        too_much.push(0);
        assert!(matches!(
            store.stage_chunk(&mut over, &too_much, NOW).unwrap_err(),
            ArtifactImportErrorV1::SizeExpansionBeyondDeclared
        ));
    }

    #[test]
    fn partial_write_resumes_and_places_atomically() {
        let (_dir, store) = store();
        let bytes = model_bytes();
        let manifest = manifest_for(&bytes);
        let mut session = store.begin_import(&manifest, NOW).unwrap();
        let split = bytes.len() / 2;
        store
            .stage_chunk(&mut session, &bytes[..split], NOW)
            .unwrap();
        let staging_id = session.staging_id();
        assert_eq!(session.bytes_written(), split as u64);
        drop(session); // simulate interruption

        let mut resumed = store.resume_import(&manifest, &staging_id, NOW).unwrap();
        assert_eq!(resumed.bytes_written(), split as u64);
        store
            .stage_chunk(&mut resumed, &bytes[split..], NOW)
            .unwrap();
        for role in [
            ArtifactMemberRoleV1::Tokenizer,
            ArtifactMemberRoleV1::Config,
        ] {
            store
                .stage_member_chunk(&mut resumed, role, member_bytes(role, &bytes), NOW)
                .unwrap();
        }
        let record = store.finalize_import(resumed, &manifest, NOW).unwrap();
        assert_eq!(record.state, ArtifactInventoryStateV1::Installed);
        assert!(store.artifact_path(&record.artifact_digest).exists());
    }

    #[test]
    fn resume_with_mismatched_manifest_discards_staging() {
        let (_dir, store) = store();
        let bytes = model_bytes();
        let manifest = manifest_for(&bytes);
        let mut session = store.begin_import(&manifest, NOW).unwrap();
        store.stage_chunk(&mut session, &bytes[..4], NOW).unwrap();
        let staging_id = session.staging_id();
        drop(session);

        let other = manifest_for(b"different model bytes");
        assert_eq!(
            store.resume_import(&other, &staging_id, NOW).unwrap_err(),
            ArtifactImportErrorV1::ResumeIdentityMismatch
        );
        assert!(!store.root.join("staging").join(&staging_id).exists());
    }

    #[test]
    fn resume_confines_opaque_staging_handles_without_leaking_them() {
        let (_dir, store) = store();
        let manifest = manifest_for(&model_bytes());
        let session = store.begin_import(&manifest, NOW).unwrap();
        let staging_id = session.staging_id();
        drop(session);

        let escaped = store.root.join("escaped-staging");
        std::fs::rename(store.root.join("staging").join(&staging_id), &escaped).unwrap();

        let traversal = "../escaped-staging";
        let error = store
            .resume_import(&manifest, traversal, NOW)
            .expect_err("a staging handle must not traverse outside staging");
        assert!(!error.to_string().contains(traversal));
        assert!(
            !error
                .to_string()
                .contains(&store.root.display().to_string())
        );
        assert!(
            escaped.exists(),
            "a rejected traversal must not delete data outside the staging root"
        );

        let opaque_handle = "not-a-valid-staging-handle";
        let error = store
            .resume_import(&manifest, opaque_handle, NOW)
            .expect_err("untrusted raw handle must be rejected");
        assert!(!error.to_string().contains(opaque_handle));
    }

    #[cfg(unix)]
    #[test]
    fn resume_does_not_follow_a_symlinked_staging_directory() {
        let (_dir, store) = store();
        let manifest = manifest_for(&model_bytes());
        let session = store.begin_import(&manifest, NOW).unwrap();
        let staging_id = session.staging_id();
        drop(session);

        let staging = store.root.join("staging").join(&staging_id);
        let escaped = store.root.join("escaped-staging");
        std::fs::rename(&staging, &escaped).unwrap();
        std::os::unix::fs::symlink(&escaped, &staging).unwrap();

        assert!(
            store.resume_import(&manifest, &staging_id, NOW).is_err(),
            "resuming must reject a staging path that resolves through a symlink"
        );
        assert!(escaped.exists());
    }

    #[cfg(unix)]
    #[test]
    fn recovery_reopens_staging_id_nofollow_after_enumeration_swap() {
        let (_dir, store) = store();
        let bytes = model_bytes();
        let manifest = manifest_for(&bytes);
        let mut session = store.begin_import(&manifest, NOW).unwrap();
        store.stage_chunk(&mut session, &bytes, NOW).unwrap();
        for role in [
            ArtifactMemberRoleV1::Tokenizer,
            ArtifactMemberRoleV1::Config,
        ] {
            store
                .stage_member_chunk(&mut session, role, member_bytes(role, &bytes), NOW)
                .unwrap();
        }
        let staging_id = session.staging_id();
        let enumerated = store.staged_ids_locked().unwrap();
        let digest = manifest.signed_identity_digest();
        let staging_root = store.root.join("staging");
        let original = staging_root.join(&staging_id);
        let held = staging_root.join("held-original");
        let replacement = staging_root.join("replacement");
        let members = session.staging_path.join("members");
        drop(session);
        std::fs::rename(members, store.artifact_dir(&digest)).unwrap();
        std::fs::rename(&original, &held).unwrap();
        std::fs::create_dir_all(replacement.join("members")).unwrap();
        std::fs::copy(
            held.join("import.meta.json"),
            replacement.join("import.meta.json"),
        )
        .unwrap();
        std::fs::write(replacement.join("sentinel"), b"replacement").unwrap();
        std::os::unix::fs::symlink("replacement", &original).unwrap();

        store.recover_staged_ids_locked(enumerated).unwrap();

        let inventory = store.inventory().unwrap();
        assert_eq!(
            inventory.records.get(digest.as_str()).unwrap().state,
            ArtifactInventoryStateV1::Staged
        );
        assert_eq!(
            std::fs::read(replacement.join("sentinel")).unwrap(),
            b"replacement"
        );
        assert!(held.exists());
    }

    #[cfg(unix)]
    #[test]
    fn held_staging_component_ignores_ambient_component_replacement() {
        let (_dir, store) = store();
        let held = store.root.join("staging-held");
        let outside = store.root.join("outside-staging");
        std::fs::rename(store.root.join("staging"), &held).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("sentinel"), b"outside").unwrap();
        std::os::unix::fs::symlink(&outside, store.root.join("staging")).unwrap();

        let manifest = manifest_for(&model_bytes());
        let session = store.begin_import(&manifest, NOW).unwrap();
        assert!(held.join(session.staging_id()).exists());
        assert_eq!(std::fs::read(outside.join("sentinel")).unwrap(), b"outside");
    }

    #[cfg(unix)]
    #[test]
    fn held_root_capability_ignores_ambient_root_replacement() {
        let (dir, store) = store();
        let ambient_root = dir.path().join("store");
        let held_root = dir.path().join("store-held");
        let outside_root = dir.path().join("outside-root");
        std::fs::rename(&ambient_root, &held_root).unwrap();
        std::fs::create_dir(&outside_root).unwrap();
        std::fs::write(outside_root.join("sentinel"), b"outside").unwrap();
        std::os::unix::fs::symlink(&outside_root, &ambient_root).unwrap();

        store
            .save_inventory(&ArtifactInventoryV1::default())
            .unwrap();
        assert!(held_root.join("inventory.json").exists());
        assert_eq!(
            std::fs::read(outside_root.join("sentinel")).unwrap(),
            b"outside"
        );
        assert!(!outside_root.join("inventory.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn held_import_session_ignores_ambient_session_replacement() {
        let (_dir, store) = store();
        let bytes = model_bytes();
        let manifest = manifest_for(&bytes);
        let mut session = store.begin_import(&manifest, NOW).unwrap();
        let ambient = store.root.join("staging").join(session.staging_id());
        let held = store.root.join("held-session");
        let outside = store.root.join("outside-session");
        std::fs::rename(&ambient, &held).unwrap();
        std::fs::create_dir_all(outside.join("members")).unwrap();
        std::fs::write(outside.join("members").join("model"), b"outside").unwrap();
        std::os::unix::fs::symlink(&outside, &ambient).unwrap();

        store.stage_chunk(&mut session, &bytes, NOW).unwrap();
        assert_eq!(
            std::fs::read(outside.join("members").join("model")).unwrap(),
            b"outside"
        );
        assert_eq!(
            std::fs::read(held.join("members").join("model")).unwrap(),
            bytes
        );
    }

    #[cfg(unix)]
    #[test]
    fn held_artifact_and_receipt_components_preserve_replacement_sentinels() {
        let (_dir, store) = store();
        let manifest = manifest_for(b"collectible component race");
        let record = store.record_for(
            &manifest,
            ArtifactTrustBindingV1 {
                root_id: "release-1".to_string(),
                rotation_epoch: 1,
            },
            ArtifactInventoryStateV1::Verified,
            NOW,
            None,
        );
        let digest = record.artifact_digest.clone();
        std::fs::create_dir_all(store.artifact_dir(&digest)).unwrap();
        let mut inventory = store.inventory().unwrap();
        inventory.records.insert(digest.to_string(), record);
        store.save_inventory(&inventory).unwrap();

        let held_artifacts = store.root.join("artifacts-held");
        let outside_artifacts = store.root.join("outside-artifacts");
        std::fs::rename(store.root.join("artifacts"), &held_artifacts).unwrap();
        std::fs::create_dir_all(outside_artifacts.join(digest.as_str())).unwrap();
        std::fs::write(
            outside_artifacts.join(digest.as_str()).join("sentinel"),
            b"artifact-outside",
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside_artifacts, store.root.join("artifacts")).unwrap();

        let held_receipts = store.root.join("receipts-held");
        let outside_receipts = store.root.join("outside-receipts");
        std::fs::rename(store.root.join("receipts"), &held_receipts).unwrap();
        std::fs::create_dir(&outside_receipts).unwrap();
        std::fs::write(outside_receipts.join("sentinel"), b"receipt-outside").unwrap();
        std::os::unix::fs::symlink(&outside_receipts, store.root.join("receipts")).unwrap();

        assert_eq!(store.gc(NOW + 150).unwrap().len(), 1);
        assert_eq!(
            std::fs::read(outside_artifacts.join(digest.as_str()).join("sentinel")).unwrap(),
            b"artifact-outside"
        );
        assert_eq!(
            std::fs::read(outside_receipts.join("sentinel")).unwrap(),
            b"receipt-outside"
        );
        assert!(held_receipts.join("gc.jsonl").exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_component_handles_block_namespace_replacement() {
        let (_dir, store) = store();
        let replacement = store.root.join("replacement-staging");
        std::fs::create_dir(&replacement).unwrap();
        std::fs::write(replacement.join("sentinel"), b"outside").unwrap();

        assert!(
            std::fs::rename(store.root.join("staging"), store.root.join("staging-held")).is_err(),
            "the held Windows component handle must deny replacement"
        );
        assert_eq!(
            std::fs::read(replacement.join("sentinel")).unwrap(),
            b"outside"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_inventory_replace_existing_is_write_through_and_leaves_no_temp() {
        let (_dir, store) = store();
        let first = ArtifactInventoryV1::default();
        store.save_inventory(&first).unwrap();

        let manifest = manifest_for(b"windows replacement");
        let record = store.record_for(
            &manifest,
            ArtifactTrustBindingV1 {
                root_id: "release-1".to_string(),
                rotation_epoch: 1,
            },
            ArtifactInventoryStateV1::Verified,
            NOW,
            None,
        );
        let mut second = ArtifactInventoryV1::default();
        second
            .records
            .insert(record.artifact_digest.to_string(), record);
        store.save_inventory(&second).unwrap();

        assert_eq!(store.inventory().unwrap(), second);
        assert!(std::fs::read_dir(&store.root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }

    #[test]
    fn reopening_recovers_an_install_interrupted_after_payload_rename() {
        let (dir, store) = store();
        let store_root = dir.path().join("store");
        let bytes = model_bytes();
        let manifest = manifest_for(&bytes);
        let mut session = store.begin_import(&manifest, NOW).unwrap();
        store.stage_chunk(&mut session, &bytes, NOW).unwrap();
        for role in [
            ArtifactMemberRoleV1::Tokenizer,
            ArtifactMemberRoleV1::Config,
        ] {
            store
                .stage_member_chunk(&mut session, role, member_bytes(role, &bytes), NOW)
                .unwrap();
        }
        let staging_id = session.staging_id();
        let digest = manifest.signed_identity_digest();
        let members_path = session.staging_path.join("members");
        drop(session);
        std::fs::rename(members_path, store.artifact_dir(&digest)).unwrap();
        drop(store);

        let reopened = ModelArtifactStore::open(
            store_root,
            trust_set(),
            Arc::new(FakeEd25519Verifier),
            RetentionPolicyV1 { grace_seconds: 100 },
        )
        .unwrap();
        let inventory = reopened.inventory().unwrap();
        let record = inventory
            .records
            .get(&digest.to_string())
            .expect("recovery must publish the renamed verified payload");
        assert_eq!(record.state, ArtifactInventoryStateV1::Installed);
        assert!(!reopened.root.join("staging").join(staging_id).exists());
    }

    #[test]
    fn reopening_finishes_a_serialized_gc_transaction() {
        let (dir, store) = store();
        let store_root = dir.path().join("store");
        let manifest = manifest_for(b"interrupted gc");
        let record = store.record_for(
            &manifest,
            ArtifactTrustBindingV1 {
                root_id: "release-1".to_string(),
                rotation_epoch: 1,
            },
            ArtifactInventoryStateV1::Verified,
            NOW,
            None,
        );
        let digest = record.artifact_digest.clone();
        std::fs::create_dir_all(store.artifact_dir(&digest)).unwrap();
        let mut inventory = store.inventory().unwrap();
        inventory.records.insert(digest.to_string(), record.clone());
        store.save_inventory(&inventory).unwrap();

        let journal_path = store.root.join(".artifact-store-recovery.json");
        let journal = serde_json::json!({
            "schema": "tracedecay.artifact-store-recovery.v1",
            "operation": "gc",
            "recorded_at_unix": NOW + 150,
            "records": [serde_json::to_value(&record).unwrap()],
        });
        std::fs::remove_dir_all(store.artifact_dir(&digest)).unwrap();
        std::fs::write(&journal_path, serde_json::to_vec(&journal).unwrap()).unwrap();
        drop(store);

        let reopened = ModelArtifactStore::open(
            store_root,
            trust_set(),
            Arc::new(FakeEd25519Verifier),
            RetentionPolicyV1 { grace_seconds: 100 },
        )
        .unwrap();
        assert!(reopened.inventory().unwrap().records.is_empty());
        assert!(!journal_path.exists());
        let receipts =
            std::fs::read_to_string(reopened.root.join("receipts").join("gc.jsonl")).unwrap();
        assert_eq!(receipts.lines().count(), 1);
    }

    #[test]
    fn gc_recovery_completes_every_receipt_crash_phase_and_clears_journal() {
        for phase in 0..4 {
            let dir = tempfile::tempdir().unwrap();
            let store_root = dir.path().join("store");
            let store = ModelArtifactStore::open(
                &store_root,
                trust_set(),
                Arc::new(FakeEd25519Verifier),
                RetentionPolicyV1 { grace_seconds: 100 },
            )
            .unwrap();
            let manifest = manifest_for(format!("gc crash phase {phase}").as_bytes());
            let record = store.record_for(
                &manifest,
                ArtifactTrustBindingV1 {
                    root_id: "release-1".to_string(),
                    rotation_epoch: 1,
                },
                ArtifactInventoryStateV1::Verified,
                NOW,
                None,
            );
            let digest = record.artifact_digest.clone();
            std::fs::create_dir_all(store.artifact_dir(&digest)).unwrap();
            let mut inventory = store.inventory().unwrap();
            inventory.records.insert(digest.to_string(), record.clone());
            store.save_inventory(&inventory).unwrap();
            let journal = RecoveryJournalV1 {
                schema: RECOVERY_SCHEMA_V1.to_string(),
                action: RecoveryActionV1::Gc {
                    recorded_at_unix: NOW + 150,
                    records: vec![record.clone()],
                },
            };
            std::fs::write(store.recovery_path(), serde_json::to_vec(&journal).unwrap()).unwrap();

            if phase >= 1 {
                std::fs::remove_dir_all(store.artifact_dir(&digest)).unwrap();
            }
            if phase >= 2 {
                inventory.records.remove(digest.as_str());
                std::fs::write(
                    store.inventory_path(),
                    serde_json::to_vec(&inventory).unwrap(),
                )
                .unwrap();
            }
            if phase >= 3 {
                let receipt = GcReceiptV1 {
                    artifact_digest: digest.clone(),
                    removed_at_unix: NOW + 150,
                    prior_state: ArtifactInventoryStateV1::Verified,
                };
                std::fs::write(
                    store.root.join("receipts").join("gc.jsonl"),
                    format!("{}\n", serde_json::to_string(&receipt).unwrap()),
                )
                .unwrap();
            }
            drop(store);

            let reopened = ModelArtifactStore::open(
                &store_root,
                trust_set(),
                Arc::new(FakeEd25519Verifier),
                RetentionPolicyV1 { grace_seconds: 100 },
            )
            .unwrap();
            assert!(reopened.inventory().unwrap().records.is_empty());
            assert!(!reopened.recovery_path().exists());
            let receipts =
                std::fs::read_to_string(reopened.root.join("receipts").join("gc.jsonl")).unwrap();
            assert_eq!(receipts.lines().count(), 1, "crash phase {phase}");
        }
    }

    #[test]
    fn gc_recovery_discards_torn_receipt_tail_before_replay() {
        let (dir, store) = store();
        let store_root = dir.path().join("store");
        let manifest = manifest_for(b"torn receipt");
        let record = store.record_for(
            &manifest,
            ArtifactTrustBindingV1 {
                root_id: "release-1".to_string(),
                rotation_epoch: 1,
            },
            ArtifactInventoryStateV1::Verified,
            NOW,
            None,
        );
        let digest = record.artifact_digest.clone();
        let old_receipt = GcReceiptV1 {
            artifact_digest: Sha256DigestHex::of_bytes(b"old receipt"),
            removed_at_unix: NOW,
            prior_state: ArtifactInventoryStateV1::Verified,
        };
        std::fs::write(
            store.root.join("receipts").join("gc.jsonl"),
            format!(
                "{}\n{{\"artifact_digest\":",
                serde_json::to_string(&old_receipt).unwrap()
            ),
        )
        .unwrap();
        let journal = RecoveryJournalV1 {
            schema: RECOVERY_SCHEMA_V1.to_string(),
            action: RecoveryActionV1::Gc {
                recorded_at_unix: NOW + 150,
                records: vec![record],
            },
        };
        std::fs::write(store.recovery_path(), serde_json::to_vec(&journal).unwrap()).unwrap();
        drop(store);

        let reopened = ModelArtifactStore::open(
            store_root,
            trust_set(),
            Arc::new(FakeEd25519Verifier),
            RetentionPolicyV1 { grace_seconds: 100 },
        )
        .unwrap();
        let receipts =
            std::fs::read_to_string(reopened.root.join("receipts").join("gc.jsonl")).unwrap();
        assert_eq!(receipts.lines().count(), 2);
        assert!(receipts.ends_with('\n'));
        assert!(receipts.contains(digest.as_str()));
        assert!(!reopened.recovery_path().exists());
    }

    #[cfg(unix)]
    #[test]
    fn receipt_recovery_atomically_replaces_existing_namespace_entry() {
        use std::os::unix::fs::MetadataExt;

        let (dir, store) = store();
        let store_root = dir.path().join("store");
        let manifest = manifest_for(b"atomic receipt replacement");
        let record = store.record_for(
            &manifest,
            ArtifactTrustBindingV1 {
                root_id: "release-1".to_string(),
                rotation_epoch: 1,
            },
            ArtifactInventoryStateV1::Verified,
            NOW,
            None,
        );
        let receipt_path = store.root.join("receipts").join("gc.jsonl");
        std::fs::write(&receipt_path, b"").unwrap();
        let old_inode = std::fs::metadata(&receipt_path).unwrap().ino();
        let journal = RecoveryJournalV1 {
            schema: RECOVERY_SCHEMA_V1.to_string(),
            action: RecoveryActionV1::Gc {
                recorded_at_unix: NOW + 150,
                records: vec![record],
            },
        };
        std::fs::write(store.recovery_path(), serde_json::to_vec(&journal).unwrap()).unwrap();
        drop(store);

        let reopened = ModelArtifactStore::open(
            store_root,
            trust_set(),
            Arc::new(FakeEd25519Verifier),
            RetentionPolicyV1 { grace_seconds: 100 },
        )
        .unwrap();
        assert_ne!(std::fs::metadata(&receipt_path).unwrap().ino(), old_inode);
        assert!(!reopened.recovery_path().exists());
    }

    #[test]
    fn inventory_operations_wait_for_the_store_transaction_lock() {
        let (_dir, store) = store();
        let store = Arc::new(store);
        let worker_store = Arc::clone(&store);
        let guard = store.acquire_lock().unwrap();
        let (sent, received) = mpsc::channel();

        let worker = thread::spawn(move || {
            sent.send(worker_store.inventory().is_ok()).unwrap();
        });
        assert!(
            received.recv_timeout(Duration::from_millis(50)).is_err(),
            "a concurrent inventory read must wait for the transaction lock"
        );
        drop(guard);
        assert!(received.recv_timeout(Duration::from_secs(1)).unwrap());
        worker.join().unwrap();
    }

    #[test]
    fn revoked_and_quarantined_artifacts_disable_semantics_without_substitution() {
        let (_dir, store) = store();
        let (manifest, digest) = import_ok(&store, &model_bytes());
        store.revoke_artifact(&digest, NOW).unwrap();
        assert_eq!(
            store
                .admit_for_runtime(&digest, &manifest, &env(), NOW)
                .unwrap_err(),
            SemanticCapabilityDisabledV1::RevokedArtifact
        );

        // Quarantined record (from a failed import) is never admitted.
        let quarantined_manifest = manifest_for(b"quarantined model");
        let mut session = store.begin_import(&quarantined_manifest, NOW).unwrap();
        store
            .stage_chunk(&mut session, b"junk bytes here", NOW)
            .unwrap();
        let _ = store.finalize_import(session, &quarantined_manifest, NOW);
        assert!(matches!(
            store
                .admit_for_runtime(
                    &quarantined_manifest.signed_identity_digest(),
                    &quarantined_manifest,
                    &env(),
                    NOW
                )
                .unwrap_err(),
            SemanticCapabilityDisabledV1::QuarantinedArtifact
        ));
        assert_eq!(
            store.begin_import(&quarantined_manifest, NOW).unwrap_err(),
            ArtifactImportErrorV1::StagingUnavailable,
            "quarantine is evidence, not an implicit retry or replacement"
        );
    }

    #[test]
    fn incompatible_platform_runtime_and_ceiling_disable_semantics() {
        let (_dir, store) = store();
        let (manifest, digest) = import_ok(&store, &model_bytes());

        let mut bad_platform = env();
        bad_platform.arch = "aarch64".to_string();
        assert!(matches!(
            store
                .admit_for_runtime(&digest, &manifest, &bad_platform, NOW)
                .unwrap_err(),
            SemanticCapabilityDisabledV1::IncompatiblePlatform
        ));

        let mut wrong_os = env();
        wrong_os.os = "windows".to_string();
        assert!(matches!(
            store
                .admit_for_runtime(&digest, &manifest, &wrong_os, NOW)
                .unwrap_err(),
            SemanticCapabilityDisabledV1::IncompatiblePlatform
        ));

        let mut bad_runtime = env();
        bad_runtime.build_revision = "rev-2".to_string();
        assert!(matches!(
            store
                .admit_for_runtime(&digest, &manifest, &bad_runtime, NOW)
                .unwrap_err(),
            SemanticCapabilityDisabledV1::IncompatibleRuntime
        ));

        let mut low_memory = env();
        low_memory.available_resident_bytes = 10;
        assert!(matches!(
            store
                .admit_for_runtime(&digest, &manifest, &low_memory, NOW)
                .unwrap_err(),
            SemanticCapabilityDisabledV1::ResourceCeilingExceeded
        ));
    }

    #[test]
    fn corrupt_on_disk_bytes_disable_semantics_at_admission() {
        let (_dir, store) = store();
        let (manifest, digest) = import_ok(&store, &model_bytes());
        // Corrupt the placed bytes after import.
        std::fs::write(store.artifact_path(&digest), b"tampered").unwrap();
        assert_eq!(
            store
                .admit_for_runtime(&digest, &manifest, &env(), NOW)
                .unwrap_err(),
            SemanticCapabilityDisabledV1::CorruptArtifact
        );
    }

    #[test]
    fn gc_collects_unreferenced_past_grace_and_appends_receipt() {
        let (_dir, store) = store();
        // Seed an unreferenced Verified record directly.
        let manifest = manifest_for(b"orphan verified artifact");
        let record = store.record_for(
            &manifest,
            ArtifactTrustBindingV1 {
                root_id: "release-1".to_string(),
                rotation_epoch: 1,
            },
            ArtifactInventoryStateV1::Verified,
            NOW,
            None,
        );
        let digest = record.artifact_digest.clone();
        std::fs::create_dir_all(store.artifact_dir(&digest)).unwrap();
        let mut inventory = store.inventory().unwrap();
        inventory.records.insert(digest.to_string(), record);
        store.save_inventory(&inventory).unwrap();

        // Within grace: retained.
        assert!(store.gc(NOW + 50).unwrap().is_empty());
        assert!(store.artifact_dir(&digest).exists());

        // Past grace: collected with an append-only receipt.
        let receipts = store.gc(NOW + 150).unwrap();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].artifact_digest, digest);
        assert_eq!(receipts[0].prior_state, ArtifactInventoryStateV1::Verified);
        assert!(!store.artifact_dir(&digest).exists());
        let log = std::fs::read_to_string(store.root.join("receipts").join("gc.jsonl")).unwrap();
        assert_eq!(log.lines().count(), 1);
        assert!(store.inventory().unwrap().records.is_empty());
    }

    #[test]
    fn gc_preserves_retained_revoked_and_installed() {
        let (_dir, store) = store();
        let (_manifest_a, _digest_a) = import_ok(&store, &model_bytes());
        let (manifest_b, digest_b) = import_ok(&store, b"second model bytes");
        store.retain_for_rollback(&digest_b, NOW).unwrap();

        // Revoked record (separate artifact) is evidence; not collected.
        let (_manifest_c, digest_c) = import_ok(&store, b"third model bytes");
        store.revoke_artifact(&digest_c, NOW).unwrap();

        let receipts = store.gc(NOW + 10_000).unwrap();
        assert!(receipts.is_empty());
        let inventory = store.inventory().unwrap();
        assert_eq!(inventory.records.len(), 3);
        // The rollback-retained artifact still admits after GC.
        let admitted = store
            .admit_for_runtime(&digest_b, &manifest_b, &env(), NOW)
            .unwrap();
        assert_eq!(admitted.artifact_digest(), &digest_b);
    }

    #[test]
    fn no_network_types_anywhere_in_this_packet() {
        // Structural guarantee: the quarantined packet performs zero network
        // operations. Scan the production (non-test) portion of every packet
        // source for client/transport tokens.
        for source in [
            include_str!("artifact_store.rs"),
            include_str!("manifest.rs"),
            include_str!("trust_roots.rs"),
        ] {
            let production = source.split("#[cfg(test)]").next().unwrap_or(source);
            for token in [
                "reqwest",
                "ureq",
                "hyper",
                "curl",
                "std::net",
                "tokio::net",
                "TcpStream",
                "http://",
                "https://",
            ] {
                assert!(
                    !production.contains(token),
                    "forbidden network token `{token}` found in quarantined packet"
                );
            }
        }
    }

    #[test]
    fn artifact_filesystem_boundary_uses_safe_capability_primitives() {
        let production = include_str!("artifact_store.rs")
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .unwrap();
        assert!(production.contains("#![forbid(unsafe_code)]"));
        assert!(!production.contains("unsafe extern"));
        assert!(!production.contains("Dir::open_ambient_dir(&root"));
        assert!(!production.contains("entry.open_dir()"));
        assert!(production.contains("open_dir_nofollow(&staging_id)"));
        assert!(production.contains("fsys::quick::write"));
    }
}
