//! The authorizer every exact SQL statement runs under.
//!
//! Every policy connection keeps one authorizer for its whole life,
//! [`authorize_connection`]. [`with_exact_sql_guard`] activates one
//! operation's [`ExactSqlAuthority`] for one connection on the current thread
//! and deactivates it on every exit path, so no statement outside that
//! operation inherits the relaxed exact SQL authority. Swapping the SQLite
//! authorizer instead would expire every prepared statement on the
//! connection, twice per operation.
//!
//! SQLite consults the authorizer only while it compiles a statement, and a
//! statement-cache hit compiles nothing. [`prepare_cached`] therefore keys
//! every cached statement by the authority that compiled it.

use std::{
    cell::RefCell,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use rusqlite::{
    CachedStatement, Connection,
    hooks::{AuthAction, AuthContext, Authorization},
};

use crate::connection::ConnectionMode;

use super::clock;
use super::{
    EXACT_SQL_EXECUTION_LIMIT, EXACT_SQL_PROGRESS_INTERVAL_OPS, ExactSqlError,
    ExactSqlWriteAuthority, ExactSqlWriteIntent, sqlite_error,
};

#[derive(Clone, PartialEq, Eq)]
pub(super) enum AuthorizedDatabaseOperation {
    Attach,
    Detach(String),
}

/// The authority one exact SQL operation runs under. Transaction control is
/// always denied; savepoints only the writer channel's pinned transaction may
/// use.
#[derive(Clone, PartialEq, Eq)]
pub(super) enum ExactSqlAuthority {
    /// A runtime-issued statement under a connection mode's canonical policy.
    Canonical(ConnectionMode),
    /// Caller SQL on the exact SQL writer channel.
    Writer {
        allow_savepoints: bool,
        database_operation: Option<AuthorizedDatabaseOperation>,
    },
}

/// Longest [`ExactSqlAuthority::cache_key_suffix`].
pub(super) const MAX_CACHE_KEY_SUFFIX_BYTES: usize = 64;

impl ExactSqlAuthority {
    /// Distinct per authority, and no suffix ends another, so two keys are
    /// equal only when both their SQL and their authority are. A detach
    /// authority's statement names its database itself, so the name stays
    /// out of the key.
    pub(super) fn cache_key_suffix(&self) -> &'static str {
        use AuthorizedDatabaseOperation::{Attach, Detach};
        match self {
            Self::Canonical(ConnectionMode::Writer) => {
                "\n/* exact SQL authority: canonical writer */"
            }
            Self::Canonical(ConnectionMode::Reader) => {
                "\n/* exact SQL authority: canonical reader */"
            }
            Self::Canonical(ConnectionMode::Maintenance) => {
                "\n/* exact SQL authority: canonical maintenance */"
            }
            Self::Writer {
                allow_savepoints,
                database_operation,
            } => match (allow_savepoints, database_operation) {
                (false, None) => "\n/* exact SQL authority: writer */",
                (true, None) => "\n/* exact SQL authority: writer, savepoints */",
                (false, Some(Attach)) => "\n/* exact SQL authority: writer, attach */",
                (true, Some(Attach)) => "\n/* exact SQL authority: writer, savepoints, attach */",
                (false, Some(Detach(_))) => "\n/* exact SQL authority: writer, detach */",
                (true, Some(Detach(_))) => {
                    "\n/* exact SQL authority: writer, savepoints, detach */"
                }
            },
        }
    }
}

struct ActiveGuard {
    connection: usize,
    authority: ExactSqlAuthority,
    transaction_control_denied: bool,
}

impl ActiveGuard {
    fn authorize(&mut self, context: AuthContext<'_>) -> Authorization {
        let transaction_control = match context.action {
            AuthAction::Transaction { .. } => true,
            AuthAction::Savepoint { .. } => !matches!(
                self.authority,
                ExactSqlAuthority::Writer {
                    allow_savepoints: true,
                    ..
                }
            ),
            _ => false,
        };
        if transaction_control {
            self.transaction_control_denied = true;
            return Authorization::Deny;
        }
        match &self.authority {
            ExactSqlAuthority::Canonical(mode) => crate::connection::authorize(*mode, context),
            ExactSqlAuthority::Writer {
                database_operation, ..
            } => authorize_exact_sql_writer(context, database_operation.as_ref()),
        }
    }
}

thread_local! {
    static ACTIVE_GUARD: RefCell<Option<ActiveGuard>> = const { RefCell::new(None) };
}

/// Keeps one operation's authority active until dropped, then reinstates
/// whatever guard was active before it.
struct Activation {
    previous: Option<ActiveGuard>,
}

impl Activation {
    fn begin(connection: &Connection, authority: ExactSqlAuthority) -> Self {
        let previous = ACTIVE_GUARD.replace(Some(ActiveGuard {
            connection: connection_identity(connection),
            authority,
            transaction_control_denied: false,
        }));
        Self { previous }
    }

    fn transaction_control_denied(&self) -> bool {
        ACTIVE_GUARD.with_borrow(|active| {
            active
                .as_ref()
                .is_some_and(|guard| guard.transaction_control_denied)
        })
    }
}

impl Drop for Activation {
    fn drop(&mut self) {
        ACTIVE_GUARD.set(self.previous.take());
    }
}

pub(crate) fn connection_identity(connection: &Connection) -> usize {
    // SAFETY: the handle is compared as an address and never dereferenced.
    unsafe { connection.handle() }.addr()
}

/// The authorizer a policy connection keeps for its whole life: the exact SQL
/// authority active for it on this thread, else its own mode's policy.
pub(crate) fn authorize_connection(
    connection: usize,
    mode: ConnectionMode,
    context: AuthContext<'_>,
) -> Authorization {
    ACTIVE_GUARD.with_borrow_mut(|active| match active {
        Some(guard) if guard.connection == connection => guard.authorize(context),
        _ => crate::connection::authorize(mode, context),
    })
}

/// `prepare_cached` for exact SQL, keyed by the authority active on
/// `connection` so a cached statement is only reused under the authority that
/// compiled it.
pub(super) fn prepare_cached<'c>(
    connection: &'c Connection,
    sql: &str,
) -> rusqlite::Result<CachedStatement<'c>> {
    let identity = connection_identity(connection);
    let suffix = ACTIVE_GUARD.with_borrow(|active| {
        active
            .as_ref()
            .filter(|guard| guard.connection == identity)
            .map(|guard| guard.authority.cache_key_suffix())
    });
    match suffix {
        Some(suffix) => connection.prepare_cached(&format!("{sql}{suffix}")),
        None => connection.prepare_cached(sql),
    }
}

pub(super) fn with_exact_sql_guard<T, F>(
    connection: &Connection,
    authority: ExactSqlAuthority,
    shutdown_requested: Option<Arc<AtomicBool>>,
    execution_deadline: Option<Instant>,
    enforce_statement_limit: bool,
    repeated_authority: Option<(Arc<dyn ExactSqlWriteAuthority>, ExactSqlWriteIntent)>,
    operation: F,
) -> Result<T, ExactSqlError>
where
    F: FnOnce() -> Result<T, ExactSqlError>,
{
    let deadline = if enforce_statement_limit {
        let operation_deadline = clock::now() + EXACT_SQL_EXECUTION_LIMIT;
        Some(
            execution_deadline
                .map(|deadline| deadline.min(operation_deadline))
                .unwrap_or(operation_deadline),
        )
    } else {
        execution_deadline
    };
    let authority_failure = Arc::new(Mutex::new(None));
    let progress_authority_failure = Arc::clone(&authority_failure);
    if let Err(error) = connection.progress_handler(
        EXACT_SQL_PROGRESS_INTERVAL_OPS,
        Some(move || {
            if shutdown_requested
                .as_ref()
                .is_some_and(|shutdown| shutdown.load(Ordering::Acquire))
                || deadline.is_some_and(|deadline| clock::now() >= deadline)
            {
                return true;
            }
            if let Some((authority, intent)) = repeated_authority.as_ref()
                && let Err(error) = authority.verify(*intent)
            {
                *progress_authority_failure
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
                return true;
            }
            false
        }),
    ) {
        return Err(sqlite_error("install execution guard", error));
    }

    let activation = Activation::begin(connection, authority);
    let result = catch_unwind(AssertUnwindSafe(operation));
    let denied = activation.transaction_control_denied();
    drop(activation);
    let cleanup = connection
        .progress_handler(EXACT_SQL_PROGRESS_INTERVAL_OPS, None::<fn() -> bool>)
        .map_err(|error| sqlite_error("clear execution guard", error));
    let result = match result {
        Ok(result) => result,
        Err(payload) => {
            let _ = cleanup;
            resume_unwind(payload);
        }
    };
    cleanup?;
    let authority_error = authority_failure
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(error) = authority_error {
        Err(error)
    } else if denied {
        Err(ExactSqlError::TransactionControlDenied)
    } else {
        result
    }
}

/// Authorizes the exact SQL writer channel.
///
/// This channel legitimately builds durable schema, so ordinary `CREATE
/// TABLE` / `CREATE TRIGGER` is allowed. Temporary **tables and indexes** are
/// allowed for the same reason and with strictly less reach: a temp object
/// lives in the connection's own `temp` schema, cannot alias or mutate
/// anything in `main`, and disappears with the connection. Denying them while
/// permitting durable DDL inverted the blast radius, and it left derived
/// per-connection scratch, the projection output-state cache, unable to
/// exist at all.
///
/// Temporary **triggers and views** stay denied. A temp trigger can fire on a
/// durable table and mutate it outside the invariant trigger contract, which
/// is exactly the authority this channel must not hand out; temp views have no
/// caller. `ATTACH`/`DETACH` are allowed only while the writer actor runs its
/// fixed attachment lifecycle operations; caller-provided SQL cannot enable
/// them. `load_extension`, unrecognized actions, and non-allowlisted pragmas
/// remain denied unconditionally.
fn authorize_exact_sql_writer(
    context: rusqlite::hooks::AuthContext<'_>,
    database_operation: Option<&AuthorizedDatabaseOperation>,
) -> Authorization {
    match context.action {
        AuthAction::Attach { .. }
            if matches!(
                database_operation,
                Some(AuthorizedDatabaseOperation::Attach)
            ) =>
        {
            return Authorization::Allow;
        }
        // SQLite supplies a null authorizer filename when ATTACH binds its
        // filename parameter. rusqlite preserves that action as Unknown.
        AuthAction::Unknown {
            code,
            arg1: None,
            arg2: None,
        } if code == rusqlite::ffi::SQLITE_ATTACH
            && matches!(
                database_operation,
                Some(AuthorizedDatabaseOperation::Attach)
            ) =>
        {
            return Authorization::Allow;
        }
        AuthAction::Detach { database_name }
            if matches!(
                database_operation,
                Some(AuthorizedDatabaseOperation::Detach(expected))
                    if database_name.eq_ignore_ascii_case(expected)
            ) =>
        {
            return Authorization::Allow;
        }
        _ => {}
    }
    if matches!(
        context.action,
        AuthAction::Attach { .. }
            | AuthAction::Detach { .. }
            | AuthAction::CreateTempTrigger { .. }
            | AuthAction::CreateTempView { .. }
            | AuthAction::DropTempTrigger { .. }
            | AuthAction::DropTempView { .. }
            | AuthAction::Unknown { .. }
    ) || matches!(
        context.action,
        AuthAction::Function { function_name }
            if function_name.eq_ignore_ascii_case("load_extension")
    ) || matches!(
        context.action,
        AuthAction::Pragma {
            pragma_name,
            pragma_value,
        }
        if !is_allowed_exact_sql_pragma(pragma_name, pragma_value)
    ) {
        Authorization::Deny
    } else {
        Authorization::Allow
    }
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
