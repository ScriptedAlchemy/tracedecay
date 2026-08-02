//! Digest-addressed model artifact store with verified resumable import
//! (Plan 31 "Model and offline lifecycle").
//!
//! Layout under the caller-owned root (Plan-02-owned user store at
//! integration; keyed by artifact digest, never an ambient cache):
//!
//! ```text
//! <root>/staging/<opaque-id>/members/*        resumable package staging
//! <root>/staging/<opaque-id>/import.meta.json immutable package identity
//! <root>/artifacts/<manifest-digest>/         verified package members
//! <root>/inventory.json                       staged|verified|installed|...
//! <root>/receipts/gc.jsonl                    append-only GC receipts
//! <root>/.artifact-store-recovery.json        crash-recovery transaction
//! ```
//!
//! Import accepts caller-provided bytes only. It stages under a random local
//! directory, resumes only because the manifest supplies immutable length and
//! digest identity, streams length + SHA-256 verification, fsyncs files and
//! directories, then atomically publishes the inventory record. Corrupt,
//! revoked, quarantined, or runtime-incompatible artifacts disable semantics
//! without substitution or query-time download.
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use cap_fs_ext::{
    DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt, ambient_authority,
};
// The directory-fsync path that needs maybe_dir is compiled out on Windows.
#[cfg(not(windows))]
use cap_fs_ext::OpenOptionsMaybeDirExt;
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
    /// Digest of the complete canonical manifest. This is the package identity
    /// and prevents same-model/different-tokenizer collisions.
    pub artifact_digest: Sha256DigestHex,
    /// Digest of canonical payload bytes, retained for audit correlation.
    pub manifest_digest: Sha256DigestHex,
    /// Canonical manifest retained so an offline restart can re-admit an
    /// independently selected embedding or reranker artifact by digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<ModelArtifactManifestV1>,
    pub members: Vec<ArtifactPackageMemberV1>,
    pub state: ArtifactInventoryStateV1,
    pub recorded_at_unix: u64,
    pub quarantine_reason: Option<QuarantineReasonV1>,
}

/// Stable, non-sensitive quarantine classification. Never retain input paths,
/// raw handles, filesystem errors, or package bytes in inventory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuarantineReasonV1 {
    IdentityMismatch,
    MemberLengthMismatch,
    MemberDigestMismatch,
    SizeExpansion,
    UnsafePackage,
    UndeclaredMember,
    SourceInterrupted,
    RecoveryFailure,
}

/// Durable profile-independent inventory. Plan 20 owns active/rollback
/// profile pointers and their compare-and-swap semantics.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactInventoryV1 {
    pub records: BTreeMap<String, ArtifactInventoryRecordV1>,
    #[serde(default)]
    pub leases: BTreeMap<String, Vec<ArtifactLeaseV1>>,
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

pub const FASTEMBED_RUNTIME_FAMILY_V1: &str = "fastembed-ort";
pub const FASTEMBED_RUNTIME_BUILD_REVISION_V1: &str = "fastembed-5.17.3+ort-2.0.0-rc.12";

impl RuntimeEnvironmentV1 {
    /// Capture the runtime and host resources of this process. These values
    /// are independent of any candidate artifact manifest.
    #[cfg(feature = "semantic-fastembed")]
    pub fn detect_fastembed_process() -> Result<Self, SemanticCapabilityDisabledV1> {
        let available_threads = std::thread::available_parallelism()
            .ok()
            .and_then(|threads| u32::try_from(threads.get()).ok())
            .filter(|threads| *threads > 0)
            .ok_or(SemanticCapabilityDisabledV1::ResourceCeilingExceeded)?;
        let mut system = sysinfo::System::new_with_specifics(
            sysinfo::RefreshKind::new().with_memory(sysinfo::MemoryRefreshKind::new().with_ram()),
        );
        system.refresh_memory();
        let host_available = system.available_memory();
        let available_resident_bytes = system.cgroup_limits().map_or(host_available, |limits| {
            host_available.min(limits.free_memory)
        });
        if available_resident_bytes == 0 {
            return Err(SemanticCapabilityDisabledV1::ResourceCeilingExceeded);
        }
        Ok(Self {
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            runtime: FASTEMBED_RUNTIME_FAMILY_V1.to_owned(),
            build_revision: FASTEMBED_RUNTIME_BUILD_REVISION_V1.to_owned(),
            available_resident_bytes,
            available_threads,
        })
    }

    #[cfg(not(feature = "semantic-fastembed"))]
    pub fn detect_fastembed_process() -> Result<Self, SemanticCapabilityDisabledV1> {
        Err(SemanticCapabilityDisabledV1::IncompatibleRuntime)
    }
}

/// Import failures. Every variant is typed; staging is discarded or
/// quarantined on failure and never exposed to runtime discovery.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ArtifactImportErrorV1 {
    #[error("artifact manifest rejected")]
    ManifestRejected,
    #[error("staged write exceeds the declared member length")]
    SizeExpansionBeyondDeclared,
    #[error("staged member length does not match its declared pin")]
    LengthMismatch,
    #[error("staged member digest does not match its declared pin")]
    DigestMismatch,
    #[error("artifact package member set is incomplete or inconsistent")]
    MemberMismatch,
    #[error("artifact package contains an unsafe filesystem entry")]
    UnsafePackageEntry,
    #[error("artifact package contains an undeclared member")]
    UndeclaredMember,
    #[error("configured artifact source must be an explicit HTTPS URL")]
    InvalidHttpsSource,
    #[error("configured HTTPS response does not match the immutable range contract")]
    ImmutableRangeMismatch,
    #[error("artifact import was interrupted and may be resumed")]
    InterruptedResumable { staging_id: String },
    #[error("artifact source was interrupted and cannot be resumed safely")]
    SourceInterrupted,
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
    #[error("artifact lease authority is ambiguous")]
    LeaseConflict,
    #[error("artifact store operation failed")]
    StorageFailure,
}

/// Explicit immutable HTTPS source. Construction rejects ambient hub/model
/// identifiers, mutable URLs, query parameters, and fragments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfiguredHttpsArtifactSourceV1 {
    base_url: String,
    immutable_revision: String,
}

impl ConfiguredHttpsArtifactSourceV1 {
    pub fn new(
        base_url: impl Into<String>,
        immutable_revision: impl Into<String>,
    ) -> Result<Self, ArtifactImportErrorV1> {
        let base_url = base_url.into();
        let immutable_revision = immutable_revision.into();
        let parsed =
            url::Url::parse(&base_url).map_err(|_| ArtifactImportErrorV1::InvalidHttpsSource)?;
        if parsed.scheme() != "https"
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || immutable_revision.trim().is_empty()
        {
            return Err(ArtifactImportErrorV1::InvalidHttpsSource);
        }
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            immutable_revision,
        })
    }

    fn member_url(&self, member: &ArtifactPackageMemberV1) -> String {
        format!("{}/{}", self.base_url, member.path)
    }
}

/// One immutable byte-range request. The transport must not redirect to a
/// mutable identity or consult an ambient model cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpsArtifactRangeRequestV1 {
    pub url: String,
    pub offset: u64,
    pub max_bytes: u64,
    pub expected_total_length: u64,
    pub expected_sha256: Sha256DigestHex,
    pub immutable_revision: String,
}

/// One response from the explicitly configured HTTPS transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpsArtifactRangeResponseV1 {
    pub offset: u64,
    pub total_length: u64,
    pub immutable_revision: String,
    pub bytes: Vec<u8>,
}

/// Typed production port for explicit HTTPS artifact import. Query and
/// runtime code receive no reference to this port.
pub trait ExplicitHttpsArtifactTransportV1: Send + Sync {
    fn fetch_range(
        &self,
        request: &HttpsArtifactRangeRequestV1,
    ) -> Result<HttpsArtifactRangeResponseV1, ArtifactImportErrorV1>;
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
    #[error("runtime is incompatible")]
    IncompatibleRuntime,
    #[error("platform is incompatible")]
    IncompatiblePlatform,
    #[error("resource ceiling cannot be honored")]
    ResourceCeilingExceeded,
    #[error("artifact lacks the required active runtime lease")]
    LeaseUnavailable,
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

/// A verified runtime member could not be opened from its pinned artifact.
///
/// The capability never exposes a path or an I/O error to callers: a changed,
/// missing, or unsafe member is indistinguishable from a corrupt artifact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmittedArtifactReadErrorV1 {
    Unavailable,
    Corrupt,
}

/// Store-owned capability for re-reading a verified artifact member without
/// reconstructing a path from package metadata.
struct AdmittedArtifactSourceV1 {
    directory: Dir,
}

impl AdmittedArtifactSourceV1 {
    fn read_member_bytes(
        &self,
        member: &ArtifactPackageMemberV1,
    ) -> Result<Vec<u8>, AdmittedArtifactReadErrorV1> {
        let mut file = open_cap_file(
            &self.directory,
            member_file_name(member.role),
            true,
            false,
            false,
            false,
            false,
        )
        .map_err(|_| AdmittedArtifactReadErrorV1::Corrupt)?;
        let declared_length = usize::try_from(member.byte_length)
            .map_err(|_| AdmittedArtifactReadErrorV1::Corrupt)?;
        let metadata = file
            .metadata()
            .map_err(|_| AdmittedArtifactReadErrorV1::Corrupt)?;
        if metadata.len() != member.byte_length {
            return Err(AdmittedArtifactReadErrorV1::Corrupt);
        }

        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(declared_length)
            .map_err(|_| AdmittedArtifactReadErrorV1::Corrupt)?;
        file.read_to_end(&mut bytes)
            .map_err(|_| AdmittedArtifactReadErrorV1::Corrupt)?;
        if bytes.len() != declared_length || Sha256DigestHex::of_bytes(&bytes) != member.digest {
            return Err(AdmittedArtifactReadErrorV1::Corrupt);
        }
        Ok(bytes)
    }
}

/// An artifact admitted for runtime use. The disk path intentionally stays
/// store-private; later runtime wiring receives a store-owned handle instead
/// of an ambient filesystem path.
#[derive(Clone)]
pub struct AdmittedArtifactV1 {
    artifact_digest: Sha256DigestHex,
    manifest_digest: Sha256DigestHex,
    manifest: ModelArtifactManifestV1,
    source: Option<Arc<AdmittedArtifactSourceV1>>,
}

impl fmt::Debug for AdmittedArtifactV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdmittedArtifactV1")
            .field("artifact_digest", &self.artifact_digest)
            .field("manifest_digest", &self.manifest_digest)
            .field("manifest", &self.manifest)
            .field("has_store_source", &self.source.is_some())
            .finish()
    }
}

impl PartialEq for AdmittedArtifactV1 {
    fn eq(&self, other: &Self) -> bool {
        self.artifact_digest == other.artifact_digest
            && self.manifest_digest == other.manifest_digest
            && self.manifest == other.manifest
    }
}

impl Eq for AdmittedArtifactV1 {}

impl AdmittedArtifactV1 {
    pub fn artifact_digest(&self) -> &Sha256DigestHex {
        &self.artifact_digest
    }

    pub fn manifest_digest(&self) -> &Sha256DigestHex {
        &self.manifest_digest
    }

    pub fn manifest(&self) -> &ModelArtifactManifestV1 {
        &self.manifest
    }

    /// Read one declared member through the digest-addressed store capability
    /// and re-check the exact signed length and SHA-256 pin.
    pub fn read_member_bytes(
        &self,
        role: ArtifactMemberRoleV1,
    ) -> Result<Vec<u8>, AdmittedArtifactReadErrorV1> {
        let member = self
            .manifest
            .package_member(role)
            .ok_or(AdmittedArtifactReadErrorV1::Unavailable)?;
        self.source
            .as_ref()
            .ok_or(AdmittedArtifactReadErrorV1::Unavailable)?
            .read_member_bytes(member)
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn test_fixture(manifest: ModelArtifactManifestV1) -> Self {
        Self {
            artifact_digest: manifest.artifact_identity_digest(),
            manifest_digest: manifest.canonical_digest(),
            manifest,
            source: None,
        }
    }

    #[cfg(test)]
    pub fn test_fixture_with_identities(
        manifest: ModelArtifactManifestV1,
        artifact_digest: Sha256DigestHex,
        manifest_digest: Sha256DigestHex,
    ) -> Self {
        Self {
            artifact_digest,
            manifest_digest,
            manifest,
            source: None,
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

/// Runtime references that protect installed artifacts from collection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactLeaseKindV1 {
    Active,
    Rollback,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactLeaseV1 {
    pub lease_id: String,
    pub kind: ArtifactLeaseKindV1,
    pub expires_at_unix: u64,
}

/// Opaque proof that the daemon exclusively owns one bounded GC pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaemonArtifactGcLeaseV1 {
    lease_id: String,
    expires_at_unix: u64,
}

/// Append-only GC receipt (one JSON line per removed artifact).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcReceiptV1 {
    pub artifact_digest: Sha256DigestHex,
    pub removed_at_unix: u64,
    pub prior_state: ArtifactInventoryStateV1,
}

/// Resume identity persisted beside staged bytes. It persists the complete
/// canonical manifest and every package member, so recovery can never infer a
/// missing identity from an ambient cache or path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct StagingMetaV1 {
    schema: String,
    manifest: ModelArtifactManifestV1,
    manifest_identity_digest: Sha256DigestHex,
    verified_at_unix: u64,
    #[serde(default)]
    immutable_source_revision: Option<String>,
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
        // Boxed to keep this variant from dominating the enum's size; `Box<T>`
        // is serde-transparent, so the on-disk journal encoding is unchanged.
        record: Box<ArtifactInventoryRecordV1>,
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
    retention: RetentionPolicyV1,
    operation_lock: Arc<Mutex<()>>,
}

impl ModelArtifactStore {
    /// Open (creating if needed) a store rooted at `root`.
    pub fn open(
        root: impl Into<PathBuf>,
        retention: RetentionPolicyV1,
    ) -> Result<Self, ArtifactImportErrorV1> {
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
            retention,
            operation_lock: Arc::new(Mutex::new(())),
        };
        {
            let _lock = store.acquire_lock()?;
            store.recover_locked()?;
        }
        Ok(store)
    }

    #[cfg(test)]
    fn inventory_path(&self) -> PathBuf {
        self.root.join("inventory.json")
    }

    #[cfg(test)]
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

    pub fn installed_directory(&self, digest: &Sha256DigestHex) -> PathBuf {
        self.artifact_dir(digest)
    }

    #[cfg(test)]
    fn artifact_path(&self, digest: &Sha256DigestHex) -> PathBuf {
        self.member_path(digest, ArtifactMemberRoleV1::Model)
    }

    #[cfg(test)]
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

    #[cfg(test)]
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

    /// Verify the canonical manifest before any bytes are staged.
    pub fn verify_manifest(
        &self,
        manifest: &ModelArtifactManifestV1,
    ) -> Result<(), ArtifactImportErrorV1> {
        manifest
            .validate()
            .map_err(|_| ArtifactImportErrorV1::ManifestRejected)
    }

    /// Begin a resumable import of caller-provided bytes for a verified
    /// manifest. Stages under a random local directory; no network access.
    pub fn begin_import(
        &self,
        manifest: &ModelArtifactManifestV1,
        now_unix: u64,
    ) -> Result<ImportSession, ArtifactImportErrorV1> {
        self.verify_manifest(manifest)?;
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        if self
            .load_inventory_locked()?
            .records
            .contains_key(&manifest.artifact_identity_digest().to_string())
        {
            return Err(ArtifactImportErrorV1::StagingUnavailable);
        }
        let (staging_id, staging_dir) = (0..16)
            .find_map(|_| {
                let staging_id = random_staging_id().ok()?;
                #[allow(unused_mut)] // mode() is unix-only
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
        #[allow(unused_mut)] // mode() is unix-only
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
            manifest_identity_digest: manifest.artifact_identity_digest(),
            verified_at_unix: now_unix,
            immutable_source_revision: None,
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
        let record = self.record_for(manifest, ArtifactInventoryStateV1::Staged, now_unix, None);
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
        self.verify_manifest(manifest)?;
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
    #[cfg(test)]
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

    /// Import one explicit local directory. The directory must contain exactly
    /// the manifest members and only regular, single-link files. Paths are
    /// validated before any package bytes become runtime-discoverable.
    pub fn import_local_directory(
        &self,
        manifest: &ModelArtifactManifestV1,
        source: &Path,
        now_unix: u64,
    ) -> Result<ArtifactInventoryRecordV1, ArtifactImportErrorV1> {
        let mut session = self.begin_import(manifest, now_unix)?;
        let files = match inspect_local_package(source) {
            Ok(files) => files,
            Err(error) => {
                self.quarantine_and_discard(
                    &session,
                    quarantine_reason_for_import_error(&error),
                    now_unix,
                )?;
                return Err(error);
            }
        };
        let declared: BTreeMap<&str, &ArtifactPackageMemberV1> = manifest
            .payload
            .members
            .iter()
            .map(|member| (member.path.as_str(), member))
            .collect();
        if files
            .keys()
            .any(|path| !declared.contains_key(path.as_str()))
        {
            self.quarantine_and_discard(&session, QuarantineReasonV1::UndeclaredMember, now_unix)?;
            return Err(ArtifactImportErrorV1::UndeclaredMember);
        }
        if files.len() != declared.len() {
            self.quarantine_and_discard(&session, QuarantineReasonV1::IdentityMismatch, now_unix)?;
            return Err(ArtifactImportErrorV1::MemberMismatch);
        }

        for member in &manifest.payload.members {
            let path = files
                .get(&member.path)
                .ok_or(ArtifactImportErrorV1::MemberMismatch)?;
            let result = stream_local_member(self, &mut session, member, path, now_unix);
            if let Err(error) = result {
                self.quarantine_and_discard(
                    &session,
                    quarantine_reason_for_import_error(&error),
                    now_unix,
                )?;
                return Err(error);
            }
        }
        self.finalize_import(session, manifest, now_unix)
    }

    /// Import from an explicit immutable HTTPS source. Callers may pass a
    /// prior opaque staging handle only after an `InterruptedResumable`
    /// result. Each response must repeat the configured immutable revision,
    /// exact offset, and declared total length.
    pub fn import_configured_https(
        &self,
        manifest: &ModelArtifactManifestV1,
        source: &ConfiguredHttpsArtifactSourceV1,
        transport: &dyn ExplicitHttpsArtifactTransportV1,
        resume_staging_id: Option<&str>,
        now_unix: u64,
    ) -> Result<ArtifactInventoryRecordV1, ArtifactImportErrorV1> {
        let mut session = match resume_staging_id {
            Some(staging_id) => self.resume_import(manifest, staging_id, now_unix)?,
            None => self.begin_import(manifest, now_unix)?,
        };
        if let Some(pinned) = &session.meta.immutable_source_revision {
            if pinned != &source.immutable_revision {
                self.quarantine_and_discard(
                    &session,
                    QuarantineReasonV1::IdentityMismatch,
                    now_unix,
                )?;
                return Err(ArtifactImportErrorV1::ResumeIdentityMismatch);
            }
        } else {
            session.meta.immutable_source_revision = Some(source.immutable_revision.clone());
            write_staging_meta(&session.staging_dir, &session.staging_path, &session.meta)?;
        }

        for member_index in 0..session.meta.members.len() {
            let member = session.meta.members[member_index].member.clone();
            while session.meta.members[member_index].bytes_written < member.byte_length {
                let offset = session.meta.members[member_index].bytes_written;
                let request = HttpsArtifactRangeRequestV1 {
                    url: source.member_url(&member),
                    offset,
                    max_bytes: (member.byte_length - offset).min(64 * 1024),
                    expected_total_length: member.byte_length,
                    expected_sha256: member.digest.clone(),
                    immutable_revision: source.immutable_revision.clone(),
                };
                let response = match transport.fetch_range(&request) {
                    Ok(response) => response,
                    Err(_) => {
                        return Err(ArtifactImportErrorV1::InterruptedResumable {
                            staging_id: session.staging_id(),
                        });
                    }
                };
                let response_len = u64::try_from(response.bytes.len())
                    .map_err(|_| ArtifactImportErrorV1::SizeExpansionBeyondDeclared)?;
                if response.offset != offset
                    || response.total_length != member.byte_length
                    || response.immutable_revision != source.immutable_revision
                    || response.bytes.is_empty()
                    || response_len > request.max_bytes
                {
                    self.quarantine_and_discard(
                        &session,
                        QuarantineReasonV1::IdentityMismatch,
                        now_unix,
                    )?;
                    return Err(ArtifactImportErrorV1::ImmutableRangeMismatch);
                }
                if let Err(error) =
                    self.stage_member_chunk(&mut session, member.role, &response.bytes, now_unix)
                {
                    self.quarantine_and_discard(
                        &session,
                        quarantine_reason_for_import_error(&error),
                        now_unix,
                    )?;
                    return Err(error);
                }
            }
        }
        self.finalize_import(session, manifest, now_unix)
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
        self.verify_manifest(manifest)?;
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

        let mut record =
            self.record_for(manifest, ArtifactInventoryStateV1::Verified, now_unix, None);
        let mut inventory = self.load_inventory_locked()?;
        inventory
            .records
            .insert(record.artifact_digest.to_string(), record.clone());
        self.save_inventory_locked(&inventory)?;
        self.write_recovery_locked(&RecoveryJournalV1 {
            schema: RECOVERY_SCHEMA_V1.to_string(),
            action: RecoveryActionV1::Install {
                record: Box::new(record.clone()),
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
        let record = self.record_for(
            &session.meta.manifest,
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

    fn quarantine_and_discard(
        &self,
        session: &ImportSession,
        reason: QuarantineReasonV1,
        now_unix: u64,
    ) -> Result<(), ArtifactImportErrorV1> {
        {
            let _lock = self.acquire_lock()?;
            self.recover_locked()?;
            self.quarantine_staging_locked(session, reason, now_unix)?;
        }
        self.remove_staging_dir_path(&session.staging_id)
    }

    /// Mark an installed artifact revoked. Revoked artifacts are never
    /// admitted and are protected from GC (revocation evidence is retained).
    #[cfg(test)]
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
    #[cfg(test)]
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

    /// Acquire or renew an active/rollback reference. Rollback leases also
    /// transition the record to the durable rollback-retained state.
    pub fn acquire_artifact_lease(
        &self,
        digest: &Sha256DigestHex,
        lease: ArtifactLeaseV1,
        now_unix: u64,
    ) -> Result<(), ArtifactImportErrorV1> {
        if lease.lease_id.trim().is_empty() || lease.expires_at_unix <= now_unix {
            return Err(ArtifactImportErrorV1::StagingUnavailable);
        }
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        let mut inventory = self.load_inventory_locked()?;
        let record = inventory
            .records
            .get_mut(&digest.to_string())
            .ok_or(ArtifactImportErrorV1::StagingUnavailable)?;
        if !matches!(
            record.state,
            ArtifactInventoryStateV1::Installed | ArtifactInventoryStateV1::RetainedForRollback
        ) {
            return Err(ArtifactImportErrorV1::StagingUnavailable);
        }
        if lease.kind == ArtifactLeaseKindV1::Rollback {
            record.state = ArtifactInventoryStateV1::RetainedForRollback;
        }
        let leases = inventory.leases.entry(digest.to_string()).or_default();
        leases
            .retain(|existing| existing.lease_id != lease.lease_id || existing.kind != lease.kind);
        leases.push(lease);
        self.save_inventory_locked(&inventory)
    }

    pub fn release_artifact_lease(
        &self,
        digest: &Sha256DigestHex,
        lease_id: &str,
        kind: ArtifactLeaseKindV1,
    ) -> Result<(), ArtifactImportErrorV1> {
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        let mut inventory = self.load_inventory_locked()?;
        if let Some(leases) = inventory.leases.get_mut(&digest.to_string()) {
            leases.retain(|lease| lease.lease_id != lease_id || lease.kind != kind);
            if leases.is_empty() {
                inventory.leases.remove(&digest.to_string());
            }
        }
        self.save_inventory_locked(&inventory)
    }

    pub fn artifact_digest_for_lease(
        &self,
        lease_id: &str,
        kind: ArtifactLeaseKindV1,
        now_unix: u64,
    ) -> Result<Option<Sha256DigestHex>, ArtifactImportErrorV1> {
        if lease_id.trim().is_empty() {
            return Err(ArtifactImportErrorV1::LeaseConflict);
        }
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        let inventory = self.load_inventory_locked()?;
        let mut matched = inventory.leases.iter().filter_map(|(digest, leases)| {
            leases
                .iter()
                .any(|lease| {
                    lease.lease_id == lease_id
                        && lease.kind == kind
                        && lease.expires_at_unix > now_unix
                })
                .then_some(digest)
        });
        let first = matched.next();
        if matched.next().is_some() {
            return Err(ArtifactImportErrorV1::LeaseConflict);
        }
        first
            .map(|digest| {
                Sha256DigestHex::new(digest.clone())
                    .map_err(|_| ArtifactImportErrorV1::LeaseConflict)
            })
            .transpose()
    }

    /// Atomically activate one installed artifact and retain the prior active
    /// artifact as the single rollback target for this lease namespace.
    pub fn activate_artifact_with_rollback(
        &self,
        digest: &Sha256DigestHex,
        active_lease_id: &str,
        rollback_lease_id: &str,
        now_unix: u64,
    ) -> Result<(), ArtifactImportErrorV1> {
        if active_lease_id.trim().is_empty()
            || rollback_lease_id.trim().is_empty()
            || active_lease_id == rollback_lease_id
        {
            return Err(ArtifactImportErrorV1::LeaseConflict);
        }
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        let mut inventory = self.load_inventory_locked()?;
        let record = inventory
            .records
            .get(&digest.to_string())
            .ok_or(ArtifactImportErrorV1::StagingUnavailable)?;
        if !matches!(
            record.state,
            ArtifactInventoryStateV1::Installed | ArtifactInventoryStateV1::RetainedForRollback
        ) {
            return Err(ArtifactImportErrorV1::StagingUnavailable);
        }

        let mut prior_active = None;
        let mut prior_rollback = None;
        for (leased_digest, leases) in &inventory.leases {
            for lease in leases
                .iter()
                .filter(|lease| lease.expires_at_unix > now_unix)
            {
                let slot = if lease.kind == ArtifactLeaseKindV1::Active
                    && lease.lease_id == active_lease_id
                {
                    Some(&mut prior_active)
                } else if lease.kind == ArtifactLeaseKindV1::Rollback
                    && lease.lease_id == rollback_lease_id
                {
                    Some(&mut prior_rollback)
                } else {
                    None
                };
                if let Some(slot) = slot {
                    if slot
                        .as_ref()
                        .is_some_and(|existing: &String| existing != leased_digest)
                    {
                        return Err(ArtifactImportErrorV1::LeaseConflict);
                    }
                    *slot = Some(leased_digest.clone());
                }
            }
        }

        let digest_text = digest.to_string();
        let rollback_digest = if prior_active
            .as_ref()
            .is_some_and(|prior| prior != &digest_text)
        {
            prior_active
        } else {
            prior_rollback.filter(|prior| prior != &digest_text)
        }
        .filter(|rollback_digest| {
            inventory
                .records
                .get(rollback_digest)
                .is_some_and(|record| {
                    matches!(
                        record.state,
                        ArtifactInventoryStateV1::Installed
                            | ArtifactInventoryStateV1::RetainedForRollback
                    )
                })
        });
        for leases in inventory.leases.values_mut() {
            leases.retain(|lease| {
                !((lease.kind == ArtifactLeaseKindV1::Active && lease.lease_id == active_lease_id)
                    || (lease.kind == ArtifactLeaseKindV1::Rollback
                        && lease.lease_id == rollback_lease_id))
            });
        }
        inventory.leases.retain(|_, leases| !leases.is_empty());
        inventory
            .leases
            .entry(digest_text)
            .or_default()
            .push(ArtifactLeaseV1 {
                lease_id: active_lease_id.to_owned(),
                kind: ArtifactLeaseKindV1::Active,
                expires_at_unix: u64::MAX,
            });
        if let Some(rollback_digest) = rollback_digest {
            let rollback_record = inventory
                .records
                .get_mut(&rollback_digest)
                .ok_or(ArtifactImportErrorV1::LeaseConflict)?;
            rollback_record.state = ArtifactInventoryStateV1::RetainedForRollback;
            inventory
                .leases
                .entry(rollback_digest)
                .or_default()
                .push(ArtifactLeaseV1 {
                    lease_id: rollback_lease_id.to_owned(),
                    kind: ArtifactLeaseKindV1::Rollback,
                    expires_at_unix: u64::MAX,
                });
        }
        let retained_rollback_digests = inventory
            .leases
            .iter()
            .filter_map(|(leased_digest, leases)| {
                leases
                    .iter()
                    .any(|lease| {
                        lease.kind == ArtifactLeaseKindV1::Rollback
                            && lease.expires_at_unix > now_unix
                    })
                    .then_some(leased_digest.clone())
            })
            .collect::<BTreeSet<_>>();
        for (record_digest, record) in &mut inventory.records {
            if record.state == ArtifactInventoryStateV1::RetainedForRollback
                && !retained_rollback_digests.contains(record_digest)
            {
                record.state = ArtifactInventoryStateV1::Installed;
            }
        }
        self.save_inventory_locked(&inventory)
    }

    pub fn acquire_daemon_gc_lease(
        &self,
        lease_id: impl Into<String>,
        expires_at_unix: u64,
        now_unix: u64,
    ) -> Result<DaemonArtifactGcLeaseV1, ArtifactImportErrorV1> {
        let lease_id = lease_id.into();
        if lease_id.trim().is_empty() || expires_at_unix <= now_unix {
            return Err(ArtifactImportErrorV1::StoreBusy);
        }
        Ok(DaemonArtifactGcLeaseV1 {
            lease_id,
            expires_at_unix,
        })
    }

    /// Admit an installed artifact for runtime use against host evidence.
    /// Re-verifies the manifest and every on-disk member digest; any corrupt,
    /// revoked, quarantined, or incompatible artifact disables semantics.
    pub fn admit_for_runtime(
        &self,
        digest: &Sha256DigestHex,
        manifest: &ModelArtifactManifestV1,
        env: &RuntimeEnvironmentV1,
        now_unix: u64,
    ) -> Result<AdmittedArtifactV1, SemanticCapabilityDisabledV1> {
        self.admit_for_runtime_with_required_lease(digest, manifest, env, None, now_unix)
    }

    fn admit_for_runtime_with_required_lease(
        &self,
        digest: &Sha256DigestHex,
        manifest: &ModelArtifactManifestV1,
        env: &RuntimeEnvironmentV1,
        required_lease: Option<(&str, ArtifactLeaseKindV1)>,
        now_unix: u64,
    ) -> Result<AdmittedArtifactV1, SemanticCapabilityDisabledV1> {
        self.verify_manifest(manifest)
            .map_err(|_| SemanticCapabilityDisabledV1::IdentityMismatch)?;
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
        if let Some((lease_id, kind)) = required_lease
            && !inventory
                .leases
                .get(&digest.to_string())
                .is_some_and(|leases| {
                    leases.iter().any(|lease| {
                        lease.lease_id == lease_id
                            && lease.kind == kind
                            && lease.expires_at_unix > now_unix
                    })
                })
        {
            return Err(SemanticCapabilityDisabledV1::LeaseUnavailable);
        }
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
            || *digest != manifest.artifact_identity_digest()
            || record.manifest_digest != manifest.canonical_digest()
            || record.members != manifest.payload.members
        {
            return Err(SemanticCapabilityDisabledV1::IdentityMismatch);
        }
        self.verify_artifact_record(record)
            .map_err(|_| SemanticCapabilityDisabledV1::CorruptArtifact)?;
        check_compatibility(&manifest.payload.runtime, env)?;
        check_resource_ceiling(&manifest.payload.resource_ceiling, env)?;
        let directory = self
            .artifacts_dir
            .open_dir_nofollow(digest.as_str())
            .map_err(|_| SemanticCapabilityDisabledV1::CorruptArtifact)?;
        Ok(AdmittedArtifactV1 {
            artifact_digest: digest.clone(),
            manifest_digest: manifest.canonical_digest(),
            manifest: manifest.clone(),
            source: Some(Arc::new(AdmittedArtifactSourceV1 { directory })),
        })
    }

    /// Re-admit an installed artifact from its durable canonical manifest and
    /// caller-supplied process evidence. Legacy records without that manifest
    /// remain unavailable rather than reconstructing authority from filenames
    /// or member rows.
    pub fn admit_for_runtime_by_digest(
        &self,
        digest: &Sha256DigestHex,
        env: &RuntimeEnvironmentV1,
    ) -> Result<AdmittedArtifactV1, SemanticCapabilityDisabledV1> {
        let manifest = {
            let inventory = self
                .inventory()
                .map_err(|_| SemanticCapabilityDisabledV1::StorageFailure)?;
            inventory
                .records
                .get(&digest.to_string())
                .and_then(|record| record.manifest.clone())
                .ok_or(SemanticCapabilityDisabledV1::MissingArtifact)?
        };
        self.admit_for_runtime(digest, &manifest, env, 0)
    }

    pub fn admit_leased_for_runtime_by_digest(
        &self,
        digest: &Sha256DigestHex,
        env: &RuntimeEnvironmentV1,
        lease_id: &str,
        kind: ArtifactLeaseKindV1,
        now_unix: u64,
    ) -> Result<AdmittedArtifactV1, SemanticCapabilityDisabledV1> {
        let manifest = {
            let inventory = self
                .inventory()
                .map_err(|_| SemanticCapabilityDisabledV1::StorageFailure)?;
            inventory
                .records
                .get(&digest.to_string())
                .and_then(|record| record.manifest.clone())
                .ok_or(SemanticCapabilityDisabledV1::MissingArtifact)?
        };
        self.admit_for_runtime_with_required_lease(
            digest,
            &manifest,
            env,
            Some((lease_id, kind)),
            now_unix,
        )
    }

    /// Garbage-collect unreferenced artifacts past the grace window.
    /// `RetainedForRollback`, `Revoked`, and `Installed` records are never
    /// collected here; each removal appends one receipt to
    /// `receipts/gc.jsonl`.
    #[cfg(test)]
    pub fn gc(&self, now_unix: u64) -> Result<Vec<GcReceiptV1>, ArtifactImportErrorV1> {
        self.gc_locked_by_policy(now_unix, false)
    }

    /// Collect installed artifacts only during an explicit daemon lease and
    /// only when no unexpired active/rollback reference protects them.
    pub fn gc_with_daemon_lease(
        &self,
        lease: &DaemonArtifactGcLeaseV1,
        now_unix: u64,
    ) -> Result<Vec<GcReceiptV1>, ArtifactImportErrorV1> {
        if lease.lease_id.trim().is_empty() || lease.expires_at_unix <= now_unix {
            return Err(ArtifactImportErrorV1::StoreBusy);
        }
        self.gc_locked_by_policy(now_unix, true)
    }

    fn gc_locked_by_policy(
        &self,
        now_unix: u64,
        include_unleased_installed: bool,
    ) -> Result<Vec<GcReceiptV1>, ArtifactImportErrorV1> {
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
                ) || (include_unleased_installed
                    && matches!(
                        r.state,
                        ArtifactInventoryStateV1::Installed
                            | ArtifactInventoryStateV1::RetainedForRollback
                    ));
                let has_live_reference = inventory
                    .leases
                    .get(&r.artifact_digest.to_string())
                    .is_some_and(|leases| {
                        leases.iter().any(|lease| lease.expires_at_unix > now_unix)
                    });
                collectible_state
                    && !has_live_reference
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
            inventory.leases.remove(&record.artifact_digest.to_string());
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
        state: ArtifactInventoryStateV1,
        recorded_at_unix: u64,
        quarantine_reason: Option<QuarantineReasonV1>,
    ) -> ArtifactInventoryRecordV1 {
        ArtifactInventoryRecordV1 {
            artifact_digest: manifest.artifact_identity_digest(),
            manifest_digest: manifest.canonical_digest(),
            manifest: Some(manifest.clone()),
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
            .get(&session.meta.manifest_identity_digest.to_string())
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
            && meta.manifest_identity_digest == manifest.artifact_identity_digest()
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
                    self.recover_install_locked(*record, &staging_id)?;
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
            let Ok(staging_dir) = self.staging_dir.open_dir_nofollow(&staging_id) else {
                continue;
            };
            let members_dir = staging_dir.open_dir_nofollow("members").ok();
            let meta = match read_staging_meta(&staging_dir) {
                Ok(meta) if meta.schema == STAGING_SCHEMA_V1 => meta,
                Ok(_) | Err(_) => continue,
            };
            if self.verify_manifest(&meta.manifest).is_err() {
                continue;
            }
            let mut record = self.record_for(
                &meta.manifest,
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

fn quarantine_reason_for_import_error(error: &ArtifactImportErrorV1) -> QuarantineReasonV1 {
    match error {
        ArtifactImportErrorV1::SizeExpansionBeyondDeclared => QuarantineReasonV1::SizeExpansion,
        ArtifactImportErrorV1::LengthMismatch => QuarantineReasonV1::MemberLengthMismatch,
        ArtifactImportErrorV1::DigestMismatch => QuarantineReasonV1::MemberDigestMismatch,
        ArtifactImportErrorV1::UndeclaredMember => QuarantineReasonV1::UndeclaredMember,
        ArtifactImportErrorV1::UnsafePackageEntry | ArtifactImportErrorV1::UnsafeStorePath => {
            QuarantineReasonV1::UnsafePackage
        }
        ArtifactImportErrorV1::SourceInterrupted => QuarantineReasonV1::SourceInterrupted,
        _ => QuarantineReasonV1::IdentityMismatch,
    }
}

fn inspect_local_package(
    source: &Path,
) -> Result<BTreeMap<String, PathBuf>, ArtifactImportErrorV1> {
    let source_meta =
        fs::symlink_metadata(source).map_err(|_| ArtifactImportErrorV1::UnsafePackageEntry)?;
    if !source_meta.is_dir() || source_meta.file_type().is_symlink() {
        return Err(ArtifactImportErrorV1::UnsafePackageEntry);
    }
    let mut files = BTreeMap::new();
    let mut pending = vec![(source.to_path_buf(), String::new())];
    while let Some((directory, prefix)) = pending.pop() {
        for entry in
            fs::read_dir(&directory).map_err(|_| ArtifactImportErrorV1::UnsafePackageEntry)?
        {
            let entry = entry.map_err(|_| ArtifactImportErrorV1::UnsafePackageEntry)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| ArtifactImportErrorV1::UnsafePackageEntry)?;
            if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
                return Err(ArtifactImportErrorV1::UnsafePackageEntry);
            }
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|_| ArtifactImportErrorV1::UnsafePackageEntry)?;
            if metadata.file_type().is_symlink() {
                return Err(ArtifactImportErrorV1::UnsafePackageEntry);
            }
            if metadata.is_dir() {
                pending.push((entry.path(), relative));
                continue;
            }
            if !metadata.is_file() || metadata_has_multiple_links(&metadata) {
                return Err(ArtifactImportErrorV1::UnsafePackageEntry);
            }
            if files.insert(relative, entry.path()).is_some() {
                return Err(ArtifactImportErrorV1::UnsafePackageEntry);
            }
        }
    }
    Ok(files)
}

#[cfg(unix)]
fn metadata_has_multiple_links(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.nlink() != 1
}

#[cfg(not(unix))]
fn metadata_has_multiple_links(_metadata: &fs::Metadata) -> bool {
    false
}

fn stream_local_member(
    store: &ModelArtifactStore,
    session: &mut ImportSession,
    member: &ArtifactPackageMemberV1,
    path: &Path,
    now_unix: u64,
) -> Result<(), ArtifactImportErrorV1> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| ArtifactImportErrorV1::UnsafePackageEntry)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata_has_multiple_links(&metadata)
    {
        return Err(ArtifactImportErrorV1::UnsafePackageEntry);
    }
    if metadata.len() > member.byte_length {
        return Err(ArtifactImportErrorV1::SizeExpansionBeyondDeclared);
    }
    if metadata.len() != member.byte_length {
        return Err(ArtifactImportErrorV1::LengthMismatch);
    }
    let mut file = File::open(path).map_err(|_| ArtifactImportErrorV1::SourceInterrupted)?;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| ArtifactImportErrorV1::SourceInterrupted)?;
        if read == 0 {
            break;
        }
        store.stage_member_chunk(session, member.role, &buffer[..read], now_unix)?;
    }
    Ok(())
}

fn sha256_open_file(mut file: impl Read) -> Result<Sha256DigestHex, ArtifactImportErrorV1> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
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
        sync_cap_dir(dir)
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
            #[allow(unused_mut)] // mode() is unix-only
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
        ArtifactMemberRoleV1::Model => "model.onnx",
        ArtifactMemberRoleV1::Tokenizer => "tokenizer.json",
        ArtifactMemberRoleV1::Config => "config.json",
        ArtifactMemberRoleV1::SpecialTokensMap => "special_tokens_map.json",
        ArtifactMemberRoleV1::TokenizerConfig => "tokenizer_config.json",
        ArtifactMemberRoleV1::QueryInstruction => "query_instruction.txt",
        ArtifactMemberRoleV1::DocumentInstruction => "document_instruction.txt",
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
    use super::*;
    use std::sync::{Arc, mpsc};
    use std::thread;
    use std::time::Duration;

    const NOW: u64 = 1_500;

    fn model_bytes() -> Vec<u8> {
        b"deterministic fake model weights".to_vec()
    }

    fn member_bytes(role: ArtifactMemberRoleV1, model: &[u8]) -> &[u8] {
        match role {
            ArtifactMemberRoleV1::Model => model,
            ArtifactMemberRoleV1::Tokenizer => b"tokenizer",
            ArtifactMemberRoleV1::Config => b"config",
            ArtifactMemberRoleV1::SpecialTokensMap => b"{}",
            ArtifactMemberRoleV1::TokenizerConfig => {
                br#"{"model_max_length": 512, "pad_token": "[PAD]"}"#
            }
            ArtifactMemberRoleV1::QueryInstruction | ArtifactMemberRoleV1::DocumentInstruction => {
                unreachable!()
            }
        }
    }

    fn manifest_for(bytes: &[u8]) -> ModelArtifactManifestV1 {
        let payload = ModelArtifactManifestPayloadV1 {
            schema: MODEL_ARTIFACT_MANIFEST_SCHEMA_V1.to_string(),
            artifact_id: "test-embed".to_string(),
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
        ModelArtifactManifestV1 { payload }
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

    fn write_local_package(root: &Path, manifest: &ModelArtifactManifestV1, model: &[u8]) {
        for member in &manifest.payload.members {
            let path = root.join(&member.path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, member_bytes(member.role, model)).unwrap();
        }
    }

    struct FixtureHttpsTransport {
        members: BTreeMap<String, Vec<u8>>,
        revision: String,
    }

    impl ExplicitHttpsArtifactTransportV1 for FixtureHttpsTransport {
        fn fetch_range(
            &self,
            request: &HttpsArtifactRangeRequestV1,
        ) -> Result<HttpsArtifactRangeResponseV1, ArtifactImportErrorV1> {
            let bytes = self
                .members
                .iter()
                .find_map(|(path, bytes)| request.url.ends_with(path).then_some(bytes))
                .ok_or(ArtifactImportErrorV1::MemberMismatch)?;
            let start = usize::try_from(request.offset)
                .map_err(|_| ArtifactImportErrorV1::ImmutableRangeMismatch)?;
            let count = usize::try_from(request.max_bytes)
                .map_err(|_| ArtifactImportErrorV1::ImmutableRangeMismatch)?;
            let end = start.saturating_add(count).min(bytes.len());
            Ok(HttpsArtifactRangeResponseV1 {
                offset: request.offset,
                total_length: bytes.len() as u64,
                immutable_revision: self.revision.clone(),
                bytes: bytes[start..end].to_vec(),
            })
        }
    }

    #[test]
    fn verified_manifest_import_places_atomically_and_admits() {
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
    fn explicit_local_directory_import_rejects_undeclared_members() {
        let (root, store) = store();
        let model = model_bytes();
        let manifest = manifest_for(&model);
        let package = root.path().join("package");
        write_local_package(&package, &manifest, &model);
        fs::write(package.join("undeclared.bin"), b"no").unwrap();
        assert_eq!(
            store
                .import_local_directory(&manifest, &package, NOW)
                .unwrap_err(),
            ArtifactImportErrorV1::UndeclaredMember
        );
        assert_eq!(
            store
                .inventory()
                .unwrap()
                .records
                .get(&manifest.artifact_identity_digest().to_string())
                .unwrap()
                .state,
            ArtifactInventoryStateV1::Quarantined
        );
    }

    #[test]
    fn explicit_https_import_uses_only_pinned_ranges() {
        let (_root, store) = store();
        let model = model_bytes();
        let manifest = manifest_for(&model);
        assert_eq!(
            ConfiguredHttpsArtifactSourceV1::new("http://models.example/rev", "immutable-r1")
                .unwrap_err(),
            ArtifactImportErrorV1::InvalidHttpsSource
        );
        let source =
            ConfiguredHttpsArtifactSourceV1::new("https://models.example/rev", "immutable-r1")
                .unwrap();
        let transport = FixtureHttpsTransport {
            members: manifest
                .payload
                .members
                .iter()
                .map(|member| {
                    (
                        member.path.clone(),
                        member_bytes(member.role, &model).to_vec(),
                    )
                })
                .collect(),
            revision: "immutable-r1".to_owned(),
        };
        let record = store
            .import_configured_https(&manifest, &source, &transport, None, NOW)
            .unwrap();
        assert_eq!(record.state, ArtifactInventoryStateV1::Installed);
    }

    #[test]
    fn daemon_gc_lease_never_collects_active_artifacts() {
        let (_root, store) = store();
        let (_manifest, digest) = import_ok(&store, &model_bytes());
        store
            .acquire_artifact_lease(
                &digest,
                ArtifactLeaseV1 {
                    lease_id: "active".to_owned(),
                    kind: ArtifactLeaseKindV1::Active,
                    expires_at_unix: NOW + 1_000,
                },
                NOW,
            )
            .unwrap();
        let daemon = store
            .acquire_daemon_gc_lease("daemon", NOW + 1_000, NOW)
            .unwrap();
        assert!(
            store
                .gc_with_daemon_lease(&daemon, NOW + 101)
                .unwrap()
                .is_empty()
        );
        store
            .release_artifact_lease(&digest, "active", ArtifactLeaseKindV1::Active)
            .unwrap();
        assert_eq!(
            store
                .gc_with_daemon_lease(&daemon, NOW + 102)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn daemon_gc_collects_only_the_superseded_rollback_after_rotation() {
        let (_root, store) = store();
        let (_first_manifest, first) = import_ok(&store, b"first verified model");
        store
            .activate_artifact_with_rollback(&first, "active", "rollback", NOW)
            .unwrap();
        let (_second_manifest, second) = import_ok(&store, b"second verified model");
        store
            .activate_artifact_with_rollback(&second, "active", "rollback", NOW + 1)
            .unwrap();
        let (_third_manifest, third) = import_ok(&store, b"third verified model");
        store
            .activate_artifact_with_rollback(&third, "active", "rollback", NOW + 2)
            .unwrap();

        let before_gc = store.inventory().unwrap();
        assert_eq!(
            before_gc.records.get(&first.to_string()).unwrap().state,
            ArtifactInventoryStateV1::Installed
        );
        assert_eq!(
            before_gc.records.get(&second.to_string()).unwrap().state,
            ArtifactInventoryStateV1::RetainedForRollback
        );
        assert_eq!(
            store
                .artifact_digest_for_lease("active", ArtifactLeaseKindV1::Active, NOW + 2)
                .unwrap(),
            Some(third.clone())
        );
        assert_eq!(
            store
                .artifact_digest_for_lease("rollback", ArtifactLeaseKindV1::Rollback, NOW + 2)
                .unwrap(),
            Some(second.clone())
        );

        let collected_at = NOW + 102;
        let daemon = store
            .acquire_daemon_gc_lease("daemon", collected_at + 1_000, collected_at)
            .unwrap();
        let receipts = store.gc_with_daemon_lease(&daemon, collected_at).unwrap();
        assert_eq!(
            receipts
                .iter()
                .map(|receipt| receipt.artifact_digest.clone())
                .collect::<Vec<_>>(),
            vec![first]
        );
        let after_gc = store.inventory().unwrap();
        assert!(after_gc.records.contains_key(&second.to_string()));
        assert!(after_gc.records.contains_key(&third.to_string()));
    }

    #[test]
    fn runtime_member_reader_rechecks_the_artifact_identity() {
        let (_dir, store) = store();
        let (manifest, digest) = import_ok(&store, &model_bytes());
        let admitted = store
            .admit_for_runtime(&digest, &manifest, &env(), NOW)
            .expect("admitted artifact");

        assert_eq!(
            admitted
                .read_member_bytes(ArtifactMemberRoleV1::Model)
                .expect("verified model bytes"),
            model_bytes()
        );

        std::fs::write(store.artifact_path(&digest), b"tampered model weights").unwrap();
        assert_eq!(
            admitted.read_member_bytes(ArtifactMemberRoleV1::Model),
            Err(AdmittedArtifactReadErrorV1::Corrupt)
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
            .get(&manifest.artifact_identity_digest().to_string())
            .unwrap();
        assert_eq!(record.state, ArtifactInventoryStateV1::Quarantined);
        assert!(
            !store
                .artifact_dir(&manifest.artifact_identity_digest())
                .exists()
        );
    }

    #[test]
    fn every_package_member_is_verified_before_installation() {
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
                .get(&manifest.artifact_identity_digest().to_string())
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
        let digest = manifest.artifact_identity_digest();
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
        std::fs::write(outside.join("members").join("model.onnx"), b"outside").unwrap();
        std::os::unix::fs::symlink(&outside, &ambient).unwrap();

        store.stage_chunk(&mut session, &bytes, NOW).unwrap();
        assert_eq!(
            std::fs::read(outside.join("members").join("model.onnx")).unwrap(),
            b"outside"
        );
        assert_eq!(
            std::fs::read(held.join("members").join("model.onnx")).unwrap(),
            bytes
        );
    }

    #[cfg(unix)]
    #[test]
    fn held_artifact_and_receipt_components_preserve_replacement_sentinels() {
        let (_dir, store) = store();
        let manifest = manifest_for(b"collectible component race");
        let record = store.record_for(&manifest, ArtifactInventoryStateV1::Verified, NOW, None);
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
        let record = store.record_for(&manifest, ArtifactInventoryStateV1::Verified, NOW, None);
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
        let digest = manifest.artifact_identity_digest();
        let members_path = session.staging_path.join("members");
        drop(session);
        std::fs::rename(members_path, store.artifact_dir(&digest)).unwrap();
        drop(store);

        let reopened =
            ModelArtifactStore::open(store_root, RetentionPolicyV1 { grace_seconds: 100 }).unwrap();
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
        let record = store.record_for(&manifest, ArtifactInventoryStateV1::Verified, NOW, None);
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

        let reopened =
            ModelArtifactStore::open(store_root, RetentionPolicyV1 { grace_seconds: 100 }).unwrap();
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
            let store =
                ModelArtifactStore::open(&store_root, RetentionPolicyV1 { grace_seconds: 100 })
                    .unwrap();
            let manifest = manifest_for(format!("gc crash phase {phase}").as_bytes());
            let record = store.record_for(&manifest, ArtifactInventoryStateV1::Verified, NOW, None);
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

            let reopened =
                ModelArtifactStore::open(&store_root, RetentionPolicyV1 { grace_seconds: 100 })
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
        let record = store.record_for(&manifest, ArtifactInventoryStateV1::Verified, NOW, None);
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

        let reopened =
            ModelArtifactStore::open(store_root, RetentionPolicyV1 { grace_seconds: 100 }).unwrap();
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
        let record = store.record_for(&manifest, ArtifactInventoryStateV1::Verified, NOW, None);
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

        let reopened =
            ModelArtifactStore::open(store_root, RetentionPolicyV1 { grace_seconds: 100 }).unwrap();
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
                    &quarantined_manifest.artifact_identity_digest(),
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
    fn digest_readmission_rejects_runtime_evidence_mismatch() {
        let (_dir, store) = store();
        let (_manifest, digest) = import_ok(&store, &model_bytes());
        let mut runtime = env();
        runtime.build_revision = "different-runtime-build".to_owned();

        assert_eq!(
            store
                .admit_for_runtime_by_digest(&digest, &runtime)
                .unwrap_err(),
            SemanticCapabilityDisabledV1::IncompatibleRuntime
        );
    }

    #[cfg(feature = "semantic-fastembed")]
    #[test]
    fn detected_fastembed_environment_uses_process_evidence() {
        let runtime = RuntimeEnvironmentV1::detect_fastembed_process().unwrap();

        assert_eq!(runtime.os, std::env::consts::OS);
        assert_eq!(runtime.arch, std::env::consts::ARCH);
        assert_eq!(runtime.runtime, FASTEMBED_RUNTIME_FAMILY_V1);
        assert_eq!(runtime.build_revision, FASTEMBED_RUNTIME_BUILD_REVISION_V1);
        assert!(runtime.available_resident_bytes > 0);
        assert!(runtime.available_threads > 0);
    }

    #[test]
    fn digest_readmission_rejects_insufficient_process_memory() {
        let (_dir, store) = store();
        let (_manifest, digest) = import_ok(&store, &model_bytes());
        let mut runtime = env();
        runtime.available_resident_bytes = 1;

        assert_eq!(
            store
                .admit_for_runtime_by_digest(&digest, &runtime)
                .unwrap_err(),
            SemanticCapabilityDisabledV1::ResourceCeilingExceeded
        );
    }

    #[test]
    fn digest_readmission_rejects_insufficient_process_threads() {
        let (_dir, store) = store();
        let (_manifest, digest) = import_ok(&store, &model_bytes());
        let mut runtime = env();
        runtime.available_threads = 1;

        assert_eq!(
            store
                .admit_for_runtime_by_digest(&digest, &runtime)
                .unwrap_err(),
            SemanticCapabilityDisabledV1::ResourceCeilingExceeded
        );
    }

    #[test]
    fn lease_rotation_never_resurrects_a_revoked_prior_active_artifact() {
        let (_dir, store) = store();
        let (_first_manifest, first) = import_ok(&store, &model_bytes());
        store
            .activate_artifact_with_rollback(&first, "active", "rollback", NOW)
            .unwrap();
        store.revoke_artifact(&first, NOW + 1).unwrap();
        let (_second_manifest, second) = import_ok(&store, b"second verified model");

        store
            .activate_artifact_with_rollback(&second, "active", "rollback", NOW + 2)
            .unwrap();

        let inventory = store.inventory().unwrap();
        assert_eq!(
            inventory.records.get(&first.to_string()).unwrap().state,
            ArtifactInventoryStateV1::Revoked
        );
        assert_eq!(
            store
                .artifact_digest_for_lease("rollback", ArtifactLeaseKindV1::Rollback, NOW + 2)
                .unwrap(),
            None
        );
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
        let record = store.record_for(&manifest, ArtifactInventoryStateV1::Verified, NOW, None);
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
