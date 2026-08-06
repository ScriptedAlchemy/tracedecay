use std::sync::Arc;

use rusqlite::Savepoint;
use serde_json::json;
use tempfile::TempDir;
use tracedecay_application::remote::{
    auth::{
        OpaqueRemoteCredential, RemoteEnrollmentAdmissionEvidenceV1,
        RemoteEnrollmentCommitReceiptV1, revoke_credential,
    },
    capture::{
        AdmittedRemoteCaptureV1, RemoteCaptureDispositionV1, RemoteCapturePersistenceErrorV1,
        RemoteCapturePortV1, RemoteCaptureSequenceV1, RemoteWriterAuthorityV1,
    },
    credential_admission::{
        RemoteCredentialAdmissionErrorV1, RemoteCredentialAdmissionPortV1,
        RemoteCredentialAdmissionServiceV1, RemoteCredentialAuthorityRecordV1,
        RemoteCredentialClassV1, RemoteCredentialLookupErrorV1, RemoteCredentialLookupPortV1,
        RemoteCredentialUseV1,
    },
    query::RemoteExactObservationQueryErrorV1,
    replay::{
        RemoteReplayApplicationErrorV1, RemoteReplayFrameLookupPortV1,
        RemoteReplayPolicyDecisionV1, RemoteReplayPolicyEvidencePortV1,
        RemoteReplayPolicyEvidenceV1, RemoteReplaySpoolPortV1,
    },
};
use tracedecay_application::{
    AuthorityReceipt, CapabilityGrantId, Deadline, DisclosureClass, OperationBudgetUsage,
    PolicyDecisionRef, ResolvedScope,
};
use tracedecay_domain::{
    ActorId, BrainId, BrainNodeId, ComponentVersion, DurableObservationV1,
    EnrollmentCredentialRecordV1, EnrollmentGrantV1, EntityId, LocatorDigest, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    PayloadReferenceV1, ProviderId, RemoteCapabilityV1, RemoteCredentialFingerprintV1,
    RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
    SanitizerDispositionV1, SensitivityV1, SessionId, UtcMicros, canonical_sha256,
};
use tracedecay_store::{
    AdmissionConfigV1, RepositoryWritePayloadV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1,
    StorageRuntimeErrorV1, StoreIncarnationV1, VerifiedStoreLocatorV1,
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
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("remote.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute_batch(REMOTE_NODE_LOCAL_SCHEMA).unwrap();
    connection
        .execute(
            "INSERT INTO remote_node_identity (
                singleton, brain_id, profile_id, node_id
             ) VALUES (1, 'brain.remote', 'profile.remote', 'node.remote')",
            (),
        )
        .unwrap();
    drop(connection);
    let path = path.canonicalize().unwrap();
    let binding: StoreRuntimeBindingV1 = serde_json::from_value(serde_json::json!({
        "shard_id": {
            "brain_id": "brain.remote",
            "profile_id": "profile.remote",
            "scope": { "kind": "remote_node", "node_id": "node.remote" }
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

fn storage(fixture: &Fixture) -> RemoteSqliteStorageV1 {
    RemoteSqliteStorageV1::from_registered(
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
        observation,
        captured_at: UtcMicros(10),
    }
}

fn enrollment_grant(secret: &[u8]) -> EnrollmentGrantV1 {
    EnrollmentGrantV1 {
        grant_id: EntityId::new("grant.remote").unwrap(),
        brain_id: BrainId::new("brain.remote").unwrap(),
        node_id: BrainNodeId::new("node.remote").unwrap(),
        fingerprint: RemoteCredentialFingerprintV1::from_secret(secret).unwrap(),
        revision: 1,
        issued_at: UtcMicros(1),
        expires_at: UtcMicros(100),
        revoked_at: None,
        capabilities: std::collections::BTreeSet::from([
            RemoteCapabilityV1::Replay,
            RemoteCapabilityV1::PublishRestore,
        ]),
        scope: writer().scope,
    }
}

fn enrollment_admission(grant: &EnrollmentGrantV1) -> RemoteEnrollmentAdmissionEvidenceV1 {
    let scope = ResolvedScope::new(
        grant.scope.project_id.clone(),
        grant.scope.repository_id.clone(),
        grant.scope.worktree_id.clone(),
        grant.scope.reference.clone(),
    )
    .unwrap();
    let digest = canonical_sha256(grant).unwrap();
    RemoteEnrollmentAdmissionEvidenceV1::new(
        grant,
        scope.clone(),
        AuthorityReceipt {
            grant_id: CapabilityGrantId::new(grant.grant_id.as_str()).unwrap(),
            grant_revision: grant.revision,
            grant_digest: digest.clone(),
            authorized_scope_digest: scope.scope_digest,
            disclosure: DisclosureClass::Evidence,
            policy: PolicyDecisionRef::new(
                "policy.remote.enrollment",
                1,
                digest,
                ComponentVersion::new("policy.remote.enrollment.v1").unwrap(),
            )
            .unwrap(),
            revalidated_at: UtcMicros(2),
        },
        ActorId::new("actor.remote").unwrap(),
        ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
        ManifestDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
        ManifestDigest::new(format!("sha256:{}", "d".repeat(64))).unwrap(),
        Deadline::new(UtcMicros(100)).unwrap(),
    )
    .unwrap()
}

fn enrollment_record(
    secret: &[u8],
) -> (
    EnrollmentCredentialRecordV1,
    RemoteEnrollmentCommitReceiptV1,
) {
    let grant = enrollment_grant(&[3_u8; 32]);
    let enrollment = EnrollmentCredentialRecordV1 {
        enrollment_id: EntityId::new("enrollment.remote").unwrap(),
        brain_id: grant.brain_id.clone(),
        node_id: grant.node_id.clone(),
        fingerprint: RemoteCredentialFingerprintV1::from_secret(secret).unwrap(),
        revision: 1,
        issued_at: UtcMicros(10),
        expires_at: UtcMicros(100),
        revoked_at: None,
        capabilities: grant.capabilities.clone(),
        scope: grant.scope.clone(),
    };
    let grant_digest = canonical_sha256(&grant).unwrap();
    let receipt = RemoteEnrollmentCommitReceiptV1 {
        admission: enrollment_admission(&grant),
        prior_grant_digest: grant_digest,
        input_digest: ManifestDigest::new(format!("sha256:{}", "e".repeat(64))).unwrap(),
        committed_state_digest: canonical_sha256(&enrollment).unwrap(),
        consumed_at: enrollment.issued_at,
        budget: OperationBudgetUsage {
            units_consumed: 1,
            bytes_consumed: 1,
            elapsed_micros: 0,
        },
        enrollment: enrollment.clone(),
    };
    receipt.validate().unwrap();
    (enrollment, receipt)
}

fn insert_enrollment(
    fixture: &Fixture,
    enrollment: &EnrollmentCredentialRecordV1,
    receipt: &RemoteEnrollmentCommitReceiptV1,
) {
    fixture
        .handle
        .execute(
            ExactSqlStatement::new(
                "INSERT INTO remote_enrollments (
                    enrollment_id, brain_id, node_id, revision,
                    credential_fingerprint, enrollment_json, commit_receipt_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
                    .to_owned(),
                vec![
                    text(enrollment.enrollment_id.as_str()),
                    text(enrollment.brain_id.as_str()),
                    text(enrollment.node_id.as_str()),
                    ExactSqlValue::Integer(i64::try_from(enrollment.revision).unwrap()),
                    text(enrollment.fingerprint.digest().as_str()),
                    text(&serde_json::to_string(enrollment).unwrap()),
                    text(&serde_json::to_string(receipt).unwrap()),
                ],
            )
            .unwrap(),
        )
        .unwrap();
}

#[test]
fn runtime_attachment_requires_registered_remote_binding() {
    let canonical = fixture();
    let keyring = || {
        Arc::new(TestKeyring(Arc::new(
            RemoteSpoolKeyV1::from_secret_bytes(7, vec![7; 32]).unwrap(),
        ))) as Arc<dyn RemoteSpoolKeyringV1>
    };
    RemoteSqliteStorageV1::from_registered(
        canonical.handle.clone(),
        canonical.binding.clone(),
        keyring(),
    )
    .unwrap();
    let project_binding: StoreRuntimeBindingV1 = serde_json::from_value(serde_json::json!({
        "shard_id": {
            "brain_id": "brain.remote",
            "profile_id": "profile.remote",
            "scope": { "kind": "project", "project_id": "project.remote" }
        },
        "incarnation": 3,
        "authority_epoch": 11
    }))
    .unwrap();
    assert!(matches!(
        RemoteSqliteStorageV1::from_registered(
            canonical.handle.clone(),
            project_binding,
            keyring(),
        ),
        Err(RemoteSqliteStorageErrorV1::BindingMismatch)
    ));
}

#[test]
fn runtime_attachment_rejects_a_missing_registered_identity_without_repairing_it() {
    let fixture = fixture();
    fixture
        .handle
        .execute_batch("DELETE FROM remote_node_identity".to_owned())
        .unwrap();

    assert!(matches!(
        RemoteSqliteStorageV1::from_registered(
            fixture.handle.clone(),
            fixture.binding.clone(),
            Arc::new(TestKeyring(Arc::new(
                RemoteSpoolKeyV1::from_secret_bytes(7, vec![7; 32]).unwrap(),
            ))),
        ),
        Err(RemoteSqliteStorageErrorV1::ResetRequired)
    ));
    let rows = fixture
        .handle
        .query(
            ExactSqlStatement::new(
                "SELECT COUNT(*) FROM remote_node_identity".to_owned(),
                Vec::new(),
            )
            .unwrap(),
            READ_WAIT,
        )
        .unwrap();
    assert!(matches!(
        rows.rows[0].values.first(),
        Some(ExactSqlValue::Integer(0))
    ));
}

#[test]
fn runtime_attachment_rejects_any_non_final_persisted_shape() {
    let fixture = fixture();
    fixture
        .handle
        .execute_batch("DROP TABLE remote_enrollments".to_owned())
        .unwrap();
    assert!(matches!(
        RemoteSqliteStorageV1::from_registered(
            fixture.handle.clone(),
            fixture.binding.clone(),
            Arc::new(TestKeyring(Arc::new(
                RemoteSpoolKeyV1::from_secret_bytes(7, vec![7; 32]).unwrap(),
            ))),
        ),
        Err(RemoteSqliteStorageErrorV1::ResetRequired)
    ));
}

#[test]
fn runtime_attachment_rejects_same_tables_with_stale_columns() {
    let fixture = fixture();
    fixture
        .handle
        .execute_batch(
            "DROP TABLE remote_enrollments;
             CREATE TABLE remote_enrollments (
                 enrollment_id TEXT PRIMARY KEY,
                 brain_id TEXT NOT NULL,
                 node_id TEXT NOT NULL,
                 revision INTEGER NOT NULL CHECK (revision > 0),
                 enrollment_json TEXT NOT NULL,
                 commit_receipt_json TEXT NOT NULL,
                 UNIQUE (brain_id, node_id, revision)
             ) STRICT;"
                .to_owned(),
        )
        .unwrap();
    assert!(matches!(
        RemoteSqliteStorageV1::from_registered(
            fixture.handle.clone(),
            fixture.binding.clone(),
            Arc::new(TestKeyring(Arc::new(
                RemoteSpoolKeyV1::from_secret_bytes(7, vec![7; 32]).unwrap(),
            ))),
        ),
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
fn credential_admission_looks_up_only_the_fingerprint_indexed_final_authority() {
    let fixture = fixture();
    let storage = storage(&fixture);
    let secret = [7_u8; 32];
    let grant = enrollment_grant(&secret);
    let admission = enrollment_admission(&grant);
    storage.store_enrollment_grant(&grant, &admission).unwrap();

    assert_eq!(
        storage
            .credential_by_fingerprint(
                RemoteCredentialClassV1::EnrollmentGrant,
                &grant.fingerprint,
            )
            .unwrap(),
        RemoteCredentialAuthorityRecordV1::Grant {
            grant: grant.clone(),
            admission,
        }
    );
    let unknown = RemoteCredentialFingerprintV1::from_secret(&[8_u8; 32]).unwrap();
    assert_eq!(
        storage.credential_by_fingerprint(RemoteCredentialClassV1::EnrollmentGrant, &unknown,),
        Err(RemoteCredentialLookupErrorV1::NotFound)
    );
}

#[test]
fn credential_registration_inventory_is_bounded_and_preserves_exact_node_identity() {
    let fixture = fixture();
    let storage = storage(&fixture);
    let grant_secret = [7_u8; 32];
    let grant = enrollment_grant(&grant_secret);
    storage
        .store_enrollment_grant(&grant, &enrollment_admission(&grant))
        .unwrap();
    let (enrollment, receipt) = enrollment_record(&[9_u8; 32]);
    insert_enrollment(&fixture, &enrollment, &receipt);

    assert_eq!(
        storage.credential_registrations(1),
        Err(RemoteCredentialInventoryErrorV1::CapacityExceeded)
    );
    assert_eq!(
        storage.credential_registrations(2).unwrap(),
        vec![
            RemoteCredentialRegistrationV1 {
                class: RemoteCredentialClassV1::EnrollmentGrant,
                fingerprint: grant.fingerprint,
                brain_id: grant.brain_id,
                node_id: grant.node_id,
            },
            RemoteCredentialRegistrationV1 {
                class: RemoteCredentialClassV1::Enrollment,
                fingerprint: enrollment.fingerprint,
                brain_id: enrollment.brain_id,
                node_id: enrollment.node_id,
            },
        ]
    );
}

#[test]
fn durable_revocation_wins_publication_reauthorization() {
    let fixture = fixture();
    let storage = storage(&fixture);
    let secret = [9_u8; 32];
    let (enrollment, receipt) = enrollment_record(&secret);
    insert_enrollment(&fixture, &enrollment, &receipt);
    let service = RemoteCredentialAdmissionServiceV1::new(storage.clone());
    let credential = OpaqueRemoteCredential::new(secret).unwrap();
    let session = service
        .admit_before_body(
            &credential,
            RemoteCredentialUseV1::PublishRestore,
            UtcMicros(20),
        )
        .unwrap();
    let (revoked, revocation_receipt) =
        revoke_credential(&enrollment, enrollment.revision, UtcMicros(21)).unwrap();
    storage
        .revoke_enrollment(&enrollment, &revoked, &revocation_receipt)
        .unwrap();
    assert!(matches!(
        storage.revoke_enrollment(&enrollment, &revoked, &revocation_receipt),
        Err(RemoteSqliteStorageErrorV1::Conflict)
    ));
    assert_eq!(
        service.reauthorize_publication(&session, UtcMicros(21)),
        Err(RemoteCredentialAdmissionErrorV1::Revoked)
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
fn query_authority_snapshot_is_exactly_scope_and_registry_bound() {
    let fixture = fixture();
    let storage = storage(&fixture);
    let writer = writer();
    let authority =
        tracedecay_domain::CurrentRemoteAuthorityStateV1::Available(writer.authority.clone());
    storage
        .publish_authority(&authority, &writer, UtcMicros(10))
        .unwrap();

    let snapshot = storage
        .query_authority_snapshot(&writer.scope, UtcMicros(11))
        .unwrap();
    assert_eq!(snapshot.authority, authority);
    assert_eq!(snapshot.writer, writer);

    let mut foreign_scope = writer.scope.clone();
    foreign_scope.project_id = tracedecay_domain::ProjectId::new("project.foreign").unwrap();
    assert_eq!(
        storage.query_authority_snapshot(&foreign_scope, UtcMicros(11)),
        Err(RemoteExactObservationQueryErrorV1::ScopeMismatch)
    );
}

#[test]
fn capture_and_promotion_gate_share_one_write_transaction() {
    let fixture = fixture();
    let storage = storage(&fixture);
    let capture = admitted();
    let authority = tracedecay_domain::CurrentRemoteAuthorityStateV1::Available(
        capture.writer.authority.clone(),
    );
    storage
        .publish_authority(&authority, &capture.writer, UtcMicros(10))
        .unwrap();
    let fence = &capture.writer.authority.fence;
    let authority_key = canonical_sha256(&(
        "tracedecay.remote-recovery-authority.v1",
        &fence.brain_id,
        &fence.shard_id,
        &fence.generation_id,
    ))
    .unwrap();
    fixture
        .handle
        .execute(
            ExactSqlStatement::new(
                "INSERT INTO remote_recovery_operations (
                    operation_id, operation_kind, request_digest,
                    expected_authority_key, pre_state_digest, context_json,
                    state, output_json, receipt_json, started_at, updated_at
                 ) VALUES (
                    ?1, 'promotion', ?2, ?3, ?4, '{}',
                    'executing', NULL, NULL, 20, 20
                 )"
                .to_owned(),
                vec![
                    text("recovery.promotion.capture-gate"),
                    text("sha256:request"),
                    text(authority_key.as_str()),
                    text("sha256:pre-state"),
                ],
            )
            .unwrap(),
        )
        .unwrap();

    assert_eq!(
        storage.capture_pending(&capture),
        Err(RemoteCapturePersistenceErrorV1::Unavailable)
    );
    assert_eq!(
        storage
            .status(&capture.writer.authority.fence.brain_id)
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
fn capture_enforces_the_registered_spool_event_bound() {
    let fixture = fixture();
    let storage = RemoteSqliteStorageV1::from_registered_with_limits(
        fixture.handle.clone(),
        fixture.binding.clone(),
        Arc::new(TestKeyring(Arc::new(
            RemoteSpoolKeyV1::from_secret_bytes(7, vec![7; 32]).unwrap(),
        ))),
        RemoteSpoolLimitsV1::new(1, 1024 * 1024).unwrap(),
    )
    .unwrap();
    let first = admitted();
    let receipt = storage.capture_pending(&first).unwrap();
    let mut second = admitted();
    second.sequence = RemoteCaptureSequenceV1 {
        sequence: 2,
        previous_event_id: Some(receipt.event_id),
    };

    assert_eq!(
        storage.capture_pending(&second),
        Err(RemoteCapturePersistenceErrorV1::Overflow)
    );
}

#[test]
fn startup_releases_only_interrupted_attempt_markers_for_idempotent_retry() {
    let fixture = fixture();
    let storage = storage(&fixture);
    let receipt = storage.capture_pending(&admitted()).unwrap();
    assert_eq!(
        storage
            .begin_replay_attempt(&receipt.event_id, UtcMicros(20))
            .unwrap(),
        1
    );
    assert_eq!(
        storage.begin_replay_attempt(&receipt.event_id, UtcMicros(21)),
        Err(RemoteCapturePersistenceErrorV1::Corruption)
    );

    let recovery = storage
        .recover_interrupted_replay_attempts(UtcMicros(30))
        .unwrap();
    assert!(recovery.lease_id.starts_with("replay.recovery."));
    assert_eq!(recovery.interrupted_attempts, 1);
    assert_eq!(recovery.preserved_newer_markers, 0);
    assert_eq!(
        storage.state(&receipt.event_id).unwrap(),
        RemoteReplaySpoolStateV1 {
            state: RemoteReplayStateV1::Pending,
            receipt: None,
            last_attempt: 1,
        }
    );
    assert_eq!(
        storage
            .begin_replay_attempt(&receipt.event_id, UtcMicros(31))
            .unwrap(),
        2
    );
}

#[test]
fn replay_policy_is_revision_guarded_and_loaded_from_the_final_store() {
    let fixture = fixture();
    let storage = storage(&fixture);
    let capture = admitted();
    let repository_scope = capture.writer.scope.clone();
    let scope = ResolvedScope::new(
        repository_scope.project_id.clone(),
        repository_scope.repository_id.clone(),
        repository_scope.worktree_id.clone(),
        repository_scope.reference.clone(),
    )
    .unwrap();
    let digest = ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap();
    let evidence = RemoteReplayPolicyEvidenceV1 {
        scope,
        repository_scope,
        policy_revision: 1,
        decision: RemoteReplayPolicyDecisionV1::Admit,
        policy: PolicyDecisionRef::new(
            "policy.remote.replay",
            1,
            digest.clone(),
            ComponentVersion::new("policy.remote.replay.v2").unwrap(),
        )
        .unwrap(),
        configuration_digest: digest.clone(),
        catalog_digest: digest.clone(),
        privacy_digest: digest,
        revalidated_at: UtcMicros(10),
    };
    storage.store_replay_policy(&evidence).unwrap();
    let frame = RemoteReplayFrameV1 {
        event_id: canonical_remote_event_id_v1(&capture).unwrap(),
        capture,
    };
    assert_eq!(storage.current_policy_evidence(&frame).unwrap(), evidence);

    let mut conflict = evidence;
    conflict.decision = RemoteReplayPolicyDecisionV1::Quarantine;
    assert_eq!(
        storage.store_replay_policy(&conflict),
        Err(RemoteReplayApplicationErrorV1::PolicyMismatch)
    );
}
