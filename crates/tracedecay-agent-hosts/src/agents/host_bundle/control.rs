//! Layout of the `.tracedecay-host-bundle-v1` control directory: file names,
//! path-rooted receipt readers, and receipt validators.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use tracedecay_host_integration::host_bundle_storage_failure;

use super::model::{
    HostComponentSetExecutionRequestV1, HostComponentSetLifecyclePreviewV1, HostComponentSetV1,
};
use super::planner::inspect_install_target;
use super::{
    HOST_BUNDLE_RECEIPT_SCHEMA_VERSION, HostBundleError, HostBundleInstallReceiptV1,
    HostBundleLifecycleOpV1, HostComponentSetReceiptV1, HostComponentV1, HostKindV1,
    MAX_HOST_COMPONENTS, MAX_MANIFEST_ARTIFACTS, stock_host_kinds, validate_identifier,
    validate_relative_install_path,
};

pub(super) const HOST_BUNDLE_CONTROL_DIR: &str = ".tracedecay-host-bundle-v1";
/// Retired lifecycle-root lock. Hosts do not share a write target, so each
/// host owns `writer.{slug}.v1.lock`. This name is not acquired; a new binary
/// must not recreate it or independent hosts serialize again.
pub(super) const HOST_BUNDLE_LOCK_FILE: &str = "writer.v1.lock";
pub(super) const MAX_CONTROL_FILE_BYTES: usize = 256 * 1024;

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
        let receipt = match parse_receipt::<HostComponentSetReceiptV1>(&bytes) {
            Ok(receipt) => receipt,
            Err(HostBundleError::ReinstallRequired)
                if receipt_schema_probe(&bytes).is_some_and(|probe| probe.host == Some(host)) =>
            {
                return Err(HostBundleError::ReinstallRequired);
            }
            Err(_) => continue,
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

pub fn latest_host_component_receipt_at(
    lifecycle_root: &Path,
    host: HostKindV1,
    component: HostComponentV1,
) -> Result<Option<HostBundleInstallReceiptV1>, HostBundleError> {
    read_receipt_at(lifecycle_root, host, component)
}

pub(super) fn read_receipt_at(
    root: &Path,
    host: HostKindV1,
    component: HostComponentV1,
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
    let receipt: HostBundleInstallReceiptV1 = parse_receipt(&bytes)?;
    validate_receipt(&receipt)?;
    if receipt.host != host || receipt.component != component {
        return Err(HostBundleError::ReceiptCorrupted);
    }
    Ok(Some(receipt))
}

#[derive(Deserialize)]
pub(super) struct ReceiptSchemaProbe {
    schema_version: u16,
    #[serde(default)]
    pub(super) host: Option<HostKindV1>,
}

impl ReceiptSchemaProbe {
    pub(super) fn is_current(&self) -> bool {
        self.schema_version == HOST_BUNDLE_RECEIPT_SCHEMA_VERSION
    }
}

pub(super) fn receipt_schema_probe(bytes: &[u8]) -> Option<ReceiptSchemaProbe> {
    serde_json::from_slice(bytes).ok()
}

/// Parse a receipt of the current schema. A receipt from an older schema is a
/// typed [`HostBundleError::ReinstallRequired`], never migrated in place.
pub(super) fn parse_receipt<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, HostBundleError> {
    let probe = receipt_schema_probe(bytes).ok_or(HostBundleError::ReceiptCorrupted)?;
    if !probe.is_current() {
        return Err(HostBundleError::ReinstallRequired);
    }
    serde_json::from_slice(bytes).map_err(|_| HostBundleError::ReceiptCorrupted)
}

pub(super) fn is_safe_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains('/')
        && !value.contains('\\')
}

pub(super) fn validate_receipt(
    receipt: &HostBundleInstallReceiptV1,
) -> Result<(), HostBundleError> {
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
            || artifact.ownership_marker
                != expected_ownership_marker(receipt.host, receipt.component)
            || receipt.artifacts[..index]
                .iter()
                .any(|existing| existing.relative_path == artifact.relative_path)
        {
            return Err(HostBundleError::ReceiptCorrupted);
        }
    }
    Ok(())
}

pub(super) fn validate_component_set_request(
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

pub(super) fn component_set_receipt_matches(
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
        });
        if !receipt_matches {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn component_set_receipt_matches_preview(
    receipt: &HostComponentSetReceiptV1,
    preview: &HostComponentSetLifecyclePreviewV1,
) -> bool {
    receipt.operation_id == preview.operation_id
        && receipt.confirmed_plan_digest == Some(preview.plan_digest)
        && receipt.base_registration_revision == Some(preview.base_registration_revision)
        && receipt.current_registration_revision == Some(preview.current_registration_revision)
        && receipt.artifact_state_revision == Some(preview.artifact_state_revision)
}

pub(super) fn validate_component_set_receipt(
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
            || receipt.component_receipts[..index]
                .iter()
                .any(|previous| previous.component == component_receipt.component)
        {
            return Err(HostBundleError::ReceiptCorrupted);
        }
    }
    Ok(())
}

pub(super) fn receipt_file(host: HostKindV1, component: HostComponentV1) -> String {
    format!(
        "receipt.{}.{}.v1.json",
        host.descriptor().slug(),
        component_slug(component)
    )
}

/// Host-scoped writer lock name.
///
/// Every host deploys its artifacts under its own disjoint subtree of the
/// artifact root (`.claude/…`, `.codex/…`, `.cursor/…`, `.config/opencode/…`,
/// `.kimi-code/…`, `.hermes/…`, `.kiro/…`, `.cline/…`, `.roo/…`,
/// `.config/kilo/…`), so one host's in-flight mutation shares no mutable path
/// with another's. `first_party_host_artifact_prefixes_are_disjoint` pins that
/// premise as a test.
pub(super) fn writer_lock_file(host: HostKindV1) -> String {
    format!("writer.{}.v1.lock", host.descriptor().slug())
}

pub(super) fn component_set_receipt_file(operation_id: [u8; 16]) -> String {
    format!(
        "component-set-receipt.{}.v1.json",
        hex::encode(operation_id)
    )
}

pub(super) fn receipt_identity_from_file_name(
    file_name: &str,
) -> Option<(HostKindV1, HostComponentV1)> {
    let components = [
        HostComponentV1::Core,
        HostComponentV1::Agent,
        HostComponentV1::ContextMcp,
        HostComponentV1::OperatorMcp,
    ];
    stock_host_kinds().into_iter().find_map(|host| {
        components
            .iter()
            .copied()
            .find(|component| receipt_file(host, *component) == file_name)
            .map(|component| (host, component))
    })
}

pub(super) fn expected_ownership_marker(host: HostKindV1, component: HostComponentV1) -> String {
    format!(
        "tracedecay.{}.{}.v1",
        host.descriptor().slug(),
        component_slug(component)
    )
}

pub(super) fn component_slug(component: HostComponentV1) -> &'static str {
    match component {
        HostComponentV1::Core => "core",
        HostComponentV1::Agent => "agent",
        HostComponentV1::ContextMcp => "context-mcp",
        HostComponentV1::OperatorMcp => "operator-mcp",
    }
}
