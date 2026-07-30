use std::collections::BTreeSet;
use std::path::PathBuf;

use rusqlite::Savepoint;
use tempfile::TempDir;
use tracedecay_application::remote::auth::{
    EnrollmentIssueRequestV1, OpaqueRemoteCredential, RemoteEnrollmentAdmissionEvidenceV1,
    RemoteEnrollmentAuthorityErrorV1, RemoteEnrollmentAuthorityPortV1,
    RemoteEnrollmentCredentialLookupPortV1, RemoteEnrollmentServiceV1, issue_enrollment,
};
use tracedecay_application::remote::protocol::{EnrollmentRequestV1, RemoteProtocolRequestV1};
use tracedecay_application::remote::replay::{
    RemoteReplayApplicationErrorV1, RemoteReplayPolicyDecisionV1, RemoteReplayPolicyEvidenceV1,
};
use tracedecay_application::{
    AuthorityReceipt, CapabilityGrantId, Deadline, DisclosureClass, PolicyDecisionRef, RequestId,
    ResolvedScope,
};
use tracedecay_domain::{
    ActorId, BrainId, BrainNodeId, ComponentVersion, EnrollmentGrantV1, EntityId, LocatorDigest,
    ManifestDigest, ProjectId, RefId, RemoteCapabilityV1, RemoteCredentialFingerprintV1,
    RemoteRepositoryScopeV1, RepositoryId, RepositoryStateSnapshotId, UtcMicros, WorktreeId,
    canonical_sha256,
};
use tracedecay_rusqlite_runtime::migration_sql::MigrationSqlHandle;
use tracedecay_rusqlite_runtime::reader::{ExistingReaderLocator, ReaderPool, ReaderQueryExecutor};
use tracedecay_rusqlite_runtime::remote_authority::{
    RegisteredRemoteEnrollmentAuthorityV1, RegisteredRemoteReplayPolicyAuthorityV1,
};
use tracedecay_rusqlite_runtime::{
    ExistingWriterLocator, PersistentWriter, StorageOperationExecutor,
};
use tracedecay_store::{
    AdmissionConfigV1, RepositoryWritePayloadV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1,
    StorageRuntimeErrorV1, StoreIncarnationV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
};

struct NoTypedWrites;

impl StorageOperationExecutor for NoTypedWrites {
    fn execute(
        &mut self,
        _savepoint: &Savepoint<'_>,
        _payload: &RepositoryWritePayloadV1,
    ) -> rusqlite::Result<()> {
        unreachable!("enrollment uses only the registered migration-SQL channel")
    }
}

#[derive(Clone)]
struct NoTypedReads;

impl ReaderQueryExecutor for NoTypedReads {
    fn execute_read(
        &mut self,
        _snapshot: &rusqlite::Transaction<'_>,
        _request: &RuntimeReadRequestV1,
    ) -> Result<RuntimeReadOutcomeV1, StorageRuntimeErrorV1> {
        unreachable!("enrollment uses only the registered migration-SQL channel")
    }
}

struct RegisteredStore {
    handle: MigrationSqlHandle,
    path: PathBuf,
    writer: PersistentWriter,
    readers: ReaderPool<NoTypedReads>,
    directory: TempDir,
}

impl RegisteredStore {
    fn start() -> Self {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("remote-enrollment.sqlite3");
        rusqlite::Connection::open(&path).unwrap();
        Self::open(path.canonicalize().unwrap(), directory)
    }

    fn open(path: PathBuf, directory: TempDir) -> Self {
        let binding = binding();
        let locator = locator(&binding);
        let writer = PersistentWriter::start(
            ExistingWriterLocator::new(binding.clone(), locator.clone(), path.clone()).unwrap(),
            AdmissionConfigV1::default(),
            NoTypedWrites,
        )
        .unwrap();
        let readers = ReaderPool::start(
            ExistingReaderLocator::new(binding, locator, path.clone()).unwrap(),
            AdmissionConfigV1::default().readers,
            NoTypedReads,
        )
        .unwrap();
        let handle = MigrationSqlHandle::attach(&writer, &readers).unwrap();
        Self {
            handle,
            path,
            writer,
            readers,
            directory,
        }
    }

    fn restart(self) -> Self {
        let Self {
            handle,
            path,
            writer,
            readers,
            directory,
        } = self;
        drop(handle);
        drop(readers);
        drop(writer);
        Self::open(path, directory)
    }
}

fn binding() -> StoreRuntimeBindingV1 {
    serde_json::from_value(serde_json::json!({
        "shard_id": {
            "brain_id": "brain.remote",
            "profile_id": "profile.remote",
            "scope": { "kind": "project", "project_id": "project.remote" }
        },
        "incarnation": 1,
        "authority_epoch": 1
    }))
    .unwrap()
}

fn locator(binding: &StoreRuntimeBindingV1) -> VerifiedStoreLocatorV1 {
    VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        StoreIncarnationV1::new(1).unwrap(),
        LocatorDigest::new(format!("sha256:{}", "5".repeat(64))).unwrap(),
    )
}

fn credential(value: u8) -> OpaqueRemoteCredential {
    OpaqueRemoteCredential::new(vec![value; 32].into_boxed_slice()).unwrap()
}

fn scope() -> RemoteRepositoryScopeV1 {
    RemoteRepositoryScopeV1 {
        project_id: ProjectId::new("project.remote").unwrap(),
        repository_id: RepositoryId::new("repository.remote").unwrap(),
        worktree_id: WorktreeId::new("worktree.remote").unwrap(),
        reference: Some(RefId::new("refs/heads/main").unwrap()),
        snapshot_id: RepositoryStateSnapshotId::new("snapshot.remote").unwrap(),
    }
}

fn enrollment_request() -> RemoteProtocolRequestV1<EnrollmentRequestV1> {
    let brain_id = BrainId::new("brain.remote").unwrap();
    let node_id = BrainNodeId::new("node.remote").unwrap();
    RemoteProtocolRequestV1::new_initial_enrollment(
        RequestId::new("request.remote.enrollment").unwrap(),
        brain_id.clone(),
        node_id.clone(),
        UtcMicros(10),
        EnrollmentRequestV1 {
            grant_id: EntityId::new("grant.remote").unwrap(),
            grant_revision: 1,
            enrollment_id: EntityId::new("enrollment.remote").unwrap(),
            brain_id,
            node_id,
            expires_at: UtcMicros(90),
            capabilities: BTreeSet::from([RemoteCapabilityV1::Query]),
            scope: scope(),
        },
    )
    .unwrap()
}

fn admission(grant: &EnrollmentGrantV1) -> RemoteEnrollmentAdmissionEvidenceV1 {
    admission_with_deadline(grant, UtcMicros(100))
}

fn admission_with_deadline(
    grant: &EnrollmentGrantV1,
    deadline: UtcMicros,
) -> RemoteEnrollmentAdmissionEvidenceV1 {
    let scope = ResolvedScope::new(
        grant.scope.project_id.clone(),
        grant.scope.repository_id.clone(),
        grant.scope.worktree_id.clone(),
        grant.scope.reference.clone(),
    )
    .unwrap();
    let grant_digest = canonical_sha256(grant).unwrap();
    RemoteEnrollmentAdmissionEvidenceV1::new(
        grant,
        scope.clone(),
        AuthorityReceipt {
            grant_id: CapabilityGrantId::new(grant.grant_id.as_str()).unwrap(),
            grant_revision: grant.revision,
            grant_digest: grant_digest.clone(),
            authorized_scope_digest: scope.scope_digest,
            disclosure: DisclosureClass::Evidence,
            policy: PolicyDecisionRef::new(
                "policy.remote.enrollment",
                1,
                grant_digest,
                ComponentVersion::new("policy.remote.enrollment.v1").unwrap(),
            )
            .unwrap(),
            revalidated_at: UtcMicros(deadline.0.saturating_sub(1).min(10)),
        },
        ActorId::new("actor.remote.node").unwrap(),
        ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
        ManifestDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
        ManifestDigest::new(format!("sha256:{}", "d".repeat(64))).unwrap(),
        Deadline::new(deadline).unwrap(),
    )
    .unwrap()
}

#[test]
fn registered_enrollment_rejects_deadline_at_exact_commit_boundary() {
    let store = RegisteredStore::start();
    let authority =
        RegisteredRemoteEnrollmentAuthorityV1::from_registered(store.handle.clone()).unwrap();
    let grant_credential = credential(b'g');
    let enrollment_credential = credential(b'e');
    let grant = EnrollmentGrantV1 {
        grant_id: EntityId::new("grant.deadline").unwrap(),
        brain_id: BrainId::new("brain.remote").unwrap(),
        node_id: BrainNodeId::new("node.remote").unwrap(),
        fingerprint: RemoteCredentialFingerprintV1::from_secret(&[b'g'; 32]).unwrap(),
        revision: 1,
        issued_at: UtcMicros(1),
        expires_at: UtcMicros(100),
        revoked_at: None,
        capabilities: BTreeSet::from([RemoteCapabilityV1::Query]),
        scope: scope(),
    };
    authority
        .provision_grant(&grant, &admission_with_deadline(&grant, UtcMicros(10)))
        .unwrap();
    let enrollment = issue_enrollment(
        &grant,
        &grant_credential,
        EnrollmentIssueRequestV1 {
            grant_id: grant.grant_id.clone(),
            grant_revision: grant.revision,
            enrollment_id: EntityId::new("enrollment.deadline").unwrap(),
            brain_id: grant.brain_id.clone(),
            node_id: grant.node_id.clone(),
            issued_at: UtcMicros(10),
            expires_at: UtcMicros(90),
            capabilities: grant.capabilities.clone(),
            scope: grant.scope.clone(),
        },
        &enrollment_credential,
    )
    .unwrap();
    let input_digest = canonical_sha256(&"deadline-boundary").unwrap();
    assert_eq!(
        authority.commit_enrollment(&grant, &enrollment, &input_digest, UtcMicros(10)),
        Err(RemoteEnrollmentAuthorityErrorV1::IdentityConflict)
    );
    assert!(authority.load_grant(&grant.grant_id).is_ok());
}

#[test]
fn registered_replay_policy_is_cas_guarded_and_survives_reopen() {
    let store = RegisteredStore::start();
    let repository_scope = scope();
    let grant = EnrollmentGrantV1 {
        grant_id: EntityId::new("grant.replay-policy").unwrap(),
        brain_id: BrainId::new("brain.remote").unwrap(),
        node_id: BrainNodeId::new("node.remote").unwrap(),
        fingerprint: RemoteCredentialFingerprintV1::from_secret(&[b'g'; 32]).unwrap(),
        revision: 1,
        issued_at: UtcMicros(1),
        expires_at: UtcMicros(100),
        revoked_at: None,
        capabilities: BTreeSet::from([RemoteCapabilityV1::Replay]),
        scope: repository_scope.clone(),
    };
    let admission = admission(&grant);
    let evidence = RemoteReplayPolicyEvidenceV1 {
        scope: admission.scope().clone(),
        repository_scope: repository_scope.clone(),
        policy_revision: admission.authority().policy.revision,
        decision: RemoteReplayPolicyDecisionV1::Quarantine,
        policy: admission.authority().policy.clone(),
        configuration_digest: admission.configuration_digest().clone(),
        catalog_digest: admission.catalog_digest().clone(),
        privacy_digest: admission.privacy_digest().clone(),
        revalidated_at: UtcMicros(11),
    };
    let authority =
        RegisteredRemoteReplayPolicyAuthorityV1::from_registered(store.handle.clone()).unwrap();
    authority.provision(&evidence).unwrap();
    assert_eq!(
        authority.policy_for_scope(&repository_scope).unwrap(),
        evidence
    );

    let mut conflicting = evidence.clone();
    conflicting.decision = RemoteReplayPolicyDecisionV1::Admit;
    assert_eq!(
        authority.provision(&conflicting),
        Err(RemoteReplayApplicationErrorV1::PolicyMismatch)
    );
    drop(authority);

    let reopened =
        RegisteredRemoteReplayPolicyAuthorityV1::from_registered(store.handle.clone()).unwrap();
    assert_eq!(
        reopened.policy_for_scope(&repository_scope).unwrap(),
        evidence
    );
}

#[test]
fn registered_enrollment_is_single_use_secret_free_and_survives_reopen() {
    let store = RegisteredStore::start();
    let authority =
        RegisteredRemoteEnrollmentAuthorityV1::from_registered(store.handle.clone()).unwrap();
    let grant_credential = credential(b'g');
    let enrollment_credential = credential(b'e');
    let grant = EnrollmentGrantV1 {
        grant_id: EntityId::new("grant.remote").unwrap(),
        brain_id: BrainId::new("brain.remote").unwrap(),
        node_id: BrainNodeId::new("node.remote").unwrap(),
        fingerprint: RemoteCredentialFingerprintV1::from_secret(&[b'g'; 32]).unwrap(),
        revision: 1,
        issued_at: UtcMicros(1),
        expires_at: UtcMicros(100),
        revoked_at: None,
        capabilities: BTreeSet::from([RemoteCapabilityV1::Query]),
        scope: scope(),
    };
    let admission = admission(&grant);
    authority.provision_grant(&grant, &admission).unwrap();
    let durable_authority = authority.clone();
    let service = RemoteEnrollmentServiceV1::new(authority);
    let outcome = service
        .enroll(
            enrollment_request(),
            &grant_credential,
            &enrollment_credential,
        )
        .unwrap();
    assert_eq!(
        outcome.effect.payload.as_ref().unwrap().fingerprint,
        RemoteCredentialFingerprintV1::from_secret(&[b'e'; 32]).unwrap()
    );
    let persisted_enrollment = durable_authority
        .enrollment_by_id(&EntityId::new("enrollment.remote").unwrap())
        .unwrap();
    assert_eq!(
        &persisted_enrollment,
        outcome.effect.payload.as_ref().unwrap()
    );
    assert_eq!(
        durable_authority
            .authority_enrollment(
                &persisted_enrollment.brain_id,
                &persisted_enrollment.node_id,
                persisted_enrollment.revision,
            )
            .unwrap(),
        persisted_enrollment
    );
    let committed = durable_authority
        .load_commit_receipt(&EntityId::new("enrollment.remote").unwrap())
        .unwrap();
    assert_eq!(committed.admission, admission);
    assert_eq!(
        committed.committed_state_digest,
        canonical_sha256(outcome.effect.payload.as_ref().unwrap()).unwrap()
    );
    assert_eq!(
        service.enroll(
            enrollment_request(),
            &grant_credential,
            &enrollment_credential,
        ),
        Err(
            tracedecay_application::remote::auth::RemoteEnrollmentServiceErrorV1::Authority(
                RemoteEnrollmentAuthorityErrorV1::GrantConsumed
            )
        )
    );

    for entry in std::fs::read_dir(store.directory.path()).unwrap() {
        let entry = entry.unwrap();
        if !entry.file_type().unwrap().is_file() {
            continue;
        }
        let bytes = std::fs::read(entry.path()).unwrap();
        assert!(!bytes.windows(32).any(|window| window == [b'g'; 32]));
        assert!(!bytes.windows(32).any(|window| window == [b'e'; 32]));
    }

    let store = store.restart();
    let reopened =
        RegisteredRemoteEnrollmentAuthorityV1::from_registered(store.handle.clone()).unwrap();
    assert_eq!(
        reopened.load_grant(&grant.grant_id),
        Err(RemoteEnrollmentAuthorityErrorV1::GrantConsumed)
    );
    assert_eq!(
        reopened
            .load_commit_receipt(&EntityId::new("enrollment.remote").unwrap())
            .unwrap(),
        committed
    );
}
