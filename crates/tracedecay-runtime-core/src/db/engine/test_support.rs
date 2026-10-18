use std::{fs::OpenOptions, ops::Deref, path::Path, sync::Arc};

use sha2::{Digest, Sha256};
use tracedecay_domain::LocatorDigest;
use tracedecay_rusqlite_runtime::{
    ExactSqlOnlyStorageOperationV1, ExistingWriterLocator, PersistentWriter,
    exact_sql::{ExactSqlError, ExactSqlHandle, ExactSqlWriteAuthority, ExactSqlWriteIntent},
    reader::{ExactSqlOnlyReaderV1, ExistingReaderLocator, ReaderPool},
};
use tracedecay_store::{
    AdmissionConfigV1, StoreIncarnationV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
};

use super::{Connection, Executor, IntoParams, QueryExecutor, Result as EngineResult, Rows};

pub struct TestConnection {
    connection: Connection,
    _readers: ReaderPool<ExactSqlOnlyReaderV1>,
    _writer: PersistentWriter,
}

impl TestConnection {
    pub fn open(path: &Path) -> Self {
        Self::open_inner(path, Some(Arc::new(AllowTestWrites)))
    }

    #[cfg(test)]
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
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .expect("create engine test database");
        let path = path
            .canonicalize()
            .expect("canonicalize engine test database");
        let mut identity_hasher = Sha256::new();
        identity_hasher.update(b"tracedecay.engine-test.identity.v1\0");
        identity_hasher.update(path.as_os_str().as_encoded_bytes());
        let identity_digest = hex::encode(identity_hasher.finalize());
        let binding: StoreRuntimeBindingV1 = serde_json::from_value(serde_json::json!({
            "shard_id": {
                "brain_id": format!("brain.{}", &identity_digest[..32]),
                "profile_id": format!("profile.{}", &identity_digest[32..]),
                "scope": {
                    "kind": "project",
                    "project_id": format!("project.{}", &identity_digest[..32])
                }
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
        let runtime =
            crate::native_runtime::handle().expect("engine fixture requires an Asupersync runtime");
        let writer = PersistentWriter::start(
            &runtime,
            ExistingWriterLocator::new(binding.clone(), locator.clone(), path.clone())
                .expect("valid engine test writer locator"),
            AdmissionConfigV1::default(),
            ExactSqlOnlyStorageOperationV1,
        )
        .expect("start engine test writer");
        let readers = ReaderPool::start(
            ExistingReaderLocator::new(binding, locator, path)
                .expect("valid engine test reader locator"),
            AdmissionConfigV1::default().readers,
            ExactSqlOnlyReaderV1,
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
