use std::{
    fmt,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    path::Path,
    time::Duration,
};

use rusqlite::{
    Connection, OpenFlags,
    hooks::{AuthAction, AuthContext, Authorization},
    limits::Limit,
};

const PROGRESS_INTERVAL_OPS: i32 = 1_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConnectionMode {
    Writer,
    Reader,
    Maintenance,
}

#[derive(Debug)]
pub struct ConnectionPolicyError {
    stage: &'static str,
    source: rusqlite::Error,
}

impl ConnectionPolicyError {
    pub fn is_open_failure(&self) -> bool {
        self.stage == "open"
    }
}

impl fmt::Display for ConnectionPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "SQLite connection policy failed at {}: {}",
            self.stage, self.source
        )
    }
}

impl std::error::Error for ConnectionPolicyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

pub(crate) fn open(path: &Path, mode: ConnectionMode) -> Result<Connection, ConnectionPolicyError> {
    let flags = match mode {
        ConnectionMode::Reader => OpenFlags::SQLITE_OPEN_READ_ONLY,
        ConnectionMode::Writer | ConnectionMode::Maintenance => OpenFlags::SQLITE_OPEN_READ_WRITE,
    } | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE;
    let connection =
        Connection::open_with_flags(path, flags).map_err(|source| policy("open", source))?;

    apply_pragmas(&connection, mode)?;
    assert_compile_options(&connection)?;
    apply_limits(&connection, mode)?;
    install_authorizer(&connection, mode)?;
    Ok(connection)
}

/// Opens a one-shot immutable, query-only health connection for Doctor.
///
/// Uses `file:…?immutable=1&mode=ro` so diagnosis never creates WAL/SHM
/// sidecars or acquires authority locks. Callers must refuse non-empty WAL
/// families before invoking this opener.
pub fn open_immutable_health_reader(path: &Path) -> Result<Connection, ConnectionPolicyError> {
    let uri = immutable_health_uri(path)?;
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE;
    let connection =
        Connection::open_with_flags(uri, flags).map_err(|source| policy("open", source))?;
    connection
        .busy_timeout(Duration::ZERO)
        .map_err(|source| policy("busy timeout", source))?;
    connection
        .pragma_update(None, "query_only", true)
        .map_err(|source| policy("query-only reader", source))?;
    verify_pragma_i64(&connection, "query_only", 1)?;
    Ok(connection)
}

fn immutable_health_uri(path: &Path) -> Result<String, ConnectionPolicyError> {
    let raw = path.to_str().ok_or_else(|| ConnectionPolicyError {
        stage: "immutable uri",
        source: rusqlite::Error::InvalidPath(path.to_path_buf()),
    })?;
    let mut encoded = String::with_capacity(raw.len() + 24);
    for ch in raw.chars() {
        match ch {
            '?' => encoded.push_str("%3f"),
            '#' => encoded.push_str("%23"),
            '%' => encoded.push_str("%25"),
            other => encoded.push(other),
        }
    }
    Ok(format!("file:{encoded}?immutable=1&mode=ro"))
}

fn apply_pragmas(
    connection: &Connection,
    mode: ConnectionMode,
) -> Result<(), ConnectionPolicyError> {
    // SQLite must never wait past the runtime's own queue/deadline authority.
    connection
        .busy_timeout(Duration::ZERO)
        .map_err(|source| policy("busy timeout", source))?;
    connection
        .pragma_update(None, "foreign_keys", true)
        .map_err(|source| policy("foreign keys", source))?;
    connection
        .pragma_update(None, "trusted_schema", false)
        .map_err(|source| policy("trusted schema", source))?;

    if mode == ConnectionMode::Writer {
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|source| policy("WAL journal", source))?;
        connection
            .pragma_update(None, "wal_autocheckpoint", 0_i64)
            .map_err(|source| policy("WAL auto-checkpoint", source))?;
        connection
            .pragma_update(None, "synchronous", "NORMAL")
            .map_err(|source| policy("synchronous mode", source))?;
    }
    if mode == ConnectionMode::Reader {
        connection
            .pragma_update(None, "query_only", true)
            .map_err(|source| policy("query-only reader", source))?;
    }

    verify_pragma_i64(connection, "foreign_keys", 1)?;
    verify_pragma_i64(connection, "trusted_schema", 0)?;
    match mode {
        ConnectionMode::Writer => {
            verify_pragma_text(connection, "journal_mode", "wal")?;
            verify_pragma_i64(connection, "wal_autocheckpoint", 0)?;
            verify_pragma_i64(connection, "synchronous", 1)?;
        }
        ConnectionMode::Reader => verify_pragma_i64(connection, "query_only", 1)?,
        ConnectionMode::Maintenance => {}
    }
    Ok(())
}

fn verify_pragma_i64(
    connection: &Connection,
    name: &'static str,
    expected: i64,
) -> Result<(), ConnectionPolicyError> {
    let actual: i64 = connection
        .pragma_query_value(None, name, |row| row.get(0))
        .map_err(|source| policy("pragma verification", source))?;
    if actual != expected {
        return Err(policy(
            "pragma verification",
            rusqlite::Error::InvalidParameterName(format!("{name}={actual}, expected {expected}")),
        ));
    }
    Ok(())
}

fn verify_pragma_text(
    connection: &Connection,
    name: &'static str,
    expected: &str,
) -> Result<(), ConnectionPolicyError> {
    let actual: String = connection
        .pragma_query_value(None, name, |row| row.get(0))
        .map_err(|source| policy("pragma verification", source))?;
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(policy(
            "pragma verification",
            rusqlite::Error::InvalidParameterName(format!("{name}={actual}, expected {expected}")),
        ));
    }
    Ok(())
}

fn assert_compile_options(connection: &Connection) -> Result<(), ConnectionPolicyError> {
    let mut statement = connection
        .prepare("PRAGMA compile_options")
        .map_err(|source| policy("compile options", source))?;
    let options = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|source| policy("compile options", source))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|source| policy("compile options", source))?;
    for required in ["ENABLE_FTS5", "THREADSAFE=1"] {
        if !options.iter().any(|option| option == required) {
            return Err(policy(
                "compile options",
                rusqlite::Error::InvalidParameterName(format!("missing {required}")),
            ));
        }
    }
    if options.iter().any(|option| option == "OMIT_FOREIGN_KEY") {
        return Err(policy(
            "compile options",
            rusqlite::Error::InvalidParameterName("OMIT_FOREIGN_KEY is unsupported".to_owned()),
        ));
    }
    Ok(())
}

fn apply_limits(
    connection: &Connection,
    mode: ConnectionMode,
) -> Result<(), ConnectionPolicyError> {
    let attached = if mode == ConnectionMode::Maintenance {
        4
    } else {
        0
    };
    for (limit, value) in [
        (Limit::SQLITE_LIMIT_LENGTH, 64 * 1024 * 1024),
        (Limit::SQLITE_LIMIT_SQL_LENGTH, 1024 * 1024),
        (Limit::SQLITE_LIMIT_COLUMN, 2_000),
        (Limit::SQLITE_LIMIT_EXPR_DEPTH, 100),
        (Limit::SQLITE_LIMIT_COMPOUND_SELECT, 100),
        (Limit::SQLITE_LIMIT_VDBE_OP, 25_000_000),
        (Limit::SQLITE_LIMIT_FUNCTION_ARG, 100),
        (Limit::SQLITE_LIMIT_ATTACHED, attached),
        (Limit::SQLITE_LIMIT_LIKE_PATTERN_LENGTH, 50_000),
        (Limit::SQLITE_LIMIT_VARIABLE_NUMBER, 32_766),
        (Limit::SQLITE_LIMIT_TRIGGER_DEPTH, 32),
        (Limit::SQLITE_LIMIT_WORKER_THREADS, 0),
    ] {
        connection
            .set_limit(limit, value)
            .map_err(|source| policy("runtime limits", source))?;
    }
    Ok(())
}

fn install_authorizer(
    connection: &Connection,
    mode: ConnectionMode,
) -> Result<(), ConnectionPolicyError> {
    let result = match mode {
        ConnectionMode::Writer => connection.authorizer(Some(authorize_writer)),
        ConnectionMode::Reader => connection.authorizer(Some(authorize_reader)),
        ConnectionMode::Maintenance => connection.authorizer(Some(authorize_maintenance)),
    };
    result.map_err(|source| policy("authorizer", source))
}

fn authorize_writer(context: AuthContext<'_>) -> Authorization {
    authorize(ConnectionMode::Writer, context)
}

fn authorize_reader(context: AuthContext<'_>) -> Authorization {
    authorize(ConnectionMode::Reader, context)
}

fn authorize_maintenance(_: AuthContext<'_>) -> Authorization {
    Authorization::Allow
}

fn authorize(mode: ConnectionMode, context: AuthContext<'_>) -> Authorization {
    if mode == ConnectionMode::Maintenance {
        return Authorization::Allow;
    }
    // Writer-mode CREATE TABLE/INDEX remains available for the closed
    // executor's idempotent ledger bootstrap. Destructive, temporary, virtual,
    // or other schema changes require the explicit Maintenance mode above.
    let denied = matches!(
        context.action,
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
    ) || matches!(context.action, AuthAction::Function { function_name } if function_name.eq_ignore_ascii_case("load_extension"))
        || matches!(
            context.action,
            AuthAction::Pragma {
                pragma_name,
                pragma_value: Some(_),
            }
            if mode != ConnectionMode::Writer
                || (!pragma_name.eq_ignore_ascii_case("wal_autocheckpoint")
                    && !pragma_name.eq_ignore_ascii_case("wal_checkpoint"))
        )
        || (mode == ConnectionMode::Reader
            && matches!(
                context.action,
                AuthAction::Insert { .. } | AuthAction::Update { .. } | AuthAction::Delete { .. }
            ));
    if denied {
        Authorization::Deny
    } else {
        Authorization::Allow
    }
}

pub(crate) fn with_progress_cancellation<T, C, F>(
    connection: &mut Connection,
    should_cancel: C,
    operation: F,
) -> rusqlite::Result<T>
where
    C: FnMut() -> bool + Send + 'static,
    F: FnOnce(&mut Connection) -> T,
{
    connection.progress_handler(PROGRESS_INTERVAL_OPS, Some(should_cancel))?;
    let result = catch_unwind(AssertUnwindSafe(|| operation(connection)));
    let clear = connection.progress_handler(PROGRESS_INTERVAL_OPS, None::<fn() -> bool>);
    match result {
        Ok(value) => {
            clear?;
            Ok(value)
        }
        Err(payload) => resume_unwind(payload),
    }
}

fn policy(stage: &'static str, source: rusqlite::Error) -> ConnectionPolicyError {
    ConnectionPolicyError { stage, source }
}

#[cfg(test)]
mod tests;
