//! Synchronous registered Work store for the storage suites.
//!
//! Work storage has exactly one transaction implementation: the registered
//! exact-SQL channel the daemon uses. These tests run against that same
//! channel rather than a private connection, so a test can never observe a
//! transaction shape production does not have.

use std::path::PathBuf;

use rusqlite::{Connection, Savepoint};
use tempfile::TempDir;
use tracedecay_domain::LocatorDigest;
use tracedecay_rusqlite_runtime::exact_sql::ExactSqlHandle;
use tracedecay_rusqlite_runtime::reader::{ExistingReaderLocator, ReaderPool, ReaderQueryExecutor};
use tracedecay_rusqlite_runtime::work::{WorkSqliteStorage, install_work_schema};
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
        unreachable!("Work storage writes only through the registered exact-SQL channel")
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
        unreachable!("Work storage reads only through the registered exact-SQL channel")
    }
}

/// A started registered store: writer, readers, and the Work storage bound to
/// them. Dropping it stops both actors and removes the directory.
pub struct RegisteredWorkStore {
    storage: WorkSqliteStorage,
    path: PathBuf,
    _writer: PersistentWriter,
    _readers: ReaderPool<NoTypedReads>,
    _directory: TempDir,
}

impl RegisteredWorkStore {
    /// Starts a registered store with the Work schema installed.
    pub fn start(name: &str) -> Self {
        Self::start_with_setup(name, |_| {})
    }

    /// Starts a registered store, running `setup` against the file after the
    /// Work schema is installed and before the writer takes ownership.
    pub fn start_with_setup(name: &str, setup: impl FnOnce(&Connection)) -> Self {
        let directory = TempDir::new().expect("work store directory");
        let path = directory.path().join(format!("{name}.sqlite3"));
        {
            let connection = Connection::open(&path).expect("open work store");
            install_work_schema(&connection).expect("install work schema");
            setup(&connection);
        }
        let path = path.canonicalize().expect("canonicalize work store");
        Self::open(name, path, directory)
    }

    /// Stops this store and starts a new one over the same file, the way a
    /// daemon restart rebinds the registered channel to persisted state.
    pub fn restart(self, name: &str) -> Self {
        let Self {
            storage,
            path,
            _writer: writer,
            _readers: readers,
            _directory: directory,
        } = self;
        drop(storage);
        drop(readers);
        drop(writer);
        Self::open(name, path, directory)
    }

    fn open(name: &str, path: PathBuf, directory: TempDir) -> Self {
        let binding = binding(name);
        let locator = locator(&binding);
        let writer = PersistentWriter::start(
            ExistingWriterLocator::new(binding.clone(), locator.clone(), path.clone())
                .expect("work store writer locator"),
            AdmissionConfigV1::default(),
            NoTypedWrites,
        )
        .expect("start work store writer");
        let readers = ReaderPool::start(
            ExistingReaderLocator::new(binding, locator, path.clone())
                .expect("work store reader locator"),
            AdmissionConfigV1::default().readers,
            NoTypedReads,
        )
        .expect("start work store readers");
        let handle = ExactSqlHandle::attach(&writer, &readers).expect("attach work store");
        Self {
            storage: WorkSqliteStorage::from_registered(handle),
            path,
            _writer: writer,
            _readers: readers,
            _directory: directory,
        }
    }

    pub fn storage(&self) -> &WorkSqliteStorage {
        &self.storage
    }

    /// Opens a short-lived connection for assertions that inspect stored rows
    /// directly. Writes still go through the registered channel.
    pub fn inspect<T>(&self, read: impl FnOnce(&Connection) -> T) -> T {
        let connection = Connection::open(&self.path).expect("open work store for inspection");
        read(&connection)
    }

    pub fn count(&self, table: &str) -> i64 {
        self.inspect(|connection| {
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap_or_else(|error| panic!("count {table}: {error}"))
        })
    }
}

fn binding(name: &str) -> StoreRuntimeBindingV1 {
    serde_json::from_value(serde_json::json!({
        "shard_id": {
            "brain_id": "brain.work-storage",
            "profile_id": "profile.work-storage",
            "scope": { "kind": "project", "project_id": format!("project.work-storage.{name}") }
        },
        "incarnation": 1,
        "authority_epoch": 1
    }))
    .expect("work store binding")
}

fn locator(binding: &StoreRuntimeBindingV1) -> VerifiedStoreLocatorV1 {
    VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        StoreIncarnationV1::new(1).expect("work store incarnation"),
        LocatorDigest::new(format!("sha256:{}", "5".repeat(64))).expect("work store digest"),
    )
}
