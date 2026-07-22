use std::{
    collections::BTreeMap,
    convert::Infallible,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use rusqlite::Connection;
use tempfile::TempDir;
use tracedecay_store::{
    FrozenWatermarkVectorV1, ShardWatermarkV1, StoreOperationIdV1, StoreRuntimeBindingV1,
};

use crate::{
    WriterState,
    backup::{
        ArtifactIdentity, BackupFilesystem, BackupFilesystemError, BackupRestoreError,
        BackupRestoreOrchestrator, BackupRoot, BackupSetId, Cancellation, DeletionState,
        FilesystemBackupStore, OnlineBackupSource, PrivacyClass, PublishedRestore,
        RestorePublicationAuthority, SchemaVersion, SqliteBackupOptions, SqliteOnlineBackupDriver,
        StagingId, StoredBackupManifest,
    },
    maintenance::{ExclusiveMaintenancePermit, FtsIndexId, MaintenanceOwnerId, SqliteFtsIndex},
    reader::{ReaderPoolSnapshot, ReaderPoolState},
    repair::{
        CorruptionClass, CorruptionEvidence, CorruptionProbe, FilesystemQuarantineStore,
        MaintenanceAuthorization, QuarantineReceipt, QuarantineStore, RejectionReason,
        RepairCoordinator, RepairFault, RepairOutcome, SqliteCorruptionProbe, SqliteRepairDriver,
    },
    runtime::SqliteDoctorHealthLane,
};

fn sqlite_nonnegative_u64(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<u64> {
    let value = row.get::<_, i64>(idx)?;
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            idx,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/storage_runtime_evidence")
}

fn fixture(name: &str) -> PathBuf {
    let path = fixture_root().join(name);
    assert!(path.is_file(), "missing S11 fixture {}", path.display());
    path
}

fn copy_fixture(directory: &TempDir, name: &str) -> PathBuf {
    let destination = directory.path().join(name);
    fs::copy(fixture(name), &destination).expect("copy S11 fixture");
    destination
}

fn binding(incarnation: u64, authority_epoch: u64) -> StoreRuntimeBindingV1 {
    serde_json::from_value(serde_json::json!({
        "shard_id": {
            "brain_id": "brain.s11.evidence",
            "profile_id": "profile.s11.evidence",
            "scope": {
                "kind": "project",
                "project_id": "project.s11.evidence"
            }
        },
        "incarnation": incarnation,
        "authority_epoch": authority_epoch
    }))
    .expect("valid S11 binding")
}

fn operation_id(value: &str) -> StoreOperationIdV1 {
    StoreOperationIdV1::try_from(value.to_owned()).expect("valid S11 operation id")
}

fn watermark() -> ShardWatermarkV1 {
    serde_json::from_value(serde_json::json!({
        "shard_id": binding(7, 19).shard_id,
        "incarnation": 7,
        "authority_epoch": 19,
        "commit_sequence": 41
    }))
    .expect("valid S11 watermark")
}

struct FixtureProbe(CorruptionEvidence);

impl CorruptionProbe for FixtureProbe {
    fn evidence(&self) -> Result<CorruptionEvidence, RepairFault> {
        Ok(self.0.clone())
    }
}

struct Authorization(StoreRuntimeBindingV1);

impl MaintenanceAuthorization for Authorization {
    fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.0
    }
}

struct NoQuarantine;

impl QuarantineStore for NoQuarantine {
    fn lookup(
        &self,
        _diagnosis: &crate::repair::CorruptionDiagnosis,
        _receipt_id: &StoreOperationIdV1,
    ) -> Result<Option<QuarantineReceipt>, RepairFault> {
        Ok(None)
    }

    fn preserve(
        &mut self,
        _diagnosis: &crate::repair::CorruptionDiagnosis,
        _receipt_id: &StoreOperationIdV1,
    ) -> Result<QuarantineReceipt, RepairFault> {
        Err(RepairFault::new(
            "unexpected_quarantine",
            "fixture expected derived FTS handling",
        ))
    }
}

struct NeverCancel;

impl Cancellation for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}

#[test]
fn checked_wal_family_exposes_pressure_and_a_snapshot_blocker() {
    let directory = TempDir::new().expect("create WAL fixture root");
    let database = copy_fixture(&directory, "wal-pressure.sqlite3");
    fs::copy(
        fixture("wal-pressure.sqlite3-wal"),
        PathBuf::from(format!("{}-wal", database.display())),
    )
    .expect("copy WAL sidecar");

    let writer = Connection::open(&database).expect("open WAL fixture writer");
    writer
        .pragma_update(None, "wal_autocheckpoint", 0_i64)
        .expect("disable fixture auto-checkpoint");
    let blocker = Connection::open(&database).expect("open snapshot blocker");
    blocker
        .execute_batch("BEGIN")
        .expect("begin blocker snapshot");
    assert_eq!(
        blocker
            .query_row("SELECT count(*) FROM events", [], |row| {
                sqlite_nonnegative_u64(row, 0)
            })
            .expect("read blocker snapshot"),
        129
    );
    writer
        .execute("INSERT INTO events VALUES (130, zeroblob(700))", [])
        .expect("append pressure frame");

    let health = Connection::open(&database).expect("open health lane");
    let snapshot = SqliteDoctorHealthLane::from_health_connection(binding(7, 19), health)
        .inspect(
            WriterState::Ready,
            ReaderPoolSnapshot {
                state: ReaderPoolState::Ready,
                general_workers: 1,
                available_general: 0,
                health_workers: 1,
                available_health: 0,
                leased_general: 1,
                leased_health: 1,
            },
            false,
        )
        .expect("inspect WAL fixture");

    assert!(snapshot.wal.enabled);
    assert!(snapshot.wal.log_frames > snapshot.wal.checkpointed_frames);
    assert_eq!(snapshot.leased_readers, 2);
    blocker.execute_batch("ROLLBACK").expect("release blocker");
}

#[test]
fn crash_after_commit_replays_the_durable_repair_receipt() {
    let directory = TempDir::new().expect("create crash fixture root");
    let database = copy_fixture(&directory, "crash-after-repair-commit.sqlite3");
    let connection = &mut Connection::open(database).expect("open crash fixture");
    let index = SqliteFtsIndex::new(
        FtsIndexId::new("fts.s11.crash").expect("valid FTS id"),
        "documents_fts",
    )
    .expect("valid fixture FTS table");
    let evidence = CorruptionEvidence {
        binding: binding(7, 19),
        evidence_id: operation_id("evidence.s11.crash"),
        observations: vec![crate::repair::CorruptionObservation::DerivedFts],
    };
    let coordinator = RepairCoordinator::new(operation_id("receipt.s11.crash"));
    let indexes = [index];
    let mut driver = SqliteRepairDriver::new(connection, &indexes, NoQuarantine);

    let outcome = coordinator.coordinate(&FixtureProbe(evidence), &mut driver, None);

    assert!(matches!(
        outcome,
        RepairOutcome::Completed { replayed: true, .. }
    ));
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM documents_fts WHERE documents_fts MATCH 'needle'",
                [],
                |row| sqlite_nonnegative_u64(row, 0),
            )
            .expect("query committed repair"),
        1
    );
}

#[test]
fn stale_fts_fixture_is_rebuilt_with_its_receipt_in_one_transaction() {
    let directory = TempDir::new().expect("create FTS fixture root");
    let database = copy_fixture(&directory, "fts-stale.sqlite3");
    let connection = &mut Connection::open(database).expect("open stale FTS fixture");
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM documents_fts WHERE documents_fts MATCH 'needle'",
                [],
                |row| sqlite_nonnegative_u64(row, 0),
            )
            .expect("query stale FTS"),
        0
    );
    let index = SqliteFtsIndex::new(
        FtsIndexId::new("fts.s11.stale").expect("valid FTS id"),
        "documents_fts",
    )
    .expect("valid fixture FTS table");
    let evidence = CorruptionEvidence {
        binding: binding(7, 19),
        evidence_id: operation_id("evidence.s11.fts"),
        observations: vec![crate::repair::CorruptionObservation::DerivedFts],
    };
    let authorization = Authorization(binding(7, 19));
    let coordinator = RepairCoordinator::new(operation_id("receipt.s11.fts"));
    let indexes = [index];
    let mut driver = SqliteRepairDriver::new(connection, &indexes, NoQuarantine);

    let outcome =
        coordinator.coordinate(&FixtureProbe(evidence), &mut driver, Some(&authorization));

    assert!(matches!(
        outcome,
        RepairOutcome::Completed {
            replayed: false,
            ..
        }
    ));
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM documents_fts WHERE documents_fts MATCH 'needle'",
                [],
                |row| sqlite_nonnegative_u64(row, 0),
            )
            .expect("query repaired FTS"),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM tracedecay_repair_receipts",
                [],
                |row| sqlite_nonnegative_u64(row, 0),
            )
            .expect("query repair receipt"),
        1
    );
}

#[test]
fn authoritative_corruption_is_classified_and_preserved_in_quarantine() {
    let directory = TempDir::new().expect("create quarantine fixture root");
    let database = copy_fixture(&directory, "authoritative-corrupt.sqlite3");
    let connection = &mut Connection::open(&database).expect("open corrupt fixture");
    let evidence = SqliteCorruptionProbe::new(
        connection,
        binding(7, 19),
        operation_id("evidence.s11.authoritative"),
        &[],
    )
    .evidence()
    .expect("probe authoritative corruption");
    let coordinator = RepairCoordinator::new(operation_id("receipt.s11.quarantine"));
    let diagnosis = coordinator
        .diagnose(&FixtureProbe(evidence.clone()))
        .expect("classify authoritative corruption");
    assert_eq!(diagnosis.class, CorruptionClass::Authoritative);

    let quarantine_root = directory.path().join("quarantine");
    let quarantine = FilesystemQuarantineStore::new(quarantine_root.clone(), database.clone())
        .expect("open quarantine capability");
    let authorization = Authorization(binding(7, 19));
    let indexes = [];
    let mut driver = SqliteRepairDriver::new(connection, &indexes, quarantine);
    let outcome =
        coordinator.coordinate(&FixtureProbe(evidence), &mut driver, Some(&authorization));
    let receipt = match outcome {
        RepairOutcome::Rejected {
            reason: RejectionReason::AuthoritativeQuarantined,
            quarantine: Some(receipt),
            ..
        } => receipt,
        other => panic!("unexpected quarantine outcome: {other:?}"),
    };
    let token = receipt
        .evidence_reference
        .strip_prefix("quarantine:")
        .expect("opaque quarantine token");
    assert_eq!(
        fs::read(quarantine_root.join(token).join("database.sqlite3"))
            .expect("read quarantined evidence"),
        fs::read(&database).expect("read original corrupt fixture"),
    );
    assert!(database.is_file());
}

#[derive(Clone)]
struct CapturingAuthority {
    captured: Arc<Mutex<Option<CapturedPublication>>>,
}

#[derive(Clone)]
struct CapturedPublication {
    recovery_source: FrozenWatermarkVectorV1,
    replacements: Vec<StoreRuntimeBindingV1>,
    published: PublishedRestore,
}

impl RestorePublicationAuthority for CapturingAuthority {
    type Error = Infallible;

    fn publish_restored(
        &mut self,
        permit: ExclusiveMaintenancePermit,
        recovery_source: FrozenWatermarkVectorV1,
        replacements: Vec<StoreRuntimeBindingV1>,
        published: PublishedRestore,
    ) -> Result<(), Self::Error> {
        assert_eq!(permit.binding(), &binding(7, 19));
        *self.captured.lock().expect("lock publication capture") = Some(CapturedPublication {
            recovery_source,
            replacements,
            published,
        });
        Ok(())
    }
}

#[test]
fn online_backup_stages_restore_then_publishes_only_higher_bindings() {
    let directory = TempDir::new().expect("create backup fixture root");
    let source = Connection::open(copy_fixture(&directory, "online-backup-source.sqlite3"))
        .expect("open backup source");
    let source_watermark = watermark();
    let required =
        FrozenWatermarkVectorV1::new([source_watermark.clone()]).expect("valid frozen watermark");
    let root_path = directory.path().join("backup-root");
    let root = BackupRoot::open(root_path.clone()).expect("open backup root");
    let capture = Arc::new(Mutex::new(None));
    let authority = CapturingAuthority {
        captured: Arc::clone(&capture),
    };
    let online_source = OnlineBackupSource::from_writer_connection(source_watermark, &source);
    let mut driver = SqliteOnlineBackupDriver::new(
        root.clone(),
        [online_source],
        SchemaVersion(11),
        PrivacyClass::Project,
        DeletionState::Live,
        BTreeMap::new(),
        SqliteBackupOptions::default(),
        authority,
    )
    .expect("construct online backup driver");
    let mut filesystem = FilesystemBackupStore::new(root.clone());
    let backup_set = BackupSetId::new("set.s11.online").expect("valid backup set");
    let stored = BackupRestoreOrchestrator::new(&mut driver, &mut filesystem)
        .backup(&required, backup_set.clone(), &NeverCancel)
        .expect("create online backup");
    let replacement = binding(8, 20);
    let permit = ExclusiveMaintenancePermit::issue(
        MaintenanceOwnerId::new(11).expect("valid maintenance owner"),
        binding(7, 19),
    );

    let restored = BackupRestoreOrchestrator::new(&mut driver, &mut filesystem)
        .restore(&backup_set, permit, vec![replacement.clone()], &NeverCancel)
        .expect("stage, verify, and publish restore");

    assert_eq!(restored, vec![replacement.clone()]);
    let captured = capture
        .lock()
        .expect("lock publication capture")
        .clone()
        .expect("publication was acknowledged");
    assert_eq!(captured.recovery_source, required);
    assert_eq!(captured.replacements, vec![replacement]);
    let expected_digest = stored
        .manifest
        .artifacts
        .iter()
        .find_map(|artifact| {
            matches!(&artifact.identity, ArtifactIdentity::Store(_)).then_some(artifact.sha256)
        })
        .expect("store artifact digest");
    assert_eq!(
        root.published_store_sha256(&captured.published, &binding(7, 19).shard_id)
            .expect("hash published restore"),
        expected_digest
    );
    assert!(
        fs::read_dir(root_path.join(".restore"))
            .expect("read restore staging")
            .next()
            .is_none()
    );
    assert!(
        root_path
            .join("published")
            .join(captured.published.token())
            .is_dir()
    );
}

struct DigestMismatchFilesystem {
    inner: FilesystemBackupStore,
    mismatched_store: Vec<u8>,
}

impl BackupFilesystem for DigestMismatchFilesystem {
    type Error = BackupFilesystemError;

    fn begin_backup(&mut self, backup: &BackupSetId) -> Result<StagingId, Self::Error> {
        self.inner.begin_backup(backup)
    }

    fn write_staged(
        &mut self,
        staging: &StagingId,
        artifact: &ArtifactIdentity,
        bytes: &[u8],
    ) -> Result<(), Self::Error> {
        self.inner.write_staged(staging, artifact, bytes)
    }

    fn read_staged(
        &self,
        staging: &StagingId,
        artifact: &ArtifactIdentity,
    ) -> Result<Vec<u8>, Self::Error> {
        if matches!(artifact, ArtifactIdentity::Store(_)) {
            Ok(self.mismatched_store.clone())
        } else {
            self.inner.read_staged(staging, artifact)
        }
    }

    fn write_manifest(
        &mut self,
        staging: &StagingId,
        manifest: &StoredBackupManifest,
    ) -> Result<(), Self::Error> {
        self.inner.write_manifest(staging, manifest)
    }

    fn read_staged_manifest(
        &self,
        staging: &StagingId,
    ) -> Result<StoredBackupManifest, Self::Error> {
        self.inner.read_staged_manifest(staging)
    }

    fn commit_backup(
        &mut self,
        staging: StagingId,
        backup: &BackupSetId,
    ) -> Result<(), Self::Error> {
        self.inner.commit_backup(staging, backup)
    }

    fn abort_staging(&mut self, staging: StagingId) {
        self.inner.abort_staging(staging);
    }

    fn load_manifest(&self, backup: &BackupSetId) -> Result<StoredBackupManifest, Self::Error> {
        self.inner.load_manifest(backup)
    }

    fn read_backup(
        &self,
        backup: &BackupSetId,
        artifact: &ArtifactIdentity,
    ) -> Result<Vec<u8>, Self::Error> {
        self.inner.read_backup(backup, artifact)
    }
}

#[test]
fn valid_sqlite_with_wrong_digest_is_rejected_before_backup_publication() {
    let directory = TempDir::new().expect("create digest fixture root");
    let source = Connection::open(copy_fixture(&directory, "online-backup-source.sqlite3"))
        .expect("open backup source");
    let source_watermark = watermark();
    let required =
        FrozenWatermarkVectorV1::new([source_watermark.clone()]).expect("valid frozen watermark");
    let root_path = directory.path().join("backup-root");
    let root = BackupRoot::open(root_path.clone()).expect("open backup root");
    let online_source = OnlineBackupSource::from_writer_connection(source_watermark, &source);
    let mut driver = SqliteOnlineBackupDriver::new(
        root.clone(),
        [online_source],
        SchemaVersion(11),
        PrivacyClass::Project,
        DeletionState::Live,
        BTreeMap::new(),
        SqliteBackupOptions::default(),
        CapturingAuthority {
            captured: Arc::new(Mutex::new(None)),
        },
    )
    .expect("construct online backup driver");
    let mut filesystem = DigestMismatchFilesystem {
        inner: FilesystemBackupStore::new(root),
        mismatched_store: fs::read(fixture("online-backup-digest-mismatch.sqlite3"))
            .expect("read digest mismatch fixture"),
    };
    let backup_set = BackupSetId::new("set.s11.digest-mismatch").expect("valid backup set");

    let error = BackupRestoreOrchestrator::new(&mut driver, &mut filesystem)
        .backup(&required, backup_set.clone(), &NeverCancel)
        .expect_err("digest mismatch must fail");

    assert!(matches!(
        error,
        BackupRestoreError::ArtifactDigestMismatch(_)
    ));
    assert!(!root_path.join("sets").join(backup_set.as_str()).exists());
    assert!(
        fs::read_dir(root_path.join(".staging"))
            .expect("read backup staging")
            .next()
            .is_none()
    );
}
