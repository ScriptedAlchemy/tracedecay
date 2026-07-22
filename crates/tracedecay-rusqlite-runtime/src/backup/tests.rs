mod support;

use super::{canonical::*, model::*, orchestrator::*, ports::*};
use crate::maintenance::ExclusiveMaintenancePermit;
use std::{cell::Cell, collections::BTreeSet};
use support::*;

fn backup(
    driver: &mut FakeDriver,
    filesystem: &mut FakeFilesystem,
    backup_set: BackupSetId,
) -> Result<StoredBackupManifest, BackupRestoreError> {
    let required = driver.snapshot.frozen_watermarks.clone();
    BackupRestoreOrchestrator::new(driver, filesystem).backup(
        &required,
        backup_set,
        &CancelAfter::never(),
    )
}

fn restore(
    driver: &mut FakeDriver,
    filesystem: &mut FakeFilesystem,
    backup_set: &BackupSetId,
    permit: ExclusiveMaintenancePermit,
    cancellation: &dyn Cancellation,
) -> Result<Vec<tracedecay_store::StoreRuntimeBindingV1>, BackupRestoreError> {
    BackupRestoreOrchestrator::new(driver, filesystem).restore(
        backup_set,
        permit,
        replacement_bindings(),
        cancellation,
    )
}

#[test]
fn rejects_incomplete_store_family_before_creating_staging() {
    let (mut driver, mut filesystem, backup_set) = setup();
    driver.snapshot.artifacts.retain(
        |item| !matches!(&item.identity, ArtifactIdentity::Store(id) if id == &project_shard()),
    );
    let error = backup(&mut driver, &mut filesystem, backup_set).unwrap_err();
    assert_eq!(
        error,
        BackupRestoreError::Manifest(ManifestError::IncompleteStoreFamily)
    );
    assert!(filesystem.staged.is_empty());
}

#[test]
fn rejects_incomplete_payload_closure() {
    let (mut driver, mut filesystem, backup_set) = setup();
    driver
        .snapshot
        .artifacts
        .retain(|item| !matches!(item.identity, ArtifactIdentity::Payload(_)));
    let error = backup(&mut driver, &mut filesystem, backup_set).unwrap_err();
    assert_eq!(
        error,
        BackupRestoreError::Manifest(ManifestError::IncompletePayloadClosure)
    );
}

#[test]
fn corrupted_staging_is_not_committed() {
    let (mut driver, mut filesystem, backup_set) = setup();
    filesystem.corrupt_staged_reads = true;
    let error = backup(&mut driver, &mut filesystem, backup_set.clone()).unwrap_err();
    assert!(matches!(
        error,
        BackupRestoreError::ArtifactDigestMismatch(_)
    ));
    assert!(!filesystem.backups.contains_key(&backup_set));
    assert_eq!(filesystem.aborted, 1);
}

#[test]
fn cancelled_backup_leaves_no_visible_backup() {
    let (mut driver, mut filesystem, backup_set) = setup();
    let required = driver.snapshot.frozen_watermarks.clone();
    let error = BackupRestoreOrchestrator::new(&mut driver, &mut filesystem)
        .backup(&required, backup_set.clone(), &CancelAfter::immediately())
        .unwrap_err();
    assert_eq!(error, BackupRestoreError::Cancelled);
    assert!(!filesystem.backups.contains_key(&backup_set));
}

#[test]
fn complete_backup_restores_all_shards_to_new_fences() {
    let (mut driver, mut filesystem, backup_set) = setup();
    let manifest = backup(&mut driver, &mut filesystem, backup_set.clone()).unwrap();
    assert_eq!(manifest.manifest.artifacts.len(), 3);
    let bindings = restore(
        &mut driver,
        &mut filesystem,
        &backup_set,
        maintenance_permit(),
        &CancelAfter::never(),
    )
    .unwrap();
    assert_eq!(bindings, replacement_bindings());
    assert_eq!(driver.published, 1);
}

#[test]
fn restore_rejects_corrupted_artifact() {
    let (mut driver, mut filesystem, backup_set) = setup();
    backup(&mut driver, &mut filesystem, backup_set.clone()).unwrap();
    filesystem.corrupt_backup_reads = true;
    let error = restore(
        &mut driver,
        &mut filesystem,
        &backup_set,
        maintenance_permit(),
        &CancelAfter::never(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        BackupRestoreError::ArtifactDigestMismatch(_)
    ));
    assert_eq!(driver.published, 0);
    assert_eq!(driver.abandoned, 1);
}

#[test]
fn restore_rejects_manifest_digest_mismatch() {
    let (mut driver, mut filesystem, backup_set) = setup();
    backup(&mut driver, &mut filesystem, backup_set.clone()).unwrap();
    filesystem
        .backups
        .get_mut(&backup_set)
        .unwrap()
        .0
        .manifest
        .schema_version = SchemaVersion(6);
    let error = restore(
        &mut driver,
        &mut filesystem,
        &backup_set,
        maintenance_permit(),
        &CancelAfter::never(),
    )
    .unwrap_err();
    assert_eq!(error, BackupRestoreError::ManifestDigestMismatch);
    assert_eq!(driver.published, 0);
}

#[test]
fn cancelled_restore_abandons_isolated_target_without_publication() {
    let (mut driver, mut filesystem, backup_set) = setup();
    backup(&mut driver, &mut filesystem, backup_set.clone()).unwrap();
    let error = restore(
        &mut driver,
        &mut filesystem,
        &backup_set,
        maintenance_permit(),
        &CancelAfter {
            checks_remaining: Cell::new(1),
        },
    )
    .unwrap_err();
    assert_eq!(error, BackupRestoreError::Cancelled);
    assert_eq!(driver.published, 0);
    assert_eq!(driver.abandoned, 1);
}

#[test]
fn publication_error_retains_recovery_evidence_instead_of_fallback() {
    let (mut driver, mut filesystem, backup_set) = setup();
    backup(&mut driver, &mut filesystem, backup_set.clone()).unwrap();
    driver.fail_publication_ack = true;
    let error = restore(
        &mut driver,
        &mut filesystem,
        &backup_set,
        maintenance_permit(),
        &CancelAfter::never(),
    )
    .unwrap_err();
    assert!(matches!(error, BackupRestoreError::Driver(_)));
    assert_eq!(driver.published, 1);
    assert_eq!(driver.abandoned, 0);
}

#[test]
fn stable_manifest_is_independent_of_artifact_order() {
    let snapshot = family_snapshot();
    let artifacts = snapshot
        .artifacts
        .iter()
        .map(|item| ArtifactManifest {
            identity: item.identity.clone(),
            byte_length: item.bytes.len() as u64,
            sha256: sha256(&item.bytes),
        })
        .collect::<Vec<_>>();
    let mut reversed = artifacts.clone();
    reversed.reverse();
    let make = |artifacts| BackupManifest {
        format_version: BACKUP_FORMAT_VERSION,
        backup_set: BackupSetId("backup-1".into()),
        frozen_watermarks: snapshot.frozen_watermarks.clone(),
        schema_version: snapshot.schema_version,
        privacy: snapshot.privacy,
        deletion: snapshot.deletion,
        payload_closure: BTreeSet::from([PayloadId("payload:01".into())]),
        artifacts,
    };
    let stable = make(artifacts).stable_bytes();
    assert!(stable.starts_with(b"tracedecay.backup-manifest.v2\n{"));
    assert_eq!(stable, make(reversed).stable_bytes());
}

#[test]
fn stable_manifest_matches_v2_golden_bytes() {
    let snapshot = family_snapshot();
    let manifest = BackupManifest {
        format_version: BACKUP_FORMAT_VERSION,
        backup_set: BackupSetId::new("backup-golden").unwrap(),
        frozen_watermarks: snapshot.frozen_watermarks,
        schema_version: snapshot.schema_version,
        privacy: snapshot.privacy,
        deletion: snapshot.deletion,
        payload_closure: snapshot.payload_closure,
        artifacts: snapshot
            .artifacts
            .iter()
            .map(|artifact| ArtifactManifest {
                identity: artifact.identity.clone(),
                byte_length: artifact.bytes.len() as u64,
                sha256: sha256(&artifact.bytes),
            })
            .collect(),
    };

    assert_eq!(
        manifest.stable_bytes(),
        include_bytes!("tests/golden/manifest-v2.bytes")
    );
}

#[test]
fn sha256_uses_the_standard_digest() {
    let digest = sha256(b"Hi There");
    assert_eq!(
        hex(&digest.0),
        "cc6d5896d770101ef0280c943a2d3c3f24cd5b11464a5186daf7a238477162ac"
    );
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;

    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(output, "{byte:02x}").unwrap();
    }
    output
}

#[test]
fn restore_rejects_replacement_that_is_not_higher_for_every_shard() {
    let (mut driver, mut filesystem, backup_set) = setup();
    backup(&mut driver, &mut filesystem, backup_set.clone()).unwrap();
    let mut replacements = replacement_bindings();
    replacements[0].authority_epoch = epoch(19);
    let error = BackupRestoreOrchestrator::new(&mut driver, &mut filesystem)
        .restore(
            &backup_set,
            maintenance_permit(),
            replacements,
            &CancelAfter::never(),
        )
        .unwrap_err();
    assert_eq!(error, BackupRestoreError::RestoreTargetNotNewer);
    assert_eq!(driver.published, 0);
}

#[test]
fn restore_rejects_driver_substitution_of_canonical_replacement() {
    let (mut driver, mut filesystem, backup_set) = setup();
    backup(&mut driver, &mut filesystem, backup_set.clone()).unwrap();
    driver.substitute_replacement = true;

    let error = restore(
        &mut driver,
        &mut filesystem,
        &backup_set,
        maintenance_permit(),
        &CancelAfter::never(),
    )
    .unwrap_err();

    assert_eq!(error, BackupRestoreError::RestoreTargetMismatch);
    assert_eq!(driver.published, 0);
    assert_eq!(driver.abandoned, 1);
}
