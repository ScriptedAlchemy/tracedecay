use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};

use serde::{Deserialize, Serialize};

use super::*;

mod fixtures;
use fixtures::*;

const LAST: Digest = Digest([1; 32]);
const FINAL: Digest = Digest([2; 32]);
const EVIDENCE: Digest = Digest([3; 32]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failpoint {
    TransformAfterCommitOnce,
    PublishAfterCommitOnce,
}

#[derive(Debug)]
struct FakeError(&'static str);

impl fmt::Display for FakeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl Error for FakeError {}

struct FakeTransform {
    family: StoreFamily,
    applications: usize,
}

impl FamilyTransform for FakeTransform {
    type Error = FakeError;

    fn family(&self) -> StoreFamily {
        self.family
    }

    fn apply(&mut self, _: &StagingHandle, _: &FamilyTransformPlan) -> Result<(), Self::Error> {
        self.applications += 1;
        Ok(())
    }
}

struct FakePort {
    preflight: PreflightReport,
    verification: VerificationReport,
    checkpoint: Option<DestinationCheckpoint>,
    staging: Option<StagingHandle>,
    publication: Option<PublicationReceipt>,
    failpoint: Option<Failpoint>,
    corrupt_verified_checkpoint_identity: bool,
    publication_verification_digest: Option<Digest>,
    publication_backup: Option<BackupReceipt>,
    backup_count: usize,
    transform_attempts: usize,
}

#[derive(Serialize, Deserialize)]
struct DurablePortState {
    checkpoint: Option<DestinationCheckpoint>,
    staging: Option<StagingHandle>,
    publication: Option<PublicationReceipt>,
}

impl FakePort {
    fn new(request: &MigrationRequest) -> Self {
        let families = request
            .source_bindings
            .iter()
            .map(|(family, bindings)| {
                (
                    *family,
                    FamilyPreflight {
                        family: *family,
                        bindings: bindings.clone(),
                        observed_schema: ObservedSchema::LastReleased(family_digest(
                            *family, false,
                        )),
                    },
                )
            })
            .collect();
        Self {
            preflight: PreflightReport { families },
            verification: Self::verification(),
            checkpoint: None,
            staging: None,
            publication: None,
            failpoint: None,
            corrupt_verified_checkpoint_identity: false,
            publication_verification_digest: None,
            publication_backup: None,
            backup_count: 0,
            transform_attempts: 0,
        }
    }

    fn serialize_durable_state(&self) -> Vec<u8> {
        serde_json::to_vec(&DurablePortState {
            checkpoint: self.checkpoint.clone(),
            staging: self.staging.clone(),
            publication: self.publication.clone(),
        })
        .unwrap()
    }

    fn from_serialized(request: &MigrationRequest, encoded: &[u8]) -> Self {
        let state: DurablePortState = serde_json::from_slice(encoded).unwrap();
        Self {
            checkpoint: state.checkpoint,
            staging: state.staging,
            publication: state.publication,
            ..Self::new(request)
        }
    }

    fn backup(request: &MigrationRequest) -> BackupReceipt {
        BackupReceipt {
            backup_id: BackupId::new("backup.one").unwrap(),
            source_manifest_digest: LAST,
            artifact_digest: request.freeze_proof.proof_digest,
        }
    }

    fn verification() -> VerificationReport {
        let exact = FamilyVerification {
            source_count: 1,
            destination_count: 1,
            source_ids: BTreeSet::from([RecordId::new("record.one").unwrap()]),
            destination_ids: BTreeSet::from([RecordId::new("record.one").unwrap()]),
            source_ids_digest: EVIDENCE,
            destination_ids_digest: EVIDENCE,
            source_content_digest: EVIDENCE,
            destination_content_digest: EVIDENCE,
            source_fts_count: 2,
            destination_fts_count: 2,
            source_fts_digest: EVIDENCE,
            destination_fts_digest: EVIDENCE,
            source_payload_count: 1,
            destination_payload_count: 1,
            source_payload_ids: BTreeSet::from([PayloadId::new("payload.one").unwrap()]),
            destination_payload_ids: BTreeSet::from([PayloadId::new("payload.one").unwrap()]),
            source_payload_digest: EVIDENCE,
            destination_payload_digest: EVIDENCE,
            source_deletion_digest: EVIDENCE,
            destination_deletion_digest: EVIDENCE,
            source_deleted_ids: BTreeSet::from([RecordId::new("record.deleted").unwrap()]),
            destination_deleted_ids: BTreeSet::from([RecordId::new("record.deleted").unwrap()]),
            source_quarantine_digest: EVIDENCE,
            destination_quarantine_digest: EVIDENCE,
            source_quarantine_ids: BTreeSet::from([QuarantineId::new("quarantine.one").unwrap()]),
            destination_quarantine_ids: BTreeSet::from([
                QuarantineId::new("quarantine.one").unwrap()
            ]),
        };
        VerificationReport {
            destination_manifest_digest: FINAL,
            families: all_families()
                .into_iter()
                .map(|family| (family, exact.clone()))
                .collect(),
            integrity_check_digest: EVIDENCE,
        }
    }
}

impl ConsolidatedMigrationPort for FakePort {
    type Error = FakeError;

    fn verify_release_freeze(&mut self, proof: &ReleaseFreezeProof) -> Result<bool, Self::Error> {
        Ok(
            proof.acceptance_id == FreezeAcceptanceId::new("release.accepted").unwrap()
                && proof.proof_digest == EVIDENCE,
        )
    }

    fn preflight(&mut self, _: &MigrationRequest) -> Result<PreflightReport, Self::Error> {
        Ok(self.preflight.clone())
    }

    fn lookup_publication(
        &mut self,
        _: &MigrationId,
    ) -> Result<Option<PublicationReceipt>, Self::Error> {
        Ok(self.publication.clone())
    }

    fn create_isolated_backup(
        &mut self,
        request: &MigrationRequest,
        _: &LastReleasedSchemaManifest,
    ) -> Result<BackupReceipt, Self::Error> {
        self.backup_count += 1;
        Ok(Self::backup(request))
    }

    fn find_staging(&mut self, _: &MigrationId) -> Result<Option<StagingHandle>, Self::Error> {
        Ok(self.staging.clone())
    }

    fn create_isolated_staging(
        &mut self,
        request: &MigrationRequest,
        _: &FinalSchemaManifest,
        backup: &BackupReceipt,
    ) -> Result<StagingHandle, Self::Error> {
        let staging = StagingHandle {
            staging_id: StagingId::new("stage.one").unwrap(),
            migration_id: request.migration_id.clone(),
            destination_epoch: request.destination_epoch,
        };
        self.staging = Some(staging.clone());
        self.checkpoint = Some(DestinationCheckpoint::Staged {
            migration_id: staging.migration_id.clone(),
            staging_id: staging.staging_id.clone(),
            destination_epoch: staging.destination_epoch,
            backup: backup.clone(),
            final_manifest_digest: FINAL,
        });
        Ok(staging)
    }

    fn load_destination_checkpoint(
        &mut self,
        _: &StagingHandle,
    ) -> Result<DestinationCheckpoint, Self::Error> {
        self.checkpoint
            .clone()
            .ok_or(FakeError("checkpoint missing"))
    }

    fn transform_and_checkpoint<T: FamilyTransform<Error = Self::Error>>(
        &mut self,
        staging: &StagingHandle,
        transform: &mut T,
        plan: &FamilyTransformPlan,
        completed: &[StoreFamily],
    ) -> Result<DestinationCheckpoint, Self::Error> {
        self.transform_attempts += 1;
        transform.apply(staging, plan)?;
        let checkpoint = DestinationCheckpoint::Transforming {
            migration_id: staging.migration_id.clone(),
            staging_id: staging.staging_id.clone(),
            destination_epoch: staging.destination_epoch,
            backup: self.checkpoint.as_ref().unwrap().backup().clone(),
            final_manifest_digest: FINAL,
            completed: completed.to_vec(),
        };
        self.checkpoint = Some(checkpoint.clone());
        if self.failpoint == Some(Failpoint::TransformAfterCommitOnce) {
            self.failpoint = None;
            return Err(FakeError("lost transform commit acknowledgement"));
        }
        Ok(checkpoint)
    }

    fn verify_staging(
        &mut self,
        _: &StagingHandle,
        _: &LastReleasedSchemaManifest,
        _: &FinalSchemaManifest,
    ) -> Result<VerificationReport, Self::Error> {
        Ok(self.verification.clone())
    }

    fn checkpoint_verification(
        &mut self,
        _: &StagingHandle,
        backup: &BackupReceipt,
        report: &VerificationReport,
    ) -> Result<DestinationCheckpoint, Self::Error> {
        let staging = self.staging.as_ref().unwrap();
        let checkpoint = DestinationCheckpoint::Verified {
            migration_id: staging.migration_id.clone(),
            staging_id: if self.corrupt_verified_checkpoint_identity {
                StagingId::new("stage.foreign").unwrap()
            } else {
                staging.staging_id.clone()
            },
            destination_epoch: staging.destination_epoch,
            backup: backup.clone(),
            final_manifest_digest: FINAL,
            verification_digest: report.integrity_check_digest,
        };
        self.checkpoint = Some(checkpoint.clone());
        Ok(checkpoint)
    }

    fn publish_atomically(
        &mut self,
        _: StagingHandle,
        request: &MigrationRequest,
        _: &FinalSchemaManifest,
        checkpoint: &DestinationCheckpoint,
    ) -> Result<PublicationReceipt, Self::Error> {
        let verification_digest = match checkpoint {
            DestinationCheckpoint::Verified {
                verification_digest,
                ..
            } => *verification_digest,
            _ => return Err(FakeError("unverified publication")),
        };
        let receipt = PublicationReceipt {
            publication_id: PublicationId::new("publication.one").unwrap(),
            migration_id: request.migration_id.clone(),
            final_manifest_digest: FINAL,
            destination_epoch: request.destination_epoch,
            verification_digest: self
                .publication_verification_digest
                .unwrap_or(verification_digest),
            backup: self
                .publication_backup
                .clone()
                .unwrap_or_else(|| checkpoint.backup().clone()),
        };
        self.publication = Some(receipt.clone());
        if self.failpoint == Some(Failpoint::PublishAfterCommitOnce) {
            self.failpoint = None;
            return Err(FakeError("lost publication acknowledgement"));
        }
        Ok(receipt)
    }
}

#[test]
fn serialized_checkpoint_resumes_after_transform_commit_acknowledgement_is_lost() {
    let request = request();
    let mut port = FakePort::new(&request);
    port.failpoint = Some(Failpoint::TransformAfterCommitOnce);
    let mut first_engine = engine(port);
    let mut transforms = family_transforms();

    assert!(matches!(
        first_engine.migrate(&request, &mut transforms),
        Err(MigrationError::Port {
            stage: MigrationStage::Transform(_),
            ..
        })
    ));

    let encoded = first_engine.into_port().serialize_durable_state();
    let mut restarted = engine(FakePort::from_serialized(&request, &encoded));
    let mut restarted_transforms = family_transforms();
    assert!(matches!(
        restarted.migrate(&request, &mut restarted_transforms),
        Ok(MigrationOutcome::Published(_))
    ));

    assert_eq!(
        restarted_transforms[&StoreFamily::Profile].applications,
        0,
        "the transform committed before the lost acknowledgement must not replay"
    );
    assert_eq!(restarted.into_port().backup_count, 0);
}

#[test]
fn uncertain_atomic_publication_is_resolved_and_replay_is_idempotent() {
    let request = request();
    let mut port = FakePort::new(&request);
    port.failpoint = Some(Failpoint::PublishAfterCommitOnce);
    let mut engine = engine(port);
    let mut transforms = family_transforms();

    assert!(matches!(
        engine.migrate(&request, &mut transforms),
        Ok(MigrationOutcome::Published(_))
    ));
    let attempts = transforms
        .values()
        .map(|transform| transform.applications)
        .sum::<usize>();
    assert!(matches!(
        engine.migrate(&request, &mut transforms),
        Ok(MigrationOutcome::Replayed(_))
    ));
    assert_eq!(
        transforms
            .values()
            .map(|transform| transform.applications)
            .sum::<usize>(),
        attempts
    );
}

#[test]
fn unknown_and_corrupt_families_fail_closed_before_backup() {
    for observed in [
        ObservedSchema::Unknown {
            revision: Some(99),
            digest: EVIDENCE,
        },
        ObservedSchema::Corrupt,
    ] {
        let request = request();
        let mut port = FakePort::new(&request);
        port.preflight
            .families
            .get_mut(&StoreFamily::Code)
            .unwrap()
            .observed_schema = observed;
        let mut engine = engine(port);
        let error = engine
            .migrate(&request, &mut family_transforms())
            .unwrap_err();
        assert!(matches!(
            error,
            MigrationError::UnknownFamily(StoreFamily::Code)
                | MigrationError::CorruptFamily(StoreFamily::Code)
        ));
        assert_eq!(engine.into_port().backup_count, 0);
    }
}

#[test]
fn freeze_proof_and_exact_verification_are_mandatory() {
    let mut bad_request = request();
    bad_request.freeze_proof.proof_digest = Digest([9; 32]);
    let port = FakePort::new(&bad_request);
    assert!(matches!(
        engine(port).migrate(&bad_request, &mut family_transforms()),
        Err(MigrationError::FreezeProofRejected)
    ));

    let mut report = FakePort::verification();
    report
        .families
        .get_mut(&StoreFamily::Profile)
        .unwrap()
        .destination_payload_count = 0;
    assert!(!report.families[&StoreFamily::Profile].is_exact());

    let request = request();
    let mut port = FakePort::new(&request);
    port.verification = report;
    assert!(matches!(
        engine(port).migrate(&request, &mut family_transforms()),
        Err(MigrationError::VerificationFailed(StoreFamily::Profile))
    ));
}

#[test]
fn family_verification_rejects_counts_inconsistent_with_exact_id_sets() {
    let mut report = FakePort::verification();
    let profile = report.families.get_mut(&StoreFamily::Profile).unwrap();
    profile.source_count += 1;
    assert!(!profile.is_exact());

    let mut report = FakePort::verification();
    let profile = report.families.get_mut(&StoreFamily::Profile).unwrap();
    profile.destination_payload_count += 1;
    assert!(!profile.is_exact());
}

#[test]
fn project_scoped_bindings_must_share_one_project_id() {
    for family in [StoreFamily::ProjectSessions, StoreFamily::Code] {
        let mut request = request();
        let binding = request
            .source_bindings
            .get_mut(&family)
            .unwrap()
            .first_mut()
            .unwrap();
        binding.shard_id.scope = match family {
            StoreFamily::ProjectSessions => serde_json::from_value(serde_json::json!({
                "kind": "project_sessions",
                "project_id": "project.foreign"
            }))
            .unwrap(),
            StoreFamily::Code => serde_json::from_value(serde_json::json!({
                "kind": "code",
                "project_id": "project.foreign",
                "repository_id": "repository.migration",
                "scope": { "kind": "worktree", "worktree_id": "worktree.migration" }
            }))
            .unwrap(),
            _ => unreachable!(),
        };

        let port = FakePort::new(&request);
        assert!(matches!(
            engine(port).migrate(&request, &mut family_transforms()),
            Err(MigrationError::BindingMismatch(rejected)) if rejected == family
        ));
    }
}

#[test]
fn checkpoint_verification_result_is_validated_before_publication() {
    let request = request();
    let mut port = FakePort::new(&request);
    port.corrupt_verified_checkpoint_identity = true;

    assert!(matches!(
        engine(port).migrate(&request, &mut family_transforms()),
        Err(MigrationError::CheckpointMismatch)
    ));
}

#[test]
fn resumed_verified_checkpoint_requires_fresh_matching_evidence() {
    let request = request();
    let mut port = FakePort::new(&request);
    let backup = FakePort::backup(&request);
    let staging = StagingHandle {
        staging_id: StagingId::new("stage.one").unwrap(),
        migration_id: request.migration_id.clone(),
        destination_epoch: request.destination_epoch,
    };
    port.staging = Some(staging.clone());
    port.checkpoint = Some(DestinationCheckpoint::Verified {
        migration_id: staging.migration_id.clone(),
        staging_id: staging.staging_id.clone(),
        destination_epoch: staging.destination_epoch,
        backup,
        final_manifest_digest: FINAL,
        verification_digest: EVIDENCE,
    });
    port.verification.integrity_check_digest = Digest([9; 32]);

    assert!(matches!(
        engine(port).migrate(&request, &mut family_transforms()),
        Err(MigrationError::CheckpointMismatch)
    ));
}

#[test]
fn publication_receipt_must_match_verification_and_complete_backup_identity() {
    let request = request();
    let mut wrong_verification = FakePort::new(&request);
    wrong_verification.publication_verification_digest = Some(Digest([9; 32]));
    assert!(matches!(
        engine(wrong_verification).migrate(&request, &mut family_transforms()),
        Err(MigrationError::CheckpointMismatch)
    ));

    let mut wrong_backup = FakePort::new(&request);
    wrong_backup.publication_backup = Some(BackupReceipt {
        backup_id: BackupId::new("backup.foreign").unwrap(),
        source_manifest_digest: LAST,
        artifact_digest: Digest([9; 32]),
    });
    assert!(matches!(
        engine(wrong_backup).migrate(&request, &mut family_transforms()),
        Err(MigrationError::CheckpointMismatch)
    ));
}

#[test]
fn source_bindings_must_share_one_canonical_authority_root() {
    let mut request = request();
    let project = request
        .source_bindings
        .get_mut(&StoreFamily::Project)
        .unwrap()
        .first_mut()
        .unwrap();
    project.shard_id.brain_id = serde_json::from_value(serde_json::json!("brain.foreign")).unwrap();
    let port = FakePort::new(&request);

    assert!(matches!(
        engine(port).migrate(&request, &mut family_transforms()),
        Err(MigrationError::BindingMismatch(StoreFamily::Project))
    ));
}

#[test]
fn profile_memory_is_a_canonical_project_family_binding() {
    let mut request = request();
    request
        .source_bindings
        .get_mut(&StoreFamily::Project)
        .unwrap()
        .first_mut()
        .unwrap()
        .shard_id
        .scope = StoreShardScopeV1::ProfileMemory;
    let port = FakePort::new(&request);

    assert!(matches!(
        engine(port).migrate(&request, &mut family_transforms()),
        Ok(MigrationOutcome::Published(_))
    ));
}

#[test]
fn final_schema_is_idempotent() {
    let release_request = request();
    let mut port = FakePort::new(&release_request);
    for (family, result) in &mut port.preflight.families {
        result.observed_schema = ObservedSchema::Final(family_digest(*family, true));
    }
    let mut release_engine = engine(port);
    // Already-final short-circuits with an empty transform map: there is no
    // crate-default live FamilyTransform registry before PR20.
    let mut empty_transforms = BTreeMap::<StoreFamily, FakeTransform>::new();
    assert_eq!(
        release_engine
            .migrate(&release_request, &mut empty_transforms)
            .unwrap(),
        MigrationOutcome::AlreadyFinal
    );
    assert_eq!(release_engine.into_port().backup_count, 0);
}

#[test]
fn missing_family_transform_fails_closed_without_live_registry() {
    let request = request();
    let mut incomplete = family_transforms();
    incomplete.remove(&StoreFamily::Code);
    let error = engine(FakePort::new(&request))
        .migrate(&request, &mut incomplete)
        .unwrap_err();
    assert!(matches!(
        error,
        MigrationError::InvalidContract("missing family transform")
    ));
}
