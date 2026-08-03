use std::{ops::Deref, path::Path, sync::Arc};

use rusqlite::Savepoint;
use sha2::{Digest, Sha256};
use tracedecay_domain::LocatorDigest;
use tracedecay_rusqlite_runtime::{
    ExistingWriterLocator, PersistentWriter, StorageOperationExecutor,
    exact_sql::{ExactSqlError, ExactSqlHandle, ExactSqlWriteAuthority, ExactSqlWriteIntent},
    reader::{ExistingReaderLocator, ReaderPool, ReaderQueryExecutor},
};
use tracedecay_store::{
    AdmissionConfigV1, RepositoryWritePayloadV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1,
    StoreIncarnationV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
};

use super::{Connection, Executor, IntoParams, QueryExecutor, Result as EngineResult, Rows};

pub struct TestConnection {
    connection: Connection,
    _readers: ReaderPool<NoReads>,
    _writer: PersistentWriter,
}

impl TestConnection {
    pub fn open(path: &Path) -> Self {
        Self::open_inner(path, Some(Arc::new(AllowTestWrites)))
    }

    // Only the in-crate engine tests drive this constructor; under
    // `feature = "test-helpers"` alone it is an unused crate-private item.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn open_without_write_authority(path: &Path) -> Self {
        Self::open_inner(path, None)
    }

    pub fn open_with_write_authority(
        path: &Path,
        authority: Arc<dyn ExactSqlWriteAuthority>,
    ) -> Self {
        Self::open_inner(path, Some(authority))
    }

    // This test-only constructor turns fixture setup failures into immediate test failures.
    #[allow(clippy::expect_used)]
    fn open_inner(path: &Path, authority: Option<Arc<dyn ExactSqlWriteAuthority>>) -> Self {
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
        let handle = ExactSqlHandle::attach(&writer, &readers)
            .expect("attach engine test exact SQL channel");
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

// The test double has to satisfy the same executor ports as a real
// `Connection`; `Deref` alone does not carry trait bounds, so schema helpers
// generic over `Executor` could not accept it.
impl QueryExecutor for TestConnection {
    async fn query<P>(&self, sql: &str, params: P) -> EngineResult<Rows>
    where
        P: IntoParams,
    {
        Connection::query(&self.connection, sql, params).await
    }
}

impl Executor for TestConnection {
    async fn execute<P>(&self, sql: &str, params: P) -> EngineResult<u64>
    where
        P: IntoParams,
    {
        Connection::execute(&self.connection, sql, params).await
    }

    async fn execute_batch(&self, sql: &str) -> EngineResult<()> {
        Connection::execute_batch(&self.connection, sql).await
    }
}

struct AllowTestWrites;

impl ExactSqlWriteAuthority for AllowTestWrites {
    fn verify(&self, _intent: ExactSqlWriteIntent) -> Result<(), ExactSqlError> {
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
