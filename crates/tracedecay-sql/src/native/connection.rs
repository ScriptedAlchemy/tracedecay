use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    io::Read,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock, Mutex, OnceLock, PoisonError, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use turso_core::{DatabaseOpts, IOResult, LimboError, OpenDbAsyncState, OpenOptions, StepResult};
use turso_parser::ast::Cmd;

use super::{
    Error, Result, Rows, Value,
    confined_io::{self, ConfinedIo},
    policy,
};
use crate::{Capability, FileIdentity, PROGRESS_INTERVAL_OPS};

const MAX_QUERY_ROWS: usize = 10_000;
const MAX_QUERY_BYTES: usize = 64 * 1024 * 1024;
const MAX_SQL_BYTES: usize = 1024 * 1024;
const MAX_SQL_PARAMETERS: usize = 32_766;
const MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;
const STATEMENT_CACHE_CAPACITY: usize = 128;
const SLEEP_SLICE: Duration = Duration::from_millis(5);
const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// `PRAGMA application_id` of every native store: "TDN1".
pub(crate) const APPLICATION_ID: u32 = 0x5444_4E31;
const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// One engine instance per file per process. POSIX record locks do not
/// exclude the same process, so a second in-process open of a file shares the
/// live instance instead of starting another engine on it.
static OPEN_IDENTITIES: LazyLock<Mutex<HashMap<FileIdentity, Weak<Shared>>>> =
    LazyLock::new(Mutex::default);

fn registry() -> std::sync::MutexGuard<'static, HashMap<FileIdentity, Weak<Shared>>> {
    lock(&OPEN_IDENTITIES)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The facade's connection policy. Deleted with the runtime-core facade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Reader,
    Writer,
}

impl From<Access> for Capability {
    fn from(access: Access) -> Self {
        match access {
            Access::Reader => Self::Reader,
            // The facade installs schema, triggers and views included, through
            // its writer, so it keeps the maintenance verdict until it is gone.
            Access::Writer => Self::Maintenance,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransactionBehavior {
    Deferred,
    Immediate,
}

pub type Authority = Arc<dyn Fn() -> std::result::Result<(), String> + Send + Sync>;
type Interrupt = Arc<dyn Fn() -> bool + Send + Sync>;

/// What a running statement re-checks before it starts, between sleep slices
/// and every [`PROGRESS_INTERVAL_OPS`] VM operations. Nothing here is compiled
/// into a statement.
#[derive(Clone)]
pub struct ExecutionGuard {
    pub deadline: Option<Instant>,
    pub cancelled: Arc<AtomicBool>,
    pub authority: Option<Authority>,
    pub(crate) interrupt: Option<Interrupt>,
}

impl ExecutionGuard {
    pub fn new(
        deadline: Option<Instant>,
        cancelled: Arc<AtomicBool>,
        authority: Option<Authority>,
    ) -> Self {
        Self {
            deadline,
            cancelled,
            authority,
            interrupt: None,
        }
    }

    pub(crate) fn polling(interrupt: Interrupt) -> Self {
        Self {
            deadline: None,
            cancelled: Arc::default(),
            authority: None,
            interrupt: Some(interrupt),
        }
    }

    pub fn verify(&self) -> Result<()> {
        if self.interrupt.as_ref().is_some_and(|f| f()) {
            return Err(Error::Cancelled);
        }
        self.verify_admission()
    }

    /// [`Self::verify`] without the interrupt poll.
    fn verify_admission(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(Error::DeadlineExceeded);
        }
        if let Some(authority) = &self.authority {
            authority().map_err(Error::Authority)?;
        }
        Ok(())
    }
}

/// The guards in force on one connection, shared with its progress handler.
/// Scopes stack; every guard on the stack is checked.
struct GuardSlot {
    state: Mutex<SlotState>,
    confined: Option<Arc<ConfinedIo>>,
}

#[derive(Default)]
struct SlotState {
    guards: Vec<ExecutionGuard>,
    /// COMMIT and rollback run with every check off.
    suspended: bool,
    /// Why the progress handler interrupted the running statement.
    tripped: Option<Error>,
    /// Progress ticks one statement may run; `None` is unbounded.
    tick_budget: Option<u64>,
    /// Progress ticks since the running statement started.
    // ponytail: one counter per connection, so two statements stepped in
    // alternation share a budget; per-statement counters if that matters.
    ticks: u64,
}

impl GuardSlot {
    fn state(&self) -> std::sync::MutexGuard<'_, SlotState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn check(&self) -> Result<()> {
        self.check_with(ExecutionGuard::verify, true)
    }

    fn check_with(
        &self,
        verify: impl Fn(&ExecutionGuard) -> Result<()>,
        verify_identity: bool,
    ) -> Result<()> {
        let guards = {
            let state = self.state();
            if state.suspended {
                return Ok(());
            }
            state.guards.clone()
        };
        guards.iter().try_for_each(verify)?;
        match &self.confined {
            Some(io) if verify_identity => io.verify(),
            _ => Ok(()),
        }
    }

    /// Admits a statement about to start: its budget restarts and the guards
    /// are checked. A scope's interrupt poll is not: as on SQLite, only a
    /// running statement is polled, so a statement that finishes within one
    /// progress interval is never interrupted. The file identity is checked
    /// when `verify_identity`: every statement that starts a transaction, so
    /// a statement inside an open transaction relies on the check that
    /// opened it and on the progress ticks.
    fn start(&self, verify_identity: bool) -> Result<()> {
        self.state().ticks = 0;
        self.check_with(ExecutionGuard::verify_admission, verify_identity)
    }

    fn progress(&self) -> bool {
        let over_budget = {
            let mut state = self.state();
            state.ticks += 1;
            !state.suspended && state.tick_budget.is_some_and(|budget| state.ticks > budget)
        };
        let verdict = if over_budget {
            Err(Error::OperationLimitExceeded)
        } else {
            self.check()
        };
        match verdict {
            Ok(()) => false,
            Err(error) => {
                self.state().tripped = Some(error);
                true
            }
        }
    }

    fn take_tripped(&self) -> Option<Error> {
        self.state().tripped.take()
    }

    fn push(&self, guard: ExecutionGuard) {
        self.state().guards.push(guard);
    }

    fn pop(&self) {
        self.state().guards.pop();
    }

    fn set_suspended(&self, suspended: bool) -> bool {
        std::mem::replace(&mut self.state().suspended, suspended)
    }
}

/// A handle on the process's single engine instance for one store.
#[derive(Clone)]
pub struct Database {
    shared: Arc<Shared>,
}

struct Shared {
    /// Set once the store is admitted and registered.
    identity: OnceLock<FileIdentity>,
    engine: Option<Engine>,
    /// `(WAL checkpoint sequence, frames copied back)` after the driver's
    /// last checkpoint; the engine keeps its backfill count private.
    checkpointed: Mutex<Option<(u32, u64)>>,
}

struct Engine {
    database: Arc<turso_core::Database>,
    io: Arc<dyn turso_core::IO>,
    confined: Option<Arc<ConfinedIo>>,
}

impl Drop for Shared {
    fn drop(&mut self) {
        // An unregistered instance is dropped by its opener, which already
        // holds the registry.
        let Some(&identity) = self.identity.get() else {
            return;
        };
        // The engine's descriptors close while the registry is held, so no
        // reopen can lock the file before this instance has released it.
        let mut registry = registry();
        drop(self.engine.take());
        if registry
            .get(&identity)
            .is_some_and(|entry| entry.strong_count() == 0)
        {
            registry.remove(&identity);
        }
    }
}

impl Database {
    /// Opens or creates the store at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_path(path, true)
    }

    pub(crate) fn open_path(path: &Path, create: bool) -> Result<Self> {
        if path.as_os_str() == ":memory:" {
            return Self::open_memory();
        }
        let path = canonical(path)?;
        let text = path.to_str().ok_or_else(|| {
            Error::InvalidOperation("native database path is not UTF-8".to_owned())
        })?;
        let mut registry = registry();
        if let Ok(identity) = confined_io::path_identity(text)
            && let Some(shared) = registry.get(&identity).and_then(Weak::upgrade)
        {
            return Ok(Self { shared });
        }
        let file = confined_io::open_regular(&path, create).map_err(|error| {
            Error::InvalidOperation(format!("open native database {}: {error}", path.display()))
        })?;
        let identity = confined_io::held_identity(&file)?;
        if let Some(shared) = registry.get(&identity).and_then(Weak::upgrade) {
            // ponytail: the path was renamed onto a live store between stat
            // and open. Closing this descriptor would drop that store's lock.
            std::mem::forget(file);
            return Ok(Self { shared });
        }
        let header = preflight(&file)?;
        let confined = Arc::new(ConfinedIo::new(text.to_owned(), file, identity)?);
        let io: Arc<dyn turso_core::IO> = confined.clone();
        #[cfg(all(unix, feature = "test-helpers"))]
        let io = super::crash_io::wrap(io);
        let main = io.open_file(text, turso_core::OpenFlags::Create, true)?;
        let storage = Arc::new(turso_core::storage::database::DatabaseFile::new(main));
        let database = open_engine(
            &io,
            text,
            OpenOptions::new(policy::dialect()).storage(storage),
        )?;
        let database = Self {
            shared: Arc::new(Shared {
                identity: OnceLock::new(),
                engine: Some(Engine {
                    database,
                    io,
                    confined: Some(confined),
                }),
                checkpointed: Mutex::default(),
            }),
        };
        database.admit_format(header)?;
        let _ = database.shared.identity.set(identity);
        registry.insert(identity, Arc::downgrade(&database.shared));
        Ok(database)
    }

    fn open_memory() -> Result<Self> {
        let io = turso_core::Database::io_for_path(":memory:")?;
        let database = turso_core::Database::open(
            Arc::clone(&io),
            ":memory:",
            OpenOptions::new(policy::dialect()).db_opts(database_options()),
        )?;
        Ok(Self {
            shared: Arc::new(Shared {
                identity: OnceLock::new(),
                engine: Some(Engine {
                    database,
                    io,
                    confined: None,
                }),
                checkpointed: Mutex::default(),
            }),
        })
    }

    /// Checks the stamp the engine reads through the WAL, and stamps a fresh
    /// store. The stamp is checkpointed at once: the engine writes an
    /// unstamped first page straight to the main file, and the preflight reads
    /// only the main file.
    fn admit_format(&self, header: Header) -> Result<()> {
        let connection = self.connect_with(Capability::Maintenance, false)?;
        let application_id = connection.pragma_u32("application_id")?;
        if application_id == APPLICATION_ID && header == Header::Stamped {
            return Ok(());
        }
        if header == Header::Stamped
            || (application_id != APPLICATION_ID
                && (application_id != 0 || connection.pragma_u32("schema_version")? != 0))
        {
            return Err(foreign_format());
        }
        if application_id != APPLICATION_ID {
            connection.execute_unchecked(&format!("PRAGMA application_id = {APPLICATION_ID}"))?;
        }
        connection.execute_unchecked("PRAGMA wal_checkpoint(TRUNCATE)")
    }

    fn engine(&self) -> &Engine {
        self.shared
            .engine
            .as_ref()
            .expect("the engine lives until the last handle drops")
    }

    /// The canonical path of a file store; `None` in memory.
    pub(crate) fn path(&self) -> Option<&str> {
        self.engine().confined.as_ref().map(|io| io.main())
    }

    pub(crate) fn file_has_moved(&self) -> Result<bool> {
        self.engine()
            .confined
            .as_ref()
            .map_or(Ok(false), |io| io.has_moved())
    }

    pub fn connect(&self, access: Access) -> Result<Connection> {
        self.connect_with(access.into(), false)
    }

    pub(crate) fn connect_with(
        &self,
        capability: Capability,
        read_only: bool,
    ) -> Result<Connection> {
        let engine = self.engine();
        if let Some(confined) = &engine.confined {
            confined.verify()?;
        }
        let inner = engine.database.connect()?;
        inner.set_foreign_keys_enabled(true);
        // A commit is acknowledged only after its WAL frames are fsynced: at
        // NORMAL a power loss drops acknowledged commits. Policy denies
        // changing it.
        inner.set_sync_mode(turso_core::SyncMode::Full);
        inner.set_busy_timeout(DEFAULT_BUSY_TIMEOUT);
        // The runtime's checkpoint lane is the only checkpoint scheduler.
        inner.wal_auto_actions_disable();
        // Spilling to a temp file would need a path outside the confinement.
        inner.set_temp_store(turso_core::TempStore::Memory);
        let guard = Arc::new(GuardSlot {
            state: Mutex::default(),
            confined: engine.confined.clone(),
        });
        let progress = Arc::clone(&guard);
        inner.set_progress_handler(
            PROGRESS_INTERVAL_OPS.unsigned_abs().into(),
            Some(Box::new(move || progress.progress())),
        );
        let connection = Connection {
            inner,
            guard,
            capability: Cell::new(capability),
            transaction_control_denied: Cell::new(false),
            read_only,
            cache: RefCell::default(),
            database: self.clone(),
        };
        connection.set_capability(capability);
        Ok(connection)
    }
}

fn database_options() -> DatabaseOpts {
    DatabaseOpts::new()
        .with_index_method(true)
        .with_generated_columns(true)
}

/// Opens without the engine's path registry: [`OPEN_IDENTITIES`] is the only
/// registry, keyed by file identity rather than a pathname.
fn open_engine(
    io: &Arc<dyn turso_core::IO>,
    path: &str,
    options: OpenOptions,
) -> Result<Arc<turso_core::Database>> {
    let options = options.db_opts(database_options());
    let mut state = OpenDbAsyncState::new();
    loop {
        match turso_core::Database::do_open_async(&mut state, Arc::clone(io), path, &options)
            .map_err(|error| *error)?
        {
            IOResult::Done(database) => return Ok(database),
            IOResult::IO(completion) => completion.wait(&**io)?,
        }
    }
}

fn canonical(path: &Path) -> Result<PathBuf> {
    let name = path.file_name().ok_or_else(|| {
        Error::InvalidOperation(format!("{} names no database file", path.display()))
    })?;
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let parent = parent.canonicalize().map_err(|error| {
        Error::InvalidOperation(format!("resolve {}: {error}", parent.display()))
    })?;
    Ok(parent.join(name))
}

fn foreign_format() -> Error {
    Error::ForeignFormat
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Header {
    /// An empty file: a store being created.
    Empty,
    /// Only the engine's first page, written before the stamp was
    /// checkpointed: a creation interrupted by a crash.
    Unstamped,
    Stamped,
}

/// Reads the main file's header before the engine touches it. Anything but an
/// empty file, a single unstamped first page, or a SQLite-format file stamped
/// TDN1 is refused.
fn preflight(file: &std::fs::File) -> Result<Header> {
    let length = file
        .metadata()
        .map_err(|error| Error::InvalidOperation(format!("inspect native database: {error}")))?
        .len();
    if length == 0 {
        return Ok(Header::Empty);
    }
    let mut header = [0_u8; 100];
    let mut reader = file;
    reader
        .read_exact(&mut header)
        .map_err(|_| foreign_format())?;
    let field = |offset: usize| {
        u32::from_be_bytes([
            header[offset],
            header[offset + 1],
            header[offset + 2],
            header[offset + 3],
        ])
    };
    if &header[..16] != SQLITE_MAGIC {
        return Err(foreign_format());
    }
    let page_size = match u16::from_be_bytes([header[16], header[17]]) {
        1 => 65_536,
        size => u64::from(size),
    };
    match field(68) {
        APPLICATION_ID => Ok(Header::Stamped),
        0 if field(28) == 1 && length == page_size => Ok(Header::Unstamped),
        _ => Err(foreign_format()),
    }
}

/// A compiled statement and the capability it was authorized under.
pub(crate) struct Prepared {
    pub(crate) statement: turso_core::Statement,
    sql: String,
    capability: Capability,
    ddl: bool,
    /// INSERT, UPDATE or DELETE: interrupting one ends its transaction.
    write: bool,
    pub(crate) columns: Arc<[String]>,
}

/// A compiled non-DDL statement waiting in the cache for its SQL text.
struct Cached {
    statement: turso_core::Statement,
    write: bool,
    columns: Arc<[String]>,
}

/// Exclusively owned by one thread: the writer actor or one reader worker.
pub struct Connection {
    inner: Arc<turso_core::Connection>,
    guard: Arc<GuardSlot>,
    capability: Cell<Capability>,
    /// Set when a statement was refused as transaction control, so a
    /// capability scope fails even if its operation swallowed the error.
    transaction_control_denied: Cell<bool>,
    read_only: bool,
    /// Compiled non-DDL statements by `(capability, sql)`, nested so a hit
    /// moves the owned SQL key out instead of allocating a lookup key.
    cache: RefCell<HashMap<Capability, HashMap<String, Cached>>>,
    database: Database,
}

impl Connection {
    fn authorize(&self, command: &Cmd, capability: Capability) -> Result<()> {
        let verdict = policy::authorize(command, capability);
        if matches!(verdict, Err(Error::TransactionControlDenied)) {
            self.transaction_control_denied.set(true);
        }
        verdict
    }

    pub(crate) fn take_transaction_control_denied(&self) -> bool {
        self.transaction_control_denied.replace(false)
    }

    pub(crate) fn set_capability(&self, capability: Capability) {
        self.capability.set(capability);
        self.inner.set_query_only(
            self.read_only || matches!(capability, Capability::Reader | Capability::ExactSqlRead),
        );
    }

    /// Bounds every statement to `ops` VM operations, counted at the progress
    /// interval; `None` lifts the bound.
    pub(crate) fn set_op_budget(&self, ops: Option<u64>) {
        let interval = u64::from(PROGRESS_INTERVAL_OPS.unsigned_abs());
        self.guard.state().tick_budget = ops.map(|ops| ops.div_ceil(interval));
    }

    /// SQLite's NOOP checkpoint: `(WAL frames, frames already copied back)`,
    /// copying nothing.
    pub(crate) fn wal_frames(&self) -> Result<(u64, u64)> {
        if self.database.engine().confined.is_none() {
            return Ok((0, 0));
        }
        let state = self.inner.wal_state()?;
        let checkpointed = match *lock(&self.database.shared.checkpointed) {
            Some((sequence, frames)) if sequence == state.checkpoint_seq_no => frames,
            _ => 0,
        };
        Ok((state.max_frame, checkpointed.min(state.max_frame)))
    }

    /// Records what a checkpoint copied back, for [`Connection::wal_frames`].
    /// A restarted WAL has a new checkpoint sequence and starts from zero.
    pub(crate) fn record_checkpoint(&self, checkpointed: u64) -> Result<()> {
        if self.database.engine().confined.is_none() {
            return Ok(());
        }
        let sequence = self.inner.wal_state()?.checkpoint_seq_no;
        *lock(&self.database.shared.checkpointed) = Some((sequence, checkpointed));
        Ok(())
    }

    pub(crate) fn capability(&self) -> Capability {
        self.capability.get()
    }

    pub(crate) fn clear_cache(&self) {
        self.cache.borrow_mut().clear();
    }

    pub(crate) fn push_guard(&self, guard: ExecutionGuard) {
        self.guard.push(guard);
    }

    pub(crate) fn pop_guard(&self) {
        self.guard.pop();
    }

    pub(crate) fn database(&self) -> &Database {
        &self.database
    }

    pub(crate) fn set_busy_timeout(&self, timeout: Duration) {
        self.inner.set_busy_timeout(timeout);
    }

    pub(crate) fn changes(&self) -> u64 {
        self.inner.total_changes().max(0).unsigned_abs()
    }

    /// Parses, authorizes under the current capability, and compiles one
    /// statement; `cached` reuses a statement compiled for the same
    /// `(capability, sql)`.
    pub(crate) fn prepare(&self, sql: &str, cached: bool) -> Result<Prepared> {
        let capability = self.capability();
        if cached
            && let Some((sql, hit)) = self
                .cache
                .borrow_mut()
                .get_mut(&capability)
                .and_then(|statements| statements.remove_entry(sql))
        {
            return Ok(Prepared {
                statement: hit.statement,
                sql,
                capability,
                ddl: false,
                write: hit.write,
                columns: hit.columns,
            });
        }
        validate_request(sql, &[])?;
        let command = parse_single(sql)?;
        self.authorize(&command, capability)?;
        let ddl = policy::is_ddl(&command);
        let write = policy::is_write(&command);
        let statement = self.compile(command, sql)?;
        let columns = (0..statement.num_columns())
            .map(|index| statement.get_column_name(index).into_owned())
            .collect();
        Ok(Prepared {
            statement,
            sql: sql.to_owned(),
            capability,
            ddl,
            write,
            columns,
        })
    }

    fn compile(&self, command: Cmd, sql: &str) -> Result<turso_core::Statement> {
        self.inner
            .prepare_translated_cmd(command, sql)
            .map_err(classify_prepare)
    }

    /// Returns a statement to the cache. Statements that changed the schema
    /// are dropped instead.
    pub(crate) fn recycle(&self, mut prepared: Prepared) {
        if prepared.ddl || prepared.statement.reset().is_err() {
            return;
        }
        prepared.statement.clear_bindings();
        let mut cache = self.cache.borrow_mut();
        if cache.values().map(HashMap::len).sum::<usize>() >= STATEMENT_CACHE_CAPACITY {
            cache.clear();
        }
        cache
            .entry(prepared.capability)
            .or_default()
            .entry(prepared.sql)
            .or_insert(Cached {
                statement: prepared.statement,
                write: prepared.write,
                columns: prepared.columns,
            });
    }

    /// Readmits a statement before it runs: a statement compiled under
    /// another capability is re-authorized, and the guards are checked. The
    /// file identity is checked only when the statement starts a transaction,
    /// since a `stat` per statement of a bulk transaction is measurable
    /// writer-thread time (#3491).
    pub(crate) fn admit(&self, prepared: &mut Prepared) -> Result<()> {
        let capability = self.capability();
        if prepared.capability != capability {
            self.authorize(&parse_single(&prepared.sql)?, capability)?;
            prepared.capability = capability;
        }
        prepared.statement.reset()?;
        self.guard.start(self.inner.get_auto_commit())
    }

    pub(crate) fn bind(&self, prepared: &mut Prepared, values: &[Value]) -> Result<()> {
        validate_request(&prepared.sql, values)?;
        let expected = prepared.statement.parameters_count();
        if expected != values.len() {
            return Err(Error::InvalidOperation(format!(
                "expected {expected} parameters, received {}",
                values.len()
            )));
        }
        for (index, value) in values.iter().enumerate() {
            let position = NonZeroUsize::new(index + 1)
                .ok_or_else(|| Error::InvalidOperation("parameter index overflow".to_owned()))?;
            prepared.statement.bind_at(position, value.to_native()?)?;
        }
        Ok(())
    }

    /// Steps until the next row (`true`) or completion (`false`).
    pub(crate) fn step(&self, prepared: &mut Prepared) -> Result<bool> {
        let profiled = crate::stmt_profile::enabled().then(Instant::now);
        let driven = self.drive(&mut prepared.statement);
        if let Some(started) = profiled {
            crate::stmt_profile::time(&prepared.sql, started.elapsed());
            if matches!(driven, Ok(false)) {
                crate::stmt_profile::finish(
                    &prepared.sql,
                    prepared
                        .statement
                        .stmt_status(turso_core::StatementStatusCounter::VmStep),
                );
            }
        }
        let row = match driven {
            Ok(row) => row,
            Err(error) => {
                return Err(self.end_interrupted_write(
                    &mut prepared.statement,
                    prepared.write,
                    error,
                ));
            }
        };
        if !row && prepared.ddl {
            self.clear_cache();
        }
        Ok(row)
    }

    /// SQLite rolls back the whole transaction when it interrupts an INSERT,
    /// UPDATE or DELETE inside one; the engine leaves it open, so the driver
    /// does it. Callers rely on an interrupted write taking its transaction
    /// with it.
    fn end_interrupted_write(
        &self,
        statement: &mut turso_core::Statement,
        write: bool,
        error: Error,
    ) -> Error {
        let interrupted = matches!(
            error,
            Error::Cancelled
                | Error::DeadlineExceeded
                | Error::Authority(_)
                | Error::OperationLimitExceeded
                | Error::Engine(LimboError::Interrupt)
        );
        if !write || !interrupted {
            return error;
        }
        // The engine refuses to roll back under a statement still in progress.
        let rolled_back = statement
            .reset()
            .map_err(Error::from)
            .and_then(|()| self.rollback_transaction());
        if let Err(rollback) = rolled_back {
            tracing::error!(%rollback, "rollback after an interrupted write failed");
        }
        error
    }

    fn drive(&self, statement: &mut turso_core::Statement) -> Result<bool> {
        loop {
            match statement.step() {
                Ok(StepResult::Row) => return Ok(true),
                Ok(StepResult::Done) => {
                    self.guard.take_tripped();
                    return Ok(false);
                }
                Ok(StepResult::IO | StepResult::Yield) => self.database.engine().io.step()?,
                // The engine's IO loop has nothing to pump while it waits on
                // another connection's lock: sleep instead of spinning.
                Ok(StepResult::Sleep { duration }) => self.sleep(duration)?,
                Ok(StepResult::Busy) => return Err(Error::Busy),
                Ok(StepResult::Interrupt) | Err(LimboError::Interrupt) => {
                    return Err(self
                        .guard
                        .take_tripped()
                        .unwrap_or(Error::Engine(LimboError::Interrupt)));
                }
                Err(error) => return Err(self.guard.take_tripped().unwrap_or(error.into())),
            }
        }
    }

    fn sleep(&self, duration: Duration) -> Result<()> {
        let until = Instant::now() + duration;
        loop {
            self.guard.check()?;
            let remaining = until.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            std::thread::sleep(remaining.min(SLEEP_SLICE));
        }
    }

    /// Runs a whole batch after authorizing every statement in it.
    pub(crate) fn run_batch(&self, sql: &str) -> Result<()> {
        validate_request(sql, &[])?;
        let statements = parse_batch(sql)?;
        let capability = self.capability();
        for (command, _) in &statements {
            self.authorize(command, capability)?;
        }
        for (command, input) in statements {
            let ddl = policy::is_ddl(&command);
            let write = policy::is_write(&command);
            let mut statement = self.compile(command, input)?;
            self.guard.start(true)?;
            let started = Instant::now();
            while self
                .drive(&mut statement)
                .map_err(|error| self.end_interrupted_write(&mut statement, write, error))?
            {
            }
            crate::stmt_profile::record(
                input,
                started.elapsed(),
                statement.stmt_status(turso_core::StatementStatusCounter::VmStep),
            );
            if ddl {
                self.clear_cache();
            }
        }
        Ok(())
    }

    /// Runs driver-issued SQL (transaction control, the format stamp) without
    /// the capability verdict.
    pub(crate) fn execute_unchecked(&self, sql: &str) -> Result<()> {
        let mut statement = self.compile(parse_single(sql)?, sql)?;
        self.guard.start(true)?;
        let started = Instant::now();
        while self.drive(&mut statement)? {}
        crate::stmt_profile::record(
            sql,
            started.elapsed(),
            statement.stmt_status(turso_core::StatementStatusCounter::VmStep),
        );
        Ok(())
    }

    /// Runs driver-issued SQL with every guard off: COMMIT and rollback must
    /// not be interrupted once they start.
    fn execute_suspended(&self, sql: &str) -> std::result::Result<(), Settlement> {
        let mut statement = self
            .compile(parse_single(sql).map_err(Settlement::Refused)?, sql)
            .map_err(Settlement::Refused)?;
        let previous = self.guard.set_suspended(true);
        let started = Instant::now();
        let result = loop {
            match self.drive(&mut statement) {
                Ok(true) => {}
                Ok(false) => break Ok(()),
                Err(error) => break Err(Settlement::Unknown(error)),
            }
        };
        self.guard.set_suspended(previous);
        crate::stmt_profile::record(sql, started.elapsed(), 0);
        result
    }

    fn pragma_u32(&self, name: &str) -> Result<u32> {
        let sql = format!("PRAGMA {name}");
        let mut statement = self.compile(parse_single(&sql)?, &sql)?;
        let value = if self.drive(&mut statement)? {
            statement.row().and_then(|row| match row.get_value(0) {
                turso_core::Value::Numeric(turso_core::Numeric::Integer(value)) => {
                    u32::try_from(*value).ok()
                }
                _ => None,
            })
        } else {
            None
        };
        value.ok_or_else(|| Error::InvalidOperation(format!("PRAGMA {name} returned no integer")))
    }

    pub(crate) fn begin_transaction(&self, behavior: TransactionBehavior) -> Result<()> {
        self.execute_unchecked(match behavior {
            TransactionBehavior::Deferred => "BEGIN DEFERRED",
            TransactionBehavior::Immediate => "BEGIN IMMEDIATE",
        })
    }

    /// COMMIT runs unguarded. A failure before it is stepped is a refusal;
    /// any failure after is commit-unknown, except a constraint: deferred
    /// foreign keys are checked before COMMIT writes anything, so that
    /// transaction is known not to be durable.
    pub(crate) fn commit_transaction(&self) -> std::result::Result<(), Settlement> {
        self.execute_suspended("COMMIT")
            .map_err(|settlement| match settlement {
                Settlement::Unknown(
                    error @ Error::Engine(
                        LimboError::Constraint(_) | LimboError::ForeignKeyConstraint(_),
                    ),
                ) => Settlement::Refused(error),
                settlement => settlement,
            })
    }

    /// Rollback ignores authority, cancellation and file identity: revocation
    /// can forbid a commit but never strand a transaction.
    pub(crate) fn rollback_transaction(&self) -> Result<()> {
        if self.inner.get_auto_commit() {
            return Ok(());
        }
        self.execute_suspended("ROLLBACK")
            .map_err(Settlement::into_error)
    }

    pub(crate) fn savepoint(&self, name: &str) -> Result<()> {
        self.execute_unchecked(&format!("SAVEPOINT \"{}\"", savepoint_name(name)?))
    }

    pub(crate) fn release_savepoint(&self, name: &str) -> Result<()> {
        self.execute_unchecked(&format!("RELEASE \"{}\"", savepoint_name(name)?))
    }

    pub(crate) fn rollback_to_savepoint(&self, name: &str) -> Result<()> {
        self.execute_suspended(&format!("ROLLBACK TO \"{}\"", savepoint_name(name)?))
            .map_err(Settlement::into_error)
    }

    /// Rolls back to and releases a savepoint, unguarded like rollback.
    pub(crate) fn abort_savepoint(&self, name: &str) -> Result<()> {
        self.rollback_to_savepoint(name)?;
        self.execute_suspended(&format!("RELEASE \"{}\"", savepoint_name(name)?))
            .map_err(Settlement::into_error)
    }

    pub fn is_autocommit(&self) -> bool {
        self.inner.get_auto_commit()
    }

    pub(crate) fn interrupt_handle(&self) -> InterruptHandle {
        InterruptHandle(Arc::clone(&self.inner))
    }

    // The facade the runtime-core `engine::native` module drives. Each call
    // runs under its own guard.

    pub fn execute(
        &mut self,
        sql: &str,
        parameters: &[Value],
        guard: &ExecutionGuard,
    ) -> Result<u64> {
        self.guarded(guard, |connection| {
            let mut prepared = connection.prepare(sql, false)?;
            connection.admit(&mut prepared)?;
            connection.bind(&mut prepared, parameters)?;
            while connection.step(&mut prepared)? {}
            u64::try_from(prepared.statement.n_change()).map_err(|_| {
                Error::InvalidOperation("negative native changed-row count".to_owned())
            })
        })
    }

    pub fn query(
        &mut self,
        sql: &str,
        parameters: &[Value],
        guard: &ExecutionGuard,
    ) -> Result<Rows> {
        self.guarded(guard, |connection| {
            let mut prepared = connection.prepare(sql, false)?;
            connection.admit(&mut prepared)?;
            connection.bind(&mut prepared, parameters)?;
            let mut rows = Rows {
                columns: (0..prepared.statement.num_columns())
                    .map(|index| prepared.statement.get_column_name(index).into_owned())
                    .collect(),
                values: Vec::new(),
            };
            let mut bytes = rows.columns.iter().map(String::len).sum::<usize>();
            while connection.step(&mut prepared)? {
                let row = prepared.statement.row().ok_or_else(|| {
                    Error::InvalidOperation("native row missing after row step".to_owned())
                })?;
                if rows.values.len() >= MAX_QUERY_ROWS {
                    return Err(Error::QueryLimitExceeded);
                }
                let values: Vec<Value> = row.get_values().map(Value::from_native).collect();
                bytes = values.iter().fold(bytes, |total, value| {
                    total.saturating_add(std::mem::size_of::<Value>() + value.payload_len())
                });
                if bytes > MAX_QUERY_BYTES {
                    return Err(Error::QueryLimitExceeded);
                }
                rows.values.push(values);
            }
            Ok(rows)
        })
    }

    pub fn validate(&mut self, sql: &str, guard: &ExecutionGuard) -> Result<()> {
        self.guarded(guard, |connection| {
            connection.prepare(sql, false)?;
            connection.guard.check()
        })
    }

    /// Validates the entire batch before applying its first statement. It
    /// joins the caller's transaction; it never opens or commits one.
    pub fn execute_batch(&mut self, sql: &str, guard: &ExecutionGuard) -> Result<()> {
        self.guarded(guard, |connection| connection.run_batch(sql))
    }

    pub fn begin(&mut self, behavior: TransactionBehavior, guard: &ExecutionGuard) -> Result<()> {
        if !self.is_autocommit() {
            return Err(Error::InvalidOperation(
                "transaction already active".to_owned(),
            ));
        }
        if self.capability() == Capability::Reader && behavior != TransactionBehavior::Deferred {
            return Err(Error::Denied(
                "reader cannot begin a write transaction".to_owned(),
            ));
        }
        self.guarded(guard, |connection| connection.begin_transaction(behavior))
    }

    /// Checks the guard once, then commits unguarded.
    pub fn commit(&mut self, guard: &ExecutionGuard) -> Result<()> {
        if self.is_autocommit() {
            return Err(Error::InvalidOperation(
                "no transaction to commit".to_owned(),
            ));
        }
        guard.verify()?;
        self.commit_transaction().map_err(Settlement::into_error)
    }

    pub fn rollback(&mut self) -> Result<()> {
        self.rollback_transaction()
    }

    pub fn checkpoint(&mut self, guard: &ExecutionGuard) -> Result<Rows> {
        self.query("PRAGMA wal_checkpoint(TRUNCATE)", &[], guard)
    }

    fn guarded<T>(
        &self,
        guard: &ExecutionGuard,
        run: impl FnOnce(&Self) -> Result<T>,
    ) -> Result<T> {
        guard.verify()?;
        self.push_guard(guard.clone());
        let result = run(self);
        self.pop_guard();
        result
    }
}

/// How a statement that ends a transaction failed.
pub(crate) enum Settlement {
    /// Nothing was stepped; the transaction is unchanged.
    Refused(Error),
    /// The statement was stepped and failed; durability is unknown.
    Unknown(Error),
}

impl Settlement {
    pub(crate) fn into_error(self) -> Error {
        match self {
            Self::Refused(error) | Self::Unknown(error) => error,
        }
    }
}

fn savepoint_name(name: &str) -> Result<&str> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(Error::InvalidOperation(
            "invalid runtime savepoint identifier".to_owned(),
        ));
    }
    Ok(name)
}

/// Function denials surface from the engine's name resolution as parse errors.
fn classify_prepare(error: LimboError) -> Error {
    match error {
        LimboError::ParseError(message) if message.contains(policy::DENIED_FUNCTION) => {
            Error::Denied(message)
        }
        error => error.into(),
    }
}

/// Interrupts the connection's running statement from any thread.
#[derive(Clone)]
pub struct InterruptHandle(Arc<turso_core::Connection>);

impl InterruptHandle {
    pub fn interrupt(&self) {
        self.0.interrupt();
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.cache.get_mut().clear();
        if let Err(error) = self.rollback_transaction() {
            tracing::error!(%error, "native transaction cleanup failed; releasing the connection rolls back remaining state");
        }
    }
}

fn parse_single(sql: &str) -> Result<Cmd> {
    let mut statements = parse_batch(sql)?;
    if statements.len() != 1 {
        return Err(Error::InvalidOperation(
            "exactly one SQL statement is required".to_owned(),
        ));
    }
    Ok(statements.remove(0).0)
}

fn parse_batch(sql: &str) -> Result<Vec<(Cmd, &str)>> {
    let dialect = policy::dialect();
    let mut remaining = sql;
    let mut statements = Vec::new();
    while !remaining.is_empty() {
        let (command, consumed) = dialect.parse(remaining)?;
        if consumed == 0 {
            if command.is_none() {
                break;
            }
            return Err(Error::InvalidOperation(
                "native parser made no progress".to_owned(),
            ));
        }
        let input = remaining
            .get(..consumed)
            .ok_or_else(|| Error::InvalidOperation("invalid native SQL boundary".to_owned()))?;
        if let Some(command) = command {
            statements.push((command, input));
        }
        remaining = remaining
            .get(consumed..)
            .ok_or_else(|| Error::InvalidOperation("invalid native SQL tail".to_owned()))?;
    }
    if statements.is_empty() {
        return Err(Error::InvalidOperation("SQL statement is empty".to_owned()));
    }
    Ok(statements)
}

fn validate_request(sql: &str, parameters: &[Value]) -> Result<()> {
    if sql.len() > MAX_SQL_BYTES || parameters.len() > MAX_SQL_PARAMETERS {
        return Err(Error::RequestLimitExceeded);
    }
    let bytes = parameters.iter().fold(sql.len(), |total, value| {
        total.saturating_add(std::mem::size_of::<Value>() + value.payload_len())
    });
    if bytes > MAX_REQUEST_BYTES {
        return Err(Error::RequestLimitExceeded);
    }
    Ok(())
}
