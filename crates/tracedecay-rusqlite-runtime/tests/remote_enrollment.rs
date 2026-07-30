use std::collections::BTreeSet;
use std::path::PathBuf;

use rusqlite::Savepoint;
use tempfile::TempDir;
use tracedecay_application::RequestId;
use tracedecay_application::remote::auth::{
    OpaqueRemoteCredential, RemoteEnrollmentAuthorityErrorV1, RemoteEnrollmentAuthorityPortV1,
    RemoteEnrollmentServiceV1,
};
use tracedecay_application::remote::protocol::{EnrollmentRequestV1, RemoteProtocolRequestV1};
use tracedecay_domain::{
    BrainId, BrainNodeId, EnrollmentGrantV1, EntityId, LocatorDigest, ProjectId, RefId,
    RemoteCapabilityV1, RemoteCredentialFingerprintV1, RemoteRepositoryScopeV1, RepositoryId,
    RepositoryStateSnapshotId, UtcMicros, WorktreeId,
};
use tracedecay_rusqlite_runtime::migration_sql::MigrationSqlHandle;
use tracedecay_rusqlite_runtime::reader::{ExistingReaderLocator, ReaderPool, ReaderQueryExecutor};
use tracedecay_rusqlite_runtime::remote_authority::RegisteredRemoteEnrollmentAuthorityV1;
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
    authority.provision_grant(&grant).unwrap();
    let service = RemoteEnrollmentServiceV1::new(authority);
    let record = service
        .enroll(
            enrollment_request(),
            &grant_credential,
            &enrollment_credential,
        )
        .unwrap();
    assert_eq!(
        record.fingerprint,
        RemoteCredentialFingerprintV1::from_secret(&[b'e'; 32]).unwrap()
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
}
