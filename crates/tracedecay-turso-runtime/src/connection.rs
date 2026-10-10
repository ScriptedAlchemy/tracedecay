use std::{
    num::NonZeroUsize,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use turso_core::{DatabaseOpts, OpenOptions, StepResult};
use turso_parser::ast::Cmd;

use crate::{Error, Result, Rows, Value, policy};

const MAX_QUERY_ROWS: usize = 10_000;
const MAX_QUERY_BYTES: usize = 64 * 1024 * 1024;
const MAX_SQL_BYTES: usize = 1024 * 1024;
const MAX_SQL_PARAMETERS: usize = 32_766;
const MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Reader,
    Writer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransactionBehavior {
    Deferred,
    Immediate,
    Concurrent,
}

pub type Authority = Arc<dyn Fn() -> std::result::Result<(), String> + Send + Sync>;

#[derive(Clone)]
pub struct ExecutionGuard {
    pub deadline: Option<Instant>,
    pub cancelled: Arc<AtomicBool>,
    pub authority: Option<Authority>,
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
        }
    }

    pub fn verify(&self) -> Result<()> {
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

#[derive(Clone)]
pub struct Database {
    inner: Arc<turso_core::Database>,
    io: Arc<dyn turso_core::IO>,
    #[cfg(unix)]
    pinned: Option<Arc<crate::pinned_io::PinnedIo>>,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        let path = path.to_str().ok_or_else(|| {
            Error::InvalidOperation("native database path is not UTF-8".to_owned())
        })?;
        let io = turso_core::Database::io_for_path(path)?;
        let options = OpenOptions::new(policy::dialect()).db_opts(
            DatabaseOpts::new()
                .with_index_method(true)
                .with_generated_columns(true),
        );
        let inner = turso_core::Database::open(Arc::clone(&io), path, options)?;
        Ok(Self {
            inner,
            io,
            #[cfg(unix)]
            pinned: None,
        })
    }

    /// Adopts a read-write descriptor supplied by the canonical file authority.
    /// The native main-file handle opens that descriptor, never the mutable path.
    #[cfg(unix)]
    pub fn open_pinned(path: &Path, file: std::fs::File) -> Result<Self> {
        let pinned = crate::pinned_io::PinnedIo::new(path, file)?;
        let native_file = pinned.open_main()?;
        let path = pinned.canonical_path().to_str().ok_or_else(|| {
            Error::InvalidOperation("native database path is not UTF-8".to_owned())
        })?;
        let io: Arc<dyn turso_core::IO> = pinned.clone();
        let storage = Arc::new(turso_core::storage::database::DatabaseFile::new(
            native_file,
        ));
        let options = OpenOptions::new(policy::pinned_dialect())
            .storage(storage)
            .db_opts(
                DatabaseOpts::new()
                    .with_index_method(true)
                    .with_generated_columns(true),
            );
        let inner = turso_core::Database::open(Arc::clone(&io), path, options)?;
        pinned.verify_path()?;
        Ok(Self {
            inner,
            io,
            pinned: Some(pinned),
        })
    }

    #[cfg(not(unix))]
    pub fn open_pinned(_path: &Path, _file: std::fs::File) -> Result<Self> {
        Err(Error::Unsupported(
            "native descriptor adoption is unavailable on this platform".to_owned(),
        ))
    }

    pub fn opened_file_identity(&self) -> Option<(u64, u64)> {
        #[cfg(unix)]
        {
            self.pinned.as_ref().map(|pinned| {
                let identity = pinned.identity();
                (identity.dev, identity.ino)
            })
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    pub fn connect(&self, access: Access) -> Result<Connection> {
        #[cfg(unix)]
        if let Some(pinned) = &self.pinned {
            pinned.verify_path()?;
        }
        let inner = self.inner.connect()?;
        inner.set_foreign_keys_enabled(true);
        inner.set_sync_mode(turso_core::SyncMode::Normal);
        inner.set_query_only(access == Access::Reader);
        inner.set_busy_timeout(Duration::from_secs(5));
        Ok(Connection {
            inner,
            io: Arc::clone(&self.io),
            access,
            transaction_active: false,
            #[cfg(unix)]
            pinned: self.pinned.clone(),
        })
    }
}

/// Exclusively owned by one writer actor or reader lease. Never exposes the
/// underlying connection, compiled program, or an unguarded SQL entrypoint.
pub struct Connection {
    inner: Arc<turso_core::Connection>,
    io: Arc<dyn turso_core::IO>,
    access: Access,
    transaction_active: bool,
    #[cfg(unix)]
    pinned: Option<Arc<crate::pinned_io::PinnedIo>>,
}

impl Connection {
    pub fn execute(
        &mut self,
        sql: &str,
        parameters: &[Value],
        guard: &ExecutionGuard,
    ) -> Result<u64> {
        let (_, changes) = self.run(sql, parameters, guard, false)?;
        Ok(changes)
    }

    pub fn query(
        &mut self,
        sql: &str,
        parameters: &[Value],
        guard: &ExecutionGuard,
    ) -> Result<Rows> {
        self.run(sql, parameters, guard, true).map(|(rows, _)| rows)
    }

    pub fn validate(&mut self, sql: &str, guard: &ExecutionGuard) -> Result<()> {
        guard.verify()?;
        #[cfg(unix)]
        if let Some(pinned) = &self.pinned {
            pinned.verify_path()?;
        }
        let command = parse_single(sql)?;
        policy::authorize(&command, self.access)?;
        let _statement = self.inner.prepare_translated_cmd(command, sql)?;
        guard.verify()
    }

    /// Validates the entire batch before applying its first statement. It joins
    /// the actor-owned transaction; it never opens or commits one implicitly.
    pub fn execute_batch(&mut self, sql: &str, guard: &ExecutionGuard) -> Result<()> {
        guard.verify()?;
        let statements = parse_batch(sql)?;
        for (command, _) in &statements {
            policy::authorize(command, self.access)?;
        }
        for (command, input) in statements {
            self.run_command(command, input, &[], guard, false)?;
        }
        Ok(())
    }

    pub fn begin(&mut self, behavior: TransactionBehavior, guard: &ExecutionGuard) -> Result<()> {
        if self.transaction_active {
            return Err(Error::InvalidOperation(
                "transaction already active".to_owned(),
            ));
        }
        if self.access == Access::Reader && behavior != TransactionBehavior::Deferred {
            return Err(Error::Denied(
                "reader cannot begin a write transaction".to_owned(),
            ));
        }
        let sql = match behavior {
            TransactionBehavior::Deferred => "BEGIN DEFERRED",
            TransactionBehavior::Immediate => "BEGIN IMMEDIATE",
            TransactionBehavior::Concurrent => "BEGIN CONCURRENT",
        };
        self.control(sql, guard)?;
        self.transaction_active = true;
        Ok(())
    }

    pub fn commit(&mut self, guard: &ExecutionGuard) -> Result<()> {
        if !self.transaction_active {
            return Err(Error::InvalidOperation(
                "no transaction to commit".to_owned(),
            ));
        }
        self.control("COMMIT", guard)?;
        self.transaction_active = false;
        Ok(())
    }

    /// Cleanup remains available after cancellation or authority revocation.
    /// Revocation can forbid a commit, but must never prevent rollback.
    pub fn rollback(&mut self) -> Result<()> {
        if !self.transaction_active {
            return Ok(());
        }
        let guard = cleanup_guard();
        self.control("ROLLBACK", &guard)?;
        self.transaction_active = false;
        Ok(())
    }

    pub fn savepoint(&mut self, name: &str, guard: &ExecutionGuard) -> Result<()> {
        self.savepoint_control("SAVEPOINT", name, guard)
    }
    pub fn release_savepoint(&mut self, name: &str, guard: &ExecutionGuard) -> Result<()> {
        self.savepoint_control("RELEASE", name, guard)
    }
    pub fn rollback_savepoint(&mut self, name: &str) -> Result<()> {
        self.savepoint_control("ROLLBACK TO", name, &cleanup_guard())
    }
    pub fn abort_savepoint(&mut self, name: &str) -> Result<()> {
        let guard = cleanup_guard();
        self.savepoint_control("ROLLBACK TO", name, &guard)?;
        self.savepoint_control("RELEASE", name, &guard)
    }

    pub fn is_autocommit(&self) -> bool {
        self.inner.get_auto_commit()
    }
    pub fn interrupt(&self) {
        self.inner.interrupt();
    }

    /// The closed writer selects full synchronization before admitting a
    /// canonical-state batch. Caller SQL cannot change synchronous mode.
    pub fn set_full_durability(&mut self, guard: &ExecutionGuard) -> Result<()> {
        guard.verify()?;
        if self.access != Access::Writer {
            return Err(Error::Denied("reader cannot change durability".to_owned()));
        }
        if !self.inner.get_auto_commit() {
            return Err(Error::InvalidOperation(
                "durability cannot change inside a transaction".to_owned(),
            ));
        }
        #[cfg(unix)]
        if let Some(pinned) = &self.pinned {
            pinned.verify_path()?;
        }
        self.inner.set_sync_mode(turso_core::SyncMode::Full);
        if self.inner.get_sync_mode() != turso_core::SyncMode::Full {
            return Err(Error::InvalidOperation(
                "native full synchronization did not apply".to_owned(),
            ));
        }
        Ok(())
    }

    /// Explicitly opts this isolated native database into overlapping
    /// transactions. It changes the durable journal mode; ordinary opens retain
    /// WAL and caller SQL cannot acquire this configuration capability.
    pub fn enable_concurrent_transactions(&mut self, guard: &ExecutionGuard) -> Result<()> {
        guard.verify()?;
        if self.access != Access::Writer {
            return Err(Error::Denied(
                "reader cannot configure concurrent storage".to_owned(),
            ));
        }
        if self.transaction_active {
            return Err(Error::InvalidOperation(
                "configure concurrency within transaction".to_owned(),
            ));
        }
        let sql = "PRAGMA journal_mode=mvcc";
        let (rows, _) = self.run_command(parse_single(sql)?, sql, &[], guard, true)?;
        if rows.values != vec![vec![Value::Text("mvcc".to_owned())]] {
            return Err(Error::Unsupported(
                "native MVCC journal mode did not apply".to_owned(),
            ));
        }
        Ok(())
    }

    pub fn checkpoint(&mut self, guard: &ExecutionGuard) -> Result<Rows> {
        if self.access != Access::Writer {
            return Err(Error::Denied("reader cannot checkpoint storage".to_owned()));
        }
        if self.transaction_active {
            return Err(Error::InvalidOperation(
                "checkpoint within transaction".to_owned(),
            ));
        }
        self.run_command(
            parse_single("PRAGMA wal_checkpoint(TRUNCATE)")?,
            "PRAGMA wal_checkpoint(TRUNCATE)",
            &[],
            guard,
            true,
        )
        .map(|(rows, _)| rows)
    }

    fn savepoint_control(
        &mut self,
        prefix: &str,
        name: &str,
        guard: &ExecutionGuard,
    ) -> Result<()> {
        if !self.transaction_active {
            return Err(Error::InvalidOperation(
                "savepoint requires actor-owned transaction".to_owned(),
            ));
        }
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(Error::InvalidOperation(
                "invalid runtime savepoint identifier".to_owned(),
            ));
        }
        self.control(&format!("{prefix} \"{name}\""), guard)
    }

    fn control(&mut self, sql: &str, guard: &ExecutionGuard) -> Result<()> {
        self.run_command(parse_single(sql)?, sql, &[], guard, false)
            .map(|_| ())
    }

    fn run(
        &mut self,
        sql: &str,
        parameters: &[Value],
        guard: &ExecutionGuard,
        collect: bool,
    ) -> Result<(Rows, u64)> {
        guard.verify()?;
        let command = parse_single(sql)?;
        policy::authorize(&command, self.access)?;
        self.run_command(command, sql, parameters, guard, collect)
    }

    fn run_command(
        &mut self,
        command: Cmd,
        sql: &str,
        parameters: &[Value],
        guard: &ExecutionGuard,
        collect: bool,
    ) -> Result<(Rows, u64)> {
        let result = self.run_command_inner(command, sql, parameters, guard, collect);
        // Native constraint/cancellation paths can end a transaction themselves.
        // Never retain a facade transaction flag after the engine rolled it back.
        self.transaction_active = !self.inner.get_auto_commit();
        result
    }

    fn run_command_inner(
        &mut self,
        command: Cmd,
        sql: &str,
        parameters: &[Value],
        guard: &ExecutionGuard,
        collect: bool,
    ) -> Result<(Rows, u64)> {
        guard.verify()?;
        validate_request(sql, parameters)?;
        #[cfg(unix)]
        if let Some(pinned) = &self.pinned {
            pinned.verify_path()?;
        }
        let progress_guard = ProgressGuard::install(
            Arc::clone(&self.inner),
            guard.clone(),
            #[cfg(unix)]
            self.pinned.clone(),
        );
        let mut statement = self.inner.prepare_translated_cmd(command, sql)?;
        if statement.parameters_count() != parameters.len() {
            return Err(Error::InvalidOperation(format!(
                "expected {} parameters, received {}",
                statement.parameters_count(),
                parameters.len()
            )));
        }
        for (index, value) in parameters.iter().enumerate() {
            let position = NonZeroUsize::new(index + 1)
                .ok_or_else(|| Error::InvalidOperation("parameter index overflow".to_owned()))?;
            statement.bind_at(position, value.to_native()?)?;
        }
        let mut rows = Rows {
            columns: (0..statement.num_columns())
                .map(|index| statement.get_column_name(index).into_owned())
                .collect(),
            values: Vec::new(),
        };
        let mut materialized_bytes = rows
            .columns
            .iter()
            .try_fold(std::mem::size_of::<Vec<String>>(), |total, column| {
                total
                    .checked_add(std::mem::size_of::<String>())?
                    .checked_add(column.len())
            })
            .ok_or(Error::QueryLimitExceeded)?;
        loop {
            guard.verify()?;
            let step = match statement.step() {
                Ok(step) => step,
                Err(error) => {
                    if let Some(error) = progress_guard.failure()? {
                        return Err(error);
                    }
                    return Err(error.into());
                }
            };
            match step {
                StepResult::Row => {
                    if collect {
                        let row = statement.row().ok_or_else(|| {
                            Error::InvalidOperation("native row missing after row step".to_owned())
                        })?;
                        if rows.values.len() >= MAX_QUERY_ROWS {
                            return Err(Error::QueryLimitExceeded);
                        }
                        materialized_bytes = materialized_bytes
                            .checked_add(std::mem::size_of::<Vec<Value>>())
                            .ok_or(Error::QueryLimitExceeded)?;
                        let mut values = Vec::with_capacity(rows.columns.len());
                        for value in row.get_values() {
                            let payload = match value {
                                turso_core::Value::Text(value) => value.as_str().len(),
                                turso_core::Value::Blob(value) => value.len(),
                                _ => 0,
                            };
                            materialized_bytes = materialized_bytes
                                .checked_add(std::mem::size_of::<Value>())
                                .and_then(|bytes| bytes.checked_add(payload))
                                .ok_or(Error::QueryLimitExceeded)?;
                            if materialized_bytes > MAX_QUERY_BYTES {
                                return Err(Error::QueryLimitExceeded);
                            }
                            values.push(Value::from_native(value));
                        }
                        rows.values.push(values);
                    }
                }
                StepResult::Done => {
                    if let Some(error) = progress_guard.failure()? {
                        return Err(error);
                    }
                    let changes = u64::try_from(statement.n_change()).map_err(|_| {
                        Error::InvalidOperation("negative native changed-row count".to_owned())
                    })?;
                    return Ok((rows, changes));
                }
                StepResult::IO | StepResult::Yield | StepResult::Sleep { .. } => self.io.step()?,
                StepResult::Busy => return Err(Error::Busy),
                StepResult::Interrupt => {
                    return Err(progress_guard.failure()?.unwrap_or(Error::Cancelled));
                }
            }
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // The engine rolls back unfinished transactions when its connection is
        // released. Attempt explicit bounded cleanup first; neither cancellation
        // nor write-authority revocation is allowed to strand an owned transaction.
        if let Err(error) = self.rollback() {
            tracing::error!(%error, "native transaction cleanup failed; releasing connection rolls back remaining state");
        }
    }
}

fn cleanup_guard() -> ExecutionGuard {
    ExecutionGuard::new(
        Some(Instant::now() + Duration::from_secs(5)),
        Arc::new(AtomicBool::new(false)),
        None,
    )
}

struct ProgressGuard {
    connection: Arc<turso_core::Connection>,
    failure: Arc<Mutex<Option<Error>>>,
}
impl ProgressGuard {
    fn install(
        connection: Arc<turso_core::Connection>,
        guard: ExecutionGuard,
        #[cfg(unix)] pinned: Option<Arc<crate::pinned_io::PinnedIo>>,
    ) -> Self {
        let failure = Arc::new(Mutex::new(None));
        let callback_failure = Arc::clone(&failure);
        connection.set_progress_handler(
            1_000,
            Some(Box::new(move || {
                let result = guard.verify().and_then(|()| {
                    #[cfg(unix)]
                    if let Some(pinned) = &pinned {
                        pinned.verify_path()?;
                    }
                    Ok(())
                });
                if let Err(error) = result {
                    match callback_failure.lock() {
                        Ok(mut failure) => *failure = Some(error),
                        Err(_) => return true,
                    }
                    return true;
                }
                false
            })),
        );
        Self {
            connection,
            failure,
        }
    }
    fn failure(&self) -> Result<Option<Error>> {
        self.failure
            .lock()
            .map(|mut failure| failure.take())
            .map_err(|_| Error::InvalidOperation("native progress state poisoned".to_owned()))
    }
}
impl Drop for ProgressGuard {
    fn drop(&mut self) {
        self.connection.set_progress_handler(0, None);
    }
}

fn parse_single(sql: &str) -> Result<Cmd> {
    let mut statements = parse_batch(sql)?;
    if statements.len() != 1 {
        return Err(Error::InvalidOperation(
            "exact SQL requires one statement".to_owned(),
        ));
    }
    Ok(statements.remove(0).0)
}

fn parse_batch(sql: &str) -> Result<Vec<(Cmd, &str)>> {
    validate_request(sql, &[])?;
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
    let bytes = parameters
        .iter()
        .try_fold(sql.len(), |total, value| {
            let payload = match value {
                Value::Text(value) => value.capacity(),
                Value::Blob(value) => value.capacity(),
                _ => 0,
            };
            total
                .checked_add(std::mem::size_of::<Value>())?
                .checked_add(payload)
        })
        .ok_or(Error::RequestLimitExceeded)?;
    if bytes > MAX_REQUEST_BYTES {
        return Err(Error::RequestLimitExceeded);
    }
    Ok(())
}
