//! Layout of the `.tracedecay-host-bundle-v1` control directory: file names,
//! path-rooted receipt readers, and receipt/journal validators.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tracedecay_host_integration::host_bundle_storage_failure;

use super::model::{
    HostComponentSetEntryV1, HostComponentSetExecutionRequestV1,
    HostComponentSetLifecyclePreviewV1, HostComponentSetV1,
};
use super::planner::inspect_install_target;
use super::{
    HOST_BUNDLE_RECEIPT_SCHEMA_VERSION, HostBundleBackupReceiptV1, HostBundleComponentV1,
    HostBundleError, HostBundleInstallReceiptV1, HostBundleJournalV1, HostBundleLifecycleOpV1,
    HostBundleRestoreReceiptV1, HostBundleRollbackBoundaryV1, HostComponentSetJournalV1,
    HostComponentSetReceiptV1, HostKindV1, MAX_HOST_COMPONENTS, MAX_MANIFEST_ARTIFACTS,
    stock_host_kinds, validate_identifier, validate_relative_install_path,
};

pub(super) const HOST_BUNDLE_CONTROL_DIR: &str = ".tracedecay-host-bundle-v1";
pub(super) const HOST_BUNDLE_JOURNAL_FILE: &str = "journal.v1.json";
/// Legacy shared component-set journal name. One journal per lifecycle root
/// meant an interrupted transaction for any host blocked every other host.
/// Journals are host-scoped now; this name is still read (and retired) so a
/// journal left by an older binary is recovered rather than orphaned.
pub(super) const HOST_COMPONENT_SET_JOURNAL_FILE: &str = "component-set-journal.v1.json";
pub(super) const HOST_COMPONENT_SET_STAGE_DIR: &str = "component-set-staging";
/// Set-aside directory for journals an operator explicitly abandoned with
/// `tracedecay host-bundle recover --quarantine --yes`. Backups stay in place.
pub(super) const HOST_BUNDLE_QUARANTINE_DIR: &str = "quarantine";
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

pub(super) fn read_receipt_at(
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

pub(super) fn validate_backup_receipt(
    receipt: &HostBundleBackupReceiptV1,
) -> Result<(), HostBundleError> {
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

pub(super) fn validate_restore_receipt(
    receipt: &HostBundleRestoreReceiptV1,
) -> Result<(), HostBundleError> {
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

pub(super) fn validate_journal(journal: &HostBundleJournalV1) -> Result<(), HostBundleError> {
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
                && component_receipt.rollback_boundary == HostBundleRollbackBoundaryV1::Passed
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

pub(super) fn component_set_from_journal(
    journal: &HostComponentSetJournalV1,
) -> HostComponentSetV1 {
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

pub(super) fn validate_component_set_journal(
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

pub(super) fn backup_name(operation_id: [u8; 16], relative_path: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(operation_id);
    hasher.update(relative_path.as_bytes());
    format!("artifact-{}", hex::encode(hasher.finalize()))
}

pub(super) fn host_bundle_snapshot_name(index: usize, relative_path: &str) -> String {
    let digest = Sha256::digest(relative_path.as_bytes());
    format!("{index:03}-{}", hex::encode(&digest[..16]))
}

pub(super) fn host_bundle_backup_receipt_file(operation_id: [u8; 16]) -> String {
    format!("backup-receipt.{}.v1.json", hex::encode(operation_id))
}

pub(super) fn host_bundle_restore_receipt_file(operation_id: [u8; 16]) -> String {
    format!("restore-receipt.{}.v1.json", hex::encode(operation_id))
}

pub(super) fn component_set_stage_name(
    component: HostBundleComponentV1,
    relative_path: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(component_slug(component).as_bytes());
    hasher.update(relative_path.as_bytes());
    format!(
        "{}-{}",
        component_slug(component),
        hex::encode(hasher.finalize())
    )
}

pub(super) fn receipt_file(host: HostKindV1, component: HostBundleComponentV1) -> String {
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
pub(super) fn component_set_journal_file(host: HostKindV1) -> String {
    format!("component-set-journal.{}.v1.json", host.descriptor().slug())
}

pub(super) fn component_set_receipt_file(operation_id: [u8; 16]) -> String {
    format!(
        "component-set-receipt.{}.v1.json",
        hex::encode(operation_id)
    )
}

pub(super) fn receipt_identity_from_file_name(
    file_name: &str,
) -> Option<(HostKindV1, HostBundleComponentV1)> {
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

pub(super) fn expected_ownership_marker(
    host: HostKindV1,
    component: HostBundleComponentV1,
) -> String {
    format!(
        "tracedecay.{}.{}.v1",
        host.descriptor().slug(),
        component_slug(component)
    )
}

pub(super) fn component_slug(component: HostBundleComponentV1) -> &'static str {
    match component {
        HostBundleComponentV1::Core => "core",
        HostBundleComponentV1::Agent => "agent",
        HostBundleComponentV1::ContextMcp => "context-mcp",
        HostBundleComponentV1::OperatorMcp => "operator-mcp",
    }
}
