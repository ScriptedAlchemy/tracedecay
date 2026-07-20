//! Digest-addressed model artifact store with verified resumable import
//! (Plan 31 "Model and offline lifecycle", packet
//! `pr10/prep-artifact-manifest`).
//!
//! Layout under the caller-owned root (Plan-02-owned user store at
//! integration; keyed by signed artifact digest, never an ambient cache):
//!
//! ```text
//! <root>/staging/<random>/payload.part        resumable import staging
//! <root>/staging/<random>/import.meta.json    resume identity (declared pins)
//! <root>/artifacts/<sha256-hex>               verified model bytes
//! <root>/inventory.json                       staged|verified|installed|...
//! <root>/receipts/gc.jsonl                    append-only GC receipts
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
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::manifest::{
    ManifestValidationErrorV1, ModelArtifactManifestV1, ResourceCeilingV1, Sha256DigestHex,
};
use super::trust_roots::{
    Ed25519Verifier, SignatureVerificationErrorV1, TrustRootErrorV1, TrustRootSetV1,
};

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
    pub artifact_digest: Sha256DigestHex,
    pub manifest_digest: Sha256DigestHex,
    pub state: ArtifactInventoryStateV1,
    pub recorded_at_unix: u64,
    pub quarantine_reason: Option<String>,
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
    #[error("manifest invalid: {0}")]
    Manifest(#[from] ManifestValidationErrorV1),
    #[error("trust root rejected: {0}")]
    TrustRoot(#[from] TrustRootErrorV1),
    #[error("detached signature does not verify over canonical manifest bytes")]
    SignatureInvalid,
    #[error(
        "staged write exceeds declared byte length: declared {declared}, attempted {attempted}"
    )]
    SizeExpansionBeyondDeclared { declared: u64, attempted: u64 },
    #[error("staged byte length mismatch: declared {declared}, actual {actual}")]
    LengthMismatch { declared: u64, actual: u64 },
    #[error("staged byte digest mismatch: declared {declared}, actual {actual}")]
    DigestMismatch { declared: String, actual: String },
    #[error("no staging session found for id: {0}")]
    StagingNotFound(String),
    #[error("staging session identity does not match the manifest pins")]
    ResumeIdentityMismatch,
    #[error("store io error: {0}")]
    Io(String),
}

impl From<io::Error> for ArtifactImportErrorV1 {
    fn from(value: io::Error) -> Self {
        ArtifactImportErrorV1::Io(value.to_string())
    }
}

/// Semantic-capability disable causes. Admission returns these typed errors;
/// there is no alternative-model field and no fallback selection — a disabled
/// semantic stage preserves the lexical/graph baseline exactly (Plan 31).
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SemanticCapabilityDisabledV1 {
    #[error("artifact not installed: {digest}")]
    MissingArtifact { digest: String },
    #[error("installed artifact bytes fail digest verification: {digest}")]
    CorruptArtifact { digest: String },
    #[error("artifact revoked: {digest}")]
    RevokedArtifact { digest: String },
    #[error("artifact quarantined: {digest}: {reason}")]
    QuarantinedArtifact { digest: String, reason: String },
    #[error("manifest trust root rejected: {0}")]
    UntrustedRoot(TrustRootErrorV1),
    #[error("manifest signature invalid")]
    SignatureInvalid,
    #[error("runtime incompatible: required {required}, found {found}")]
    IncompatibleRuntime { required: String, found: String },
    #[error("platform incompatible: required one of {required:?}, found {found}")]
    IncompatiblePlatform {
        required: Vec<String>,
        found: String,
    },
    #[error(
        "resource ceiling cannot be honored: {field} requires {required}, available {available}"
    )]
    ResourceCeilingExceeded {
        field: String,
        required: u64,
        available: u64,
    },
    #[error("store io error: {0}")]
    Io(String),
}

impl From<io::Error> for SemanticCapabilityDisabledV1 {
    fn from(value: io::Error) -> Self {
        SemanticCapabilityDisabledV1::Io(value.to_string())
    }
}

/// An artifact admitted for runtime use: verified digest-addressed bytes plus
/// the verified manifest. Carrying the path (not the bytes) keeps admission
/// cheap; the runtime adapter re-reads under its own bounds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmittedArtifactV1 {
    pub artifact_digest: Sha256DigestHex,
    pub manifest_digest: Sha256DigestHex,
    pub model_bytes_path: PathBuf,
    pub manifest: ModelArtifactManifestV1,
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

/// Resume identity persisted beside staged bytes. Import is resumable only
/// because the manifest supplies immutable length and digest identity; the
/// sidecar binds the staging directory to those pins.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct StagingMetaV1 {
    manifest_digest: Sha256DigestHex,
    declared_digest: Sha256DigestHex,
    declared_length: u64,
    bytes_written: u64,
}

/// An open import session over one staging directory.
#[derive(Debug)]
pub struct ImportSession {
    staging_dir: PathBuf,
    meta: StagingMetaV1,
}

impl ImportSession {
    pub fn staging_id(&self) -> String {
        self.staging_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub fn bytes_written(&self) -> u64 {
        self.meta.bytes_written
    }
}

/// The digest-addressed, profile-independent model artifact store.
pub struct ModelArtifactStore {
    root: PathBuf,
    trust_roots: TrustRootSetV1,
    verifier: Arc<dyn Ed25519Verifier>,
    retention: RetentionPolicyV1,
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
        trust_roots.validate()?;
        let root = root.into();
        for dir in ["staging", "artifacts", "receipts"] {
            fs::create_dir_all(root.join(dir))?;
        }
        Ok(Self {
            root,
            trust_roots,
            verifier,
            retention,
        })
    }

    fn inventory_path(&self) -> PathBuf {
        self.root.join("inventory.json")
    }

    fn artifact_path(&self, digest: &Sha256DigestHex) -> PathBuf {
        self.root.join("artifacts").join(digest.as_str())
    }

    /// Load the inventory (absent file = empty inventory).
    pub fn inventory(&self) -> Result<ArtifactInventoryV1, ArtifactImportErrorV1> {
        let path = self.inventory_path();
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| ArtifactImportErrorV1::Io(format!("inventory decode: {e}"))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(ArtifactInventoryV1::default()),
            Err(e) => Err(e.into()),
        }
    }

    fn save_inventory(&self, inventory: &ArtifactInventoryV1) -> Result<(), ArtifactImportErrorV1> {
        let bytes = serde_json::to_vec(inventory)
            .map_err(|e| ArtifactImportErrorV1::Io(format!("inventory encode: {e}")))?;
        let tmp = self.root.join("inventory.json.tmp");
        {
            let mut file = File::create(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        fs::rename(&tmp, self.inventory_path())?;
        sync_dir(&self.root)?;
        Ok(())
    }

    /// Verify manifest structure, trust-root admission, and the detached
    /// Ed25519 signature over the canonical payload bytes. Runs BEFORE any
    /// byte is staged, so a bad signature never reaches disk.
    pub fn verify_manifest(
        &self,
        manifest: &ModelArtifactManifestV1,
        now_unix: u64,
    ) -> Result<(), ArtifactImportErrorV1> {
        manifest.validate()?;
        let root = self
            .trust_roots
            .resolve(&manifest.signature.trust_root_id, now_unix)?;
        self.verifier
            .verify_ed25519(
                &root.public_key.to_bytes(),
                &manifest.canonical_bytes(),
                &manifest.signature.signature.to_bytes(),
            )
            .map_err(|_: SignatureVerificationErrorV1| ArtifactImportErrorV1::SignatureInvalid)?;
        Ok(())
    }

    /// Begin a resumable import of caller-provided bytes for a verified
    /// manifest. Stages under a random local directory; no network access.
    pub fn begin_import(
        &self,
        manifest: &ModelArtifactManifestV1,
        now_unix: u64,
    ) -> Result<ImportSession, ArtifactImportErrorV1> {
        self.verify_manifest(manifest, now_unix)?;
        let staging_id = random_staging_id()?;
        let staging_dir = self.root.join("staging").join(&staging_id);
        fs::create_dir_all(&staging_dir)?;
        File::create(staging_dir.join("payload.part"))?;
        let meta = StagingMetaV1 {
            manifest_digest: manifest.canonical_digest(),
            declared_digest: manifest.payload.model_member.digest.clone(),
            declared_length: manifest.payload.model_member.byte_length,
            bytes_written: 0,
        };
        write_staging_meta(&staging_dir, &meta)?;
        Ok(ImportSession { staging_dir, meta })
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
        self.verify_manifest(manifest, now_unix)?;
        let staging_dir = self.root.join("staging").join(staging_id);
        let meta_bytes =
            fs::read(staging_dir.join("import.meta.json")).map_err(|e| match e.kind() {
                io::ErrorKind::NotFound => {
                    ArtifactImportErrorV1::StagingNotFound(staging_id.to_string())
                }
                _ => ArtifactImportErrorV1::Io(e.to_string()),
            })?;
        let meta: StagingMetaV1 = serde_json::from_slice(&meta_bytes)
            .map_err(|e| ArtifactImportErrorV1::Io(format!("staging meta decode: {e}")))?;
        let actual_len = fs::metadata(staging_dir.join("payload.part"))
            .map(|m| m.len())
            .unwrap_or(0);
        let identity_ok = meta.manifest_digest == manifest.canonical_digest()
            && meta.declared_digest == manifest.payload.model_member.digest
            && meta.declared_length == manifest.payload.model_member.byte_length
            && meta.bytes_written == actual_len
            && actual_len <= meta.declared_length;
        if !identity_ok {
            let _ = fs::remove_dir_all(&staging_dir);
            return Err(ArtifactImportErrorV1::ResumeIdentityMismatch);
        }
        Ok(ImportSession { staging_dir, meta })
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
        let attempted = session
            .meta
            .bytes_written
            .saturating_add(bytes.len() as u64);
        if attempted > session.meta.declared_length {
            let declared = session.meta.declared_length;
            self.quarantine_staging(session, "size expansion beyond declared length", now_unix)?;
            return Err(ArtifactImportErrorV1::SizeExpansionBeyondDeclared {
                declared,
                attempted,
            });
        }
        let mut file = OpenOptions::new()
            .append(true)
            .open(session.staging_dir.join("payload.part"))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        session.meta.bytes_written = attempted;
        write_staging_meta(&session.staging_dir, &session.meta)?;
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
        // Signature re-verified at the placement boundary: trust state may
        // have changed between begin and finalize.
        self.verify_manifest(manifest, now_unix)?;
        let payload_path = session.staging_dir.join("payload.part");
        let actual_length = fs::metadata(&payload_path)?.len();
        let declared_length = manifest.payload.model_member.byte_length;
        if actual_length != declared_length {
            self.quarantine_staging(&session, "length mismatch", now_unix)?;
            return Err(ArtifactImportErrorV1::LengthMismatch {
                declared: declared_length,
                actual: actual_length,
            });
        }
        let actual_digest = sha256_file(&payload_path)?;
        let declared_digest = &manifest.payload.model_member.digest;
        if &actual_digest != declared_digest {
            self.quarantine_staging(&session, "digest mismatch", now_unix)?;
            return Err(ArtifactImportErrorV1::DigestMismatch {
                declared: declared_digest.to_string(),
                actual: actual_digest.to_string(),
            });
        }
        let dest = self.artifact_path(declared_digest);
        {
            let file = File::open(&payload_path)?;
            file.sync_all()?;
        }
        fs::rename(&payload_path, &dest)?;
        sync_dir(&self.root.join("artifacts"))?;
        let _ = fs::remove_dir_all(&session.staging_dir);
        let record = ArtifactInventoryRecordV1 {
            artifact_digest: declared_digest.clone(),
            manifest_digest: manifest.canonical_digest(),
            state: ArtifactInventoryStateV1::Installed,
            recorded_at_unix: now_unix,
            quarantine_reason: None,
        };
        let mut inventory = self.inventory()?;
        inventory
            .records
            .insert(declared_digest.to_string(), record.clone());
        self.save_inventory(&inventory)?;
        Ok(record)
    }

    fn quarantine_staging(
        &self,
        session: &ImportSession,
        reason: &str,
        now_unix: u64,
    ) -> Result<(), ArtifactImportErrorV1> {
        let _ = fs::remove_dir_all(&session.staging_dir);
        let mut inventory = self.inventory()?;
        inventory.records.insert(
            session.meta.declared_digest.to_string(),
            ArtifactInventoryRecordV1 {
                artifact_digest: session.meta.declared_digest.clone(),
                manifest_digest: session.meta.manifest_digest.clone(),
                state: ArtifactInventoryStateV1::Quarantined,
                recorded_at_unix: now_unix,
                quarantine_reason: Some(reason.to_string()),
            },
        );
        self.save_inventory(&inventory)
    }

    /// Mark an installed artifact revoked. Revoked artifacts are never
    /// admitted and are protected from GC (revocation evidence is retained).
    pub fn revoke_artifact(
        &self,
        digest: &Sha256DigestHex,
        now_unix: u64,
    ) -> Result<(), ArtifactImportErrorV1> {
        let mut inventory = self.inventory()?;
        if let Some(record) = inventory.records.get_mut(&digest.to_string()) {
            record.state = ArtifactInventoryStateV1::Revoked;
            record.recorded_at_unix = now_unix;
        }
        self.save_inventory(&inventory)
    }

    /// Retain an installed artifact explicitly for rollback; retained
    /// artifacts are never collected.
    pub fn retain_for_rollback(
        &self,
        digest: &Sha256DigestHex,
        now_unix: u64,
    ) -> Result<(), ArtifactImportErrorV1> {
        let mut inventory = self.inventory()?;
        if let Some(record) = inventory.records.get_mut(&digest.to_string()) {
            if record.state == ArtifactInventoryStateV1::Installed {
                record.state = ArtifactInventoryStateV1::RetainedForRollback;
                record.recorded_at_unix = now_unix;
            }
        }
        self.save_inventory(&inventory)
    }

    /// Admit an installed artifact for runtime use against host evidence.
    /// Re-verifies trust + signature + on-disk digest; any corrupt, revoked,
    /// quarantined, or incompatible artifact disables the semantic capability
    /// with a typed error and no substitution.
    pub fn admit_for_runtime(
        &self,
        digest: &Sha256DigestHex,
        manifest: &ModelArtifactManifestV1,
        env: &RuntimeEnvironmentV1,
        now_unix: u64,
    ) -> Result<AdmittedArtifactV1, SemanticCapabilityDisabledV1> {
        let inventory = self
            .inventory()
            .map_err(|e| SemanticCapabilityDisabledV1::Io(e.to_string()))?;
        let record = inventory.records.get(&digest.to_string()).ok_or_else(|| {
            SemanticCapabilityDisabledV1::MissingArtifact {
                digest: digest.to_string(),
            }
        })?;
        match record.state {
            ArtifactInventoryStateV1::Installed | ArtifactInventoryStateV1::RetainedForRollback => {
            }
            ArtifactInventoryStateV1::Revoked => {
                return Err(SemanticCapabilityDisabledV1::RevokedArtifact {
                    digest: digest.to_string(),
                });
            }
            ArtifactInventoryStateV1::Quarantined => {
                return Err(SemanticCapabilityDisabledV1::QuarantinedArtifact {
                    digest: digest.to_string(),
                    reason: record
                        .quarantine_reason
                        .clone()
                        .unwrap_or_else(|| "unspecified".to_string()),
                });
            }
            ArtifactInventoryStateV1::Staged | ArtifactInventoryStateV1::Verified => {
                return Err(SemanticCapabilityDisabledV1::MissingArtifact {
                    digest: digest.to_string(),
                });
            }
        }
        let root = self
            .trust_roots
            .resolve(&manifest.signature.trust_root_id, now_unix)
            .map_err(SemanticCapabilityDisabledV1::UntrustedRoot)?;
        self.verifier
            .verify_ed25519(
                &root.public_key.to_bytes(),
                &manifest.canonical_bytes(),
                &manifest.signature.signature.to_bytes(),
            )
            .map_err(|_: SignatureVerificationErrorV1| {
                SemanticCapabilityDisabledV1::SignatureInvalid
            })?;
        let path = self.artifact_path(digest);
        let actual = sha256_file(&path).map_err(SemanticCapabilityDisabledV1::from)?;
        if &actual != digest {
            return Err(SemanticCapabilityDisabledV1::CorruptArtifact {
                digest: digest.to_string(),
            });
        }
        check_compatibility(&manifest.payload.runtime, env)?;
        check_resource_ceiling(&manifest.payload.resource_ceiling, env)?;
        Ok(AdmittedArtifactV1 {
            artifact_digest: digest.clone(),
            manifest_digest: manifest.canonical_digest(),
            model_bytes_path: path,
            manifest: manifest.clone(),
        })
    }

    /// Garbage-collect unreferenced artifacts past the grace window.
    /// `RetainedForRollback`, `Revoked`, and `Installed` records are never
    /// collected here; each removal appends one receipt to
    /// `receipts/gc.jsonl`.
    pub fn gc(&self, now_unix: u64) -> Result<Vec<GcReceiptV1>, ArtifactImportErrorV1> {
        let mut inventory = self.inventory()?;
        let mut receipts = Vec::new();
        let collectible: Vec<String> = inventory
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
            .map(|r| r.artifact_digest.to_string())
            .collect();
        if collectible.is_empty() {
            return Ok(receipts);
        }
        for key in &collectible {
            if let Some(record) = inventory.records.remove(key) {
                let _ = fs::remove_file(self.artifact_path(&record.artifact_digest));
                receipts.push(GcReceiptV1 {
                    artifact_digest: record.artifact_digest,
                    removed_at_unix: now_unix,
                    prior_state: record.state,
                });
            }
        }
        self.save_inventory(&inventory)?;
        let receipts_path = self.root.join("receipts").join("gc.jsonl");
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(receipts_path)?;
        for receipt in &receipts {
            let line = serde_json::to_string(receipt)
                .map_err(|e| ArtifactImportErrorV1::Io(format!("gc receipt encode: {e}")))?;
            file.write_all(line.as_bytes())?;
            file.write_all(b"\n")?;
        }
        file.sync_all()?;
        Ok(receipts)
    }
}

fn check_compatibility(
    required: &super::manifest::RuntimeCompatibilityV1,
    env: &RuntimeEnvironmentV1,
) -> Result<(), SemanticCapabilityDisabledV1> {
    if required.runtime != env.runtime || required.build_revision != env.build_revision {
        return Err(SemanticCapabilityDisabledV1::IncompatibleRuntime {
            required: format!("{}@{}", required.runtime, required.build_revision),
            found: format!("{}@{}", env.runtime, env.build_revision),
        });
    }
    let supported: Vec<String> = required
        .platforms
        .iter()
        .map(|p| format!("{}/{}", p.os, p.arch))
        .collect();
    let found = format!("{}/{}", env.os, env.arch);
    if !required
        .platforms
        .iter()
        .any(|p| p.os == env.os && p.arch == env.arch)
    {
        return Err(SemanticCapabilityDisabledV1::IncompatiblePlatform {
            required: supported,
            found,
        });
    }
    Ok(())
}

fn check_resource_ceiling(
    ceiling: &ResourceCeilingV1,
    env: &RuntimeEnvironmentV1,
) -> Result<(), SemanticCapabilityDisabledV1> {
    if env.available_resident_bytes < ceiling.max_resident_bytes {
        return Err(SemanticCapabilityDisabledV1::ResourceCeilingExceeded {
            field: "max_resident_bytes".to_string(),
            required: ceiling.max_resident_bytes,
            available: env.available_resident_bytes,
        });
    }
    if env.available_threads < ceiling.max_threads {
        return Err(SemanticCapabilityDisabledV1::ResourceCeilingExceeded {
            field: "max_threads".to_string(),
            required: ceiling.max_threads as u64,
            available: env.available_threads as u64,
        });
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<Sha256DigestHex, io::Error> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Sha256DigestHex::new(hex::encode(hasher.finalize()))
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

fn write_staging_meta(dir: &Path, meta: &StagingMetaV1) -> Result<(), ArtifactImportErrorV1> {
    let bytes = serde_json::to_vec(meta)
        .map_err(|e| ArtifactImportErrorV1::Io(format!("staging meta encode: {e}")))?;
    let mut file = File::create(dir.join("import.meta.json"))?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(())
}

fn random_staging_id() -> Result<String, ArtifactImportErrorV1> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes)
        .map_err(|e| ArtifactImportErrorV1::Io(format!("staging id randomness: {e}")))?;
    Ok(hex::encode(bytes))
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<(), io::Error> {
    File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> Result<(), io::Error> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::manifest::*;
    use super::super::trust_roots::test_support::*;
    use super::super::trust_roots::*;
    use super::*;
    use std::sync::Arc;

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

    fn manifest_for(bytes: &[u8]) -> ModelArtifactManifestV1 {
        let payload = ManifestSignedPayloadV1 {
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
        assert_eq!(admitted.artifact_digest, digest);
        assert!(admitted.model_bytes_path.exists());
        // Staging drained; layout is digest-addressed.
        assert_eq!(
            std::fs::read_dir(store.root.join("staging"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(
            std::fs::read(&admitted.model_bytes_path).unwrap(),
            model_bytes()
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
            ArtifactImportErrorV1::TrustRoot(TrustRootErrorV1::Unknown { .. })
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
            ArtifactImportErrorV1::TrustRoot(TrustRootErrorV1::Revoked { .. })
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
            ArtifactImportErrorV1::DigestMismatch { .. }
        ));
        let inventory = store.inventory().unwrap();
        let record = inventory
            .records
            .get(&manifest.payload.model_member.digest.to_string())
            .unwrap();
        assert_eq!(record.state, ArtifactInventoryStateV1::Quarantined);
        assert!(
            !store
                .artifact_path(&manifest.payload.model_member.digest)
                .exists()
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
            ArtifactImportErrorV1::LengthMismatch { .. }
        ));

        // Over-long write -> size expansion rejected at stage time.
        let mut over = store.begin_import(&manifest, NOW).unwrap();
        let mut too_much = model_bytes();
        too_much.push(0);
        assert!(matches!(
            store.stage_chunk(&mut over, &too_much, NOW).unwrap_err(),
            ArtifactImportErrorV1::SizeExpansionBeyondDeclared { .. }
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
    fn revoked_and_quarantined_artifacts_disable_semantics_without_substitution() {
        let (_dir, store) = store();
        let (manifest, digest) = import_ok(&store, &model_bytes());
        store.revoke_artifact(&digest, NOW).unwrap();
        assert_eq!(
            store
                .admit_for_runtime(&digest, &manifest, &env(), NOW)
                .unwrap_err(),
            SemanticCapabilityDisabledV1::RevokedArtifact {
                digest: digest.to_string()
            }
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
                    &quarantined_manifest.payload.model_member.digest,
                    &quarantined_manifest,
                    &env(),
                    NOW
                )
                .unwrap_err(),
            SemanticCapabilityDisabledV1::QuarantinedArtifact { .. }
        ));
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
            SemanticCapabilityDisabledV1::IncompatiblePlatform { .. }
        ));

        let mut bad_runtime = env();
        bad_runtime.build_revision = "rev-2".to_string();
        assert!(matches!(
            store
                .admit_for_runtime(&digest, &manifest, &bad_runtime, NOW)
                .unwrap_err(),
            SemanticCapabilityDisabledV1::IncompatibleRuntime { .. }
        ));

        let mut low_memory = env();
        low_memory.available_resident_bytes = 10;
        assert!(matches!(
            store
                .admit_for_runtime(&digest, &manifest, &low_memory, NOW)
                .unwrap_err(),
            SemanticCapabilityDisabledV1::ResourceCeilingExceeded { .. }
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
            SemanticCapabilityDisabledV1::CorruptArtifact {
                digest: digest.to_string()
            }
        );
    }

    #[test]
    fn gc_collects_unreferenced_past_grace_and_appends_receipt() {
        let (_dir, store) = store();
        // Seed an unreferenced Verified record directly.
        let digest = Sha256DigestHex::of_bytes(b"orphan verified artifact");
        std::fs::write(store.artifact_path(&digest), b"orphan").unwrap();
        let mut inventory = store.inventory().unwrap();
        inventory.records.insert(
            digest.to_string(),
            ArtifactInventoryRecordV1 {
                artifact_digest: digest.clone(),
                manifest_digest: Sha256DigestHex::of_bytes(b"orphan manifest"),
                state: ArtifactInventoryStateV1::Verified,
                recorded_at_unix: NOW,
                quarantine_reason: None,
            },
        );
        store.save_inventory(&inventory).unwrap();

        // Within grace: retained.
        assert!(store.gc(NOW + 50).unwrap().is_empty());
        assert!(store.artifact_path(&digest).exists());

        // Past grace: collected with an append-only receipt.
        let receipts = store.gc(NOW + 150).unwrap();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].artifact_digest, digest);
        assert_eq!(receipts[0].prior_state, ArtifactInventoryStateV1::Verified);
        assert!(!store.artifact_path(&digest).exists());
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
        assert_eq!(admitted.artifact_digest, digest_b);
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
}
