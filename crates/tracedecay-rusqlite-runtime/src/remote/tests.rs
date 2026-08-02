use std::sync::Arc;

use rusqlite::Savepoint;
use serde_json::json;
use tempfile::TempDir;
use tracedecay_application::remote::{
    capture::{
        AdmittedRemoteCaptureV1, RemoteCaptureDispositionV1, RemoteCapturePersistenceErrorV1,
        RemoteCapturePortV1, RemoteCaptureSequenceV1, RemoteWriterAuthorityV1,
    },
    replay::{
        RemoteReplayFrameLookupPortV1, RemoteReplaySpoolPortV1, RemoteReplayStateV1,
        RemoteReplayTransactionOutcomeV1, RemoteReplayTransactionPortV1, RemoteReplayTransitionV1,
    },
};
use tracedecay_domain::{
    ComponentVersion, DurableObservationV1, EntityId, LocatorDigest, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    PayloadReferenceV1, ProviderId, RetentionClass, SanitizationReceiptId,
    SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
    SessionId, UtcMicros,
};
use tracedecay_store::{
    AdmissionConfigV1, RepositoryWritePayloadV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1,
    StorageRuntimeErrorV1, StoreIncarnationV1, VerifiedStoreLocatorV1,
};

use crate::{
    ExistingWriterLocator, PersistentWriter, StorageOperationExecutor,
    migration_sql::{MigrationSqlWriteAuthority, MigrationSqlWriteIntent},
    reader::{ExistingReaderLocator, ReaderPool, ReaderQueryExecutor},
};

use super::*;

struct NoWrites;

impl StorageOperationExecutor for NoWrites {
    fn execute(
        &mut self,
        _savepoint: &Savepoint<'_>,
        _payload: &RepositoryWritePayloadV1,
    ) -> rusqlite::Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
struct NoReads;

impl ReaderQueryExecutor for NoReads {
    fn execute_read(
        &mut self,
        _snapshot: &rusqlite::Transaction<'_>,
        _request: &RuntimeReadRequestV1,
    ) -> Result<RuntimeReadOutcomeV1, StorageRuntimeErrorV1> {
        unreachable!("migration SQL queries bypass the product read executor")
    }
}

struct AllowSchema;

impl MigrationSqlWriteAuthority for AllowSchema {
    fn verify(&self, _intent: MigrationSqlWriteIntent) -> Result<(), MigrationSqlError> {
        Ok(())
    }
}

struct Fixture {
    _directory: TempDir,
    _writer: PersistentWriter,
    _readers: ReaderPool<NoReads>,
    handle: MigrationSqlHandle,
    binding: StoreRuntimeBindingV1,
}

fn fixture() -> Fixture {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("remote.sqlite3");
    rusqlite::Connection::open(&path).unwrap();
    let path = path.canonicalize().unwrap();
    let binding: StoreRuntimeBindingV1 = serde_json::from_value(serde_json::json!({
        "shard_id": {
            "brain_id": "brain.remote",
            "profile_id": "profile.remote",
            "scope": { "kind": "project", "project_id": "project.remote" }
        },
        "incarnation": 3,
        "authority_epoch": 11
    }))
    .unwrap();
    let locator = VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        StoreIncarnationV1::new(3).unwrap(),
        LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
    );
    let writer = PersistentWriter::start(
        ExistingWriterLocator::new(binding.clone(), locator.clone(), path.clone()).unwrap(),
        AdmissionConfigV1::default(),
        NoWrites,
    )
    .unwrap();
    let readers = ReaderPool::start(
        ExistingReaderLocator::new(binding.clone(), locator, path).unwrap(),
        AdmissionConfigV1::default().readers,
        NoReads,
    )
    .unwrap();
    let handle = MigrationSqlHandle::attach(&writer, &readers)
        .unwrap()
        .with_write_authority(Arc::new(AllowSchema))
        .unwrap();
    Fixture {
        _directory: directory,
        _writer: writer,
        _readers: readers,
        handle,
        binding,
    }
}

struct TestKeyring(Arc<RemoteSpoolKeyV1>);

impl RemoteSpoolKeyringV1 for TestKeyring {
    fn active_key(&self) -> Result<Arc<RemoteSpoolKeyV1>, RemoteSqliteStorageErrorV1> {
        Ok(Arc::clone(&self.0))
    }

    fn key(
        &self,
        revision: u64,
    ) -> Result<Option<Arc<RemoteSpoolKeyV1>>, RemoteSqliteStorageErrorV1> {
        Ok((revision == self.0.revision()).then(|| Arc::clone(&self.0)))
    }
}

fn storage(fixture: &Fixture) -> RemoteSqliteStorageV1 {
    install_remote_schema_v1(&fixture.handle).unwrap();
    RemoteSqliteStorageV1::attach(
        fixture.handle.clone(),
        fixture.binding.clone(),
        Arc::new(TestKeyring(Arc::new(
            RemoteSpoolKeyV1::from_secret_bytes(7, vec![7; 32]).unwrap(),
        ))),
    )
    .unwrap()
}

fn writer() -> RemoteWriterAuthorityV1 {
    serde_json::from_value(json!({
        "project_id": "project.remote",
        "scope": {
            "project_id": "project.remote",
            "repository_id": "repository.remote",
            "worktree_id": "worktree.remote",
            "reference": "refs/heads/main",
            "snapshot_id": "snapshot.remote"
        },
        "authority": {
            "fence": {
                "brain_id": "brain.remote",
                "shard_id": "shard.remote",
                "generation_id": "generation.remote",
                "placement_revision": 1,
                "authority_epoch": 11,
                "authority_node_id": "node.authority"
            },
            "credential_revision": 1,
            "observed_at": 10
        }
    }))
    .unwrap()
}

fn observation() -> DurableObservationV1 {
    let payload = json!({
        "kind": "assistant_message",
        "body": "plaintext-must-not-appear-in-spool"
    });
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new("receipt.remote").unwrap(),
            ComponentVersion::new("sanitizer.remote.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(&payload).unwrap()),
    )
    .unwrap();
    DurableObservationV1::new(
        ObservationIdentityMaterialV1::for_native_record(
            ObservationSourceIdentityV1::for_provider(
                ProviderId::new("provider.remote").unwrap(),
                SessionId::new("session.remote").unwrap(),
            )
            .unwrap(),
            ObservationScopeV1::Project {
                project_id: tracedecay_domain::ProjectId::new("project.remote").unwrap(),
            },
            ObservationSourceGenerationV1::new(1).unwrap(),
            ObservationSourceRangeV1::new(0, 1).unwrap(),
            ObservationOrderingDomainV1::SqliteRowId,
            ObservationId::new("observation.remote").unwrap(),
        )
        .unwrap(),
        receipt,
        RetentionClass::new("retention.remote").unwrap(),
        payload,
    )
    .unwrap()
}

fn admitted() -> AdmittedRemoteCaptureV1 {
    AdmittedRemoteCaptureV1 {
        enrollment_id: EntityId::new("enrollment.remote").unwrap(),
        enrollment_revision: 1,
        node_id: tracedecay_domain::BrainNodeId::new("node.remote").unwrap(),
        writer: writer(),
        policy_revision: 1,
        sequence: RemoteCaptureSequenceV1 {
            sequence: 1,
            previous_event_id: None,
        },
        observation: observation(),
        captured_at: UtcMicros(10),
    }
}

#[test]
fn runtime_attachment_requires_explicit_remote_migration() {
    let fixture = fixture();
    assert!(matches!(
        validate_remote_schema(&fixture.handle),
        Err(RemoteSqliteStorageErrorV1::MigrationRequired)
    ));

    install_remote_schema_v1(&fixture.handle).unwrap();

    assert!(validate_remote_schema(&fixture.handle).is_ok());
    install_remote_schema_v1(&fixture.handle).unwrap();
}

#[test]
fn spool_key_rejects_zero_revision_and_wrong_size() {
    assert!(matches!(
        RemoteSpoolKeyV1::from_secret_bytes(0, vec![7; 32]),
        Err(RemoteSqliteStorageErrorV1::InvalidKeyRevision)
    ));
    assert!(matches!(
        RemoteSpoolKeyV1::from_secret_bytes(1, vec![7; 31]),
        Err(RemoteSqliteStorageErrorV1::InvalidKeyLength)
    ));
    assert_eq!(
        RemoteSpoolKeyV1::from_secret_bytes(7, vec![7; 32])
            .unwrap()
            .revision(),
        7
    );
}

#[test]
fn capture_is_encrypted_idempotent_and_replay_commits_once() {
    let fixture = fixture();
    let storage = storage(&fixture);
    let writer = writer();
    let authority =
        tracedecay_domain::CurrentRemoteAuthorityStateV1::Available(writer.authority.clone());
    storage
        .publish_authority(&authority, &writer, UtcMicros(10))
        .unwrap();
    let capture = admitted();

    let first = storage.capture_pending(&capture).unwrap();
    assert_eq!(
        first.disposition,
        RemoteCaptureDispositionV1::CapturedPending
    );
    assert_eq!(
        storage.capture_pending(&capture).unwrap().disposition,
        RemoteCaptureDispositionV1::AlreadyPending
    );
    assert_eq!(
        storage
            .status(&writer.authority.fence.brain_id)
            .unwrap()
            .pending_spool_items,
        1
    );
    assert_eq!(
        storage.load_replay_frame(&first.event_id).unwrap().capture,
        capture
    );
    let ciphertext = query(
        &fixture.handle,
        "SELECT ciphertext FROM remote_spool_frames_v1 WHERE event_id = ?1",
        vec![text(&first.event_id)],
    )
    .unwrap();
    let bytes = match &ciphertext.rows[0].values[0] {
        MigrationSqlValue::Blob(bytes) => bytes,
        value => panic!("expected ciphertext blob, got {value:?}"),
    };
    assert!(
        !bytes
            .windows(b"plaintext-must-not-appear-in-spool".len())
            .any(|window| window == b"plaintext-must-not-appear-in-spool")
    );

    let frame = storage.load_replay_frame(&first.event_id).unwrap();
    let committed = storage.commit(&frame, &writer, UtcMicros(20)).unwrap();
    let receipt = match committed {
        RemoteReplayTransactionOutcomeV1::Admitted(receipt) => receipt,
        RemoteReplayTransactionOutcomeV1::Duplicate(_) => panic!("first commit was duplicate"),
    };
    assert!(matches!(
        storage.commit(&frame, &writer, UtcMicros(21)).unwrap(),
        RemoteReplayTransactionOutcomeV1::Duplicate(duplicate) if duplicate == receipt
    ));

    let attempt = storage
        .begin_replay_attempt(&first.event_id, UtcMicros(22))
        .unwrap();
    storage
        .transition(RemoteReplayTransitionV1 {
            event_id: first.event_id.clone(),
            from: RemoteReplayStateV1::Pending,
            to: RemoteReplayStateV1::Admitted,
            replay_attempt: attempt,
            observed_at: UtcMicros(23),
            finding: None,
            receipt: Some(receipt.clone()),
        })
        .unwrap();
    storage
        .transition(RemoteReplayTransitionV1 {
            event_id: first.event_id.clone(),
            from: RemoteReplayStateV1::Admitted,
            to: RemoteReplayStateV1::Acknowledged,
            replay_attempt: attempt,
            observed_at: UtcMicros(24),
            finding: None,
            receipt: Some(receipt),
        })
        .unwrap();
    assert_eq!(
        storage.state(&first.event_id).unwrap().state,
        RemoteReplayStateV1::Acknowledged
    );
    assert_eq!(
        storage
            .status(&writer.authority.fence.brain_id)
            .unwrap()
            .pending_spool_items,
        0
    );
}

#[test]
fn capture_rejects_sequence_gaps_and_corrupt_ciphertext() {
    let fixture = fixture();
    let storage = storage(&fixture);
    let mut gap = admitted();
    gap.sequence = RemoteCaptureSequenceV1 {
        sequence: 2,
        previous_event_id: Some("remote.event.missing".to_owned()),
    };
    assert_eq!(
        storage.capture_pending(&gap),
        Err(RemoteCapturePersistenceErrorV1::SequenceGap)
    );

    let capture = admitted();
    let receipt = storage.capture_pending(&capture).unwrap();
    fixture
        .handle
        .execute(
            MigrationSqlStatement::new(
                "UPDATE remote_spool_frames_v1 SET ciphertext = ?1 WHERE event_id = ?2".to_owned(),
                vec![
                    MigrationSqlValue::Blob(vec![0; 32]),
                    text(&receipt.event_id),
                ],
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(
        storage.load_replay_frame(&receipt.event_id),
        Err(RemoteCapturePersistenceErrorV1::Corruption)
    );
}
