use std::{fs, path::Path};

pub(super) fn validate_exact_final_databases(
    profile_root: &Path,
) -> tracedecay_runtime_core::errors::Result<()> {
    let mut pending = vec![profile_root.to_path_buf()];
    let mut found = false;
    while let Some(path) = pending.pop() {
        let mut entries = fs::read_dir(&path)
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "validate final profile schema".to_owned(),
                    message: format!("read profile directory '{}': {error}", path.display()),
                },
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "validate final profile schema".to_owned(),
                    message: format!("read profile entry: {error}"),
                },
            )?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "validate final profile schema".to_owned(),
                    message: format!("inspect profile entry '{}': {error}", path.display()),
                }
            })?;
            if metadata.file_type().is_symlink() {
                return Err(
                    tracedecay_runtime_core::errors::TraceDecayError::ResetRequired {
                        store: path.display().to_string(),
                        expected: "regular exact-final profile files".to_owned(),
                        actual: "symlink".to_owned(),
                    },
                );
            }
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            if metadata.is_file() && path.extension().is_some_and(|extension| extension == "db") {
                found = true;
                validate_exact_final_database(&path)?;
            }
        }
    }
    if !found {
        return Err(
            tracedecay_runtime_core::errors::TraceDecayError::ResetRequired {
                store: profile_root.display().to_string(),
                expected: "an exact-final TraceDecay profile".to_owned(),
                actual: "no TraceDecay databases".to_owned(),
            },
        );
    }
    Ok(())
}

fn validate_exact_final_database(path: &Path) -> tracedecay_runtime_core::errors::Result<()> {
    let reset = |actual: String| tracedecay_runtime_core::errors::TraceDecayError::ResetRequired {
        store: path.display().to_string(),
        expected: format!(
            "schema v{} ({})",
            tracedecay_runtime_core::db::schema::SCHEMA_VERSION,
            tracedecay_runtime_core::db::schema::SCHEMA_IDENTITY
        ),
        actual,
    };
    let connection = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| reset(format!("unreadable SQLite database: {error}")))?;
    let version = connection
        .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
        .map_err(|error| reset(format!("unreadable schema version: {error}")))?;
    if version != tracedecay_runtime_core::db::schema::SCHEMA_VERSION {
        return Err(reset(format!("schema version v{version}")));
    }
    let identity = connection
        .query_row(
            "SELECT value FROM metadata WHERE key = ?1",
            [tracedecay_runtime_core::db::schema::SCHEMA_IDENTITY_KEY],
            |row| row.get::<_, String>(0),
        )
        .map_err(|_| reset("missing canonical schema identity".to_owned()))?;
    if identity != tracedecay_runtime_core::db::schema::SCHEMA_IDENTITY {
        return Err(reset(format!("schema identity {identity}")));
    }
    for (object_type, name) in tracedecay_runtime_core::db::schema::REQUIRED_SCHEMA_OBJECTS {
        let present = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master WHERE type = ?1 AND name = ?2
                )",
                [*object_type, *name],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|error| reset(format!("unreadable {object_type} {name}: {error}")))?;
        if !present {
            return Err(reset(format!("missing {object_type} {name}")));
        }
    }
    for (table, required_columns) in tracedecay_runtime_core::db::schema::REQUIRED_SCHEMA_COLUMNS {
        let mut statement = connection
            .prepare(&format!("PRAGMA table_info({table})"))
            .map_err(|error| reset(format!("unreadable table {table}: {error}")))?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|error| reset(format!("unreadable table {table}: {error}")))?
            .collect::<Result<std::collections::BTreeSet<_>, _>>()
            .map_err(|error| reset(format!("invalid table {table}: {error}")))?;
        if let Some(missing) = required_columns
            .iter()
            .find(|column| !columns.contains(**column))
        {
            return Err(reset(format!("missing column {table}.{missing}")));
        }
    }
    let reference_violation = connection
        .prepare("PRAGMA foreign_key_check")
        .and_then(|mut statement| statement.exists([]))
        .map_err(|error| reset(format!("unreadable foreign-key references: {error}")))?;
    if reference_violation {
        return Err(reset("foreign-key reference violations".to_owned()));
    }
    Ok(())
}
