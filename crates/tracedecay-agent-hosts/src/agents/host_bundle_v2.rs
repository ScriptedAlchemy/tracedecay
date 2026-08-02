//! Manifest-driven host bundle lifecycle contracts (Plan 27 PR13).
//!
//! This module plans host-registration mutations only after the embedded
//! first-party catalog verifies manifest identity and content digests. It
//! contains no signing key, trust root, external bundle loader, credential,
//! daemon lifecycle, product semantics, or host-specific business authority.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsMaybeDirExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions as CapOpenOptions};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_domain::canonical_json_bytes;
use tracedecay_host_integration::host_bundle_storage_failure;
pub use tracedecay_host_integration::{
    ClineFamilyAdmissionV1, ClineFamilyEvidenceV1, ClineFamilyProviderV1,
    EmbeddedHostIntegrationEvidenceV1, EmbeddedNativeHostFixtureV1,
    HOST_BUNDLE_RECEIPT_SCHEMA_VERSION, HOST_BUNDLE_SCHEMA_VERSION, HostBundleArtifactContentV1,
    HostBundleArtifactV1, HostBundleBackupArtifactV1, HostBundleBackupReceiptV1,
    HostBundleComponentV1, HostBundleError, HostBundleInstallReceiptV1, HostBundleJournalEntryV1,
    HostBundleJournalStateV1, HostBundleJournalV1, HostBundleLifecycleOpV1, HostBundleManifestV1,
    HostBundleReceiptArtifactV1, HostBundleRestoreReceiptV1, HostBundleRollbackBoundaryV1,
    HostBundleVerificationAdapterV1, HostCapabilityRecordV1, HostCapabilityStateV1,
    HostCapabilityUnavailableReasonV1, HostCapabilityV1, HostComponentSetJournalComponentV1,
    HostComponentSetJournalStateV1, HostComponentSetJournalV1, HostComponentSetReceiptV1,
    HostKindV1, HostNativeFixtureEvidenceV1, HostRegistrationEvidenceV1, HostRegistrationRouteV1,
    MAX_ARTIFACT_CONTENT_BYTES, MAX_HOST_COMPONENTS, MAX_MANIFEST_ARTIFACTS,
    MAX_RELATIVE_PATH_BYTES, stock_host_capabilities, validate_identifier,
    validate_relative_install_path,
};
use tracedecay_host_integration::{
    cline_family_evidence_from_embedded_assets,
    native_host_edit_stop_conformance_evidence_from_embedded_assets,
    stock_host_native_fixture_evidence_from_embedded_assets,
    stock_host_registration_evidence as stock_host_registration_evidence_from_contract,
};

const HOST_BUNDLE_CONTROL_DIR: &str = ".tracedecay-host-bundle-v1";
const HOST_BUNDLE_JOURNAL_FILE: &str = "journal.v1.json";
/// Legacy shared component-set journal name. One journal per lifecycle root
/// meant an interrupted transaction for any host blocked every other host.
/// Journals are host-scoped now; this name is still read (and retired) so a
/// journal left by an older binary is recovered rather than orphaned.
const HOST_COMPONENT_SET_JOURNAL_FILE: &str = "component-set-journal.v1.json";
const HOST_COMPONENT_SET_STAGE_DIR: &str = "component-set-staging";
/// Set-aside directory for journals an operator explicitly abandoned with
/// `tracedecay host-bundle recover --quarantine --yes`. Backups stay in place.
const HOST_BUNDLE_QUARANTINE_DIR: &str = "quarantine";
const HOST_BUNDLE_LOCK_FILE: &str = "writer.v1.lock";
const MAX_CONTROL_FILE_BYTES: usize = 256 * 1024;
static HOST_BUNDLE_TEMP_NONCE: AtomicU64 = AtomicU64::new(1);

/// Resolve the lifecycle authority from the active `TraceDecay` user profile.
/// Host homes contain deployed artifacts only; receipts, journals, locks, and
/// rollback backups are owned by this profile-scoped root.
pub fn resolved_host_bundle_lifecycle_root() -> crate::errors::Result<PathBuf> {
    Ok(crate::storage::default_profile_root()?.join("host-components"))
}

/// Canonical stock-host enumeration shared by packaging, delivery, and
/// conformance consumers.
pub const fn stock_host_kinds() -> [HostKindV1; 12] {
    HostKindV1::ALL
}

/// Evidence references are stable repository or host-contract identifiers.
/// Their capability semantics live in the root-free host-integration crate.
pub fn stock_host_registration_evidence(host: HostKindV1) -> Vec<HostRegistrationEvidenceV1> {
    stock_host_registration_evidence_from_contract(host)
}

const CLINE_FAMILY_EVIDENCE_PACKET_PATH: &str =
    "crates/tracedecay-hooks/fixtures/host_events/cline-family.json";
const CLINE_FAMILY_EVIDENCE_PACKET: &[u8] =
    include_bytes!("../../../../tests/fixtures/packaged_host_events/cline-family.json");
const CLINE_FAMILY_TRANSCRIPT_MANIFEST_PATH: &str =
    "tests/fixtures/transcript_golden/cline_like/manifest.json";
const CLINE_FAMILY_TRANSCRIPT_MANIFEST: &[u8] =
    include_bytes!("../../../../tests/fixtures/transcript_golden/cline_like/manifest.json");
static EMBEDDED_NATIVE_HOST_FIXTURES: [EmbeddedNativeHostFixtureV1; 7] = [
    EmbeddedNativeHostFixtureV1 {
        host: HostKindV1::ClaudeCode,
        bytes: include_bytes!("../../../../tests/fixtures/packaged_host_events/claude.json"),
    },
    EmbeddedNativeHostFixtureV1 {
        host: HostKindV1::Codex,
        bytes: include_bytes!("../../../../tests/fixtures/packaged_host_events/codex.json"),
    },
    EmbeddedNativeHostFixtureV1 {
        host: HostKindV1::CursorDesktop,
        bytes: include_bytes!("../../../../tests/fixtures/packaged_host_events/cursor.json"),
    },
    EmbeddedNativeHostFixtureV1 {
        host: HostKindV1::Hermes,
        bytes: include_bytes!("../../../../tests/fixtures/packaged_host_events/hermes.json"),
    },
    EmbeddedNativeHostFixtureV1 {
        host: HostKindV1::Kiro,
        bytes: include_bytes!("../../../../tests/fixtures/packaged_host_events/kiro.json"),
    },
    EmbeddedNativeHostFixtureV1 {
        host: HostKindV1::KimiCode,
        bytes: include_bytes!("../../../../tests/fixtures/packaged_host_events/kimi-code.json"),
    },
    EmbeddedNativeHostFixtureV1 {
        host: HostKindV1::OpenCode,
        bytes: include_bytes!(
            "../../../../tests/fixtures/packaged_host_events/opencode/baseline.json"
        ),
    },
];

fn embedded_host_integration_evidence() -> EmbeddedHostIntegrationEvidenceV1 {
    EmbeddedHostIntegrationEvidenceV1 {
        cline_family_evidence_packet_path: CLINE_FAMILY_EVIDENCE_PACKET_PATH,
        cline_family_evidence_packet: CLINE_FAMILY_EVIDENCE_PACKET,
        cline_family_transcript_manifest_path: CLINE_FAMILY_TRANSCRIPT_MANIFEST_PATH,
        cline_family_transcript_manifest: CLINE_FAMILY_TRANSCRIPT_MANIFEST,
        native_fixtures: &EMBEDDED_NATIVE_HOST_FIXTURES,
    }
}

pub fn cline_family_evidence(provider: ClineFamilyProviderV1) -> Option<ClineFamilyEvidenceV1> {
    cline_family_evidence_from_embedded_assets(&embedded_host_integration_evidence(), provider)
}

pub fn stock_host_native_fixture_evidence(host: HostKindV1) -> Option<HostNativeFixtureEvidenceV1> {
    stock_host_native_fixture_evidence_from_embedded_assets(
        &embedded_host_integration_evidence(),
        host,
    )
}

pub fn native_host_edit_stop_conformance_evidence() -> Vec<HostNativeFixtureEvidenceV1> {
    native_host_edit_stop_conformance_evidence_from_embedded_assets(
        &embedded_host_integration_evidence(),
    )
}

/// Verify embedded first-party catalog identity and content digests, then
/// produce the lifecycle plan. This keeps the older closure-based planner
/// compatible while giving production callers one concrete verification
/// contract.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedArtifactKindV1 {
    Missing,
    RegularFile,
    Directory,
    Symlink,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ObservedHostArtifactV1 {
    pub relative_path: String,
    pub kind: ObservedArtifactKindV1,
    pub artifact_digest: Option<[u8; 32]>,
    pub ownership_marker: Option<String>,
    /// Digest last recorded by the component's durable ownership receipt.
    /// This is distinct from the bytes currently observed on disk.
    pub owned_artifact_digest: Option<[u8; 32]>,
    /// Ownership marker the first-party catalog assigns to this exact deploy
    /// path, independent of any receipt. Only observations taken directly from
    /// the planned component's cataloged artifact list may set it; receipt- and
    /// orphan-derived observations must leave it `None`. Pre-v2 installers
    /// wrote these cataloged paths without ever writing a v2 receipt, so
    /// `Repair` alone may adopt such an artifact.
    pub cataloged_ownership_marker: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostArtifactActionV1 {
    Noop,
    WriteNew,
    BackupThenReplace,
    BackupThenRemove,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HostBundleMutationPlanV1 {
    pub operation: HostBundleLifecycleOpV1,
    pub host: HostKindV1,
    pub component: HostBundleComponentV1,
    pub mutations: Vec<HostArtifactMutationV1>,
    pub rollback_required: bool,
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

/// Refuse a component whose host capabilities or binding fixture evidence do
/// not justify the native surfaces that component would install.
pub fn require_component_capabilities(
    host: HostKindV1,
    component: HostBundleComponentV1,
) -> Result<(), HostBundleError> {
    use HostBundleComponentV1::{Agent, ContextMcp, Core, OperatorMcp};
    use HostCapabilityV1::{Cli, Hooks, Lsp, Mcp, NativeDiagnostics};

    let required: &[HostCapabilityV1] = match (host, component) {
        (HostKindV1::ClaudeCode, Core) => &[Lsp, Hooks],
        (HostKindV1::CursorDesktop | HostKindV1::Codex, Core) => &[Hooks],
        (
            HostKindV1::Hermes | HostKindV1::Kiro | HostKindV1::KimiCode | HostKindV1::OpenCode,
            Core,
        ) => &[Hooks, Mcp],
        (
            HostKindV1::CursorCloud
            | HostKindV1::ClineFamily
            | HostKindV1::Cline
            | HostKindV1::RooCode
            | HostKindV1::Kilo,
            Core,
        ) => &[Mcp],
        (_, ContextMcp | OperatorMcp) => &[Mcp],
        (HostKindV1::CursorDesktop, Agent) => &[NativeDiagnostics],
        (HostKindV1::OpenCode, Agent) => &[Cli],
        (_, Agent) => return Err(HostBundleError::UnsupportedCapability),
    };
    for capability in required {
        require_capability(host, *capability)?;
    }
    if required
        .iter()
        .any(|capability| matches!(capability, Lsp | NativeDiagnostics | Hooks))
        && stock_host_native_fixture_evidence(host).is_none()
    {
        return Err(HostBundleError::UnsupportedCapability);
    }

    let cline_provider = match host {
        HostKindV1::Cline => Some(ClineFamilyProviderV1::Cline),
        HostKindV1::RooCode => Some(ClineFamilyProviderV1::RooCode),
        HostKindV1::Kilo => Some(ClineFamilyProviderV1::Kilo),
        HostKindV1::ClineFamily => return Err(HostBundleError::UnsupportedCapability),
        _ => None,
    };
    if let Some(provider) = cline_provider {
        let evidence =
            cline_family_evidence(provider).ok_or(HostBundleError::UnsupportedCapability)?;
        if evidence.registration.state != HostCapabilityStateV1::Supported
            || evidence.edit != HostCapabilityStateV1::Supported
            || evidence.stop != HostCapabilityStateV1::Supported
        {
            return Err(HostBundleError::UnsupportedCapability);
        }
    }
    Ok(())
}

/// Validate the compiled catalog entry before producing a mutation-only plan.
pub fn plan_lifecycle_mutation(
    manifest: &HostBundleManifestV1,
    request: &HostBundleLifecycleRequestV1,
    observed: &[ObservedHostArtifactV1],
    verify: impl FnOnce(&HostBundleManifestV1) -> Result<(), HostBundleError>,
) -> Result<HostBundleMutationPlanV1, HostBundleError> {
    manifest.validate_structure()?;
    verify(manifest).map_err(|_| HostBundleError::CatalogMismatch)?;
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
        // A verified embedded uninstall target authorizes lifecycle execution,
        // but only the durable ownership receipt identifies removable files.
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
        // Receiptless artifacts left behind by the pre-v2 installer are the one
        // exception, and only under `Repair`. Adoption still requires the exact
        // expected ownership marker; a foreign or absent marker conflicts.
        if !adopts_pre_receipt_artifact(operation, artifact, state) {
            return Err(HostBundleError::OwnershipConflict);
        }
        return Ok(if state.artifact_digest == Some(artifact.artifact_digest) {
            HostArtifactActionV1::Noop
        } else {
            HostArtifactActionV1::BackupThenReplace
        });
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

/// Decide whether a receiptless observation may be adopted into v2 ownership.
///
/// Pre-v2 installers deployed first-party artifacts without writing a v2
/// ownership receipt, so their files are indistinguishable from foreign files
/// by receipt evidence alone and every lifecycle operation refuses them. The
/// adoption boundary is deliberately narrow:
///
/// * only `Repair`, whose contract is already "restore the cataloged
///   deployment", may adopt. `Install` must never claim a path it did not
///   create, `Update` must know the previously owned digest before it can tell
///   a stale deployment from a user edit, and `Uninstall` must never delete a
///   file whose ownership it cannot prove;
/// * the observation must carry the planned component's own cataloged
///   ownership marker for this exact deploy path, and it must equal the
///   expected artifact's marker. Observations derived from receipts or from
///   orphan paths never set it, so they can never reach this branch;
/// * no receipt may claim the path. A receipt-backed artifact keeps the
///   unmodified marker equality check, which stays the security boundary.
///
/// Adoption itself never destroys anything: a byte-identical file becomes a
/// `Noop`, and anything else is backed up before it is replaced.
fn adopts_pre_receipt_artifact(
    operation: HostBundleLifecycleOpV1,
    artifact: &HostBundleArtifactV1,
    state: &ObservedHostArtifactV1,
) -> bool {
    operation == HostBundleLifecycleOpV1::Repair
        && state.ownership_marker.is_none()
        && state.owned_artifact_digest.is_none()
        && state.cataloged_ownership_marker.as_deref() == Some(artifact.ownership_marker.as_str())
}

/// Execution-specific input kept separate from the public lifecycle request
/// so existing plan consumers do not accidentally gain filesystem authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostBundleExecutionRequestV1 {
    pub lifecycle: HostBundleLifecycleRequestV1,
    pub operation_id: [u8; 16],
}

/// One verified component in the canonical set for a host lifecycle operation.
/// The content remains outside receipts and journals; it is staged and checked
/// against the embedded manifest before any host path is changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostComponentSetEntryV1 {
    pub manifest: HostBundleManifestV1,
    pub contents: Vec<HostBundleArtifactContentV1>,
}

/// The complete, host-specific component set that must be committed or rolled
/// back as one lifecycle boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostComponentSetV1 {
    pub host: HostKindV1,
    pub components: Vec<HostComponentSetEntryV1>,
}

/// Set-level lifecycle authority. Component selection is explicit so a
/// default install and an explicit `--component` use the same transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostComponentSetLifecycleRequestV1 {
    pub operation: HostBundleLifecycleOpV1,
    pub expected_host: HostKindV1,
    pub expected_components: Vec<HostBundleComponentV1>,
    pub explicit_confirmation: bool,
    pub hermes_profile_bindings: u8,
}

/// One operation id spans every component, registration mutation, receipt,
/// backup, and recovery record in a component-set transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostComponentSetExecutionRequestV1 {
    pub lifecycle: HostComponentSetLifecycleRequestV1,
    pub operation_id: [u8; 16],
}

/// Immutable component-set dry run consumed by confirmed apply. The plan
/// digest binds the operation id, complete component inventory, artifact
/// actions, and exact registration revision observed before preview returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostComponentSetLifecyclePreviewV1 {
    pub operation_id: [u8; 16],
    pub plan_digest: [u8; 32],
    pub base_registration_revision: [u8; 32],
    pub current_registration_revision: [u8; 32],
    pub artifact_state_revision: [u8; 32],
    pub component_plans: Vec<HostBundleMutationPlanV1>,
    /// Third-party extensions the registration adapter found already claiming
    /// a surface this component set would register, ordered by extension id.
    pub competing_extension_claims: Vec<CompetingHostExtensionClaimV1>,
    pub confirmation_required: bool,
}

/// A third-party host extension claiming a surface `TraceDecay` would register.
/// The digest points to bounded discovery evidence; raw host config is never
/// retained in lifecycle requests or receipts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CompetingHostExtensionClaimV1 {
    pub extension_id: String,
    pub capability: HostCapabilityV1,
    pub evidence_digest: [u8; 32],
}

/// Explicit host-level rollback handoff returned by a dry run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostBundleRollbackSeamV1 {
    pub operation_id: [u8; 16],
    pub host: HostKindV1,
    pub component: HostBundleComponentV1,
    pub backup_relative_paths: Vec<String>,
    pub interrupted_recovery_required: bool,
}

/// Read-only lifecycle result. Producing this value verifies the embedded
/// first-party manifest and exact ownership observations but never opens a
/// writer, creates a control directory, writes a receipt, or recovers a journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostBundleLifecyclePreviewV1 {
    pub plan: HostBundleMutationPlanV1,
    pub confirmation_required: bool,
    pub competing_extension_claims: Vec<CompetingHostExtensionClaimV1>,
    pub rollback: HostBundleRollbackSeamV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostBundleRegistrationStateV1 {
    Current,
    Repairable,
    Missing,
    Corrupt,
}

pub trait HostBundleRegistrationInspectorV1 {
    fn inspect_registration(
        &self,
        host: HostKindV1,
        component: HostBundleComponentV1,
    ) -> HostBundleRegistrationStateV1;

    /// Operator guidance for a host that exposes component activation only
    /// through an interactive UI, or `None` when the host has a supported
    /// non-interactive activation surface.
    ///
    /// This is the single capability signal behind
    /// [`HostBundleComponentDoctorStateV1::ActivationDeferred`]: a host that
    /// returns `None` here keeps the blocking `Missing` classification for
    /// absent receipt-owned artifacts, because for such a host an unattended
    /// reinstall really can converge the state.
    fn interactive_activation_guidance(&self, _host: HostKindV1) -> Option<String> {
        None
    }
}

/// Read-only classification of one installed component (or one of its
/// artifacts). This type is `Serialize`-only and is never persisted into a
/// receipt, journal, or any other durable control file — it exists solely for
/// the transient [`HostBundleDoctorReportV1`]. Adding a variant therefore
/// widens the doctor's reported vocabulary without making any previously
/// written artifact unreadable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostBundleComponentDoctorStateV1 {
    Current,
    Repairable,
    /// A receipt-owned artifact whose ownership marker is still this
    /// component's own but whose bytes moved away from the recorded digest.
    /// This is ordinary content drift, not a contested path: `Repair` plans it
    /// as `BackupThenReplace` (see `plan_artifact_action`), so reinstall
    /// converges without an operator first resolving a foreign claim.
    Drifted,
    OwnershipConflict,
    /// A `TraceDecay`-named host registration that no install receipt owns —
    /// an uninstall that removed the receipt-owned artifacts but left the host
    /// still advertising the extension. Reported so the leftover registration
    /// is visible; repairing it is an explicit operator command.
    OrphanedRegistration,
    /// Every receipt-owned artifact of a component whose host activates only
    /// through an interactive UI is absent, and the host's staged source bundle
    /// is present but unactivated. Nothing TraceDecay can drive non-interactively
    /// deploys these bytes — the host materialises them when the operator
    /// activates the extension — so this is a pending user action rather than
    /// receipt drift. Ranked below `Missing`: a component that still holds SOME
    /// of its receipt-owned bytes lost the rest after activation, which is real
    /// drift and stays blocking.
    ActivationDeferred,
    Missing,
    Corrupt,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HostBundleArtifactDoctorResultV1 {
    pub relative_path: String,
    pub expected_digest: [u8; 32],
    pub observed_digest: Option<[u8; 32]>,
    pub ownership_marker: String,
    pub state: HostBundleComponentDoctorStateV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HostBundleComponentDoctorResultV1 {
    pub receipt_path: PathBuf,
    pub host: Option<HostKindV1>,
    pub component: Option<HostBundleComponentV1>,
    pub state: HostBundleComponentDoctorStateV1,
    pub registration: Option<HostBundleRegistrationStateV1>,
    pub artifacts: Vec<HostBundleArtifactDoctorResultV1>,
    pub repair_action: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HostBundleDoctorReportV1 {
    pub components: Vec<HostBundleComponentDoctorResultV1>,
    /// Checked-in native edit/stop conformance behind every packaged host.
    /// It is reported even when nothing is installed, so an empty component
    /// list never reads as "there was no host evidence to check".
    pub native_edit_stop_conformance: Vec<HostNativeFixtureEvidenceV1>,
}

impl Default for HostBundleDoctorReportV1 {
    fn default() -> Self {
        Self {
            components: Vec::new(),
            native_edit_stop_conformance: native_host_edit_stop_conformance_evidence(),
        }
    }
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

/// Host-native registration boundary coordinated with an artifact component
/// set. Implementations persist their own bounded registration backups during
/// `stage`; the aggregate writer records the state transition in its recovery
/// journal and invokes these hooks in reverse on failure or restart.
pub trait HostComponentSetRegistrationV1 {
    /// Exact revision of the host registration state that this adapter may
    /// mutate. Concrete host adapters hash their bounded native config;
    /// artifact-only adapters use this stable no-registration revision.
    fn current_revision(
        &self,
        _component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
    ) -> Result<[u8; 32], HostBundleError> {
        Ok(Sha256::digest(b"tracedecay.host-registration.none.v1").into())
    }

    /// Bounded read-only discovery of third-party extensions that already
    /// claim a surface this component set would register. Discovery reports;
    /// it never grants authority to disable, replace, or adopt the competing
    /// extension. Adapters that cannot observe a host's registration surface
    /// must refuse rather than report an empty slice.
    fn discover_competing_extension_claims(
        &self,
        _component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
    ) -> Result<Vec<CompetingHostExtensionClaimV1>, HostBundleError> {
        Ok(Vec::new())
    }

    /// Bind the adapter to the confirmed preview immediately before staging.
    /// Implementations may retain the revision and recheck it while capturing
    /// their rollback backup.
    fn confirm_preview(
        &mut self,
        component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
        preview: &HostComponentSetLifecyclePreviewV1,
    ) -> Result<(), HostBundleError> {
        if preview.operation_id != request.operation_id
            || preview.current_registration_revision != preview.base_registration_revision
            || self.current_revision(component_set, request)? != preview.base_registration_revision
        {
            return Err(HostBundleError::StalePreview);
        }
        Ok(())
    }

    /// Absolute host paths this transaction will write itself, declared before
    /// any mutation runs.
    ///
    /// A host may register itself through a file that is also one of this
    /// component set's managed artifacts, in which case the adapter's own
    /// registration revision changes as a direct consequence of the
    /// transaction's declared write. Adapters use this set to tell their own
    /// writes apart from a foreign edit; every path outside it stays under
    /// full drift protection.
    fn declare_artifact_writes(
        &mut self,
        _component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
        _paths: &[PathBuf],
    ) -> Result<(), HostBundleError> {
        Ok(())
    }

    fn preflight(
        &mut self,
        _component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
    ) -> Result<(), HostBundleError> {
        Ok(())
    }

    fn stage(
        &mut self,
        _component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
    ) -> Result<(), HostBundleError> {
        Ok(())
    }

    fn apply(
        &mut self,
        _component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
    ) -> Result<(), HostBundleError> {
        Ok(())
    }

    fn verify(
        &mut self,
        _component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
    ) -> Result<(), HostBundleError> {
        Ok(())
    }

    fn commit(
        &mut self,
        _component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
    ) -> Result<(), HostBundleError> {
        Ok(())
    }

    fn rollback(
        &mut self,
        _component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
    ) -> Result<(), HostBundleError> {
        Ok(())
    }
}

/// Public component-set lifecycle façade over the capability-rooted writer.
/// It keeps the existing per-component receipt API intact while ensuring the
/// default host lifecycle has one aggregate recovery boundary.
pub struct HostComponentSetTransactionV1<'a> {
    writer: &'a mut HostBundleWriterV1,
}

impl<'a> HostComponentSetTransactionV1<'a> {
    pub fn new(writer: &'a mut HostBundleWriterV1) -> Self {
        Self { writer }
    }

    /// Recover whichever single component-set journal is outstanding. Callers
    /// that know the host should prefer [`Self::recover_host`], which never
    /// hands another host's journal to this registration adapter.
    pub fn recover<R: HostComponentSetRegistrationV1>(
        &mut self,
        registration: &mut R,
    ) -> Result<(), HostBundleError> {
        self.writer
            .recover_component_set_operation(None, registration)?;
        self.writer.recover_interrupted_operation()
    }

    /// Recover only `host`'s pending component-set journal. Other hosts'
    /// journals are left untouched: their artifact path spaces are disjoint,
    /// and their registration state belongs to a different adapter.
    pub fn recover_host<R: HostComponentSetRegistrationV1>(
        &mut self,
        host: HostKindV1,
        registration: &mut R,
    ) -> Result<(), HostBundleError> {
        self.writer
            .recover_component_set_operation(Some(host), registration)?;
        self.writer.recover_interrupted_operation()
    }

    pub fn preview<V: HostBundleVerificationAdapterV1, R: HostComponentSetRegistrationV1>(
        &mut self,
        component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
        verifier: &V,
        registration: &mut R,
    ) -> Result<HostComponentSetLifecyclePreviewV1, HostBundleError> {
        // Only this host's own pending journal blocks the preview. A wedged
        // transaction for an unrelated host mutates a disjoint path space and
        // is not a reason to refuse work here.
        if self.writer.load_journal()?.is_some()
            || self
                .writer
                .load_component_set_journal_for(component_set.host)?
                .is_some()
        {
            return Err(HostBundleError::RecoveryRequired);
        }
        dry_run_host_component_set_lifecycle_with_lifecycle_root_at(
            &self.writer.root_path,
            &self.writer.lifecycle_root_path,
            component_set,
            request,
            verifier,
            registration,
        )
    }

    pub fn execute_confirmed<
        V: HostBundleVerificationAdapterV1,
        R: HostComponentSetRegistrationV1,
    >(
        &mut self,
        component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
        preview: &HostComponentSetLifecyclePreviewV1,
        verifier: &V,
        registration: &mut R,
    ) -> Result<HostComponentSetReceiptV1, HostBundleError> {
        if !request.lifecycle.explicit_confirmation {
            return Err(HostBundleError::ConfirmationRequired);
        }
        validate_component_set_request(component_set, request)?;
        if preview.operation_id != request.operation_id {
            return Err(HostBundleError::StalePreview);
        }
        if let Some(receipt) = self
            .writer
            .load_component_set_receipt(request.operation_id)?
        {
            if !component_set_receipt_matches(&receipt, component_set, request)? {
                return Err(HostBundleError::ReceiptCorrupted);
            }
            return component_set_receipt_matches_preview(&receipt, preview)
                .then_some(receipt)
                .ok_or(HostBundleError::StalePreview);
        }
        // The re-preview reports why this plan can no longer be applied.
        // `StalePreview` is reserved for genuine drift between the confirmed
        // preview and what is observed now (checked below); a typed refusal
        // such as an ownership conflict or a catalog mismatch is a standing
        // condition that retrying cannot clear, and laundering it into
        // "stale, retry" hides the only diagnostic the operator has.
        let current = self.preview(component_set, request, verifier, registration)?;
        if current.operation_id != preview.operation_id
            || current.plan_digest != preview.plan_digest
            || current.base_registration_revision != preview.base_registration_revision
            || current.current_registration_revision != preview.current_registration_revision
            || current.artifact_state_revision != preview.artifact_state_revision
            || current.component_plans != preview.component_plans
            || current.competing_extension_claims != preview.competing_extension_claims
        {
            return Err(HostBundleError::StalePreview);
        }
        registration.confirm_preview(component_set, request, preview)?;
        self.writer.execute_confirmed_component_set(
            component_set,
            request,
            preview,
            verifier,
            registration,
        )
    }

    pub fn execute<V: HostBundleVerificationAdapterV1, R: HostComponentSetRegistrationV1>(
        &mut self,
        component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
        verifier: &V,
        registration: &mut R,
    ) -> Result<HostComponentSetReceiptV1, HostBundleError> {
        // Host-scoped: a pending journal for an unrelated host governs a
        // disjoint artifact subtree and belongs to a different registration
        // adapter, so it must neither be recovered here nor block this work.
        self.recover_host(component_set.host, registration)?;
        self.writer
            .execute_component_set(component_set, request, verifier, registration)
    }
}

#[derive(Serialize)]
struct HostComponentSetPlanDigestPayloadV1 {
    domain: &'static str,
    schema_version: u16,
    operation_id: [u8; 16],
    operation: HostBundleLifecycleOpV1,
    host: HostKindV1,
    expected_components: Vec<HostBundleComponentV1>,
    hermes_profile_bindings: u8,
    base_registration_revision: [u8; 32],
    current_registration_revision: [u8; 32],
    artifact_state_revision: [u8; 32],
    component_plans: Vec<HostBundleMutationPlanV1>,
    competing_extension_claims: Vec<CompetingHostExtensionClaimV1>,
}

#[derive(Serialize)]
struct HostComponentSetArtifactStatePayloadV1 {
    domain: &'static str,
    schema_version: u16,
    components: Vec<HostComponentArtifactStateV1>,
}

#[derive(Serialize)]
struct HostComponentArtifactStateV1 {
    component: HostBundleComponentV1,
    manifest_digest: [u8; 32],
    receipt_digest: Option<[u8; 32]>,
    observed: Vec<ObservedHostArtifactV1>,
}

fn component_set_artifact_state_revision(
    artifact_root: &Path,
    lifecycle_root: &Path,
    component_set: &HostComponentSetV1,
) -> Result<[u8; 32], HostBundleError> {
    let mut components = component_set.components.iter().collect::<Vec<_>>();
    components.sort_by_key(|component| component.manifest.component);
    let mut states = Vec::with_capacity(components.len());
    for component in components {
        let receipt = read_receipt_at(
            lifecycle_root,
            component.manifest.host,
            component.manifest.component,
        )?;
        let receipt_digest = receipt
            .as_ref()
            .map(canonical_json_bytes)
            .transpose()
            .map_err(|_| HostBundleError::CanonicalizationFailed)?
            .map(|bytes| Sha256::digest(bytes).into());
        let mut paths = BTreeMap::new();
        for artifact in &component.manifest.artifacts {
            paths.insert(artifact.relative_path.clone(), (None, None));
        }
        if let Some(receipt) = &receipt {
            for artifact in &receipt.artifacts {
                paths.insert(
                    artifact.relative_path.clone(),
                    (
                        Some(artifact.ownership_marker.clone()),
                        Some(artifact.artifact_digest),
                    ),
                );
            }
        }
        let observed = paths
            .into_iter()
            .map(
                |(relative_path, (ownership_marker, owned_artifact_digest))| {
                    let cataloged_ownership_marker = component
                        .manifest
                        .artifacts
                        .iter()
                        .find(|artifact| artifact.relative_path == relative_path)
                        .map(|artifact| artifact.ownership_marker.clone());
                    observe_artifact_at(
                        artifact_root,
                        &relative_path,
                        ownership_marker,
                        owned_artifact_digest,
                        cataloged_ownership_marker,
                    )
                },
            )
            .collect::<Result<Vec<_>, _>>()?;
        states.push(HostComponentArtifactStateV1 {
            component: component.manifest.component,
            manifest_digest: component.manifest.canonical_digest()?,
            receipt_digest,
            observed,
        });
    }
    canonical_json_bytes(&HostComponentSetArtifactStatePayloadV1 {
        domain: "tracedecay.host-component-set.artifact-state.v1",
        schema_version: 1,
        components: states,
    })
    .map(|bytes| Sha256::digest(bytes).into())
    .map_err(|_| HostBundleError::CanonicalizationFailed)
}

fn component_set_plan_digest(
    request: &HostComponentSetExecutionRequestV1,
    base_registration_revision: [u8; 32],
    current_registration_revision: [u8; 32],
    artifact_state_revision: [u8; 32],
    component_plans: &[HostBundleMutationPlanV1],
    competing_extension_claims: &[CompetingHostExtensionClaimV1],
) -> Result<[u8; 32], HostBundleError> {
    let mut expected_components = request.lifecycle.expected_components.clone();
    expected_components.sort_unstable();
    let mut component_plans = component_plans.to_vec();
    for plan in &mut component_plans {
        plan.mutations
            .sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    }
    component_plans.sort_by_key(|plan| plan.component);
    let payload = HostComponentSetPlanDigestPayloadV1 {
        domain: "tracedecay.host-component-set.plan.v1",
        schema_version: 1,
        operation_id: request.operation_id,
        operation: request.lifecycle.operation,
        host: request.lifecycle.expected_host,
        expected_components,
        hermes_profile_bindings: request.lifecycle.hermes_profile_bindings,
        base_registration_revision,
        current_registration_revision,
        artifact_state_revision,
        component_plans,
        competing_extension_claims: competing_extension_claims.to_vec(),
    };
    canonical_json_bytes(&payload)
        .map(|bytes| Sha256::digest(bytes).into())
        .map_err(|_| HostBundleError::CanonicalizationFailed)
}

/// Production-composition seam for independently injected cryptographic and
/// filesystem authorities. It verifies before it asks storage to recover or
/// mutate, so an incompatible catalog entry cannot trigger filesystem access.
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
    #[allow(clippy::too_many_arguments)]
    pub fn dry_run(
        &self,
        manifest: &HostBundleManifestV1,
        request: &HostBundleExecutionRequestV1,
        manifest_observed: &[ObservedHostArtifactV1],
        owned_receipt: Option<&HostBundleInstallReceiptV1>,
        orphan_observed: &[ObservedHostArtifactV1],
        competing_extension_claims: &[CompetingHostExtensionClaimV1],
    ) -> Result<HostBundleLifecyclePreviewV1, HostBundleError> {
        if request.operation_id == [0; 16] {
            return Err(HostBundleError::InvalidManifest);
        }
        validate_competing_extension_claims(competing_extension_claims)?;
        self.verifier.verify_manifest(manifest)?;
        let mut planning_request = request.lifecycle.clone();
        planning_request.explicit_confirmation = true;
        let plan = plan_verified_complete_lifecycle_mutation(
            manifest,
            &planning_request,
            manifest_observed,
            owned_receipt,
            orphan_observed,
            &self.verifier,
        )?;
        let backup_relative_paths = plan
            .mutations
            .iter()
            .filter(|mutation| {
                matches!(
                    mutation.action,
                    HostArtifactActionV1::BackupThenReplace
                        | HostArtifactActionV1::BackupThenRemove
                )
            })
            .map(|mutation| mutation.relative_path.clone())
            .collect();
        Ok(HostBundleLifecyclePreviewV1 {
            confirmation_required: !request.lifecycle.explicit_confirmation
                || !competing_extension_claims.is_empty(),
            competing_extension_claims: competing_extension_claims.to_vec(),
            rollback: HostBundleRollbackSeamV1 {
                operation_id: request.operation_id,
                host: manifest.host,
                component: manifest.component,
                backup_relative_paths,
                interrupted_recovery_required: plan.rollback_required,
            },
            plan,
        })
    }

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

    pub fn execute_confirmed(
        &mut self,
        manifest: &HostBundleManifestV1,
        request: &HostBundleExecutionRequestV1,
        contents: &[HostBundleArtifactContentV1],
        competing_extension_claims: &[CompetingHostExtensionClaimV1],
    ) -> Result<HostBundleInstallReceiptV1, HostBundleError> {
        validate_competing_extension_claims(competing_extension_claims)?;
        if !competing_extension_claims.is_empty() && !request.lifecycle.explicit_confirmation {
            return Err(HostBundleError::ConfirmationRequired);
        }
        self.execute(manifest, request, contents)
    }
}

/// Read-only host-root preview used by the official CLI. It verifies the
/// manifest, reads existing receipts and artifact digests, and produces the
/// same immutable plan as apply without creating control files, backups, or
/// directories and without recovering an interrupted journal.
pub fn dry_run_host_bundle_lifecycle_at(
    root: &Path,
    manifest: &HostBundleManifestV1,
    request: &HostBundleExecutionRequestV1,
    verifier: &impl HostBundleVerificationAdapterV1,
    competing_extension_claims: &[CompetingHostExtensionClaimV1],
) -> Result<HostBundleLifecyclePreviewV1, HostBundleError> {
    dry_run_host_bundle_lifecycle_with_lifecycle_root_at(
        root,
        root,
        manifest,
        request,
        verifier,
        competing_extension_claims,
    )
}

pub fn dry_run_host_bundle_lifecycle_with_lifecycle_root_at(
    artifact_root: &Path,
    lifecycle_root: &Path,
    manifest: &HostBundleManifestV1,
    request: &HostBundleExecutionRequestV1,
    verifier: &impl HostBundleVerificationAdapterV1,
    competing_extension_claims: &[CompetingHostExtensionClaimV1],
) -> Result<HostBundleLifecyclePreviewV1, HostBundleError> {
    if request.operation_id == [0; 16] {
        return Err(HostBundleError::InvalidManifest);
    }
    validate_competing_extension_claims(competing_extension_claims)?;
    verifier.verify_manifest(manifest)?;
    let previous_receipt = read_receipt_at(lifecycle_root, manifest.host, manifest.component)?;
    let owned_receipt = previous_receipt
        .as_ref()
        .filter(|receipt| receipt.operation != HostBundleLifecycleOpV1::Uninstall);
    let manifest_observed = if request.lifecycle.operation == HostBundleLifecycleOpV1::Uninstall {
        Vec::new()
    } else {
        manifest
            .artifacts
            .iter()
            .map(|artifact| {
                let owned = owned_receipt.and_then(|receipt| {
                    receipt
                        .artifacts
                        .iter()
                        .find(|owned| owned.relative_path == artifact.relative_path)
                });
                observe_artifact_at(
                    artifact_root,
                    &artifact.relative_path,
                    owned.map(|owned| owned.ownership_marker.clone()),
                    owned.map(|owned| owned.artifact_digest),
                    Some(artifact.ownership_marker.clone()),
                )
            })
            .collect::<Result<Vec<_>, _>>()?
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
            .map(|owned| {
                observe_artifact_at(
                    artifact_root,
                    &owned.relative_path,
                    Some(owned.ownership_marker.clone()),
                    Some(owned.artifact_digest),
                    None,
                )
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };
    let mut planning_request = request.lifecycle.clone();
    planning_request.explicit_confirmation = true;
    let plan = plan_verified_complete_lifecycle_mutation(
        manifest,
        &planning_request,
        &manifest_observed,
        owned_receipt,
        &orphan_observed,
        verifier,
    )?;
    let backup_relative_paths = plan
        .mutations
        .iter()
        .filter(|mutation| {
            matches!(
                mutation.action,
                HostArtifactActionV1::BackupThenReplace | HostArtifactActionV1::BackupThenRemove
            )
        })
        .map(|mutation| mutation.relative_path.clone())
        .collect();
    Ok(HostBundleLifecyclePreviewV1 {
        confirmation_required: !request.lifecycle.explicit_confirmation
            || !competing_extension_claims.is_empty(),
        competing_extension_claims: competing_extension_claims.to_vec(),
        rollback: HostBundleRollbackSeamV1 {
            operation_id: request.operation_id,
            host: manifest.host,
            component: manifest.component,
            backup_relative_paths,
            interrupted_recovery_required: plan.rollback_required,
        },
        plan,
    })
}

/// Read-only component-set preview for the official CLI. The registration
/// adapter contributes the exact native-config revision, while every artifact
/// plan is derived through the same ownership-aware planner used by apply.
pub fn dry_run_host_component_set_lifecycle_with_lifecycle_root_at<
    V: HostBundleVerificationAdapterV1,
    R: HostComponentSetRegistrationV1,
>(
    artifact_root: &Path,
    lifecycle_root: &Path,
    component_set: &HostComponentSetV1,
    request: &HostComponentSetExecutionRequestV1,
    verifier: &V,
    registration: &mut R,
) -> Result<HostComponentSetLifecyclePreviewV1, HostBundleError> {
    let mut planning_request = request.clone();
    planning_request.lifecycle.explicit_confirmation = true;
    validate_component_set_request(component_set, &planning_request)?;
    let base_registration_revision =
        registration.current_revision(component_set, &planning_request)?;
    if base_registration_revision == [0; 32] {
        return Err(HostBundleError::InvalidObservedState);
    }
    let competing_extension_claims =
        discovered_competing_extension_claims(component_set, &planning_request, registration)?;
    registration.preflight(component_set, &planning_request)?;
    let base_artifact_state_revision =
        component_set_artifact_state_revision(artifact_root, lifecycle_root, component_set)?;
    let mut component_plans = Vec::with_capacity(component_set.components.len());
    for component in &component_set.components {
        validate_artifact_contents_for_operation(
            &component.manifest,
            planning_request.lifecycle.operation,
            &component.contents,
        )?;
        let component_request = HostBundleExecutionRequestV1 {
            lifecycle: HostBundleLifecycleRequestV1 {
                operation: planning_request.lifecycle.operation,
                expected_host: planning_request.lifecycle.expected_host,
                expected_component: component.manifest.component,
                explicit_confirmation: true,
                hermes_profile_bindings: planning_request.lifecycle.hermes_profile_bindings,
            },
            operation_id: planning_request.operation_id,
        };
        component_plans.push(
            dry_run_host_bundle_lifecycle_with_lifecycle_root_at(
                artifact_root,
                lifecycle_root,
                &component.manifest,
                &component_request,
                verifier,
                &competing_extension_claims,
            )?
            .plan,
        );
    }
    let current_registration_revision =
        registration.current_revision(component_set, &planning_request)?;
    if current_registration_revision != base_registration_revision {
        return Err(HostBundleError::StalePreview);
    }
    if discovered_competing_extension_claims(component_set, &planning_request, registration)?
        != competing_extension_claims
    {
        return Err(HostBundleError::StalePreview);
    }
    let current_artifact_state_revision =
        component_set_artifact_state_revision(artifact_root, lifecycle_root, component_set)?;
    if current_artifact_state_revision != base_artifact_state_revision {
        return Err(HostBundleError::StalePreview);
    }
    let artifact_state_revision = base_artifact_state_revision;
    let plan_digest = component_set_plan_digest(
        &planning_request,
        base_registration_revision,
        current_registration_revision,
        artifact_state_revision,
        &component_plans,
        &competing_extension_claims,
    )?;
    Ok(HostComponentSetLifecyclePreviewV1 {
        operation_id: request.operation_id,
        plan_digest,
        base_registration_revision,
        current_registration_revision,
        artifact_state_revision,
        component_plans,
        // A competing claim is ambiguous ownership: the operator confirms this
        // exact plan or nothing is mutated.
        confirmation_required: !request.lifecycle.explicit_confirmation
            || !competing_extension_claims.is_empty(),
        competing_extension_claims,
    })
}

/// Collect and normalise the adapter's claim discovery so preview, plan
/// digest, and apply all compare the same canonical ordering.
fn discovered_competing_extension_claims<R: HostComponentSetRegistrationV1>(
    component_set: &HostComponentSetV1,
    request: &HostComponentSetExecutionRequestV1,
    registration: &R,
) -> Result<Vec<CompetingHostExtensionClaimV1>, HostBundleError> {
    let mut claims = registration.discover_competing_extension_claims(component_set, request)?;
    claims.sort_by(|left, right| left.extension_id.cmp(&right.extension_id));
    validate_competing_extension_claims(&claims)?;
    Ok(claims)
}

pub fn inspect_installed_host_bundle_components_at(
    artifact_root: &Path,
    lifecycle_root: &Path,
    registrations: &impl HostBundleRegistrationInspectorV1,
) -> Result<HostBundleDoctorReportV1, HostBundleError> {
    match fs::symlink_metadata(lifecycle_root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err(HostBundleError::UnsafeInstallPath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(HostBundleDoctorReportV1::default());
        }
        Err(_) => return Err(host_bundle_storage_failure!()),
    }
    let control_root = lifecycle_root.join(HOST_BUNDLE_CONTROL_DIR);
    match fs::symlink_metadata(&control_root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err(HostBundleError::UnsafeInstallPath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(HostBundleDoctorReportV1::default());
        }
        Err(_) => return Err(host_bundle_storage_failure!()),
    }
    let entries = match fs::read_dir(&control_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(HostBundleDoctorReportV1::default());
        }
        Err(_) => return Err(host_bundle_storage_failure!()),
    };
    let mut receipt_paths = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("receipt.") && name.ends_with(".v1.json"))
        })
        .collect::<Vec<_>>();
    receipt_paths.sort();

    let ownership_claims = receipt_ownership_claims(&receipt_paths);

    let mut components = Vec::with_capacity(receipt_paths.len());
    for receipt_path in receipt_paths {
        let receipt_identity = receipt_path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(receipt_identity_from_file_name);
        let bytes = match fs::read(&receipt_path) {
            Ok(bytes) if !bytes.is_empty() && bytes.len() <= MAX_CONTROL_FILE_BYTES => bytes,
            _ => {
                components.push(corrupt_component_result(
                    receipt_path,
                    receipt_identity.map(|identity| identity.0),
                    receipt_identity.map(|identity| identity.1),
                ));
                continue;
            }
        };
        let receipt =
            if let Ok(receipt) = serde_json::from_slice::<HostBundleInstallReceiptV1>(&bytes) {
                receipt
            } else {
                components.push(corrupt_component_result(
                    receipt_path,
                    receipt_identity.map(|identity| identity.0),
                    receipt_identity.map(|identity| identity.1),
                ));
                continue;
            };
        if validate_receipt(&receipt).is_err() {
            components.push(corrupt_component_result(
                receipt_path,
                Some(receipt.host),
                Some(receipt.component),
            ));
            continue;
        }
        let expected_file = receipt_file(receipt.host, receipt.component);
        if receipt_path.file_name().and_then(|name| name.to_str()) != Some(expected_file.as_str()) {
            components.push(corrupt_component_result(
                receipt_path,
                Some(receipt.host),
                Some(receipt.component),
            ));
            continue;
        }
        if receipt.operation == HostBundleLifecycleOpV1::Uninstall {
            // An uninstall receipt owns nothing, so there are no artifacts to
            // check. The host can still advertise the component (a leftover
            // `extensions.json` entry, a stale plugin registration), and that
            // orphan is invisible if discovery simply skips the receipt.
            let registration = registrations.inspect_registration(receipt.host, receipt.component);
            if registration != HostBundleRegistrationStateV1::Missing {
                let state = HostBundleComponentDoctorStateV1::OrphanedRegistration;
                components.push(HostBundleComponentDoctorResultV1 {
                    repair_action: repair_action(
                        receipt.host,
                        receipt.component,
                        state,
                        registration,
                    ),
                    receipt_path,
                    host: Some(receipt.host),
                    component: Some(receipt.component),
                    state,
                    registration: Some(registration),
                    artifacts: Vec::new(),
                });
            }
            continue;
        }

        let catalog_current = crate::agents::host_bundle_registry::verified_embedded_host_bundle(
            receipt.host,
            receipt.component,
            0,
        )
        .ok()
        .is_some_and(|bundle| {
            bundle.manifest.canonical_digest() == Ok(receipt.manifest_digest)
                && receipt.artifacts.len() == bundle.manifest.artifacts.len()
                && receipt.artifacts.iter().all(|artifact| {
                    bundle.manifest.artifacts.iter().any(|expected| {
                        artifact.relative_path == expected.relative_path
                            && artifact.artifact_digest == expected.artifact_digest
                            && artifact.ownership_marker == expected.ownership_marker
                    })
                })
        });
        let registration = registrations.inspect_registration(receipt.host, receipt.component);
        let activation_guidance = registrations.interactive_activation_guidance(receipt.host);
        let mut artifacts = Vec::with_capacity(receipt.artifacts.len());
        for artifact in &receipt.artifacts {
            // Ownership at a deploy path is proven by receipt evidence, exactly
            // as `plan_artifact_action` proves it. A path claimed by more than
            // one component (or claimed with an unexpected marker) has no
            // single owner, so the marker is withheld and the planner's foreign
            // branch is what discovery reports.
            let sole_owner = ownership_claims
                .get(&artifact.relative_path)
                .is_some_and(|markers| {
                    markers.len() == 1 && markers.contains(&artifact.ownership_marker)
                });
            let observed = observe_artifact_at(
                artifact_root,
                &artifact.relative_path,
                sole_owner.then(|| artifact.ownership_marker.clone()),
                Some(artifact.artifact_digest),
                None,
            );
            let expected = HostBundleArtifactV1 {
                relative_path: artifact.relative_path.clone(),
                artifact_digest: artifact.artifact_digest,
                ownership_marker: artifact.ownership_marker.clone(),
            };
            let (observed_digest, state) = match observed {
                Ok(observed) => (
                    observed.artifact_digest,
                    doctor_artifact_state(&observed, &expected),
                ),
                Err(_) => (None, HostBundleComponentDoctorStateV1::Corrupt),
            };
            artifacts.push(HostBundleArtifactDoctorResultV1 {
                relative_path: artifact.relative_path.clone(),
                expected_digest: artifact.artifact_digest,
                observed_digest,
                ownership_marker: artifact.ownership_marker.clone(),
                state,
            });
        }
        let state = if receipt.rollback_boundary != HostBundleRollbackBoundaryV1::Passed
            || artifacts
                .iter()
                .any(|artifact| artifact.state == HostBundleComponentDoctorStateV1::Corrupt)
        {
            HostBundleComponentDoctorStateV1::Corrupt
        } else if artifacts
            .iter()
            .any(|artifact| artifact.state == HostBundleComponentDoctorStateV1::OwnershipConflict)
        {
            HostBundleComponentDoctorStateV1::OwnershipConflict
        } else if artifacts
            .iter()
            .any(|artifact| artifact.state == HostBundleComponentDoctorStateV1::Missing)
        {
            if activation_guidance.is_some()
                && artifacts_are_wholly_unmaterialised(&artifacts)
                && registration == HostBundleRegistrationStateV1::Repairable
            {
                HostBundleComponentDoctorStateV1::ActivationDeferred
            } else {
                HostBundleComponentDoctorStateV1::Missing
            }
        } else if artifacts
            .iter()
            .any(|artifact| artifact.state == HostBundleComponentDoctorStateV1::Drifted)
        {
            // Ranked below every contested or absent state: drift is repairable
            // by the ordinary reinstall, so it must not mask a conflict, a
            // missing artifact, or a corrupt receipt in the same component.
            HostBundleComponentDoctorStateV1::Drifted
        } else if !catalog_current {
            HostBundleComponentDoctorStateV1::Repairable
        } else {
            match registration {
                HostBundleRegistrationStateV1::Current => HostBundleComponentDoctorStateV1::Current,
                HostBundleRegistrationStateV1::Repairable
                | HostBundleRegistrationStateV1::Missing => {
                    HostBundleComponentDoctorStateV1::Repairable
                }
                HostBundleRegistrationStateV1::Corrupt => HostBundleComponentDoctorStateV1::Corrupt,
            }
        };
        // A deferred activation is finished by the host's own UI, so the host
        // adapter's exact wording is the repair action; nothing TraceDecay can
        // run would converge it.
        let component_repair_action = match (state, activation_guidance) {
            (HostBundleComponentDoctorStateV1::ActivationDeferred, Some(guidance)) => guidance,
            _ => repair_action(receipt.host, receipt.component, state, registration),
        };
        components.push(HostBundleComponentDoctorResultV1 {
            receipt_path,
            host: Some(receipt.host),
            component: Some(receipt.component),
            state,
            registration: Some(registration),
            artifacts,
            repair_action: component_repair_action,
        });
    }
    let journal_path = control_root.join(HOST_BUNDLE_JOURNAL_FILE);
    if journal_path.exists() {
        let journal = fs::read(&journal_path)
            .ok()
            .filter(|bytes| !bytes.is_empty() && bytes.len() <= MAX_CONTROL_FILE_BYTES)
            .and_then(|bytes| serde_json::from_slice::<HostBundleJournalV1>(&bytes).ok())
            .filter(|journal| validate_journal(journal).is_ok());
        match journal {
            Some(journal) => {
                if let Some(component) = components.iter_mut().find(|component| {
                    component.host == Some(journal.host)
                        && component.component == Some(journal.component)
                }) {
                    component.state = HostBundleComponentDoctorStateV1::Repairable;
                    component.repair_action = repair_action(
                        journal.host,
                        journal.component,
                        HostBundleComponentDoctorStateV1::Repairable,
                        HostBundleRegistrationStateV1::Current,
                    );
                } else {
                    components.push(HostBundleComponentDoctorResultV1 {
                        receipt_path: journal_path.clone(),
                        host: Some(journal.host),
                        component: Some(journal.component),
                        state: HostBundleComponentDoctorStateV1::Repairable,
                        registration: None,
                        artifacts: Vec::new(),
                        repair_action: repair_action(
                            journal.host,
                            journal.component,
                            HostBundleComponentDoctorStateV1::Repairable,
                            HostBundleRegistrationStateV1::Current,
                        ),
                    });
                }
            }
            None => components.push(corrupt_component_result(journal_path, None, None)),
        }
    }
    // Component-set journals are host-scoped; the legacy shared name is still
    // inspected so a journal left by an older binary stays visible to doctor.
    let component_set_journal_paths = std::iter::once(HOST_COMPONENT_SET_JOURNAL_FILE.to_string())
        .chain(
            stock_host_kinds()
                .into_iter()
                .map(component_set_journal_file),
        )
        .map(|file| control_root.join(file));
    for component_set_journal_path in component_set_journal_paths {
        if component_set_journal_path.exists() {
            let journal = fs::read(&component_set_journal_path)
                .ok()
                .filter(|bytes| !bytes.is_empty() && bytes.len() <= MAX_CONTROL_FILE_BYTES)
                .and_then(|bytes| serde_json::from_slice::<HostComponentSetJournalV1>(&bytes).ok())
                .filter(|journal| validate_component_set_journal(journal).is_ok());
            match journal {
                Some(journal) => {
                    for set_component in journal.components {
                        let host = set_component.manifest.host;
                        let component = set_component.manifest.component;
                        if let Some(result) = components.iter_mut().find(|result| {
                            result.host == Some(host) && result.component == Some(component)
                        }) {
                            result.state = HostBundleComponentDoctorStateV1::Repairable;
                            result.repair_action = repair_action(
                                host,
                                component,
                                HostBundleComponentDoctorStateV1::Repairable,
                                HostBundleRegistrationStateV1::Current,
                            );
                        } else {
                            components.push(HostBundleComponentDoctorResultV1 {
                                receipt_path: component_set_journal_path.clone(),
                                host: Some(host),
                                component: Some(component),
                                state: HostBundleComponentDoctorStateV1::Repairable,
                                registration: None,
                                artifacts: Vec::new(),
                                repair_action: repair_action(
                                    host,
                                    component,
                                    HostBundleComponentDoctorStateV1::Repairable,
                                    HostBundleRegistrationStateV1::Current,
                                ),
                            });
                        }
                    }
                }
                None => components.push(corrupt_component_result(
                    component_set_journal_path,
                    None,
                    None,
                )),
            }
        }
    }
    for entry in fs::read_dir(&control_root)
        .map_err(|_| host_bundle_storage_failure!())?
        .filter_map(Result::ok)
    {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with("feedback-rollback.") || !name.ends_with(".v1.json") {
            continue;
        }
        let Some(value) = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        else {
            components.push(corrupt_component_result(path, None, None));
            continue;
        };
        if value.get("status").and_then(serde_json::Value::as_str) == Some("restored") {
            continue;
        }
        let Some(host) = value
            .get("host")
            .cloned()
            .and_then(|host| serde_json::from_value::<HostKindV1>(host).ok())
        else {
            components.push(corrupt_component_result(path, None, None));
            continue;
        };
        let state_path = value
            .get("state_path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("<state-path>");
        let restore_action = format!(
            "run `tracedecay feedback-rollback restore --state {} --yes` for {}",
            state_path,
            host.descriptor().cli_id()
        );
        let component = HostBundleComponentV1::Core;
        if let Some(result) = components
            .iter_mut()
            .find(|result| result.host == Some(host) && result.component == Some(component))
        {
            result.state = HostBundleComponentDoctorStateV1::Repairable;
            result.receipt_path.clone_from(&path);
            result.repair_action.clone_from(&restore_action);
        } else {
            components.push(HostBundleComponentDoctorResultV1 {
                receipt_path: path,
                host: Some(host),
                component: Some(component),
                state: HostBundleComponentDoctorStateV1::Repairable,
                registration: None,
                artifacts: Vec::new(),
                repair_action: restore_action,
            });
        }
    }
    Ok(HostBundleDoctorReportV1 {
        components,
        ..HostBundleDoctorReportV1::default()
    })
}

/// Load the newest durable aggregate receipt for one host. This is used by
/// the official feedback rollback CLI to bind the currently installed route
/// to a compiled target without accepting an external bundle.
pub fn latest_host_component_set_receipt_at(
    lifecycle_root: &Path,
    host: HostKindV1,
) -> Result<Option<HostComponentSetReceiptV1>, HostBundleError> {
    let control_root = lifecycle_root.join(HOST_BUNDLE_CONTROL_DIR);
    let entries = match fs::read_dir(&control_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(host_bundle_storage_failure!()),
    };
    let mut latest: Option<(std::time::SystemTime, HostComponentSetReceiptV1)> = None;
    for entry in entries {
        let entry = entry.map_err(|_| host_bundle_storage_failure!())?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("component-set-receipt.") || !name.ends_with(".v1.json") {
            continue;
        }
        let metadata = entry
            .metadata()
            .map_err(|_| host_bundle_storage_failure!())?;
        if !metadata.is_file() || metadata.len() > MAX_CONTROL_FILE_BYTES as u64 {
            continue;
        }
        let bytes = fs::read(entry.path()).map_err(|_| host_bundle_storage_failure!())?;
        let Ok(receipt) = serde_json::from_slice::<HostComponentSetReceiptV1>(&bytes) else {
            continue;
        };
        if receipt.host != host
            || receipt.operation == HostBundleLifecycleOpV1::Uninstall
            || validate_component_set_receipt(&receipt).is_err()
        {
            continue;
        }
        let modified = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);
        if latest
            .as_ref()
            .is_none_or(|(current, _)| modified > *current)
        {
            latest = Some((modified, receipt));
        }
    }
    Ok(latest.map(|(_, receipt)| receipt))
}

/// Where rollback backups are written, one subdirectory per applied operation
/// id. Exposed so a dry run can tell the operator where the bytes it is about
/// to replace will be preserved, without the CLI reconstructing a
/// control-directory layout it does not own. The operation id is minted when
/// the mutation actually runs, so only the root is knowable during a preview.
#[must_use]
pub fn host_bundle_backup_root(lifecycle_root: &Path) -> PathBuf {
    lifecycle_root.join(HOST_BUNDLE_CONTROL_DIR).join("backups")
}

pub fn latest_host_component_receipt_at(
    lifecycle_root: &Path,
    host: HostKindV1,
    component: HostBundleComponentV1,
) -> Result<Option<HostBundleInstallReceiptV1>, HostBundleError> {
    read_receipt_at(lifecycle_root, host, component)
}

/// Every ownership marker the valid, non-uninstall receipts under one control
/// root claim for each deploy path. Discovery consults this instead of trusting
/// whichever receipt it happens to be reading, so a path two components both
/// claim is reported as a contested claim rather than silently attributed to
/// the receipt that sorted first.
fn receipt_ownership_claims(receipt_paths: &[PathBuf]) -> BTreeMap<String, BTreeSet<String>> {
    let mut claims: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for receipt_path in receipt_paths {
        let Ok(bytes) = fs::read(receipt_path) else {
            continue;
        };
        if bytes.is_empty() || bytes.len() > MAX_CONTROL_FILE_BYTES {
            continue;
        }
        let Ok(receipt) = serde_json::from_slice::<HostBundleInstallReceiptV1>(&bytes) else {
            continue;
        };
        if validate_receipt(&receipt).is_err()
            || receipt.operation == HostBundleLifecycleOpV1::Uninstall
            || receipt_path.file_name().and_then(|name| name.to_str())
                != Some(receipt_file(receipt.host, receipt.component).as_str())
        {
            continue;
        }
        for artifact in &receipt.artifacts {
            claims
                .entry(artifact.relative_path.clone())
                .or_default()
                .insert(artifact.ownership_marker.clone());
        }
    }
    claims
}

/// Doctor-side mirror of [`plan_artifact_action`]'s marker-vs-digest boundary
/// under `Repair` — the operation every repair action recommends.
///
/// The ownership marker is the only conflict gate: a foreign or absent marker
/// is a contested path that planning refuses outside the narrow pre-receipt
/// adoption boundary, while a path whose marker is still this component's own
/// is ordinary content drift that `Repair` plans as `BackupThenReplace`.
/// Keeping the two in lockstep means discovery can never report a conflict the
/// planner would have converged, or vice versa.
fn doctor_artifact_state(
    observed: &ObservedHostArtifactV1,
    expected: &HostBundleArtifactV1,
) -> HostBundleComponentDoctorStateV1 {
    match observed.kind {
        ObservedArtifactKindV1::Missing => return HostBundleComponentDoctorStateV1::Missing,
        ObservedArtifactKindV1::Symlink | ObservedArtifactKindV1::Directory => {
            return HostBundleComponentDoctorStateV1::Corrupt;
        }
        ObservedArtifactKindV1::RegularFile => {}
    }
    if observed.ownership_marker.as_deref() != Some(expected.ownership_marker.as_str()) {
        return HostBundleComponentDoctorStateV1::OwnershipConflict;
    }
    if observed.artifact_digest == Some(expected.artifact_digest) {
        HostBundleComponentDoctorStateV1::Current
    } else {
        HostBundleComponentDoctorStateV1::Drifted
    }
}

/// Whether the receipt's deploy paths carry no evidence that the host ever
/// materialised this component.
///
/// This is what separates a never-activated component from real drift. A
/// component that holds even one of its receipt-owned files was materialised at
/// some point, so the absent siblings are bytes that went missing afterwards —
/// exactly the receipt-integrity failure the blocking `Missing` state exists to
/// report. Only a wholly absent set can honestly be attributed to an activation
/// the operator has not performed yet. A receipt with no artifacts at all proves
/// nothing either way, so it is excluded.
fn artifacts_are_wholly_unmaterialised(artifacts: &[HostBundleArtifactDoctorResultV1]) -> bool {
    !artifacts.is_empty()
        && artifacts
            .iter()
            .all(|artifact| artifact.state == HostBundleComponentDoctorStateV1::Missing)
}

fn corrupt_component_result(
    receipt_path: PathBuf,
    host: Option<HostKindV1>,
    component: Option<HostBundleComponentV1>,
) -> HostBundleComponentDoctorResultV1 {
    let repair_action = match (host, component) {
        (Some(HostKindV1::KimiCode), Some(_)) => format!(
            "remove the corrupt receipt {}, run `tracedecay install --agent kimi` to refresh the staged bundle, then open Kimi Code and run `/plugins install ~/.tracedecay/host-bundle-stage/kimi/tracedecay`; rerun Doctor to verify registration",
            receipt_path.display()
        ),
        (Some(host), Some(component)) => format!(
            "remove the corrupt receipt {}, then run `tracedecay install --agent {} --component {} --yes`",
            receipt_path.display(),
            host.descriptor().cli_id(),
            component_slug(component)
        ),
        _ => format!(
            "quarantine the unidentifiable corrupt receipt {} and reinstall its owning host component",
            receipt_path.display()
        ),
    };
    HostBundleComponentDoctorResultV1 {
        repair_action,
        receipt_path,
        host,
        component,
        state: HostBundleComponentDoctorStateV1::Corrupt,
        registration: None,
        artifacts: Vec::new(),
    }
}

fn repair_action(
    host: HostKindV1,
    component: HostBundleComponentV1,
    state: HostBundleComponentDoctorStateV1,
    registration: HostBundleRegistrationStateV1,
) -> String {
    if host == HostKindV1::KimiCode && state != HostBundleComponentDoctorStateV1::Current {
        return "run `tracedecay install --agent kimi` to refresh the staged bundle, then open Kimi Code and run `/plugins install ~/.tracedecay/host-bundle-stage/kimi/tracedecay`; rerun Doctor to verify registration".to_string();
    }
    let component = component_slug(component);
    let host_descriptor = host.descriptor();
    let host = host_descriptor.cli_id();
    match state {
        HostBundleComponentDoctorStateV1::Current => "none".to_string(),
        HostBundleComponentDoctorStateV1::Repairable
            if registration != HostBundleRegistrationStateV1::Current =>
        {
            format!("run `tracedecay install --agent {host}`")
        }
        HostBundleComponentDoctorStateV1::OwnershipConflict => format!(
            "resolve the foreign or modified files for {host}/{component}, then run `tracedecay reinstall --component {component} --yes`"
        ),
        HostBundleComponentDoctorStateV1::Drifted => format!(
            "run `tracedecay reinstall --component {component} --yes` (backs up and re-owns)"
        ),
        HostBundleComponentDoctorStateV1::OrphanedRegistration => format!(
            "{host} still registers {component} with no owning receipt; run `tracedecay uninstall --agent {host} --component {component} --yes` to finish removing it, or `tracedecay reinstall --component {component} --yes` to re-own it"
        ),
        // Reached only when an inspector classifies a deferral without
        // supplying its host's own wording; recommending a reinstall here would
        // be the advice that cannot converge, so name the user action instead.
        HostBundleComponentDoctorStateV1::ActivationDeferred => format!(
            "{host} activates {component} only through its interactive plugin UI; activate tracedecay there, then re-run doctor"
        ),
        HostBundleComponentDoctorStateV1::Repairable
        | HostBundleComponentDoctorStateV1::Missing
        | HostBundleComponentDoctorStateV1::Corrupt => {
            format!("run `tracedecay reinstall --component {component} --yes`")
        }
    }
}

fn read_receipt_at(
    root: &Path,
    host: HostKindV1,
    component: HostBundleComponentV1,
) -> Result<Option<HostBundleInstallReceiptV1>, HostBundleError> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err(HostBundleError::UnsafeInstallPath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(host_bundle_storage_failure!()),
    }
    let relative = Path::new(HOST_BUNDLE_CONTROL_DIR).join(receipt_file(host, component));
    let path = inspect_install_target(root, &relative)?;
    let bytes = match fs::read(&path) {
        Ok(bytes) if bytes.len() <= MAX_CONTROL_FILE_BYTES => bytes,
        Ok(_) => return Err(HostBundleError::ReceiptCorrupted),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(host_bundle_storage_failure!()),
    };
    let receipt = serde_json::from_slice(&bytes).map_err(|_| HostBundleError::ReceiptCorrupted)?;
    validate_receipt(&receipt)?;
    if receipt.host != host || receipt.component != component {
        return Err(HostBundleError::ReceiptCorrupted);
    }
    Ok(Some(receipt))
}

fn observe_artifact_at(
    root: &Path,
    relative_path: &str,
    ownership_marker: Option<String>,
    owned_artifact_digest: Option<[u8; 32]>,
    cataloged_ownership_marker: Option<String>,
) -> Result<ObservedHostArtifactV1, HostBundleError> {
    let path = inspect_install_target(root, Path::new(relative_path))?;
    let (kind, artifact_digest) = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() => {
            if metadata.len() > MAX_ARTIFACT_CONTENT_BYTES as u64 {
                return Err(HostBundleError::ArtifactContentMismatch);
            }
            let bytes = fs::read(&path).map_err(|_| host_bundle_storage_failure!())?;
            (
                ObservedArtifactKindV1::RegularFile,
                Some(Sha256::digest(&bytes).into()),
            )
        }
        Ok(metadata) if metadata.is_dir() => (ObservedArtifactKindV1::Directory, None),
        Ok(_) => return Err(HostBundleError::UnsafeInstallPath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            (ObservedArtifactKindV1::Missing, None)
        }
        Err(_) => return Err(host_bundle_storage_failure!()),
    };
    Ok(ObservedHostArtifactV1 {
        relative_path: relative_path.to_string(),
        kind,
        artifact_digest,
        ownership_marker,
        owned_artifact_digest,
        cataloged_ownership_marker,
    })
}

fn validate_competing_extension_claims(
    claims: &[CompetingHostExtensionClaimV1],
) -> Result<(), HostBundleError> {
    for (index, claim) in claims.iter().enumerate() {
        validate_identifier(&claim.extension_id)?;
        if claim.evidence_digest == [0; 32]
            || claims[..index]
                .iter()
                .any(|existing| existing.extension_id == claim.extension_id)
        {
            return Err(HostBundleError::InvalidObservedState);
        }
    }
    Ok(())
}

/// Durable evidence that one host's feedback path moved between two verified
/// embedded first-party core bundle versions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedbackPathRollbackReceiptV1 {
    pub host: HostKindV1,
    pub previous_manifest_digest: [u8; 32],
    pub applied_manifest_digest: [u8; 32],
    pub apply_receipt: HostBundleInstallReceiptV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedbackPathRestoreReceiptV1 {
    pub switch_operation_id: [u8; 16],
    pub restore_receipt: HostBundleInstallReceiptV1,
}

/// Concrete feedback rollback switch backed by the same digest-verified,
/// receipt-based atomic host-bundle lifecycle as
/// install/update/repair/uninstall. It owns no host-local scorer, scheduler,
/// store, or feedback business logic.
pub struct FeedbackPathRollbackSwitchV1<V, S> {
    lifecycle: HostBundleLifecycleRuntimeV1<V, S>,
}

impl<V, S> FeedbackPathRollbackSwitchV1<V, S> {
    pub fn new(lifecycle: HostBundleLifecycleRuntimeV1<V, S>) -> Self {
        Self { lifecycle }
    }

    pub fn into_lifecycle(self) -> HostBundleLifecycleRuntimeV1<V, S> {
        self.lifecycle
    }
}

impl<V, S> FeedbackPathRollbackSwitchV1<V, S>
where
    V: HostBundleVerificationAdapterV1,
    S: HostBundleLifecycleStorageV1,
{
    #[allow(clippy::too_many_arguments)]
    pub fn feedback_rollback_switch_dry_run(
        &self,
        previous_manifest: &HostBundleManifestV1,
        target_manifest: &HostBundleManifestV1,
        request: &HostBundleExecutionRequestV1,
        manifest_observed: &[ObservedHostArtifactV1],
        owned_receipt: Option<&HostBundleInstallReceiptV1>,
        orphan_observed: &[ObservedHostArtifactV1],
        competing_extension_claims: &[CompetingHostExtensionClaimV1],
    ) -> Result<HostBundleLifecyclePreviewV1, HostBundleError> {
        validate_feedback_switch_manifests(previous_manifest, target_manifest)?;
        self.lifecycle.verifier.verify_manifest(previous_manifest)?;
        self.lifecycle.dry_run(
            target_manifest,
            request,
            manifest_observed,
            owned_receipt,
            orphan_observed,
            competing_extension_claims,
        )
    }

    pub fn feedback_rollback_switch_apply(
        &mut self,
        previous_manifest: &HostBundleManifestV1,
        target_manifest: &HostBundleManifestV1,
        request: &HostBundleExecutionRequestV1,
        target_contents: &[HostBundleArtifactContentV1],
        competing_extension_claims: &[CompetingHostExtensionClaimV1],
    ) -> Result<FeedbackPathRollbackReceiptV1, HostBundleError> {
        validate_feedback_switch_manifests(previous_manifest, target_manifest)?;
        self.lifecycle.verifier.verify_manifest(previous_manifest)?;
        if !request.lifecycle.explicit_confirmation {
            return Err(HostBundleError::ConfirmationRequired);
        }
        let previous_manifest_digest = previous_manifest.canonical_digest()?;
        let applied_manifest_digest = target_manifest.canonical_digest()?;
        let apply_receipt = self.lifecycle.execute_confirmed(
            target_manifest,
            request,
            target_contents,
            competing_extension_claims,
        )?;
        Ok(FeedbackPathRollbackReceiptV1 {
            host: target_manifest.host,
            previous_manifest_digest,
            applied_manifest_digest,
            apply_receipt,
        })
    }

    pub fn feedback_rollback_switch_restore(
        &mut self,
        switch_receipt: &FeedbackPathRollbackReceiptV1,
        previous_manifest: &HostBundleManifestV1,
        request: &HostBundleExecutionRequestV1,
        previous_contents: &[HostBundleArtifactContentV1],
        competing_extension_claims: &[CompetingHostExtensionClaimV1],
    ) -> Result<FeedbackPathRestoreReceiptV1, HostBundleError> {
        if !request.lifecycle.explicit_confirmation {
            return Err(HostBundleError::ConfirmationRequired);
        }
        validate_receipt(&switch_receipt.apply_receipt)?;
        if switch_receipt.apply_receipt.rollback_boundary != HostBundleRollbackBoundaryV1::Passed {
            return Err(HostBundleError::ReceiptCorrupted);
        }
        if previous_manifest.host != switch_receipt.host
            || previous_manifest.component != HostBundleComponentV1::Core
            || previous_manifest.canonical_digest()? != switch_receipt.previous_manifest_digest
            || request.lifecycle.operation != HostBundleLifecycleOpV1::Repair
            || request.lifecycle.expected_host != switch_receipt.host
            || request.lifecycle.expected_component != HostBundleComponentV1::Core
            || switch_receipt.apply_receipt.host != switch_receipt.host
            || switch_receipt.apply_receipt.component != HostBundleComponentV1::Core
            || switch_receipt.apply_receipt.manifest_digest
                != switch_receipt.applied_manifest_digest
        {
            return Err(HostBundleError::WrongTarget);
        }
        let restore_receipt = self.lifecycle.execute_confirmed(
            previous_manifest,
            request,
            previous_contents,
            competing_extension_claims,
        )?;
        Ok(FeedbackPathRestoreReceiptV1 {
            switch_operation_id: switch_receipt.apply_receipt.operation_id,
            restore_receipt,
        })
    }
}

fn validate_feedback_switch_manifests(
    previous_manifest: &HostBundleManifestV1,
    target_manifest: &HostBundleManifestV1,
) -> Result<(), HostBundleError> {
    if previous_manifest.host != target_manifest.host
        || previous_manifest.component != HostBundleComponentV1::Core
        || target_manifest.component != HostBundleComponentV1::Core
        || previous_manifest.canonical_digest()? == target_manifest.canonical_digest()?
    {
        return Err(HostBundleError::WrongTarget);
    }
    Ok(())
}

struct PreparedHostComponentSetComponentV1 {
    manifest: HostBundleManifestV1,
    content_by_path: BTreeMap<String, Vec<u8>>,
    plan: HostBundleMutationPlanV1,
    previous_receipt: Option<HostBundleInstallReceiptV1>,
}

/// Atomic, capability-rooted host-bundle writer. Every descendant directory
/// is opened without following symlinks; files are staged, fsynced, renamed,
/// and followed by a directory sync before receipt publication.
pub struct HostBundleWriterV1 {
    root_path: PathBuf,
    lifecycle_root_path: PathBuf,
    root: Dir,
    control: Dir,
    _writer_lock: fs::File,
}

impl HostBundleWriterV1 {
    pub fn open(root_path: impl Into<PathBuf>) -> Result<Self, HostBundleError> {
        let root_path = root_path.into();
        Self::open_with_lifecycle_root(root_path.clone(), root_path)
    }

    pub fn open_with_lifecycle_root(
        root_path: impl Into<PathBuf>,
        lifecycle_root_path: impl Into<PathBuf>,
    ) -> Result<Self, HostBundleError> {
        let root_path = root_path.into();
        let lifecycle_root_path = lifecycle_root_path.into();
        ensure_bundle_root(&root_path)?;
        ensure_bundle_root(&lifecycle_root_path)?;
        let root = Dir::open_ambient_dir(&root_path, ambient_authority())
            .map_err(|_| HostBundleError::UnsafeInstallPath)?;
        let lifecycle_root = Dir::open_ambient_dir(&lifecycle_root_path, ambient_authority())
            .map_err(|_| HostBundleError::UnsafeInstallPath)?;
        let control = open_or_create_nofollow_dir(&lifecycle_root, HOST_BUNDLE_CONTROL_DIR)?;
        let writer_lock = open_writer_lock(&control)?;
        let mut writer = Self {
            root_path,
            lifecycle_root_path,
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
        if let Some(receipt) =
            self.load_receipt(journal.host, journal.component)?
                .filter(|receipt| {
                    receipt.operation_id == journal.operation_id
                        && receipt.operation == journal.operation
                        && receipt.manifest_digest == journal.manifest_digest
                })
        {
            self.remove_control_file(HOST_BUNDLE_JOURNAL_FILE)?;
            if receipt.rollback_boundary == HostBundleRollbackBoundaryV1::Passed {
                self.cleanup_unreferenced_backup_dir(journal.operation_id)?;
            }
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
                    .map_err(|_| host_bundle_storage_failure!())?;
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
        self.remove_control_file(HOST_BUNDLE_JOURNAL_FILE)?;
        self.cleanup_unreferenced_backup_dir(journal.operation_id)
    }

    /// Verify first-party catalog identity, validate artifact bytes, plan ownership-aware
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
        // Scoped to this manifest's own host: another host's pending
        // component-set journal governs a disjoint artifact subtree.
        if self
            .load_component_set_journal_for(manifest.host)?
            .is_some()
        {
            return Err(HostBundleError::RecoveryRequired);
        }
        self.recover_interrupted_operation()?;
        let previous_receipt = self.load_receipt(manifest.host, manifest.component)?;
        let manifest_digest = manifest.canonical_digest()?;
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
            rollback_boundary: HostBundleRollbackBoundaryV1::Passed,
            rollback_history: previous_receipt
                .as_ref()
                .map(|receipt| receipt.rollback_history.clone())
                .unwrap_or_default(),
        };
        self.write_receipt(&receipt)?;
        journal.state = HostBundleJournalStateV1::Committed;
        self.write_journal(&journal)?;
        self.remove_control_file(HOST_BUNDLE_JOURNAL_FILE)?;
        if receipt.rollback_boundary == HostBundleRollbackBoundaryV1::Passed {
            self.cleanup_unreferenced_backup_dir(request.operation_id)?;
        }
        Ok(receipt)
    }

    /// Execute a complete canonical host component set under one aggregate
    /// journal. Every component is preflighted and staged before any owned
    /// file is moved; receipts are published only after all artifacts and the
    /// host registration adapter verify successfully.
    pub fn execute_component_set<
        V: HostBundleVerificationAdapterV1,
        R: HostComponentSetRegistrationV1,
    >(
        &mut self,
        component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
        verifier: &V,
        registration: &mut R,
    ) -> Result<HostComponentSetReceiptV1, HostBundleError> {
        self.execute_component_set_with_preview(
            component_set,
            request,
            None,
            verifier,
            registration,
        )
    }

    fn execute_confirmed_component_set<
        V: HostBundleVerificationAdapterV1,
        R: HostComponentSetRegistrationV1,
    >(
        &mut self,
        component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
        preview: &HostComponentSetLifecyclePreviewV1,
        verifier: &V,
        registration: &mut R,
    ) -> Result<HostComponentSetReceiptV1, HostBundleError> {
        self.execute_component_set_with_preview(
            component_set,
            request,
            Some(preview),
            verifier,
            registration,
        )
    }

    fn execute_component_set_with_preview<
        V: HostBundleVerificationAdapterV1,
        R: HostComponentSetRegistrationV1,
    >(
        &mut self,
        component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
        confirmed_preview: Option<&HostComponentSetLifecyclePreviewV1>,
        verifier: &V,
        registration: &mut R,
    ) -> Result<HostComponentSetReceiptV1, HostBundleError> {
        validate_component_set_request(component_set, request)?;
        if self.load_journal()?.is_some() {
            return Err(HostBundleError::RecoveryRequired);
        }
        // Never clobber this host's own outstanding journal: it is the only
        // durable record of how to roll the earlier transaction back.
        if self
            .load_component_set_journal_for(component_set.host)?
            .is_some()
        {
            return Err(HostBundleError::RecoveryRequired);
        }
        if let Some(receipt) = self.load_component_set_receipt(request.operation_id)? {
            if !component_set_receipt_matches(&receipt, component_set, request)? {
                return Err(HostBundleError::ReceiptCorrupted);
            }
            if confirmed_preview
                .is_some_and(|preview| !component_set_receipt_matches_preview(&receipt, preview))
            {
                return Err(HostBundleError::StalePreview);
            }
            return Ok(receipt);
        }

        let prepared = self.preflight_component_set(component_set, request, verifier)?;
        // Declare the exact write set before any adapter observes state, so a
        // registration surface that is also one of these artifacts can tell
        // this transaction's own write apart from a foreign edit.
        let declared_writes = prepared
            .iter()
            .flat_map(|component| component.plan.mutations.iter())
            .map(|mutation| self.root_path.join(&mutation.relative_path))
            .collect::<Vec<_>>();
        registration.declare_artifact_writes(component_set, request, &declared_writes)?;
        registration.preflight(component_set, request)?;

        let mut journal = HostComponentSetJournalV1 {
            schema_version: HOST_BUNDLE_RECEIPT_SCHEMA_VERSION,
            operation_id: request.operation_id,
            host: component_set.host,
            operation: request.lifecycle.operation,
            explicit_confirmation: request.lifecycle.explicit_confirmation,
            hermes_profile_bindings: request.lifecycle.hermes_profile_bindings,
            confirmed_plan_digest: confirmed_preview.map(|preview| preview.plan_digest),
            base_registration_revision: confirmed_preview
                .map(|preview| preview.base_registration_revision),
            current_registration_revision: confirmed_preview
                .map(|preview| preview.current_registration_revision),
            artifact_state_revision: confirmed_preview
                .map(|preview| preview.artifact_state_revision),
            state: HostComponentSetJournalStateV1::Prepared,
            registration_staged: false,
            registration_applied: false,
            components: prepared
                .iter()
                .map(|component| HostComponentSetJournalComponentV1 {
                    manifest: component.manifest.clone(),
                    previous_receipt: component.previous_receipt.clone(),
                    entries: component
                        .plan
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
                            installed_digest: component
                                .manifest
                                .artifacts
                                .iter()
                                .find(|artifact| artifact.relative_path == mutation.relative_path)
                                .map(|artifact| artifact.artifact_digest)
                                .filter(|_| {
                                    !matches!(
                                        mutation.action,
                                        HostArtifactActionV1::BackupThenRemove
                                    )
                                }),
                        })
                        .collect(),
                })
                .collect(),
        };
        self.write_component_set_journal(&journal)?;

        let result = (|| {
            self.stage_component_set_assets(&prepared, request.operation_id)?;
            journal.registration_staged = true;
            self.write_component_set_journal(&journal)?;
            registration.stage(component_set, request)?;
            journal.state = HostComponentSetJournalStateV1::Staged;
            self.write_component_set_journal(&journal)?;

            let backup_dir = self.open_or_create_backup_dir(request.operation_id)?;
            self.backup_component_set_entries(&prepared, &mut journal, &backup_dir)?;
            self.write_component_set_entries(&prepared, &mut journal)?;

            // Mark this before calling into host registration: a failing
            // adapter can still have made a partial native mutation.
            journal.registration_applied = true;
            self.write_component_set_journal(&journal)?;
            registration.apply(component_set, request)?;
            journal.state = HostComponentSetJournalStateV1::Applied;
            self.write_component_set_journal(&journal)?;

            self.verify_component_set_artifacts(&journal)?;
            registration.verify(component_set, request)?;
            journal.state = HostComponentSetJournalStateV1::Verified;
            self.write_component_set_journal(&journal)?;

            let receipt =
                component_set_receipt_from_prepared(&prepared, request, confirmed_preview)?;
            for component_receipt in &receipt.component_receipts {
                self.write_receipt(component_receipt)?;
            }
            self.write_component_set_receipt(&receipt)?;
            journal.state = HostComponentSetJournalStateV1::Committed;
            self.write_component_set_journal(&journal)?;

            // Registration cleanup and backup retirement happen only after the
            // aggregate and every component receipt have crossed commit.
            registration.commit(component_set, request)?;
            self.cleanup_component_set_boundary(request.operation_id)?;
            self.remove_component_set_journal(component_set.host)?;
            Ok(receipt)
        })();

        match result {
            Ok(receipt) => Ok(receipt),
            Err(error) if journal.state == HostComponentSetJournalStateV1::Committed => {
                // The durable receipts prove commit. Keep the journal for a
                // restarted transaction to finish registration/backup cleanup.
                Err(error)
            }
            Err(error) => {
                if self
                    .rollback_component_set(component_set, request, registration, &mut journal)
                    .is_err()
                {
                    Err(HostBundleError::RecoveryRequired)
                } else {
                    Err(error)
                }
            }
        }
    }

    /// Resume a component-set operation left by a failed apply or a process
    /// interruption. A fully published aggregate receipt wins; any other
    /// state is rolled back in reverse component and artifact order.
    fn recover_component_set_operation<R: HostComponentSetRegistrationV1>(
        &mut self,
        host: Option<HostKindV1>,
        registration: &mut R,
    ) -> Result<(), HostBundleError> {
        let loaded = match host {
            Some(host) => self.load_component_set_journal_for(host)?,
            None => self.load_component_set_journal()?,
        };
        let Some(mut journal) = loaded else {
            return Ok(());
        };
        validate_component_set_journal(&journal)?;
        let component_set = component_set_from_journal(&journal);
        let request = HostComponentSetExecutionRequestV1 {
            lifecycle: HostComponentSetLifecycleRequestV1 {
                operation: journal.operation,
                expected_host: journal.host,
                expected_components: journal
                    .components
                    .iter()
                    .map(|component| component.manifest.component)
                    .collect(),
                explicit_confirmation: journal.explicit_confirmation,
                hermes_profile_bindings: journal.hermes_profile_bindings,
            },
            operation_id: journal.operation_id,
        };

        if journal.state == HostComponentSetJournalStateV1::Committed
            || self.component_set_commit_is_complete(&journal)?
        {
            registration.commit(&component_set, &request)?;
            self.cleanup_component_set_boundary(journal.operation_id)?;
            self.remove_component_set_journal(journal.host)?;
            return Ok(());
        }

        if journal.state == HostComponentSetJournalStateV1::RolledBack {
            // A rolled-back journal keeps whichever flags the failed attempt
            // had reached, so they describe the interrupted work rather than
            // the compensation still owed. Re-attempt it unconditionally: the
            // adapter contract is idempotent and no-ops when it finds no staged
            // registration backup, while skipping it would strand a mutated
            // native host configuration with nothing left to compensate it.
            registration.rollback(&component_set, &request)?;
            self.cleanup_component_set_boundary(journal.operation_id)?;
            self.remove_component_set_journal(journal.host)?;
            return Ok(());
        }

        if journal.registration_compensation_required() {
            registration.rollback(&component_set, &request)?;
        }
        self.restore_component_set_artifacts(&journal)?;
        journal.state = HostComponentSetJournalStateV1::RolledBack;
        self.write_component_set_journal(&journal)?;
        self.cleanup_component_set_boundary(journal.operation_id)?;
        self.remove_component_set_journal(journal.host)
    }

    fn preflight_component_set<V: HostBundleVerificationAdapterV1>(
        &self,
        component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
        verifier: &V,
    ) -> Result<Vec<PreparedHostComponentSetComponentV1>, HostBundleError> {
        let mut prepared = Vec::with_capacity(component_set.components.len());
        let mut claimed_paths = BTreeMap::new();

        for component in &component_set.components {
            component.manifest.validate_structure()?;
            verifier.verify_manifest(&component.manifest)?;
            let content_by_path = validate_artifact_contents_for_operation(
                &component.manifest,
                request.lifecycle.operation,
                &component.contents,
            )?;
            let previous_receipt =
                self.load_receipt(component.manifest.host, component.manifest.component)?;
            let owned_receipt = previous_receipt
                .as_ref()
                .filter(|receipt| receipt.operation != HostBundleLifecycleOpV1::Uninstall);
            let manifest_observed =
                if request.lifecycle.operation == HostBundleLifecycleOpV1::Uninstall {
                    Vec::new()
                } else {
                    component
                        .manifest
                        .artifacts
                        .iter()
                        .map(|artifact| {
                            let owned = owned_receipt.and_then(|receipt| {
                                receipt
                                    .artifacts
                                    .iter()
                                    .find(|owned| owned.relative_path == artifact.relative_path)
                            });
                            observe_artifact_at(
                                &self.root_path,
                                &artifact.relative_path,
                                owned.map(|owned| owned.ownership_marker.clone()),
                                owned.map(|owned| owned.artifact_digest),
                                Some(artifact.ownership_marker.clone()),
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?
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
                            || !component
                                .manifest
                                .artifacts
                                .iter()
                                .any(|artifact| artifact.relative_path == owned.relative_path)
                    })
                    .map(|owned| {
                        observe_artifact_at(
                            &self.root_path,
                            &owned.relative_path,
                            Some(owned.ownership_marker.clone()),
                            Some(owned.artifact_digest),
                            None,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                Vec::new()
            };
            let lifecycle = HostBundleLifecycleRequestV1 {
                operation: request.lifecycle.operation,
                expected_host: request.lifecycle.expected_host,
                expected_component: component.manifest.component,
                explicit_confirmation: request.lifecycle.explicit_confirmation,
                hermes_profile_bindings: request.lifecycle.hermes_profile_bindings,
            };
            let plan = plan_verified_complete_lifecycle_mutation(
                &component.manifest,
                &lifecycle,
                &manifest_observed,
                owned_receipt,
                &orphan_observed,
                verifier,
            )?;
            for mutation in &plan.mutations {
                if claimed_paths
                    .insert(mutation.relative_path.clone(), component.manifest.component)
                    .is_some()
                {
                    return Err(HostBundleError::InvalidObservedState);
                }
            }
            prepared.push(PreparedHostComponentSetComponentV1 {
                manifest: component.manifest.clone(),
                content_by_path,
                plan,
                previous_receipt,
            });
        }
        Ok(prepared)
    }

    fn stage_component_set_assets(
        &self,
        prepared: &[PreparedHostComponentSetComponentV1],
        operation_id: [u8; 16],
    ) -> Result<(), HostBundleError> {
        let stage = self.open_or_create_component_set_stage_dir(operation_id)?;
        for component in prepared {
            for (relative_path, bytes) in &component.content_by_path {
                let stage_name =
                    component_set_stage_name(component.manifest.component, relative_path);
                atomic_write_nofollow(&stage, &stage_name, bytes, false)?;
            }
        }
        sync_cap_dir(&stage)
    }

    fn backup_component_set_entries(
        &self,
        prepared: &[PreparedHostComponentSetComponentV1],
        journal: &mut HostComponentSetJournalV1,
        backup_dir: &Dir,
    ) -> Result<(), HostBundleError> {
        for (component_index, prepared_component) in prepared.iter().enumerate() {
            for (entry_index, mutation) in prepared_component.plan.mutations.iter().enumerate() {
                if !matches!(
                    mutation.action,
                    HostArtifactActionV1::BackupThenReplace
                        | HostArtifactActionV1::BackupThenRemove
                ) {
                    continue;
                }
                let backup_name = journal.components[component_index].entries[entry_index]
                    .backup_name
                    .clone()
                    .ok_or(HostBundleError::ReceiptCorrupted)?;
                let (parent, name) =
                    self.open_parent_nofollow(Path::new(&mutation.relative_path))?;
                move_regular_to_backup(&parent, &name, backup_dir, &backup_name)?;
                journal.components[component_index].entries[entry_index].backup_created = true;
                self.write_component_set_journal(journal)?;
            }
        }
        Ok(())
    }

    fn write_component_set_entries(
        &self,
        prepared: &[PreparedHostComponentSetComponentV1],
        journal: &mut HostComponentSetJournalV1,
    ) -> Result<(), HostBundleError> {
        for (component_index, prepared_component) in prepared.iter().enumerate() {
            for (entry_index, mutation) in prepared_component.plan.mutations.iter().enumerate() {
                let (parent, name) =
                    self.open_parent_nofollow(Path::new(&mutation.relative_path))?;
                match mutation.action {
                    HostArtifactActionV1::Noop | HostArtifactActionV1::BackupThenRemove => {}
                    HostArtifactActionV1::WriteNew | HostArtifactActionV1::BackupThenReplace => {
                        journal.components[component_index].entries[entry_index].wrote_new = true;
                        self.write_component_set_journal(journal)?;
                        atomic_write_nofollow(
                            &parent,
                            &name,
                            prepared_component
                                .content_by_path
                                .get(&mutation.relative_path)
                                .ok_or(HostBundleError::ArtifactContentMismatch)?,
                            false,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    fn verify_component_set_artifacts(
        &self,
        journal: &HostComponentSetJournalV1,
    ) -> Result<(), HostBundleError> {
        for component in &journal.components {
            for entry in &component.entries {
                let (parent, name) = self.open_parent_nofollow(Path::new(&entry.relative_path))?;
                let observed = read_regular_nofollow(&parent, &name)?;
                match (entry.installed_digest, observed) {
                    (Some(expected), Some(bytes)) => {
                        let digest: [u8; 32] = Sha256::digest(&bytes).into();
                        if digest != expected {
                            return Err(HostBundleError::ArtifactContentMismatch);
                        }
                    }
                    (Some(_), None) => return Err(HostBundleError::ArtifactContentMismatch),
                    (None, None) => {}
                    (None, Some(_)) => return Err(HostBundleError::OwnershipConflict),
                }
            }
        }
        Ok(())
    }

    fn rollback_component_set<R: HostComponentSetRegistrationV1>(
        &mut self,
        component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
        registration: &mut R,
        journal: &mut HostComponentSetJournalV1,
    ) -> Result<(), HostBundleError> {
        if journal.registration_compensation_required() {
            registration.rollback(component_set, request)?;
        }
        self.restore_component_set_artifacts(journal)?;
        self.remove_component_set_receipt(journal.operation_id)?;
        journal.state = HostComponentSetJournalStateV1::RolledBack;
        // Leave the completed rollback journal and its backups for an explicit
        // restart reconciliation boundary; a new transaction invokes recover.
        self.write_component_set_journal(journal)
    }

    fn restore_component_set_artifacts(
        &self,
        journal: &HostComponentSetJournalV1,
    ) -> Result<(), HostBundleError> {
        let backup_dir = self.open_existing_backup_dir(journal.operation_id)?;
        for component in journal.components.iter().rev() {
            for entry in component.entries.iter().rev() {
                self.restore_component_set_entry(entry, backup_dir.as_ref())?;
            }
        }
        for component in journal.components.iter().rev() {
            match &component.previous_receipt {
                Some(receipt) => self.write_receipt(receipt)?,
                None => self.remove_receipt(journal.host, component.manifest.component)?,
            }
        }
        Ok(())
    }

    /// Restore one journal entry to its pre-transaction state.
    ///
    /// Rollback must be able to CONVERGE when a second writer touched a
    /// deployed path after this transaction wrote it. The compatibility
    /// registration adapter re-runs a legacy installer over the same paths
    /// during `apply`, so a post-apply failure routinely leaves live bytes that
    /// are neither the backup nor this transaction's cataloged output. Before
    /// the convergence rules below, that state was unrecoverable: rollback
    /// returned `RecoveryRequired` forever, the journal stayed behind, and
    /// every later host transaction failed up front.
    ///
    /// Two content equalities are provably safe to converge on, because in both
    /// cases the operator-visible end state is byte-identical to a successful
    /// restore:
    ///
    /// 1. **Live bytes equal the pre-transaction backup.** The end state
    ///    rollback wants is already true; renaming the backup over it would
    ///    produce the same bytes. Treat the path as restored.
    /// 2. **Live bytes equal this entry's cataloged install target
    ///    (`installed_digest`).** Those bytes are provably this transaction's
    ///    own output, so removing them is a restore and not third-party data
    ///    loss. This also closes the crash window between the artifact write
    ///    and the `wrote_new` journal update.
    ///
    /// Anything else — foreign bytes that match neither — stays fail-closed
    /// with `RecoveryRequired`, and the operator resolves it explicitly with
    /// `tracedecay host-bundle recover`.
    fn restore_component_set_entry(
        &self,
        entry: &HostBundleJournalEntryV1,
        backup_dir: Option<&Dir>,
    ) -> Result<(), HostBundleError> {
        let (parent, name) = self.open_parent_nofollow(Path::new(&entry.relative_path))?;
        if let Some(backup_name) = &entry.backup_name {
            let backup_bytes = match backup_dir {
                Some(backups) => read_regular_nofollow(backups, backup_name)?,
                None => None,
            };
            let backup_exists = backup_bytes.is_some();
            // Convergence rule 1: the live file already holds the exact
            // pre-transaction bytes, so this path needs no mutation at all.
            // The backup stays until the boundary cleanup retires the whole
            // operation directory, which keeps a repeated restore idempotent.
            if let (Some(backup), Some(live)) = (
                backup_bytes.as_ref(),
                read_regular_nofollow(&parent, &name)?,
            ) && live == *backup
            {
                return Ok(());
            }
            if !entry.backup_created {
                if !backup_exists {
                    return Ok(());
                }
                if regular_file_exists(&parent, &name)? {
                    return Err(HostBundleError::RecoveryRequired);
                }
            }
            let backups = backup_dir
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
            } else if let Some(live) = read_regular_nofollow(&parent, &name)? {
                // Convergence rule 2. `installed_digest` is `None` for a
                // BackupThenRemove entry, which has no cataloged target and
                // therefore stays fail-closed.
                let installed = entry
                    .installed_digest
                    .ok_or(HostBundleError::RecoveryRequired)?;
                if <[u8; 32]>::from(Sha256::digest(&live)) != installed {
                    return Err(HostBundleError::RecoveryRequired);
                }
                parent
                    .remove_file(&name)
                    .map_err(|_| host_bundle_storage_failure!())?;
            }
            backups
                .rename(backup_name, &parent, &name)
                .map_err(|_| host_bundle_storage_failure!())?;
            sync_cap_dir(backups)?;
            sync_cap_dir(&parent)
        } else if entry.wrote_new {
            // No backup: the path did not exist before the transaction, so
            // rollback wants it gone. `remove_if_digest_matches` already
            // converges on the two safe outcomes (already absent, or holding
            // this transaction's cataloged bytes). Foreign bytes at a path this
            // transaction created are genuinely ambiguous — removing them could
            // destroy another writer's file — so that case stays fail-closed.
            remove_if_digest_matches(
                &parent,
                &name,
                entry
                    .installed_digest
                    .ok_or(HostBundleError::ReceiptCorrupted)?,
            )?;
            sync_cap_dir(&parent)
        } else {
            Ok(())
        }
    }

    fn component_set_commit_is_complete(
        &self,
        journal: &HostComponentSetJournalV1,
    ) -> Result<bool, HostBundleError> {
        let Some(receipt) = self.load_component_set_receipt(journal.operation_id)? else {
            return Ok(false);
        };
        let component_set = component_set_from_journal(journal);
        let request = HostComponentSetExecutionRequestV1 {
            lifecycle: HostComponentSetLifecycleRequestV1 {
                operation: journal.operation,
                expected_host: journal.host,
                expected_components: component_set
                    .components
                    .iter()
                    .map(|component| component.manifest.component)
                    .collect(),
                explicit_confirmation: true,
                hermes_profile_bindings: u8::from(journal.host == HostKindV1::Hermes),
            },
            operation_id: journal.operation_id,
        };
        component_set_receipt_matches(&receipt, &component_set, &request)
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
                cataloged_ownership_marker: Some(artifact.ownership_marker.clone()),
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
            cataloged_ownership_marker: None,
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

    fn open_or_create_component_set_stage_dir(
        &self,
        operation_id: [u8; 16],
    ) -> Result<Dir, HostBundleError> {
        let stages = open_or_create_nofollow_dir(&self.control, HOST_COMPONENT_SET_STAGE_DIR)?;
        open_or_create_nofollow_dir(&stages, &hex::encode(operation_id))
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

    fn load_component_set_receipt(
        &self,
        operation_id: [u8; 16],
    ) -> Result<Option<HostComponentSetReceiptV1>, HostBundleError> {
        let receipt = read_control_json(&self.control, &component_set_receipt_file(operation_id))?
            .map(|bytes| {
                serde_json::from_slice(&bytes).map_err(|_| HostBundleError::ReceiptCorrupted)
            })
            .transpose()?;
        if let Some(receipt) = &receipt {
            validate_component_set_receipt(receipt)?;
        }
        Ok(receipt)
    }

    fn write_component_set_receipt(
        &self,
        receipt: &HostComponentSetReceiptV1,
    ) -> Result<(), HostBundleError> {
        validate_component_set_receipt(receipt)?;
        let bytes = serde_json::to_vec(receipt).map_err(|_| HostBundleError::ReceiptCorrupted)?;
        atomic_write_nofollow(
            &self.control,
            &component_set_receipt_file(receipt.operation_id),
            &bytes,
            false,
        )
    }

    fn remove_component_set_receipt(&self, operation_id: [u8; 16]) -> Result<(), HostBundleError> {
        self.remove_control_file(&component_set_receipt_file(operation_id))
    }

    fn load_journal(&self) -> Result<Option<HostBundleJournalV1>, HostBundleError> {
        read_control_json(&self.control, HOST_BUNDLE_JOURNAL_FILE)?
            .map(|bytes| {
                serde_json::from_slice(&bytes).map_err(|_| HostBundleError::ReceiptCorrupted)
            })
            .transpose()
    }

    fn read_component_set_journal_file(
        &self,
        file_name: &str,
    ) -> Result<Option<HostComponentSetJournalV1>, HostBundleError> {
        read_control_json(&self.control, file_name)?
            .map(|bytes| {
                serde_json::from_slice(&bytes).map_err(|_| HostBundleError::ReceiptCorrupted)
            })
            .transpose()
    }

    /// Load the pending component-set journal for one host.
    ///
    /// Journals are host-scoped so an interrupted transaction for host X never
    /// blocks an unrelated host Y. Journals written by an older binary live
    /// under the shared legacy name; they carry their own `host` field, so they
    /// are readable here and are attributed to exactly one host.
    fn load_component_set_journal_for(
        &self,
        host: HostKindV1,
    ) -> Result<Option<HostComponentSetJournalV1>, HostBundleError> {
        if let Some(journal) =
            self.read_component_set_journal_file(&component_set_journal_file(host))?
        {
            return Ok(Some(journal));
        }
        Ok(self
            .read_component_set_journal_file(HOST_COMPONENT_SET_JOURNAL_FILE)?
            .filter(|journal| journal.host == host))
    }

    /// Load any pending component-set journal, host-scoped or legacy. Used by
    /// the host-blind recovery entry point, which must still be able to find a
    /// single outstanding transaction.
    fn load_component_set_journal(
        &self,
    ) -> Result<Option<HostComponentSetJournalV1>, HostBundleError> {
        for host in stock_host_kinds() {
            if let Some(journal) =
                self.read_component_set_journal_file(&component_set_journal_file(host))?
            {
                return Ok(Some(journal));
            }
        }
        self.read_component_set_journal_file(HOST_COMPONENT_SET_JOURNAL_FILE)
    }

    /// Every host with a pending component-set journal. The recovery verb
    /// reports these; `--agent` narrows the set.
    pub fn pending_component_set_journal_hosts(&self) -> Result<Vec<HostKindV1>, HostBundleError> {
        let mut hosts = Vec::new();
        for host in stock_host_kinds() {
            if self.load_component_set_journal_for(host)?.is_some() {
                hosts.push(host);
            }
        }
        Ok(hosts)
    }

    pub fn pending_component_set_journal_operation(
        &self,
        host: HostKindV1,
    ) -> Result<Option<HostBundleLifecycleOpV1>, HostBundleError> {
        let Some(journal) = self.load_component_set_journal_for(host)? else {
            return Ok(None);
        };
        validate_component_set_journal(&journal)?;
        Ok(Some(journal.operation))
    }

    fn write_journal(&self, journal: &HostBundleJournalV1) -> Result<(), HostBundleError> {
        validate_journal(journal)?;
        let bytes = serde_json::to_vec(journal).map_err(|_| HostBundleError::ReceiptCorrupted)?;
        atomic_write_nofollow(&self.control, HOST_BUNDLE_JOURNAL_FILE, &bytes, true)
    }

    fn write_component_set_journal(
        &self,
        journal: &HostComponentSetJournalV1,
    ) -> Result<(), HostBundleError> {
        validate_component_set_journal(journal)?;
        let bytes = serde_json::to_vec(journal).map_err(|_| HostBundleError::ReceiptCorrupted)?;
        atomic_write_nofollow(
            &self.control,
            &component_set_journal_file(journal.host),
            &bytes,
            true,
        )?;
        // A journal written by an older binary lives under the shared legacy
        // name. Once its host-scoped successor is durable, retire it so the
        // legacy file can never shadow or double-recover this transaction.
        if self
            .read_component_set_journal_file(HOST_COMPONENT_SET_JOURNAL_FILE)?
            .is_some_and(|legacy| legacy.host == journal.host)
        {
            self.remove_control_file(HOST_COMPONENT_SET_JOURNAL_FILE)?;
        }
        Ok(())
    }

    fn remove_control_file(&self, name: &str) -> Result<(), HostBundleError> {
        remove_regular_if_exists(&self.control, name)?;
        sync_cap_dir(&self.control)
    }

    /// Last-resort operator escape when convergent recovery still cannot
    /// resolve a host's component-set journal (genuinely foreign bytes at a
    /// path the transaction created, for example).
    ///
    /// The journal is *moved* into a quarantine directory rather than deleted:
    /// the transaction's immutable backups stay on disk beside it, so the
    /// pre-transaction bytes remain recoverable by hand and nothing about the
    /// failure is destroyed. Only the authority file that blocks further
    /// mutation of this host is set aside. This replaces the previous recovery
    /// path, which was hand-deleting the journal.
    ///
    /// Returns the quarantined path, or `None` when no journal was pending.
    pub fn quarantine_component_set_journal(
        &mut self,
        host: HostKindV1,
        now_unix: u64,
    ) -> Result<Option<PathBuf>, HostBundleError> {
        let mut moved = None;
        for file in [
            component_set_journal_file(host),
            HOST_COMPONENT_SET_JOURNAL_FILE.to_string(),
        ] {
            // The legacy shared file belongs to whichever host wrote it; never
            // quarantine another host's journal from under it.
            if file == HOST_COMPONENT_SET_JOURNAL_FILE
                && self
                    .read_component_set_journal_file(&file)?
                    .is_none_or(|journal| journal.host != host)
            {
                continue;
            }
            if !regular_file_exists(&self.control, &file)? {
                continue;
            }
            let quarantine =
                open_or_create_nofollow_dir(&self.control, HOST_BUNDLE_QUARANTINE_DIR)?;
            let target = format!("{now_unix}.{file}");
            if !is_safe_component(&target) {
                return Err(HostBundleError::UnsafeInstallPath);
            }
            self.control
                .rename(&file, &quarantine, &target)
                .map_err(|_| host_bundle_storage_failure!())?;
            sync_cap_dir(&quarantine)?;
            sync_cap_dir(&self.control)?;
            moved = Some(
                self.lifecycle_root_path
                    .join(HOST_BUNDLE_CONTROL_DIR)
                    .join(HOST_BUNDLE_QUARANTINE_DIR)
                    .join(target),
            );
        }
        Ok(moved)
    }

    fn remove_component_set_journal(&self, host: HostKindV1) -> Result<(), HostBundleError> {
        self.remove_control_file(&component_set_journal_file(host))?;
        if self
            .read_component_set_journal_file(HOST_COMPONENT_SET_JOURNAL_FILE)?
            .is_some_and(|legacy| legacy.host == host)
        {
            self.remove_control_file(HOST_COMPONENT_SET_JOURNAL_FILE)?;
        }
        Ok(())
    }

    fn cleanup_unreferenced_backup_dir(
        &self,
        operation_id: [u8; 16],
    ) -> Result<(), HostBundleError> {
        let control_path = self.lifecycle_root_path.join(HOST_BUNDLE_CONTROL_DIR);
        let mut referenced = false;
        for entry in fs::read_dir(&control_path).map_err(|_| host_bundle_storage_failure!())? {
            let Ok(entry) = entry else {
                return Ok(());
            };
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.starts_with("receipt.") || !name.ends_with(".v1.json") {
                continue;
            }
            let Ok(bytes) = fs::read(entry.path()) else {
                return Ok(());
            };
            let Ok(receipt) = serde_json::from_slice::<HostBundleInstallReceiptV1>(&bytes) else {
                return Ok(());
            };
            if validate_receipt(&receipt).is_err() {
                return Ok(());
            }
            referenced |= receipt.rollback_history.contains(&operation_id);
        }
        if referenced {
            return Ok(());
        }
        let backup_path = control_path.join("backups").join(hex::encode(operation_id));
        match fs::symlink_metadata(&backup_path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                fs::remove_dir_all(&backup_path).map_err(|_| host_bundle_storage_failure!())?;
            }
            Ok(_) => return Err(HostBundleError::UnsafeInstallPath),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(host_bundle_storage_failure!()),
        }
        if let Some(backups) = backup_path.parent() {
            let _ = fs::remove_dir(backups);
        }
        Ok(())
    }

    fn cleanup_component_set_boundary(
        &self,
        operation_id: [u8; 16],
    ) -> Result<(), HostBundleError> {
        self.cleanup_unreferenced_backup_dir(operation_id)?;
        self.remove_component_set_stage_dir(operation_id)
    }

    fn remove_component_set_stage_dir(
        &self,
        operation_id: [u8; 16],
    ) -> Result<(), HostBundleError> {
        let stage_path = self
            .lifecycle_root_path
            .join(HOST_BUNDLE_CONTROL_DIR)
            .join(HOST_COMPONENT_SET_STAGE_DIR)
            .join(hex::encode(operation_id));
        match fs::symlink_metadata(&stage_path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                fs::remove_dir_all(&stage_path).map_err(|_| host_bundle_storage_failure!())?;
            }
            Ok(_) => return Err(HostBundleError::UnsafeInstallPath),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(host_bundle_storage_failure!()),
        }
        if let Some(stages) = stage_path.parent() {
            let _ = fs::remove_dir(stages);
        }
        Ok(())
    }

    /// Snapshot one installed component without mutating host state. Replaying
    /// the same operation id returns the existing receipt after revalidation.
    /// A missing, edited, or foreign artifact fails before receipt publication.
    pub fn backup_component<V: HostBundleVerificationAdapterV1>(
        &self,
        manifest: &HostBundleManifestV1,
        operation_id: [u8; 16],
        explicit_confirmation: bool,
        verifier: &V,
    ) -> Result<HostBundleBackupReceiptV1, HostBundleError> {
        if operation_id == [0; 16] {
            return Err(HostBundleError::InvalidManifest);
        }
        if !explicit_confirmation {
            return Err(HostBundleError::ConfirmationRequired);
        }
        manifest.validate_structure()?;
        verifier.verify_manifest(manifest)?;
        if let Some(receipt) = self.load_backup_receipt(operation_id)? {
            validate_backup_receipt(&receipt)?;
            self.read_backup_contents(&receipt)?;
            return (receipt.manifest == *manifest)
                .then_some(receipt)
                .ok_or(HostBundleError::ReceiptCorrupted);
        }

        let source_receipt = self
            .load_receipt(manifest.host, manifest.component)?
            .filter(|receipt| receipt.operation != HostBundleLifecycleOpV1::Uninstall)
            .ok_or(HostBundleError::InvalidObservedState)?;
        if source_receipt.manifest_digest != manifest.canonical_digest()?
            || source_receipt.artifacts.len() != manifest.artifacts.len()
        {
            return Err(HostBundleError::InvalidObservedState);
        }
        let source_receipt_digest: [u8; 32] = Sha256::digest(
            canonical_json_bytes(&source_receipt)
                .map_err(|_| HostBundleError::CanonicalizationFailed)?,
        )
        .into();
        let snapshot_dir = self.open_or_create_snapshot_dir(operation_id)?;
        let mut artifacts = Vec::with_capacity(source_receipt.artifacts.len());
        for (index, owned) in source_receipt.artifacts.iter().enumerate() {
            let expected = manifest
                .artifacts
                .iter()
                .find(|artifact| artifact.relative_path == owned.relative_path)
                .filter(|artifact| {
                    artifact.artifact_digest == owned.artifact_digest
                        && artifact.ownership_marker == owned.ownership_marker
                })
                .ok_or(HostBundleError::InvalidObservedState)?;
            let (parent, name) = self.open_parent_nofollow(Path::new(&expected.relative_path))?;
            let bytes = read_regular_nofollow(&parent, &name)?
                .ok_or(HostBundleError::InvalidObservedState)?;
            if <[u8; 32]>::from(Sha256::digest(&bytes)) != expected.artifact_digest {
                return Err(HostBundleError::OwnershipConflict);
            }
            let snapshot_name = host_bundle_snapshot_name(index, &expected.relative_path);
            match read_regular_nofollow(&snapshot_dir, &snapshot_name)? {
                Some(existing) if existing == bytes => {}
                Some(_) => return Err(HostBundleError::ReceiptCorrupted),
                None => atomic_write_nofollow(&snapshot_dir, &snapshot_name, &bytes, false)?,
            }
            artifacts.push(HostBundleBackupArtifactV1 {
                relative_path: expected.relative_path.clone(),
                artifact_digest: expected.artifact_digest,
                ownership_marker: expected.ownership_marker.clone(),
                snapshot_name,
            });
        }
        sync_cap_dir(&snapshot_dir)?;
        let receipt = HostBundleBackupReceiptV1 {
            schema_version: HOST_BUNDLE_RECEIPT_SCHEMA_VERSION,
            operation_id,
            host: manifest.host,
            component: manifest.component,
            manifest: manifest.clone(),
            source_receipt_digest,
            artifacts,
        };
        self.write_backup_receipt(&receipt)?;
        Ok(receipt)
    }

    /// Restore a named component backup through the ordinary Repair
    /// transaction. Any failure rolls the host files back to their pre-restore
    /// bytes; replaying `operation_id` returns the durable terminal receipt.
    pub fn restore_component_backup<V: HostBundleVerificationAdapterV1>(
        &mut self,
        backup_operation_id: [u8; 16],
        operation_id: [u8; 16],
        explicit_confirmation: bool,
        verifier: &V,
    ) -> Result<HostBundleRestoreReceiptV1, HostBundleError> {
        if backup_operation_id == [0; 16] || operation_id == [0; 16] {
            return Err(HostBundleError::InvalidManifest);
        }
        if !explicit_confirmation {
            return Err(HostBundleError::ConfirmationRequired);
        }
        if let Some(receipt) = self.load_restore_receipt(operation_id)? {
            validate_restore_receipt(&receipt)?;
            return (receipt.backup_operation_id == backup_operation_id)
                .then_some(receipt)
                .ok_or(HostBundleError::ReceiptCorrupted);
        }
        let backup = self
            .load_backup_receipt(backup_operation_id)?
            .ok_or(HostBundleError::InvalidObservedState)?;
        validate_backup_receipt(&backup)?;
        verifier.verify_manifest(&backup.manifest)?;
        let contents = self.read_backup_contents(&backup)?;
        let request = HostBundleExecutionRequestV1 {
            lifecycle: HostBundleLifecycleRequestV1 {
                operation: HostBundleLifecycleOpV1::Repair,
                expected_host: backup.host,
                expected_component: backup.component,
                explicit_confirmation: true,
                hermes_profile_bindings: u8::from(backup.host == HostKindV1::Hermes),
            },
            operation_id,
        };
        let restored_receipt = self.execute(&backup.manifest, &request, &contents, verifier)?;
        let receipt = HostBundleRestoreReceiptV1 {
            schema_version: HOST_BUNDLE_RECEIPT_SCHEMA_VERSION,
            operation_id,
            backup_operation_id,
            restored_receipt,
        };
        self.write_restore_receipt(&receipt)?;
        Ok(receipt)
    }

    pub fn publish_feedback_component_set_receipt(
        &self,
        manifest: &HostBundleManifestV1,
        component_receipt: &HostBundleInstallReceiptV1,
    ) -> Result<HostComponentSetReceiptV1, HostBundleError> {
        if manifest.host != component_receipt.host
            || manifest.component != component_receipt.component
            || manifest.canonical_digest()? != component_receipt.manifest_digest
        {
            return Err(HostBundleError::WrongTarget);
        }
        let previous = latest_host_component_set_receipt_at(
            &self.lifecycle_root_path,
            component_receipt.host,
        )?;
        let mut component_manifests = previous
            .as_ref()
            .map(|receipt| receipt.component_manifests.clone())
            .unwrap_or_default();
        component_manifests.retain(|previous| previous.component != manifest.component);
        component_manifests.push(manifest.clone());
        component_manifests.sort_by_key(|manifest| manifest.component);
        let mut component_receipts = previous
            .map(|receipt| receipt.component_receipts)
            .unwrap_or_default();
        component_receipts.retain(|previous| previous.component != component_receipt.component);
        component_receipts.push(component_receipt.clone());
        component_receipts.sort_by_key(|receipt| receipt.component);
        let receipt = HostComponentSetReceiptV1 {
            schema_version: HOST_BUNDLE_RECEIPT_SCHEMA_VERSION,
            operation_id: component_receipt.operation_id,
            host: component_receipt.host,
            operation: component_receipt.operation,
            component_manifests,
            component_receipts,
            confirmed_plan_digest: None,
            base_registration_revision: None,
            current_registration_revision: None,
            artifact_state_revision: None,
        };
        self.write_component_set_receipt(&receipt)?;
        Ok(receipt)
    }

    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    pub fn lifecycle_root_path(&self) -> &Path {
        &self.lifecycle_root_path
    }

    fn open_or_create_snapshot_dir(&self, operation_id: [u8; 16]) -> Result<Dir, HostBundleError> {
        let snapshots = open_or_create_nofollow_dir(&self.control, "snapshots")?;
        open_or_create_nofollow_dir(&snapshots, &hex::encode(operation_id))
    }

    fn open_existing_snapshot_dir(
        &self,
        operation_id: [u8; 16],
    ) -> Result<Option<Dir>, HostBundleError> {
        let snapshots = match self.control.open_dir_nofollow("snapshots") {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(HostBundleError::UnsafeInstallPath),
        };
        match snapshots.open_dir_nofollow(hex::encode(operation_id)) {
            Ok(directory) => Ok(Some(directory)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(HostBundleError::UnsafeInstallPath),
        }
    }

    fn read_backup_contents(
        &self,
        receipt: &HostBundleBackupReceiptV1,
    ) -> Result<Vec<HostBundleArtifactContentV1>, HostBundleError> {
        validate_backup_receipt(receipt)?;
        let snapshot_dir = self
            .open_existing_snapshot_dir(receipt.operation_id)?
            .ok_or(HostBundleError::ReceiptCorrupted)?;
        receipt
            .artifacts
            .iter()
            .map(|artifact| {
                let bytes = read_regular_nofollow(&snapshot_dir, &artifact.snapshot_name)?
                    .ok_or(HostBundleError::ReceiptCorrupted)?;
                if <[u8; 32]>::from(Sha256::digest(&bytes)) != artifact.artifact_digest {
                    return Err(HostBundleError::ReceiptCorrupted);
                }
                Ok(HostBundleArtifactContentV1 {
                    relative_path: artifact.relative_path.clone(),
                    bytes,
                })
            })
            .collect()
    }

    fn load_backup_receipt(
        &self,
        operation_id: [u8; 16],
    ) -> Result<Option<HostBundleBackupReceiptV1>, HostBundleError> {
        read_control_json(
            &self.control,
            &host_bundle_backup_receipt_file(operation_id),
        )?
        .map(|bytes| serde_json::from_slice(&bytes).map_err(|_| HostBundleError::ReceiptCorrupted))
        .transpose()
    }

    fn write_backup_receipt(
        &self,
        receipt: &HostBundleBackupReceiptV1,
    ) -> Result<(), HostBundleError> {
        validate_backup_receipt(receipt)?;
        let bytes = serde_json::to_vec(receipt).map_err(|_| HostBundleError::ReceiptCorrupted)?;
        atomic_write_nofollow(
            &self.control,
            &host_bundle_backup_receipt_file(receipt.operation_id),
            &bytes,
            false,
        )
    }

    fn load_restore_receipt(
        &self,
        operation_id: [u8; 16],
    ) -> Result<Option<HostBundleRestoreReceiptV1>, HostBundleError> {
        read_control_json(
            &self.control,
            &host_bundle_restore_receipt_file(operation_id),
        )?
        .map(|bytes| serde_json::from_slice(&bytes).map_err(|_| HostBundleError::ReceiptCorrupted))
        .transpose()
    }

    fn write_restore_receipt(
        &self,
        receipt: &HostBundleRestoreReceiptV1,
    ) -> Result<(), HostBundleError> {
        validate_restore_receipt(receipt)?;
        let bytes = serde_json::to_vec(receipt).map_err(|_| HostBundleError::ReceiptCorrupted)?;
        atomic_write_nofollow(
            &self.control,
            &host_bundle_restore_receipt_file(receipt.operation_id),
            &bytes,
            false,
        )
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
    validate_artifact_contents_for_operation(manifest, request.lifecycle.operation, contents)
}

fn validate_artifact_contents_for_operation(
    manifest: &HostBundleManifestV1,
    operation: HostBundleLifecycleOpV1,
    contents: &[HostBundleArtifactContentV1],
) -> Result<BTreeMap<String, Vec<u8>>, HostBundleError> {
    let uninstall = operation == HostBundleLifecycleOpV1::Uninstall;
    if uninstall && contents.is_empty() {
        return Ok(BTreeMap::new());
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
    // A canonical component set keeps its embedded assets for every lifecycle
    // operation. Validate supplied uninstall content, but do not stage bytes
    // that are only needed to prove the compiled catalog identity.
    if uninstall {
        Ok(BTreeMap::new())
    } else {
        Ok(values)
    }
}

fn ensure_bundle_root(root: &Path) -> Result<(), HostBundleError> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(HostBundleError::UnsafeInstallPath);
        }
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(host_bundle_storage_failure!()),
    }
    fs::create_dir_all(root).map_err(|_| host_bundle_storage_failure!())?;
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
                .map_err(|_| host_bundle_storage_failure!())?;
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
        Err(_) => return Err(host_bundle_storage_failure!()),
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
        .map_err(|_| host_bundle_storage_failure!())?;
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
        Err(_) => Err(host_bundle_storage_failure!()),
    }
}

fn remove_regular_if_exists(parent: &Dir, name: &str) -> Result<(), HostBundleError> {
    if regular_file_exists(parent, name)? {
        parent
            .remove_file(name)
            .map_err(|_| host_bundle_storage_failure!())?;
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
        .map_err(|_| host_bundle_storage_failure!())
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
        .map_err(|_| host_bundle_storage_failure!())?;
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
        Err(_) => return Err(host_bundle_storage_failure!()),
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
            Err(_) => return Err(host_bundle_storage_failure!()),
        };
        let result = (|| {
            file.write_all(bytes)
                .map_err(|_| host_bundle_storage_failure!())?;
            file.sync_all()
                .map_err(|_| host_bundle_storage_failure!())?;
            drop(file);
            // A rename changes the final directory entry rather than following
            // a final symlink; the preflight and capability parent prevent
            // traversal through any descendant component.
            if replace_existing {
                parent
                    .rename(&temporary, parent, name)
                    .map_err(|_| host_bundle_storage_failure!())?;
            } else {
                parent
                    .hard_link(&temporary, parent, name)
                    .map_err(|error| {
                        if error.kind() == io::ErrorKind::AlreadyExists {
                            HostBundleError::OwnershipConflict
                        } else {
                            host_bundle_storage_failure!()
                        }
                    })?;
                parent
                    .remove_file(&temporary)
                    .map_err(|_| host_bundle_storage_failure!())?;
            }
            sync_cap_dir(parent)
        })();
        if result.is_err() {
            let _ = parent.remove_file(&temporary);
        }
        return result;
    }
    Err(host_bundle_storage_failure!())
}

fn sync_cap_dir(dir: &Dir) -> Result<(), HostBundleError> {
    let mut options = CapOpenOptions::new();
    options.read(true).maybe_dir(true);
    dir.open_with(".", &options)
        .and_then(|file| file.sync_all())
        .map_err(|_| host_bundle_storage_failure!())
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
        || receipt.rollback_history.len() > MAX_MANIFEST_ARTIFACTS
        || (receipt.operation == HostBundleLifecycleOpV1::Uninstall) != receipt.artifacts.is_empty()
    {
        return Err(HostBundleError::ReceiptCorrupted);
    }
    for (index, artifact) in receipt.artifacts.iter().enumerate() {
        validate_relative_install_path(Path::new(&artifact.relative_path))?;
        validate_identifier(&artifact.ownership_marker)?;
        if artifact.artifact_digest == [0; 32]
            || artifact.ownership_marker
                != expected_ownership_marker(receipt.host, receipt.component)
            || receipt.artifacts[..index]
                .iter()
                .any(|existing| existing.relative_path == artifact.relative_path)
        {
            return Err(HostBundleError::ReceiptCorrupted);
        }
    }
    for (index, operation_id) in receipt.rollback_history.iter().enumerate() {
        if *operation_id == [0; 16] || receipt.rollback_history[..index].contains(operation_id) {
            return Err(HostBundleError::ReceiptCorrupted);
        }
    }
    Ok(())
}

fn validate_backup_receipt(receipt: &HostBundleBackupReceiptV1) -> Result<(), HostBundleError> {
    if receipt.schema_version != HOST_BUNDLE_RECEIPT_SCHEMA_VERSION
        || receipt.operation_id == [0; 16]
        || receipt.source_receipt_digest == [0; 32]
        || receipt.host != receipt.manifest.host
        || receipt.component != receipt.manifest.component
        || receipt.artifacts.len() != receipt.manifest.artifacts.len()
    {
        return Err(HostBundleError::ReceiptCorrupted);
    }
    receipt
        .manifest
        .validate_structure()
        .map_err(|_| HostBundleError::ReceiptCorrupted)?;
    for (index, artifact) in receipt.artifacts.iter().enumerate() {
        validate_relative_install_path(Path::new(&artifact.relative_path))?;
        validate_identifier(&artifact.ownership_marker)?;
        if artifact.artifact_digest == [0; 32]
            || !is_safe_component(&artifact.snapshot_name)
            || receipt.artifacts[..index]
                .iter()
                .any(|existing| existing.relative_path == artifact.relative_path)
            || !receipt.manifest.artifacts.iter().any(|expected| {
                expected.relative_path == artifact.relative_path
                    && expected.artifact_digest == artifact.artifact_digest
                    && expected.ownership_marker == artifact.ownership_marker
            })
        {
            return Err(HostBundleError::ReceiptCorrupted);
        }
    }
    Ok(())
}

fn validate_restore_receipt(receipt: &HostBundleRestoreReceiptV1) -> Result<(), HostBundleError> {
    validate_receipt(&receipt.restored_receipt)?;
    if receipt.schema_version != HOST_BUNDLE_RECEIPT_SCHEMA_VERSION
        || receipt.operation_id == [0; 16]
        || receipt.backup_operation_id == [0; 16]
        || receipt.restored_receipt.operation_id != receipt.operation_id
        || receipt.restored_receipt.operation != HostBundleLifecycleOpV1::Repair
        || receipt.restored_receipt.rollback_boundary != HostBundleRollbackBoundaryV1::Passed
    {
        return Err(HostBundleError::ReceiptCorrupted);
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

fn validate_component_set_request(
    component_set: &HostComponentSetV1,
    request: &HostComponentSetExecutionRequestV1,
) -> Result<(), HostBundleError> {
    if request.operation_id == [0; 16]
        || component_set.components.is_empty()
        || component_set.components.len() > MAX_HOST_COMPONENTS
        || component_set.host != request.lifecycle.expected_host
    {
        return Err(HostBundleError::InvalidManifest);
    }
    if !request.lifecycle.explicit_confirmation {
        return Err(HostBundleError::ConfirmationRequired);
    }
    match component_set.host {
        HostKindV1::Hermes if request.lifecycle.hermes_profile_bindings != 1 => {
            return Err(HostBundleError::InvalidHermesProfileBinding);
        }
        HostKindV1::Hermes => {}
        _ if request.lifecycle.hermes_profile_bindings != 0 => {
            return Err(HostBundleError::InvalidHermesProfileBinding);
        }
        _ => {}
    }

    let mut expected = request.lifecycle.expected_components.clone();
    let mut actual = Vec::with_capacity(component_set.components.len());
    let mut claimed_paths = BTreeMap::new();
    for component in &component_set.components {
        component.manifest.validate_structure()?;
        if component.manifest.host != component_set.host {
            return Err(HostBundleError::WrongTarget);
        }
        actual.push(component.manifest.component);
        for artifact in &component.manifest.artifacts {
            if claimed_paths
                .insert(artifact.relative_path.clone(), component.manifest.component)
                .is_some()
            {
                return Err(HostBundleError::InvalidManifest);
            }
        }
    }
    actual.sort_unstable();
    expected.sort_unstable();
    if actual
        .windows(2)
        .any(|components| components[0] == components[1])
        || expected
            .windows(2)
            .any(|components| components[0] == components[1])
        || actual != expected
    {
        return Err(HostBundleError::WrongTarget);
    }
    Ok(())
}

fn component_set_receipt_from_prepared(
    prepared: &[PreparedHostComponentSetComponentV1],
    request: &HostComponentSetExecutionRequestV1,
    confirmed_preview: Option<&HostComponentSetLifecyclePreviewV1>,
) -> Result<HostComponentSetReceiptV1, HostBundleError> {
    // Provenance is preserved only for a *companion*: a component an incremental
    // Update left untouched while it did real work on a sibling. Two gates bound
    // this:
    //
    // * The operation is an Update — the only incremental one. Install
    //   first-deploys every component, Repair re-asserts ownership of the whole
    //   cataloged set, and Uninstall removes it, so each legitimately stamps its
    //   operation onto every receipt, changed or not.
    // * The set performed at least one effective artifact write. A transaction
    //   that writes nothing anywhere is a pure no-op re-run of the identical
    //   set, and still records its operation rather than reusing the prior one.
    let preserves_untouched_companions = request.lifecycle.operation
        == HostBundleLifecycleOpV1::Update
        && prepared.iter().any(|component| {
            component
                .plan
                .mutations
                .iter()
                .any(|mutation| mutation.action != HostArtifactActionV1::Noop)
        });
    let component_receipts = prepared
        .iter()
        .map(|component| {
            // An unchanged companion — one whose plan writes nothing and whose
            // manifest is byte-identical to its durable receipt — keeps its
            // original operation provenance. "Writes nothing" must be read from
            // each mutation's action, not from an empty mutation list: a
            // component with manifest artifacts always plans one Noop mutation
            // per artifact.
            if preserves_untouched_companions
                && component
                    .plan
                    .mutations
                    .iter()
                    .all(|mutation| mutation.action == HostArtifactActionV1::Noop)
                && let Some(previous_receipt) = &component.previous_receipt
                && previous_receipt.manifest_digest == component.manifest.canonical_digest()?
            {
                return Ok(previous_receipt.clone());
            }
            let mut rollback_history = component
                .previous_receipt
                .as_ref()
                .map(|receipt| receipt.rollback_history.clone())
                .unwrap_or_default();
            // A Repair that overwrites a receipt-owned path whose bytes drifted
            // from the catalog backs up genuinely foreign content — a user edit,
            // never tracedecay's own prior output, because Repair replaces a
            // path only when its observed digest differs from the cataloged one,
            // which for an unchanged Repair manifest is also the previously
            // owned digest. Referencing this operation from the receipt keeps the
            // commit boundary from retiring that backup, so an operator can still
            // recover the overwritten bytes. Ordinary Update backups hold
            // tracedecay's own output and stay retired on commit.
            if request.lifecycle.operation == HostBundleLifecycleOpV1::Repair
                && component
                    .plan
                    .mutations
                    .iter()
                    .any(|mutation| mutation.action == HostArtifactActionV1::BackupThenReplace)
                && !rollback_history.contains(&request.operation_id)
            {
                rollback_history.push(request.operation_id);
            }
            Ok(HostBundleInstallReceiptV1 {
                schema_version: HOST_BUNDLE_RECEIPT_SCHEMA_VERSION,
                operation_id: request.operation_id,
                host: component.manifest.host,
                component: component.manifest.component,
                operation: request.lifecycle.operation,
                manifest_digest: component.manifest.canonical_digest()?,
                artifacts: if request.lifecycle.operation == HostBundleLifecycleOpV1::Uninstall {
                    Vec::new()
                } else {
                    component
                        .manifest
                        .artifacts
                        .iter()
                        .map(|artifact| HostBundleReceiptArtifactV1 {
                            relative_path: artifact.relative_path.clone(),
                            artifact_digest: artifact.artifact_digest,
                            ownership_marker: artifact.ownership_marker.clone(),
                        })
                        .collect()
                },
                rollback_boundary: HostBundleRollbackBoundaryV1::Passed,
                rollback_history,
            })
        })
        .collect::<Result<Vec<_>, HostBundleError>>()?;
    let receipt = HostComponentSetReceiptV1 {
        schema_version: HOST_BUNDLE_RECEIPT_SCHEMA_VERSION,
        operation_id: request.operation_id,
        host: request.lifecycle.expected_host,
        operation: request.lifecycle.operation,
        component_manifests: prepared
            .iter()
            .map(|component| component.manifest.clone())
            .collect(),
        component_receipts,
        confirmed_plan_digest: confirmed_preview.map(|preview| preview.plan_digest),
        base_registration_revision: confirmed_preview
            .map(|preview| preview.base_registration_revision),
        current_registration_revision: confirmed_preview
            .map(|preview| preview.current_registration_revision),
        artifact_state_revision: confirmed_preview.map(|preview| preview.artifact_state_revision),
    };
    validate_component_set_receipt(&receipt)?;
    Ok(receipt)
}

fn component_set_receipt_matches(
    receipt: &HostComponentSetReceiptV1,
    component_set: &HostComponentSetV1,
    request: &HostComponentSetExecutionRequestV1,
) -> Result<bool, HostBundleError> {
    validate_component_set_request(component_set, request)?;
    validate_component_set_receipt(receipt)?;
    if receipt.operation_id != request.operation_id
        || receipt.host != component_set.host
        || receipt.operation != request.lifecycle.operation
        || receipt.component_receipts.len() != component_set.components.len()
    {
        return Ok(false);
    }
    for component in &component_set.components {
        let manifest_digest = component.manifest.canonical_digest()?;
        let receipt_matches = receipt.component_receipts.iter().any(|component_receipt| {
            component_receipt.host == component.manifest.host
                && component_receipt.component == component.manifest.component
                && component_receipt.manifest_digest == manifest_digest
                && component_receipt.rollback_boundary == HostBundleRollbackBoundaryV1::Passed
        });
        if !receipt_matches {
            return Ok(false);
        }
    }
    Ok(true)
}

fn component_set_receipt_matches_preview(
    receipt: &HostComponentSetReceiptV1,
    preview: &HostComponentSetLifecyclePreviewV1,
) -> bool {
    receipt.operation_id == preview.operation_id
        && receipt.confirmed_plan_digest == Some(preview.plan_digest)
        && receipt.base_registration_revision == Some(preview.base_registration_revision)
        && receipt.current_registration_revision == Some(preview.current_registration_revision)
        && receipt.artifact_state_revision == Some(preview.artifact_state_revision)
}

fn validate_component_set_receipt(
    receipt: &HostComponentSetReceiptV1,
) -> Result<(), HostBundleError> {
    if receipt.schema_version != HOST_BUNDLE_RECEIPT_SCHEMA_VERSION
        || receipt.operation_id == [0; 16]
        || receipt.component_manifests.is_empty()
        || receipt.component_receipts.is_empty()
        || receipt.component_manifests.len() != receipt.component_receipts.len()
        || receipt.component_receipts.len() > MAX_HOST_COMPONENTS
        || match (
            receipt.confirmed_plan_digest,
            receipt.base_registration_revision,
            receipt.current_registration_revision,
            receipt.artifact_state_revision,
        ) {
            (None, None, None, None) => false,
            (Some(plan), Some(base), Some(current), Some(artifacts)) => {
                plan == [0; 32] || base == [0; 32] || current != base || artifacts == [0; 32]
            }
            _ => true,
        }
    {
        return Err(HostBundleError::ReceiptCorrupted);
    }
    for (index, component_receipt) in receipt.component_receipts.iter().enumerate() {
        validate_receipt(component_receipt)?;
        let manifest = receipt
            .component_manifests
            .iter()
            .find(|manifest| manifest.component == component_receipt.component)
            .ok_or(HostBundleError::ReceiptCorrupted)?;
        manifest.validate_structure()?;
        if component_receipt.host != receipt.host
            || manifest.host != receipt.host
            || manifest.canonical_digest()? != component_receipt.manifest_digest
            || component_receipt.rollback_boundary != HostBundleRollbackBoundaryV1::Passed
            || receipt.component_receipts[..index]
                .iter()
                .any(|previous| previous.component == component_receipt.component)
        {
            return Err(HostBundleError::ReceiptCorrupted);
        }
    }
    Ok(())
}

fn component_set_from_journal(journal: &HostComponentSetJournalV1) -> HostComponentSetV1 {
    HostComponentSetV1 {
        host: journal.host,
        components: journal
            .components
            .iter()
            .map(|component| HostComponentSetEntryV1 {
                manifest: component.manifest.clone(),
                contents: Vec::new(),
            })
            .collect(),
    }
}

fn validate_component_set_journal(
    journal: &HostComponentSetJournalV1,
) -> Result<(), HostBundleError> {
    if journal.schema_version != HOST_BUNDLE_RECEIPT_SCHEMA_VERSION
        || journal.operation_id == [0; 16]
        || journal.components.is_empty()
        || journal.components.len() > MAX_HOST_COMPONENTS
        || !journal.explicit_confirmation
        || matches!(
            journal.host,
            HostKindV1::Hermes if journal.hermes_profile_bindings != 1
        )
        || matches!(
            journal.host,
            host if host != HostKindV1::Hermes && journal.hermes_profile_bindings != 0
        )
    {
        return Err(HostBundleError::ReceiptCorrupted);
    }
    let preview_authority = [
        journal.confirmed_plan_digest,
        journal.base_registration_revision,
        journal.current_registration_revision,
        journal.artifact_state_revision,
    ];
    if preview_authority.iter().any(Option::is_some)
        && preview_authority.iter().any(Option::is_none)
    {
        return Err(HostBundleError::ReceiptCorrupted);
    }
    // The recorded phase and the two registration flags are not independent:
    // the writer raises each flag before the hook it names and advances the
    // phase after that hook returns. A journal claiming a phase its flags
    // cannot support was never written by this lifecycle, so recovery must not
    // act on its registration story at all.
    if !journal.registration_flags_match_state() {
        return Err(HostBundleError::ReceiptCorrupted);
    }
    let mut components = BTreeMap::new();
    let mut paths = BTreeMap::new();
    let mut configuration_authority = None;
    for component in &journal.components {
        component.manifest.validate_structure()?;
        let authority = (
            component.manifest.configuration_snapshot_id.as_str(),
            component.manifest.integration_manifest_digest,
            component.manifest.catalog_digest,
        );
        if let Some(expected) = configuration_authority {
            if authority != expected {
                return Err(HostBundleError::ReceiptCorrupted);
            }
        } else {
            configuration_authority = Some(authority);
        }
        if component.manifest.host != journal.host
            || components
                .insert(component.manifest.component, ())
                .is_some()
            || (component.entries.is_empty()
                && journal.operation != HostBundleLifecycleOpV1::Uninstall)
            || component.entries.len() > MAX_MANIFEST_ARTIFACTS
        {
            return Err(HostBundleError::ReceiptCorrupted);
        }
        if let Some(receipt) = &component.previous_receipt {
            validate_receipt(receipt)?;
            if receipt.host != journal.host || receipt.component != component.manifest.component {
                return Err(HostBundleError::ReceiptCorrupted);
            }
        }
        for (index, entry) in component.entries.iter().enumerate() {
            validate_relative_install_path(Path::new(&entry.relative_path))?;
            if entry
                .backup_name
                .as_deref()
                .is_some_and(|backup| !is_safe_component(backup))
                || component.entries[..index]
                    .iter()
                    .any(|previous| previous.relative_path == entry.relative_path)
                || paths.insert(entry.relative_path.clone(), ()).is_some()
                || (entry.backup_created && entry.backup_name.is_none())
                || (entry.backup_name.is_some() && entry.wrote_new && !entry.backup_created)
                || (entry.wrote_new && entry.installed_digest.is_none())
            {
                return Err(HostBundleError::ReceiptCorrupted);
            }
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

fn host_bundle_snapshot_name(index: usize, relative_path: &str) -> String {
    let digest = Sha256::digest(relative_path.as_bytes());
    format!("{index:03}-{}", hex::encode(&digest[..16]))
}

fn host_bundle_backup_receipt_file(operation_id: [u8; 16]) -> String {
    format!("backup-receipt.{}.v1.json", hex::encode(operation_id))
}

fn host_bundle_restore_receipt_file(operation_id: [u8; 16]) -> String {
    format!("restore-receipt.{}.v1.json", hex::encode(operation_id))
}

fn component_set_stage_name(component: HostBundleComponentV1, relative_path: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(component_slug(component).as_bytes());
    hasher.update(relative_path.as_bytes());
    format!(
        "{}-{}",
        component_slug(component),
        hex::encode(hasher.finalize())
    )
}

fn receipt_file(host: HostKindV1, component: HostBundleComponentV1) -> String {
    format!(
        "receipt.{}.{}.v1.json",
        host.descriptor().slug(),
        component_slug(component)
    )
}

/// Host-scoped component-set journal name.
///
/// Blast-radius argument for per-host isolation: every host deploys its
/// artifacts under its own disjoint subtree of the artifact root
/// (`.claude/…`, `.codex/…`, `.cursor/…`, `.config/opencode/…`,
/// `.kimi-code/…`, `.hermes/…`, `.kiro/…`, `.cline/…`, `.roo/…`,
/// `.config/kilo/…`), and backups plus staging directories are keyed by
/// `operation_id`. A pending transaction for host X therefore shares no
/// mutable path with a transaction for host Y, so X awaiting recovery is not a
/// reason to refuse Y. `first_party_host_artifact_prefixes_are_disjoint`
/// pins that premise as a test, so a future host that violates it fails the
/// suite rather than silently widening the blast radius. The receipt namespace
/// is already host-scoped (`receipt_file`), and the single writer lock still
/// serializes all mutation within a lifecycle root.
fn component_set_journal_file(host: HostKindV1) -> String {
    format!("component-set-journal.{}.v1.json", host.descriptor().slug())
}

fn component_set_receipt_file(operation_id: [u8; 16]) -> String {
    format!(
        "component-set-receipt.{}.v1.json",
        hex::encode(operation_id)
    )
}

fn receipt_identity_from_file_name(file_name: &str) -> Option<(HostKindV1, HostBundleComponentV1)> {
    let components = [
        HostBundleComponentV1::Core,
        HostBundleComponentV1::Agent,
        HostBundleComponentV1::ContextMcp,
        HostBundleComponentV1::OperatorMcp,
    ];
    stock_host_kinds().into_iter().find_map(|host| {
        components
            .iter()
            .copied()
            .find(|component| receipt_file(host, *component) == file_name)
            .map(|component| (host, component))
    })
}

fn expected_ownership_marker(host: HostKindV1, component: HostBundleComponentV1) -> String {
    format!(
        "tracedecay.{}.{}.v1",
        host.descriptor().slug(),
        component_slug(component)
    )
}

fn component_slug(component: HostBundleComponentV1) -> &'static str {
    match component {
        HostBundleComponentV1::Core => "core",
        HostBundleComponentV1::Agent => "agent",
        HostBundleComponentV1::ContextMcp => "context-mcp",
        HostBundleComponentV1::OperatorMcp => "operator-mcp",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_integration_contracts_are_owned_by_the_leaf_crate() {
        assert_eq!(
            std::any::type_name::<HostBundleManifestV1>(),
            "tracedecay_host_integration::HostBundleManifestV1"
        );
        assert_eq!(
            std::any::type_name::<HostBundleInstallReceiptV1>(),
            "tracedecay_host_integration::HostBundleInstallReceiptV1"
        );
        assert_eq!(
            std::any::type_name::<HostBundleJournalV1>(),
            "tracedecay_host_integration::HostBundleJournalV1"
        );
        assert_eq!(
            std::any::type_name::<HostNativeFixtureEvidenceV1>(),
            "tracedecay_host_integration::HostNativeFixtureEvidenceV1"
        );
    }

    #[test]
    fn kimi_repair_actions_name_the_interactive_plugins_flow() {
        for state in [
            HostBundleComponentDoctorStateV1::Repairable,
            HostBundleComponentDoctorStateV1::Missing,
            HostBundleComponentDoctorStateV1::Corrupt,
            HostBundleComponentDoctorStateV1::OwnershipConflict,
        ] {
            let action = repair_action(
                HostKindV1::KimiCode,
                HostBundleComponentV1::Core,
                state,
                HostBundleRegistrationStateV1::Repairable,
            );
            assert!(
                action.contains("/plugins install ~/.tracedecay/host-bundle-stage/kimi/tracedecay"),
                "Kimi remediation must name the interactive host command: {action}"
            );
            assert!(
                !action.contains("reinstall --component"),
                "Kimi remediation must not advertise an unsupported repair command: {action}"
            );
        }

        let corrupt = corrupt_component_result(
            PathBuf::from("/tmp/kimi-corrupt-receipt.json"),
            Some(HostKindV1::KimiCode),
            Some(HostBundleComponentV1::Core),
        );
        assert!(
            corrupt
                .repair_action
                .contains("/plugins install ~/.tracedecay/host-bundle-stage/kimi/tracedecay")
        );
    }

    #[derive(Clone)]
    struct FirstPartyVerifier([u8; 32]);

    impl HostBundleVerificationAdapterV1 for FirstPartyVerifier {
        fn verify_manifest(&self, manifest: &HostBundleManifestV1) -> Result<(), HostBundleError> {
            manifest.validate_structure()?;
            if manifest.canonical_digest()? == self.0 {
                Ok(())
            } else {
                Err(HostBundleError::CatalogMismatch)
            }
        }
    }

    fn manifest(host: HostKindV1, bytes: &[u8]) -> HostBundleManifestV1 {
        let identity: [u8; 32] = Sha256::digest(b"first-party.catalog.v1").into();
        HostBundleManifestV1 {
            schema_version: HOST_BUNDLE_SCHEMA_VERSION,
            host,
            component: HostBundleComponentV1::Core,
            integration_manifest_digest: identity,
            catalog_digest: identity,
            configuration_snapshot_id: "first-party.v1".to_string(),
            effective_behavior_digest: identity,
            resolution_provenance_digest: identity,
            protocol_min: 1,
            protocol_max: 1,
            artifacts: vec![HostBundleArtifactV1 {
                relative_path: "plugins/tracedecay.json".to_string(),
                artifact_digest: Sha256::digest(bytes).into(),
                ownership_marker: expected_ownership_marker(host, HostBundleComponentV1::Core),
            }],
        }
    }

    fn verifier(manifest: &HostBundleManifestV1) -> FirstPartyVerifier {
        FirstPartyVerifier(manifest.canonical_digest().unwrap())
    }

    fn execution(
        host: HostKindV1,
        operation: HostBundleLifecycleOpV1,
        operation_id: u8,
        confirmed: bool,
    ) -> HostBundleExecutionRequestV1 {
        HostBundleExecutionRequestV1 {
            lifecycle: HostBundleLifecycleRequestV1 {
                operation,
                expected_host: host,
                expected_component: HostBundleComponentV1::Core,
                explicit_confirmation: confirmed,
                hermes_profile_bindings: u8::from(host == HostKindV1::Hermes),
            },
            operation_id: [operation_id; 16],
        }
    }

    fn content(bytes: &[u8]) -> Vec<HostBundleArtifactContentV1> {
        vec![HostBundleArtifactContentV1 {
            relative_path: "plugins/tracedecay.json".to_string(),
            bytes: bytes.to_vec(),
        }]
    }

    fn component_manifest(
        host: HostKindV1,
        component: HostBundleComponentV1,
        relative_path: &str,
        bytes: &[u8],
    ) -> HostBundleManifestV1 {
        let identity: [u8; 32] = Sha256::digest(b"first-party.catalog.v1").into();
        HostBundleManifestV1 {
            schema_version: HOST_BUNDLE_SCHEMA_VERSION,
            host,
            component,
            integration_manifest_digest: identity,
            catalog_digest: identity,
            configuration_snapshot_id: "first-party.v1".to_string(),
            effective_behavior_digest: identity,
            resolution_provenance_digest: identity,
            protocol_min: 1,
            protocol_max: 1,
            artifacts: vec![HostBundleArtifactV1 {
                relative_path: relative_path.to_string(),
                artifact_digest: Sha256::digest(bytes).into(),
                ownership_marker: expected_ownership_marker(host, component),
            }],
        }
    }

    fn component_entry(manifest: HostBundleManifestV1, bytes: &[u8]) -> HostComponentSetEntryV1 {
        HostComponentSetEntryV1 {
            contents: vec![HostBundleArtifactContentV1 {
                relative_path: manifest.artifacts[0].relative_path.clone(),
                bytes: bytes.to_vec(),
            }],
            manifest,
        }
    }

    fn component_set(
        host: HostKindV1,
        core_bytes: &[u8],
        agent_bytes: &[u8],
    ) -> HostComponentSetV1 {
        HostComponentSetV1 {
            host,
            components: vec![
                component_entry(
                    component_manifest(
                        host,
                        HostBundleComponentV1::Core,
                        "plugins/core.json",
                        core_bytes,
                    ),
                    core_bytes,
                ),
                component_entry(
                    component_manifest(
                        host,
                        HostBundleComponentV1::Agent,
                        "plugins/agent.json",
                        agent_bytes,
                    ),
                    agent_bytes,
                ),
            ],
        }
    }

    fn component_set_request(
        host: HostKindV1,
        operation: HostBundleLifecycleOpV1,
        operation_id: u8,
    ) -> HostComponentSetExecutionRequestV1 {
        HostComponentSetExecutionRequestV1 {
            lifecycle: HostComponentSetLifecycleRequestV1 {
                operation,
                expected_host: host,
                expected_components: vec![
                    HostBundleComponentV1::Core,
                    HostBundleComponentV1::Agent,
                ],
                explicit_confirmation: true,
                hermes_profile_bindings: u8::from(host == HostKindV1::Hermes),
            },
            operation_id: [operation_id; 16],
        }
    }

    #[derive(Clone)]
    struct ComponentSetVerifier(Vec<[u8; 32]>);

    impl ComponentSetVerifier {
        fn from_set(component_set: &HostComponentSetV1) -> Self {
            Self(
                component_set
                    .components
                    .iter()
                    .map(|component| component.manifest.canonical_digest().unwrap())
                    .collect(),
            )
        }
    }

    impl HostBundleVerificationAdapterV1 for ComponentSetVerifier {
        fn verify_manifest(&self, manifest: &HostBundleManifestV1) -> Result<(), HostBundleError> {
            manifest.validate_structure()?;
            self.0
                .contains(&manifest.canonical_digest()?)
                .then_some(())
                .ok_or(HostBundleError::CatalogMismatch)
        }
    }

    /// The exact failure [`FailingSetRegistration::verify`] injects. Named so
    /// assertions can compare the whole error value: `StorageFailure` carries
    /// the source site that raised it, so a second `host_bundle_storage_failure!()`
    /// written at the assertion would never equal the one raised in the fake.
    const FAILING_SET_REGISTRATION_VERIFY: HostBundleError =
        HostBundleError::StorageFailure("test:FailingSetRegistration::verify");

    #[derive(Default)]
    struct FailingSetRegistration {
        applied: bool,
        rolled_back: bool,
    }

    struct ArtifactOnlyTestRegistration;

    impl HostComponentSetRegistrationV1 for ArtifactOnlyTestRegistration {}

    struct RevisionedTestRegistration {
        revision: [u8; 32],
    }

    impl HostComponentSetRegistrationV1 for RevisionedTestRegistration {
        fn current_revision(
            &self,
            _component_set: &HostComponentSetV1,
            _request: &HostComponentSetExecutionRequestV1,
        ) -> Result<[u8; 32], HostBundleError> {
            Ok(self.revision)
        }
    }

    impl HostComponentSetRegistrationV1 for FailingSetRegistration {
        fn apply(
            &mut self,
            _component_set: &HostComponentSetV1,
            _request: &HostComponentSetExecutionRequestV1,
        ) -> Result<(), HostBundleError> {
            self.applied = true;
            Ok(())
        }

        fn verify(
            &mut self,
            _component_set: &HostComponentSetV1,
            _request: &HostComponentSetExecutionRequestV1,
        ) -> Result<(), HostBundleError> {
            Err(FAILING_SET_REGISTRATION_VERIFY)
        }

        fn rollback(
            &mut self,
            _component_set: &HostComponentSetV1,
            _request: &HostComponentSetExecutionRequestV1,
        ) -> Result<(), HostBundleError> {
            self.rolled_back = true;
            Ok(())
        }
    }

    #[test]
    fn component_backup_restore_is_durable_idempotent_and_rollback_safe() {
        let root = tempfile::tempdir().unwrap();
        let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
        let original = manifest(HostKindV1::Codex, b"original");
        let original_verifier = verifier(&original);
        writer
            .execute(
                &original,
                &execution(
                    HostKindV1::Codex,
                    HostBundleLifecycleOpV1::Install,
                    71,
                    true,
                ),
                &content(b"original"),
                &original_verifier,
            )
            .unwrap();
        assert_eq!(
            writer.backup_component(&original, [72; 16], false, &original_verifier),
            Err(HostBundleError::ConfirmationRequired)
        );

        let backup = writer
            .backup_component(&original, [72; 16], true, &original_verifier)
            .unwrap();
        assert_eq!(
            writer
                .backup_component(&original, [72; 16], true, &original_verifier)
                .unwrap(),
            backup
        );

        let updated = manifest(HostKindV1::Codex, b"updated");
        writer
            .execute(
                &updated,
                &execution(HostKindV1::Codex, HostBundleLifecycleOpV1::Update, 73, true),
                &content(b"updated"),
                &verifier(&updated),
            )
            .unwrap();
        assert_eq!(
            std::fs::read(root.path().join("plugins/tracedecay.json")).unwrap(),
            b"updated"
        );
        assert_eq!(
            writer.restore_component_backup([72; 16], [74; 16], false, &original_verifier),
            Err(HostBundleError::ConfirmationRequired)
        );

        let restored = writer
            .restore_component_backup([72; 16], [74; 16], true, &original_verifier)
            .unwrap();
        assert_eq!(
            writer
                .restore_component_backup([72; 16], [74; 16], true, &original_verifier)
                .unwrap(),
            restored
        );
        assert_eq!(
            std::fs::read(root.path().join("plugins/tracedecay.json")).unwrap(),
            b"original"
        );
        assert_eq!(
            restored.restored_receipt.rollback_boundary,
            HostBundleRollbackBoundaryV1::Passed
        );
        assert!(
            root.path()
                .join(HOST_BUNDLE_CONTROL_DIR)
                .join(host_bundle_backup_receipt_file([72; 16]))
                .is_file()
        );
        assert!(
            root.path()
                .join(HOST_BUNDLE_CONTROL_DIR)
                .join(host_bundle_restore_receipt_file([74; 16]))
                .is_file()
        );
    }

    #[test]
    fn component_set_transaction_is_idempotent_and_rolls_back_every_component() {
        let root = tempfile::tempdir().unwrap();
        let initial = component_set(HostKindV1::OpenCode, b"core-v1", b"agent-v1");
        let initial_request =
            component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Install, 21);
        let initial_verifier = ComponentSetVerifier::from_set(&initial);
        let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
        let mut registration = ArtifactOnlyTestRegistration;
        let first = HostComponentSetTransactionV1::new(&mut writer)
            .execute(
                &initial,
                &initial_request,
                &initial_verifier,
                &mut registration,
            )
            .unwrap();
        let repeated = HostComponentSetTransactionV1::new(&mut writer)
            .execute(
                &initial,
                &initial_request,
                &initial_verifier,
                &mut registration,
            )
            .unwrap();
        assert_eq!(repeated, first);

        let updated = component_set(HostKindV1::OpenCode, b"core-v2", b"agent-v2");
        let update_request =
            component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Update, 22);
        let updated_verifier = ComponentSetVerifier::from_set(&updated);
        let mut failing_registration = FailingSetRegistration::default();
        let preview = HostComponentSetTransactionV1::new(&mut writer)
            .preview(
                &updated,
                &update_request,
                &updated_verifier,
                &mut failing_registration,
            )
            .unwrap();
        assert_eq!(
            HostComponentSetTransactionV1::new(&mut writer).execute_confirmed(
                &updated,
                &update_request,
                &preview,
                &updated_verifier,
                &mut failing_registration,
            ),
            Err(FAILING_SET_REGISTRATION_VERIFY)
        );
        assert!(failing_registration.applied);
        assert!(failing_registration.rolled_back);
        assert_eq!(
            std::fs::read(root.path().join("plugins/core.json")).unwrap(),
            b"core-v1"
        );
        assert_eq!(
            std::fs::read(root.path().join("plugins/agent.json")).unwrap(),
            b"agent-v1"
        );
        assert_eq!(
            writer
                .load_receipt(HostKindV1::OpenCode, HostBundleComponentV1::Core)
                .unwrap()
                .expect("previous core receipt remains published")
                .operation_id,
            [21; 16]
        );
        assert_eq!(
            writer
                .load_receipt(HostKindV1::OpenCode, HostBundleComponentV1::Agent)
                .unwrap()
                .expect("previous agent receipt remains published")
                .operation_id,
            [21; 16]
        );
        assert!(
            root.path()
                .join(HOST_BUNDLE_CONTROL_DIR)
                .join(component_set_journal_file(HostKindV1::OpenCode))
                .is_file(),
            "a rollback journal must remain available for restart reconciliation"
        );
        let journal = writer
            .load_component_set_journal_for(HostKindV1::OpenCode)
            .unwrap()
            .expect("interrupted rollback retains exact lifecycle authority");
        assert!(journal.explicit_confirmation);
        assert_eq!(journal.hermes_profile_bindings, 0);
        assert_eq!(journal.confirmed_plan_digest, Some(preview.plan_digest));
        assert_eq!(
            journal.base_registration_revision,
            Some(preview.base_registration_revision)
        );
        assert_eq!(
            journal.current_registration_revision,
            Some(preview.current_registration_revision)
        );
        assert_eq!(
            journal.artifact_state_revision,
            Some(preview.artifact_state_revision)
        );
        let doctor = inspect_installed_host_bundle_components_at(
            root.path(),
            root.path(),
            &CurrentRegistration,
        )
        .unwrap();
        assert!(
            doctor.components.iter().any(|component| {
                component.host == Some(HostKindV1::OpenCode)
                    && component.component == Some(HostBundleComponentV1::Core)
                    && component.state == HostBundleComponentDoctorStateV1::Repairable
            }),
            "Doctor keeps the component receipt API while surfacing the aggregate recovery boundary"
        );
        // Explicit recover clears the completed rollback journal. Re-open then
        // proves a restarted writer can take the lock once recovery finished.
        HostComponentSetTransactionV1::new(&mut writer)
            .recover(&mut failing_registration)
            .unwrap();
        assert!(
            !root
                .path()
                .join(HOST_BUNDLE_CONTROL_DIR)
                .join(component_set_journal_file(HostKindV1::OpenCode))
                .exists(),
            "restart recovery clears only a completed rollback boundary"
        );
        drop(writer);
        HostBundleWriterV1::open(root.path()).expect("reopen after recovery");
    }

    /// Drive the two-component `OpenCode` set to a durable `RolledBack`
    /// journal: install, then fail the update at registration verification so
    /// the completed rollback boundary is left behind for restart recovery.
    fn wedged_rolled_back_component_set_journal(root: &Path) {
        let initial = component_set(HostKindV1::OpenCode, b"core-v1", b"agent-v1");
        let initial_request =
            component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Install, 81);
        let initial_verifier = ComponentSetVerifier::from_set(&initial);
        let mut writer = HostBundleWriterV1::open(root).unwrap();
        HostComponentSetTransactionV1::new(&mut writer)
            .execute(
                &initial,
                &initial_request,
                &initial_verifier,
                &mut ArtifactOnlyTestRegistration,
            )
            .unwrap();

        let updated = component_set(HostKindV1::OpenCode, b"core-v2", b"agent-v2");
        let update_request =
            component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Update, 82);
        let updated_verifier = ComponentSetVerifier::from_set(&updated);
        let mut failing = FailingSetRegistration::default();
        assert_eq!(
            HostComponentSetTransactionV1::new(&mut writer).execute(
                &updated,
                &update_request,
                &updated_verifier,
                &mut failing,
            ),
            Err(FAILING_SET_REGISTRATION_VERIFY)
        );
        assert_eq!(
            writer
                .load_component_set_journal_for(HostKindV1::OpenCode)
                .unwrap()
                .expect("a completed rollback journal remains")
                .state,
            HostComponentSetJournalStateV1::RolledBack
        );
        drop(writer);
    }

    /// Rewrite the pending `OpenCode` journal on disk under `file_name`, as a
    /// differently shaped binary or a corrupted control file would leave it.
    fn reshape_component_set_journal(
        root: &Path,
        file_name: &str,
        reshape: impl FnOnce(&mut HostComponentSetJournalV1),
    ) {
        let control = root.join(HOST_BUNDLE_CONTROL_DIR);
        let host_scoped = control.join(component_set_journal_file(HostKindV1::OpenCode));
        let mut journal: HostComponentSetJournalV1 =
            serde_json::from_slice(&fs::read(&host_scoped).unwrap()).unwrap();
        reshape(&mut journal);
        if file_name != component_set_journal_file(HostKindV1::OpenCode) {
            fs::remove_file(&host_scoped).unwrap();
        }
        fs::write(
            control.join(file_name),
            serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
    }

    /// Defect: recovery read `registration_staged`/`registration_applied` as
    /// proof that a rolled-back journal owed no host-native compensation. Those
    /// flags describe the interrupted attempt, not the outstanding work, so a
    /// `RolledBack` journal with both flags clear silently skipped
    /// `registration.rollback` and left the native host configuration mutated.
    #[test]
    fn a_rolled_back_component_set_journal_compensates_registration_without_its_flags() {
        let root = tempfile::tempdir().unwrap();
        wedged_rolled_back_component_set_journal(root.path());
        reshape_component_set_journal(
            root.path(),
            &component_set_journal_file(HostKindV1::OpenCode),
            |journal| {
                journal.registration_staged = false;
                journal.registration_applied = false;
            },
        );

        let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
        let mut registration = FailingSetRegistration::default();
        HostComponentSetTransactionV1::new(&mut writer)
            .recover_host(HostKindV1::OpenCode, &mut registration)
            .unwrap();
        assert!(
            registration.rolled_back,
            "a rolled-back journal must always re-attempt registration compensation"
        );
        assert!(
            !root
                .path()
                .join(HOST_BUNDLE_CONTROL_DIR)
                .join(component_set_journal_file(HostKindV1::OpenCode))
                .exists(),
            "recovery still clears the completed rollback boundary"
        );
    }

    /// The recorded phase and the registration flags are not independent, so a
    /// journal claiming a phase its flags cannot support was never written by
    /// this lifecycle. It must be refused at load rather than recovered from.
    #[test]
    fn an_unrepresentable_component_set_journal_phase_and_flag_pair_is_rejected() {
        use HostComponentSetJournalStateV1 as State;

        let root = tempfile::tempdir().unwrap();
        wedged_rolled_back_component_set_journal(root.path());
        reshape_component_set_journal(
            root.path(),
            &component_set_journal_file(HostKindV1::OpenCode),
            |journal| {
                journal.state = HostComponentSetJournalStateV1::Applied;
                journal.registration_staged = false;
                journal.registration_applied = false;
            },
        );

        let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
        let mut registration = FailingSetRegistration::default();
        assert_eq!(
            HostComponentSetTransactionV1::new(&mut writer)
                .recover_host(HostKindV1::OpenCode, &mut registration),
            Err(HostBundleError::ReceiptCorrupted)
        );
        assert_eq!(
            writer.pending_component_set_journal_operation(HostKindV1::OpenCode),
            Err(HostBundleError::ReceiptCorrupted)
        );
        assert!(
            !registration.rolled_back,
            "a journal no writer could produce never drives host-native compensation"
        );

        let journal: HostComponentSetJournalV1 = serde_json::from_slice(
            &fs::read(
                root.path()
                    .join(HOST_BUNDLE_CONTROL_DIR)
                    .join(component_set_journal_file(HostKindV1::OpenCode)),
            )
            .unwrap(),
        )
        .unwrap();
        for (state, staged, applied, representable) in [
            (State::Prepared, false, false, true),
            (State::Prepared, true, false, true),
            (State::Prepared, false, true, false),
            (State::Staged, true, false, true),
            (State::Staged, false, false, false),
            (State::Applied, true, true, true),
            (State::Applied, true, false, false),
            (State::Verified, false, true, false),
            (State::Committed, true, true, true),
            // Rollback preserves whichever flags the failed attempt reached, so
            // every combination is authentic in this state.
            (State::RolledBack, false, false, true),
            (State::RolledBack, true, false, true),
            (State::RolledBack, true, true, true),
        ] {
            let candidate = HostComponentSetJournalV1 {
                state,
                registration_staged: staged,
                registration_applied: applied,
                ..journal.clone()
            };
            assert_eq!(
                candidate.registration_flags_match_state(),
                representable,
                "{state:?} staged={staged} applied={applied}"
            );
            assert_eq!(
                validate_component_set_journal(&candidate).is_ok(),
                representable,
                "{state:?} staged={staged} applied={applied}"
            );
        }
    }

    /// A journal written by an older binary lives under the shared legacy name
    /// and may carry any flag combination its rollback happened to reach. It
    /// must still load, and its compensation must still run.
    #[test]
    fn a_legacy_named_rolled_back_component_set_journal_still_recovers() {
        let root = tempfile::tempdir().unwrap();
        wedged_rolled_back_component_set_journal(root.path());
        reshape_component_set_journal(root.path(), HOST_COMPONENT_SET_JOURNAL_FILE, |journal| {
            journal.registration_staged = false;
            journal.registration_applied = false;
        });

        let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
        assert_eq!(
            writer.pending_component_set_journal_hosts().unwrap(),
            vec![HostKindV1::OpenCode],
            "a legacy-named journal is still discovered"
        );
        let mut registration = FailingSetRegistration::default();
        HostComponentSetTransactionV1::new(&mut writer)
            .recover(&mut registration)
            .unwrap();
        assert!(
            registration.rolled_back,
            "a legacy rolled-back journal still owes registration compensation"
        );
        assert!(
            !root
                .path()
                .join(HOST_BUNDLE_CONTROL_DIR)
                .join(HOST_COMPONENT_SET_JOURNAL_FILE)
                .exists(),
            "recovery retires the legacy boundary it just resolved"
        );
    }

    /// Stand-in for the compatibility registration adapter, which re-runs a
    /// legacy installer over the very paths the transaction just wrote. It
    /// rewrites one deployed path during `apply`, so artifact verification
    /// fails afterwards and rollback has to cope with a second writer's bytes.
    struct SecondWriterRegistration {
        artifact_root: PathBuf,
        relative_path: &'static str,
        bytes: Vec<u8>,
        rolled_back: bool,
    }

    impl HostComponentSetRegistrationV1 for SecondWriterRegistration {
        fn apply(
            &mut self,
            _component_set: &HostComponentSetV1,
            _request: &HostComponentSetExecutionRequestV1,
        ) -> Result<(), HostBundleError> {
            fs::write(self.artifact_root.join(self.relative_path), &self.bytes)
                .map_err(|_| host_bundle_storage_failure!())
        }

        fn rollback(
            &mut self,
            _component_set: &HostComponentSetV1,
            _request: &HostComponentSetExecutionRequestV1,
        ) -> Result<(), HostBundleError> {
            self.rolled_back = true;
            Ok(())
        }
    }

    /// Install the two-component `OpenCode` set, then attempt a repair whose
    /// registration adapter rewrites `plugins/core.json` with `second_bytes`.
    /// Returns the repair outcome plus the writer for further assertions.
    fn wedge_repair_with_second_writer(
        root: &Path,
        second_bytes: &[u8],
    ) -> (
        HostBundleWriterV1,
        Result<HostComponentSetReceiptV1, HostBundleError>,
    ) {
        let initial = component_set(HostKindV1::OpenCode, b"core-v1", b"agent-v1");
        let initial_request =
            component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Install, 41);
        let initial_verifier = ComponentSetVerifier::from_set(&initial);
        let mut writer = HostBundleWriterV1::open(root).unwrap();
        HostComponentSetTransactionV1::new(&mut writer)
            .execute(
                &initial,
                &initial_request,
                &initial_verifier,
                &mut ArtifactOnlyTestRegistration,
            )
            .unwrap();

        let repair = component_set(HostKindV1::OpenCode, b"core-v2", b"agent-v2");
        let repair_request =
            component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Repair, 42);
        let repair_verifier = ComponentSetVerifier::from_set(&repair);
        let mut registration = SecondWriterRegistration {
            artifact_root: root.to_path_buf(),
            relative_path: "plugins/core.json",
            bytes: second_bytes.to_vec(),
            rolled_back: false,
        };
        let outcome = HostComponentSetTransactionV1::new(&mut writer).execute(
            &repair,
            &repair_request,
            &repair_verifier,
            &mut registration,
        );
        assert!(registration.rolled_back, "registration rollback must run");
        (writer, outcome)
    }

    /// Defect: a second writer that left the deployed path holding the exact
    /// pre-transaction bytes used to make rollback unconvergeable forever —
    /// `remove_if_digest_matches` refused to touch a file that no longer
    /// matched the installed digest, so the journal stayed behind and wedged
    /// every later host transaction.
    #[test]
    fn component_set_rollback_converges_when_a_second_writer_left_the_backup_bytes() {
        let root = tempfile::tempdir().unwrap();
        let (mut writer, outcome) = wedge_repair_with_second_writer(root.path(), b"core-v1");

        assert_eq!(
            outcome.err(),
            Some(HostBundleError::ArtifactContentMismatch),
            "the failure must surface as the real content mismatch, not RecoveryRequired"
        );
        assert_eq!(
            fs::read(root.path().join("plugins/core.json")).unwrap(),
            b"core-v1",
            "the pre-transaction bytes are the converged end state"
        );
        assert_eq!(
            fs::read(root.path().join("plugins/agent.json")).unwrap(),
            b"agent-v1"
        );
        assert_eq!(
            writer
                .load_receipt(HostKindV1::OpenCode, HostBundleComponentV1::Core)
                .unwrap()
                .expect("the pre-transaction receipt is restored")
                .operation_id,
            [41; 16]
        );

        // A completed rollback leaves the journal for an explicit restart
        // boundary; recovery clears it and the host is usable again.
        HostComponentSetTransactionV1::new(&mut writer)
            .recover_host(HostKindV1::OpenCode, &mut ArtifactOnlyTestRegistration)
            .unwrap();
        assert!(
            writer
                .pending_component_set_journal_hosts()
                .unwrap()
                .is_empty()
        );
        let next = component_set(HostKindV1::OpenCode, b"core-v3", b"agent-v3");
        let next_request =
            component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Repair, 43);
        let next_verifier = ComponentSetVerifier::from_set(&next);
        HostComponentSetTransactionV1::new(&mut writer)
            .execute(
                &next,
                &next_request,
                &next_verifier,
                &mut ArtifactOnlyTestRegistration,
            )
            .expect("the host is no longer wedged");
    }

    /// Genuinely foreign bytes stay fail-closed: converging would silently
    /// destroy content this transaction can not account for. The operator
    /// resolves it with the explicit recovery verb instead.
    #[test]
    fn component_set_rollback_stays_fail_closed_for_foreign_bytes() {
        let root = tempfile::tempdir().unwrap();
        let (mut writer, outcome) =
            wedge_repair_with_second_writer(root.path(), b"foreign-third-party-bytes");

        assert_eq!(outcome.err(), Some(HostBundleError::RecoveryRequired));
        assert_eq!(
            fs::read(root.path().join("plugins/core.json")).unwrap(),
            b"foreign-third-party-bytes",
            "unaccountable content is preserved, never silently discarded"
        );
        assert_eq!(
            writer.pending_component_set_journal_hosts().unwrap(),
            vec![HostKindV1::OpenCode]
        );
        // Convergent recovery cannot resolve this, so it still fails closed.
        assert_eq!(
            HostComponentSetTransactionV1::new(&mut writer)
                .recover_host(HostKindV1::OpenCode, &mut ArtifactOnlyTestRegistration)
                .err(),
            Some(HostBundleError::RecoveryRequired)
        );

        // The recovery verb's escape hatch: the journal is set aside (not
        // deleted) and the immutable backups stay on disk.
        let quarantined = writer
            .quarantine_component_set_journal(HostKindV1::OpenCode, 1_700_000_000)
            .unwrap()
            .expect("the pending journal is quarantined");
        assert!(quarantined.is_file());
        assert!(
            writer
                .pending_component_set_journal_hosts()
                .unwrap()
                .is_empty()
        );
        assert!(
            root.path()
                .join(HOST_BUNDLE_CONTROL_DIR)
                .join("backups")
                .exists(),
            "quarantine preserves the rollback backups"
        );
        let next = component_set(HostKindV1::OpenCode, b"core-v4", b"agent-v4");
        let next_request =
            component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Repair, 44);
        let next_verifier = ComponentSetVerifier::from_set(&next);
        HostComponentSetTransactionV1::new(&mut writer)
            .execute(
                &next,
                &next_request,
                &next_verifier,
                &mut ArtifactOnlyTestRegistration,
            )
            .expect("the recovery verb unblocks the host without hand-deleting a journal");
    }

    fn host_scoped_component_set(host: HostKindV1, slug: &str, tag: &[u8]) -> HostComponentSetV1 {
        let core_path = format!("{slug}/core.json");
        let agent_path = format!("{slug}/agent.json");
        HostComponentSetV1 {
            host,
            components: vec![
                component_entry(
                    component_manifest(host, HostBundleComponentV1::Core, &core_path, tag),
                    tag,
                ),
                component_entry(
                    component_manifest(host, HostBundleComponentV1::Agent, &agent_path, tag),
                    tag,
                ),
            ],
        }
    }

    /// Defect: one shared journal per lifecycle root meant a wedged opencode
    /// repair blocked codex, cursor, cline, roo-code, kilo, kiro, and kimi in
    /// the same `tracedecay reinstall`. Journals are host-scoped now, and the
    /// hosts' artifact path spaces are disjoint, so an unrelated host proceeds.
    #[test]
    fn a_wedged_host_journal_does_not_block_an_unrelated_host() {
        let root = tempfile::tempdir().unwrap();
        let wedged = host_scoped_component_set(HostKindV1::OpenCode, "opencode", b"v1");
        let wedged_request =
            component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Install, 51);
        let wedged_verifier = ComponentSetVerifier::from_set(&wedged);
        let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
        let mut second_writer = SecondWriterRegistration {
            artifact_root: root.path().to_path_buf(),
            relative_path: "opencode/core.json",
            bytes: b"foreign".to_vec(),
            rolled_back: false,
        };
        assert_eq!(
            HostComponentSetTransactionV1::new(&mut writer)
                .execute(
                    &wedged,
                    &wedged_request,
                    &wedged_verifier,
                    &mut second_writer,
                )
                .err(),
            Some(HostBundleError::RecoveryRequired)
        );
        assert_eq!(
            writer.pending_component_set_journal_hosts().unwrap(),
            vec![HostKindV1::OpenCode]
        );

        let unrelated = host_scoped_component_set(HostKindV1::Codex, "codex", b"v1");
        let unrelated_request =
            component_set_request(HostKindV1::Codex, HostBundleLifecycleOpV1::Install, 52);
        let unrelated_verifier = ComponentSetVerifier::from_set(&unrelated);
        HostComponentSetTransactionV1::new(&mut writer)
            .execute(
                &unrelated,
                &unrelated_request,
                &unrelated_verifier,
                &mut ArtifactOnlyTestRegistration,
            )
            .expect("an unrelated host's disjoint path space is not blocked");
        assert_eq!(
            fs::read(root.path().join("codex/core.json")).unwrap(),
            b"v1"
        );
        assert_eq!(
            writer.pending_component_set_journal_hosts().unwrap(),
            vec![HostKindV1::OpenCode],
            "the wedged host still awaits its own recovery"
        );

        // Recovering one host must not touch another host's journal.
        let mut also_wedged = SecondWriterRegistration {
            artifact_root: root.path().to_path_buf(),
            relative_path: "codex/core.json",
            bytes: b"foreign".to_vec(),
            rolled_back: false,
        };
        let repair = host_scoped_component_set(HostKindV1::Codex, "codex", b"v2");
        let repair_request =
            component_set_request(HostKindV1::Codex, HostBundleLifecycleOpV1::Repair, 53);
        let repair_verifier = ComponentSetVerifier::from_set(&repair);
        assert_eq!(
            HostComponentSetTransactionV1::new(&mut writer)
                .execute(&repair, &repair_request, &repair_verifier, &mut also_wedged)
                .err(),
            Some(HostBundleError::RecoveryRequired)
        );
        writer
            .quarantine_component_set_journal(HostKindV1::Codex, 1_700_000_000)
            .unwrap()
            .expect("codex journal quarantined");
        assert_eq!(
            writer.pending_component_set_journal_hosts().unwrap(),
            vec![HostKindV1::OpenCode],
            "quarantining one host leaves every other host's journal intact"
        );
    }

    #[test]
    fn unchanged_companion_receipt_keeps_original_operation_provenance() {
        let root = tempfile::tempdir().unwrap();
        let initial = component_set(HostKindV1::OpenCode, b"core-v1", b"agent-v1");
        let initial_request =
            component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Install, 81);
        let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
        let mut registration = ArtifactOnlyTestRegistration;
        HostComponentSetTransactionV1::new(&mut writer)
            .execute(
                &initial,
                &initial_request,
                &ComponentSetVerifier::from_set(&initial),
                &mut registration,
            )
            .unwrap();

        let core_only_change = component_set(HostKindV1::OpenCode, b"core-v2", b"agent-v1");
        let update_request =
            component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Update, 82);
        let receipt = HostComponentSetTransactionV1::new(&mut writer)
            .execute(
                &core_only_change,
                &update_request,
                &ComponentSetVerifier::from_set(&core_only_change),
                &mut registration,
            )
            .unwrap();

        let core = receipt
            .component_receipts
            .iter()
            .find(|receipt| receipt.component == HostBundleComponentV1::Core)
            .unwrap();
        let companion = receipt
            .component_receipts
            .iter()
            .find(|receipt| receipt.component == HostBundleComponentV1::Agent)
            .unwrap();
        assert_eq!(core.operation_id, [82; 16]);
        assert_eq!(core.operation, HostBundleLifecycleOpV1::Update);
        assert_eq!(companion.operation_id, [81; 16]);
        assert_eq!(companion.operation, HostBundleLifecycleOpV1::Install);

        // A companion whose manifest changed but whose artifact bytes did not
        // still earns a fresh receipt. The change must keep the set's shared
        // configuration authority (`configuration_snapshot_id`,
        // `integration_manifest_digest`, `catalog_digest`) uniform across
        // components — `validate_component_set_journal` rejects a set whose
        // components disagree on it — so bump a per-component manifest field
        // (`effective_behavior_digest`) that shifts only the agent's canonical
        // digest and leaves the core component entirely unchanged.
        let mut metadata_only_change = core_only_change.clone();
        metadata_only_change.components[1]
            .manifest
            .effective_behavior_digest = Sha256::digest(b"first-party.behavior.v2").into();
        let metadata_request =
            component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Update, 83);
        let receipt = HostComponentSetTransactionV1::new(&mut writer)
            .execute(
                &metadata_only_change,
                &metadata_request,
                &ComponentSetVerifier::from_set(&metadata_only_change),
                &mut registration,
            )
            .unwrap();
        let metadata_updated = receipt
            .component_receipts
            .iter()
            .find(|receipt| receipt.component == HostBundleComponentV1::Agent)
            .unwrap();
        assert_eq!(metadata_updated.operation_id, [83; 16]);
        assert_eq!(
            metadata_updated.manifest_digest,
            metadata_only_change.components[1]
                .manifest
                .canonical_digest()
                .unwrap()
        );
    }

    /// A journal written by an older binary lives under the shared legacy name.
    /// It must still be discoverable, attributable to exactly one host, and
    /// retired once its host-scoped successor is durable.
    #[test]
    fn a_legacy_shared_component_set_journal_is_attributed_to_its_own_host() {
        let root = tempfile::tempdir().unwrap();
        let wedged = host_scoped_component_set(HostKindV1::OpenCode, "opencode", b"v1");
        let wedged_request =
            component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Install, 61);
        let wedged_verifier = ComponentSetVerifier::from_set(&wedged);
        let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
        let mut second_writer = SecondWriterRegistration {
            artifact_root: root.path().to_path_buf(),
            relative_path: "opencode/core.json",
            bytes: b"foreign".to_vec(),
            rolled_back: false,
        };
        assert!(
            HostComponentSetTransactionV1::new(&mut writer)
                .execute(
                    &wedged,
                    &wedged_request,
                    &wedged_verifier,
                    &mut second_writer,
                )
                .is_err()
        );
        let control = root.path().join(HOST_BUNDLE_CONTROL_DIR);
        fs::rename(
            control.join(component_set_journal_file(HostKindV1::OpenCode)),
            control.join(HOST_COMPONENT_SET_JOURNAL_FILE),
        )
        .unwrap();
        drop(writer);

        let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
        assert_eq!(
            writer.pending_component_set_journal_hosts().unwrap(),
            vec![HostKindV1::OpenCode],
            "a legacy journal is discovered and attributed to its recorded host"
        );
        let unrelated = host_scoped_component_set(HostKindV1::Codex, "codex", b"v1");
        let unrelated_request =
            component_set_request(HostKindV1::Codex, HostBundleLifecycleOpV1::Install, 62);
        let unrelated_verifier = ComponentSetVerifier::from_set(&unrelated);
        HostComponentSetTransactionV1::new(&mut writer)
            .execute(
                &unrelated,
                &unrelated_request,
                &unrelated_verifier,
                &mut ArtifactOnlyTestRegistration,
            )
            .expect("a legacy journal blocks only its own host");
        assert!(
            control.join(HOST_COMPONENT_SET_JOURNAL_FILE).is_file(),
            "another host's transaction never retires the legacy journal"
        );
        writer
            .quarantine_component_set_journal(HostKindV1::OpenCode, 1_700_000_000)
            .unwrap()
            .expect("the legacy journal is quarantined for its own host");
        assert!(!control.join(HOST_COMPONENT_SET_JOURNAL_FILE).exists());
    }

    #[test]
    fn component_set_preflights_cross_component_path_conflicts_before_artifact_writes() {
        let root = tempfile::tempdir().unwrap();
        let component_set = HostComponentSetV1 {
            host: HostKindV1::OpenCode,
            components: vec![
                component_entry(
                    component_manifest(
                        HostKindV1::OpenCode,
                        HostBundleComponentV1::Core,
                        "plugins/shared.json",
                        b"core",
                    ),
                    b"core",
                ),
                component_entry(
                    component_manifest(
                        HostKindV1::OpenCode,
                        HostBundleComponentV1::Agent,
                        "plugins/shared.json",
                        b"agent",
                    ),
                    b"agent",
                ),
            ],
        };
        let request =
            component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Install, 23);
        let verifier = ComponentSetVerifier::from_set(&component_set);
        let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
        let mut registration = ArtifactOnlyTestRegistration;

        assert_eq!(
            HostComponentSetTransactionV1::new(&mut writer).execute(
                &component_set,
                &request,
                &verifier,
                &mut registration,
            ),
            Err(HostBundleError::InvalidManifest)
        );
        assert!(
            !root.path().join("plugins").exists(),
            "cross-component conflicts are rejected before artifact paths are created"
        );
    }

    #[test]
    fn confirmed_component_set_rejects_stale_registration_revision_without_writes() {
        let root = tempfile::tempdir().unwrap();
        let component_set = component_set(HostKindV1::OpenCode, b"core", b"agent");
        let request =
            component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Install, 24);
        let verifier = ComponentSetVerifier::from_set(&component_set);
        let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
        let mut registration = RevisionedTestRegistration { revision: [1; 32] };
        let preview = HostComponentSetTransactionV1::new(&mut writer)
            .preview(&component_set, &request, &verifier, &mut registration)
            .unwrap();
        assert_eq!(preview.operation_id, request.operation_id);
        assert_eq!(preview.base_registration_revision, [1; 32]);
        assert_eq!(preview.current_registration_revision, [1; 32]);
        assert_ne!(preview.plan_digest, [0; 32]);
        let repeated = HostComponentSetTransactionV1::new(&mut writer)
            .preview(&component_set, &request, &verifier, &mut registration)
            .unwrap();
        assert_eq!(repeated, preview);

        registration.revision = [2; 32];
        assert_eq!(
            HostComponentSetTransactionV1::new(&mut writer).execute_confirmed(
                &component_set,
                &request,
                &preview,
                &verifier,
                &mut registration,
            ),
            Err(HostBundleError::StalePreview)
        );
        assert!(!root.path().join("plugins").exists());
    }

    #[test]
    fn confirmed_component_set_rejects_narrowed_plan_identity_without_writes() {
        let root = tempfile::tempdir().unwrap();
        let full = component_set(HostKindV1::OpenCode, b"core", b"agent");
        let full_request =
            component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Install, 25);
        let verifier = ComponentSetVerifier::from_set(&full);
        let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
        let mut registration = RevisionedTestRegistration { revision: [3; 32] };
        let preview = HostComponentSetTransactionV1::new(&mut writer)
            .preview(&full, &full_request, &verifier, &mut registration)
            .unwrap();
        let narrowed = HostComponentSetV1 {
            host: full.host,
            components: vec![full.components[0].clone()],
        };
        let narrowed_request = HostComponentSetExecutionRequestV1 {
            lifecycle: HostComponentSetLifecycleRequestV1 {
                expected_components: vec![HostBundleComponentV1::Core],
                ..full_request.lifecycle.clone()
            },
            operation_id: full_request.operation_id,
        };

        assert_eq!(
            HostComponentSetTransactionV1::new(&mut writer).execute_confirmed(
                &narrowed,
                &narrowed_request,
                &preview,
                &verifier,
                &mut registration,
            ),
            Err(HostBundleError::StalePreview)
        );
        assert!(!root.path().join("plugins").exists());
    }

    #[test]
    fn confirmed_component_set_rejects_changed_artifact_state_without_overwrite() {
        let root = tempfile::tempdir().unwrap();
        let component_set = component_set(HostKindV1::OpenCode, b"core", b"agent");
        let request =
            component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Install, 26);
        let verifier = ComponentSetVerifier::from_set(&component_set);
        let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
        let mut registration = RevisionedTestRegistration { revision: [4; 32] };
        let preview = HostComponentSetTransactionV1::new(&mut writer)
            .preview(&component_set, &request, &verifier, &mut registration)
            .unwrap();

        std::fs::create_dir_all(root.path().join("plugins")).unwrap();
        std::fs::write(root.path().join("plugins/core.json"), b"external").unwrap();
        // Somebody else owns the bytes on this artifact path. That is a
        // standing refusal, not preview staleness: retrying cannot clear it,
        // so it must be reported as the ownership conflict it is.
        assert_eq!(
            HostComponentSetTransactionV1::new(&mut writer).execute_confirmed(
                &component_set,
                &request,
                &preview,
                &verifier,
                &mut registration,
            ),
            Err(HostBundleError::OwnershipConflict)
        );
        assert_eq!(
            std::fs::read(root.path().join("plugins/core.json")).unwrap(),
            b"external"
        );
        assert!(!root.path().join("plugins/agent.json").exists());
    }

    #[test]
    fn feedback_switch_apply_restore_and_aggregate_receipt_share_writer_recovery() {
        let root = tempfile::tempdir().unwrap();
        let previous = manifest(HostKindV1::KimiCode, b"previous");
        let target = manifest(HostKindV1::KimiCode, b"target");
        let verifier = ComponentSetVerifier(vec![
            previous.canonical_digest().unwrap(),
            target.canonical_digest().unwrap(),
        ]);
        let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
        writer
            .execute(
                &previous,
                &execution(
                    HostKindV1::KimiCode,
                    HostBundleLifecycleOpV1::Install,
                    31,
                    true,
                ),
                &content(b"previous"),
                &verifier,
            )
            .unwrap();
        let lifecycle = HostBundleLifecycleRuntimeV1::new(verifier.clone(), writer);
        let mut switch = FeedbackPathRollbackSwitchV1::new(lifecycle);
        let apply = switch
            .feedback_rollback_switch_apply(
                &previous,
                &target,
                &execution(
                    HostKindV1::KimiCode,
                    HostBundleLifecycleOpV1::Update,
                    32,
                    true,
                ),
                &content(b"target"),
                &[],
            )
            .unwrap();
        let mut corrupted_apply = apply.clone();
        corrupted_apply.apply_receipt.operation_id = [0; 16];
        assert_eq!(
            switch.feedback_rollback_switch_restore(
                &corrupted_apply,
                &previous,
                &execution(
                    HostKindV1::KimiCode,
                    HostBundleLifecycleOpV1::Repair,
                    33,
                    true,
                ),
                &content(b"previous"),
                &[],
            ),
            Err(HostBundleError::ReceiptCorrupted)
        );
        assert_eq!(
            std::fs::read(root.path().join("plugins/tracedecay.json")).unwrap(),
            b"target"
        );
        let restore = switch
            .feedback_rollback_switch_restore(
                &apply,
                &previous,
                &execution(
                    HostKindV1::KimiCode,
                    HostBundleLifecycleOpV1::Repair,
                    33,
                    true,
                ),
                &content(b"previous"),
                &[],
            )
            .unwrap();
        let writer = switch.into_lifecycle().into_storage();
        let aggregate = writer
            .publish_feedback_component_set_receipt(&previous, &restore.restore_receipt)
            .unwrap();
        assert_eq!(aggregate.component_manifests, vec![previous]);
        assert_eq!(
            std::fs::read(root.path().join("plugins/tracedecay.json")).unwrap(),
            b"previous"
        );
    }

    #[test]
    fn static_catalog_validates_schema_version_and_capabilities() {
        let bundle = crate::agents::host_bundle_registry::verified_embedded_host_bundle(
            HostKindV1::OpenCode,
            HostBundleComponentV1::Core,
            0,
        )
        .unwrap();
        assert_eq!(
            crate::agents::host_bundle_registry::FIRST_PARTY_COMPONENT_CATALOG_VERSION,
            1
        );
        bundle.manifest.validate_structure().unwrap();
        assert!(require_capability(HostKindV1::OpenCode, HostCapabilityV1::Lsp).is_ok());
        assert!(require_capability(HostKindV1::KimiCode, HostCapabilityV1::Hooks).is_ok());
    }

    #[test]
    fn cursor_native_diagnostics_are_supported_by_the_packaged_extension() {
        assert_eq!(
            stock_host_capabilities(HostKindV1::CursorDesktop)
                .into_iter()
                .find(|record| record.capability == HostCapabilityV1::NativeDiagnostics)
                .map(|record| record.state),
            Some(HostCapabilityStateV1::Supported)
        );
        assert!(
            stock_host_registration_evidence(HostKindV1::CursorDesktop)
                .into_iter()
                .any(|evidence| {
                    evidence.route == HostRegistrationRouteV1::CursorNativeDiagnostics
                        && evidence.state == HostCapabilityStateV1::Supported
                        && evidence.evidence_ref == "plugin/cursor-native-extension/package.json"
                })
        );
    }

    #[test]
    fn corruption_and_external_bundle_paths_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let bundle = manifest(HostKindV1::OpenCode, b"expected");
        let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
        assert_eq!(
            writer.execute(
                &bundle,
                &execution(
                    HostKindV1::OpenCode,
                    HostBundleLifecycleOpV1::Install,
                    1,
                    true
                ),
                &content(b"corrupt"),
                &verifier(&bundle),
            ),
            Err(HostBundleError::ArtifactContentMismatch)
        );
        let mut external = bundle.clone();
        external.artifacts[0].relative_path = "/tmp/third-party.json".to_string();
        assert_eq!(
            external.validate_structure(),
            Err(HostBundleError::UnsafeInstallPath)
        );
    }

    #[test]
    fn repair_adopts_only_cataloged_pre_receipt_artifacts() {
        let repair_root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repair_root.path().join("plugins")).unwrap();
        std::fs::write(
            repair_root.path().join("plugins/tracedecay.json"),
            b"legacy",
        )
        .unwrap();
        let bundle = manifest(HostKindV1::KimiCode, b"expected");
        let mut writer = HostBundleWriterV1::open(repair_root.path()).unwrap();
        let receipt = writer
            .execute(
                &bundle,
                &execution(
                    HostKindV1::KimiCode,
                    HostBundleLifecycleOpV1::Repair,
                    20,
                    true,
                ),
                &content(b"expected"),
                &verifier(&bundle),
            )
            .unwrap();
        assert_eq!(
            std::fs::read(repair_root.path().join("plugins/tracedecay.json")).unwrap(),
            b"expected"
        );
        assert_eq!(receipt.artifacts.len(), 1);

        let install_root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(install_root.path().join("plugins")).unwrap();
        std::fs::write(
            install_root.path().join("plugins/tracedecay.json"),
            b"legacy",
        )
        .unwrap();
        let mut writer = HostBundleWriterV1::open(install_root.path()).unwrap();
        assert_eq!(
            writer.execute(
                &bundle,
                &execution(
                    HostKindV1::KimiCode,
                    HostBundleLifecycleOpV1::Install,
                    21,
                    true,
                ),
                &content(b"expected"),
                &verifier(&bundle),
            ),
            Err(HostBundleError::OwnershipConflict)
        );
        assert_eq!(
            std::fs::read(install_root.path().join("plugins/tracedecay.json")).unwrap(),
            b"legacy"
        );
    }

    #[test]
    fn lifecycle_preserves_ownership_receipts_and_rollback_plan() {
        let root = tempfile::tempdir().unwrap();
        let first = manifest(HostKindV1::KimiCode, b"first");
        let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
        let receipt = writer
            .execute(
                &first,
                &execution(
                    HostKindV1::KimiCode,
                    HostBundleLifecycleOpV1::Install,
                    2,
                    true,
                ),
                &content(b"first"),
                &verifier(&first),
            )
            .unwrap();
        assert_eq!(receipt.host, HostKindV1::KimiCode);
        assert_eq!(receipt.artifacts.len(), 1);
        drop(writer);

        let second = manifest(HostKindV1::KimiCode, b"second");
        let preview = dry_run_host_bundle_lifecycle_at(
            root.path(),
            &second,
            &execution(
                HostKindV1::KimiCode,
                HostBundleLifecycleOpV1::Update,
                3,
                false,
            ),
            &verifier(&second),
            &[],
        )
        .unwrap();
        assert!(preview.confirmation_required);
        assert!(preview.plan.rollback_required);
        assert_eq!(preview.rollback.backup_relative_paths.len(), 1);

        std::fs::write(root.path().join("plugins/tracedecay.json"), b"foreign").unwrap();
        let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
        assert_eq!(
            writer.execute(
                &second,
                &execution(
                    HostKindV1::KimiCode,
                    HostBundleLifecycleOpV1::Update,
                    4,
                    true
                ),
                &content(b"second"),
                &verifier(&second),
            ),
            Err(HostBundleError::OwnershipConflict)
        );
    }

    fn pre_v2_artifact(
        artifact: &HostBundleArtifactV1,
        observed_bytes: &[u8],
        cataloged_ownership_marker: Option<String>,
    ) -> ObservedHostArtifactV1 {
        ObservedHostArtifactV1 {
            relative_path: artifact.relative_path.clone(),
            kind: ObservedArtifactKindV1::RegularFile,
            artifact_digest: Some(Sha256::digest(observed_bytes).into()),
            ownership_marker: None,
            owned_artifact_digest: None,
            cataloged_ownership_marker,
        }
    }

    #[test]
    fn repair_adopts_a_receiptless_artifact_carrying_the_expected_ownership_marker() {
        let bundle = manifest(HostKindV1::KimiCode, b"current");
        let artifact = &bundle.artifacts[0];
        let marker = Some(artifact.ownership_marker.clone());

        // Stale pre-v2 bytes are adopted, but only after they are backed up.
        assert_eq!(
            plan_artifact_action(
                HostBundleLifecycleOpV1::Repair,
                artifact,
                Some(&pre_v2_artifact(artifact, b"pre-v2", marker.clone())),
            ),
            Ok(HostArtifactActionV1::BackupThenReplace)
        );
        // Pre-v2 bytes that already match the catalog need no mutation at all.
        assert_eq!(
            plan_artifact_action(
                HostBundleLifecycleOpV1::Repair,
                artifact,
                Some(&pre_v2_artifact(artifact, b"current", marker.clone())),
            ),
            Ok(HostArtifactActionV1::Noop)
        );

        // Adoption belongs to Repair alone.
        for operation in [
            HostBundleLifecycleOpV1::Install,
            HostBundleLifecycleOpV1::Update,
            HostBundleLifecycleOpV1::Uninstall,
        ] {
            assert_eq!(
                plan_artifact_action(
                    operation,
                    artifact,
                    Some(&pre_v2_artifact(artifact, b"pre-v2", marker.clone())),
                ),
                Err(HostBundleError::OwnershipConflict),
                "{operation:?} must not adopt an artifact no receipt records"
            );
        }
    }

    #[test]
    fn repair_refuses_a_receiptless_artifact_whose_ownership_marker_does_not_match() {
        let bundle = manifest(HostKindV1::KimiCode, b"current");
        let artifact = &bundle.artifacts[0];
        let foreign = expected_ownership_marker(HostKindV1::Hermes, HostBundleComponentV1::Core);
        assert_ne!(foreign, artifact.ownership_marker);

        // A foreign marker on the same deploy path is still a conflict.
        assert_eq!(
            plan_artifact_action(
                HostBundleLifecycleOpV1::Repair,
                artifact,
                Some(&pre_v2_artifact(artifact, b"pre-v2", Some(foreign))),
            ),
            Err(HostBundleError::OwnershipConflict)
        );
        // So is an absent marker: receipt- and orphan-derived observations
        // never carry one, so they can never be adopted.
        assert_eq!(
            plan_artifact_action(
                HostBundleLifecycleOpV1::Repair,
                artifact,
                Some(&pre_v2_artifact(artifact, b"pre-v2", None)),
            ),
            Err(HostBundleError::OwnershipConflict)
        );
        // A receipt claiming the path with a foreign marker keeps the original
        // ownership boundary; adoption never applies to receipt-backed state.
        let mut claimed =
            pre_v2_artifact(artifact, b"pre-v2", Some(artifact.ownership_marker.clone()));
        claimed.ownership_marker = Some(expected_ownership_marker(
            HostKindV1::Kiro,
            HostBundleComponentV1::Core,
        ));
        claimed.owned_artifact_digest = Some(Sha256::digest(b"pre-v2").into());
        assert_eq!(
            plan_artifact_action(HostBundleLifecycleOpV1::Repair, artifact, Some(&claimed)),
            Err(HostBundleError::OwnershipConflict)
        );
    }

    /// Discovery and planning must agree on the ownership boundary: whenever
    /// `Repair` refuses a path as contested, the doctor reports an ownership
    /// conflict, and whenever `Repair` would converge it, the doctor reports
    /// drift or current. A disagreement means the doctor either fails on
    /// something `reinstall` fixes, or hides something it cannot.
    #[test]
    fn doctor_discovery_mirrors_the_repair_ownership_boundary() {
        let bundle = manifest(HostKindV1::OpenCode, b"current");
        let artifact = &bundle.artifacts[0];
        let owned = Some(artifact.ownership_marker.clone());
        let foreign = Some(expected_ownership_marker(
            HostKindV1::Hermes,
            HostBundleComponentV1::Core,
        ));

        for (marker, bytes, expected) in [
            (
                owned.clone(),
                b"current".as_slice(),
                HostBundleComponentDoctorStateV1::Current,
            ),
            (
                owned.clone(),
                b"drifted".as_slice(),
                HostBundleComponentDoctorStateV1::Drifted,
            ),
            (
                foreign,
                b"drifted".as_slice(),
                HostBundleComponentDoctorStateV1::OwnershipConflict,
            ),
            (
                None,
                b"drifted".as_slice(),
                HostBundleComponentDoctorStateV1::OwnershipConflict,
            ),
        ] {
            let mut observed = pre_v2_artifact(artifact, bytes, None);
            observed.ownership_marker = marker;
            observed.owned_artifact_digest = observed
                .ownership_marker
                .as_ref()
                .map(|_| Sha256::digest(b"current").into());

            let state = doctor_artifact_state(&observed, artifact);
            assert_eq!(state, expected, "observed {observed:?}");
            assert_eq!(
                plan_artifact_action(HostBundleLifecycleOpV1::Repair, artifact, Some(&observed))
                    .is_err(),
                state == HostBundleComponentDoctorStateV1::OwnershipConflict,
                "planning and discovery must refuse the same observations"
            );
        }
    }

    #[test]
    fn repair_takes_ownership_of_pre_v2_artifacts_that_no_receipt_records() {
        let root = tempfile::tempdir().unwrap();
        let bundle = manifest(HostKindV1::KimiCode, b"current");
        std::fs::create_dir_all(root.path().join("plugins")).unwrap();
        std::fs::write(root.path().join("plugins/tracedecay.json"), b"pre-v2").unwrap();

        let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
        let receipt = writer
            .execute(
                &bundle,
                &execution(
                    HostKindV1::KimiCode,
                    HostBundleLifecycleOpV1::Repair,
                    21,
                    true,
                ),
                &content(b"current"),
                &verifier(&bundle),
            )
            .unwrap();

        assert_eq!(receipt.artifacts.len(), 1);
        assert_eq!(
            receipt.artifacts[0].ownership_marker,
            bundle.artifacts[0].ownership_marker
        );
        assert_eq!(
            std::fs::read(root.path().join("plugins/tracedecay.json")).unwrap(),
            b"current"
        );
    }

    #[test]
    fn install_still_refuses_pre_v2_artifacts_that_no_receipt_records() {
        let root = tempfile::tempdir().unwrap();
        let bundle = manifest(HostKindV1::KimiCode, b"current");
        std::fs::create_dir_all(root.path().join("plugins")).unwrap();
        std::fs::write(root.path().join("plugins/tracedecay.json"), b"pre-v2").unwrap();

        let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
        assert_eq!(
            writer.execute(
                &bundle,
                &execution(
                    HostKindV1::KimiCode,
                    HostBundleLifecycleOpV1::Install,
                    22,
                    true
                ),
                &content(b"current"),
                &verifier(&bundle),
            ),
            Err(HostBundleError::OwnershipConflict)
        );
        assert_eq!(
            std::fs::read(root.path().join("plugins/tracedecay.json")).unwrap(),
            b"pre-v2"
        );
    }

    struct CurrentRegistration;

    impl HostBundleRegistrationInspectorV1 for CurrentRegistration {
        fn inspect_registration(
            &self,
            _host: HostKindV1,
            _component: HostBundleComponentV1,
        ) -> HostBundleRegistrationStateV1 {
            HostBundleRegistrationStateV1::Current
        }
    }

    struct MissingRegistration;

    impl HostBundleRegistrationInspectorV1 for MissingRegistration {
        fn inspect_registration(
            &self,
            _host: HostKindV1,
            _component: HostBundleComponentV1,
        ) -> HostBundleRegistrationStateV1 {
            HostBundleRegistrationStateV1::Missing
        }
    }

    #[test]
    fn profile_lifecycle_dry_run_does_not_create_missing_control_root() {
        let artifacts = tempfile::tempdir().unwrap();
        let profile = tempfile::tempdir().unwrap();
        let lifecycle = profile.path().join("host-components");
        let manifest = manifest(HostKindV1::Hermes, b"first");

        let preview = dry_run_host_bundle_lifecycle_with_lifecycle_root_at(
            artifacts.path(),
            &lifecycle,
            &manifest,
            &execution(
                HostKindV1::Hermes,
                HostBundleLifecycleOpV1::Install,
                10,
                false,
            ),
            &verifier(&manifest),
            &[],
        )
        .unwrap();

        assert!(preview.confirmation_required);
        assert!(!lifecycle.exists());
    }

    #[test]
    fn profile_owned_receipts_enumerate_only_installed_components_and_retire_backups() {
        let artifacts = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let first = manifest(HostKindV1::Hermes, b"first");
        let second = manifest(HostKindV1::Hermes, b"second");
        let mut writer =
            HostBundleWriterV1::open_with_lifecycle_root(artifacts.path(), lifecycle.path())
                .unwrap();

        writer
            .execute(
                &first,
                &execution(
                    HostKindV1::Hermes,
                    HostBundleLifecycleOpV1::Install,
                    11,
                    true,
                ),
                &content(b"first"),
                &verifier(&first),
            )
            .unwrap();
        let receipt = writer
            .execute(
                &second,
                &execution(
                    HostKindV1::Hermes,
                    HostBundleLifecycleOpV1::Update,
                    12,
                    true,
                ),
                &content(b"second"),
                &verifier(&second),
            )
            .unwrap();

        assert_eq!(
            receipt.rollback_boundary,
            HostBundleRollbackBoundaryV1::Passed
        );
        assert!(
            lifecycle
                .path()
                .join(HOST_BUNDLE_CONTROL_DIR)
                .join(receipt_file(
                    HostKindV1::Hermes,
                    HostBundleComponentV1::Core
                ))
                .is_file()
        );
        assert!(
            !artifacts.path().join(HOST_BUNDLE_CONTROL_DIR).exists(),
            "receipts must be profile-owned rather than ambient-home-owned"
        );
        assert!(
            !lifecycle
                .path()
                .join(HOST_BUNDLE_CONTROL_DIR)
                .join("backups")
                .join(hex::encode([12; 16]))
                .exists(),
            "a committed receipt that passed the rollback boundary retires its backups"
        );

        let referenced_operation = [14; 16];
        let referenced_backup = lifecycle
            .path()
            .join(HOST_BUNDLE_CONTROL_DIR)
            .join("backups")
            .join(hex::encode(referenced_operation));
        drop(
            writer
                .open_or_create_backup_dir(referenced_operation)
                .unwrap(),
        );
        let mut receipt_with_history = receipt.clone();
        receipt_with_history
            .rollback_history
            .push(referenced_operation);
        writer.write_receipt(&receipt_with_history).unwrap();
        writer
            .cleanup_unreferenced_backup_dir(referenced_operation)
            .unwrap();
        assert!(
            referenced_backup.is_dir(),
            "receipt-referenced rollback history must be preserved"
        );

        let report = inspect_installed_host_bundle_components_at(
            artifacts.path(),
            lifecycle.path(),
            &CurrentRegistration,
        )
        .unwrap();
        assert_eq!(report.components.len(), 1);
        // Synthetic fixture bytes are not the verified embedded Hermes catalog
        // entry, so Doctor surfaces Repairable (catalog drift) even when the
        // registration probe reports Current.
        assert_eq!(
            report.components[0].state,
            HostBundleComponentDoctorStateV1::Repairable
        );
        assert_eq!(report.components[0].host, Some(HostKindV1::Hermes));
        assert_eq!(
            report.components[0].component,
            Some(HostBundleComponentV1::Core)
        );
        let repairable = inspect_installed_host_bundle_components_at(
            artifacts.path(),
            lifecycle.path(),
            &MissingRegistration,
        )
        .unwrap();
        assert_eq!(
            repairable.components[0].state,
            HostBundleComponentDoctorStateV1::Repairable
        );
        assert_eq!(
            repairable.components[0].repair_action,
            "run `tracedecay install --agent hermes`"
        );

        writer
            .execute(
                &second,
                &execution(
                    HostKindV1::Hermes,
                    HostBundleLifecycleOpV1::Uninstall,
                    15,
                    true,
                ),
                &[],
                &verifier(&second),
            )
            .unwrap();
        let uninstalled = inspect_installed_host_bundle_components_at(
            artifacts.path(),
            lifecycle.path(),
            &MissingRegistration,
        )
        .unwrap();
        assert!(uninstalled.components.is_empty());

        // The same uninstall receipt with the host still advertising the
        // component is a registered orphan: nothing owns the registration, so
        // Doctor must surface it rather than skip past the receipt.
        let orphaned = inspect_installed_host_bundle_components_at(
            artifacts.path(),
            lifecycle.path(),
            &CurrentRegistration,
        )
        .unwrap();
        assert_eq!(orphaned.components.len(), 1);
        assert_eq!(
            orphaned.components[0].state,
            HostBundleComponentDoctorStateV1::OrphanedRegistration
        );
        assert_eq!(orphaned.components[0].host, Some(HostKindV1::Hermes));
        assert!(orphaned.components[0].artifacts.is_empty());
    }

    #[test]
    fn receipt_doctor_never_treats_unknown_embedded_bundle_as_current() {
        let artifacts = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let manifest = manifest(HostKindV1::CursorCloud, b"unsupported");
        let mut writer =
            HostBundleWriterV1::open_with_lifecycle_root(artifacts.path(), lifecycle.path())
                .unwrap();
        writer
            .execute(
                &manifest,
                &execution(
                    HostKindV1::CursorCloud,
                    HostBundleLifecycleOpV1::Install,
                    16,
                    true,
                ),
                &content(b"unsupported"),
                &verifier(&manifest),
            )
            .unwrap();

        let report = inspect_installed_host_bundle_components_at(
            artifacts.path(),
            lifecycle.path(),
            &CurrentRegistration,
        )
        .unwrap();
        assert_eq!(
            report.components[0].state,
            HostBundleComponentDoctorStateV1::Repairable
        );
    }

    #[test]
    fn receipt_doctor_classifies_missing_conflicting_and_corrupt_components() {
        let artifacts = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let bundle = manifest(HostKindV1::OpenCode, b"current");
        let mut writer =
            HostBundleWriterV1::open_with_lifecycle_root(artifacts.path(), lifecycle.path())
                .unwrap();
        writer
            .execute(
                &bundle,
                &execution(
                    HostKindV1::OpenCode,
                    HostBundleLifecycleOpV1::Install,
                    13,
                    true,
                ),
                &content(b"current"),
                &verifier(&bundle),
            )
            .unwrap();

        let artifact = artifacts.path().join("plugins/tracedecay.json");
        std::fs::remove_file(&artifact).unwrap();
        let report = inspect_installed_host_bundle_components_at(
            artifacts.path(),
            lifecycle.path(),
            &CurrentRegistration,
        )
        .unwrap();
        assert_eq!(
            report.components[0].state,
            HostBundleComponentDoctorStateV1::Missing
        );

        // Same ownership marker, different bytes: ordinary content drift. The
        // planner would converge this with `BackupThenReplace` under `Repair`,
        // so discovery must not report a contested path.
        std::fs::write(&artifact, b"drifted").unwrap();
        let report = inspect_installed_host_bundle_components_at(
            artifacts.path(),
            lifecycle.path(),
            &CurrentRegistration,
        )
        .unwrap();
        assert_eq!(
            report.components[0].state,
            HostBundleComponentDoctorStateV1::Drifted
        );
        assert_eq!(
            report.components[0].artifacts[0].state,
            HostBundleComponentDoctorStateV1::Drifted
        );
        assert_eq!(
            report.components[0].repair_action,
            "run `tracedecay reinstall --component core --yes` (backs up and re-owns)"
        );

        // A second receipt claiming the same deploy path with a different
        // ownership marker is a foreign claim: no single component owns the
        // bytes, so both components report the conflict rather than one of
        // them silently adopting the other's path.
        let foreign_receipt = HostBundleInstallReceiptV1 {
            schema_version: HOST_BUNDLE_RECEIPT_SCHEMA_VERSION,
            operation_id: [23; 16],
            host: HostKindV1::Hermes,
            component: HostBundleComponentV1::Core,
            operation: HostBundleLifecycleOpV1::Install,
            manifest_digest: manifest(HostKindV1::Hermes, b"foreign")
                .canonical_digest()
                .unwrap(),
            artifacts: vec![HostBundleReceiptArtifactV1 {
                relative_path: "plugins/tracedecay.json".to_string(),
                artifact_digest: Sha256::digest(b"foreign").into(),
                ownership_marker: expected_ownership_marker(
                    HostKindV1::Hermes,
                    HostBundleComponentV1::Core,
                ),
            }],
            rollback_boundary: HostBundleRollbackBoundaryV1::Passed,
            rollback_history: Vec::new(),
        };
        let foreign_receipt_path =
            lifecycle
                .path()
                .join(HOST_BUNDLE_CONTROL_DIR)
                .join(receipt_file(
                    HostKindV1::Hermes,
                    HostBundleComponentV1::Core,
                ));
        std::fs::write(
            &foreign_receipt_path,
            serde_json::to_vec(&foreign_receipt).unwrap(),
        )
        .unwrap();
        let report = inspect_installed_host_bundle_components_at(
            artifacts.path(),
            lifecycle.path(),
            &CurrentRegistration,
        )
        .unwrap();
        assert!(
            report
                .components
                .iter()
                .all(|component| component.state
                    == HostBundleComponentDoctorStateV1::OwnershipConflict),
            "a contested deploy path conflicts for every claimant"
        );
        std::fs::remove_file(&foreign_receipt_path).unwrap();

        let receipt_path = lifecycle
            .path()
            .join(HOST_BUNDLE_CONTROL_DIR)
            .join(receipt_file(
                HostKindV1::OpenCode,
                HostBundleComponentV1::Core,
            ));
        std::fs::write(receipt_path, b"{").unwrap();
        let report = inspect_installed_host_bundle_components_at(
            artifacts.path(),
            lifecycle.path(),
            &CurrentRegistration,
        )
        .unwrap();
        assert_eq!(
            report.components[0].state,
            HostBundleComponentDoctorStateV1::Corrupt
        );
    }

    /// Exactly what Codex's adapter reports: activation lives behind an
    /// interactive plugin UI, and the staged source bundle the host would
    /// materialise from is present (`Repairable`) but never activated.
    const INTERACTIVE_ACTIVATION_GUIDANCE: &str = "Non-interactive Codex plugin activation is unavailable. In Codex's plugin UI, activate \
         tracedecay from the personal marketplace, then re-run doctor.";

    struct InteractiveActivationRegistration(HostBundleRegistrationStateV1);

    impl HostBundleRegistrationInspectorV1 for InteractiveActivationRegistration {
        fn inspect_registration(
            &self,
            _host: HostKindV1,
            _component: HostBundleComponentV1,
        ) -> HostBundleRegistrationStateV1 {
            self.0
        }

        fn interactive_activation_guidance(&self, _host: HostKindV1) -> Option<String> {
            Some(INTERACTIVE_ACTIVATION_GUIDANCE.to_string())
        }
    }

    /// A host TraceDecay can activate without the operator: no guidance, so a
    /// missing receipt-owned artifact stays a blocking receipt-integrity fault.
    struct NonInteractiveStagedRegistration;

    impl HostBundleRegistrationInspectorV1 for NonInteractiveStagedRegistration {
        fn inspect_registration(
            &self,
            _host: HostKindV1,
            _component: HostBundleComponentV1,
        ) -> HostBundleRegistrationStateV1 {
            HostBundleRegistrationStateV1::Repairable
        }
    }

    /// Write one receipt claiming `artifacts`, materialising only the entries
    /// whose bytes are `Some`. Absent entries are what the host would have
    /// created during activation.
    fn write_component_receipt(
        artifact_root: &Path,
        lifecycle_root: &Path,
        host: HostKindV1,
        component: HostBundleComponentV1,
        artifacts: &[(&str, Option<&[u8]>)],
    ) {
        let receipt = HostBundleInstallReceiptV1 {
            schema_version: HOST_BUNDLE_RECEIPT_SCHEMA_VERSION,
            operation_id: [31; 16],
            host,
            component,
            operation: HostBundleLifecycleOpV1::Install,
            manifest_digest: Sha256::digest(b"staged-component-set").into(),
            artifacts: artifacts
                .iter()
                .map(|(relative_path, bytes)| HostBundleReceiptArtifactV1 {
                    relative_path: (*relative_path).to_string(),
                    artifact_digest: Sha256::digest(bytes.unwrap_or(b"activated")).into(),
                    ownership_marker: expected_ownership_marker(host, component),
                })
                .collect(),
            rollback_boundary: HostBundleRollbackBoundaryV1::Passed,
            rollback_history: Vec::new(),
        };
        for (relative_path, bytes) in artifacts {
            let Some(bytes) = bytes else {
                continue;
            };
            let path = artifact_root.join(relative_path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        std::fs::write(
            lifecycle_root
                .join(HOST_BUNDLE_CONTROL_DIR)
                .join(receipt_file(host, component)),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
    }

    /// A host that only activates through its own UI has no command that could
    /// deploy these bytes, so a component whose receipt-owned artifacts were
    /// never materialised is a pending user action, not receipt drift. Doctor
    /// would otherwise fail forever on every machine whose operator has not
    /// clicked through the host.
    #[test]
    fn never_activated_interactive_host_component_defers_instead_of_failing() {
        let artifacts = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        drop(
            HostBundleWriterV1::open_with_lifecycle_root(artifacts.path(), lifecycle.path())
                .unwrap(),
        );
        write_component_receipt(
            artifacts.path(),
            lifecycle.path(),
            HostKindV1::Codex,
            HostBundleComponentV1::ContextMcp,
            &[(".codex/plugins/tracedecay/.mcp.json", None)],
        );

        let report = inspect_installed_host_bundle_components_at(
            artifacts.path(),
            lifecycle.path(),
            &InteractiveActivationRegistration(HostBundleRegistrationStateV1::Repairable),
        )
        .unwrap();

        assert_eq!(
            report.components[0].state,
            HostBundleComponentDoctorStateV1::ActivationDeferred
        );
        assert_eq!(
            report.components[0].repair_action, INTERACTIVE_ACTIVATION_GUIDANCE,
            "the deferral must carry the host's own activation guidance, never a reinstall that cannot converge"
        );
    }

    /// The deferral is scoped to components the host never materialised. Once
    /// any receipt-owned byte is on disk, an absent sibling is a file that went
    /// missing after activation — real drift, and still blocking.
    #[test]
    fn partially_materialised_interactive_host_component_still_fails() {
        let artifacts = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        drop(
            HostBundleWriterV1::open_with_lifecycle_root(artifacts.path(), lifecycle.path())
                .unwrap(),
        );
        write_component_receipt(
            artifacts.path(),
            lifecycle.path(),
            HostKindV1::Codex,
            HostBundleComponentV1::Core,
            &[
                (
                    ".codex/plugins/tracedecay/.codex-plugin/plugin.json",
                    Some(b"activated"),
                ),
                (".codex/plugins/tracedecay/hooks/hooks.json", None),
            ],
        );

        let report = inspect_installed_host_bundle_components_at(
            artifacts.path(),
            lifecycle.path(),
            &InteractiveActivationRegistration(HostBundleRegistrationStateV1::Repairable),
        )
        .unwrap();

        assert_eq!(
            report.components[0].state,
            HostBundleComponentDoctorStateV1::Missing
        );
    }

    /// Nothing staged is not a pending activation: with no source bundle the
    /// operator has nothing to activate, so the component is genuinely missing.
    #[test]
    fn unstaged_interactive_host_component_still_fails() {
        let artifacts = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        drop(
            HostBundleWriterV1::open_with_lifecycle_root(artifacts.path(), lifecycle.path())
                .unwrap(),
        );
        write_component_receipt(
            artifacts.path(),
            lifecycle.path(),
            HostKindV1::Codex,
            HostBundleComponentV1::ContextMcp,
            &[(".codex/plugins/tracedecay/.mcp.json", None)],
        );

        let report = inspect_installed_host_bundle_components_at(
            artifacts.path(),
            lifecycle.path(),
            &InteractiveActivationRegistration(HostBundleRegistrationStateV1::Missing),
        )
        .unwrap();

        assert_eq!(
            report.components[0].state,
            HostBundleComponentDoctorStateV1::Missing
        );
    }

    /// Receipt-integrity checking is untouched for every host TraceDecay can
    /// actually drive: there the reinstall converges the state, so an absent
    /// receipt-owned artifact keeps blocking.
    #[test]
    fn non_interactive_host_missing_artifacts_still_fail() {
        let artifacts = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        drop(
            HostBundleWriterV1::open_with_lifecycle_root(artifacts.path(), lifecycle.path())
                .unwrap(),
        );
        write_component_receipt(
            artifacts.path(),
            lifecycle.path(),
            HostKindV1::CursorDesktop,
            HostBundleComponentV1::ContextMcp,
            &[(".cursor/plugins/local/tracedecay/mcp.json", None)],
        );

        let report = inspect_installed_host_bundle_components_at(
            artifacts.path(),
            lifecycle.path(),
            &NonInteractiveStagedRegistration,
        )
        .unwrap();

        assert_eq!(
            report.components[0].state,
            HostBundleComponentDoctorStateV1::Missing
        );
        assert_eq!(
            report.components[0].repair_action,
            "run `tracedecay reinstall --component context-mcp --yes`"
        );
    }

    #[test]
    fn doctor_surfaces_restart_safe_feedback_rollback_state() {
        let root = tempfile::tempdir().unwrap();
        let writer = HostBundleWriterV1::open(root.path()).unwrap();
        std::fs::write(
            root.path()
                .join(HOST_BUNDLE_CONTROL_DIR)
                .join("feedback-rollback.kimi.v1.json"),
            serde_json::to_vec(&serde_json::json!({
                "host": "kimi_code",
                "status": "applied"
            }))
            .unwrap(),
        )
        .unwrap();
        drop(writer);

        let report = inspect_installed_host_bundle_components_at(
            root.path(),
            root.path(),
            &CurrentRegistration,
        )
        .unwrap();
        assert_eq!(report.components.len(), 1);
        assert_eq!(report.components[0].host, Some(HostKindV1::KimiCode));
        assert_eq!(
            report.components[0].component,
            Some(HostBundleComponentV1::Core)
        );
        assert_eq!(
            report.components[0].state,
            HostBundleComponentDoctorStateV1::Repairable
        );
        assert!(
            report.components[0]
                .repair_action
                .contains("feedback-rollback restore")
        );
    }
}
