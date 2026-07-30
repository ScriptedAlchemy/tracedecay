use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
};

use tracedecay_migrate::{
    CutoverPublicationReceipt, DerivedRebuildFamily, DurableMigrationCheckpoint,
    ExactMigrationSourceIdentity, FINAL_LCM_SCHEMA_VERSION, FINAL_PROFILE_IDENTITY_SCHEMA_VERSION,
    FINAL_PROJECT_SCHEMA_VERSION, FINAL_REPOSITORY_IDENTITY_SCHEMA_VERSION,
    FINAL_STORE_MANIFEST_SCHEMA_VERSION, FINAL_V2_SCHEMA_ID, FinalV2ExecutionJournal,
    FinalV2ExecutionPhase, FinalV2ExecutionRequest, FinalV2FaultInjector, FinalV2FaultPoint,
    FinalV2JournalPort, FinalV2MigrationRuntime, FinalV2PreservationReceipt, FinalV2SchemaEvidence,
    FinalV2TransformReceipt, LAST_RELEASED_SCHEMA_ID, PublicationCasGrant,
    ReadOnlyReleasedSchemaInspection, ReleasedDurableFamily, ReleasedSchemaFixture,
    ReleasedStoreKind, ReleasedV0067Fixture, VerifiedBackupIdentity,
    execute_final_v2_migration_with_faults,
};
use tracedecay_store::{
    BrainId, CodeShardScopeV1, LocatorDigest, ProjectId, RepositoryId, StoreAuthorityEpochV1,
    StoreIncarnationV1, StoreRuntimeBindingV1, StoreShardIdV1, UserProfileId,
    VerifiedStoreLocatorV1, WorktreeId,
};

const RELEASED_FIXTURE: &str = include_str!("fixtures/v0.0.67/final-v2-input.json");

#[derive(Default)]
struct MemoryJournal(RefCell<Option<FinalV2ExecutionJournal>>);

impl FinalV2JournalPort for MemoryJournal {
    fn load(&self) -> Result<Option<FinalV2ExecutionJournal>, String> {
        Ok(self.0.borrow().clone())
    }

    fn compare_and_swap(
        &self,
        expected_revision: Option<u64>,
        journal: &FinalV2ExecutionJournal,
    ) -> Result<(), String> {
        let current_revision = self.0.borrow().as_ref().map(|current| current.revision);
        if current_revision != expected_revision
            || journal.revision != expected_revision.unwrap_or(0).saturating_add(1)
        {
            return Err("stale test journal revision".to_owned());
        }
        self.0.replace(Some(journal.clone()));
        Ok(())
    }
}

#[derive(Default)]
struct BoundaryRuntime {
    publication: RefCell<Option<CutoverPublicationReceipt>>,
    publications: Cell<usize>,
    rollbacks: Cell<usize>,
    roll_forwards: Cell<usize>,
    verifications: Cell<usize>,
}

impl FinalV2MigrationRuntime for BoundaryRuntime {
    fn inspect_released_source(&self) -> Result<ReadOnlyReleasedSchemaInspection, String> {
        Ok(released_inspection())
    }

    fn create_verified_backup(
        &self,
        source: &ExactMigrationSourceIdentity,
    ) -> Result<VerifiedBackupIdentity, String> {
        VerifiedBackupIdentity::new(
            "backup.release",
            source.clone(),
            "archive.release",
            [7; 32],
            11,
        )
        .map_err(|error| format!("{error:?}"))
    }

    fn transform_release_to_final_v2(
        &self,
        _fixtures: &[tracedecay_migrate::ReleasedSchemaEvidence],
    ) -> Result<FinalV2TransformReceipt, String> {
        Ok(transform_receipt())
    }

    fn publish_registry_and_marker(
        &self,
        source: &ExactMigrationSourceIdentity,
        transformation: &FinalV2TransformReceipt,
        grant: &PublicationCasGrant,
    ) -> Result<CutoverPublicationReceipt, String> {
        transformation
            .validate()
            .map_err(|error| format!("{error:?}"))?;
        self.publications.set(self.publications.get() + 1);
        let receipt = CutoverPublicationReceipt::from_cas_grant(
            "publication.release",
            source.clone(),
            FINAL_V2_SCHEMA_ID,
            grant,
            12,
        )
        .map_err(|error| format!("{error:?}"))?;
        self.publication.replace(Some(receipt.clone()));
        Ok(receipt)
    }

    fn verify_final_v2_schema(
        &self,
        transformation: &FinalV2TransformReceipt,
    ) -> Result<FinalV2SchemaEvidence, String> {
        self.verifications.set(self.verifications.get() + 1);
        Ok(transformation.schema.clone())
    }

    fn recover_publication_boundary(
        &self,
        _source: &ExactMigrationSourceIdentity,
    ) -> Result<Option<CutoverPublicationReceipt>, String> {
        Ok(self.publication.borrow().clone())
    }

    fn rollback_before_publication(&self, _backup: &VerifiedBackupIdentity) -> Result<(), String> {
        self.rollbacks.set(self.rollbacks.get() + 1);
        Ok(())
    }

    fn roll_forward_after_publication(
        &self,
        _receipt: &CutoverPublicationReceipt,
    ) -> Result<(), String> {
        self.roll_forwards.set(self.roll_forwards.get() + 1);
        Ok(())
    }
}

struct NoFaults;

impl FinalV2FaultInjector for NoFaults {
    fn inject(&self, _point: FinalV2FaultPoint) -> Result<(), String> {
        Ok(())
    }
}

struct FailOnce {
    point: FinalV2FaultPoint,
    failed: RefCell<bool>,
}

impl FinalV2FaultInjector for FailOnce {
    fn inject(&self, point: FinalV2FaultPoint) -> Result<(), String> {
        if point == self.point && !self.failed.replace(true) {
            return Err(format!("interrupted at {point:?}"));
        }
        Ok(())
    }
}

fn request(fixture: &ReleasedV0067Fixture) -> FinalV2ExecutionRequest {
    let source = source_identity(fixture);
    FinalV2ExecutionRequest {
        migration_id: "migration.release-final-v2".to_owned(),
        checkpoint_id: "checkpoint.release-final-v2".to_owned(),
        publication_grant: PublicationCasGrant::new(
            "authority-cas.release",
            "migration.release-final-v2",
            "checkpoint.release-final-v2",
            source.clone(),
            transform_receipt().schema,
            0,
            1,
        )
        .unwrap(),
        source,
        released_schemas: [
            ReleasedStoreKind::Project,
            ReleasedStoreKind::GlobalSession,
            ReleasedStoreKind::Lcm,
            ReleasedStoreKind::StoreManifest,
            ReleasedStoreKind::RepositoryIdentity,
        ]
        .into_iter()
        .map(|kind| ReleasedSchemaFixture::for_kind(kind).evidence())
        .collect(),
        prepared_at: 10,
    }
}

fn released_inspection() -> ReadOnlyReleasedSchemaInspection {
    let fixture = ReleasedV0067Fixture::from_json(RELEASED_FIXTURE).unwrap();
    ReadOnlyReleasedSchemaInspection {
        source: source_identity(&fixture),
        project_schema: Some(18),
        lcm_schema: Some(5),
        store_manifest_schema: Some(1),
        repository_identity_schema: Some(1),
        project_structural_members: ReleasedSchemaFixture::for_kind(ReleasedStoreKind::Project)
            .structural_members,
        lcm_structural_members: ReleasedSchemaFixture::for_kind(ReleasedStoreKind::Lcm)
            .structural_members,
        durable_families: ReleasedDurableFamily::all(),
    }
}

fn transform_receipt() -> FinalV2TransformReceipt {
    let fixture = ReleasedV0067Fixture::from_json(RELEASED_FIXTURE).unwrap();
    let source = source_identity(&fixture);
    FinalV2TransformReceipt {
        schema: FinalV2SchemaEvidence {
            source: source.clone(),
            schema_id: FINAL_V2_SCHEMA_ID.to_owned(),
            project_schema_version: FINAL_PROJECT_SCHEMA_VERSION,
            lcm_schema_version: FINAL_LCM_SCHEMA_VERSION,
            store_manifest_schema_version: FINAL_STORE_MANIFEST_SCHEMA_VERSION,
            repository_identity_schema_version: FINAL_REPOSITORY_IDENTITY_SCHEMA_VERSION,
            profile_identity_schema_version: FINAL_PROFILE_IDENTITY_SCHEMA_VERSION,
            durable_families: ReleasedDurableFamily::all(),
        },
        preservation: FinalV2PreservationReceipt {
            source,
            preserved_families: ReleasedDurableFamily::all(),
            before_digest: [3; 32],
            after_digest: [3; 32],
        },
        rebuilt_derived_families: [
            DerivedRebuildFamily::Graph,
            DerivedRebuildFamily::Vector,
            DerivedRebuildFamily::FullTextSearch,
            DerivedRebuildFamily::CodeGeneration,
        ]
        .into_iter()
        .collect::<BTreeSet<_>>(),
    }
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn source_identity(fixture: &ReleasedV0067Fixture) -> ExactMigrationSourceIdentity {
    let shard_id = StoreShardIdV1::code(
        id::<BrainId>("brain.release"),
        id::<UserProfileId>(fixture.profile_id()),
        id::<ProjectId>(fixture.project_id()),
        id::<RepositoryId>(fixture.repository_id()),
        CodeShardScopeV1::Worktree {
            worktree_id: id::<WorktreeId>("worktree.release"),
        },
    );
    let incarnation = StoreIncarnationV1::new(1).unwrap();
    let binding = StoreRuntimeBindingV1::new(
        shard_id.clone(),
        incarnation,
        StoreAuthorityEpochV1::new(1).unwrap(),
    );
    let locator = VerifiedStoreLocatorV1::new(
        shard_id,
        incarnation,
        LocatorDigest::new(&"3".repeat(64)).unwrap(),
    );
    ExactMigrationSourceIdentity::new(
        fixture.profile_id(),
        fixture.repository_id(),
        fixture.project_id(),
        fixture.store_id(),
        binding,
        locator,
        [3; 32],
        LAST_RELEASED_SCHEMA_ID,
    )
    .unwrap()
}

#[test]
fn checked_in_fixture_is_exact_released_18_5_and_target_25_7() {
    let fixture = ReleasedV0067Fixture::from_json(RELEASED_FIXTURE).unwrap();

    assert_eq!(fixture.project_schema(), 18);
    assert_eq!(fixture.lcm_schema(), 5);
    assert_eq!(fixture.store_manifest_schema(), 1);
    assert_eq!(fixture.repository_identity_schema(), 1);
    assert_eq!(fixture.target_project_schema(), 25);
    assert_eq!(fixture.target_lcm_schema(), 7);
    fixture.validate().unwrap();
}

#[test]
fn admission_uses_read_only_schema_and_identity_evidence_not_null_hints() {
    let fixture = ReleasedV0067Fixture::from_json(RELEASED_FIXTURE).unwrap();
    let exact = ReadOnlyReleasedSchemaInspection {
        source: source_identity(&fixture),
        project_schema: Some(18),
        lcm_schema: Some(5),
        store_manifest_schema: Some(1),
        repository_identity_schema: Some(1),
        project_structural_members: ReleasedSchemaFixture::for_kind(ReleasedStoreKind::Project)
            .structural_members,
        lcm_structural_members: ReleasedSchemaFixture::for_kind(ReleasedStoreKind::Lcm)
            .structural_members,
        durable_families: ReleasedDurableFamily::all(),
    };
    fixture.admit_read_only_inspection(&exact).unwrap();

    let mut null_schema = exact;
    null_schema.project_schema = None;
    assert!(fixture.admit_read_only_inspection(&null_schema).is_err());
}

#[test]
fn every_cutover_boundary_resumes_without_dual_publication() {
    let fixture = ReleasedV0067Fixture::from_json(RELEASED_FIXTURE).unwrap();
    for point in FinalV2FaultPoint::ALL {
        let journal = MemoryJournal::default();
        let runtime = BoundaryRuntime::default();
        let fault = FailOnce {
            point,
            failed: RefCell::new(false),
        };

        assert!(
            execute_final_v2_migration_with_faults(&runtime, &journal, request(&fixture), &fault,)
                .is_err(),
            "{point:?} must interrupt the first pass"
        );
        execute_final_v2_migration_with_faults(&runtime, &journal, request(&fixture), &NoFaults)
            .unwrap();
        assert_eq!(
            journal.0.borrow().as_ref().unwrap().phase,
            FinalV2ExecutionPhase::Verified
        );
        let persisted = journal.0.borrow();
        let checkpoint: &DurableMigrationCheckpoint = &persisted.as_ref().unwrap().checkpoint;
        assert!(checkpoint.publication.is_some());
        assert_eq!(runtime.publications.get(), 1, "{point:?} republished");
    }
}

#[test]
fn repeated_pre_publication_recovery_is_idempotent() {
    let fixture = ReleasedV0067Fixture::from_json(RELEASED_FIXTURE).unwrap();
    let journal = MemoryJournal::default();
    let runtime = BoundaryRuntime::default();

    for _ in 0..2 {
        let fault = FailOnce {
            point: FinalV2FaultPoint::AfterTransformedCheckpoint,
            failed: RefCell::new(false),
        };
        assert!(
            execute_final_v2_migration_with_faults(&runtime, &journal, request(&fixture), &fault)
                .is_err()
        );
    }
    execute_final_v2_migration_with_faults(&runtime, &journal, request(&fixture), &NoFaults)
        .unwrap();

    assert_eq!(runtime.rollbacks.get(), 2);
    assert_eq!(runtime.publications.get(), 1);
}

#[test]
fn repeated_post_publication_recovery_never_republishes() {
    let fixture = ReleasedV0067Fixture::from_json(RELEASED_FIXTURE).unwrap();
    let journal = MemoryJournal::default();
    let runtime = BoundaryRuntime::default();
    let after_cas = FailOnce {
        point: FinalV2FaultPoint::AfterPublicationCas,
        failed: RefCell::new(false),
    };
    assert!(
        execute_final_v2_migration_with_faults(&runtime, &journal, request(&fixture), &after_cas)
            .is_err()
    );

    for _ in 0..2 {
        let before_verify = FailOnce {
            point: FinalV2FaultPoint::BeforePostPublicationVerification,
            failed: RefCell::new(false),
        };
        assert!(
            execute_final_v2_migration_with_faults(
                &runtime,
                &journal,
                request(&fixture),
                &before_verify
            )
            .is_err()
        );
    }
    execute_final_v2_migration_with_faults(&runtime, &journal, request(&fixture), &NoFaults)
        .unwrap();

    assert_eq!(runtime.publications.get(), 1);
    assert_eq!(runtime.roll_forwards.get(), 3);
}

#[test]
fn stale_journal_writer_loses_revision_cas() {
    let fixture = ReleasedV0067Fixture::from_json(RELEASED_FIXTURE).unwrap();
    let journal = MemoryJournal::default();
    let runtime = BoundaryRuntime::default();
    let fault = FailOnce {
        point: FinalV2FaultPoint::AfterPreparedCheckpoint,
        failed: RefCell::new(false),
    };
    assert!(
        execute_final_v2_migration_with_faults(&runtime, &journal, request(&fixture), &fault)
            .is_err()
    );

    let stale = journal.load().unwrap().unwrap();
    let mut winner = stale.clone();
    winner.revision += 1;
    journal
        .compare_and_swap(Some(stale.revision), &winner)
        .unwrap();
    let mut loser = stale.clone();
    loser.revision += 1;
    assert!(
        journal
            .compare_and_swap(Some(stale.revision), &loser)
            .is_err()
    );
}
