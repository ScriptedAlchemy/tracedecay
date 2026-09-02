//! The authorizer every exact SQL statement runs under.
//!
//! [`with_exact_sql_guard`] installs the hooks for one operation and removes
//! them again on every exit path, so no statement outside that operation ever
//! inherits the relaxed exact SQL authority.

use std::{
    collections::BTreeSet,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use rusqlite::{
    Connection,
    hooks::{Action, AuthAction, Authorization},
};

use super::{
    EXACT_SQL_EXECUTION_LIMIT, EXACT_SQL_PROGRESS_INTERVAL_OPS, ExactSqlError,
    ExactSqlWriteAuthority, ExactSqlWriteIntent, sqlite_error,
};

#[derive(Default)]
pub(super) struct InsertTracker {
    authorized_tables: Mutex<BTreeSet<String>>,
    pub(super) applied: AtomicBool,
}

#[derive(Clone)]
pub(super) enum AuthorizedDatabaseOperation {
    Attach,
    Detach(String),
    Vacuum,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn with_exact_sql_guard<T, F>(
    connection: &Connection,
    allow_savepoints: bool,
    allow_transactions: bool,
    shutdown_requested: Option<Arc<AtomicBool>>,
    execution_deadline: Option<Instant>,
    enforce_statement_limit: bool,
    repeated_authority: Option<(Arc<dyn ExactSqlWriteAuthority>, ExactSqlWriteIntent)>,
    canonical_authorizer: for<'a> fn(rusqlite::hooks::AuthContext<'a>) -> Authorization,
    exact_sql_writer: bool,
    database_operation: Option<AuthorizedDatabaseOperation>,
    insert_tracker: Option<Arc<InsertTracker>>,
    operation: F,
) -> Result<T, ExactSqlError>
where
    F: FnOnce() -> Result<T, ExactSqlError>,
{
    let denied = Arc::new(AtomicBool::new(false));
    let hook_denied = Arc::clone(&denied);
    let authorizer_tracker = insert_tracker.clone();
    let authorized_database_operation = database_operation.clone();
    connection
        .authorizer(Some(move |context: rusqlite::hooks::AuthContext<'_>| {
            if context.accessor.is_none()
                && let AuthAction::Insert { table_name } = context.action
                && !table_name.eq_ignore_ascii_case("sqlite_master")
                && !table_name.eq_ignore_ascii_case("sqlite_schema")
                && let Some(tracker) = &authorizer_tracker
            {
                tracker
                    .authorized_tables
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(table_name.to_owned());
            }
            if (!allow_transactions && matches!(context.action, AuthAction::Transaction { .. }))
                || (!allow_savepoints && matches!(context.action, AuthAction::Savepoint { .. }))
            {
                hook_denied.store(true, Ordering::Release);
                Authorization::Deny
            } else if exact_sql_writer {
                authorize_exact_sql_writer(context, authorized_database_operation.as_ref())
            } else {
                canonical_authorizer(context)
            }
        }))
        .map_err(|error| sqlite_error("install transaction-control guard", error))?;
    if let Some(tracker) = &insert_tracker {
        let hook_tracker = Arc::clone(tracker);
        if let Err(error) = connection.update_hook(Some(
            move |action: Action, _database: &str, table: &str, _rowid: i64| {
                if action == Action::SQLITE_INSERT
                    && hook_tracker
                        .authorized_tables
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .contains(table)
                {
                    hook_tracker.applied.store(true, Ordering::Release);
                }
            },
        )) {
            let _ = connection.authorizer(Some(canonical_authorizer));
            return Err(sqlite_error("install insert tracker", error));
        }
    }
    let deadline = if enforce_statement_limit {
        let operation_deadline = Instant::now() + EXACT_SQL_EXECUTION_LIMIT;
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
                || deadline.is_some_and(|deadline| Instant::now() >= deadline)
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
        let _ = connection.update_hook(None::<fn(Action, &str, &str, i64)>);
        let _ = connection.authorizer(Some(canonical_authorizer));
        return Err(sqlite_error("install execution guard", error));
    }

    let result = catch_unwind(AssertUnwindSafe(operation));
    let clear_progress =
        connection.progress_handler(EXACT_SQL_PROGRESS_INTERVAL_OPS, None::<fn() -> bool>);
    let clear_update_hook = connection.update_hook(None::<fn(Action, &str, &str, i64)>);
    let restore_authorizer = connection.authorizer(Some(canonical_authorizer));
    let cleanup = clear_progress
        .map_err(|error| sqlite_error("clear execution guard", error))
        .and_then(|()| {
            clear_update_hook.map_err(|error| sqlite_error("clear insert tracker", error))
        })
        .and_then(|()| {
            restore_authorizer.map_err(|error| sqlite_error("restore connection authorizer", error))
        });
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
    } else if denied.load(Ordering::Acquire) {
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
/// per-connection scratch — the projection output-state cache — unable to
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
                Some(AuthorizedDatabaseOperation::Attach | AuthorizedDatabaseOperation::Vacuum)
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
                Some(AuthorizedDatabaseOperation::Attach | AuthorizedDatabaseOperation::Vacuum)
            ) =>
        {
            return Authorization::Allow;
        }
        AuthAction::Detach { database_name }
            if matches!(
                database_operation,
                Some(AuthorizedDatabaseOperation::Detach(expected))
                    if database_name.eq_ignore_ascii_case(expected)
            ) || matches!(
                database_operation,
                Some(AuthorizedDatabaseOperation::Vacuum)
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
