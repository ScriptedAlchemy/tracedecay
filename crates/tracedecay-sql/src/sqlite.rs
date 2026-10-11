//! The SQLite arm: rusqlite's types plus the control surface as extension
//! traits. The flip unit deletes this module together with the engine flag.

use std::{
    ffi::CString,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
};

pub use rusqlite::{
    CachedStatement, Connection, DropBehavior, Error, InterruptHandle, MappedRows, OpenFlags,
    OptionalExtension, Params, Result, Row, Rows, Savepoint, Statement, ToSql, Transaction,
    TransactionBehavior, params, params_from_iter,
};
use rusqlite::{
    ErrorCode,
    hooks::{AuthAction, AuthContext, Authorization},
};

use crate::{
    Capability, CheckpointMode, CheckpointOutcome, ConnectionControl, Engine, ErrorExt, ErrorKind,
    PROGRESS_INTERVAL_OPS, PageStats,
};

pub const ENGINE: Engine = Engine::Sqlite;

pub mod types {
    pub use rusqlite::types::{
        FromSql, FromSqlError, FromSqlResult, ToSqlOutput, Type, Value, ValueRef,
    };
}

impl ErrorExt for Error {
    fn kind(&self) -> ErrorKind {
        if matches!(self, Error::QueryReturnedNoRows) {
            return ErrorKind::NoRows;
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
}

impl ConnectionControl for Connection {
    fn set_capability(&self, capability: Capability) -> Result<()> {
        match capability {
            Capability::Reader => self.authorizer(Some(authorize_reader)),
            Capability::Writer => self.authorizer(Some(authorize_writer)),
            Capability::Maintenance => {
                self.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
            }
        }
    }

    fn with_capability<T>(
        &self,
        scoped: Capability,
        base: Capability,
        operation: impl FnOnce(&Self) -> Result<T>,
    ) -> Result<T> {
        self.set_capability(scoped)?;
        let result = catch_unwind(AssertUnwindSafe(|| operation(self)));
        // A cached statement keeps the program compiled under `scoped`; SQLite
        // only reauthorizes on reprepare, so the cache must not outlive it.
        self.flush_prepared_statement_cache();
        let restore = self.set_capability(base);
        match result {
            Ok(result) => {
                let value = result?;
                restore?;
                Ok(value)
            }
            Err(payload) => resume_unwind(payload),
        }
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

fn authorize_reader(context: AuthContext<'_>) -> Authorization {
    authorize(Capability::Reader, &context.action)
}

fn authorize_writer(context: AuthContext<'_>) -> Authorization {
    authorize(Capability::Writer, &context.action)
}

/// Schema-introspection and integrity pragmas that cannot mutate the database,
/// file, or connection configuration.
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

fn is_safe_writer_pragma(name: &str) -> bool {
    ["busy_timeout", "wal_autocheckpoint", "wal_checkpoint"]
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

fn authorize(capability: Capability, action: &AuthAction<'_>) -> Authorization {
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
    ) || matches!(action, AuthAction::Function { function_name } if function_name.eq_ignore_ascii_case("load_extension"))
        || matches!(
            action,
            AuthAction::Pragma { pragma_name, pragma_value: Some(_) }
            if !is_read_only_introspection_pragma(pragma_name)
                && (capability != Capability::Writer || !is_safe_writer_pragma(pragma_name))
        )
        || (capability == Capability::Reader
            && matches!(
                action,
                AuthAction::Insert { .. } | AuthAction::Update { .. } | AuthAction::Delete { .. }
            ));
    if denied {
        Authorization::Deny
    } else {
        Authorization::Allow
    }
}
