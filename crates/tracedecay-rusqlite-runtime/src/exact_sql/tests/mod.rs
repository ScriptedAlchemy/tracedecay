use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use tempfile::TempDir;
use tracedecay_domain::LocatorDigest;
use tracedecay_sql::Savepoint;
use tracedecay_store::{
    AdmissionConfigV1, RepositoryWritePayloadV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1,
    StoreIncarnationV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
};

use crate::{
    ExistingWriterLocator, PersistentWriter, StorageOperationExecutor,
    reader::{ExistingReaderLocator, ReaderPool, ReaderQueryExecutor},
};

use super::{clock::fake::FakeClock, *};

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

/// Puts the writer thread on `clock`, so the test holding `clock` decides when
/// every writer-side deadline passes. Authority verification is the only test
/// code that runs on the writer thread, and a transaction's begin check runs
/// before the writer computes its lease deadlines.
///
/// `advance`, when set, moves the clock at every check of its intent. The
/// writer checks a command's lease before that verification and computes the
/// statement's own deadline after it.
struct FakeClockAuthority {
    clock: Arc<FakeClock>,
    advance: Option<(ExactSqlWriteIntent, Duration)>,
}

impl FakeClockAuthority {
    fn installing(clock: &Arc<FakeClock>) -> Arc<Self> {
        Arc::new(Self {
            clock: Arc::clone(clock),
            advance: None,
        })
    }
}

impl ExactSqlWriteAuthority for FakeClockAuthority {
    fn verify(&self, intent: ExactSqlWriteIntent) -> Result<(), ExactSqlError> {
        self.clock.install();
        if let Some((advance_intent, by)) = self.advance
            && advance_intent == intent
        {
            self.clock.advance(by);
        }
        Ok(())
    }
}

struct SlowSchemaAuthority {
    clock: Arc<FakeClock>,
    execute_batch_checks: AtomicUsize,
}

impl ExactSqlWriteAuthority for SlowSchemaAuthority {
    fn verify(&self, intent: ExactSqlWriteIntent) -> Result<(), ExactSqlError> {
        self.clock.install();
        // The check before the batch and two from its progress callbacks
        // together pass the ordinary per-statement deadline, so the
        // revalidated batch provably carries none.
        if intent == ExactSqlWriteIntent::ExecuteBatch
            && self.execute_batch_checks.fetch_add(1, Ordering::AcqRel) < 3
        {
            self.clock.advance(EXACT_SQL_EXECUTION_LIMIT / 2);
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
    ) -> tracedecay_sql::Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
struct NoReads;

impl ReaderQueryExecutor for NoReads {
    fn execute_read(
        &mut self,
        _snapshot: &tracedecay_sql::Transaction<'_>,
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
    tracedecay_sql::Connection::open(&path).unwrap();
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

mod async_dispatch;
mod authority;
mod dispatch;
mod guard;
mod lease;
mod limits;
mod pragma;
mod prepare_cache;
mod reader_contention;
mod transaction;
