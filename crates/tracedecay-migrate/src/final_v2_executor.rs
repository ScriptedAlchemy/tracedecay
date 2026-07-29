use crate::{
    CutoverPublicationReceipt, DurableMigrationCheckpoint, ExactMigrationSourceIdentity,
    MigrationContractError, ReleasedSchemaEvidence, ReleasedStoreKind, VerifiedBackupIdentity,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinalV2ExecutionPhase {
    Prepared,
    Published,
    Verified,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalV2ExecutionRequest {
    pub migration_id: String,
    pub checkpoint_id: String,
    pub source: ExactMigrationSourceIdentity,
    pub released_schemas: Vec<ReleasedSchemaEvidence>,
    pub prepared_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalV2ExecutionJournal {
    pub migration_id: String,
    pub source: ExactMigrationSourceIdentity,
    pub released_schemas: Vec<ReleasedSchemaEvidence>,
    pub checkpoint: DurableMigrationCheckpoint,
    pub phase: FinalV2ExecutionPhase,
}

pub trait FinalV2JournalPort {
    fn load(&self) -> Result<Option<FinalV2ExecutionJournal>, String>;
    fn save(&self, journal: &FinalV2ExecutionJournal) -> Result<(), String>;
}

pub trait FinalV2MigrationRuntime {
    fn create_verified_backup(
        &self,
        source: &ExactMigrationSourceIdentity,
    ) -> Result<VerifiedBackupIdentity, String>;

    fn transform_release_to_final_v2(
        &self,
        fixtures: &[ReleasedSchemaEvidence],
    ) -> Result<(), String>;

    fn publish_registry_and_marker(
        &self,
        source: &ExactMigrationSourceIdentity,
    ) -> Result<CutoverPublicationReceipt, String>;

    fn verify_final_v2_schema(&self) -> Result<(), String>;

    fn rollback_before_publication(&self, backup: &VerifiedBackupIdentity) -> Result<(), String>;

    fn roll_forward_after_publication(
        &self,
        receipt: &CutoverPublicationReceipt,
    ) -> Result<(), String>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinalV2MigrationStatus {
    Verified,
    AlreadyVerified,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FinalV2ExecutionError {
    Contract(MigrationContractError),
    RequestMismatch,
    Runtime(String),
    Journal(String),
}

impl FinalV2ExecutionError {
    pub const fn contract_error(&self) -> Option<&MigrationContractError> {
        match self {
            Self::Contract(error) => Some(error),
            Self::RequestMismatch | Self::Runtime(_) | Self::Journal(_) => None,
        }
    }
}

pub fn execute_final_v2_migration(
    runtime: &dyn FinalV2MigrationRuntime,
    journal_port: &dyn FinalV2JournalPort,
    request: FinalV2ExecutionRequest,
) -> Result<FinalV2MigrationStatus, FinalV2ExecutionError> {
    validate_release_request(&request)?;
    let existing = journal_port
        .load()
        .map_err(FinalV2ExecutionError::Journal)?;
    let resuming_prepared = existing
        .as_ref()
        .is_some_and(|journal| journal.phase == FinalV2ExecutionPhase::Prepared);
    let mut journal = match existing {
        Some(journal) => {
            if journal.migration_id != request.migration_id
                || journal.source != request.source
                || journal.released_schemas != request.released_schemas
            {
                return Err(FinalV2ExecutionError::RequestMismatch);
            }
            journal
        }
        None => {
            let backup = runtime
                .create_verified_backup(&request.source)
                .map_err(FinalV2ExecutionError::Runtime)?;
            let checkpoint = DurableMigrationCheckpoint::before_publication(
                request.checkpoint_id,
                request.migration_id.clone(),
                request.source.clone(),
                backup,
                request.prepared_at,
            )
            .map_err(FinalV2ExecutionError::Contract)?;
            let journal = FinalV2ExecutionJournal {
                migration_id: request.migration_id,
                source: request.source,
                released_schemas: request.released_schemas,
                checkpoint,
                phase: FinalV2ExecutionPhase::Prepared,
            };
            journal_port
                .save(&journal)
                .map_err(FinalV2ExecutionError::Journal)?;
            journal
        }
    };

    match journal.phase {
        FinalV2ExecutionPhase::Verified => {
            runtime
                .verify_final_v2_schema()
                .map_err(FinalV2ExecutionError::Runtime)?;
            return Ok(FinalV2MigrationStatus::AlreadyVerified);
        }
        FinalV2ExecutionPhase::Published => {
            let receipt =
                journal
                    .checkpoint
                    .publication
                    .as_ref()
                    .ok_or(FinalV2ExecutionError::Contract(
                        MigrationContractError::PublicationInvalid,
                    ))?;
            runtime
                .roll_forward_after_publication(receipt)
                .map_err(FinalV2ExecutionError::Runtime)?;
        }
        FinalV2ExecutionPhase::Prepared => {
            if resuming_prepared {
                runtime
                    .rollback_before_publication(&journal.checkpoint.backup)
                    .map_err(FinalV2ExecutionError::Runtime)?;
            }
            runtime
                .transform_release_to_final_v2(&journal.released_schemas)
                .map_err(FinalV2ExecutionError::Runtime)?;
            let receipt = runtime
                .publish_registry_and_marker(&journal.source)
                .map_err(FinalV2ExecutionError::Runtime)?;
            journal
                .checkpoint
                .record_publication(receipt)
                .map_err(FinalV2ExecutionError::Contract)?;
            journal.phase = FinalV2ExecutionPhase::Published;
            journal_port
                .save(&journal)
                .map_err(FinalV2ExecutionError::Journal)?;
        }
    }

    runtime
        .verify_final_v2_schema()
        .map_err(FinalV2ExecutionError::Runtime)?;
    journal.phase = FinalV2ExecutionPhase::Verified;
    journal_port
        .save(&journal)
        .map_err(FinalV2ExecutionError::Journal)?;
    Ok(FinalV2MigrationStatus::Verified)
}

fn validate_release_request(
    request: &FinalV2ExecutionRequest,
) -> Result<(), FinalV2ExecutionError> {
    if request.released_schemas.len() != 5 {
        return Err(FinalV2ExecutionError::Contract(
            MigrationContractError::SourceSchemaMismatch,
        ));
    }
    let mut seen = [false; 5];
    for evidence in &request.released_schemas {
        evidence
            .recognize_v0067()
            .map_err(FinalV2ExecutionError::Contract)?;
        let index = match evidence.kind {
            ReleasedStoreKind::Project => 0,
            ReleasedStoreKind::GlobalSession => 1,
            ReleasedStoreKind::Lcm => 2,
            ReleasedStoreKind::StoreManifest => 3,
            ReleasedStoreKind::RepositoryIdentity => 4,
        };
        if std::mem::replace(&mut seen[index], true) {
            return Err(FinalV2ExecutionError::Contract(
                MigrationContractError::SourceSchemaMismatch,
            ));
        }
    }
    if !seen.into_iter().all(|present| present) {
        return Err(FinalV2ExecutionError::Contract(
            MigrationContractError::SourceSchemaMismatch,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use crate::{
        ExactMigrationSourceIdentity, FINAL_V2_SCHEMA_ID, FinalV2ExecutionJournal,
        FinalV2ExecutionPhase, FinalV2ExecutionRequest, FinalV2JournalPort,
        FinalV2MigrationRuntime, FinalV2MigrationStatus, MigrationContractError,
        ReleasedSchemaEvidence, ReleasedSchemaFixture, ReleasedStoreKind, VerifiedBackupIdentity,
        execute_final_v2_migration,
    };

    #[derive(Default)]
    struct MemoryJournal(RefCell<Option<FinalV2ExecutionJournal>>);

    impl FinalV2JournalPort for MemoryJournal {
        fn load(&self) -> Result<Option<FinalV2ExecutionJournal>, String> {
            Ok(self.0.borrow().clone())
        }

        fn save(&self, journal: &FinalV2ExecutionJournal) -> Result<(), String> {
            self.0.replace(Some(journal.clone()));
            Ok(())
        }
    }

    #[derive(Default)]
    struct RecordingRuntime {
        calls: RefCell<Vec<&'static str>>,
        fail_transform_once: RefCell<bool>,
        fail_verify_once: RefCell<bool>,
    }

    impl RecordingRuntime {
        fn fail_transform_once() -> Self {
            Self {
                fail_transform_once: RefCell::new(true),
                ..Self::default()
            }
        }

        fn fail_verify_once() -> Self {
            Self {
                fail_verify_once: RefCell::new(true),
                ..Self::default()
            }
        }
    }

    impl FinalV2MigrationRuntime for RecordingRuntime {
        fn create_verified_backup(
            &self,
            source: &ExactMigrationSourceIdentity,
        ) -> Result<VerifiedBackupIdentity, String> {
            self.calls.borrow_mut().push("backup");
            VerifiedBackupIdentity::new(
                "backup.release",
                source.clone(),
                "archive.release",
                [7; 32],
                9,
            )
            .map_err(|error| format!("{error:?}"))
        }

        fn transform_release_to_final_v2(
            &self,
            fixtures: &[ReleasedSchemaEvidence],
        ) -> Result<(), String> {
            self.calls.borrow_mut().push("transform");
            assert_eq!(fixtures.len(), 5);
            if self.fail_transform_once.replace(false) {
                return Err("transform interrupted".to_string());
            }
            Ok(())
        }

        fn publish_registry_and_marker(
            &self,
            source: &ExactMigrationSourceIdentity,
        ) -> Result<crate::CutoverPublicationReceipt, String> {
            self.calls.borrow_mut().push("publish");
            crate::CutoverPublicationReceipt::new(
                "publication.release",
                source.clone(),
                FINAL_V2_SCHEMA_ID,
                "authority-cas.release",
                12,
            )
            .map_err(|error| format!("{error:?}"))
        }

        fn verify_final_v2_schema(&self) -> Result<(), String> {
            self.calls.borrow_mut().push("verify");
            if self.fail_verify_once.replace(false) {
                return Err("verify interrupted".to_string());
            }
            Ok(())
        }

        fn rollback_before_publication(
            &self,
            _backup: &VerifiedBackupIdentity,
        ) -> Result<(), String> {
            self.calls.borrow_mut().push("rollback");
            Ok(())
        }

        fn roll_forward_after_publication(
            &self,
            _receipt: &crate::CutoverPublicationReceipt,
        ) -> Result<(), String> {
            self.calls.borrow_mut().push("roll_forward");
            Ok(())
        }
    }

    fn request() -> FinalV2ExecutionRequest {
        FinalV2ExecutionRequest {
            migration_id: "migration.release-final-v2".to_string(),
            checkpoint_id: "checkpoint.release-final-v2".to_string(),
            source: ExactMigrationSourceIdentity::new(
                "project.release",
                "generation.release",
                crate::LAST_RELEASED_SCHEMA_ID,
            )
            .unwrap(),
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

    #[test]
    fn executes_exact_release_transform_and_atomic_publication() {
        let runtime = RecordingRuntime::default();
        let journal = MemoryJournal::default();

        let status = execute_final_v2_migration(&runtime, &journal, request()).unwrap();

        assert_eq!(status, FinalV2MigrationStatus::Verified);
        assert_eq!(
            *runtime.calls.borrow(),
            ["backup", "transform", "publish", "verify"]
        );
        assert_eq!(
            journal.0.borrow().as_ref().unwrap().phase,
            FinalV2ExecutionPhase::Verified
        );
    }

    #[test]
    fn interrupted_transform_rolls_back_before_retrying() {
        let journal = MemoryJournal::default();
        let interrupted = RecordingRuntime::fail_transform_once();
        assert!(execute_final_v2_migration(&interrupted, &journal, request()).is_err());
        assert_eq!(
            journal.0.borrow().as_ref().unwrap().phase,
            FinalV2ExecutionPhase::Prepared
        );

        let resumed = RecordingRuntime::default();
        assert_eq!(
            execute_final_v2_migration(&resumed, &journal, request()).unwrap(),
            FinalV2MigrationStatus::Verified
        );
        assert_eq!(
            *resumed.calls.borrow(),
            ["rollback", "transform", "publish", "verify"]
        );
    }

    #[test]
    fn interruption_after_publication_rolls_forward_without_republishing() {
        let journal = MemoryJournal::default();
        let interrupted = RecordingRuntime::fail_verify_once();
        assert!(execute_final_v2_migration(&interrupted, &journal, request()).is_err());
        assert_eq!(
            journal.0.borrow().as_ref().unwrap().phase,
            FinalV2ExecutionPhase::Published
        );

        let resumed = RecordingRuntime::default();
        assert_eq!(
            execute_final_v2_migration(&resumed, &journal, request()).unwrap(),
            FinalV2MigrationStatus::Verified
        );
        assert_eq!(*resumed.calls.borrow(), ["roll_forward", "verify"]);
    }

    #[test]
    fn verified_rerun_only_rechecks_final_schema() {
        let journal = MemoryJournal::default();
        execute_final_v2_migration(&RecordingRuntime::default(), &journal, request()).unwrap();
        let rerun = RecordingRuntime::default();

        assert_eq!(
            execute_final_v2_migration(&rerun, &journal, request()).unwrap(),
            FinalV2MigrationStatus::AlreadyVerified
        );
        assert_eq!(*rerun.calls.borrow(), ["verify"]);
    }

    #[test]
    fn structural_source_drift_fails_before_backup_or_transform() {
        let runtime = RecordingRuntime::default();
        let journal = MemoryJournal::default();
        let mut request = request();
        request.released_schemas[0]
            .structural_members
            .insert("invented_table".to_string());

        let error = execute_final_v2_migration(&runtime, &journal, request).unwrap_err();

        assert_eq!(
            error.contract_error(),
            Some(&MigrationContractError::SourceSchemaMismatch)
        );
        assert!(runtime.calls.borrow().is_empty());
        assert!(journal.0.borrow().is_none());
    }
}
