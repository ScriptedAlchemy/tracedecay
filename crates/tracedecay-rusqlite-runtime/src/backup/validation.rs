use std::collections::{BTreeMap, BTreeSet};

use tracedecay_store::{FrozenWatermarkVectorV1, ShardWatermarkV1, StoreShardIdV1};

use super::{canonical::sha256, model::*, orchestrator::BackupRestoreError};

pub(super) fn validate_snapshot(snapshot: &FrozenFamilySnapshot) -> Result<(), BackupRestoreError> {
    validate_artifact_closure(
        snapshot.frozen_watermarks.iter().map(|(shard, _)| shard),
        &snapshot.payload_closure,
        snapshot.artifacts.iter().map(|artifact| &artifact.identity),
    )
    .map_err(BackupRestoreError::Manifest)
}

pub(super) fn validate_manifest(manifest: &BackupManifest) -> Result<(), BackupRestoreError> {
    if manifest.format_version != BACKUP_FORMAT_VERSION {
        return Err(BackupRestoreError::Manifest(
            ManifestError::UnsupportedFormat,
        ));
    }
    validate_artifact_closure(
        manifest.frozen_watermarks.iter().map(|(shard, _)| shard),
        &manifest.payload_closure,
        manifest.artifacts.iter().map(|artifact| &artifact.identity),
    )
    .map_err(BackupRestoreError::Manifest)
}

fn validate_artifact_closure<'a>(
    required_shards: impl IntoIterator<Item = &'a StoreShardIdV1>,
    payload_closure: &BTreeSet<PayloadId>,
    artifacts: impl IntoIterator<Item = &'a ArtifactIdentity>,
) -> Result<(), ManifestError> {
    let required_shards = required_shards
        .into_iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut members = BTreeSet::new();
    let mut payloads = BTreeSet::new();
    let mut identities = BTreeSet::new();
    for identity in artifacts {
        if !identities.insert(identity.clone()) {
            return Err(ManifestError::DuplicateArtifact);
        }
        match identity {
            ArtifactIdentity::Store(id) => {
                members.insert(id.clone());
            }
            ArtifactIdentity::Payload(id) => {
                payloads.insert(id.clone());
            }
        }
    }
    if !members.is_subset(&required_shards) || !payloads.is_subset(payload_closure) {
        return Err(ManifestError::UnexpectedArtifact);
    }
    if members != required_shards {
        return Err(ManifestError::IncompleteStoreFamily);
    }
    if payloads != *payload_closure {
        return Err(ManifestError::IncompletePayloadClosure);
    }
    Ok(())
}

pub(super) fn verify_restore_evidence(
    evidence: &FrozenFamilySnapshot,
    manifest: &BackupManifest,
    target: &RestoreTarget,
) -> Result<(), BackupRestoreError> {
    let expected_watermarks = restored_watermarks(&manifest.frozen_watermarks, target)?;
    if evidence.frozen_watermarks != expected_watermarks
        || evidence.schema_version != manifest.schema_version
        || evidence.privacy != manifest.privacy
        || evidence.deletion != manifest.deletion
        || evidence.payload_closure != manifest.payload_closure
    {
        return Err(BackupRestoreError::RestoreVerificationMismatch);
    }
    validate_snapshot(evidence)?;
    let actual = evidence
        .artifacts
        .iter()
        .map(|item| {
            (
                item.identity.clone(),
                item.bytes.len() as u64,
                sha256(&item.bytes),
            )
        })
        .collect::<BTreeSet<_>>();
    let expected = manifest
        .artifacts
        .iter()
        .map(|item| (item.identity.clone(), item.byte_length, item.sha256))
        .collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(BackupRestoreError::RestoreVerificationMismatch);
    }
    Ok(())
}

pub(super) fn validate_replacement_bindings(
    source: &FrozenWatermarkVectorV1,
    target: &RestoreTarget,
) -> Result<(), BackupRestoreError> {
    restored_watermarks(source, target).map(|_| ())
}

fn restored_watermarks(
    source: &FrozenWatermarkVectorV1,
    target: &RestoreTarget,
) -> Result<FrozenWatermarkVectorV1, BackupRestoreError> {
    let replacements = target
        .replacement_bindings
        .iter()
        .map(|binding| (binding.shard_id.clone(), binding))
        .collect::<BTreeMap<_, _>>();
    if replacements.len() != target.replacement_bindings.len()
        || replacements.len() != source.iter().count()
    {
        return Err(BackupRestoreError::RestoreTargetNotNewer);
    }
    let watermarks = source
        .iter()
        .map(|(shard_id, watermark)| {
            let replacement = replacements
                .get(shard_id)
                .ok_or(BackupRestoreError::RestoreTargetNotNewer)?;
            if replacement.incarnation <= watermark.incarnation
                || replacement.authority_epoch <= watermark.authority_epoch
            {
                return Err(BackupRestoreError::RestoreTargetNotNewer);
            }
            Ok(ShardWatermarkV1 {
                shard_id: shard_id.clone(),
                incarnation: replacement.incarnation,
                authority_epoch: replacement.authority_epoch,
                commit_sequence: watermark.commit_sequence,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    FrozenWatermarkVectorV1::new(watermarks).map_err(|_| BackupRestoreError::RestoreTargetNotNewer)
}
