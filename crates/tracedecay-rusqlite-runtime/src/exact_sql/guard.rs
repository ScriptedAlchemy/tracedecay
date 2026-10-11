//! The capability and execution guard every exact SQL statement runs under.
//!
//! [`with_exact_sql_guard`] scopes both to one operation. The driver restores
//! the connection's base capability and removes the guard on every exit path,
//! so no statement outside that operation inherits the exact SQL authority.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use tracedecay_sql::{Capability, Connection, ConnectionControl, ErrorExt, ErrorKind};

use super::clock;
use super::{
    EXACT_SQL_EXECUTION_LIMIT, ExactSqlError, ExactSqlWriteAuthority, ExactSqlWriteIntent,
    sqlite_error,
};

#[allow(clippy::too_many_arguments)]
pub(super) fn with_exact_sql_guard<T, F>(
    connection: &Connection,
    capability: Capability,
    base: Capability,
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
    let should_interrupt = move || {
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
    };
    let guarded = connection.with_capability(capability, base, |connection| {
        connection.guard_scope(should_interrupt, |_| operation())
    });
    let authority_error = authority_failure
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    match guarded {
        Err(error) if error.kind() == ErrorKind::TransactionControlDenied => {
            Err(authority_error.unwrap_or(ExactSqlError::TransactionControlDenied))
        }
        Err(error) => Err(sqlite_error("exact SQL guard scope", error)),
        Ok(result) => match authority_error {
            Some(error) => Err(error),
            None => result,
        },
    }
}
