use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    CutoverPublicationReceipt, DurableMigrationCheckpoint, ExactMigrationSourceIdentity,
    FinalV2SchemaEvidence, FinalV2TransformReceipt, MigrationContractError, PublicationCasGrant,
    ReadOnlyReleasedSchemaInspection, ReleasedSchemaEvidence, ReleasedStoreKind,
    VerifiedBackupIdentity,
};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FinalV2ExecutionPhase {
    Prepared,
    Published,
    Verified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinalV2FaultPoint {
    BeforeBackup,
    AfterBackup,
    BeforePreparedCheckpoint,
    AfterPreparedCheckpoint,
    BeforeTransformedCheckpoint,
    AfterTransformedCheckpoint,
    BeforePublicationCas,
    AfterPublicationCas,
    BeforePublishedCheckpoint,
    AfterPublishedCheckpoint,
    BeforePrePublicationVerification,
    AfterPrePublicationVerification,
    BeforePostPublicationVerification,
    AfterPostPublicationVerification,
}

impl FinalV2FaultPoint {
    pub const ALL: [Self; 14] = [
        Self::BeforeBackup,
        Self::AfterBackup,
        Self::BeforePreparedCheckpoint,
        Self::AfterPreparedCheckpoint,
        Self::BeforeTransformedCheckpoint,
        Self::AfterTransformedCheckpoint,
        Self::BeforePublicationCas,
        Self::AfterPublicationCas,
        Self::BeforePublishedCheckpoint,
        Self::AfterPublishedCheckpoint,
        Self::BeforePrePublicationVerification,
        Self::AfterPrePublicationVerification,
        Self::BeforePostPublicationVerification,
        Self::AfterPostPublicationVerification,
    ];
}

pub trait FinalV2FaultInjector {
    fn inject(&self, point: FinalV2FaultPoint) -> Result<(), String>;
}

struct NoFinalV2Faults;

impl FinalV2FaultInjector for NoFinalV2Faults {
    fn inject(&self, _point: FinalV2FaultPoint) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalV2ExecutionRequest {
    pub migration_id: String,
    pub checkpoint_id: String,
    pub source: ExactMigrationSourceIdentity,
    pub publication_grant: PublicationCasGrant,
    pub released_schemas: Vec<ReleasedSchemaEvidence>,
    pub prepared_at: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FinalV2ExecutionJournal {
    pub revision: u64,
    pub migration_id: String,
    pub source: ExactMigrationSourceIdentity,
    pub publication_grant: PublicationCasGrant,
    pub released_schemas: Vec<ReleasedSchemaEvidence>,
    pub checkpoint: DurableMigrationCheckpoint,
    pub phase: FinalV2ExecutionPhase,
}

pub trait FinalV2JournalPort {
    fn acquire_execution_lock(&self) -> Result<Box<dyn FinalV2ExecutionLockGuard + '_>, String> {
        Ok(Box::new(InMemoryExecutionLock))
    }

    fn load(&self) -> Result<Option<FinalV2ExecutionJournal>, String>;
    fn compare_and_swap(
        &self,
        expected_revision: Option<u64>,
        journal: &FinalV2ExecutionJournal,
    ) -> Result<(), String>;
}

pub trait FinalV2ExecutionLockGuard {}

struct InMemoryExecutionLock;

impl FinalV2ExecutionLockGuard for InMemoryExecutionLock {}

pub struct FileFinalV2Journal {
    path: PathBuf,
    lock_path: PathBuf,
}

impl FileFinalV2Journal {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let lock_path = path.with_extension("lock");
        Self { path, lock_path }
    }
}

struct FileFinalV2ExecutionLock {
    _file: File,
}

impl FinalV2ExecutionLockGuard for FileFinalV2ExecutionLock {}

impl FinalV2JournalPort for FileFinalV2Journal {
    fn acquire_execution_lock(&self) -> Result<Box<dyn FinalV2ExecutionLockGuard + '_>, String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "create final-V2 journal directory '{}': {error}",
                    parent.display()
                )
            })?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&self.lock_path).map_err(|error| {
            format!(
                "open final-V2 journal lock '{}': {error}",
                self.lock_path.display()
            )
        })?;
        file.try_lock().map_err(|error| {
            format!(
                "acquire final-V2 journal lock '{}': {error}",
                self.lock_path.display()
            )
        })?;
        file.set_len(0).map_err(|error| {
            format!(
                "reset final-V2 journal lock '{}': {error}",
                self.lock_path.display()
            )
        })?;
        writeln!(file, "pid={}", std::process::id())
            .and_then(|()| file.sync_all())
            .map_err(|error| {
                format!(
                    "persist final-V2 journal lock '{}': {error}",
                    self.lock_path.display()
                )
            })?;
        Ok(Box::new(FileFinalV2ExecutionLock { _file: file }))
    }

    fn load(&self) -> Result<Option<FinalV2ExecutionJournal>, String> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "open final-V2 journal '{}': {error}",
                    self.path.display()
                ));
            }
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|error| format!("read final-V2 journal '{}': {error}", self.path.display()))?;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| format!("decode final-V2 journal '{}': {error}", self.path.display()))
    }

    fn compare_and_swap(
        &self,
        expected_revision: Option<u64>,
        journal: &FinalV2ExecutionJournal,
    ) -> Result<(), String> {
        let current_revision = self.load()?.map(|current| current.revision);
        if current_revision != expected_revision
            || journal.revision != expected_revision.unwrap_or(0).saturating_add(1)
        {
            return Err(format!(
                "stale final-V2 journal revision: expected {expected_revision:?}, found {current_revision:?}"
            ));
        }
        let bytes = serde_json::to_vec_pretty(journal)
            .map_err(|error| format!("encode final-V2 journal: {error}"))?;
        write_atomic_durable(&self.path, &bytes)
    }
}

fn write_atomic_durable(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("final-V2 journal '{}' has no parent", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "create final-V2 journal directory '{}': {error}",
            parent.display()
        )
    })?;
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    match fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "remove stale final-V2 journal temporary '{}': {error}",
                temporary.display()
            ));
        }
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).map_err(|error| {
        format!(
            "create final-V2 journal temporary '{}': {error}",
            temporary.display()
        )
    })?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        Ok::<(), std::io::Error>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|error| format!("persist final-V2 journal '{}': {error}", path.display()))
}

pub trait FinalV2MigrationRuntime {
    fn inspect_released_source(&self) -> Result<ReadOnlyReleasedSchemaInspection, String>;

    fn create_verified_backup(
        &self,
        source: &ExactMigrationSourceIdentity,
    ) -> Result<VerifiedBackupIdentity, String>;

    fn transform_release_to_final_v2(
        &self,
        fixtures: &[ReleasedSchemaEvidence],
    ) -> Result<FinalV2TransformReceipt, String>;

    fn publish_registry_and_marker(
        &self,
        source: &ExactMigrationSourceIdentity,
        transformation: &FinalV2TransformReceipt,
        grant: &PublicationCasGrant,
    ) -> Result<CutoverPublicationReceipt, String>;

    fn verify_final_v2_schema(
        &self,
        transformation: &FinalV2TransformReceipt,
    ) -> Result<FinalV2SchemaEvidence, String>;

    fn recover_publication_boundary(
        &self,
        source: &ExactMigrationSourceIdentity,
    ) -> Result<Option<CutoverPublicationReceipt>, String>;

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
    PublicationWithoutJournal,
    Runtime(String),
    Journal(String),
}

impl FinalV2ExecutionError {
    pub const fn contract_error(&self) -> Option<&MigrationContractError> {
        match self {
            Self::Contract(error) => Some(error),
            Self::RequestMismatch
            | Self::PublicationWithoutJournal
            | Self::Runtime(_)
            | Self::Journal(_) => None,
        }
    }
}

pub fn execute_final_v2_migration(
    runtime: &dyn FinalV2MigrationRuntime,
    journal_port: &dyn FinalV2JournalPort,
    request: FinalV2ExecutionRequest,
) -> Result<FinalV2MigrationStatus, FinalV2ExecutionError> {
    execute_final_v2_migration_with_faults(runtime, journal_port, request, &NoFinalV2Faults)
}

pub fn execute_final_v2_migration_with_faults(
    runtime: &dyn FinalV2MigrationRuntime,
    journal_port: &dyn FinalV2JournalPort,
    mut request: FinalV2ExecutionRequest,
    fault_injector: &dyn FinalV2FaultInjector,
) -> Result<FinalV2MigrationStatus, FinalV2ExecutionError> {
    request
        .released_schemas
        .sort_by_key(|evidence| evidence.kind);
    validate_release_request(&request)?;
    let _execution_lock = journal_port
        .acquire_execution_lock()
        .map_err(FinalV2ExecutionError::Journal)?;
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
                || journal.publication_grant != request.publication_grant
                || journal.released_schemas != request.released_schemas
                || journal.checkpoint.checkpoint_id != request.checkpoint_id
                || journal.checkpoint.prepared_at != request.prepared_at
            {
                if journal.checkpoint.backup.validate().is_ok()
                    && journal.checkpoint.publication.is_none()
                {
                    runtime
                        .rollback_before_publication(&journal.checkpoint.backup)
                        .map_err(FinalV2ExecutionError::Runtime)?;
                }
                return Err(FinalV2ExecutionError::RequestMismatch);
            }
            if let Err(error) = validate_journal(&journal) {
                if journal.checkpoint.backup.validate().is_ok()
                    && journal.checkpoint.publication.is_none()
                {
                    runtime
                        .rollback_before_publication(&journal.checkpoint.backup)
                        .map_err(FinalV2ExecutionError::Runtime)?;
                }
                return Err(error);
            }
            journal
        }
        None => {
            if runtime
                .recover_publication_boundary(&request.source)
                .map_err(FinalV2ExecutionError::Runtime)?
                .is_some()
            {
                return Err(FinalV2ExecutionError::PublicationWithoutJournal);
            }
            let inspection = runtime
                .inspect_released_source()
                .map_err(FinalV2ExecutionError::Runtime)?;
            validate_released_inspection(&request, &inspection)?;
            inject(fault_injector, FinalV2FaultPoint::BeforeBackup)?;
            let backup = runtime
                .create_verified_backup(&request.source)
                .map_err(FinalV2ExecutionError::Runtime)?;
            inject(fault_injector, FinalV2FaultPoint::AfterBackup)?;
            let checkpoint = DurableMigrationCheckpoint::before_publication(
                request.checkpoint_id,
                request.migration_id.clone(),
                request.source.clone(),
                backup,
                request.prepared_at,
            )
            .map_err(FinalV2ExecutionError::Contract)?;
            let mut journal = FinalV2ExecutionJournal {
                revision: 0,
                migration_id: request.migration_id,
                source: request.source,
                publication_grant: request.publication_grant.clone(),
                released_schemas: request.released_schemas,
                checkpoint,
                phase: FinalV2ExecutionPhase::Prepared,
            };
            inject(fault_injector, FinalV2FaultPoint::BeforePreparedCheckpoint)?;
            persist_journal_transition(journal_port, &mut journal)?;
            inject(fault_injector, FinalV2FaultPoint::AfterPreparedCheckpoint)?;
            journal
        }
    };

    match journal.phase {
        FinalV2ExecutionPhase::Verified => {
            let transformation = journal.checkpoint.transformation.as_ref().ok_or(
                FinalV2ExecutionError::Contract(MigrationContractError::TargetSchemaMismatch),
            )?;
            verify_comparable_schema(runtime, transformation)?;
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
            receipt
                .validate_for_grant(&journal.publication_grant)
                .map_err(FinalV2ExecutionError::Contract)?;
            runtime
                .roll_forward_after_publication(receipt)
                .map_err(FinalV2ExecutionError::Runtime)?;
        }
        FinalV2ExecutionPhase::Prepared => {
            let recovered_publication = if resuming_prepared {
                runtime
                    .recover_publication_boundary(&journal.source)
                    .map_err(FinalV2ExecutionError::Runtime)?
            } else {
                None
            };
            if let Some(receipt) = recovered_publication {
                receipt
                    .validate_for_grant(&journal.publication_grant)
                    .map_err(FinalV2ExecutionError::Contract)?;
                journal
                    .checkpoint
                    .record_publication(receipt, &journal.publication_grant)
                    .map_err(FinalV2ExecutionError::Contract)?;
                journal.phase = FinalV2ExecutionPhase::Published;
                inject(fault_injector, FinalV2FaultPoint::BeforePublishedCheckpoint)?;
                persist_journal_transition(journal_port, &mut journal)?;
                inject(fault_injector, FinalV2FaultPoint::AfterPublishedCheckpoint)?;
                let receipt = journal.checkpoint.publication.as_ref().ok_or(
                    FinalV2ExecutionError::Contract(MigrationContractError::PublicationInvalid),
                )?;
                runtime
                    .roll_forward_after_publication(receipt)
                    .map_err(FinalV2ExecutionError::Runtime)?;
            } else {
                if resuming_prepared {
                    runtime
                        .rollback_before_publication(&journal.checkpoint.backup)
                        .map_err(FinalV2ExecutionError::Runtime)?;
                    journal.checkpoint.transformation = None;
                    persist_journal_transition(journal_port, &mut journal)?;
                }
                let transformation =
                    match runtime.transform_release_to_final_v2(&journal.released_schemas) {
                        Ok(transformation) => transformation,
                        Err(error) => {
                            return Err(rollback_runtime_error(
                                runtime,
                                &journal.checkpoint.backup,
                                error,
                            ));
                        }
                    };
                if let Err(error) = transformation.validate() {
                    return Err(rollback_contract_error(
                        runtime,
                        &journal.checkpoint.backup,
                        error,
                    ));
                }
                if transformation.schema.source != journal.source {
                    return Err(rollback_contract_error(
                        runtime,
                        &journal.checkpoint.backup,
                        MigrationContractError::IdentityMismatch,
                    ));
                }
                if transformation.schema != journal.publication_grant.target_evidence {
                    return Err(rollback_contract_error(
                        runtime,
                        &journal.checkpoint.backup,
                        MigrationContractError::TargetSchemaMismatch,
                    ));
                }
                journal
                    .checkpoint
                    .record_transformation(transformation)
                    .map_err(FinalV2ExecutionError::Contract)?;
                inject(
                    fault_injector,
                    FinalV2FaultPoint::BeforeTransformedCheckpoint,
                )?;
                persist_journal_transition(journal_port, &mut journal)?;
                inject(
                    fault_injector,
                    FinalV2FaultPoint::AfterTransformedCheckpoint,
                )?;
                let transformation = journal.checkpoint.transformation.as_ref().ok_or(
                    FinalV2ExecutionError::Contract(MigrationContractError::TargetSchemaMismatch),
                )?;
                let prepublication_verification = inject(
                    fault_injector,
                    FinalV2FaultPoint::BeforePrePublicationVerification,
                )
                .and_then(|()| verify_comparable_schema(runtime, transformation))
                .and_then(|()| {
                    inject(
                        fault_injector,
                        FinalV2FaultPoint::AfterPrePublicationVerification,
                    )
                });
                if let Err(error) = prepublication_verification {
                    runtime
                        .rollback_before_publication(&journal.checkpoint.backup)
                        .map_err(FinalV2ExecutionError::Runtime)?;
                    journal.checkpoint.transformation = None;
                    persist_journal_transition(journal_port, &mut journal)?;
                    return Err(error);
                }
                inject(fault_injector, FinalV2FaultPoint::BeforePublicationCas)?;
                let receipt = runtime
                    .publish_registry_and_marker(
                        &journal.source,
                        transformation,
                        &request.publication_grant,
                    )
                    .map_err(FinalV2ExecutionError::Runtime)?;
                receipt
                    .validate_for_grant(&request.publication_grant)
                    .map_err(FinalV2ExecutionError::Contract)?;
                inject(fault_injector, FinalV2FaultPoint::AfterPublicationCas)?;
                journal
                    .checkpoint
                    .record_publication(receipt, &journal.publication_grant)
                    .map_err(FinalV2ExecutionError::Contract)?;
                journal.phase = FinalV2ExecutionPhase::Published;
                inject(fault_injector, FinalV2FaultPoint::BeforePublishedCheckpoint)?;
                persist_journal_transition(journal_port, &mut journal)?;
                inject(fault_injector, FinalV2FaultPoint::AfterPublishedCheckpoint)?;
            }
        }
    }

    inject(
        fault_injector,
        FinalV2FaultPoint::BeforePostPublicationVerification,
    )?;
    let transformation =
        journal
            .checkpoint
            .transformation
            .as_ref()
            .ok_or(FinalV2ExecutionError::Contract(
                MigrationContractError::TargetSchemaMismatch,
            ))?;
    verify_comparable_schema(runtime, transformation)?;
    inject(
        fault_injector,
        FinalV2FaultPoint::AfterPostPublicationVerification,
    )?;
    journal.phase = FinalV2ExecutionPhase::Verified;
    persist_journal_transition(journal_port, &mut journal)?;
    Ok(FinalV2MigrationStatus::Verified)
}

fn inject(
    fault_injector: &dyn FinalV2FaultInjector,
    point: FinalV2FaultPoint,
) -> Result<(), FinalV2ExecutionError> {
    fault_injector
        .inject(point)
        .map_err(FinalV2ExecutionError::Runtime)
}

fn persist_journal_transition(
    journal_port: &dyn FinalV2JournalPort,
    journal: &mut FinalV2ExecutionJournal,
) -> Result<(), FinalV2ExecutionError> {
    let expected_revision = (journal.revision != 0).then_some(journal.revision);
    journal.revision = journal
        .revision
        .checked_add(1)
        .ok_or_else(|| FinalV2ExecutionError::Journal("journal revision overflow".to_owned()))?;
    if let Err(error) = journal_port.compare_and_swap(expected_revision, journal) {
        journal.revision -= 1;
        return Err(FinalV2ExecutionError::Journal(error));
    }
    Ok(())
}

fn verify_comparable_schema(
    runtime: &dyn FinalV2MigrationRuntime,
    transformation: &FinalV2TransformReceipt,
) -> Result<(), FinalV2ExecutionError> {
    let observed = runtime
        .verify_final_v2_schema(transformation)
        .map_err(FinalV2ExecutionError::Runtime)?;
    observed
        .validate()
        .map_err(FinalV2ExecutionError::Contract)?;
    if observed != transformation.schema {
        return Err(FinalV2ExecutionError::Contract(
            MigrationContractError::TargetSchemaMismatch,
        ));
    }
    Ok(())
}

fn rollback_runtime_error(
    runtime: &dyn FinalV2MigrationRuntime,
    backup: &VerifiedBackupIdentity,
    error: String,
) -> FinalV2ExecutionError {
    match runtime.rollback_before_publication(backup) {
        Ok(()) => FinalV2ExecutionError::Runtime(error),
        Err(rollback) => {
            FinalV2ExecutionError::Runtime(format!("{error}; backup rollback failed: {rollback}"))
        }
    }
}

fn rollback_contract_error(
    runtime: &dyn FinalV2MigrationRuntime,
    backup: &VerifiedBackupIdentity,
    error: MigrationContractError,
) -> FinalV2ExecutionError {
    match runtime.rollback_before_publication(backup) {
        Ok(()) => FinalV2ExecutionError::Contract(error),
        Err(rollback) => FinalV2ExecutionError::Runtime(format!(
            "transformation contract failed ({error:?}); backup rollback failed: {rollback}"
        )),
    }
}

fn validate_released_inspection(
    request: &FinalV2ExecutionRequest,
    inspection: &ReadOnlyReleasedSchemaInspection,
) -> Result<(), FinalV2ExecutionError> {
    let project = request
        .released_schemas
        .iter()
        .find(|evidence| evidence.kind == ReleasedStoreKind::Project)
        .ok_or(FinalV2ExecutionError::Contract(
            MigrationContractError::SourceSchemaMismatch,
        ))?;
    let lcm = request
        .released_schemas
        .iter()
        .find(|evidence| evidence.kind == ReleasedStoreKind::Lcm)
        .ok_or(FinalV2ExecutionError::Contract(
            MigrationContractError::SourceSchemaMismatch,
        ))?;
    if inspection.project_schema != project.user_version
        || inspection.lcm_schema != lcm.schema_version
        || inspection.store_manifest_schema != Some(1)
        || inspection.repository_identity_schema != Some(1)
        || inspection.project_structural_members != project.structural_members
        || inspection.lcm_structural_members != lcm.structural_members
        || inspection.durable_families != crate::ReleasedDurableFamily::all()
        || inspection.source != request.source
    {
        return Err(FinalV2ExecutionError::Contract(
            MigrationContractError::SourceSchemaMismatch,
        ));
    }
    Ok(())
}

fn validate_journal(journal: &FinalV2ExecutionJournal) -> Result<(), FinalV2ExecutionError> {
    journal
        .publication_grant
        .validate()
        .map_err(FinalV2ExecutionError::Contract)?;
    journal
        .checkpoint
        .validate()
        .map_err(FinalV2ExecutionError::Contract)?;
    if let Some(receipt) = &journal.checkpoint.publication {
        receipt
            .validate_for_grant(&journal.publication_grant)
            .map_err(FinalV2ExecutionError::Contract)?;
    }
    if journal.revision == 0
        || journal.migration_id != journal.checkpoint.migration_id
        || journal.source != journal.checkpoint.source
        || journal.publication_grant.source != journal.source
        || journal.publication_grant.migration_id != journal.migration_id
        || journal.publication_grant.checkpoint_id != journal.checkpoint.checkpoint_id
        || matches!(journal.phase, FinalV2ExecutionPhase::Prepared)
            != journal.checkpoint.publication.is_none()
    {
        return Err(FinalV2ExecutionError::Contract(
            MigrationContractError::IdentityMismatch,
        ));
    }
    Ok(())
}

fn validate_release_request(
    request: &FinalV2ExecutionRequest,
) -> Result<(), FinalV2ExecutionError> {
    request
        .publication_grant
        .validate()
        .map_err(FinalV2ExecutionError::Contract)?;
    if request.publication_grant.source != request.source {
        return Err(FinalV2ExecutionError::Contract(
            MigrationContractError::IdentityMismatch,
        ));
    }
    if request.publication_grant.migration_id != request.migration_id
        || request.publication_grant.checkpoint_id != request.checkpoint_id
    {
        return Err(FinalV2ExecutionError::Contract(
            MigrationContractError::IdentityMismatch,
        ));
    }
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
    use std::{cell::RefCell, collections::BTreeSet};

    use tracedecay_store::{
        BrainId, CodeShardScopeV1, LocatorDigest, ProjectId, RepositoryId, StoreAuthorityEpochV1,
        StoreIncarnationV1, StoreRuntimeBindingV1, StoreShardIdV1, UserProfileId,
        VerifiedStoreLocatorV1, WorktreeId,
    };

    use crate::{
        DerivedRebuildFamily, ExactMigrationSourceIdentity, FINAL_LCM_SCHEMA_VERSION,
        FINAL_PROFILE_IDENTITY_SCHEMA_VERSION, FINAL_PROJECT_SCHEMA_VERSION,
        FINAL_REPOSITORY_IDENTITY_SCHEMA_VERSION, FINAL_STORE_MANIFEST_SCHEMA_VERSION,
        FINAL_V2_SCHEMA_ID, FinalV2ExecutionJournal, FinalV2ExecutionPhase,
        FinalV2ExecutionRequest, FinalV2JournalPort, FinalV2MigrationRuntime,
        FinalV2MigrationStatus, FinalV2PreservationReceipt, FinalV2SchemaEvidence,
        FinalV2TransformReceipt, MigrationContractError, PublicationCasGrant,
        ReadOnlyReleasedSchemaInspection, ReleasedDurableFamily, ReleasedSchemaEvidence,
        ReleasedSchemaFixture, ReleasedStoreKind, VerifiedBackupIdentity,
        execute_final_v2_migration,
    };

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
    struct RecordingRuntime {
        calls: RefCell<Vec<&'static str>>,
        fail_transform_once: RefCell<bool>,
        fail_verify_once: RefCell<bool>,
        fail_publish_after_commit_once: RefCell<bool>,
        durable_publication: RefCell<Option<crate::CutoverPublicationReceipt>>,
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

        fn fail_publish_after_commit_once() -> Self {
            Self {
                fail_publish_after_commit_once: RefCell::new(true),
                ..Self::default()
            }
        }
    }

    impl FinalV2MigrationRuntime for RecordingRuntime {
        fn inspect_released_source(&self) -> Result<ReadOnlyReleasedSchemaInspection, String> {
            self.calls.borrow_mut().push("inspect");
            Ok(ReadOnlyReleasedSchemaInspection {
                source: source_identity(3),
                project_schema: Some(18),
                lcm_schema: Some(5),
                store_manifest_schema: Some(1),
                repository_identity_schema: Some(1),
                project_structural_members: ReleasedSchemaFixture::for_kind(
                    ReleasedStoreKind::Project,
                )
                .structural_members,
                lcm_structural_members: ReleasedSchemaFixture::for_kind(ReleasedStoreKind::Lcm)
                    .structural_members,
                durable_families: ReleasedDurableFamily::all(),
            })
        }

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
                11,
            )
            .map_err(|error| format!("{error:?}"))
        }

        fn transform_release_to_final_v2(
            &self,
            fixtures: &[ReleasedSchemaEvidence],
        ) -> Result<FinalV2TransformReceipt, String> {
            self.calls.borrow_mut().push("transform");
            assert_eq!(fixtures.len(), 5);
            if self.fail_transform_once.replace(false) {
                return Err("transform interrupted".to_string());
            }
            Ok(transform_receipt())
        }

        fn publish_registry_and_marker(
            &self,
            source: &ExactMigrationSourceIdentity,
            transformation: &FinalV2TransformReceipt,
            grant: &PublicationCasGrant,
        ) -> Result<crate::CutoverPublicationReceipt, String> {
            self.calls.borrow_mut().push("publish");
            transformation
                .validate()
                .map_err(|error| format!("{error:?}"))?;
            let receipt = crate::CutoverPublicationReceipt::from_cas_grant(
                "publication.release",
                source.clone(),
                FINAL_V2_SCHEMA_ID,
                grant,
                12,
            )
            .map_err(|error| format!("{error:?}"))?;
            self.durable_publication.replace(Some(receipt.clone()));
            if self.fail_publish_after_commit_once.replace(false) {
                return Err("publication committed before interruption".to_string());
            }
            Ok(receipt)
        }

        fn verify_final_v2_schema(
            &self,
            transformation: &FinalV2TransformReceipt,
        ) -> Result<FinalV2SchemaEvidence, String> {
            self.calls.borrow_mut().push("verify");
            if self.fail_verify_once.replace(false) {
                return Err("verify interrupted".to_string());
            }
            Ok(transformation.schema.clone())
        }

        fn recover_publication_boundary(
            &self,
            _source: &ExactMigrationSourceIdentity,
        ) -> Result<Option<crate::CutoverPublicationReceipt>, String> {
            self.calls.borrow_mut().push("recover_publication");
            Ok(self.durable_publication.borrow().clone())
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

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn source_identity(material: u8) -> ExactMigrationSourceIdentity {
        let shard_id = StoreShardIdV1::code(
            id::<BrainId>("brain.release"),
            id::<UserProfileId>("project.release"),
            id::<ProjectId>("project.release"),
            id::<RepositoryId>("project.release"),
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
            LocatorDigest::new(format!("sha256:{material:064x}")).unwrap(),
        );
        ExactMigrationSourceIdentity::new(crate::ExactMigrationSourceIdentityRequest {
            profile_id: "project.release".to_owned(),
            repository_id: "project.release".to_owned(),
            project_id: "project.release".to_owned(),
            store_id: "project.release".to_owned(),
            runtime_binding: binding,
            verified_locator: locator,
            material_digest: [material; 32],
            schema_id: crate::LAST_RELEASED_SCHEMA_ID.to_owned(),
        })
        .unwrap()
    }

    fn transform_receipt() -> FinalV2TransformReceipt {
        let source = source_identity(3);
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

    fn request() -> FinalV2ExecutionRequest {
        let source = source_identity(3);
        FinalV2ExecutionRequest {
            migration_id: "migration.release-final-v2".to_string(),
            checkpoint_id: "checkpoint.release-final-v2".to_string(),
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

    #[test]
    fn executes_exact_release_transform_and_atomic_publication() {
        let runtime = RecordingRuntime::default();
        let journal = MemoryJournal::default();

        let status = execute_final_v2_migration(&runtime, &journal, request()).unwrap();

        assert_eq!(status, FinalV2MigrationStatus::Verified);
        assert_eq!(
            *runtime.calls.borrow(),
            [
                "recover_publication",
                "inspect",
                "backup",
                "transform",
                "verify",
                "publish",
                "verify"
            ]
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
            [
                "recover_publication",
                "rollback",
                "transform",
                "verify",
                "publish",
                "verify"
            ]
        );
    }

    #[test]
    fn committed_publication_is_discovered_before_prepared_rollback() {
        let journal = MemoryJournal::default();
        let interrupted = RecordingRuntime::fail_publish_after_commit_once();
        assert!(execute_final_v2_migration(&interrupted, &journal, request()).is_err());
        assert_eq!(
            journal.0.borrow().as_ref().unwrap().phase,
            FinalV2ExecutionPhase::Prepared
        );

        let resumed = RecordingRuntime {
            durable_publication: RefCell::new(interrupted.durable_publication.borrow().clone()),
            ..RecordingRuntime::default()
        };
        assert_eq!(
            execute_final_v2_migration(&resumed, &journal, request()).unwrap(),
            FinalV2MigrationStatus::Verified
        );
        assert_eq!(
            *resumed.calls.borrow(),
            ["recover_publication", "roll_forward", "verify"]
        );
    }

    #[test]
    fn failed_prepublication_verification_restores_backup_before_retry() {
        let journal = MemoryJournal::default();
        let interrupted = RecordingRuntime::fail_verify_once();
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
            [
                "recover_publication",
                "rollback",
                "transform",
                "verify",
                "publish",
                "verify"
            ]
        );
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

    #[test]
    fn execution_journal_roundtrips_without_losing_boundary_identity() {
        let journal = MemoryJournal::default();
        execute_final_v2_migration(&RecordingRuntime::default(), &journal, request()).unwrap();
        let expected = journal.0.borrow().clone().unwrap();

        let encoded = serde_json::to_vec(&expected).unwrap();
        let decoded: FinalV2ExecutionJournal = serde_json::from_slice(&encoded).unwrap();

        assert_eq!(decoded, expected);
        assert_eq!(
            decoded.checkpoint.publication.unwrap().source,
            decoded.source
        );
    }

    #[test]
    fn malformed_persisted_journal_fails_before_runtime_effects() {
        let runtime = RecordingRuntime::default();
        let journal = MemoryJournal::default();
        execute_final_v2_migration(&runtime, &journal, request()).unwrap();
        journal
            .0
            .borrow_mut()
            .as_mut()
            .unwrap()
            .checkpoint
            .backup
            .source
            .material_digest = [9; 32];
        let resumed = RecordingRuntime::default();

        let error = execute_final_v2_migration(&resumed, &journal, request()).unwrap_err();

        assert_eq!(
            error.contract_error(),
            Some(&MigrationContractError::BackupNotVerified)
        );
        assert!(resumed.calls.borrow().is_empty());
    }
}
