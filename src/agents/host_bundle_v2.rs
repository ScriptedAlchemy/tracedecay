//! Manifest-driven host bundle lifecycle contracts (Plan 27 PR13).
//!
//! This module plans host-registration mutations only after an external Plan
//! 20 verifier accepts the signed manifest. It contains no signing key,
//! credential, daemon lifecycle, product semantics, or host-specific business
//! authority.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsMaybeDirExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions as CapOpenOptions};
use ed25519_dalek::{Signature, VerifyingKey};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_domain::canonical_json_bytes;

const HOST_BUNDLE_SCHEMA_VERSION: u16 = 1;
const MAX_MANIFEST_ARTIFACTS: usize = 128;
const MAX_RELATIVE_PATH_BYTES: usize = 512;
const MAX_IDENTIFIER_BYTES: usize = 128;
const ED25519_SIGNATURE_BYTES: usize = 64;
const MAX_ARTIFACT_CONTENT_BYTES: usize = 1024 * 1024;
const HOST_BUNDLE_RECEIPT_SCHEMA_VERSION: u16 = 1;
const HOST_BUNDLE_CONTROL_DIR: &str = ".tracedecay-host-bundle-v1";
const HOST_BUNDLE_JOURNAL_FILE: &str = "journal.v1.json";
const HOST_BUNDLE_LOCK_FILE: &str = "writer.v1.lock";
const MAX_CONTROL_FILE_BYTES: usize = 256 * 1024;
static HOST_BUNDLE_TEMP_NONCE: AtomicU64 = AtomicU64::new(1);

/// Hosts and host surfaces covered by the five-host stock conformance set.
/// Cursor desktop/cloud remain projections of one Cursor integration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostKindV1 {
    ClaudeCode,
    CursorDesktop,
    CursorCloud,
    Codex,
    Hermes,
    Kiro,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostBundleComponentV1 {
    Core,
    ContextMcp,
    OperatorMcp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostBundleLifecycleOpV1 {
    Install,
    Update,
    Repair,
    Uninstall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostCapabilityV1 {
    Lsp,
    NativeDiagnostics,
    Hooks,
    Mcp,
    Cli,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostCapabilityUnavailableReasonV1 {
    HostApiAbsent,
    HostRegistrationUnsupported,
    NativeFixtureLimited,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "reason", rename_all = "snake_case")]
pub enum HostCapabilityStateV1 {
    Supported,
    Degraded(HostCapabilityUnavailableReasonV1),
    Unavailable(HostCapabilityUnavailableReasonV1),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostCapabilityRecordV1 {
    pub capability: HostCapabilityV1,
    pub state: HostCapabilityStateV1,
}

/// Static host-surface facts only. Runtime installation and availability are
/// observed elsewhere and must not be inferred from this matrix.
pub fn stock_host_capabilities(host: HostKindV1) -> [HostCapabilityRecordV1; 5] {
    use HostCapabilityStateV1::{Degraded, Supported, Unavailable};
    use HostCapabilityUnavailableReasonV1::{
        HostApiAbsent, HostRegistrationUnsupported, NativeFixtureLimited,
    };
    use HostCapabilityV1::{Cli, Hooks, Lsp, Mcp, NativeDiagnostics};

    let (lsp, native_diagnostics, hooks) = match host {
        HostKindV1::ClaudeCode => (Supported, Unavailable(HostApiAbsent), Supported),
        HostKindV1::CursorDesktop => (
            Unavailable(HostRegistrationUnsupported),
            Supported,
            Supported,
        ),
        HostKindV1::CursorCloud | HostKindV1::Codex | HostKindV1::Hermes => (
            Unavailable(HostRegistrationUnsupported),
            Unavailable(HostApiAbsent),
            Supported,
        ),
        HostKindV1::Kiro => (
            Unavailable(HostRegistrationUnsupported),
            Unavailable(HostApiAbsent),
            Degraded(NativeFixtureLimited),
        ),
    };
    [
        HostCapabilityRecordV1 {
            capability: Lsp,
            state: lsp,
        },
        HostCapabilityRecordV1 {
            capability: NativeDiagnostics,
            state: native_diagnostics,
        },
        HostCapabilityRecordV1 {
            capability: Hooks,
            state: hooks,
        },
        HostCapabilityRecordV1 {
            capability: Mcp,
            state: Supported,
        },
        HostCapabilityRecordV1 {
            capability: Cli,
            state: Supported,
        },
    ]
}

/// One generated artifact. Contents and credentials never enter the manifest;
/// the signed digest identifies bytes obtained from the verified bundle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBundleArtifactV1 {
    pub relative_path: String,
    pub artifact_digest: [u8; 32],
    pub ownership_marker: String,
}

/// Generated signed projection for one host/component. It references the one
/// integration/catalog authority and duplicates no workflow semantics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBundleManifestV1 {
    pub schema_version: u16,
    pub host: HostKindV1,
    pub component: HostBundleComponentV1,
    pub integration_manifest_digest: [u8; 32],
    pub catalog_digest: [u8; 32],
    pub configuration_snapshot_id: String,
    pub effective_behavior_digest: [u8; 32],
    pub resolution_provenance_digest: [u8; 32],
    pub protocol_min: u16,
    pub protocol_max: u16,
    pub signer_key_id: String,
    pub signature: Vec<u8>,
    pub artifacts: Vec<HostBundleArtifactV1>,
}

impl HostBundleManifestV1 {
    pub fn validate_structure(&self) -> Result<(), HostBundleError> {
        if self.schema_version != HOST_BUNDLE_SCHEMA_VERSION {
            return Err(HostBundleError::UnsupportedManifestVersion);
        }
        if self.integration_manifest_digest == [0; 32]
            || self.catalog_digest == [0; 32]
            || self.effective_behavior_digest == [0; 32]
            || self.resolution_provenance_digest == [0; 32]
            || self.protocol_min == 0
            || self.protocol_min > self.protocol_max
        {
            return Err(HostBundleError::InvalidManifest);
        }
        validate_identifier(&self.configuration_snapshot_id)?;
        validate_identifier(&self.signer_key_id)?;
        if self.signature.len() != ED25519_SIGNATURE_BYTES {
            return Err(HostBundleError::InvalidManifest);
        }
        if self.artifacts.is_empty() || self.artifacts.len() > MAX_MANIFEST_ARTIFACTS {
            return Err(HostBundleError::InvalidManifest);
        }
        for (index, artifact) in self.artifacts.iter().enumerate() {
            validate_relative_install_path(Path::new(&artifact.relative_path))?;
            validate_identifier(&artifact.ownership_marker)?;
            if artifact.artifact_digest == [0; 32]
                || self.artifacts[..index]
                    .iter()
                    .any(|existing| existing.relative_path == artifact.relative_path)
            {
                return Err(HostBundleError::InvalidManifest);
            }
        }
        Ok(())
    }

    /// RFC 8785-style canonical JSON bytes for the detached Ed25519 payload.
    /// The signature itself is intentionally absent to avoid self-reference.
    pub fn canonical_signed_bytes(&self) -> Result<Vec<u8>, HostBundleError> {
        canonical_json_bytes(&HostBundleSignedPayloadV1 {
            schema_version: self.schema_version,
            host: self.host,
            component: self.component,
            integration_manifest_digest: self.integration_manifest_digest,
            catalog_digest: self.catalog_digest,
            configuration_snapshot_id: &self.configuration_snapshot_id,
            effective_behavior_digest: self.effective_behavior_digest,
            resolution_provenance_digest: self.resolution_provenance_digest,
            protocol_min: self.protocol_min,
            protocol_max: self.protocol_max,
            signer_key_id: &self.signer_key_id,
            artifacts: &self.artifacts,
        })
        .map_err(|_| HostBundleError::CanonicalizationFailed)
    }

    pub fn canonical_signed_digest(&self) -> Result<[u8; 32], HostBundleError> {
        Ok(Sha256::digest(self.canonical_signed_bytes()?).into())
    }
}

#[derive(Serialize)]
struct HostBundleSignedPayloadV1<'a> {
    schema_version: u16,
    host: HostKindV1,
    component: HostBundleComponentV1,
    integration_manifest_digest: [u8; 32],
    catalog_digest: [u8; 32],
    configuration_snapshot_id: &'a str,
    effective_behavior_digest: [u8; 32],
    resolution_provenance_digest: [u8; 32],
    protocol_min: u16,
    protocol_max: u16,
    signer_key_id: &'a str,
    artifacts: &'a [HostBundleArtifactV1],
}

/// Plan 20 supplies trusted/revocation-checked public keys. Host bundles never
/// accept a trust root from the bundle being verified.
pub trait HostBundleTrustResolverV1 {
    fn resolve_ed25519_public_key(&self, signer_key_id: &str) -> Result<[u8; 32], HostBundleError>;
}

/// Narrow verifier adapter so a caller can use the real built-in Ed25519
/// implementation or an approved test adapter without introducing a signing
/// key into host installation code.
pub trait HostBundleVerificationAdapterV1 {
    fn verify_manifest(&self, manifest: &HostBundleManifestV1) -> Result<(), HostBundleError>;
}

/// Actual JCS/Ed25519 verifier adapter used by production wiring.
#[derive(Clone, Debug)]
pub struct JcsEd25519HostBundleVerifierV1<R> {
    trust_resolver: R,
}

impl<R> JcsEd25519HostBundleVerifierV1<R> {
    pub const fn new(trust_resolver: R) -> Self {
        Self { trust_resolver }
    }
}

impl<R: HostBundleTrustResolverV1> HostBundleVerificationAdapterV1
    for JcsEd25519HostBundleVerifierV1<R>
{
    fn verify_manifest(&self, manifest: &HostBundleManifestV1) -> Result<(), HostBundleError> {
        manifest.validate_structure()?;
        let public_key = self
            .trust_resolver
            .resolve_ed25519_public_key(&manifest.signer_key_id)
            .map_err(|_| HostBundleError::VerificationFailed)?;
        let signature: [u8; ED25519_SIGNATURE_BYTES] = manifest
            .signature
            .as_slice()
            .try_into()
            .map_err(|_| HostBundleError::VerificationFailed)?;
        VerifyingKey::from_bytes(&public_key)
            .map_err(|_| HostBundleError::VerificationFailed)?
            .verify_strict(
                &manifest.canonical_signed_bytes()?,
                &Signature::from_bytes(&signature),
            )
            .map_err(|_| HostBundleError::VerificationFailed)
    }
}

/// Verify a signed bundle first, then produce its lifecycle plan. This keeps
/// the older closure-based planner compatible while giving production callers
/// one concrete verification contract.
pub fn plan_verified_lifecycle_mutation(
    manifest: &HostBundleManifestV1,
    request: &HostBundleLifecycleRequestV1,
    observed: &[ObservedHostArtifactV1],
    verifier: &impl HostBundleVerificationAdapterV1,
) -> Result<HostBundleMutationPlanV1, HostBundleError> {
    plan_lifecycle_mutation(manifest, request, observed, |manifest| {
        verifier.verify_manifest(manifest)
    })
}

/// Verify first, then produce the full immutable lifecycle plan, including
/// receipt-derived orphan removals for update, repair, and uninstall.
pub fn plan_verified_complete_lifecycle_mutation(
    manifest: &HostBundleManifestV1,
    request: &HostBundleLifecycleRequestV1,
    manifest_observed: &[ObservedHostArtifactV1],
    owned_receipt: Option<&HostBundleInstallReceiptV1>,
    orphan_observed: &[ObservedHostArtifactV1],
    verifier: &impl HostBundleVerificationAdapterV1,
) -> Result<HostBundleMutationPlanV1, HostBundleError> {
    plan_complete_lifecycle_mutation(
        manifest,
        request,
        manifest_observed,
        owned_receipt,
        orphan_observed,
        |manifest| verifier.verify_manifest(manifest),
    )
}

fn validate_identifier(value: &str) -> Result<(), HostBundleError> {
    if value.is_empty()
        || value.len() > MAX_IDENTIFIER_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(HostBundleError::InvalidManifest);
    }
    Ok(())
}

/// Lexically validate a manifest path. Absolute paths, parent traversal,
/// platform prefixes, NUL, and ambiguous `.` components are rejected.
pub fn validate_relative_install_path(path: &Path) -> Result<(), HostBundleError> {
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.is_empty()
        || bytes.len() > MAX_RELATIVE_PATH_BYTES
        || bytes.contains(&0)
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::Prefix(_)
                    | Component::RootDir
                    | Component::ParentDir
                    | Component::CurDir
            )
        })
    {
        return Err(HostBundleError::UnsafeInstallPath);
    }
    Ok(())
}

/// Resolve a validated target while rejecting a symlink at the install root or
/// any already-existing path component. Missing descendants are permitted;
/// the writer must create them without following links and recheck at commit.
pub fn inspect_install_target(root: &Path, relative: &Path) -> Result<PathBuf, HostBundleError> {
    validate_relative_install_path(relative)?;
    if std::fs::symlink_metadata(root)
        .map_err(|_| HostBundleError::UnsafeInstallPath)?
        .file_type()
        .is_symlink()
    {
        return Err(HostBundleError::UnsafeInstallPath);
    }
    let mut target = root.to_path_buf();
    for component in relative.components() {
        target.push(component.as_os_str());
        match std::fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(HostBundleError::UnsafeInstallPath);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(HostBundleError::UnsafeInstallPath),
        }
    }
    Ok(target)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservedArtifactKindV1 {
    Missing,
    RegularFile,
    Directory,
    Symlink,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedHostArtifactV1 {
    pub relative_path: String,
    pub kind: ObservedArtifactKindV1,
    pub artifact_digest: Option<[u8; 32]>,
    pub ownership_marker: Option<String>,
    /// Digest last recorded by the component's durable ownership receipt.
    /// This is distinct from the bytes currently observed on disk.
    pub owned_artifact_digest: Option<[u8; 32]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostArtifactActionV1 {
    Noop,
    WriteNew,
    BackupThenReplace,
    BackupThenRemove,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostArtifactMutationV1 {
    pub relative_path: String,
    pub action: HostArtifactActionV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostBundleLifecycleRequestV1 {
    pub operation: HostBundleLifecycleOpV1,
    pub expected_host: HostKindV1,
    pub expected_component: HostBundleComponentV1,
    pub explicit_confirmation: bool,
    /// Hermes has one user-profile binding. Other hosts must pass zero here;
    /// this is not an ambient profile-discovery mechanism.
    pub hermes_profile_bindings: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostBundleMutationPlanV1 {
    pub operation: HostBundleLifecycleOpV1,
    pub host: HostKindV1,
    pub component: HostBundleComponentV1,
    pub mutations: Vec<HostArtifactMutationV1>,
    pub rollback_required: bool,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum HostBundleError {
    #[error("host capability is unsupported and must not be emulated")]
    UnsupportedCapability,
    #[error("bundle manifest schema version is unsupported")]
    UnsupportedManifestVersion,
    #[error("bundle manifest is structurally invalid")]
    InvalidManifest,
    #[error("bundle signature, trust record, or signed digests are invalid")]
    VerificationFailed,
    #[error("bundle signed payload cannot be canonicalized")]
    CanonicalizationFailed,
    #[error("bundle does not address the requested host/component")]
    WrongTarget,
    #[error("lifecycle mutation requires explicit confirmation")]
    ConfirmationRequired,
    #[error("bundle ownership marker conflicts or is ambiguous")]
    OwnershipConflict,
    #[error("install target is absolute, traversing, symlinked, or otherwise unsafe")]
    UnsafeInstallPath,
    #[error("observed installation state is incomplete or duplicated")]
    InvalidObservedState,
    #[error("Hermes must bind exactly one user TraceDecay profile")]
    InvalidHermesProfileBinding,
    #[error("bundle artifact content is missing, oversized, duplicated, or digest-mismatched")]
    ArtifactContentMismatch,
    #[error("host bundle receipt or operation journal is invalid")]
    ReceiptCorrupted,
    #[error("host bundle atomic filesystem operation failed")]
    StorageFailure,
    #[error("host bundle interrupted operation requires recovery before mutation")]
    RecoveryRequired,
}

/// Refuse silent emulation of unsupported/degraded host capabilities.
pub fn require_capability(
    host: HostKindV1,
    capability: HostCapabilityV1,
) -> Result<(), HostBundleError> {
    let record = stock_host_capabilities(host)
        .into_iter()
        .find(|record| record.capability == capability)
        .ok_or(HostBundleError::UnsupportedCapability)?;
    if record.state == HostCapabilityStateV1::Supported {
        Ok(())
    } else {
        Err(HostBundleError::UnsupportedCapability)
    }
}

/// Verify first, then produce a mutation-only plan. The verifier is the Plan
/// 20 authority and must check canonical signed bytes, key trust/revocation,
/// expiry, configuration/provenance digests, and protocol compatibility.
pub fn plan_lifecycle_mutation(
    manifest: &HostBundleManifestV1,
    request: &HostBundleLifecycleRequestV1,
    observed: &[ObservedHostArtifactV1],
    verify: impl FnOnce(&HostBundleManifestV1) -> Result<(), HostBundleError>,
) -> Result<HostBundleMutationPlanV1, HostBundleError> {
    manifest.validate_structure()?;
    verify(manifest).map_err(|_| HostBundleError::VerificationFailed)?;
    if manifest.host != request.expected_host || manifest.component != request.expected_component {
        return Err(HostBundleError::WrongTarget);
    }
    if !request.explicit_confirmation {
        return Err(HostBundleError::ConfirmationRequired);
    }
    match manifest.host {
        HostKindV1::Hermes if request.hermes_profile_bindings != 1 => {
            return Err(HostBundleError::InvalidHermesProfileBinding);
        }
        HostKindV1::Hermes => {}
        _ if request.hermes_profile_bindings != 0 => {
            return Err(HostBundleError::InvalidHermesProfileBinding);
        }
        _ => {}
    }

    for (index, state) in observed.iter().enumerate() {
        validate_relative_install_path(Path::new(&state.relative_path))?;
        if observed[..index]
            .iter()
            .any(|existing| existing.relative_path == state.relative_path)
        {
            return Err(HostBundleError::InvalidObservedState);
        }
    }

    let mut mutations = Vec::with_capacity(manifest.artifacts.len());
    for artifact in &manifest.artifacts {
        let state = observed
            .iter()
            .find(|state| state.relative_path == artifact.relative_path);
        let action = plan_artifact_action(request.operation, artifact, state)?;
        mutations.push(HostArtifactMutationV1 {
            relative_path: artifact.relative_path.clone(),
            action,
        });
    }
    let rollback_required = mutations.iter().any(|mutation| {
        matches!(
            mutation.action,
            HostArtifactActionV1::BackupThenReplace | HostArtifactActionV1::BackupThenRemove
        )
    });
    Ok(HostBundleMutationPlanV1 {
        operation: request.operation,
        host: manifest.host,
        component: manifest.component,
        mutations,
        rollback_required,
    })
}

/// Produce the complete immutable mutation plan for execution. Manifest
/// observations drive install/update/repair actions; the optional ownership
/// receipt plus orphan observations drive receipt-derived removals.
pub fn plan_complete_lifecycle_mutation(
    manifest: &HostBundleManifestV1,
    request: &HostBundleLifecycleRequestV1,
    manifest_observed: &[ObservedHostArtifactV1],
    owned_receipt: Option<&HostBundleInstallReceiptV1>,
    orphan_observed: &[ObservedHostArtifactV1],
    verify: impl FnOnce(&HostBundleManifestV1) -> Result<(), HostBundleError>,
) -> Result<HostBundleMutationPlanV1, HostBundleError> {
    for (index, state) in orphan_observed.iter().enumerate() {
        validate_relative_install_path(Path::new(&state.relative_path))?;
        if orphan_observed[..index]
            .iter()
            .any(|existing| existing.relative_path == state.relative_path)
        {
            return Err(HostBundleError::InvalidObservedState);
        }
    }

    let mut plan = if request.operation == HostBundleLifecycleOpV1::Uninstall {
        // A signed uninstall request authorizes lifecycle execution, but only
        // the durable ownership receipt identifies removable files.
        plan_lifecycle_mutation(manifest, request, &[], verify)?
    } else {
        plan_lifecycle_mutation(manifest, request, manifest_observed, verify)?
    };
    if request.operation == HostBundleLifecycleOpV1::Uninstall {
        plan.mutations.clear();
    }
    if matches!(
        request.operation,
        HostBundleLifecycleOpV1::Update
            | HostBundleLifecycleOpV1::Repair
            | HostBundleLifecycleOpV1::Uninstall
    ) {
        for owned in owned_receipt
            .into_iter()
            .flat_map(|receipt| &receipt.artifacts)
        {
            if request.operation != HostBundleLifecycleOpV1::Uninstall
                && manifest
                    .artifacts
                    .iter()
                    .any(|artifact| artifact.relative_path == owned.relative_path)
            {
                continue;
            }
            let observed = orphan_observed
                .iter()
                .find(|state| state.relative_path == owned.relative_path)
                .ok_or(HostBundleError::InvalidObservedState)?;
            let artifact = HostBundleArtifactV1 {
                relative_path: owned.relative_path.clone(),
                artifact_digest: owned.artifact_digest,
                ownership_marker: owned.ownership_marker.clone(),
            };
            plan.mutations.push(HostArtifactMutationV1 {
                relative_path: owned.relative_path.clone(),
                action: plan_artifact_action(
                    HostBundleLifecycleOpV1::Uninstall,
                    &artifact,
                    Some(observed),
                )?,
            });
        }
    }
    plan.rollback_required = plan.mutations.iter().any(|mutation| {
        matches!(
            mutation.action,
            HostArtifactActionV1::BackupThenReplace | HostArtifactActionV1::BackupThenRemove
        )
    });
    Ok(plan)
}

fn plan_artifact_action(
    operation: HostBundleLifecycleOpV1,
    artifact: &HostBundleArtifactV1,
    state: Option<&ObservedHostArtifactV1>,
) -> Result<HostArtifactActionV1, HostBundleError> {
    let Some(state) = state else {
        return match operation {
            HostBundleLifecycleOpV1::Install
            | HostBundleLifecycleOpV1::Update
            | HostBundleLifecycleOpV1::Repair => Ok(HostArtifactActionV1::WriteNew),
            HostBundleLifecycleOpV1::Uninstall => Ok(HostArtifactActionV1::Noop),
        };
    };
    match state.kind {
        ObservedArtifactKindV1::Missing => return plan_artifact_action(operation, artifact, None),
        ObservedArtifactKindV1::Symlink | ObservedArtifactKindV1::Directory => {
            return Err(HostBundleError::UnsafeInstallPath);
        }
        ObservedArtifactKindV1::RegularFile => {}
    }
    if state.ownership_marker.as_deref() != Some(artifact.ownership_marker.as_str()) {
        return Err(HostBundleError::OwnershipConflict);
    }
    let owned_digest = state
        .owned_artifact_digest
        .ok_or(HostBundleError::InvalidObservedState)?;
    match operation {
        HostBundleLifecycleOpV1::Uninstall => {
            if state.artifact_digest == Some(owned_digest) {
                Ok(HostArtifactActionV1::BackupThenRemove)
            } else {
                Err(HostBundleError::OwnershipConflict)
            }
        }
        HostBundleLifecycleOpV1::Install => {
            if state.artifact_digest == Some(artifact.artifact_digest) {
                Ok(HostArtifactActionV1::Noop)
            } else {
                Err(HostBundleError::OwnershipConflict)
            }
        }
        HostBundleLifecycleOpV1::Update => {
            if state.artifact_digest == Some(artifact.artifact_digest) {
                Ok(HostArtifactActionV1::Noop)
            } else if state.artifact_digest == Some(owned_digest) {
                Ok(HostArtifactActionV1::BackupThenReplace)
            } else {
                Err(HostBundleError::OwnershipConflict)
            }
        }
        HostBundleLifecycleOpV1::Repair => {
            if state.artifact_digest == Some(artifact.artifact_digest) {
                Ok(HostArtifactActionV1::Noop)
            } else {
                Ok(HostArtifactActionV1::BackupThenReplace)
            }
        }
    }
}

/// Bytes obtained from the verified host bundle. They are checked against the
/// signed artifact digest before any host path is touched and are never copied
/// into receipts or journals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostBundleArtifactContentV1 {
    pub relative_path: String,
    pub bytes: Vec<u8>,
}

/// Execution-specific input kept separate from the public lifecycle request
/// so existing plan consumers do not accidentally gain filesystem authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostBundleExecutionRequestV1 {
    pub lifecycle: HostBundleLifecycleRequestV1,
    pub operation_id: [u8; 16],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBundleReceiptArtifactV1 {
    pub relative_path: String,
    pub artifact_digest: [u8; 32],
    pub ownership_marker: String,
}

/// Durable local receipt. It is a host-install ownership record, not a
/// product/configuration store and contains no artifact content or credentials.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBundleInstallReceiptV1 {
    pub schema_version: u16,
    pub operation_id: [u8; 16],
    pub host: HostKindV1,
    pub component: HostBundleComponentV1,
    pub operation: HostBundleLifecycleOpV1,
    pub manifest_digest: [u8; 32],
    pub artifacts: Vec<HostBundleReceiptArtifactV1>,
}

/// Injected lifecycle storage boundary. The concrete no-follow writer below
/// implements this trait, while daemon wiring can provide its opened authority
/// without exposing a filesystem path or mutation capability to callers.
pub trait HostBundleLifecycleStorageV1 {
    fn recover_lifecycle(&mut self) -> Result<(), HostBundleError>;

    fn execute_lifecycle<V: HostBundleVerificationAdapterV1>(
        &mut self,
        manifest: &HostBundleManifestV1,
        request: &HostBundleExecutionRequestV1,
        contents: &[HostBundleArtifactContentV1],
        verifier: &V,
    ) -> Result<HostBundleInstallReceiptV1, HostBundleError>;
}

/// Production-composition seam for independently injected cryptographic and
/// filesystem authorities. It verifies before it asks storage to recover or
/// mutate, so a bad signature cannot trigger a lifecycle filesystem attempt.
pub struct HostBundleLifecycleRuntimeV1<V, S> {
    verifier: V,
    storage: S,
}

impl<V, S> HostBundleLifecycleRuntimeV1<V, S> {
    pub fn new(verifier: V, storage: S) -> Self {
        Self { verifier, storage }
    }

    pub fn into_storage(self) -> S {
        self.storage
    }
}

impl<V, S> HostBundleLifecycleRuntimeV1<V, S>
where
    V: HostBundleVerificationAdapterV1,
    S: HostBundleLifecycleStorageV1,
{
    pub fn recover(&mut self) -> Result<(), HostBundleError> {
        self.storage.recover_lifecycle()
    }

    pub fn execute(
        &mut self,
        manifest: &HostBundleManifestV1,
        request: &HostBundleExecutionRequestV1,
        contents: &[HostBundleArtifactContentV1],
    ) -> Result<HostBundleInstallReceiptV1, HostBundleError> {
        self.verifier.verify_manifest(manifest)?;
        self.storage.recover_lifecycle()?;
        self.storage
            .execute_lifecycle(manifest, request, contents, &self.verifier)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum HostBundleJournalStateV1 {
    Prepared,
    Committed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostBundleJournalEntryV1 {
    relative_path: String,
    backup_name: Option<String>,
    backup_created: bool,
    wrote_new: bool,
    installed_digest: Option<[u8; 32]>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostBundleJournalV1 {
    schema_version: u16,
    operation_id: [u8; 16],
    host: HostKindV1,
    component: HostBundleComponentV1,
    operation: HostBundleLifecycleOpV1,
    manifest_digest: [u8; 32],
    state: HostBundleJournalStateV1,
    previous_receipt: Option<HostBundleInstallReceiptV1>,
    entries: Vec<HostBundleJournalEntryV1>,
}

/// Atomic, capability-rooted host-bundle writer. Every descendant directory
/// is opened without following symlinks; files are staged, fsynced, renamed,
/// and followed by a directory sync before receipt publication.
pub struct HostBundleWriterV1 {
    root_path: PathBuf,
    root: Dir,
    control: Dir,
    _writer_lock: fs::File,
}

impl HostBundleWriterV1 {
    pub fn open(root_path: impl Into<PathBuf>) -> Result<Self, HostBundleError> {
        let root_path = root_path.into();
        ensure_bundle_root(&root_path)?;
        let root = Dir::open_ambient_dir(&root_path, ambient_authority())
            .map_err(|_| HostBundleError::UnsafeInstallPath)?;
        let control = open_or_create_nofollow_dir(&root, HOST_BUNDLE_CONTROL_DIR)?;
        let writer_lock = open_writer_lock(&control)?;
        let mut writer = Self {
            root_path,
            root,
            control,
            _writer_lock: writer_lock,
        };
        writer.recover_interrupted_operation()?;
        Ok(writer)
    }

    /// Recover by rolling an incomplete transaction back from its immutable
    /// backups. A receipt matching the journal operation is a durable commit
    /// marker and is never rolled back after a crash between receipt/journal
    /// cleanup.
    pub fn recover_interrupted_operation(&mut self) -> Result<(), HostBundleError> {
        let Some(journal) = self.load_journal()? else {
            return Ok(());
        };
        validate_journal(&journal)?;
        if self
            .load_receipt(journal.host, journal.component)?
            .as_ref()
            .is_some_and(|receipt| {
                receipt.operation_id == journal.operation_id
                    && receipt.operation == journal.operation
                    && receipt.manifest_digest == journal.manifest_digest
            })
        {
            self.remove_control_file(HOST_BUNDLE_JOURNAL_FILE)?;
            return Ok(());
        }

        let backup_dir = self.open_existing_backup_dir(journal.operation_id)?;
        for entry in journal.entries.iter().rev() {
            let (parent, name) = self.open_parent_nofollow(Path::new(&entry.relative_path))?;
            if let Some(backup_name) = &entry.backup_name {
                let backup_exists = match &backup_dir {
                    Some(backups) => regular_file_exists(backups, backup_name)?,
                    None => false,
                };
                if !entry.backup_created {
                    if !backup_exists {
                        continue;
                    }
                    if regular_file_exists(&parent, &name)? {
                        return Err(HostBundleError::RecoveryRequired);
                    }
                }
                let backups = backup_dir
                    .as_ref()
                    .filter(|_| backup_exists)
                    .ok_or(HostBundleError::RecoveryRequired)?;
                if entry.wrote_new {
                    remove_if_digest_matches(
                        &parent,
                        &name,
                        entry
                            .installed_digest
                            .ok_or(HostBundleError::ReceiptCorrupted)?,
                    )?;
                } else if regular_file_exists(&parent, &name)? {
                    return Err(HostBundleError::RecoveryRequired);
                }
                backups
                    .rename(backup_name, &parent, &name)
                    .map_err(|_| HostBundleError::StorageFailure)?;
                sync_cap_dir(backups)?;
                sync_cap_dir(&parent)?;
            } else if entry.wrote_new {
                remove_if_digest_matches(
                    &parent,
                    &name,
                    entry
                        .installed_digest
                        .ok_or(HostBundleError::ReceiptCorrupted)?,
                )?;
                sync_cap_dir(&parent)?;
            }
        }
        match journal.previous_receipt {
            Some(receipt) => self.write_receipt(&receipt)?,
            None => self.remove_receipt(journal.host, journal.component)?,
        }
        self.remove_control_file(HOST_BUNDLE_JOURNAL_FILE)
    }

    /// Verify JCS/Ed25519 first, validate artifact bytes, plan ownership-aware
    /// mutations, then execute them atomically with a recoverable journal.
    pub fn execute(
        &mut self,
        manifest: &HostBundleManifestV1,
        request: &HostBundleExecutionRequestV1,
        contents: &[HostBundleArtifactContentV1],
        verifier: &impl HostBundleVerificationAdapterV1,
    ) -> Result<HostBundleInstallReceiptV1, HostBundleError> {
        if request.operation_id == [0; 16] {
            return Err(HostBundleError::InvalidManifest);
        }
        verifier.verify_manifest(manifest)?;
        let content_by_path = validate_artifact_contents(manifest, request, contents)?;
        self.recover_interrupted_operation()?;
        let previous_receipt = self.load_receipt(manifest.host, manifest.component)?;
        let manifest_digest = manifest.canonical_signed_digest()?;
        if let Some(receipt) = previous_receipt.as_ref()
            && receipt.operation_id == request.operation_id
        {
            return if receipt.operation == request.lifecycle.operation
                && receipt.manifest_digest == manifest_digest
            {
                Ok(receipt.clone())
            } else {
                Err(HostBundleError::ReceiptCorrupted)
            };
        }
        let owned_receipt = previous_receipt
            .as_ref()
            .filter(|receipt| receipt.operation != HostBundleLifecycleOpV1::Uninstall);
        let manifest_observed = if request.lifecycle.operation == HostBundleLifecycleOpV1::Uninstall
        {
            Vec::new()
        } else {
            self.observe_artifacts(manifest, owned_receipt)?
        };
        let orphan_observed = if matches!(
            request.lifecycle.operation,
            HostBundleLifecycleOpV1::Update
                | HostBundleLifecycleOpV1::Repair
                | HostBundleLifecycleOpV1::Uninstall
        ) {
            owned_receipt
                .into_iter()
                .flat_map(|receipt| &receipt.artifacts)
                .filter(|owned| {
                    request.lifecycle.operation == HostBundleLifecycleOpV1::Uninstall
                        || !manifest
                            .artifacts
                            .iter()
                            .any(|artifact| artifact.relative_path == owned.relative_path)
                })
                .map(|owned| self.observe_owned_artifact(owned))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        let plan = plan_verified_complete_lifecycle_mutation(
            manifest,
            &request.lifecycle,
            &manifest_observed,
            owned_receipt,
            &orphan_observed,
            verifier,
        )?;
        let mut journal = HostBundleJournalV1 {
            schema_version: HOST_BUNDLE_RECEIPT_SCHEMA_VERSION,
            operation_id: request.operation_id,
            host: manifest.host,
            component: manifest.component,
            operation: request.lifecycle.operation,
            manifest_digest,
            state: HostBundleJournalStateV1::Prepared,
            previous_receipt: previous_receipt.clone(),
            entries: plan
                .mutations
                .iter()
                .map(|mutation| HostBundleJournalEntryV1 {
                    relative_path: mutation.relative_path.clone(),
                    backup_name: matches!(
                        mutation.action,
                        HostArtifactActionV1::BackupThenReplace
                            | HostArtifactActionV1::BackupThenRemove
                    )
                    .then(|| backup_name(request.operation_id, &mutation.relative_path)),
                    backup_created: false,
                    wrote_new: false,
                    installed_digest: manifest
                        .artifacts
                        .iter()
                        .find(|artifact| artifact.relative_path == mutation.relative_path)
                        .map(|artifact| artifact.artifact_digest)
                        .filter(|_| {
                            !matches!(mutation.action, HostArtifactActionV1::BackupThenRemove)
                        }),
                })
                .collect(),
        };
        self.write_journal(&journal)?;
        let backup_dir = self.open_or_create_backup_dir(request.operation_id)?;

        for (index, mutation) in plan.mutations.iter().enumerate() {
            let (parent, name) = self.open_parent_nofollow(Path::new(&mutation.relative_path))?;
            match mutation.action {
                HostArtifactActionV1::Noop => {}
                HostArtifactActionV1::WriteNew => {
                    journal.entries[index].wrote_new = true;
                    self.write_journal(&journal)?;
                    atomic_write_nofollow(
                        &parent,
                        &name,
                        content_by_path
                            .get(&mutation.relative_path)
                            .ok_or(HostBundleError::ArtifactContentMismatch)?,
                        false,
                    )?;
                }
                HostArtifactActionV1::BackupThenReplace => {
                    let backup_name = journal.entries[index]
                        .backup_name
                        .as_deref()
                        .ok_or(HostBundleError::ReceiptCorrupted)?;
                    move_regular_to_backup(&parent, &name, &backup_dir, backup_name)?;
                    journal.entries[index].backup_created = true;
                    self.write_journal(&journal)?;
                    journal.entries[index].wrote_new = true;
                    self.write_journal(&journal)?;
                    atomic_write_nofollow(
                        &parent,
                        &name,
                        content_by_path
                            .get(&mutation.relative_path)
                            .ok_or(HostBundleError::ArtifactContentMismatch)?,
                        false,
                    )?;
                }
                HostArtifactActionV1::BackupThenRemove => {
                    let backup_name = journal.entries[index]
                        .backup_name
                        .as_deref()
                        .ok_or(HostBundleError::ReceiptCorrupted)?;
                    move_regular_to_backup(&parent, &name, &backup_dir, backup_name)?;
                    journal.entries[index].backup_created = true;
                    self.write_journal(&journal)?;
                }
            }
        }

        let receipt = HostBundleInstallReceiptV1 {
            schema_version: HOST_BUNDLE_RECEIPT_SCHEMA_VERSION,
            operation_id: request.operation_id,
            host: manifest.host,
            component: manifest.component,
            operation: request.lifecycle.operation,
            manifest_digest,
            artifacts: if request.lifecycle.operation == HostBundleLifecycleOpV1::Uninstall {
                Vec::new()
            } else {
                manifest
                    .artifacts
                    .iter()
                    .map(|artifact| HostBundleReceiptArtifactV1 {
                        relative_path: artifact.relative_path.clone(),
                        artifact_digest: artifact.artifact_digest,
                        ownership_marker: artifact.ownership_marker.clone(),
                    })
                    .collect()
            },
        };
        self.write_receipt(&receipt)?;
        journal.state = HostBundleJournalStateV1::Committed;
        self.write_journal(&journal)?;
        self.remove_control_file(HOST_BUNDLE_JOURNAL_FILE)?;
        Ok(receipt)
    }

    fn observe_artifacts(
        &self,
        manifest: &HostBundleManifestV1,
        receipt: Option<&HostBundleInstallReceiptV1>,
    ) -> Result<Vec<ObservedHostArtifactV1>, HostBundleError> {
        let mut observed = Vec::with_capacity(manifest.artifacts.len());
        for artifact in &manifest.artifacts {
            let (parent, name) = self.open_parent_nofollow(Path::new(&artifact.relative_path))?;
            let receipt_artifact = receipt.and_then(|receipt| {
                (receipt.host == manifest.host && receipt.component == manifest.component)
                    .then_some(receipt)
                    .and_then(|receipt| {
                        receipt
                            .artifacts
                            .iter()
                            .find(|record| record.relative_path == artifact.relative_path)
                    })
            });
            let (kind, digest) = match read_regular_nofollow(&parent, &name)? {
                None => (ObservedArtifactKindV1::Missing, None),
                Some(bytes) => {
                    let digest: [u8; 32] = Sha256::digest(&bytes).into();
                    (ObservedArtifactKindV1::RegularFile, Some(digest))
                }
            };
            observed.push(ObservedHostArtifactV1 {
                relative_path: artifact.relative_path.clone(),
                kind,
                artifact_digest: digest,
                ownership_marker: receipt_artifact.map(|record| record.ownership_marker.clone()),
                owned_artifact_digest: receipt_artifact.map(|record| record.artifact_digest),
            });
        }
        Ok(observed)
    }

    fn observe_owned_artifact(
        &self,
        owned: &HostBundleReceiptArtifactV1,
    ) -> Result<ObservedHostArtifactV1, HostBundleError> {
        let (parent, name) = self.open_parent_nofollow(Path::new(&owned.relative_path))?;
        let (kind, artifact_digest) = match read_regular_nofollow(&parent, &name)? {
            Some(bytes) => (
                ObservedArtifactKindV1::RegularFile,
                Some(Sha256::digest(&bytes).into()),
            ),
            None => (ObservedArtifactKindV1::Missing, None),
        };
        Ok(ObservedHostArtifactV1 {
            relative_path: owned.relative_path.clone(),
            kind,
            artifact_digest,
            ownership_marker: Some(owned.ownership_marker.clone()),
            owned_artifact_digest: Some(owned.artifact_digest),
        })
    }

    fn open_parent_nofollow(&self, relative: &Path) -> Result<(Dir, String), HostBundleError> {
        validate_relative_install_path(relative)?;
        let mut parent = self
            .root
            .open_dir_nofollow(".")
            .map_err(|_| HostBundleError::UnsafeInstallPath)?;
        let components = relative.components().collect::<Vec<_>>();
        let Some(Component::Normal(last)) = components.last() else {
            return Err(HostBundleError::UnsafeInstallPath);
        };
        for component in &components[..components.len().saturating_sub(1)] {
            let Component::Normal(component) = component else {
                return Err(HostBundleError::UnsafeInstallPath);
            };
            let name = component
                .to_str()
                .ok_or(HostBundleError::UnsafeInstallPath)?;
            parent = open_or_create_nofollow_dir(&parent, name)?;
        }
        Ok((
            parent,
            last.to_str()
                .ok_or(HostBundleError::UnsafeInstallPath)?
                .to_owned(),
        ))
    }

    fn open_or_create_backup_dir(&self, operation_id: [u8; 16]) -> Result<Dir, HostBundleError> {
        let backups = open_or_create_nofollow_dir(&self.control, "backups")?;
        open_or_create_nofollow_dir(&backups, &hex::encode(operation_id))
    }

    fn open_existing_backup_dir(
        &self,
        operation_id: [u8; 16],
    ) -> Result<Option<Dir>, HostBundleError> {
        let backups = match self.control.open_dir_nofollow("backups") {
            Ok(backups) => backups,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(HostBundleError::UnsafeInstallPath),
        };
        match backups.open_dir_nofollow(hex::encode(operation_id)) {
            Ok(directory) => Ok(Some(directory)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(HostBundleError::UnsafeInstallPath),
        }
    }

    fn load_receipt(
        &self,
        host: HostKindV1,
        component: HostBundleComponentV1,
    ) -> Result<Option<HostBundleInstallReceiptV1>, HostBundleError> {
        let receipt = read_control_json(&self.control, &receipt_file(host, component))?;
        let receipt = receipt
            .map(|bytes| {
                serde_json::from_slice(&bytes).map_err(|_| HostBundleError::ReceiptCorrupted)
            })
            .transpose()?;
        if let Some(receipt) = &receipt {
            validate_receipt(receipt)?;
            if receipt.host != host || receipt.component != component {
                return Err(HostBundleError::ReceiptCorrupted);
            }
        }
        Ok(receipt)
    }

    fn write_receipt(&self, receipt: &HostBundleInstallReceiptV1) -> Result<(), HostBundleError> {
        validate_receipt(receipt)?;
        let bytes = serde_json::to_vec(receipt).map_err(|_| HostBundleError::ReceiptCorrupted)?;
        atomic_write_nofollow(
            &self.control,
            &receipt_file(receipt.host, receipt.component),
            &bytes,
            true,
        )
    }

    fn remove_receipt(
        &self,
        host: HostKindV1,
        component: HostBundleComponentV1,
    ) -> Result<(), HostBundleError> {
        self.remove_control_file(&receipt_file(host, component))
    }

    fn load_journal(&self) -> Result<Option<HostBundleJournalV1>, HostBundleError> {
        read_control_json(&self.control, HOST_BUNDLE_JOURNAL_FILE)?
            .map(|bytes| {
                serde_json::from_slice(&bytes).map_err(|_| HostBundleError::ReceiptCorrupted)
            })
            .transpose()
    }

    fn write_journal(&self, journal: &HostBundleJournalV1) -> Result<(), HostBundleError> {
        validate_journal(journal)?;
        let bytes = serde_json::to_vec(journal).map_err(|_| HostBundleError::ReceiptCorrupted)?;
        atomic_write_nofollow(&self.control, HOST_BUNDLE_JOURNAL_FILE, &bytes, true)
    }

    fn remove_control_file(&self, name: &str) -> Result<(), HostBundleError> {
        remove_regular_if_exists(&self.control, name)?;
        sync_cap_dir(&self.control)
    }

    pub fn root_path(&self) -> &Path {
        &self.root_path
    }
}

impl HostBundleLifecycleStorageV1 for HostBundleWriterV1 {
    fn recover_lifecycle(&mut self) -> Result<(), HostBundleError> {
        self.recover_interrupted_operation()
    }

    fn execute_lifecycle<V: HostBundleVerificationAdapterV1>(
        &mut self,
        manifest: &HostBundleManifestV1,
        request: &HostBundleExecutionRequestV1,
        contents: &[HostBundleArtifactContentV1],
        verifier: &V,
    ) -> Result<HostBundleInstallReceiptV1, HostBundleError> {
        self.execute(manifest, request, contents, verifier)
    }
}

fn validate_artifact_contents(
    manifest: &HostBundleManifestV1,
    request: &HostBundleExecutionRequestV1,
    contents: &[HostBundleArtifactContentV1],
) -> Result<BTreeMap<String, Vec<u8>>, HostBundleError> {
    if request.lifecycle.operation == HostBundleLifecycleOpV1::Uninstall {
        return if contents.is_empty() {
            Ok(BTreeMap::new())
        } else {
            Err(HostBundleError::ArtifactContentMismatch)
        };
    }
    if contents.len() != manifest.artifacts.len() {
        return Err(HostBundleError::ArtifactContentMismatch);
    }
    let mut values = BTreeMap::new();
    for content in contents {
        if content.bytes.len() > MAX_ARTIFACT_CONTENT_BYTES
            || values.contains_key(&content.relative_path)
        {
            return Err(HostBundleError::ArtifactContentMismatch);
        }
        let artifact = manifest
            .artifacts
            .iter()
            .find(|artifact| artifact.relative_path == content.relative_path)
            .ok_or(HostBundleError::ArtifactContentMismatch)?;
        let digest: [u8; 32] = Sha256::digest(&content.bytes).into();
        if digest != artifact.artifact_digest {
            return Err(HostBundleError::ArtifactContentMismatch);
        }
        values.insert(content.relative_path.clone(), content.bytes.clone());
    }
    Ok(values)
}

fn ensure_bundle_root(root: &Path) -> Result<(), HostBundleError> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(HostBundleError::UnsafeInstallPath);
        }
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(HostBundleError::StorageFailure),
    }
    fs::create_dir_all(root).map_err(|_| HostBundleError::StorageFailure)?;
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        _ => Err(HostBundleError::UnsafeInstallPath),
    }
}

fn open_or_create_nofollow_dir(parent: &Dir, name: &str) -> Result<Dir, HostBundleError> {
    if !is_safe_component(name) {
        return Err(HostBundleError::UnsafeInstallPath);
    }
    match parent.open_dir_nofollow(name) {
        Ok(directory) => Ok(directory),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            parent
                .create_dir(name)
                .map_err(|_| HostBundleError::StorageFailure)?;
            parent
                .open_dir_nofollow(name)
                .map_err(|_| HostBundleError::UnsafeInstallPath)
        }
        Err(_) => Err(HostBundleError::UnsafeInstallPath),
    }
}

fn open_writer_lock(control: &Dir) -> Result<fs::File, HostBundleError> {
    let mut options = CapOpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .follow(FollowSymlinks::No);
    let file = control
        .open_with(HOST_BUNDLE_LOCK_FILE, &options)
        .map_err(|_| HostBundleError::UnsafeInstallPath)?
        .into_std();
    file.try_lock_exclusive()
        .map_err(|_| HostBundleError::RecoveryRequired)?;
    Ok(file)
}

fn is_safe_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains('/')
        && !value.contains('\\')
}

fn read_regular_nofollow(parent: &Dir, name: &str) -> Result<Option<Vec<u8>>, HostBundleError> {
    match parent.symlink_metadata(name) {
        Ok(metadata) if metadata.file_type().is_file() => {
            if metadata.len() > MAX_ARTIFACT_CONTENT_BYTES as u64 {
                return Err(HostBundleError::ArtifactContentMismatch);
            }
        }
        Ok(_) => return Err(HostBundleError::UnsafeInstallPath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(HostBundleError::StorageFailure),
    }
    let mut options = CapOpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = parent
        .open_with(name, &options)
        .map_err(|_| HostBundleError::UnsafeInstallPath)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_ARTIFACT_CONTENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| HostBundleError::StorageFailure)?;
    if bytes.len() > MAX_ARTIFACT_CONTENT_BYTES {
        return Err(HostBundleError::ArtifactContentMismatch);
    }
    Ok(Some(bytes))
}

fn regular_file_exists(parent: &Dir, name: &str) -> Result<bool, HostBundleError> {
    match parent.symlink_metadata(name) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => Err(HostBundleError::UnsafeInstallPath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(HostBundleError::StorageFailure),
    }
}

fn remove_regular_if_exists(parent: &Dir, name: &str) -> Result<(), HostBundleError> {
    if regular_file_exists(parent, name)? {
        parent
            .remove_file(name)
            .map_err(|_| HostBundleError::StorageFailure)?;
    }
    Ok(())
}

fn remove_if_digest_matches(
    parent: &Dir,
    name: &str,
    expected_digest: [u8; 32],
) -> Result<(), HostBundleError> {
    let Some(bytes) = read_regular_nofollow(parent, name)? else {
        return Ok(());
    };
    let actual: [u8; 32] = Sha256::digest(&bytes).into();
    if actual != expected_digest {
        return Err(HostBundleError::RecoveryRequired);
    }
    parent
        .remove_file(name)
        .map_err(|_| HostBundleError::StorageFailure)
}

fn move_regular_to_backup(
    parent: &Dir,
    name: &str,
    backup_dir: &Dir,
    backup_name: &str,
) -> Result<(), HostBundleError> {
    if !regular_file_exists(parent, name)? || !is_safe_component(backup_name) {
        return Err(HostBundleError::UnsafeInstallPath);
    }
    if regular_file_exists(backup_dir, backup_name)? {
        return Err(HostBundleError::RecoveryRequired);
    }
    parent
        .rename(name, backup_dir, backup_name)
        .map_err(|_| HostBundleError::StorageFailure)?;
    sync_cap_dir(parent)?;
    sync_cap_dir(backup_dir)
}

fn atomic_write_nofollow(
    parent: &Dir,
    name: &str,
    bytes: &[u8],
    replace_existing: bool,
) -> Result<(), HostBundleError> {
    if !is_safe_component(name) || bytes.len() > MAX_ARTIFACT_CONTENT_BYTES {
        return Err(HostBundleError::ArtifactContentMismatch);
    }
    match parent.symlink_metadata(name) {
        Ok(metadata) if metadata.file_type().is_file() && replace_existing => {}
        Ok(metadata) if metadata.file_type().is_file() => {
            return Err(HostBundleError::OwnershipConflict);
        }
        Ok(_) => return Err(HostBundleError::UnsafeInstallPath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(HostBundleError::StorageFailure),
    }
    for _ in 0..32 {
        let temporary = format!(
            ".{name}.{}.{}.tmp",
            std::process::id(),
            HOST_BUNDLE_TEMP_NONCE.fetch_add(1, Ordering::Relaxed)
        );
        let mut options = CapOpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        let mut file = match parent.open_with(&temporary, &options) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(HostBundleError::StorageFailure),
        };
        let result = (|| {
            file.write_all(bytes)
                .map_err(|_| HostBundleError::StorageFailure)?;
            file.sync_all()
                .map_err(|_| HostBundleError::StorageFailure)?;
            drop(file);
            // A rename changes the final directory entry rather than following
            // a final symlink; the preflight and capability parent prevent
            // traversal through any descendant component.
            if replace_existing {
                parent
                    .rename(&temporary, parent, name)
                    .map_err(|_| HostBundleError::StorageFailure)?;
            } else {
                parent
                    .hard_link(&temporary, parent, name)
                    .map_err(|error| {
                        if error.kind() == io::ErrorKind::AlreadyExists {
                            HostBundleError::OwnershipConflict
                        } else {
                            HostBundleError::StorageFailure
                        }
                    })?;
                parent
                    .remove_file(&temporary)
                    .map_err(|_| HostBundleError::StorageFailure)?;
            }
            sync_cap_dir(parent)
        })();
        if result.is_err() {
            let _ = parent.remove_file(&temporary);
        }
        return result;
    }
    Err(HostBundleError::StorageFailure)
}

fn sync_cap_dir(dir: &Dir) -> Result<(), HostBundleError> {
    let mut options = CapOpenOptions::new();
    options.read(true).maybe_dir(true);
    dir.open_with(".", &options)
        .and_then(|file| file.sync_all())
        .map_err(|_| HostBundleError::StorageFailure)
}

fn read_control_json(parent: &Dir, name: &str) -> Result<Option<Vec<u8>>, HostBundleError> {
    let Some(bytes) = read_regular_nofollow(parent, name)? else {
        return Ok(None);
    };
    if bytes.is_empty() || bytes.len() > MAX_CONTROL_FILE_BYTES {
        return Err(HostBundleError::ReceiptCorrupted);
    }
    Ok(Some(bytes))
}

fn validate_receipt(receipt: &HostBundleInstallReceiptV1) -> Result<(), HostBundleError> {
    if receipt.schema_version != HOST_BUNDLE_RECEIPT_SCHEMA_VERSION
        || receipt.operation_id == [0; 16]
        || receipt.manifest_digest == [0; 32]
        || receipt.artifacts.len() > MAX_MANIFEST_ARTIFACTS
        || (receipt.operation == HostBundleLifecycleOpV1::Uninstall) != receipt.artifacts.is_empty()
    {
        return Err(HostBundleError::ReceiptCorrupted);
    }
    for (index, artifact) in receipt.artifacts.iter().enumerate() {
        validate_relative_install_path(Path::new(&artifact.relative_path))?;
        validate_identifier(&artifact.ownership_marker)?;
        if artifact.artifact_digest == [0; 32]
            || receipt.artifacts[..index]
                .iter()
                .any(|existing| existing.relative_path == artifact.relative_path)
        {
            return Err(HostBundleError::ReceiptCorrupted);
        }
    }
    Ok(())
}

fn validate_journal(journal: &HostBundleJournalV1) -> Result<(), HostBundleError> {
    if journal.schema_version != HOST_BUNDLE_RECEIPT_SCHEMA_VERSION
        || journal.operation_id == [0; 16]
        || journal.manifest_digest == [0; 32]
        || (journal.entries.is_empty() && journal.operation != HostBundleLifecycleOpV1::Uninstall)
        || journal.entries.len() > MAX_MANIFEST_ARTIFACTS
    {
        return Err(HostBundleError::ReceiptCorrupted);
    }
    if let Some(receipt) = &journal.previous_receipt {
        validate_receipt(receipt)?;
        if receipt.host != journal.host || receipt.component != journal.component {
            return Err(HostBundleError::ReceiptCorrupted);
        }
    }
    for (index, entry) in journal.entries.iter().enumerate() {
        validate_relative_install_path(Path::new(&entry.relative_path))?;
        if entry
            .backup_name
            .as_deref()
            .is_some_and(|backup| !is_safe_component(backup))
            || journal.entries[..index]
                .iter()
                .any(|existing| existing.relative_path == entry.relative_path)
            || (entry.backup_created && entry.backup_name.is_none())
            || (entry.backup_name.is_some() && entry.wrote_new && !entry.backup_created)
            || (entry.wrote_new && entry.installed_digest.is_none())
        {
            return Err(HostBundleError::ReceiptCorrupted);
        }
    }
    Ok(())
}

fn backup_name(operation_id: [u8; 16], relative_path: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(operation_id);
    hasher.update(relative_path.as_bytes());
    format!("artifact-{}", hex::encode(hasher.finalize()))
}

fn receipt_file(host: HostKindV1, component: HostBundleComponentV1) -> String {
    format!(
        "receipt.{}.{}.v1.json",
        match host {
            HostKindV1::ClaudeCode => "claude-code",
            HostKindV1::CursorDesktop => "cursor-desktop",
            HostKindV1::CursorCloud => "cursor-cloud",
            HostKindV1::Codex => "codex",
            HostKindV1::Hermes => "hermes",
            HostKindV1::Kiro => "kiro",
        },
        match component {
            HostBundleComponentV1::Core => "core",
            HostBundleComponentV1::ContextMcp => "context-mcp",
            HostBundleComponentV1::OperatorMcp => "operator-mcp",
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn manifest(host: HostKindV1) -> HostBundleManifestV1 {
        HostBundleManifestV1 {
            schema_version: HOST_BUNDLE_SCHEMA_VERSION,
            host,
            component: HostBundleComponentV1::Core,
            integration_manifest_digest: [1; 32],
            catalog_digest: [2; 32],
            configuration_snapshot_id: "config.v1".to_owned(),
            effective_behavior_digest: [3; 32],
            resolution_provenance_digest: [4; 32],
            protocol_min: 1,
            protocol_max: 2,
            signer_key_id: "release-key.v1".to_owned(),
            signature: vec![5; 64],
            artifacts: vec![HostBundleArtifactV1 {
                relative_path: "plugins/tracedecay.json".to_owned(),
                artifact_digest: [6; 32],
                ownership_marker: "tracedecay.install.v1".to_owned(),
            }],
        }
    }

    fn request(
        host: HostKindV1,
        operation: HostBundleLifecycleOpV1,
    ) -> HostBundleLifecycleRequestV1 {
        HostBundleLifecycleRequestV1 {
            operation,
            expected_host: host,
            expected_component: HostBundleComponentV1::Core,
            explicit_confirmation: true,
            hermes_profile_bindings: u8::from(host == HostKindV1::Hermes),
        }
    }

    #[derive(Clone, Copy)]
    struct TestTrustResolver([u8; 32]);

    impl HostBundleTrustResolverV1 for TestTrustResolver {
        fn resolve_ed25519_public_key(
            &self,
            signer_key_id: &str,
        ) -> Result<[u8; 32], HostBundleError> {
            (signer_key_id == "release-key.v1")
                .then_some(self.0)
                .ok_or(HostBundleError::VerificationFailed)
        }
    }

    struct RejectingVerifier;

    impl HostBundleVerificationAdapterV1 for RejectingVerifier {
        fn verify_manifest(&self, _manifest: &HostBundleManifestV1) -> Result<(), HostBundleError> {
            Err(HostBundleError::VerificationFailed)
        }
    }

    #[derive(Default)]
    struct RecordingLifecycleStorage {
        recovery_calls: usize,
        execute_calls: usize,
    }

    impl HostBundleLifecycleStorageV1 for RecordingLifecycleStorage {
        fn recover_lifecycle(&mut self) -> Result<(), HostBundleError> {
            self.recovery_calls = self.recovery_calls.saturating_add(1);
            Ok(())
        }

        fn execute_lifecycle<V: HostBundleVerificationAdapterV1>(
            &mut self,
            _manifest: &HostBundleManifestV1,
            _request: &HostBundleExecutionRequestV1,
            _contents: &[HostBundleArtifactContentV1],
            _verifier: &V,
        ) -> Result<HostBundleInstallReceiptV1, HostBundleError> {
            self.execute_calls = self.execute_calls.saturating_add(1);
            Err(HostBundleError::StorageFailure)
        }
    }

    fn signed_manifest(
        host: HostKindV1,
        bytes: &[u8],
    ) -> (HostBundleManifestV1, TestTrustResolver) {
        let key = SigningKey::from_bytes(&[42; 32]);
        let digest: [u8; 32] = Sha256::digest(bytes).into();
        let mut manifest = manifest(host);
        manifest.artifacts[0].artifact_digest = digest;
        manifest.signature = key
            .sign(&manifest.canonical_signed_bytes().unwrap())
            .to_bytes()
            .to_vec();
        (manifest, TestTrustResolver(key.verifying_key().to_bytes()))
    }

    fn execution(
        host: HostKindV1,
        operation: HostBundleLifecycleOpV1,
        byte: u8,
    ) -> HostBundleExecutionRequestV1 {
        HostBundleExecutionRequestV1 {
            lifecycle: request(host, operation),
            operation_id: [byte; 16],
        }
    }

    fn content(manifest: &HostBundleManifestV1, bytes: &[u8]) -> Vec<HostBundleArtifactContentV1> {
        vec![HostBundleArtifactContentV1 {
            relative_path: manifest.artifacts[0].relative_path.clone(),
            bytes: bytes.to_vec(),
        }]
    }

    #[test]
    fn claude_and_cursor_project_only_native_diagnostic_routes() {
        assert!(require_capability(HostKindV1::ClaudeCode, HostCapabilityV1::Lsp).is_ok());
        assert_eq!(
            require_capability(HostKindV1::ClaudeCode, HostCapabilityV1::NativeDiagnostics),
            Err(HostBundleError::UnsupportedCapability)
        );
        assert!(
            require_capability(
                HostKindV1::CursorDesktop,
                HostCapabilityV1::NativeDiagnostics
            )
            .is_ok()
        );
        assert_eq!(
            require_capability(HostKindV1::CursorCloud, HostCapabilityV1::Lsp),
            Err(HostBundleError::UnsupportedCapability)
        );
    }

    #[test]
    fn unsafe_manifest_paths_are_rejected() {
        let mut bundle = manifest(HostKindV1::Codex);
        bundle.artifacts[0].relative_path = "../credentials".to_owned();
        assert_eq!(
            bundle.validate_structure(),
            Err(HostBundleError::UnsafeInstallPath)
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_install_component_is_rejected() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.path().join("plugins")).unwrap();
        assert_eq!(
            inspect_install_target(root.path(), Path::new("plugins/tracedecay.json")),
            Err(HostBundleError::UnsafeInstallPath)
        );
    }

    #[test]
    fn signature_failure_prevents_any_mutation_plan() {
        let bundle = manifest(HostKindV1::Codex);
        let error = plan_lifecycle_mutation(
            &bundle,
            &request(HostKindV1::Codex, HostBundleLifecycleOpV1::Install),
            &[],
            |_| Err(HostBundleError::VerificationFailed),
        )
        .unwrap_err();
        assert_eq!(error, HostBundleError::VerificationFailed);
    }

    #[test]
    fn runtime_rejects_signature_before_injected_storage_recovery_or_mutation() {
        let bundle = manifest(HostKindV1::Codex);
        let mut runtime = HostBundleLifecycleRuntimeV1::new(
            RejectingVerifier,
            RecordingLifecycleStorage::default(),
        );
        assert_eq!(
            runtime.execute(
                &bundle,
                &execution(HostKindV1::Codex, HostBundleLifecycleOpV1::Install, 1),
                &[],
            ),
            Err(HostBundleError::VerificationFailed)
        );
        let storage = runtime.into_storage();
        assert_eq!(storage.recovery_calls, 0);
        assert_eq!(storage.execute_calls, 0);
    }

    #[test]
    fn owned_replacement_is_backed_up_and_conflicts_are_refused() {
        let bundle = manifest(HostKindV1::Codex);
        let mut state = ObservedHostArtifactV1 {
            relative_path: bundle.artifacts[0].relative_path.clone(),
            kind: ObservedArtifactKindV1::RegularFile,
            artifact_digest: Some([9; 32]),
            ownership_marker: Some(bundle.artifacts[0].ownership_marker.clone()),
            owned_artifact_digest: Some([9; 32]),
        };
        let plan = plan_lifecycle_mutation(
            &bundle,
            &request(HostKindV1::Codex, HostBundleLifecycleOpV1::Update),
            &[state.clone()],
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(
            plan.mutations[0].action,
            HostArtifactActionV1::BackupThenReplace
        );
        assert!(plan.rollback_required);

        state.ownership_marker = Some("somebody-else".to_owned());
        assert_eq!(
            plan_lifecycle_mutation(
                &bundle,
                &request(HostKindV1::Codex, HostBundleLifecycleOpV1::Update),
                &[state],
                |_| Ok(()),
            ),
            Err(HostBundleError::OwnershipConflict)
        );
    }

    #[test]
    fn uninstall_removes_only_owned_artifacts() {
        let bundle = manifest(HostKindV1::ClaudeCode);
        let state = ObservedHostArtifactV1 {
            relative_path: bundle.artifacts[0].relative_path.clone(),
            kind: ObservedArtifactKindV1::RegularFile,
            artifact_digest: Some(bundle.artifacts[0].artifact_digest),
            ownership_marker: Some(bundle.artifacts[0].ownership_marker.clone()),
            owned_artifact_digest: Some(bundle.artifacts[0].artifact_digest),
        };
        let plan = plan_lifecycle_mutation(
            &bundle,
            &request(HostKindV1::ClaudeCode, HostBundleLifecycleOpV1::Uninstall),
            &[state],
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(
            plan.mutations[0].action,
            HostArtifactActionV1::BackupThenRemove
        );
    }

    #[test]
    fn complete_uninstall_plan_derives_removals_from_receipt_orphans() {
        let bundle = manifest(HostKindV1::ClaudeCode);
        let receipt = HostBundleInstallReceiptV1 {
            schema_version: HOST_BUNDLE_RECEIPT_SCHEMA_VERSION,
            operation_id: [1; 16],
            host: bundle.host,
            component: bundle.component,
            operation: HostBundleLifecycleOpV1::Install,
            manifest_digest: [2; 32],
            artifacts: vec![HostBundleReceiptArtifactV1 {
                relative_path: bundle.artifacts[0].relative_path.clone(),
                artifact_digest: bundle.artifacts[0].artifact_digest,
                ownership_marker: bundle.artifacts[0].ownership_marker.clone(),
            }],
        };
        let orphan_observed = vec![ObservedHostArtifactV1 {
            relative_path: bundle.artifacts[0].relative_path.clone(),
            kind: ObservedArtifactKindV1::RegularFile,
            artifact_digest: Some(bundle.artifacts[0].artifact_digest),
            ownership_marker: Some(bundle.artifacts[0].ownership_marker.clone()),
            owned_artifact_digest: Some(bundle.artifacts[0].artifact_digest),
        }];
        let plan = plan_complete_lifecycle_mutation(
            &bundle,
            &request(HostKindV1::ClaudeCode, HostBundleLifecycleOpV1::Uninstall),
            &[],
            Some(&receipt),
            &orphan_observed,
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(plan.mutations.len(), 1);
        assert_eq!(
            plan.mutations[0].action,
            HostArtifactActionV1::BackupThenRemove
        );
        assert!(plan.rollback_required);
    }

    #[test]
    fn hermes_requires_exactly_one_profile_binding() {
        let bundle = manifest(HostKindV1::Hermes);
        let mut lifecycle = request(HostKindV1::Hermes, HostBundleLifecycleOpV1::Install);
        lifecycle.hermes_profile_bindings = 2;
        assert_eq!(
            plan_lifecycle_mutation(&bundle, &lifecycle, &[], |_| Ok(())),
            Err(HostBundleError::InvalidHermesProfileBinding)
        );
    }

    #[test]
    fn jcs_ed25519_verifier_rejects_a_tampered_signed_manifest() {
        let (manifest, resolver) = signed_manifest(HostKindV1::Codex, b"one");
        let verifier = JcsEd25519HostBundleVerifierV1::new(resolver);
        verifier.verify_manifest(&manifest).unwrap();

        let mut tampered = manifest.clone();
        tampered.protocol_max += 1;
        assert_eq!(
            verifier.verify_manifest(&tampered),
            Err(HostBundleError::VerificationFailed)
        );
    }

    #[test]
    fn atomic_writer_install_update_repair_and_uninstall_preserve_ownership() {
        for host in [
            HostKindV1::ClaudeCode,
            HostKindV1::Codex,
            HostKindV1::CursorDesktop,
            HostKindV1::Hermes,
            HostKindV1::Kiro,
        ] {
            let root = tempfile::tempdir().unwrap();
            let (first, resolver) = signed_manifest(host, b"first");
            let verifier = JcsEd25519HostBundleVerifierV1::new(resolver);
            let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
            assert!(matches!(
                HostBundleWriterV1::open(root.path()),
                Err(HostBundleError::RecoveryRequired)
            ));
            let install_request = execution(host, HostBundleLifecycleOpV1::Install, 1);
            let receipt = writer
                .execute(
                    &first,
                    &install_request,
                    &content(&first, b"first"),
                    &verifier,
                )
                .unwrap();
            assert_eq!(
                writer
                    .execute(
                        &first,
                        &install_request,
                        &content(&first, b"first"),
                        &verifier,
                    )
                    .unwrap(),
                receipt
            );
            assert_eq!(receipt.host, host);
            let installed = root.path().join("plugins/tracedecay.json");
            assert_eq!(std::fs::read(&installed).unwrap(), b"first");

            let (second, resolver) = signed_manifest(host, b"second");
            let verifier = JcsEd25519HostBundleVerifierV1::new(resolver);
            writer
                .execute(
                    &second,
                    &execution(host, HostBundleLifecycleOpV1::Update, 2),
                    &content(&second, b"second"),
                    &verifier,
                )
                .unwrap();
            assert_eq!(std::fs::read(&installed).unwrap(), b"second");
            assert!(
                root.path()
                    .join(HOST_BUNDLE_CONTROL_DIR)
                    .join("backups")
                    .exists()
            );

            std::fs::write(&installed, b"locally modified").unwrap();
            writer
                .execute(
                    &second,
                    &execution(host, HostBundleLifecycleOpV1::Repair, 3),
                    &content(&second, b"second"),
                    &verifier,
                )
                .unwrap();
            assert_eq!(std::fs::read(&installed).unwrap(), b"second");

            writer
                .execute(
                    &second,
                    &execution(host, HostBundleLifecycleOpV1::Uninstall, 4),
                    &[],
                    &verifier,
                )
                .unwrap();
            assert!(!installed.exists());
            let uninstall_receipt = writer
                .load_receipt(host, HostBundleComponentV1::Core)
                .unwrap()
                .unwrap();
            assert_eq!(
                uninstall_receipt.operation,
                HostBundleLifecycleOpV1::Uninstall
            );
            assert!(uninstall_receipt.artifacts.is_empty());
        }
    }

    #[test]
    fn uninstall_refuses_a_user_modified_owned_artifact() {
        let root = tempfile::tempdir().unwrap();
        let (manifest, resolver) = signed_manifest(HostKindV1::ClaudeCode, b"installed");
        let verifier = JcsEd25519HostBundleVerifierV1::new(resolver);
        let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
        writer
            .execute(
                &manifest,
                &execution(HostKindV1::ClaudeCode, HostBundleLifecycleOpV1::Install, 20),
                &content(&manifest, b"installed"),
                &verifier,
            )
            .unwrap();
        let installed = root.path().join(&manifest.artifacts[0].relative_path);
        std::fs::write(&installed, b"user-owned modification").unwrap();
        assert_eq!(
            writer.execute(
                &manifest,
                &execution(
                    HostKindV1::ClaudeCode,
                    HostBundleLifecycleOpV1::Uninstall,
                    21,
                ),
                &[],
                &verifier,
            ),
            Err(HostBundleError::OwnershipConflict)
        );
        assert_eq!(
            std::fs::read(installed).unwrap(),
            b"user-owned modification"
        );
    }

    #[test]
    fn writer_refuses_foreign_or_symlinked_targets() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("plugins")).unwrap();
        std::fs::write(root.path().join("plugins/tracedecay.json"), b"foreign").unwrap();
        let (manifest, resolver) = signed_manifest(HostKindV1::ClaudeCode, b"ours");
        let verifier = JcsEd25519HostBundleVerifierV1::new(resolver);
        let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
        assert_eq!(
            writer.execute(
                &manifest,
                &execution(HostKindV1::ClaudeCode, HostBundleLifecycleOpV1::Install, 5),
                &content(&manifest, b"ours"),
                &verifier,
            ),
            Err(HostBundleError::OwnershipConflict)
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let symlink_root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            symlink(outside.path(), symlink_root.path().join("plugins")).unwrap();
            let mut writer = HostBundleWriterV1::open(symlink_root.path()).unwrap();
            assert_eq!(
                writer.execute(
                    &manifest,
                    &execution(HostKindV1::ClaudeCode, HostBundleLifecycleOpV1::Install, 6),
                    &content(&manifest, b"ours"),
                    &verifier,
                ),
                Err(HostBundleError::UnsafeInstallPath)
            );
        }
    }

    #[test]
    fn interrupted_update_rolls_back_from_durable_backups() {
        let root = tempfile::tempdir().unwrap();
        let (first, resolver) = signed_manifest(HostKindV1::Hermes, b"first");
        let verifier = JcsEd25519HostBundleVerifierV1::new(resolver);
        let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
        writer
            .execute(
                &first,
                &execution(HostKindV1::Hermes, HostBundleLifecycleOpV1::Install, 7),
                &content(&first, b"first"),
                &verifier,
            )
            .unwrap();
        let old_receipt = writer
            .load_receipt(HostKindV1::Hermes, HostBundleComponentV1::Core)
            .unwrap();
        let expected_receipt = old_receipt.clone().unwrap();
        let operation_id = [8; 16];
        let relative = first.artifacts[0].relative_path.clone();
        let mut journal = HostBundleJournalV1 {
            schema_version: HOST_BUNDLE_RECEIPT_SCHEMA_VERSION,
            operation_id,
            host: HostKindV1::Hermes,
            component: HostBundleComponentV1::Core,
            operation: HostBundleLifecycleOpV1::Update,
            manifest_digest: first.canonical_signed_digest().unwrap(),
            state: HostBundleJournalStateV1::Prepared,
            previous_receipt: old_receipt.clone(),
            entries: vec![
                HostBundleJournalEntryV1 {
                    relative_path: relative.clone(),
                    backup_name: Some(backup_name(operation_id, &relative)),
                    backup_created: false,
                    wrote_new: false,
                    installed_digest: Some(Sha256::digest(b"second").into()),
                },
                HostBundleJournalEntryV1 {
                    relative_path: "agents/hermes/not-started.json".to_owned(),
                    backup_name: Some(backup_name(operation_id, "agents/hermes/not-started.json")),
                    backup_created: false,
                    wrote_new: false,
                    installed_digest: Some(Sha256::digest(b"not-started").into()),
                },
            ],
        };
        writer.write_journal(&journal).unwrap();
        let (parent, name) = writer.open_parent_nofollow(Path::new(&relative)).unwrap();
        let backups = writer.open_or_create_backup_dir(operation_id).unwrap();
        move_regular_to_backup(
            &parent,
            &name,
            &backups,
            journal.entries[0].backup_name.as_deref().unwrap(),
        )
        .unwrap();
        journal.entries[0].backup_created = true;
        journal.entries[0].wrote_new = true;
        writer.write_journal(&journal).unwrap();
        atomic_write_nofollow(&parent, &name, b"second", false).unwrap();
        let mut misleading_receipt = expected_receipt.clone();
        misleading_receipt.operation_id = operation_id;
        misleading_receipt.operation = HostBundleLifecycleOpV1::Update;
        misleading_receipt.manifest_digest = [9; 32];
        writer.write_receipt(&misleading_receipt).unwrap();
        journal.state = HostBundleJournalStateV1::Committed;
        writer.write_journal(&journal).unwrap();
        drop(writer);

        let recovered = HostBundleWriterV1::open(root.path()).unwrap();
        assert_eq!(std::fs::read(root.path().join(relative)).unwrap(), b"first");
        assert!(recovered.load_journal().unwrap().is_none());
        assert_eq!(
            recovered
                .load_receipt(HostKindV1::Hermes, HostBundleComponentV1::Core)
                .unwrap()
                .unwrap(),
            expected_receipt
        );
    }

    #[test]
    fn five_host_capability_matrix_is_explicit_and_hermes_is_single_profile() {
        for host in [
            HostKindV1::ClaudeCode,
            HostKindV1::CursorDesktop,
            HostKindV1::Codex,
            HostKindV1::Hermes,
            HostKindV1::Kiro,
        ] {
            assert_eq!(stock_host_capabilities(host).len(), 5);
        }
        let bundle = manifest(HostKindV1::Hermes);
        let mut lifecycle = request(HostKindV1::Hermes, HostBundleLifecycleOpV1::Install);
        lifecycle.hermes_profile_bindings = 1;
        assert!(plan_lifecycle_mutation(&bundle, &lifecycle, &[], |_| Ok(())).is_ok());
    }
}
