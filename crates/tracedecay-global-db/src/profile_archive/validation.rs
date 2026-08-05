use std::{collections::BTreeSet, ffi::OsStr, fs, path::Path};

#[derive(Clone, Copy)]
enum DatabaseRole {
    Registered,
    Graph,
}

pub(super) fn validate_exact_final_databases(
    profile_root: &Path,
) -> tracedecay_runtime_core::errors::Result<()> {
    let mut pending = vec![profile_root.to_path_buf()];
    let mut found = false;
    while let Some(path) = pending.pop() {
        let mut entries = fs::read_dir(&path)
            .map_err(|error| {
                database_error(format!(
                    "read profile directory '{}': {error}",
                    path.display()
                ))
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| database_error(format!("read profile entry: {error}")))?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                database_error(format!(
                    "inspect profile entry '{}': {error}",
                    path.display()
                ))
            })?;
            if metadata.file_type().is_symlink() {
                return Err(reset(&path, "regular exact-final profile files", "symlink"));
            }
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            if metadata.is_file() && path.extension() == Some(OsStr::new("db")) {
                found = true;
                let role = database_role(profile_root, &path)?;
                validate_exact_final_database(&path, role)?;
            }
        }
    }
    if !found {
        return Err(reset(
            profile_root,
            "an exact-final TraceDecay profile",
            "no TraceDecay databases",
        ));
    }
    Ok(())
}

fn database_role(
    profile_root: &Path,
    path: &Path,
) -> tracedecay_runtime_core::errors::Result<DatabaseRole> {
    let relative = path.strip_prefix(profile_root).map_err(|_| {
        reset(
            path,
            "a database contained by the profile root",
            "database escaped the profile root",
        )
    })?;
    match relative {
        path if path == Path::new("global.db") || path == Path::new("user-sessions.db") => {
            Ok(DatabaseRole::Registered)
        }
        path if path == Path::new("user-memory.db") => Ok(DatabaseRole::Graph),
        path if path.starts_with("projects")
            && path.file_name() == Some(OsStr::new("sessions.db")) =>
        {
            Ok(DatabaseRole::Registered)
        }
        path if path.starts_with("projects") => Ok(DatabaseRole::Graph),
        _ => Err(reset(
            path,
            "a recognized final database role",
            "unrecognized .db path",
        )),
    }
}

fn validate_exact_final_database(
    path: &Path,
    role: DatabaseRole,
) -> tracedecay_runtime_core::errors::Result<()> {
    let connection = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| {
        reset(
            path,
            role.expected(),
            format!("unreadable SQLite database: {error}"),
        )
    })?;
    match role {
        DatabaseRole::Registered => validate_registered(path, &connection)?,
        DatabaseRole::Graph => validate_graph(path, &connection)?,
    }
    validate_integrity(path, role, &connection)
}

impl DatabaseRole {
    fn expected(self) -> String {
        match self {
            Self::Registered => "exact-final registered global/session schema".to_owned(),
            Self::Graph => format!(
                "schema v{} ({})",
                tracedecay_runtime_core::db::schema::SCHEMA_VERSION,
                tracedecay_runtime_core::db::schema::SCHEMA_IDENTITY
            ),
        }
    }
}

fn validate_registered(
    path: &Path,
    connection: &rusqlite::Connection,
) -> tracedecay_runtime_core::errors::Result<()> {
    let version = connection
        .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
        .map_err(|error| {
            reset(
                path,
                DatabaseRole::Registered.expected(),
                format!("unreadable schema version: {error}"),
            )
        })?;
    if version != 0 {
        return Err(reset(
            path,
            DatabaseRole::Registered.expected(),
            format!("registered schema user_version {version}"),
        ));
    }
    for (kind, name) in registered_objects() {
        require_object(path, DatabaseRole::Registered, connection, kind, name)?;
    }
    require_marker(
        path,
        connection,
        "lcm",
        tracedecay_sessions::runtime::lcm::schema::LCM_SCHEMA_VERSION,
    )?;
    require_marker(
        path,
        connection,
        "workflow_indexing",
        tracedecay_sessions::runtime::workflow_index::WORKFLOW_INDEX_SCHEMA_VERSION,
    )?;
    let git_identity = connection
        .query_row(
            "SELECT value FROM git_correlation_meta WHERE key = 'schema_identity'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| {
            reset(
                path,
                DatabaseRole::Registered.expected(),
                format!("missing git correlation identity: {error}"),
            )
        })?;
    if git_identity
        != tracedecay_sessions::runtime::git_correlation::GIT_CORRELATION_SCHEMA_IDENTITY
    {
        return Err(reset(
            path,
            DatabaseRole::Registered.expected(),
            format!("git correlation identity {git_identity}"),
        ));
    }
    Ok(())
}

fn registered_objects() -> Vec<(&'static str, &'static str)> {
    let mut objects = Vec::new();
    objects.extend_from_slice(crate::schema_stages::CONFIGURATION_OBJECTS);
    objects.extend_from_slice(crate::schema_stages::TRANSCRIPT_OBJECTS);
    objects.extend_from_slice(crate::schema_stages::LCM_OBJECTS);
    objects.extend_from_slice(crate::schema_stages::WORKFLOW_OBJECTS);
    objects.extend_from_slice(&[
        ("table", "projects"),
        ("table", "code_projects"),
        ("table", "project_aliases"),
        ("table", "store_instances"),
        ("table", "graph_scopes"),
        ("table", "store_artifacts"),
        ("table", "observations"),
        ("table", "retrieval_anchors"),
        ("table", "projection_queue"),
        ("table", "work_events_v1"),
        ("table", "authorized_scope_sets_v1"),
        ("table", "session_git_spans"),
        ("table", "commit_sessions"),
        ("table", "git_correlation_meta"),
    ]);
    objects
}

fn require_marker(
    path: &Path,
    connection: &rusqlite::Connection,
    name: &str,
    expected: i64,
) -> tracedecay_runtime_core::errors::Result<()> {
    let actual = connection
        .query_row(
            "SELECT version FROM session_schema_migrations WHERE name = ?1",
            [name],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| {
            reset(
                path,
                DatabaseRole::Registered.expected(),
                format!("missing schema marker '{name}': {error}"),
            )
        })?;
    if actual != expected {
        return Err(reset(
            path,
            DatabaseRole::Registered.expected(),
            format!("schema marker '{name}' is {actual}, expected {expected}"),
        ));
    }
    Ok(())
}

fn validate_graph(
    path: &Path,
    connection: &rusqlite::Connection,
) -> tracedecay_runtime_core::errors::Result<()> {
    let expected = DatabaseRole::Graph.expected();
    let version = connection
        .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
        .map_err(|error| {
            reset(
                path,
                &expected,
                format!("unreadable schema version: {error}"),
            )
        })?;
    if version != tracedecay_runtime_core::db::schema::SCHEMA_VERSION {
        return Err(reset(path, expected, format!("schema version v{version}")));
    }
    let identity = connection
        .query_row(
            "SELECT value FROM metadata WHERE key = ?1",
            [tracedecay_runtime_core::db::schema::SCHEMA_IDENTITY_KEY],
            |row| row.get::<_, String>(0),
        )
        .map_err(|_| reset(path, &expected, "missing canonical schema identity"))?;
    if identity != tracedecay_runtime_core::db::schema::SCHEMA_IDENTITY {
        return Err(reset(path, expected, format!("schema identity {identity}")));
    }
    for (kind, name) in tracedecay_runtime_core::db::schema::REQUIRED_SCHEMA_OBJECTS {
        require_object(path, DatabaseRole::Graph, connection, kind, name)?;
    }
    for (table, required_columns) in tracedecay_runtime_core::db::schema::REQUIRED_SCHEMA_COLUMNS {
        let columns = table_columns(path, DatabaseRole::Graph, connection, table)?;
        if let Some(missing) = required_columns
            .iter()
            .find(|column| !columns.contains(**column))
        {
            return Err(reset(
                path,
                DatabaseRole::Graph.expected(),
                format!("missing column {table}.{missing}"),
            ));
        }
    }
    Ok(())
}

fn require_object(
    path: &Path,
    role: DatabaseRole,
    connection: &rusqlite::Connection,
    kind: &str,
    name: &str,
) -> tracedecay_runtime_core::errors::Result<()> {
    let present = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = ?1 AND name = ?2)",
            [kind, name],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|error| {
            reset(
                path,
                role.expected(),
                format!("unreadable {kind} {name}: {error}"),
            )
        })?;
    if !present {
        return Err(reset(
            path,
            role.expected(),
            format!("missing {kind} {name}"),
        ));
    }
    Ok(())
}

fn table_columns(
    path: &Path,
    role: DatabaseRole,
    connection: &rusqlite::Connection,
    table: &str,
) -> tracedecay_runtime_core::errors::Result<BTreeSet<String>> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|error| {
            reset(
                path,
                role.expected(),
                format!("unreadable table {table}: {error}"),
            )
        })?;
    statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| {
            reset(
                path,
                role.expected(),
                format!("unreadable table {table}: {error}"),
            )
        })?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| {
            reset(
                path,
                role.expected(),
                format!("invalid table {table}: {error}"),
            )
        })
}

fn validate_integrity(
    path: &Path,
    role: DatabaseRole,
    connection: &rusqlite::Connection,
) -> tracedecay_runtime_core::errors::Result<()> {
    let integrity = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
        .map_err(|error| {
            reset(
                path,
                role.expected(),
                format!("unreadable integrity state: {error}"),
            )
        })?;
    if integrity != "ok" {
        return Err(reset(
            path,
            role.expected(),
            format!("integrity check: {integrity}"),
        ));
    }
    let reference_violation = connection
        .prepare("PRAGMA foreign_key_check")
        .and_then(|mut statement| statement.exists([]))
        .map_err(|error| {
            reset(
                path,
                role.expected(),
                format!("unreadable foreign-key references: {error}"),
            )
        })?;
    if reference_violation {
        return Err(reset(
            path,
            role.expected(),
            "foreign-key reference violations",
        ));
    }
    Ok(())
}

fn reset(
    path: &Path,
    expected: impl Into<String>,
    actual: impl Into<String>,
) -> tracedecay_runtime_core::errors::TraceDecayError {
    tracedecay_runtime_core::errors::TraceDecayError::ResetRequired {
        store: path.display().to_string(),
        expected: expected.into(),
        actual: actual.into(),
    }
}

fn database_error(message: String) -> tracedecay_runtime_core::errors::TraceDecayError {
    tracedecay_runtime_core::errors::TraceDecayError::Database {
        operation: "validate final profile schema".to_owned(),
        message,
    }
}
