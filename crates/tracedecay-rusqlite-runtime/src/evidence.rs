//! Fixed S11 storage-runtime evidence command.
//!
//! This is a private product adapter, not a general administration CLI. It
//! accepts only copied fixtures, fixed gate IDs, and runner-owned identities.

use std::{
    collections::BTreeMap,
    error::Error,
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};

use rusqlite::{Connection, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_store::{
    AdmissionConfigV1, CommitSequenceV1, FrozenWatermarkVectorV1, LocatorDigest,
    RuntimeCancellationIdentityV1, RuntimeDeadlineV1, RuntimeInterruptionV1,
    RuntimeMaintenanceStateV1, RuntimePublicationIdV1, RuntimeReadCoverageV1, RuntimeReadOutcomeV1,
    RuntimeReadRequestV1, RuntimeReadResultV1, RuntimeRequestProbeV1, ShardWatermarkV1,
    StorageRuntimeErrorV1, StoreCommitReceiptV1, StoreOperationIdV1, StoreRuntimeBindingV1,
    StoreRuntimeRegistryPublicationV1, VerifiedStoreLocatorV1,
};

use crate::{
    WriterState,
    backup::{
        ArtifactIdentity, BackupRestoreOrchestrator, BackupRoot, BackupSetId, Cancellation,
        DeletionState, FilesystemBackupStore, OnlineBackupSource, PrivacyClass, PublishedRestore,
        RestorePublicationAuthority, SchemaVersion, SqliteBackupOptions, SqliteOnlineBackupDriver,
    },
    maintenance::{
        CanonicalRegistryAuthority, CompactionMode, DrainBlockers, DrainedStateProof,
        DriverMaintenanceError, ExclusiveMaintenancePermit, FtsIndexId, MaintenanceAction,
        MaintenanceArtifactInstaller, MaintenanceCoordinator, MaintenanceDriver, MaintenanceError,
        MaintenanceLifecycle, MaintenanceOwnerId, MaintenanceProgress, MaintenanceRequest,
        MaintenanceStart, MigrationPlanId, ReplacementPublicationKind,
        ReplacementPublicationReceipt, ReplacementPublicationRequest, SqliteMaintenanceCatalog,
        SqliteMaintenanceDriver, VerifiedMaintenanceArtifact,
    },
    read_consistency::{CommitWatermarkSource, WatermarkSourceState},
    reader::{
        ExistingReaderLocator, ReaderPool, ReaderPoolSnapshot, ReaderPoolState, ReaderQueryExecutor,
    },
    repair::{
        CorruptionClass, FilesystemQuarantineStore, RejectionReason, RepairCoordinator,
        RepairOutcome, SqliteCorruptionProbe, SqliteRepairDriver,
    },
    runtime::{IntegrityResult, SqliteDoctorHealthLane},
    watermark::{CommitWatermarkPublicationError, CommittedWatermarkPublisher},
};

pub const MAINTENANCE_GATE_ID: &str = "storage-runtime-maintenance-doctor-v1";
pub const REPAIR_GATE_ID: &str = "storage-runtime-crash-recovery-repair-v1";
pub const BACKUP_GATE_ID: &str = "storage-runtime-backup-restore-v1";
pub const FIXTURE_MANIFEST: &str = "storage-runtime-fixture-v1.json";

const EVIDENCE_SCHEMA: &str = "storage-runtime-s6-gate-evidence-v1";
const LOGICAL_SCHEMA: &str = "storage-runtime-logical-sqlite-evidence-v1";

const MAINTENANCE_BINDINGS: &[&str] = &[
    "MaintenanceCoordinator",
    "SqliteMaintenanceDriver",
    "SqliteDoctorHealthLane",
];
const REPAIR_BINDINGS: &[&str] = &[
    "MaintenanceCoordinator",
    "SqliteDoctorHealthLane",
    "SqliteCorruptionProbe",
    "SqliteRepairDriver",
    "FilesystemQuarantineStore",
];
const BACKUP_BINDINGS: &[&str] = &[
    "BackupRoot",
    "FilesystemBackupStore",
    "SqliteOnlineBackupDriver",
    "RestorePublicationAuthority",
    "BackupRestoreOrchestrator",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvidenceGate {
    MaintenanceDoctor,
    CrashRecoveryRepair,
    BackupRestore,
}

impl EvidenceGate {
    fn parse(value: &str) -> Result<Self, EvidenceError> {
        match value {
            MAINTENANCE_GATE_ID => Ok(Self::MaintenanceDoctor),
            REPAIR_GATE_ID => Ok(Self::CrashRecoveryRepair),
            BACKUP_GATE_ID => Ok(Self::BackupRestore),
            _ => Err(EvidenceError::InvalidArgument(
                "--gate must name one fixed S11 gate",
            )),
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::MaintenanceDoctor => MAINTENANCE_GATE_ID,
            Self::CrashRecoveryRepair => REPAIR_GATE_ID,
            Self::BackupRestore => BACKUP_GATE_ID,
        }
    }

    fn bindings(self) -> &'static [&'static str] {
        match self {
            Self::MaintenanceDoctor => MAINTENANCE_BINDINGS,
            Self::CrashRecoveryRepair => REPAIR_BINDINGS,
            Self::BackupRestore => BACKUP_BINDINGS,
        }
    }
}

#[derive(Clone, Debug)]
pub struct EvidenceCommand {
    pub gate: EvidenceGate,
    pub fixture: PathBuf,
    pub output: PathBuf,
    pub fixture_sha256: String,
    pub product_commit_sha: String,
    pub product_binary_sha256: String,
    pub evidence_binary_sha256: String,
    pub crash_count: u32,
    pub restore_rehearsals: u32,
}

impl EvidenceCommand {
    pub fn parse<I, S>(arguments: I) -> Result<Self, EvidenceError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let arguments = arguments.into_iter().map(Into::into).collect::<Vec<_>>();
        let mut values = BTreeMap::new();
        let mut index = 0;
        while index < arguments.len() {
            let key = arguments[index].as_str();
            if !matches!(
                key,
                "--gate"
                    | "--fixture"
                    | "--output"
                    | "--fixture-sha256"
                    | "--product-commit-sha"
                    | "--product-binary-sha256"
                    | "--evidence-binary-sha256"
                    | "--crash-count"
                    | "--restore-rehearsals"
            ) {
                return Err(EvidenceError::InvalidArgument("unknown command argument"));
            }
            let value = arguments
                .get(index + 1)
                .ok_or(EvidenceError::InvalidArgument("argument value is missing"))?;
            if values.insert(key.to_owned(), value.clone()).is_some() {
                return Err(EvidenceError::InvalidArgument("duplicate command argument"));
            }
            index += 2;
        }
        let required = |name: &str| {
            values
                .get(name)
                .cloned()
                .ok_or(EvidenceError::InvalidArgument(
                    "required argument is missing",
                ))
        };
        let gate = EvidenceGate::parse(&required("--gate")?)?;
        let fixture = absolute_path(&required("--fixture")?, "--fixture")?;
        let output = absolute_path(&required("--output")?, "--output")?;
        let fixture_sha256 = required("--fixture-sha256")?;
        if !valid_hex(&fixture_sha256, 64, 64) {
            return Err(EvidenceError::InvalidArgument(
                "--fixture-sha256 must be lowercase SHA-256",
            ));
        }
        let product_commit_sha = required("--product-commit-sha")?;
        if !valid_hex(&product_commit_sha, 40, 64) {
            return Err(EvidenceError::InvalidArgument(
                "--product-commit-sha must be a lowercase 40-64 digit commit",
            ));
        }
        let product_binary_sha256 = required("--product-binary-sha256")?;
        if !valid_hex(&product_binary_sha256, 64, 64) {
            return Err(EvidenceError::InvalidArgument(
                "--product-binary-sha256 must be lowercase SHA-256",
            ));
        }
        let evidence_binary_sha256 = required("--evidence-binary-sha256")?;
        if !valid_hex(&evidence_binary_sha256, 64, 64) {
            return Err(EvidenceError::InvalidArgument(
                "--evidence-binary-sha256 must be lowercase SHA-256",
            ));
        }
        if product_binary_sha256 == evidence_binary_sha256 {
            return Err(EvidenceError::InvalidArgument(
                "product and evidence binaries must be distinct artifacts",
            ));
        }
        Ok(Self {
            gate,
            fixture,
            output,
            fixture_sha256,
            product_commit_sha,
            product_binary_sha256,
            evidence_binary_sha256,
            crash_count: optional_count(&values, "--crash-count")?,
            restore_rehearsals: optional_count(&values, "--restore-rehearsals")?,
        })
    }
}

#[derive(Debug)]
pub enum EvidenceError {
    InvalidArgument(&'static str),
    Refused(String),
    Io(std::io::Error),
    Json(serde_json::Error),
    Sqlite(rusqlite::Error),
    Watermark(CommitWatermarkPublicationError),
    Runtime(String),
}

impl fmt::Display for EvidenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgument(message) => formatter.write_str(message),
            Self::Refused(message) => formatter.write_str(message),
            Self::Io(error) => write!(formatter, "storage evidence I/O failed: {error}"),
            Self::Json(error) => write!(formatter, "storage evidence JSON failed: {error}"),
            Self::Sqlite(error) => write!(formatter, "storage evidence SQLite failed: {error}"),
            Self::Watermark(error) => {
                write!(formatter, "storage evidence watermark failed: {error}")
            }
            Self::Runtime(message) => {
                write!(formatter, "storage evidence runtime failed: {message}")
            }
        }
    }
}

impl Error for EvidenceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::Watermark(error) => Some(error),
            Self::InvalidArgument(_) | Self::Refused(_) | Self::Runtime(_) => None,
        }
    }
}

impl From<std::io::Error> for EvidenceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for EvidenceError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<rusqlite::Error> for EvidenceError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl From<CommitWatermarkPublicationError> for EvidenceError {
    fn from(error: CommitWatermarkPublicationError) -> Self {
        Self::Watermark(error)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureManifest {
    schema_version: u32,
    project_root: String,
    profile_root: String,
    #[serde(default)]
    fts_queries: BTreeMap<String, String>,
    s11: S11Fixture,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct S11Fixture {
    database: String,
    binding: StoreRuntimeBindingV1,
    #[serde(default = "default_tables")]
    evidence_tables: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct FingerprintEntry {
    pub path: String,
    pub sha256: String,
}

#[derive(Serialize)]
struct EvidenceStatus {
    state: &'static str,
    reasons: Vec<String>,
}

#[derive(Serialize)]
struct GateEvidence<O> {
    schema: &'static str,
    gate_id: &'static str,
    status: &'static str,
    evidence_status: EvidenceStatus,
    api_bindings: &'static [&'static str],
    fixture_sha256: Option<String>,
    product_commit_sha: Option<String>,
    product_binary_sha256: Option<String>,
    evidence_binary_sha256: Option<String>,
    logical_evidence: Vec<LogicalSqliteEvidence>,
    outcome: O,
}

#[derive(Serialize)]
struct LogicalSqliteEvidence {
    schema: &'static str,
    integrity: IntegrityEvidence,
    schema_sha256: String,
    tables: Vec<TableEvidence>,
    fts: Vec<serde_json::Value>,
}

#[derive(Serialize)]
struct IntegrityEvidence {
    status: &'static str,
    result_sha256: String,
    result_row_count: usize,
}

#[derive(Serialize)]
struct TableEvidence {
    table_id: String,
    row_count: u64,
}

#[derive(Serialize)]
struct MaintenanceOutcome {
    maintenance_reopened: bool,
    doctor_quick_check: &'static str,
    doctor_integrity_check: &'static str,
    writer_state: &'static str,
    reader_state: &'static str,
    wal_enabled: bool,
}

#[derive(Serialize)]
struct RepairOutcomeEvidence {
    crashes_requested: u32,
    crashes_completed: u32,
    recoveries_completed: u32,
    doctor_detected_fault: bool,
    repair_class: &'static str,
    repair_receipt_bound: bool,
    quarantine_preserved: bool,
    recovery_health: &'static str,
}

#[derive(Serialize)]
struct BackupOutcome {
    restores_requested: u32,
    backups_completed: u32,
    restores_completed: u32,
    backup_manifest_verified: bool,
    artifact_digests_verified: bool,
    restore_verified: bool,
    replacement_published: bool,
    restored_binding_newer: bool,
}

pub fn execute(command: EvidenceCommand) -> Result<String, EvidenceError> {
    validate_command_paths(&command)?;
    let actual_fixture = fingerprint_tree(&command.fixture)?;
    if actual_fixture != command.fixture_sha256 {
        return Err(EvidenceError::Refused(
            "fixture identity mismatch; refusing S11 evidence".to_owned(),
        ));
    }
    let manifest = load_manifest(&command.fixture)?;
    let database = fixture_file(&command.fixture, &manifest.s11.database)?;
    let work_root = create_work_root(&command)?;
    let document = match command.gate {
        EvidenceGate::MaintenanceDoctor => serde_json::to_value(run_maintenance(
            &command,
            &manifest.s11,
            &database,
            &work_root,
        )?)?,
        EvidenceGate::CrashRecoveryRepair => {
            serde_json::to_value(run_repair(&command, &manifest.s11, &database, &work_root)?)?
        }
        EvidenceGate::BackupRestore => {
            serde_json::to_value(run_backup(&command, &manifest.s11, &database, &work_root)?)?
        }
    };
    let encoded = serde_json::to_string(&document)?;
    publish_output(&command.output, encoded.as_bytes())?;
    Ok(encoded)
}

pub fn run_crash_worker(database: &Path, ready: &Path) -> Result<(), EvidenceError> {
    let mut connection = Connection::open(database)?;
    connection.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE IF NOT EXISTS evidence_rows(value TEXT NOT NULL);",
    )?;
    let transaction = connection.transaction()?;
    transaction.execute(
        "INSERT INTO evidence_rows(value) VALUES ('uncommitted-crash-write')",
        [],
    )?;
    publish_output(ready, b"ready\n")?;
    std::process::abort();
}

fn run_maintenance(
    command: &EvidenceCommand,
    fixture: &S11Fixture,
    source: &Path,
    work_root: &Path,
) -> Result<GateEvidence<MaintenanceOutcome>, EvidenceError> {
    let database = work_root.join("maintenance.sqlite3");
    fs::copy(source, &database)?;
    let connection = Connection::open(&database)?;
    connection.execute_batch("PRAGMA journal_mode=WAL")?;

    let reader = ReaderPool::start(
        reader_locator(&fixture.binding, &database)?,
        AdmissionConfigV1::default().readers,
        EvidenceReadExecutor,
    )
    .map_err(runtime)?;
    execute_reader(&reader, &fixture.binding)?;
    let reader_before = reader.snapshot();

    let publisher =
        CommittedWatermarkPublisher::with_initial_watermarks([watermark(&fixture.binding, 1)])?;
    let receipt: StoreCommitReceiptV1 = serde_json::from_value(serde_json::json!({
        "operation_id": "operation.storage.evidence",
        "idempotency": {
            "key": "key.storage.evidence",
            "command_digest": format!("sha256:{}", "a".repeat(64))
        },
        "shard_id": fixture.binding.shard_id,
        "incarnation": fixture.binding.incarnation,
        "authority_epoch": fixture.binding.authority_epoch,
        "commit_sequence": 2,
        "committed_at": 1
    }))?;
    publisher.publish_committed(&receipt)?;
    if publisher.subscribe().current(&fixture.binding.shard_id)
        != WatermarkSourceState::Available(watermark(&fixture.binding, 2))
    {
        return Err(EvidenceError::Runtime(
            "committed watermark was not observable".to_owned(),
        ));
    }
    drop(reader);

    let prior = publication("publication.storage.evidence.1", fixture.binding.clone(), 1)?;
    let replacement_binding = stronger_binding(&fixture.binding, 1)?;
    let replacement = publication("publication.storage.evidence.2", replacement_binding, 2)?;
    let lifecycle = EvidenceLifecycle::ready(prior.clone());
    let coordinator = MaintenanceCoordinator::new(Arc::new(EvidenceAuthority {
        replacement: replacement.clone(),
    }));
    let owner = MaintenanceOwnerId::new(1)
        .ok_or_else(|| EvidenceError::Runtime("maintenance owner was invalid".to_owned()))?;
    let request = MaintenanceRequest {
        owner,
        expected: prior,
        action: MaintenanceAction::Compaction {
            mode: CompactionMode::Incremental,
        },
    };
    if coordinator
        .start(&lifecycle, request, &|_| false)
        .map_err(runtime_debug)?
        != MaintenanceStart::Started
    {
        return Err(EvidenceError::Runtime(
            "maintenance did not start".to_owned(),
        ));
    }
    let mut driver = SqliteMaintenanceDriver::from_writer_connection(
        connection,
        SqliteMaintenanceCatalog::default(),
        NoArtifacts,
    );
    let progress = coordinator
        .advance(owner, &lifecycle, &mut driver, &|_| false)
        .map_err(runtime_debug)?;
    driver.close()?;
    let reopened = matches!(
        progress,
        MaintenanceProgress::Reopened {
            action_performed: true,
            ..
        }
    );

    let reopened_reader = ReaderPool::start(
        reader_locator(&replacement.binding, &database)?,
        AdmissionConfigV1::default().readers,
        EvidenceReadExecutor,
    )
    .map_err(runtime)?;
    execute_reader(&reopened_reader, &replacement.binding)?;
    let doctor = SqliteDoctorHealthLane::from_health_connection(
        replacement.binding.clone(),
        Connection::open(&database)?,
    )
    .inspect(WriterState::Ready, reopened_reader.snapshot(), true)
    .map_err(runtime)?;
    let logical = logical_evidence(&database, &fixture.evidence_tables)?;
    let reader_ready = reader_before.state == ReaderPoolState::Ready
        && doctor.reader_state == ReaderPoolState::Ready;
    Ok(completed(
        command,
        vec![logical],
        MaintenanceOutcome {
            maintenance_reopened: reopened,
            doctor_quick_check: integrity_name(&doctor.quick_check),
            doctor_integrity_check: doctor
                .integrity_check
                .as_ref()
                .map(integrity_name)
                .unwrap_or("not_observed"),
            writer_state: writer_state_name(doctor.writer_state),
            reader_state: if reader_ready { "ready" } else { "faulted" },
            wal_enabled: doctor.wal.enabled,
        },
    ))
}

fn run_repair(
    command: &EvidenceCommand,
    fixture: &S11Fixture,
    source: &Path,
    work_root: &Path,
) -> Result<GateEvidence<RepairOutcomeEvidence>, EvidenceError> {
    if command.crash_count == 0 {
        return Ok(not_run(
            command,
            "crash-recovery gate requires --crash-count greater than zero",
            RepairOutcomeEvidence {
                crashes_requested: 0,
                crashes_completed: 0,
                recoveries_completed: 0,
                doctor_detected_fault: false,
                repair_class: "not_observed",
                repair_receipt_bound: false,
                quarantine_preserved: false,
                recovery_health: "not_observed",
            },
        ));
    }
    let mut crashes_completed = 0;
    let mut recoveries_completed = 0;
    let mut recovery_database = None;
    for attempt in 0..command.crash_count {
        let database = work_root.join(format!("crash-{attempt}.sqlite3"));
        fs::copy(source, &database)?;
        let ready = work_root.join(format!("crash-{attempt}.ready"));
        let status = Command::new(std::env::current_exe()?)
            .arg("--storage-runtime-crash-worker")
            .arg(&database)
            .arg(&ready)
            .status()?;
        if !status.success() && ready.is_file() {
            crashes_completed += 1;
        }
        let reader = ReaderPool::start(
            reader_locator(&fixture.binding, &database)?,
            AdmissionConfigV1::default().readers,
            EvidenceReadExecutor,
        )
        .map_err(runtime)?;
        execute_reader(&reader, &fixture.binding)?;
        let health = SqliteDoctorHealthLane::from_health_connection(
            fixture.binding.clone(),
            Connection::open(&database)?,
        )
        .inspect(WriterState::Ready, reader.snapshot(), true)
        .map_err(runtime)?;
        if health.quick_check == IntegrityResult::Healthy {
            recoveries_completed += 1;
        }
        recovery_database = Some(database);
    }

    let corrupt = work_root.join("corrupt.sqlite3");
    fs::copy(source, &corrupt)?;
    let corrupt_connection = Connection::open(&corrupt)?;
    corrupt_connection.execute_batch(
        "PRAGMA writable_schema=ON;
         UPDATE sqlite_schema SET rootpage=2147483646
         WHERE name=(SELECT name FROM sqlite_schema WHERE type='table'
                     AND name NOT LIKE 'sqlite_%' ORDER BY name LIMIT 1);
         PRAGMA writable_schema=OFF;",
    )?;
    drop(corrupt_connection);
    let probe_connection = Connection::open(&corrupt)?;
    let probe = SqliteCorruptionProbe::new(
        &probe_connection,
        fixture.binding.clone(),
        operation_id("evidence.storage.corruption")?,
        &[],
    );
    let coordinator = RepairCoordinator::new(operation_id("repair.storage.corruption")?);
    let diagnosis = coordinator.diagnose(&probe).map_err(runtime)?;
    let repair_class = corruption_name(diagnosis.class);
    let doctor_detected_fault = match Connection::open(&corrupt) {
        Ok(connection) => {
            SqliteDoctorHealthLane::from_health_connection(fixture.binding.clone(), connection)
                .inspect(WriterState::Ready, empty_reader_snapshot(), true)
                .map(|snapshot| snapshot.quick_check != IntegrityResult::Healthy)
                .unwrap_or(true)
        }
        Err(_) => true,
    };
    let quarantine = FilesystemQuarantineStore::new(work_root.join("quarantine"), corrupt.clone())
        .map_err(runtime)?;
    let mut repair_connection = Connection::open(&corrupt)?;
    let sqlite_driver = SqliteRepairDriver::new(&mut repair_connection, &[], quarantine);
    let prior = publication("publication.storage.repair.1", fixture.binding.clone(), 1)?;
    let replacement = publication(
        "publication.storage.repair.2",
        stronger_binding(&fixture.binding, 1)?,
        2,
    )?;
    let lifecycle = EvidenceLifecycle::ready(prior.clone());
    let maintenance = MaintenanceCoordinator::new(Arc::new(EvidenceAuthority { replacement }));
    let owner = MaintenanceOwnerId::new(20)
        .ok_or_else(|| EvidenceError::Runtime("repair owner was invalid".to_owned()))?;
    let request = MaintenanceRequest {
        owner,
        expected: prior,
        action: MaintenanceAction::Compaction {
            mode: CompactionMode::Incremental,
        },
    };
    if maintenance
        .start(&lifecycle, request, &|_| false)
        .map_err(runtime_debug)?
        != MaintenanceStart::Started
    {
        return Err(EvidenceError::Runtime(
            "repair maintenance did not start".to_owned(),
        ));
    }
    let mut driver = AuthorizedRepairDriver {
        coordinator: &coordinator,
        probe: &probe,
        driver: sqlite_driver,
        outcome: None,
    };
    let progress = maintenance
        .advance(owner, &lifecycle, &mut driver, &|_| false)
        .map_err(runtime_debug)?;
    if !matches!(
        progress,
        MaintenanceProgress::Reopened {
            action_performed: true,
            ..
        }
    ) {
        return Err(EvidenceError::Runtime(
            "repair maintenance did not reopen".to_owned(),
        ));
    }
    let outcome = driver
        .outcome
        .take()
        .ok_or_else(|| EvidenceError::Runtime("repair outcome was missing".to_owned()))?;
    let (repair_receipt_bound, quarantine_preserved) = match outcome {
        RepairOutcome::Completed { receipt, .. } => (receipt.binding == fixture.binding, false),
        RepairOutcome::Rejected {
            reason: RejectionReason::AuthoritativeQuarantined,
            quarantine: Some(receipt),
            ..
        } => (false, receipt.binding == fixture.binding),
        _ => (false, false),
    };
    let logical = logical_evidence(
        recovery_database
            .as_deref()
            .ok_or_else(|| EvidenceError::Runtime("recovery database was missing".to_owned()))?,
        &fixture.evidence_tables,
    )?;
    Ok(completed(
        command,
        vec![logical],
        RepairOutcomeEvidence {
            crashes_requested: command.crash_count,
            crashes_completed,
            recoveries_completed,
            doctor_detected_fault,
            repair_class,
            repair_receipt_bound,
            quarantine_preserved,
            recovery_health: if recoveries_completed == command.crash_count {
                "healthy"
            } else {
                "faulted"
            },
        },
    ))
}

fn run_backup(
    command: &EvidenceCommand,
    fixture: &S11Fixture,
    source: &Path,
    work_root: &Path,
) -> Result<GateEvidence<BackupOutcome>, EvidenceError> {
    if command.restore_rehearsals == 0 {
        return Ok(not_run(
            command,
            "backup-restore gate requires --restore-rehearsals greater than zero",
            BackupOutcome {
                restores_requested: 0,
                backups_completed: 0,
                restores_completed: 0,
                backup_manifest_verified: false,
                artifact_digests_verified: false,
                restore_verified: false,
                replacement_published: false,
                restored_binding_newer: false,
            },
        ));
    }
    let database = work_root.join("backup-source.sqlite3");
    fs::copy(source, &database)?;
    let connection = Connection::open(&database)?;
    connection.execute_batch("PRAGMA journal_mode=WAL")?;
    let root = BackupRoot::open(work_root.join("backup-root")).map_err(runtime)?;
    let mut filesystem = FilesystemBackupStore::new(root.clone());
    let watermark = watermark(&fixture.binding, 2);
    let required = FrozenWatermarkVectorV1::new([watermark.clone()]).map_err(runtime)?;
    let capture = Arc::new(Mutex::new(Vec::<PublishedRestore>::new()));
    let mut backups_completed = 0;
    let mut restores_completed = 0;
    let mut manifests_verified = true;
    let mut digests_verified = true;
    let mut restored_newer = true;

    for rehearsal in 0..command.restore_rehearsals {
        let authority = EvidenceRestoreAuthority {
            published: Arc::clone(&capture),
        };
        let mut driver = SqliteOnlineBackupDriver::new(
            root.clone(),
            [OnlineBackupSource::from_writer_connection(
                watermark.clone(),
                &connection,
            )],
            SchemaVersion(1),
            PrivacyClass::Project,
            DeletionState::Live,
            BTreeMap::new(),
            SqliteBackupOptions::default(),
            authority,
        )
        .ok_or_else(|| EvidenceError::Runtime("online backup source was empty".to_owned()))?;
        let backup_set = BackupSetId::new(format!("s11-rehearsal-{rehearsal}"))
            .map_err(|error| EvidenceError::Runtime(format!("{error:?}")))?;
        let stored = BackupRestoreOrchestrator::new(&mut driver, &mut filesystem)
            .backup(&required, backup_set.clone(), &NeverCancel)
            .map_err(runtime)?;
        backups_completed += 1;
        manifests_verified &= stored.manifest.backup_set == backup_set
            && stored.manifest.frozen_watermarks == required;
        let replacement = stronger_binding(&fixture.binding, u64::from(rehearsal) + 1)?;
        let permit = evidence_permit(fixture.binding.clone(), 100 + u64::from(rehearsal))?;
        let restored = BackupRestoreOrchestrator::new(&mut driver, &mut filesystem)
            .restore(&backup_set, permit, vec![replacement.clone()], &NeverCancel)
            .map_err(runtime)?;
        restores_completed += 1;
        restored_newer &= restored == vec![replacement];
        let published = capture
            .lock()
            .map_err(|_| EvidenceError::Runtime("restore capture was poisoned".to_owned()))?
            .last()
            .cloned()
            .ok_or_else(|| EvidenceError::Runtime("restore was not published".to_owned()))?;
        let published_digest = root
            .published_store_sha256(&published, &fixture.binding.shard_id)
            .map_err(runtime)?;
        let manifest_digest = stored
            .manifest
            .artifacts
            .iter()
            .find_map(|artifact| {
                matches!(
                    &artifact.identity,
                    ArtifactIdentity::Store(shard) if shard == &fixture.binding.shard_id
                )
                .then_some(artifact.sha256)
            })
            .ok_or_else(|| {
                EvidenceError::Runtime("backup store artifact was missing".to_owned())
            })?;
        digests_verified &= published_digest == manifest_digest;
    }
    let publication_count = capture
        .lock()
        .map_err(|_| EvidenceError::Runtime("restore capture was poisoned".to_owned()))?
        .len() as u32;
    let logical = logical_evidence(&database, &fixture.evidence_tables)?;
    Ok(completed(
        command,
        vec![logical],
        BackupOutcome {
            restores_requested: command.restore_rehearsals,
            backups_completed,
            restores_completed,
            backup_manifest_verified: manifests_verified,
            artifact_digests_verified: digests_verified,
            restore_verified: digests_verified && restores_completed == command.restore_rehearsals,
            replacement_published: publication_count == command.restore_rehearsals,
            restored_binding_newer: restored_newer,
        },
    ))
}

fn completed<O>(
    command: &EvidenceCommand,
    logical: Vec<LogicalSqliteEvidence>,
    outcome: O,
) -> GateEvidence<O> {
    GateEvidence {
        schema: EVIDENCE_SCHEMA,
        gate_id: command.gate.id(),
        status: "completed",
        evidence_status: EvidenceStatus {
            state: "evidence",
            reasons: Vec::new(),
        },
        api_bindings: command.gate.bindings(),
        fixture_sha256: Some(command.fixture_sha256.clone()),
        product_commit_sha: Some(command.product_commit_sha.clone()),
        product_binary_sha256: Some(command.product_binary_sha256.clone()),
        evidence_binary_sha256: Some(command.evidence_binary_sha256.clone()),
        logical_evidence: logical,
        outcome,
    }
}

fn not_run<O>(command: &EvidenceCommand, reason: &str, outcome: O) -> GateEvidence<O> {
    GateEvidence {
        schema: EVIDENCE_SCHEMA,
        gate_id: command.gate.id(),
        status: "not_run",
        evidence_status: EvidenceStatus {
            state: "not_evidence",
            reasons: vec![reason.to_owned()],
        },
        api_bindings: command.gate.bindings(),
        fixture_sha256: None,
        product_commit_sha: None,
        product_binary_sha256: None,
        evidence_binary_sha256: None,
        logical_evidence: Vec::new(),
        outcome,
    }
}

#[derive(Clone, Copy)]
struct EvidenceReadExecutor;

impl ReaderQueryExecutor for EvidenceReadExecutor {
    fn execute_read(
        &mut self,
        snapshot: &Transaction<'_>,
        _request: &RuntimeReadRequestV1,
    ) -> Result<RuntimeReadOutcomeV1, StorageRuntimeErrorV1> {
        let count = snapshot
            .query_row("SELECT COUNT(*) FROM evidence_rows", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|error| StorageRuntimeErrorV1::Infrastructure {
                operation: format!("read evidence row count: {error}"),
            })?;
        RuntimeReadOutcomeV1::new(
            Some(RuntimeReadResultV1::GraphQuickCheck { healthy: count > 0 }),
            RuntimeReadCoverageV1::Latest { observed: None },
        )
        .map_err(|error| StorageRuntimeErrorV1::Infrastructure {
            operation: format!("construct evidence read outcome: {error}"),
        })
    }
}

struct EvidenceProbe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
}

impl RuntimeRequestProbeV1 for EvidenceProbe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        None
    }
}

fn execute_reader(
    pool: &ReaderPool<EvidenceReadExecutor>,
    binding: &StoreRuntimeBindingV1,
) -> Result<(), EvidenceError> {
    let request: RuntimeReadRequestV1 = serde_json::from_value(serde_json::json!({
        "binding": binding,
        "consistency": { "kind": "latest_available" },
        "operation": { "kind": "graph_quick_check" },
        "priority": "health",
        "admission_bytes": 64,
        "control": {
            "requested_at": 1,
            "deadline": { "deadline_id": "deadline.storage.evidence" },
            "cancellation": {
                "cancellation_id": "cancellation.storage.evidence",
                "generation": 1
            }
        }
    }))?;
    let probe = EvidenceProbe {
        cancellation: request.control().cancellation.clone(),
        deadline: request.control().deadline.clone(),
    };
    let mut lease = pool
        .acquire(&request, &probe, Duration::ZERO)
        .map_err(runtime)?;
    let mut snapshot = lease.begin_snapshot().map_err(runtime)?;
    let outcome = snapshot.execute(request, &probe).map_err(runtime)?;
    if !matches!(
        outcome.value(),
        Some(RuntimeReadResultV1::GraphQuickCheck { healthy: true })
    ) {
        return Err(EvidenceError::Runtime(
            "reader did not observe the fixture row".to_owned(),
        ));
    }
    Ok(())
}

struct NoArtifacts;

impl MaintenanceArtifactInstaller for NoArtifacts {
    fn restore(
        &mut self,
        _connection: &mut Connection,
        _permit: &ExclusiveMaintenancePermit,
        _artifact: &VerifiedMaintenanceArtifact,
    ) -> Result<(), crate::maintenance::DriverMaintenanceError> {
        Err(crate::maintenance::DriverMaintenanceError {
            code: "unsupported_evidence_restore",
            retryable: false,
        })
    }

    fn replace_shard(
        &mut self,
        _connection: &mut Connection,
        _permit: &ExclusiveMaintenancePermit,
        _artifact: &VerifiedMaintenanceArtifact,
    ) -> Result<(), crate::maintenance::DriverMaintenanceError> {
        Err(crate::maintenance::DriverMaintenanceError {
            code: "unsupported_evidence_replacement",
            retryable: false,
        })
    }
}

struct AuthorizedRepairDriver<'a> {
    coordinator: &'a RepairCoordinator,
    probe: &'a SqliteCorruptionProbe<'a>,
    driver: SqliteRepairDriver<'a, FilesystemQuarantineStore>,
    outcome: Option<RepairOutcome>,
}

impl MaintenanceDriver for AuthorizedRepairDriver<'_> {
    fn migrate(
        &mut self,
        _permit: &ExclusiveMaintenancePermit,
        _plan: &MigrationPlanId,
    ) -> Result<(), DriverMaintenanceError> {
        Err(unsupported_repair_action())
    }

    fn rebuild_fts(
        &mut self,
        _permit: &ExclusiveMaintenancePermit,
        _index: &FtsIndexId,
    ) -> Result<(), DriverMaintenanceError> {
        Err(unsupported_repair_action())
    }

    fn restore(
        &mut self,
        _permit: &ExclusiveMaintenancePermit,
        _artifact: &VerifiedMaintenanceArtifact,
    ) -> Result<(), DriverMaintenanceError> {
        Err(unsupported_repair_action())
    }

    fn compact(
        &mut self,
        permit: &ExclusiveMaintenancePermit,
        _mode: CompactionMode,
    ) -> Result<(), DriverMaintenanceError> {
        let outcome = self
            .coordinator
            .coordinate(self.probe, &mut self.driver, Some(permit));
        let accepted = matches!(
            &outcome,
            RepairOutcome::Completed { .. }
                | RepairOutcome::Rejected {
                    reason: RejectionReason::AuthoritativeQuarantined,
                    ..
                }
        );
        self.outcome = Some(outcome);
        if accepted {
            Ok(())
        } else {
            Err(DriverMaintenanceError {
                code: "repair_evidence_not_accepted",
                retryable: false,
            })
        }
    }

    fn replace_shard(
        &mut self,
        _permit: &ExclusiveMaintenancePermit,
        _artifact: &VerifiedMaintenanceArtifact,
    ) -> Result<(), DriverMaintenanceError> {
        Err(unsupported_repair_action())
    }
}

fn unsupported_repair_action() -> DriverMaintenanceError {
    DriverMaintenanceError {
        code: "unsupported_repair_action",
        retryable: false,
    }
}

struct EvidenceAuthority {
    replacement: StoreRuntimeRegistryPublicationV1,
}

impl CanonicalRegistryAuthority for EvidenceAuthority {
    fn request_replacement(
        &self,
        request: &ReplacementPublicationRequest,
    ) -> Result<ReplacementPublicationReceipt, MaintenanceError> {
        if request.kind != ReplacementPublicationKind::Reopen {
            return Err(MaintenanceError::CanonicalAuthority {
                stage: "evidence authority accepts reopen only",
            });
        }
        Ok(ReplacementPublicationReceipt {
            request: request.clone(),
            publication: self.replacement.clone(),
        })
    }
}

struct LifecycleState {
    publication: StoreRuntimeRegistryPublicationV1,
    state: RuntimeMaintenanceStateV1,
}

struct EvidenceLifecycle {
    state: Mutex<LifecycleState>,
}

impl EvidenceLifecycle {
    fn ready(publication: StoreRuntimeRegistryPublicationV1) -> Self {
        Self {
            state: Mutex::new(LifecycleState {
                publication,
                state: RuntimeMaintenanceStateV1::Ready,
            }),
        }
    }
}

impl MaintenanceLifecycle for EvidenceLifecycle {
    fn publication(&self) -> StoreRuntimeRegistryPublicationV1 {
        self.state
            .lock()
            .expect("evidence lifecycle poisoned")
            .publication
            .clone()
    }

    fn state(&self) -> RuntimeMaintenanceStateV1 {
        self.state
            .lock()
            .expect("evidence lifecycle poisoned")
            .state
    }

    fn stop_admissions_and_begin_drain(
        &self,
        expected: &StoreRuntimeRegistryPublicationV1,
    ) -> Result<(), MaintenanceError> {
        let mut state = self.state.lock().map_err(|_| MaintenanceError::Lifecycle {
            stage: "evidence lifecycle poisoned",
        })?;
        if &state.publication != expected {
            return Err(MaintenanceError::Lifecycle {
                stage: "evidence publication mismatch",
            });
        }
        state.state = RuntimeMaintenanceStateV1::Draining;
        Ok(())
    }

    fn drain_blockers(
        &self,
        _expected: &StoreRuntimeRegistryPublicationV1,
    ) -> Result<DrainBlockers, MaintenanceError> {
        Ok(DrainBlockers::default())
    }

    fn enter_exclusive(
        &self,
        _expected: &StoreRuntimeRegistryPublicationV1,
        _owner: MaintenanceOwnerId,
    ) -> Result<(), MaintenanceError> {
        self.state
            .lock()
            .map_err(|_| MaintenanceError::Lifecycle {
                stage: "evidence lifecycle poisoned",
            })?
            .state = RuntimeMaintenanceStateV1::ExclusiveMaintenance;
        Ok(())
    }

    fn reopen(
        &self,
        _permit: ExclusiveMaintenancePermit,
        receipt: ReplacementPublicationReceipt,
    ) -> Result<ReplacementPublicationReceipt, MaintenanceError> {
        let mut state = self.state.lock().map_err(|_| MaintenanceError::Lifecycle {
            stage: "evidence lifecycle poisoned",
        })?;
        state.publication = receipt.publication.clone();
        state.state = RuntimeMaintenanceStateV1::Ready;
        Ok(receipt)
    }

    fn fault(
        &self,
        _permit: ExclusiveMaintenancePermit,
        receipt: ReplacementPublicationReceipt,
    ) -> Result<ReplacementPublicationReceipt, MaintenanceError> {
        let mut state = self.state.lock().map_err(|_| MaintenanceError::Lifecycle {
            stage: "evidence lifecycle poisoned",
        })?;
        state.publication = receipt.publication.clone();
        state.state = RuntimeMaintenanceStateV1::Faulted;
        Ok(receipt)
    }
}

struct NeverCancel;

impl Cancellation for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}

struct EvidenceRestoreAuthority {
    published: Arc<Mutex<Vec<PublishedRestore>>>,
}

impl RestorePublicationAuthority for EvidenceRestoreAuthority {
    type Error = EvidenceError;

    fn publish_restored(
        &mut self,
        _permit: ExclusiveMaintenancePermit,
        _recovery_source: FrozenWatermarkVectorV1,
        _replacements: Vec<StoreRuntimeBindingV1>,
        published: PublishedRestore,
    ) -> Result<(), Self::Error> {
        self.published
            .lock()
            .map_err(|_| EvidenceError::Runtime("restore capture was poisoned".to_owned()))?
            .push(published);
        Ok(())
    }
}

fn evidence_permit(
    binding: StoreRuntimeBindingV1,
    sequence: u64,
) -> Result<ExclusiveMaintenancePermit, EvidenceError> {
    let owner = MaintenanceOwnerId::new(sequence)
        .ok_or_else(|| EvidenceError::Runtime("maintenance owner was invalid".to_owned()))?;
    let publication = publication(
        &format!("publication.storage.evidence.{sequence}"),
        binding,
        sequence,
    )?;
    let proof = DrainedStateProof::observe(publication.clone(), DrainBlockers::default())
        .map_err(runtime_debug)?;
    ExclusiveMaintenancePermit::issue_after_drain(owner, publication, proof).map_err(runtime_debug)
}

fn logical_evidence(
    database: &Path,
    tables: &[String],
) -> Result<LogicalSqliteEvidence, EvidenceError> {
    let connection = Connection::open_with_flags(
        database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_PRIVATE_CACHE,
    )?;
    let integrity_rows = connection
        .prepare("PRAGMA integrity_check")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if integrity_rows.len() != 1 || !integrity_rows[0].eq_ignore_ascii_case("ok") {
        return Err(EvidenceError::Runtime(
            "logical evidence integrity_check was not healthy".to_owned(),
        ));
    }
    let schema_rows = connection
        .prepare(
            "SELECT type, name, tbl_name, sql FROM sqlite_master
             WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
        )?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut table_evidence = Vec::new();
    for table in tables {
        if !valid_identifier(table) {
            return Err(EvidenceError::Refused(
                "fixture evidence table is not a safe identifier".to_owned(),
            ));
        }
        let count: i64 =
            connection.query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |row| {
                row.get(0)
            })?;
        table_evidence.push(TableEvidence {
            table_id: table.clone(),
            row_count: u64::try_from(count).unwrap_or(0),
        });
    }
    let integrity_json = serde_json::to_string(&integrity_rows)?;
    let schema_json = serde_json::to_string(&schema_rows)?;
    Ok(LogicalSqliteEvidence {
        schema: LOGICAL_SCHEMA,
        integrity: IntegrityEvidence {
            status: "ok",
            result_sha256: sha256_hex(integrity_json.as_bytes()),
            result_row_count: integrity_rows.len(),
        },
        schema_sha256: sha256_hex(schema_json.as_bytes()),
        tables: table_evidence,
        fts: Vec::new(),
    })
}

fn load_manifest(fixture: &Path) -> Result<FixtureManifest, EvidenceError> {
    let bytes = fs::read(fixture.join(FIXTURE_MANIFEST))?;
    let manifest: FixtureManifest = serde_json::from_slice(&bytes)?;
    if manifest.schema_version != 1 {
        return Err(EvidenceError::Refused(
            "fixture schema_version must be 1".to_owned(),
        ));
    }
    let _ = (
        &manifest.project_root,
        &manifest.profile_root,
        &manifest.fts_queries,
    );
    Ok(manifest)
}

fn validate_command_paths(command: &EvidenceCommand) -> Result<(), EvidenceError> {
    if !command.fixture.is_dir() {
        return Err(EvidenceError::Refused(
            "--fixture must be an existing directory".to_owned(),
        ));
    }
    if command.output.exists() {
        return Err(EvidenceError::Refused(
            "--output must not already exist".to_owned(),
        ));
    }
    let output_parent = command
        .output
        .parent()
        .ok_or_else(|| EvidenceError::Refused("--output has no parent".to_owned()))?;
    if !output_parent.is_dir() || command.output.starts_with(&command.fixture) {
        return Err(EvidenceError::Refused(
            "--output must be outside the copied fixture".to_owned(),
        ));
    }
    Ok(())
}

fn create_work_root(command: &EvidenceCommand) -> Result<PathBuf, EvidenceError> {
    let parent = command
        .output
        .parent()
        .ok_or_else(|| EvidenceError::Refused("--output has no parent".to_owned()))?;
    let root = parent.join(format!("{}.work", command.gate.id()));
    fs::create_dir(&root)?;
    Ok(root)
}

fn fixture_file(root: &Path, relative: &str) -> Result<PathBuf, EvidenceError> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(EvidenceError::Refused(
            "fixture database must be a contained relative path".to_owned(),
        ));
    }
    let path = root.join(relative);
    if fs::symlink_metadata(&path)?.file_type().is_symlink() || !path.is_file() {
        return Err(EvidenceError::Refused(
            "fixture database must be a regular file".to_owned(),
        ));
    }
    Ok(path)
}

pub(crate) fn fingerprint_tree(root: &Path) -> Result<String, EvidenceError> {
    fn visit(
        root: &Path,
        directory: &Path,
        entries: &mut Vec<FingerprintEntry>,
    ) -> Result<(), EvidenceError> {
        let mut children = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
        children.sort_by_key(fs::DirEntry::file_name);
        for child in children {
            let path = child.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(EvidenceError::Refused(
                    "fixture identity rejects symlinks".to_owned(),
                ));
            }
            if metadata.is_dir() {
                visit(root, &path, entries)?;
            } else if metadata.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .map_err(|_| EvidenceError::Refused("fixture path escaped root".to_owned()))?;
                entries.push(FingerprintEntry {
                    path: relative.to_string_lossy().replace('\\', "/"),
                    sha256: sha256_hex(&fs::read(path)?),
                });
            } else {
                return Err(EvidenceError::Refused(
                    "fixture identity rejects special files".to_owned(),
                ));
            }
        }
        Ok(())
    }

    let mut entries = Vec::new();
    visit(root, root, &mut entries)?;
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(sha256_hex(serde_json::to_string(&entries)?.as_bytes()))
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            use fmt::Write as _;
            write!(output, "{byte:02x}").expect("writing to String cannot fail");
            output
        })
}

fn publish_output(path: &Path, bytes: &[u8]) -> Result<(), EvidenceError> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn reader_locator(
    binding: &StoreRuntimeBindingV1,
    database: &Path,
) -> Result<ExistingReaderLocator, EvidenceError> {
    let digest = LocatorDigest::new(format!("sha256:{}", sha256_hex(&fs::read(database)?)))
        .map_err(runtime)?;
    ExistingReaderLocator::new(
        binding.clone(),
        VerifiedStoreLocatorV1::new(binding.shard_id.clone(), binding.incarnation, digest),
        database.to_path_buf(),
    )
    .map_err(runtime)
}

fn publication(
    id: &str,
    binding: StoreRuntimeBindingV1,
    published_at: u64,
) -> Result<StoreRuntimeRegistryPublicationV1, EvidenceError> {
    let publication_id = RuntimePublicationIdV1::new(id.to_owned()).map_err(runtime)?;
    Ok(serde_json::from_value(serde_json::json!({
        "publication_id": publication_id,
        "binding": binding,
        "published_at": published_at
    }))?)
}

fn stronger_binding(
    binding: &StoreRuntimeBindingV1,
    increment: u64,
) -> Result<StoreRuntimeBindingV1, EvidenceError> {
    let mut value = serde_json::to_value(binding)?;
    let incarnation = value
        .get_mut("incarnation")
        .and_then(|field| field.as_u64())
        .ok_or_else(|| EvidenceError::Runtime("binding incarnation was invalid".to_owned()))?;
    let epoch = value
        .get_mut("authority_epoch")
        .and_then(|field| field.as_u64())
        .ok_or_else(|| EvidenceError::Runtime("binding authority epoch was invalid".to_owned()))?;
    value["incarnation"] = serde_json::json!(incarnation.saturating_add(increment));
    value["authority_epoch"] = serde_json::json!(epoch.saturating_add(increment));
    Ok(serde_json::from_value(value)?)
}

fn watermark(binding: &StoreRuntimeBindingV1, sequence: u64) -> ShardWatermarkV1 {
    ShardWatermarkV1 {
        shard_id: binding.shard_id.clone(),
        incarnation: binding.incarnation,
        authority_epoch: binding.authority_epoch,
        commit_sequence: CommitSequenceV1(sequence),
    }
}

fn empty_reader_snapshot() -> ReaderPoolSnapshot {
    ReaderPoolSnapshot {
        state: ReaderPoolState::Ready,
        general_workers: 0,
        available_general: 0,
        health_workers: 0,
        available_health: 0,
        leased_general: 0,
        leased_health: 0,
    }
}

fn integrity_name(result: &IntegrityResult) -> &'static str {
    match result {
        IntegrityResult::Healthy => "healthy",
        IntegrityResult::Corrupt { .. } => "corrupt",
    }
}

fn corruption_name(class: CorruptionClass) -> &'static str {
    match class {
        CorruptionClass::Healthy => "healthy",
        CorruptionClass::DerivedFtsOnly => "derived_fts",
        CorruptionClass::Authoritative => "authoritative",
        CorruptionClass::Indeterminate => "indeterminate",
    }
}

fn writer_state_name(state: WriterState) -> &'static str {
    match state {
        WriterState::Ready => "ready",
        WriterState::Faulted | WriterState::Draining | WriterState::Closed => "faulted",
    }
}

fn operation_id(value: &str) -> Result<StoreOperationIdV1, EvidenceError> {
    StoreOperationIdV1::new(value.to_owned())
        .map_err(|_| EvidenceError::Runtime("operation identity was invalid".to_owned()))
}

fn optional_count(values: &BTreeMap<String, String>, name: &str) -> Result<u32, EvidenceError> {
    values
        .get(name)
        .map(|value| {
            value
                .parse::<u32>()
                .map_err(|_| EvidenceError::InvalidArgument("count must be a non-negative integer"))
        })
        .transpose()
        .map(Option::unwrap_or_default)
}

fn absolute_path(value: &str, name: &'static str) -> Result<PathBuf, EvidenceError> {
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(EvidenceError::InvalidArgument(name));
    }
    Ok(path)
}

fn valid_hex(value: &str, minimum: usize, maximum: usize) -> bool {
    (minimum..=maximum).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn default_tables() -> Vec<String> {
    vec!["evidence_rows".to_owned()]
}

fn runtime(error: impl fmt::Display) -> EvidenceError {
    EvidenceError::Runtime(error.to_string())
}

fn runtime_debug(error: impl fmt::Debug) -> EvidenceError {
    EvidenceError::Runtime(format!("{error:?}"))
}

#[cfg(test)]
mod tests;
