use super::super::{model::*, ports::*};
use crate::maintenance::{ExclusiveMaintenancePermit, MaintenanceOwnerId};
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};
use tracedecay_store::{
    BrainId, CommitSequenceV1, FrozenWatermarkVectorV1, ProjectId, ShardWatermarkV1,
    StoreAuthorityEpochV1, StoreIncarnationV1, StoreRuntimeBindingV1, StoreShardIdV1,
    UserProfileId,
};

#[derive(Debug)]
pub(super) struct FakeError(&'static str);
impl fmt::Display for FakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl Error for FakeError {}

pub(super) struct CancelAfter {
    pub(super) checks_remaining: Cell<usize>,
}
impl CancelAfter {
    pub(super) fn never() -> Self {
        Self {
            checks_remaining: Cell::new(usize::MAX),
        }
    }
    pub(super) fn immediately() -> Self {
        Self {
            checks_remaining: Cell::new(0),
        }
    }
}
impl Cancellation for CancelAfter {
    fn is_cancelled(&self) -> bool {
        let remaining = self.checks_remaining.get();
        if remaining == 0 {
            true
        } else {
            self.checks_remaining.set(remaining - 1);
            false
        }
    }
}

#[derive(Default)]
pub(super) struct FakeFilesystem {
    next_stage: u64,
    pub(super) staged: BTreeMap<StagingId, BTreeMap<ArtifactIdentity, Vec<u8>>>,
    staged_manifests: BTreeMap<StagingId, StoredBackupManifest>,
    pub(super) backups:
        BTreeMap<BackupSetId, (StoredBackupManifest, BTreeMap<ArtifactIdentity, Vec<u8>>)>,
    pub(super) corrupt_staged_reads: bool,
    pub(super) corrupt_backup_reads: bool,
    pub(super) aborted: usize,
}

impl BackupFilesystem for FakeFilesystem {
    type Error = FakeError;
    fn begin_backup(&mut self, _: &BackupSetId) -> Result<StagingId, Self::Error> {
        self.next_stage += 1;
        let id = StagingId(format!("stage-{}", self.next_stage));
        self.staged.insert(id.clone(), BTreeMap::new());
        Ok(id)
    }
    fn write_staged(
        &mut self,
        stage: &StagingId,
        id: &ArtifactIdentity,
        bytes: &[u8],
    ) -> Result<(), Self::Error> {
        self.staged
            .entry(stage.clone())
            .or_default()
            .insert(id.clone(), bytes.to_vec());
        Ok(())
    }
    fn read_staged(
        &self,
        stage: &StagingId,
        id: &ArtifactIdentity,
    ) -> Result<Vec<u8>, Self::Error> {
        let mut bytes = self
            .staged
            .get(stage)
            .and_then(|items| items.get(id))
            .cloned()
            .ok_or(FakeError("missing staged"))?;
        if self.corrupt_staged_reads {
            bytes.push(0xff);
        }
        Ok(bytes)
    }
    fn write_manifest(
        &mut self,
        stage: &StagingId,
        manifest: &StoredBackupManifest,
    ) -> Result<(), Self::Error> {
        self.staged_manifests
            .insert(stage.clone(), manifest.clone());
        Ok(())
    }
    fn read_staged_manifest(&self, stage: &StagingId) -> Result<StoredBackupManifest, Self::Error> {
        self.staged_manifests
            .get(stage)
            .cloned()
            .ok_or(FakeError("missing staged manifest"))
    }
    fn commit_backup(&mut self, stage: StagingId, backup: &BackupSetId) -> Result<(), Self::Error> {
        let manifest = self
            .staged_manifests
            .remove(&stage)
            .ok_or(FakeError("missing manifest"))?;
        let files = self
            .staged
            .remove(&stage)
            .ok_or(FakeError("missing stage"))?;
        self.backups.insert(backup.clone(), (manifest, files));
        Ok(())
    }
    fn abort_staging(&mut self, stage: StagingId) {
        self.staged.remove(&stage);
        self.staged_manifests.remove(&stage);
        self.aborted += 1;
    }
    fn load_manifest(&self, backup: &BackupSetId) -> Result<StoredBackupManifest, Self::Error> {
        self.backups
            .get(backup)
            .map(|item| item.0.clone())
            .ok_or(FakeError("missing backup"))
    }
    fn read_backup(
        &self,
        backup: &BackupSetId,
        id: &ArtifactIdentity,
    ) -> Result<Vec<u8>, Self::Error> {
        let mut bytes = self
            .backups
            .get(backup)
            .and_then(|item| item.1.get(id))
            .cloned()
            .ok_or(FakeError("missing artifact"))?;
        if self.corrupt_backup_reads {
            bytes.push(0xee);
        }
        Ok(bytes)
    }
}

pub(super) struct FakeDriver {
    pub(super) snapshot: FrozenFamilySnapshot,
    pub(super) published: usize,
    pub(super) abandoned: usize,
    pub(super) fail_publication_ack: bool,
    pub(super) substitute_replacement: bool,
}

impl BackupDriver for FakeDriver {
    type Error = FakeError;
    fn freeze_families(
        &mut self,
        _: &FrozenWatermarkVectorV1,
        _: &dyn Cancellation,
    ) -> Result<FrozenFamilySnapshot, Self::Error> {
        Ok(self.snapshot.clone())
    }
    fn allocate_restore_target(
        &mut self,
        _: &ExclusiveMaintenancePermit,
        mut replacement_bindings: Vec<StoreRuntimeBindingV1>,
    ) -> Result<RestoreTarget, Self::Error> {
        if self.substitute_replacement {
            replacement_bindings[0].authority_epoch = epoch(21);
        }
        Ok(RestoreTarget {
            replacement_bindings,
            staging: StagingId("restore-stage".into()),
        })
    }
    fn verify_restore(
        &mut self,
        _: &ExclusiveMaintenancePermit,
        target: &RestoreTarget,
        _: &BackupManifest,
    ) -> Result<FrozenFamilySnapshot, Self::Error> {
        let replacements = target
            .replacement_bindings
            .iter()
            .map(|binding| (&binding.shard_id, binding))
            .collect::<BTreeMap<_, _>>();
        let watermarks = self
            .snapshot
            .frozen_watermarks
            .iter()
            .map(|(shard, source)| {
                let replacement = replacements[shard];
                ShardWatermarkV1 {
                    shard_id: shard.clone(),
                    incarnation: replacement.incarnation,
                    authority_epoch: replacement.authority_epoch,
                    commit_sequence: source.commit_sequence,
                }
            });
        let mut evidence = self.snapshot.clone();
        evidence.frozen_watermarks = FrozenWatermarkVectorV1::new(watermarks).unwrap();
        Ok(evidence)
    }
    fn publish_restore(
        &mut self,
        _: ExclusiveMaintenancePermit,
        _: &FrozenWatermarkVectorV1,
        _: RestoreTarget,
    ) -> Result<(), Self::Error> {
        self.published += 1;
        if self.fail_publication_ack {
            return Err(FakeError("publication acknowledgement lost"));
        }
        Ok(())
    }
    fn abandon_restore(&mut self, _: &ExclusiveMaintenancePermit, _: RestoreTarget) {
        self.abandoned += 1;
    }
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}
pub(super) fn incarnation(value: u64) -> StoreIncarnationV1 {
    StoreIncarnationV1::new(value).unwrap()
}
pub(super) fn epoch(value: u64) -> StoreAuthorityEpochV1 {
    StoreAuthorityEpochV1::new(value).unwrap()
}
fn brain() -> BrainId {
    id("brain.backup")
}
fn profile() -> UserProfileId {
    id("profile.backup")
}
fn project() -> ProjectId {
    id("project.backup")
}
pub(super) fn project_shard() -> StoreShardIdV1 {
    StoreShardIdV1::project(brain(), profile(), project())
}
fn sessions_shard() -> StoreShardIdV1 {
    StoreShardIdV1::project_sessions(brain(), profile(), project())
}

pub(super) fn family_snapshot() -> FrozenFamilySnapshot {
    let project = project_shard();
    let sessions = sessions_shard();
    let watermark = |shard_id| ShardWatermarkV1 {
        shard_id,
        incarnation: incarnation(7),
        authority_epoch: epoch(19),
        commit_sequence: CommitSequenceV1(47),
    };
    let payload = PayloadId("payload:01".into());
    FrozenFamilySnapshot {
        frozen_watermarks: FrozenWatermarkVectorV1::new([
            watermark(project.clone()),
            watermark(sessions.clone()),
        ])
        .unwrap(),
        schema_version: SchemaVersion(5),
        privacy: PrivacyClass::Project,
        deletion: DeletionState::Live,
        payload_closure: BTreeSet::from([payload.clone()]),
        artifacts: vec![
            SnapshotArtifact {
                identity: ArtifactIdentity::Payload(payload),
                bytes: b"payload".to_vec(),
            },
            SnapshotArtifact {
                identity: ArtifactIdentity::Store(sessions),
                bytes: b"sessions-db".to_vec(),
            },
            SnapshotArtifact {
                identity: ArtifactIdentity::Store(project),
                bytes: b"project-db".to_vec(),
            },
        ],
    }
}

pub(super) fn replacement_bindings() -> Vec<StoreRuntimeBindingV1> {
    vec![project_shard(), sessions_shard()]
        .into_iter()
        .map(|shard| StoreRuntimeBindingV1::new(shard, incarnation(8), epoch(20)))
        .collect()
}

pub(super) fn maintenance_permit() -> ExclusiveMaintenancePermit {
    ExclusiveMaintenancePermit::issue(
        MaintenanceOwnerId::new(1).unwrap(),
        StoreRuntimeBindingV1::new(project_shard(), incarnation(7), epoch(19)),
    )
}

pub(super) fn setup() -> (FakeDriver, FakeFilesystem, BackupSetId) {
    (
        FakeDriver {
            snapshot: family_snapshot(),
            published: 0,
            abandoned: 0,
            fail_publication_ack: false,
            substitute_replacement: false,
        },
        FakeFilesystem::default(),
        BackupSetId("backup-1".into()),
    )
}
