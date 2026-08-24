use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};
use tracedecay::agents::host_bundle_v2::{
    HostBundleComponentV1, HostBundleInstallReceiptV1, HostBundleManifestV1,
    HostComponentSetReceiptV1,
};

pub(super) fn selected_feedback_component(
    aggregate: &HostComponentSetReceiptV1,
) -> tracedecay::errors::Result<HostBundleComponentV1> {
    if aggregate
        .component_manifests
        .iter()
        .any(|manifest| manifest.component == HostBundleComponentV1::Core)
    {
        return Ok(HostBundleComponentV1::Core);
    }
    if let [manifest] = aggregate.component_manifests.as_slice()
        && manifest.component == HostBundleComponentV1::ContextMcp
    {
        return Ok(HostBundleComponentV1::ContextMcp);
    }
    Err(tracedecay::errors::TraceDecayError::Config {
        message: "aggregate receipt has no selected feedback component".to_string(),
    })
}

pub(super) fn live_feedback_receipt(
    home: &Path,
    aggregate: &HostComponentSetReceiptV1,
) -> tracedecay::errors::Result<(HostBundleManifestV1, HostBundleInstallReceiptV1)> {
    let component = selected_feedback_component(aggregate)?;
    let mut manifest = aggregate
        .component_manifests
        .iter()
        .find(|manifest| manifest.component == component)
        .cloned()
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: "aggregate receipt has no selected feedback manifest".to_string(),
        })?;
    let mut receipt = aggregate
        .component_receipts
        .iter()
        .find(|receipt| receipt.component == component)
        .cloned()
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: "aggregate receipt has no selected feedback receipt".to_string(),
        })?;
    let companion_owned_paths = companion_owned_live_paths(home, aggregate)?;
    manifest
        .artifacts
        .retain(|artifact| !companion_owned_paths.contains(&artifact.relative_path));
    receipt
        .artifacts
        .retain(|artifact| !companion_owned_paths.contains(&artifact.relative_path));
    for artifact in &mut manifest.artifacts {
        if let Ok(bytes) = fs::read(home.join(&artifact.relative_path)) {
            artifact.artifact_digest = Sha256::digest(bytes).into();
        }
    }
    Ok((manifest, receipt))
}

pub(super) fn companion_owned_live_paths(
    home: &Path,
    aggregate: &HostComponentSetReceiptV1,
) -> tracedecay::errors::Result<BTreeSet<String>> {
    let selected = selected_feedback_component(aggregate)?;
    Ok(aggregate
        .component_receipts
        .iter()
        .filter(|receipt| receipt.component != selected)
        .flat_map(|receipt| &receipt.artifacts)
        .filter(|owned| {
            fs::read(home.join(&owned.relative_path))
                .ok()
                .is_some_and(|bytes| {
                    <[u8; 32]>::from(Sha256::digest(bytes)) == owned.artifact_digest
                })
        })
        .map(|owned| owned.relative_path.clone())
        .collect())
}

pub(super) fn aggregate_with_feedback_component(
    previous: &HostComponentSetReceiptV1,
    manifest: &HostBundleManifestV1,
    component_receipt: &HostBundleInstallReceiptV1,
) -> HostComponentSetReceiptV1 {
    let mut aggregate = previous.clone();
    aggregate.operation_id = component_receipt.operation_id;
    aggregate.operation = component_receipt.operation;
    aggregate
        .component_manifests
        .retain(|candidate| candidate.component != manifest.component);
    aggregate.component_manifests.push(manifest.clone());
    aggregate
        .component_manifests
        .sort_by_key(|candidate| candidate.component);
    aggregate
        .component_receipts
        .retain(|candidate| candidate.component != component_receipt.component);
    aggregate.component_receipts.push(component_receipt.clone());
    aggregate
        .component_receipts
        .sort_by_key(|candidate| candidate.component);
    aggregate.confirmed_plan_digest = None;
    aggregate.base_registration_revision = None;
    aggregate.current_registration_revision = None;
    aggregate.artifact_state_revision = None;
    aggregate
}
