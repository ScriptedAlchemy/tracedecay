use std::{ops::Deref, path::Path, sync::Arc};

use rusqlite::Savepoint;
use sha2::{Digest, Sha256};
use tracedecay_domain::LocatorDigest;
use tracedecay_rusqlite_runtime::{
    ExistingWriterLocator, PersistentWriter, StorageOperationExecutor,
    migration_sql::{
        MigrationSqlError, MigrationSqlHandle, MigrationSqlWriteAuthority, MigrationSqlWriteIntent,
    },
    reader::{ExistingReaderLocator, ReaderPool, ReaderQueryExecutor},
};
use tracedecay_store::{
    AdmissionConfigV1, RepositoryWritePayloadV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1,
    StoreIncarnationV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
};

use super::Connection;

pub struct TestConnection {
    connection: Connection,
    _readers: ReaderPool<NoReads>,
    _writer: PersistentWriter,
}

impl TestConnection {
    pub fn open(path: &Path) -> Self {
        Self::open_inner(path, Some(Arc::new(AllowTestWrites)))
    }

    pub fn open_without_write_authority(path: &Path) -> Self {
        Self::open_inner(path, None)
    }

    pub fn open_with_write_authority(
        path: &Path,
        authority: Arc<dyn MigrationSqlWriteAuthority>,
    ) -> Self {
        Self::open_inner(path, Some(authority))
    }

    fn open_inner(path: &Path, authority: Option<Arc<dyn MigrationSqlWriteAuthority>>) -> Self {
        rusqlite::Connection::open(path).expect("create engine test database");
        let path = path
            .canonicalize()
            .expect("canonicalize engine test database");
        let binding: StoreRuntimeBindingV1 = serde_json::from_value(serde_json::json!({
            "shard_id": {
                "brain_id": "brain.engine-test",
                "profile_id": "profile.engine-test",
                "scope": { "kind": "project", "project_id": "project.engine-test" }
            },
            "incarnation": 1,
            "authority_epoch": 1
        }))
        .expect("construct engine test binding");
        let digest = hex::encode(Sha256::digest(path.as_os_str().as_encoded_bytes()));
        let locator = VerifiedStoreLocatorV1::new(
            binding.shard_id.clone(),
            StoreIncarnationV1::new(1).expect("valid engine test incarnation"),
            LocatorDigest::new(format!("sha256:{digest}"))
                .expect("valid engine test locator digest"),
        );
        let writer = PersistentWriter::start(
            ExistingWriterLocator::new(binding.clone(), locator.clone(), path.clone())
                .expect("valid engine test writer locator"),
            AdmissionConfigV1::default(),
            NoWrites,
        )
        .expect("start engine test writer");
        let readers = ReaderPool::start(
            ExistingReaderLocator::new(binding, locator, path)
                .expect("valid engine test reader locator"),
            AdmissionConfigV1::default().readers,
            NoReads,
        )
        .expect("start engine test readers");
        let handle = MigrationSqlHandle::attach(&writer, &readers)
            .expect("attach engine test migration SQL channel");
        let handle = match authority {
            Some(authority) => handle
                .with_write_authority(authority)
                .expect("attach engine test write authority"),
            None => handle,
        };
        let connection = Connection::attach(handle);
        Self {
            connection,
            _readers: readers,
            _writer: writer,
        }
    }
}

impl Deref for TestConnection {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}

struct AllowTestWrites;

impl MigrationSqlWriteAuthority for AllowTestWrites {
    fn verify(&self, _intent: MigrationSqlWriteIntent) -> Result<(), MigrationSqlError> {
        Ok(())
    }
}

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
    ) -> Result<RuntimeReadOutcomeV1, tracedecay_store::StorageRuntimeErrorV1> {
        unreachable!("engine test SQL does not use the product read contract")
    }
}
