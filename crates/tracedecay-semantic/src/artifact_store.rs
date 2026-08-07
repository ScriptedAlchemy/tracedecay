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
/// Exact runtime identity recorded in manifests, projection keys, and runtime
/// admission evidence. It must name the crate versions this binary actually
/// links (`fastembed` is pinned exactly in Cargo.toml); a runtime upgrade must
/// update this revision so vector generations replay under the new identity.
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

mod gc_recovery;
mod import;
mod io_primitives;
mod lease_admission;
mod paths;
use io_primitives::*;

include!("artifact_store/tests.rs");
