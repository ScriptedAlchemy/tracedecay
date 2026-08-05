//! Synchronous registered workflow store for the storage suites.

use std::path::PathBuf;

use rusqlite::{Connection, Savepoint};
use tempfile::TempDir;
use tracedecay_domain::LocatorDigest;
use tracedecay_rusqlite_runtime::exact_sql::ExactSqlHandle;
use tracedecay_rusqlite_runtime::reader::{ExistingReaderLocator, ReaderPool, ReaderQueryExecutor};
use tracedecay_rusqlite_runtime::workflow::install_workflow_schema;
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
        unreachable!("workflow writes only through the registered exact-SQL channel")
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
        unreachable!("workflow reads only through the registered exact-SQL channel")
    }
}

/// A started registered workflow store.
pub struct RegisteredWorkflowStore {
    storage: ExactSqlHandle,
    path: PathBuf,
    _writer: PersistentWriter,
    _readers: ReaderPool<NoTypedReads>,
    _directory: TempDir,
}

impl RegisteredWorkflowStore {
    pub fn start(name: &str) -> Self {
        Self::start_with_setup(name, |_| {})
    }

    /// Starts a registered store, running `setup` against the file after the
    /// workflow schema is installed and before the writer takes ownership.
    pub fn start_with_setup(name: &str, setup: impl FnOnce(&Connection)) -> Self {
        let directory = TempDir::new().expect("workflow store directory");
        let path = directory.path().join(format!("{name}.sqlite3"));
        {
            let connection = Connection::open(&path).expect("open workflow store");
            install_workflow_schema(&connection).expect("install workflow schema");
            setup(&connection);
        }
        let path = path.canonicalize().expect("canonicalize workflow store");
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
                .expect("workflow store writer locator"),
            AdmissionConfigV1::default(),
            NoTypedWrites,
        )
        .expect("start workflow store writer");
        let readers = ReaderPool::start(
            ExistingReaderLocator::new(binding, locator, path.clone())
                .expect("workflow store reader locator"),
            AdmissionConfigV1::default().readers,
            NoTypedReads,
        )
        .expect("start workflow store readers");
        let handle = ExactSqlHandle::attach(&writer, &readers).expect("attach workflow store");
        Self {
            storage: handle,
            path,
            _writer: writer,
            _readers: readers,
            _directory: directory,
        }
    }

    pub fn storage(&self) -> &ExactSqlHandle {
        &self.storage
    }

    /// Opens a short-lived connection for assertions that inspect stored rows
    /// directly. Writes still go through the registered channel.
    pub fn inspect<T>(&self, read: impl FnOnce(&Connection) -> T) -> T {
        let connection = Connection::open(&self.path).expect("open workflow store for inspection");
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
    .expect("workflow store binding")
}

fn locator(binding: &StoreRuntimeBindingV1) -> VerifiedStoreLocatorV1 {
    VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        StoreIncarnationV1::new(1).expect("workflow store incarnation"),
        LocatorDigest::new(format!("sha256:{}", "5".repeat(64))).expect("workflow store digest"),
    )
}
