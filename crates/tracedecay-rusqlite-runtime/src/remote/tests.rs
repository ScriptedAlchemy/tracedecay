use std::sync::Arc;
use std::sync::Mutex;

use rusqlite::Savepoint;
use serde_json::json;
use tempfile::TempDir;
use tracedecay_application::RequestId;
use tracedecay_application::remote::protocol::{
    RemoteClockPortV1, RemoteProtocolExecutionErrorV1, RemoteProtocolRequestV1,
    RemoteProtocolResponseV1,
};
use tracedecay_application::remote::{
    capture::{
        AdmittedRemoteCaptureV1, RemoteCaptureDispositionV1, RemoteCapturePersistenceErrorV1,
        RemoteCapturePortV1, RemoteCaptureSequenceV1, RemoteWriterAuthorityV1,
    },
    replay::{
        RemoteReplayFrameLookupPortV1, RemoteReplayOutcomeV1, RemoteReplayRequestV1,
        RemoteReplaySpoolPortV1, RemoteReplaySpoolStateV1, RemoteReplayStateV1,
    },
    replay_node::{
        RemoteNodeReplayCommandV1, RemoteNodeReplayErrorV1, RemoteNodeReplayServiceV1,
        RemoteReplayTransportErrorV1, RemoteReplayTransportPortV1,
    },
};
use tracedecay_domain::{
    ComponentVersion, DurableObservationV1, EntityId, LocatorDigest, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadReferenceV1, ProjectionGenerationId, ProviderId,
    RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
    SanitizerDispositionV1, SensitivityV1, SessionId, UtcMicros,
};
use tracedecay_store::{
    AdmissionConfigV1, AnchoredObservationWrite, ObservationWrite, RepositoryWritePayloadV1,
    RuntimeReadOutcomeV1, RuntimeReadRequestV1, StorageRuntimeErrorV1, StoreIncarnationV1,
    VerifiedStoreLocatorV1, build_observation_resolution_authorization_v1,
    build_observation_retrieval_anchor_v2,
};

use crate::{
    ExistingWriterLocator, PersistentWriter, StorageOperationExecutor,
    exact_sql::{ExactSqlWriteAuthority, ExactSqlWriteIntent},
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

impl ExactSqlWriteAuthority for AllowSchema {
    fn verify(&self, _intent: ExactSqlWriteIntent) -> Result<(), ExactSqlError> {
        Ok(())
    }
}

struct Fixture {
    _directory: TempDir,
    _writer: PersistentWriter,
    _readers: ReaderPool<NoReads>,
    handle: ExactSqlHandle,
    binding: StoreRuntimeBindingV1,
}

fn fixture() -> Fixture {
    fixture_with_schema(Some(REMOTE_NODE_LOCAL_SCHEMA))
}

fn empty_fixture() -> Fixture {
    fixture_with_schema(None)
}

fn fixture_with_schema(schema: Option<&str>) -> Fixture {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("remote.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    if let Some(schema) = schema {
        connection.execute_batch(schema).unwrap();
    }
    drop(connection);
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
    let handle = ExactSqlHandle::attach(&writer, &readers)
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

struct TestClock;

impl RemoteClockPortV1 for TestClock {
    fn now(&self) -> Result<UtcMicros, RemoteProtocolExecutionErrorV1> {
        Ok(UtcMicros(20))
    }
}

#[derive(Default)]
struct UnreachableTransport {
    request: Mutex<Option<RemoteProtocolRequestV1<RemoteReplayRequestV1>>>,
}

impl RemoteReplayTransportPortV1 for UnreachableTransport {
    fn replay(
        &self,
        request: &RemoteProtocolRequestV1<RemoteReplayRequestV1>,
    ) -> Result<RemoteProtocolResponseV1<RemoteReplayOutcomeV1>, RemoteReplayTransportErrorV1> {
        *self.request.lock().unwrap() = Some(request.clone());
        Err(RemoteReplayTransportErrorV1::AuthorityUnreachable)
    }
}

fn storage(fixture: &Fixture) -> RemoteSqliteStorageV1 {
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
    let observation = observation();
    let identity = observation.identity();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        identity.generation(),
        identity.ordering_domain(),
        identity.position().end(),
    )
    .unwrap();
    let projection_generation = ProjectionGenerationId::new("projection.remote").unwrap();
    let authorization =
        build_observation_resolution_authorization_v1(&observation, "remote-test.v1").unwrap();
    let retrieval_anchor = build_observation_retrieval_anchor_v2(
        &observation,
        projection_generation.clone(),
        UtcMicros(10),
        authorization,
    )
    .unwrap();
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
        anchored_write: AnchoredObservationWrite::new(
            ObservationWrite::new(observation, None, next_cursor).unwrap(),
            retrieval_anchor,
            projection_generation,
        )
        .unwrap(),
        captured_at: UtcMicros(10),
    }
}

#[test]
fn runtime_attachment_requires_explicit_remote_migration() {
    let empty = empty_fixture();
    assert!(matches!(
        validate_remote_schema(&empty.handle),
        Err(RemoteSqliteStorageErrorV1::MigrationRequired)
    ));

    let canonical = fixture();
    assert!(validate_remote_schema(&canonical.handle).is_ok());

    let incompatible = fixture_with_schema(Some(
        "CREATE TABLE remote_observations_v1 (
                event_id TEXT PRIMARY KEY
            ) STRICT;",
    ));
    assert!(matches!(
        validate_remote_schema(&incompatible.handle),
        Err(RemoteSqliteStorageErrorV1::ResetRequired)
    ));

    let mixed_schema = format!("{REMOTE_NODE_LOCAL_SCHEMA}\n{REMOTE_OBSERVATION_EVENTS_SCHEMA}");
    let mixed_store = fixture_with_schema(Some(&mixed_schema));
    assert!(matches!(
        validate_remote_schema(&mixed_store.handle),
        Err(RemoteSqliteStorageErrorV1::ResetRequired)
    ));
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
fn capture_is_encrypted_and_idempotent() {
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
        "SELECT ciphertext FROM remote_spool_frames WHERE event_id = ?1",
        vec![text(&first.event_id)],
    )
    .unwrap();
    let bytes = match &ciphertext.rows[0].values[0] {
        ExactSqlValue::Blob(bytes) => bytes,
        value => panic!("expected ciphertext blob, got {value:?}"),
    };
    assert!(
        !bytes
            .windows(b"plaintext-must-not-appear-in-spool".len())
            .any(|window| window == b"plaintext-must-not-appear-in-spool")
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
            ExactSqlStatement::new(
                "UPDATE remote_spool_frames SET ciphertext = ?1 WHERE event_id = ?2".to_owned(),
                vec![ExactSqlValue::Blob(vec![0; 32]), text(&receipt.event_id)],
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(
        storage.load_replay_frame(&receipt.event_id),
        Err(RemoteCapturePersistenceErrorV1::Corruption)
    );
}

#[test]
fn node_replay_sends_the_decrypted_frame_and_keeps_transport_failure_pending() {
    let fixture = fixture();
    let storage = Arc::new(storage(&fixture));
    let capture = admitted();
    let captured = storage.capture_pending(&capture).unwrap();
    let transport = Arc::new(UnreachableTransport::default());
    let service = RemoteNodeReplayServiceV1::new(
        Arc::clone(&storage) as Arc<dyn RemoteReplayFrameLookupPortV1>,
        Arc::clone(&storage) as Arc<dyn RemoteReplaySpoolPortV1>,
        Arc::clone(&transport) as Arc<dyn RemoteReplayTransportPortV1>,
        Arc::new(TestClock),
    );

    assert_eq!(
        service.replay(RemoteNodeReplayCommandV1 {
            request_id: RequestId::new("request.remote.node-replay").unwrap(),
            event_id: captured.event_id.clone(),
            expected_authority: capture.writer.authority.fence.clone(),
        }),
        Err(RemoteNodeReplayErrorV1::Transport(
            RemoteReplayTransportErrorV1::AuthorityUnreachable
        ))
    );
    let request = transport.request.lock().unwrap().clone().unwrap();
    assert_eq!(request.body.frame.capture, capture);
    assert_eq!(request.body.replay_attempt, 1);
    assert_eq!(
        storage.state(&captured.event_id).unwrap(),
        RemoteReplaySpoolStateV1 {
            state: RemoteReplayStateV1::Pending,
            receipt: None,
            last_attempt: 1,
        }
    );
}
