use std::{
    sync::atomic::AtomicUsize,
    time::{Duration, Instant},
};

use rusqlite::Savepoint;
use tempfile::TempDir;
use tracedecay_domain::LocatorDigest;
use tracedecay_store::{
    AdmissionConfigV1, RepositoryWritePayloadV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1,
    StoreIncarnationV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
};

use crate::{
    ExistingWriterLocator, PersistentWriter, StorageOperationExecutor,
    reader::{ExistingReaderLocator, ReaderPool, ReaderQueryExecutor},
};

use super::*;

struct AtomicWriteAuthority(Arc<AtomicBool>);

impl ExactSqlWriteAuthority for AtomicWriteAuthority {
    fn verify(&self, _intent: ExactSqlWriteIntent) -> Result<(), ExactSqlError> {
        if self.0.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(ExactSqlError::AuthorityDenied("revoked".to_owned()))
        }
    }
}

struct SlowSchemaAuthority {
    execute_batch_checks: AtomicUsize,
}

impl ExactSqlWriteAuthority for SlowSchemaAuthority {
    fn verify(&self, intent: ExactSqlWriteIntent) -> Result<(), ExactSqlError> {
        if intent == ExactSqlWriteIntent::ExecuteBatch
            && self.execute_batch_checks.fetch_add(1, Ordering::AcqRel) < 3
        {
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }
}

struct RevokeDuringSchemaStep {
    execute_batch_checks: AtomicUsize,
}

impl ExactSqlWriteAuthority for RevokeDuringSchemaStep {
    fn verify(&self, intent: ExactSqlWriteIntent) -> Result<(), ExactSqlError> {
        if intent == ExactSqlWriteIntent::ExecuteBatch
            && self.execute_batch_checks.fetch_add(1, Ordering::AcqRel) >= 1
        {
            return Err(ExactSqlError::AuthorityDenied(
                "revoked during authority-revalidated batch".to_owned(),
            ));
        }
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
        unreachable!("exact SQL queries bypass the closed product read executor")
    }
}

struct Fixture {
    _directory: TempDir,
    writer: PersistentWriter,
    readers: ReaderPool<NoReads>,
}

fn binding() -> StoreRuntimeBindingV1 {
    serde_json::from_value(serde_json::json!({
        "shard_id": {
            "brain_id": "brain.exact-sql",
            "profile_id": "profile.exact-sql",
            "scope": { "kind": "project", "project_id": "project.exact-sql" }
        },
        "incarnation": 3,
        "authority_epoch": 11
    }))
    .unwrap()
}

fn locator(binding: &StoreRuntimeBindingV1, byte: char) -> VerifiedStoreLocatorV1 {
    VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        StoreIncarnationV1::new(3).unwrap(),
        LocatorDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap(),
    )
}

fn fixture(writer_digest: char, reader_digest: char) -> Fixture {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("exact-sql.sqlite3");
    rusqlite::Connection::open(&path).unwrap();
    let path = path.canonicalize().unwrap();
    let binding = binding();
    let writer = PersistentWriter::start(
        ExistingWriterLocator::new(
            binding.clone(),
            locator(&binding, writer_digest),
            path.clone(),
        )
        .unwrap(),
        AdmissionConfigV1::default(),
        NoWrites,
    )
    .unwrap();
    let readers = ReaderPool::start(
        ExistingReaderLocator::new(binding.clone(), locator(&binding, reader_digest), path)
            .unwrap(),
        AdmissionConfigV1::default().readers,
        NoReads,
    )
    .unwrap();
    Fixture {
        _directory: directory,
        writer,
        readers,
    }
}

fn statement(sql: &str, params: Vec<ExactSqlValue>) -> ExactSqlStatement {
    ExactSqlStatement::new(sql.to_owned(), params).unwrap()
}

mod authority;
mod dispatch;
mod guard;
mod lease;
mod limits;
mod pragma;
mod transaction;
