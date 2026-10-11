//! The SQLite arm: rusqlite's types plus the control surface as extension
//! traits. The flip unit deletes this module together with the engine flag.

use std::{
    ffi::CString,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub use rusqlite::{
    CachedStatement, Connection, DropBehavior, Error, InterruptHandle, MappedRows, OpenFlags,
    OptionalExtension, Params, Result, Row, Rows, Savepoint, Statement, ToSql, Transaction,
    TransactionBehavior, params, params_from_iter,
};
use rusqlite::{
    ErrorCode, StatementStatus,
    config::DbConfig,
    hooks::{AuthAction, AuthContext, Authorization},
    limits::Limit,
};

use crate::{
    Capability, CheckpointMode, CheckpointOutcome, ConnectionControl, Engine, EnginePolicy,
    ErrorExt, ErrorKind, PROGRESS_INTERVAL_OPS, PageStats, StatementControl, StatementCounters,
    TransactionControl,
};

pub const ENGINE: Engine = Engine::Sqlite;

pub mod types {
    pub use rusqlite::types::{
        FromSql, FromSqlError, FromSqlResult, ToSqlOutput, Type, Value, ValueRef,
    };
}

/// The message on the error a capability scope reports when it denied
/// transaction control; [`ErrorExt::kind`] keys on it.
const TRANSACTION_CONTROL_DENIED: &str = "transaction control is denied in this capability scope";

/// Writer and reader connections are single-threaded actors, so rusqlite's
/// per-connection statement cache is the reuse path. Sixteen (the rusqlite
/// default) is too small for the distinct statements a store issues; 128
/// covers the repeated catalog without holding an unbounded compile cache.
const PREPARED_STATEMENT_CACHE_CAPACITY: usize = 128;

impl ErrorExt for Error {
    fn kind(&self) -> ErrorKind {
        if matches!(self, Error::QueryReturnedNoRows) {
            return ErrorKind::NoRows;
        }
        if matches!(self, Error::SqliteFailure(failure, Some(message))
            if failure.code == ErrorCode::AuthorizationForStatementDenied
                && message == TRANSACTION_CONTROL_DENIED)
        {
            return ErrorKind::TransactionControlDenied;
        }
        match self.sqlite_error_code() {
            Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => ErrorKind::Busy,
            Some(ErrorCode::ConstraintViolation) => ErrorKind::Constraint,
            Some(ErrorCode::OperationInterrupted) => ErrorKind::Interrupted,
            Some(ErrorCode::ReadOnly) => ErrorKind::ReadOnly,
            Some(ErrorCode::SchemaChanged) => ErrorKind::Schema,
            Some(ErrorCode::AuthorizationForStatementDenied) => ErrorKind::Denied,
            _ => ErrorKind::Other,
        }
    }

    fn sqlite_codes(&self) -> Option<(i32, i32)> {
        self.sqlite_error()
            .map(|failure| (failure.extended_code & 0xff, failure.extended_code))
    }
}

impl ConnectionControl for Connection {
    fn set_capability(&self, capability: Capability) -> Result<()> {
        install_authorizer(self, capability, None)
    }

    // No statement-cache flush: `sqlite3_set_authorizer` with a hook expires
    // every prepared statement, so a cached statement is authorized again
    // under the restored capability before it next runs.
    fn with_capability<T>(
        &self,
        scoped: Capability,
        base: Capability,
        operation: impl FnOnce(&Self) -> Result<T>,
    ) -> Result<T> {
        let transaction_control_denied = Arc::new(AtomicBool::new(false));
        install_authorizer(self, scoped, Some(Arc::clone(&transaction_control_denied)))?;
        let result = catch_unwind(AssertUnwindSafe(|| operation(self)));
        let restore = self.set_capability(base);
        let result = match result {
            Ok(result) => result,
            Err(payload) => resume_unwind(payload),
        };
        restore?;
        if transaction_control_denied.load(Ordering::Acquire) {
            return Err(Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_AUTH),
                Some(TRANSACTION_CONTROL_DENIED.to_owned()),
            ));
        }
        result
    }

    fn guard_scope<T>(
        &self,
        should_interrupt: impl FnMut() -> bool + Send + 'static,
        operation: impl FnOnce(&Self) -> T,
    ) -> Result<T> {
        self.progress_handler(PROGRESS_INTERVAL_OPS, Some(should_interrupt))?;
        let result = catch_unwind(AssertUnwindSafe(|| operation(self)));
        let clear = self.progress_handler(PROGRESS_INTERVAL_OPS, None::<fn() -> bool>);
        match result {
            Ok(value) => clear.map(|()| value),
            Err(payload) => resume_unwind(payload),
        }
    }

    fn checkpoint(&self, mode: CheckpointMode) -> Result<CheckpointOutcome> {
        let mode = match mode {
            CheckpointMode::Noop => "NOOP",
            CheckpointMode::Passive => "PASSIVE",
            CheckpointMode::Full => "FULL",
            CheckpointMode::Restart => "RESTART",
            CheckpointMode::Truncate => "TRUNCATE",
        };
        self.query_row(&format!("PRAGMA wal_checkpoint({mode})"), [], |row| {
            Ok(CheckpointOutcome {
                busy: row.get::<_, i64>(0)? != 0,
                log_frames: row.get(1)?,
                checkpointed_frames: row.get(2)?,
            })
        })
    }

    fn page_stats(&self) -> Result<PageStats> {
        let pragma = |name: &str| {
            self.pragma_query_value(None, name, |row| row.get::<_, i64>(0))
                .map(i64::unsigned_abs)
        };
        Ok(PageStats {
            page_size: pragma("page_size")?,
            page_count: pragma("page_count")?,
            freelist_count: pragma("freelist_count")?,
        })
    }

    fn file_has_moved(&self) -> Result<bool> {
        let main = CString::new("main").expect("static database name has no NUL");
        let mut moved = 0_i32;
        // SAFETY: `self` owns a live SQLite handle, `main` is NUL-terminated,
        // and `moved` is the writable int SQLITE_FCNTL_HAS_MOVED requires.
        let code = unsafe {
            rusqlite::ffi::sqlite3_file_control(
                self.handle(),
                main.as_ptr(),
                rusqlite::ffi::SQLITE_FCNTL_HAS_MOVED,
                (&raw mut moved).cast(),
            )
        };
        match code {
            rusqlite::ffi::SQLITE_OK => Ok(moved != 0),
            // A VFS that predates HAS_MOVED cannot answer; the caller's own
            // identity check stays authoritative.
            rusqlite::ffi::SQLITE_NOTFOUND => Ok(false),
            code => Err(Error::SqliteFailure(rusqlite::ffi::Error::new(code), None)),
        }
    }
}

impl TransactionControl for Transaction<'_> {
    fn guard_scope_mut<T>(
        &mut self,
        should_interrupt: impl FnMut() -> bool + Send + 'static,
        operation: impl FnOnce(&mut Self) -> T,
    ) -> Result<T> {
        self.progress_handler(PROGRESS_INTERVAL_OPS, Some(should_interrupt))?;
        let result = catch_unwind(AssertUnwindSafe(|| operation(self)));
        let clear = self.progress_handler(PROGRESS_INTERVAL_OPS, None::<fn() -> bool>);
        match result {
            Ok(value) => clear.map(|()| value),
            Err(payload) => resume_unwind(payload),
        }
    }
}

impl EnginePolicy for Connection {
    fn apply_engine_policy(&self) -> Result<()> {
        // SQLite must never wait past the runtime's own queue/deadline authority.
        self.busy_timeout(Duration::ZERO)?;
        self.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
        self.pragma_update(None, "trusted_schema", false)?;
        if !self.db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE)? {
            return Err(policy_violation(
                "SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE=false, expected true",
            ));
        }
        let trusted_schema: i64 =
            self.pragma_query_value(None, "trusted_schema", |row| row.get(0))?;
        if trusted_schema != 0 {
            return Err(policy_violation(format!(
                "trusted_schema={trusted_schema}, expected 0"
            )));
        }
        assert_compile_options(self)?;
        for (limit, value) in [
            (Limit::SQLITE_LIMIT_LENGTH, 64 * 1024 * 1024),
            (Limit::SQLITE_LIMIT_SQL_LENGTH, 1024 * 1024),
            (Limit::SQLITE_LIMIT_COLUMN, 2_000),
            (Limit::SQLITE_LIMIT_EXPR_DEPTH, 100),
            (Limit::SQLITE_LIMIT_COMPOUND_SELECT, 100),
            (Limit::SQLITE_LIMIT_VDBE_OP, 25_000_000),
            (Limit::SQLITE_LIMIT_FUNCTION_ARG, 100),
            (Limit::SQLITE_LIMIT_ATTACHED, 0),
            (Limit::SQLITE_LIMIT_LIKE_PATTERN_LENGTH, 50_000),
            (Limit::SQLITE_LIMIT_VARIABLE_NUMBER, 32_766),
            (Limit::SQLITE_LIMIT_TRIGGER_DEPTH, 32),
            (Limit::SQLITE_LIMIT_WORKER_THREADS, 0),
        ] {
            self.set_limit(limit, value)?;
        }
        self.set_prepared_statement_cache_capacity(PREPARED_STATEMENT_CACHE_CAPACITY);
        Ok(())
    }
}

impl StatementControl for Statement<'_> {
    fn take_counters(&self) -> StatementCounters {
        let take = |status| u64::try_from(self.reset_status(status).max(0)).unwrap_or(u64::MAX);
        StatementCounters {
            vm_steps: take(StatementStatus::VmStep),
            fullscan_steps: take(StatementStatus::FullscanStep),
            sort_steps: take(StatementStatus::Sort),
        }
    }
}

fn policy_violation(message: impl Into<String>) -> Error {
    Error::InvalidParameterName(message.into())
}

fn assert_compile_options(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare("PRAGMA compile_options")?;
    let options = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>>>()?;
    for required in ["ENABLE_FTS5", "THREADSAFE=1"] {
        if !options.iter().any(|option| option == required) {
            return Err(policy_violation(format!("missing {required}")));
        }
    }
    if options.iter().any(|option| option == "OMIT_FOREIGN_KEY") {
        return Err(policy_violation("OMIT_FOREIGN_KEY is unsupported"));
    }
    Ok(())
}

fn install_authorizer(
    connection: &Connection,
    capability: Capability,
    transaction_control_denied: Option<Arc<AtomicBool>>,
) -> Result<()> {
    connection.authorizer(Some(move |context: AuthContext<'_>| {
        if denies_transaction_control(capability, &context.action) {
            if let Some(denied) = &transaction_control_denied {
                denied.store(true, Ordering::Release);
            }
            return Authorization::Deny;
        }
        if authorize(capability, &context.action) {
            Authorization::Allow
        } else {
            Authorization::Deny
        }
    }))
}

fn denies_transaction_control(capability: Capability, action: &AuthAction<'_>) -> bool {
    match capability {
        Capability::ExactSqlRead | Capability::ExactSql { savepoints: false } => matches!(
            action,
            AuthAction::Transaction { .. } | AuthAction::Savepoint { .. }
        ),
        Capability::ExactSql { savepoints: true } => {
            matches!(action, AuthAction::Transaction { .. })
        }
        Capability::Reader | Capability::Writer | Capability::Maintenance => false,
    }
}

/// Schema-introspection and integrity pragmas that cannot mutate the database,
/// file, or connection configuration. These stay available to read-only lanes
/// so health and shape audits work without a writable connection.
fn is_read_only_introspection_pragma(name: &str) -> bool {
    [
        "collation_list",
        "database_list",
        "foreign_key_check",
        "foreign_key_list",
        "function_list",
        "index_info",
        "index_list",
        "index_xinfo",
        "integrity_check",
        "module_list",
        "pragma_list",
        "quick_check",
        "table_info",
        "table_list",
        "table_xinfo",
    ]
    .iter()
    .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

fn is_safe_writer_pragma(name: &str, value: &str) -> bool {
    [
        "busy_timeout",
        "incremental_vacuum",
        "wal_autocheckpoint",
        "wal_checkpoint",
    ]
    .iter()
    .any(|candidate| name.eq_ignore_ascii_case(candidate))
        || (name.eq_ignore_ascii_case("auto_vacuum")
            && (value.eq_ignore_ascii_case("incremental") || value == "2"))
}

fn is_load_extension(action: &AuthAction<'_>) -> bool {
    matches!(action, AuthAction::Function { function_name }
        if function_name.eq_ignore_ascii_case("load_extension"))
}

/// Whether `capability` allows `action`. Transaction control is decided by
/// [`denies_transaction_control`] first.
fn authorize(capability: Capability, action: &AuthAction<'_>) -> bool {
    match capability {
        // ATTACH is gone end to end; a parameter-bound filename reaches the
        // hook as an unknown ATTACH action.
        Capability::Maintenance => !matches!(
            action,
            AuthAction::Attach { .. }
                | AuthAction::Detach { .. }
                | AuthAction::Unknown {
                    code: rusqlite::ffi::SQLITE_ATTACH,
                    ..
                }
        ),
        Capability::ExactSql { .. } => authorize_exact_sql_writer(action),
        Capability::ExactSqlRead => authorize_canonical(Capability::Reader, action),
        Capability::Reader | Capability::Writer => authorize_canonical(capability, action),
    }
}

/// Writer CREATE TABLE/INDEX remains available for the closed executor's
/// idempotent ledger bootstrap. Destructive, temporary, virtual or other
/// schema changes require [`Capability::Maintenance`].
fn authorize_canonical(capability: Capability, action: &AuthAction<'_>) -> bool {
    let denied = matches!(
        action,
        AuthAction::Attach { .. }
            | AuthAction::Detach { .. }
            | AuthAction::CreateTempIndex { .. }
            | AuthAction::CreateTempTable { .. }
            | AuthAction::CreateTempTrigger { .. }
            | AuthAction::CreateTempView { .. }
            | AuthAction::CreateTrigger { .. }
            | AuthAction::CreateView { .. }
            | AuthAction::DropIndex { .. }
            | AuthAction::DropTable { .. }
            | AuthAction::DropTempIndex { .. }
            | AuthAction::DropTempTable { .. }
            | AuthAction::DropTempTrigger { .. }
            | AuthAction::DropTempView { .. }
            | AuthAction::DropTrigger { .. }
            | AuthAction::DropView { .. }
            | AuthAction::AlterTable { .. }
            | AuthAction::Analyze { .. }
            | AuthAction::CreateVtable { .. }
            | AuthAction::DropVtable { .. }
            | AuthAction::Unknown { .. }
    ) || is_load_extension(action)
        || matches!(
            action,
            AuthAction::Pragma { pragma_name, pragma_value: Some(pragma_value) }
            if !is_read_only_introspection_pragma(pragma_name)
                && (capability != Capability::Writer
                    || !is_safe_writer_pragma(pragma_name, pragma_value))
        )
        || (capability == Capability::Reader
            && matches!(
                action,
                AuthAction::Insert { .. } | AuthAction::Update { .. } | AuthAction::Delete { .. }
            ));
    !denied
}

/// The exact SQL writer channel legitimately builds durable schema, so
/// ordinary `CREATE TABLE` / `CREATE TRIGGER` is allowed. Temporary tables and
/// indexes are allowed with strictly less reach: a temp object lives in the
/// connection's own `temp` schema, cannot alias or mutate anything in `main`,
/// and disappears with the connection.
///
/// Temporary triggers and views stay denied. A temp trigger can fire on a
/// durable table and mutate it outside the invariant trigger contract; temp
/// views have no caller. `ATTACH`/`DETACH`, `load_extension`, unrecognized
/// actions (SQLite reports a parameter-bound `ATTACH` filename as one), and
/// non-allowlisted pragmas are denied unconditionally.
fn authorize_exact_sql_writer(action: &AuthAction<'_>) -> bool {
    let denied = matches!(
        action,
        AuthAction::Attach { .. }
            | AuthAction::Detach { .. }
            | AuthAction::CreateTempTrigger { .. }
            | AuthAction::CreateTempView { .. }
            | AuthAction::DropTempTrigger { .. }
            | AuthAction::DropTempView { .. }
            | AuthAction::Unknown { .. }
    ) || is_load_extension(action)
        || matches!(
            action,
            AuthAction::Pragma { pragma_name, pragma_value }
            if !is_allowed_exact_sql_pragma(pragma_name, *pragma_value)
        );
    !denied
}

fn is_allowed_exact_sql_pragma(pragma_name: &str, pragma_value: Option<&str>) -> bool {
    is_exact_sql_read_pragma(pragma_name, pragma_value)
        || (pragma_value.is_none() && pragma_name.eq_ignore_ascii_case("shrink_memory"))
        || pragma_value.is_some_and(|value| {
            (pragma_name.eq_ignore_ascii_case("auto_vacuum")
                && (value.eq_ignore_ascii_case("incremental") || value == "2"))
                || (pragma_name.eq_ignore_ascii_case("foreign_keys")
                    && (value.eq_ignore_ascii_case("on") || value == "1"))
                || (pragma_name.eq_ignore_ascii_case("defer_foreign_keys")
                    && (value.eq_ignore_ascii_case("on") || value == "1"))
                || (pragma_name.eq_ignore_ascii_case("busy_timeout")
                    && value.parse::<u32>().is_ok())
                || (pragma_name.eq_ignore_ascii_case("incremental_vacuum")
                    && value.parse::<u32>().is_ok())
                || (pragma_name.eq_ignore_ascii_case("secure_delete")
                    && (value.eq_ignore_ascii_case("on") || value == "1"))
                || (pragma_name.eq_ignore_ascii_case("user_version")
                    && value.parse::<u32>().is_ok())
                || (pragma_name.eq_ignore_ascii_case("wal_autocheckpoint")
                    && value.parse::<u32>().is_ok())
        })
}

fn is_exact_sql_read_pragma(pragma_name: &str, pragma_value: Option<&str>) -> bool {
    const ARGUMENT_SAFE: &[&str] = &[
        "foreign_key_check",
        "foreign_key_list",
        "index_info",
        "index_list",
        "index_xinfo",
        "integrity_check",
        "quick_check",
        "table_info",
        "table_list",
        "table_xinfo",
    ];
    const NO_ARGUMENT_ONLY: &[&str] = &[
        "application_id",
        "auto_vacuum",
        "busy_timeout",
        "cache_size",
        "collation_list",
        "compile_options",
        "data_version",
        "database_list",
        "defer_foreign_keys",
        "foreign_keys",
        "freelist_count",
        "function_list",
        "journal_mode",
        "mmap_size",
        "module_list",
        "page_count",
        "page_size",
        "pragma_list",
        "query_only",
        "recursive_triggers",
        "schema_version",
        "secure_delete",
        "synchronous",
        "temp_store",
        "user_version",
        "wal_autocheckpoint",
    ];

    ARGUMENT_SAFE
        .iter()
        .any(|candidate| pragma_name.eq_ignore_ascii_case(candidate))
        || (pragma_value.is_none()
            && NO_ARGUMENT_ONLY
                .iter()
                .any(|candidate| pragma_name.eq_ignore_ascii_case(candidate)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writer() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection.apply_engine_policy().unwrap();
        connection
            .execute_batch("CREATE TABLE items (value INTEGER)")
            .unwrap();
        connection.set_capability(Capability::Writer).unwrap();
        connection
    }

    #[test]
    fn engine_policy_bounds_the_connection() {
        let connection = writer();
        assert_eq!(connection.limit(Limit::SQLITE_LIMIT_ATTACHED).unwrap(), 0);
        assert_eq!(
            connection.limit(Limit::SQLITE_LIMIT_SQL_LENGTH).unwrap(),
            1024 * 1024
        );
        assert_eq!(
            connection.limit(Limit::SQLITE_LIMIT_VDBE_OP).unwrap(),
            25_000_000
        );
        assert!(
            connection
                .db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE)
                .unwrap()
        );
        let busy_timeout: i64 = connection
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .unwrap();
        assert_eq!(busy_timeout, 0);
    }

    #[test]
    fn exact_sql_scope_reports_transaction_control_and_restores_the_base() {
        let connection = writer();
        for (capability, sql) in [
            (
                Capability::ExactSql { savepoints: false },
                "BEGIN IMMEDIATE",
            ),
            (
                Capability::ExactSql { savepoints: false },
                "SAVEPOINT nested",
            ),
            (Capability::ExactSql { savepoints: true }, "COMMIT"),
            (Capability::ExactSqlRead, "SAVEPOINT nested"),
        ] {
            let error = connection
                .with_capability(capability, Capability::Writer, |connection| {
                    // Swallowing the statement's own error must not hide the denial.
                    let _ = connection.execute_batch(sql);
                    Ok(())
                })
                .expect_err("transaction control is denied");
            assert_eq!(
                error.kind(),
                ErrorKind::TransactionControlDenied,
                "{capability:?} `{sql}`: {error}"
            );
        }
        connection.execute_batch("BEGIN").unwrap();
        connection
            .with_capability(
                Capability::ExactSql { savepoints: true },
                Capability::Writer,
                |connection| connection.execute_batch("SAVEPOINT nested; RELEASE nested;"),
            )
            .expect("a leased savepoint is allowed");
        connection.execute_batch("COMMIT").unwrap();
        let denied = connection
            .execute_batch("CREATE TEMP TABLE scratch (value INTEGER)")
            .expect_err("the restored writer denies temp tables");
        assert_eq!(denied.kind(), ErrorKind::Denied, "{denied}");
    }

    #[test]
    fn exact_sql_writer_allows_temp_tables_and_denies_temp_triggers() {
        let connection = writer();
        connection
            .with_capability(
                Capability::ExactSql { savepoints: false },
                Capability::Writer,
                |connection| connection.execute_batch("CREATE TEMP TABLE scratch (value INTEGER)"),
            )
            .unwrap();
        let denied = connection
            .with_capability(
                Capability::ExactSql { savepoints: false },
                Capability::Writer,
                |connection| {
                    connection.execute_batch(
                        "CREATE TEMP TRIGGER t AFTER INSERT ON items BEGIN SELECT 1; END",
                    )
                },
            )
            .expect_err("temp triggers are denied");
        assert_eq!(denied.kind(), ErrorKind::Denied, "{denied}");
    }

    #[test]
    fn maintenance_scope_denies_attach() {
        let connection = writer();
        let error = connection
            .with_capability(Capability::Maintenance, Capability::Writer, |connection| {
                connection.prepare("ATTACH DATABASE ':memory:' AS other")?;
                Ok(())
            })
            .expect_err("maintenance denies ATTACH");
        assert_eq!(error.kind(), ErrorKind::Denied, "{error}");
    }

    #[test]
    fn statements_cached_in_a_scope_are_authorized_again_under_the_base() {
        let connection = writer();
        let sql = "DROP TABLE items";
        connection
            .with_capability(Capability::Maintenance, Capability::Writer, |connection| {
                connection.prepare_cached(sql).map(drop)
            })
            .unwrap();
        let error = connection
            .prepare_cached(sql)
            .and_then(|mut statement| statement.execute([]))
            .expect_err("the cached DROP is denied once the writer is restored");
        assert_eq!(error.kind(), ErrorKind::Denied, "{error}");
        connection
            .execute("INSERT INTO items VALUES (1)", [])
            .unwrap();
    }
}
