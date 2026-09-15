//! Read-only discovery of installed components and their repair guidance.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tracedecay_host_integration::host_bundle_storage_failure;

use super::control::{
    HOST_BUNDLE_CONTROL_DIR, HOST_BUNDLE_JOURNAL_FILE, HOST_COMPONENT_SET_JOURNAL_FILE,
    MAX_CONTROL_FILE_BYTES, component_set_journal_file, component_slug, receipt_file,
    receipt_identity_from_file_name, validate_component_set_journal, validate_journal,
    validate_receipt,
};
use super::planner::{ObservedArtifactKindV1, ObservedHostArtifactV1, observe_artifact_at};
use super::{
    HostBundleArtifactV1, HostBundleComponentV1, HostBundleError, HostBundleInstallReceiptV1,
    HostBundleJournalV1, HostBundleLifecycleOpV1, HostBundleRollbackBoundaryV1,
    HostComponentSetJournalV1, HostEditStopConformanceEvidenceV1, HostKindV1,
    HostNativeFixtureEvidenceV1, native_host_edit_stop_conformance_evidence, stock_host_kinds,
    supported_host_edit_stop_conformance_evidence,
};

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
    /// Event-specific truth for the canonical receipt-backed host set. Read
    /// routes remain independently available, but never stand in for a native
    /// edit or stop event.
    pub supported_host_edit_stop_conformance: Vec<HostEditStopConformanceEvidenceV1>,
}

impl Default for HostBundleDoctorReportV1 {
    fn default() -> Self {
        Self {
            components: Vec::new(),
            native_edit_stop_conformance: native_host_edit_stop_conformance_evidence(),
            supported_host_edit_stop_conformance: supported_host_edit_stop_conformance_evidence(),
        }
    }
}

#[hotpath::measure(label = "hosts.agent.host_bundle.inspect_installed")]
pub fn inspect_installed_host_bundle_components_at(
    artifact_root: &Path,
    lifecycle_root: &Path,
    registrations: &impl HostBundleRegistrationInspectorV1,
    generator_commit: &str,
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
            generator_commit,
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

/// Doctor-side mirror of the planner's (`plan_artifact_action`) marker-vs-digest boundary
/// under `Repair` — the operation every repair action recommends.
///
/// The ownership marker is the only conflict gate: a foreign or absent marker
/// is a contested path that planning refuses outside the narrow pre-receipt
/// adoption boundary, while a path whose marker is still this component's own
/// is ordinary content drift that `Repair` plans as `BackupThenReplace`.
/// Keeping the two in lockstep means discovery can never report a conflict the
/// planner would have converged, or vice versa.
pub(super) fn doctor_artifact_state(
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

pub(super) fn corrupt_component_result(
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

pub(super) fn repair_action(
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
