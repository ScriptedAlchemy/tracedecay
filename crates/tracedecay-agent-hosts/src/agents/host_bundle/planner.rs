//! Ownership-aware lifecycle planning and read-only previews.
//!
//! Everything here observes host state and produces immutable mutation plans;
//! nothing writes a host path, a receipt, or a journal.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tracedecay_domain::canonical_json_bytes;
use tracedecay_host_integration::host_bundle_stale_preview;
use tracedecay_host_integration::host_bundle_storage_failure;

use super::control::{read_receipt_at, validate_component_set_request};
use super::model::{
    CompetingHostExtensionClaimV1, HostBundleExecutionRequestV1, HostBundleLifecyclePreviewV1,
    HostBundleRollbackSeamV1, HostComponentSetExecutionRequestV1,
    HostComponentSetLifecyclePreviewV1, HostComponentSetRegistrationV1, HostComponentSetV1,
};
use super::{
    HostBundleArtifactContentV1, HostBundleArtifactV1, HostBundleComponentV1, HostBundleError,
    HostBundleInstallReceiptV1, HostBundleLifecycleOpV1, HostBundleManifestV1,
    HostBundleVerificationAdapterV1, HostKindV1, MAX_ARTIFACT_CONTENT_BYTES, validate_identifier,
    validate_relative_install_path,
};

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
#[hotpath::measure(label = "hosts.agent.host_bundle.plan_complete")]
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
    /// `Install`, `Update`, and `Repair` may adopt such an artifact.
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
    /// Authorization to adopt receiptless observations at this component's
    /// cataloged deploy paths. True only when the operator explicitly
    /// confirmed adoption (`--yes --adopt`) or the host adapter recognized
    /// the receiptless deployment as a prior first-party bundle
    /// ([`HostComponentSetRegistrationV1::receiptless_component_provenance`]).
    /// A cataloged deploy path alone never grants this; byte-identical
    /// staged deploys are adoptable without it.
    pub adopt_receiptless: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HostBundleMutationPlanV1 {
    pub operation: HostBundleLifecycleOpV1,
    pub host: HostKindV1,
    pub component: HostBundleComponentV1,
    pub mutations: Vec<HostArtifactMutationV1>,
    pub rollback_required: bool,
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
        let action = plan_artifact_action(
            request.operation,
            artifact,
            state,
            request.adopt_receiptless,
        )?;
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
                // Receipt-derived removals never adopt; ownership is proven
                // by the receipt digest or refused.
                action: plan_artifact_action(
                    HostBundleLifecycleOpV1::Uninstall,
                    &artifact,
                    Some(observed),
                    false,
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

pub(super) fn plan_artifact_action(
    operation: HostBundleLifecycleOpV1,
    artifact: &HostBundleArtifactV1,
    state: Option<&ObservedHostArtifactV1>,
    adopt_receiptless: bool,
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
        ObservedArtifactKindV1::Missing => {
            return plan_artifact_action(operation, artifact, None, adopt_receiptless);
        }
        ObservedArtifactKindV1::Symlink | ObservedArtifactKindV1::Directory => {
            return Err(HostBundleError::UnsafeInstallPath);
        }
        ObservedArtifactKindV1::RegularFile => {}
    }
    if state.ownership_marker.as_deref() != Some(artifact.ownership_marker.as_str()) {
        // Receiptless artifacts are adoptable only inside the boundary
        // `adopts_pre_receipt_artifact` defines: byte-identical staged bytes,
        // host-recognized legacy provenance, or the operator's explicit
        // adoption. Everything else with a foreign or absent marker conflicts.
        if !adopts_pre_receipt_artifact(operation, artifact, state, adopt_receiptless) {
            let reason = if let Some(marker) = state.ownership_marker.as_deref() {
                format!(
                    "a receipt records ownership marker {marker:?}, expected {:?}; uninstall the \
                     component named by the recorded marker before retrying",
                    artifact.ownership_marker
                )
            } else if state.cataloged_ownership_marker.as_deref()
                != Some(artifact.ownership_marker.as_str())
            {
                format!(
                    "the observation is neither owned by this component nor a receiptless \
                     cataloged deploy (expected marker {:?}); move or remove the conflicting \
                     file before retrying",
                    artifact.ownership_marker
                )
            } else {
                "a receiptless file at a cataloged deploy path is adopted only when it matches \
                 the staged bytes, carries recognizable legacy first-party provenance, or the \
                 operator re-runs with `--yes --adopt`"
                    .to_string()
            };
            return Err(HostBundleError::OwnershipConflict(format!(
                "{}: existing file is not owned by this component; {reason}",
                artifact.relative_path,
            )));
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
                Err(HostBundleError::OwnershipConflict(format!(
                    "{}: deployed bytes no longer match the receipt-owned content; refusing to \
                     delete a file modified outside TraceDecay",
                    artifact.relative_path
                )))
            }
        }
        // Install over a path this component's own receipt already claims is
        // the ordinary reinstall/update journey (e.g. a new bundle version
        // over an unmodified prior deploy), so it converges exactly as
        // `Update` does. Only a third-party edit — bytes that match neither
        // the catalog nor the receipt-owned content — is a conflict.
        HostBundleLifecycleOpV1::Install | HostBundleLifecycleOpV1::Update => {
            if state.artifact_digest == Some(artifact.artifact_digest) {
                Ok(HostArtifactActionV1::Noop)
            } else if state.artifact_digest == Some(owned_digest) {
                Ok(HostArtifactActionV1::BackupThenReplace)
            } else {
                Err(HostBundleError::OwnershipConflict(format!(
                    "{}: deployed file was modified outside TraceDecay since its receipt was \
                     written (marker {:?})",
                    artifact.relative_path, artifact.ownership_marker
                )))
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
/// Pre-v2 installers and pre-activation staging deploy first-party artifacts
/// without writing a v2 ownership receipt, so their files are
/// indistinguishable from foreign files by receipt evidence alone. Sitting at
/// a cataloged deploy path proves nothing: `cataloged_ownership_marker` is
/// synthesized from the current manifest, so any bytes an operator placed at
/// that path would carry it. Adoption therefore requires the cataloged path
/// AND an explicit authority:
///
/// * byte identity: the observed bytes equal the staged catalog content.
///   This is the documented hand-over journey for hosts that own their own
///   activation (Claude Code, Codex): TraceDecay stages the deploy, the host
///   activates it, and the operator's re-run records exactly the bytes
///   TraceDecay staged. Paths inside TraceDecay's own staging namespace
///   ([`HOST_BUNDLE_STAGE_ROOT_RELATIVE`]) extend this to divergent bytes,
///   because everything there is TraceDecay-staged by construction;
/// * recognizable legacy provenance: the host adapter inspected the
///   receiptless deployment and recognized a prior first-party bundle
///   ([`HostComponentSetRegistrationV1::receiptless_component_provenance`]),
///   e.g. a Cursor plugin directory whose own manifest names tracedecay.
///   Live pre-receipt bundles restamp versions and binary paths every
///   release, so they are never byte-identical — provenance is what lets
///   `install`/`update-plugin` converge them without wedging;
/// * explicit operator adoption: `--yes --adopt` claimed the path knowingly.
///
/// `Uninstall` never adopts: it must not delete a file whose ownership it
/// cannot prove. Observations derived from receipts or orphan paths never
/// carry the cataloged marker, so they can never reach this branch, and a
/// receipt-backed artifact keeps the unmodified marker equality check as the
/// security boundary. Adoption itself never destroys anything: byte-identical
/// files become `Noop`, everything else is backed up before it is replaced.
fn adopts_pre_receipt_artifact(
    operation: HostBundleLifecycleOpV1,
    artifact: &HostBundleArtifactV1,
    state: &ObservedHostArtifactV1,
    adopt_receiptless: bool,
) -> bool {
    let receiptless_cataloged_path = state.ownership_marker.is_none()
        && state.owned_artifact_digest.is_none()
        && state.cataloged_ownership_marker.as_deref() == Some(artifact.ownership_marker.as_str());
    if !receiptless_cataloged_path {
        return false;
    }
    match operation {
        HostBundleLifecycleOpV1::Install
        | HostBundleLifecycleOpV1::Update
        | HostBundleLifecycleOpV1::Repair => {
            state.artifact_digest == Some(artifact.artifact_digest)
                || adopt_receiptless
                || first_party_staged_deploy_path(&artifact.relative_path)
        }
        HostBundleLifecycleOpV1::Uninstall => false,
    }
}

/// Relative prefix of TraceDecay's own host-bundle staging namespace. Hosts
/// that activate a staged source natively (Kimi's `/plugins install`) deploy
/// their cataloged artifacts here, inside TraceDecay's private data dir.
pub const HOST_BUNDLE_STAGE_ROOT_RELATIVE: &str = ".tracedecay/host-bundle-stage";

/// A deploy path inside TraceDecay's own staging namespace is
/// TraceDecay-staged by construction — it is never host or user config, so a
/// receiptless divergent file there is a staging left by another TraceDecay
/// binary version (its render bakes in the binary path), not a foreign claim.
/// Refusing it would wedge the documented native-activation hand-over
/// whenever the binary moved between staging and the recording re-run.
fn first_party_staged_deploy_path(relative_path: &str) -> bool {
    Path::new(relative_path).starts_with(HOST_BUNDLE_STAGE_ROOT_RELATIVE)
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

#[hotpath::measure(label = "hosts.agent.host_bundle.dry_run")]
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
#[hotpath::measure(label = "hosts.agent.host_bundle.component_set_dry_run")]
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
                adopt_receiptless: component_receiptless_adoption(
                    &planning_request,
                    registration,
                    component.manifest.component,
                ),
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
        return Err(host_bundle_stale_preview!());
    }
    if discovered_competing_extension_claims(component_set, &planning_request, registration)?
        != competing_extension_claims
    {
        return Err(host_bundle_stale_preview!());
    }
    let current_artifact_state_revision =
        component_set_artifact_state_revision(artifact_root, lifecycle_root, component_set)?;
    if current_artifact_state_revision != base_artifact_state_revision {
        return Err(host_bundle_stale_preview!());
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

/// Resolve per-component receiptless-adoption authority for a set request:
/// the operator's explicit `--adopt` or the adapter's recognized legacy
/// provenance. Preview and confirmed execute both resolve through this, so
/// their plans agree; provenance drift between them surfaces as the ordinary
/// plan/`StalePreview` mismatch.
pub(super) fn component_receiptless_adoption<R: HostComponentSetRegistrationV1>(
    request: &HostComponentSetExecutionRequestV1,
    registration: &R,
    component: HostBundleComponentV1,
) -> bool {
    request.lifecycle.explicit_adoption || registration.receiptless_component_provenance(component)
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

pub(super) fn observe_artifact_at(
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

pub(super) fn validate_competing_extension_claims(
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

pub(super) fn validate_artifact_contents_for_operation(
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
